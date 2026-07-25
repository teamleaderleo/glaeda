use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::debian_package_plan::{DEBIAN_PACKAGE_PLAN_SCHEMA_VERSION, PackagePlanDisposition};
use crate::durable_lane_execution::DurableLanePlan;
use crate::host_readiness::{
    HOST_READINESS_SCHEMA_VERSION, HostObservationState, HostReadinessReport,
};
use crate::journal::{ExecutionLane, PlannedMutation, Preconditions, RollbackClass};
use crate::lane_command::{LaneCommand, LaneCommandKind};
use crate::subordinate_id::PodmanMigrationPlan;

use super::account::{account_observations_matching, subordinate_barrier, validate_runner_account};
use super::rootless::validate_rootless_podman;
use super::{
    ActionSlot, CandidateAction, DeferredActionReason, DeferredHostPreparationAction,
    ExecutableHostPreparationAction, ExecutableHostPreparationPhase, HostPreparationBlocker,
    HostPreparationBlockerCode, HostPreparationResource, HostPreparationResult,
    MIGRATION_ACTION_ID, ROOT_PHASE_ID, RUNNER_MIGRATION_PHASE_ID,
};

const REQUIRED_EXECUTABLES: [(&str, &str); 3] = [
    ("git", "/usr/bin/git"),
    ("podman", "/usr/bin/podman"),
    ("systemctl", "/usr/bin/systemctl"),
];

pub(super) fn build_result(report: &HostReadinessReport) -> HostPreparationResult {
    let mut blockers = Vec::new();
    let mut candidates = Vec::new();

    validate_report_identity(report, &mut blockers);
    validate_package_plan(report, &mut candidates, &mut blockers);
    validate_exact_executables(report, &mut blockers);

    let account = validate_runner_account(report, &mut candidates, &mut blockers);
    let has_package_action = has_slot(&candidates, ActionSlot::Package);
    let has_account_action = candidates.iter().any(|candidate| {
        matches!(
            candidate.slot,
            ActionSlot::Group | ActionSlot::User | ActionSlot::Home | ActionSlot::Linger
        )
    });
    let has_mapping_action = candidates.iter().any(|candidate| {
        matches!(
            candidate.slot,
            ActionSlot::SubordinateUids | ActionSlot::SubordinateGids
        )
    });
    let runner_execution_ready = validate_rootless_podman(
        report,
        has_package_action,
        has_account_action,
        has_mapping_action,
        &mut blockers,
    );

    let mut continuation_barriers = Vec::new();
    let mut deferred_actions = Vec::new();
    if let Some(account) = account {
        if account.mapping_changed {
            validate_deferred_migration(account.migration, &mut blockers);
            if blockers.is_empty() {
                continuation_barriers.push(subordinate_barrier(
                    account.desired,
                    account.uid_item,
                    account.gid_item,
                    &account.changed_mapping_ids,
                ));
                deferred_actions.push(DeferredHostPreparationAction {
                    id: MIGRATION_ACTION_ID.to_owned(),
                    lane: ExecutionLane::RunnerUser,
                    command_kind: LaneCommandKind::RunnerPodmanMigrate,
                    summary: "refresh reviewed rootless Podman namespace state".to_owned(),
                    depends_on: account.changed_mapping_ids,
                    reason: DeferredActionReason::FreshObservationRequired,
                });
            }
        } else {
            match account.migration {
                PodmanMigrationPlan::NotRequired => {}
                PodmanMigrationPlan::Blocked { .. } => push_blocker(
                    &mut blockers,
                    HostPreparationResource::RootlessPodman,
                    HostPreparationBlockerCode::NeedsInspection,
                    "rootless Podman migration remains blocked by incomplete reviewed evidence",
                ),
                PodmanMigrationPlan::Required {
                    mutation, command, ..
                } => {
                    if !account_observations_matching(account.observations)
                        || !runner_execution_ready
                        || !candidates.is_empty()
                    {
                        inconsistent(
                            &mut blockers,
                            HostPreparationResource::RootlessPodman,
                            "runner-user migration requires a fresh report with all account, mapping, package, executable, configuration, and runtime evidence matching",
                        );
                    } else if let Some(candidate) =
                        migration_candidate(report, mutation, command, &mut blockers)
                    {
                        candidates.push(candidate);
                    }
                }
            }
        }
    }

    if !blockers.is_empty() {
        return blocked_result(blockers);
    }
    candidates.sort_by_key(|candidate| candidate.slot);
    if candidates.is_empty() {
        return HostPreparationResult::Ready;
    }
    if has_mapping_action && has_slot(&candidates, ActionSlot::PodmanMigration) {
        return blocked_result(vec![HostPreparationBlocker {
            resource: HostPreparationResource::DurableLanePlan,
            code: HostPreparationBlockerCode::InvalidExecutablePhase,
            evidence: vec![
                "runner-user Podman migration cannot share a phase with subordinate-ID changes"
                    .to_owned(),
            ],
        }]);
    }

    let actions = public_actions(&candidates);
    if let Some(problem) = validate_dependencies(&actions) {
        return blocked_result(vec![HostPreparationBlocker {
            resource: HostPreparationResource::DurableLanePlan,
            code: HostPreparationBlockerCode::InvalidExecutablePhase,
            evidence: vec![problem],
        }]);
    }
    let durable_plan = match DurableLanePlan::new(
        candidates
            .iter()
            .map(|candidate| candidate.mutation.clone())
            .collect(),
        candidates
            .iter()
            .map(|candidate| candidate.command.clone())
            .collect(),
    ) {
        Ok(plan) => plan,
        Err(_) => {
            return blocked_result(vec![HostPreparationBlocker {
                resource: HostPreparationResource::DurableLanePlan,
                code: HostPreparationBlockerCode::InvalidExecutablePhase,
                evidence: vec![
                    "the executable phase failed bounded durable lane validation".to_owned(),
                ],
            }]);
        }
    };
    let phase_id = if candidates
        .iter()
        .all(|candidate| candidate.slot == ActionSlot::PodmanMigration)
    {
        RUNNER_MIGRATION_PHASE_ID
    } else {
        ROOT_PHASE_ID
    };
    HostPreparationResult::Executable {
        phase: ExecutableHostPreparationPhase {
            id: phase_id.to_owned(),
            actions,
            durable_plan,
        },
        continuation_barriers,
        deferred_actions,
    }
}

fn validate_report_identity(
    report: &HostReadinessReport,
    blockers: &mut Vec<HostPreparationBlocker>,
) {
    if report.schema_version != HOST_READINESS_SCHEMA_VERSION {
        push_blocker(
            blockers,
            HostPreparationResource::HostReadinessReport,
            HostPreparationBlockerCode::UnsupportedSchema,
            "the host readiness report schema is unsupported",
        );
    }
    if report.repository.is_empty()
        || report.repository.len() > 256
        || report.repository.chars().any(char::is_control)
    {
        inconsistent(
            blockers,
            HostPreparationResource::HostReadinessReport,
            "the reviewed repository identity is invalid",
        );
    }
}

fn validate_package_plan(
    report: &HostReadinessReport,
    candidates: &mut Vec<CandidateAction>,
    blockers: &mut Vec<HostPreparationBlocker>,
) {
    let plan = &report.package_plan;
    if plan.schema_version != DEBIAN_PACKAGE_PLAN_SCHEMA_VERSION {
        push_blocker(
            blockers,
            HostPreparationResource::DebianPackages,
            HostPreparationBlockerCode::UnsupportedSchema,
            "the Debian package plan schema is unsupported",
        );
    }
    match plan.disposition {
        PackagePlanDisposition::Ready => {
            if plan.mutation.is_some()
                || plan.command.is_some()
                || !plan.missing_packages.is_empty()
                || !plan.unknown_packages.is_empty()
            {
                inconsistent(
                    blockers,
                    HostPreparationResource::DebianPackages,
                    "the ready package classification conflicts with its plan fields",
                );
            }
        }
        PackagePlanDisposition::NeedsInspection => {
            if plan.mutation.is_some() || plan.command.is_some() || plan.unknown_packages.is_empty()
            {
                inconsistent(
                    blockers,
                    HostPreparationResource::DebianPackages,
                    "the package inspection classification conflicts with its plan fields",
                );
            }
            push_blocker(
                blockers,
                HostPreparationResource::DebianPackages,
                HostPreparationBlockerCode::NeedsInspection,
                "required package state is incomplete",
            );
        }
        PackagePlanDisposition::Required => {
            if plan.missing_packages.is_empty() || !plan.unknown_packages.is_empty() {
                inconsistent(
                    blockers,
                    HostPreparationResource::DebianPackages,
                    "the required package classification lacks a complete absent package set",
                );
                return;
            }
            let summary = format!(
                "install reviewed Debian-family host prerequisites: {}",
                plan.missing_packages
                    .iter()
                    .map(|package| package.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            if let Some(candidate) = root_candidate(
                report,
                ActionSlot::Package,
                "install-debian-host-prerequisites",
                LaneCommandKind::AptInstall,
                summary,
                plan.mutation.as_ref(),
                plan.command.as_ref(),
                |mutation| LaneCommand::apt_install(mutation, &plan.missing_packages),
                blockers,
                HostPreparationResource::DebianPackages,
            ) {
                candidates.push(candidate);
            }
        }
    }
}

fn validate_exact_executables(
    report: &HostReadinessReport,
    blockers: &mut Vec<HostPreparationBlocker>,
) {
    let expected = REQUIRED_EXECUTABLES.into_iter().collect::<BTreeMap<_, _>>();
    let mut seen = BTreeSet::new();
    for observation in &report.executables {
        let Some(expected_path) = expected.get(observation.name.as_str()) else {
            inconsistent(
                blockers,
                HostPreparationResource::ExactExecutables,
                "the report contains an unsupported executable observation",
            );
            continue;
        };
        if !seen.insert(observation.name.as_str()) || observation.path != Path::new(*expected_path)
        {
            inconsistent(
                blockers,
                HostPreparationResource::ExactExecutables,
                "the executable observation set contains a duplicate or unexpected path",
            );
            continue;
        }
        match observation.state {
            HostObservationState::Matching => {}
            HostObservationState::Unknown => push_blocker(
                blockers,
                HostPreparationResource::ExactExecutables,
                HostPreparationBlockerCode::NeedsInspection,
                "reviewed executable metadata remains unknown",
            ),
            HostObservationState::Conflicting => push_blocker(
                blockers,
                HostPreparationResource::ExactExecutables,
                HostPreparationBlockerCode::ConflictingEvidence,
                "reviewed executable metadata conflicts with the trusted path policy",
            ),
            HostObservationState::Absent => {
                if !host_executable_absence_is_explained(report, &observation.name) {
                    push_blocker(
                        blockers,
                        HostPreparationResource::ExactExecutables,
                        HostPreparationBlockerCode::UnsupportedMutation,
                        "a reviewed executable is absent without a mapped package action",
                    );
                }
            }
        }
    }
    if seen.len() != expected.len() {
        inconsistent(
            blockers,
            HostPreparationResource::ExactExecutables,
            "the exact executable observation set is incomplete",
        );
    }
}

fn validate_deferred_migration(
    migration: &PodmanMigrationPlan,
    blockers: &mut Vec<HostPreparationBlocker>,
) {
    match migration {
        PodmanMigrationPlan::NotRequired => inconsistent(
            blockers,
            HostPreparationResource::RootlessPodman,
            "subordinate-ID changes require a deferred Podman migration decision",
        ),
        PodmanMigrationPlan::Blocked { .. } => {}
        PodmanMigrationPlan::Required {
            mutation, command, ..
        } => {
            if !valid_source_binding(
                mutation,
                command,
                MIGRATION_ACTION_ID,
                ExecutionLane::RunnerUser,
                LaneCommandKind::RunnerPodmanMigrate,
            ) {
                invalid_binding(blockers, HostPreparationResource::RootlessPodman);
            }
        }
    }
}

fn migration_candidate(
    report: &HostReadinessReport,
    source_mutation: &PlannedMutation,
    source_command: &LaneCommand,
    blockers: &mut Vec<HostPreparationBlocker>,
) -> Option<CandidateAction> {
    if !valid_source_binding(
        source_mutation,
        source_command,
        MIGRATION_ACTION_ID,
        ExecutionLane::RunnerUser,
        LaneCommandKind::RunnerPodmanMigrate,
    ) {
        invalid_binding(blockers, HostPreparationResource::RootlessPodman);
        return None;
    }
    Some(CandidateAction {
        slot: ActionSlot::PodmanMigration,
        mutation: normalized_mutation(
            report,
            MIGRATION_ACTION_ID,
            ExecutionLane::RunnerUser,
            "refresh reviewed rootless Podman namespace state".to_owned(),
            "fresh report proves exact runner account, subordinate-ID, configuration, and runtime evidence matching",
        ),
        command: source_command.clone(),
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn root_candidate(
    report: &HostReadinessReport,
    slot: ActionSlot,
    action_id: &str,
    command_kind: LaneCommandKind,
    summary: String,
    source_mutation: Option<&PlannedMutation>,
    source_command: Option<&LaneCommand>,
    expected_command: impl FnOnce(
        &PlannedMutation,
    ) -> Result<LaneCommand, crate::lane_command::LaneCommandError>,
    blockers: &mut Vec<HostPreparationBlocker>,
    resource: HostPreparationResource,
) -> Option<CandidateAction> {
    let (Some(source_mutation), Some(source_command)) = (source_mutation, source_command) else {
        invalid_binding(blockers, resource);
        return None;
    };
    if !valid_source_binding(
        source_mutation,
        source_command,
        action_id,
        ExecutionLane::Root,
        command_kind,
    ) {
        invalid_binding(blockers, resource);
        return None;
    }
    let mutation = normalized_mutation(
        report,
        action_id,
        ExecutionLane::Root,
        summary,
        "reviewed resource observation is classified required",
    );
    let expected = match expected_command(&mutation) {
        Ok(command) => command,
        Err(_) => {
            invalid_binding(blockers, resource);
            return None;
        }
    };
    if &expected != source_command {
        invalid_binding(blockers, resource);
        return None;
    }
    Some(CandidateAction {
        slot,
        mutation,
        command: source_command.clone(),
    })
}

fn normalized_mutation(
    report: &HostReadinessReport,
    action_id: &str,
    lane: ExecutionLane,
    summary: String,
    classification: &str,
) -> PlannedMutation {
    PlannedMutation::new(
        action_id,
        lane,
        summary,
        RollbackClass::Irreversible,
        Preconditions::new([
            format!(
                "reviewed host readiness report schema {} for repository {}",
                report.schema_version, report.repository
            ),
            classification.to_owned(),
        ]),
    )
}

pub(super) fn valid_source_binding(
    mutation: &PlannedMutation,
    command: &LaneCommand,
    action_id: &str,
    lane: ExecutionLane,
    kind: LaneCommandKind,
) -> bool {
    mutation.id == action_id
        && mutation.lane == lane
        && mutation.rollback == RollbackClass::Compensating
        && !mutation.summary.is_empty()
        && !mutation.preconditions.evidence.is_empty()
        && command.action_id() == action_id
        && command.lane() == lane
        && command.kind() == kind
}

pub(super) fn require_no_binding(
    mutation: Option<&PlannedMutation>,
    command: Option<&LaneCommand>,
    blockers: &mut Vec<HostPreparationBlocker>,
    resource: HostPreparationResource,
) {
    if mutation.is_some() || command.is_some() {
        invalid_binding(blockers, resource);
    }
}

fn public_actions(candidates: &[CandidateAction]) -> Vec<ExecutableHostPreparationAction> {
    let present = candidates
        .iter()
        .map(|candidate| (candidate.slot, candidate.mutation.id.clone()))
        .collect::<BTreeMap<_, _>>();
    candidates
        .iter()
        .map(|candidate| ExecutableHostPreparationAction {
            id: candidate.mutation.id.clone(),
            lane: candidate.mutation.lane,
            command_kind: candidate.command.kind(),
            rollback: candidate.mutation.rollback,
            summary: candidate.mutation.summary.clone(),
            depends_on: dependencies(candidate.slot, &present),
        })
        .collect()
}

fn dependencies(slot: ActionSlot, present: &BTreeMap<ActionSlot, String>) -> Vec<String> {
    let dependency_slots: &[ActionSlot] = match slot {
        ActionSlot::Package | ActionSlot::Group | ActionSlot::PodmanMigration => &[],
        ActionSlot::User => &[ActionSlot::Group],
        ActionSlot::Home | ActionSlot::Linger | ActionSlot::SubordinateUids => &[ActionSlot::User],
        ActionSlot::SubordinateGids => &[ActionSlot::User, ActionSlot::SubordinateUids],
    };
    dependency_slots
        .iter()
        .filter_map(|slot| present.get(slot).cloned())
        .collect()
}

fn validate_dependencies(actions: &[ExecutableHostPreparationAction]) -> Option<String> {
    let mut completed = BTreeSet::new();
    for action in actions {
        let mut dependencies = BTreeSet::new();
        for dependency in &action.depends_on {
            if dependency == &action.id
                || !dependencies.insert(dependency.as_str())
                || !completed.contains(dependency.as_str())
            {
                return Some(
                    "an executable action has an unstable, duplicate, self, or forward dependency"
                        .to_owned(),
                );
            }
        }
        completed.insert(action.id.as_str());
    }
    None
}

fn has_slot(candidates: &[CandidateAction], slot: ActionSlot) -> bool {
    candidates.iter().any(|candidate| candidate.slot == slot)
}

fn host_executable_absence_is_explained(report: &HostReadinessReport, name: &str) -> bool {
    match name {
        "git" => package_is_missing(report, "git"),
        "podman" => package_is_missing(report, "podman"),
        "systemctl" => false,
        _ => false,
    }
}

pub(super) fn package_is_missing(report: &HostReadinessReport, package: &str) -> bool {
    report.package_plan.disposition == PackagePlanDisposition::Required
        && report
            .package_plan
            .missing_packages
            .iter()
            .any(|candidate| candidate.as_str() == package)
}

pub(super) fn push_blocker(
    blockers: &mut Vec<HostPreparationBlocker>,
    resource: HostPreparationResource,
    code: HostPreparationBlockerCode,
    evidence: &str,
) {
    blockers.push(HostPreparationBlocker {
        resource,
        code,
        evidence: vec![evidence.to_owned()],
    });
}

pub(super) fn inconsistent(
    blockers: &mut Vec<HostPreparationBlocker>,
    resource: HostPreparationResource,
    evidence: &str,
) {
    push_blocker(
        blockers,
        resource,
        HostPreparationBlockerCode::InconsistentObservation,
        evidence,
    );
}

pub(super) fn invalid_binding(
    blockers: &mut Vec<HostPreparationBlocker>,
    resource: HostPreparationResource,
) {
    push_blocker(
        blockers,
        resource,
        HostPreparationBlockerCode::InvalidCommandBinding,
        "a required mutation does not bind exactly one expected typed lane command",
    );
}

fn blocked_result(mut blockers: Vec<HostPreparationBlocker>) -> HostPreparationResult {
    blockers.sort();
    blockers.dedup();
    HostPreparationResult::Blocked { blockers }
}
