use std::collections::BTreeMap;

use crate::host_readiness::{HostReadinessReport, RunnerAccountReadiness};
use crate::lane_command::{LaneCommand, LaneCommandKind};
use crate::runner_account_plan::{
    DesiredRunnerAccount, PlannedSubordinateRange, PreparationObservationState,
    RUNNER_ACCOUNT_PLAN_SCHEMA_VERSION, RunnerAccountObservations, RunnerAccountPlan,
    RunnerAccountPlanDisposition, RunnerAccountPlanItem, RunnerAccountResourceKind,
};
use crate::subordinate_id::{
    FreshAuthorityObservation, PodmanMigrationPlan, SubordinateIdKind, SubordinateIdRange,
    SubordinateMappingPlanItem, SubordinatePlanDisposition,
};

use super::planner::{
    inconsistent, invalid_binding, push_blocker, require_no_binding, root_candidate,
    valid_source_binding,
};
use super::{
    ActionSlot, CandidateAction, FreshObservationBarrier, FreshObservationRequirement,
    HostPreparationBlocker, HostPreparationBlockerCode, HostPreparationResource,
    SUBORDINATE_BARRIER_ID,
};

pub(super) struct AccountPlanContext<'a> {
    pub observations: &'a RunnerAccountObservations,
    pub desired: &'a DesiredRunnerAccount,
    pub uid_item: &'a SubordinateMappingPlanItem,
    pub gid_item: &'a SubordinateMappingPlanItem,
    pub migration: &'a PodmanMigrationPlan,
    pub mapping_changed: bool,
    pub changed_mapping_ids: Vec<String>,
}

pub(super) fn validate_runner_account<'a>(
    report: &'a HostReadinessReport,
    candidates: &mut Vec<CandidateAction>,
    blockers: &mut Vec<HostPreparationBlocker>,
) -> Option<AccountPlanContext<'a>> {
    match &report.runner_account {
        RunnerAccountReadiness::NeedsConfiguration { .. } => {
            push_blocker(
                blockers,
                HostPreparationResource::RunnerAccount,
                HostPreparationBlockerCode::MissingConfiguration,
                "exact runner account policy is required before host preparation",
            );
            None
        }
        RunnerAccountReadiness::Planned {
            observations,
            plan,
            subordinate_ids,
        } => {
            validate_account_items(report, observations, plan, candidates, blockers);
            validate_subordinate_item(
                report,
                observations.subordinate_uids.state(),
                &plan.desired,
                &subordinate_ids.subordinate_uids,
                SubordinateIdKind::Uid,
                candidates,
                blockers,
            );
            validate_subordinate_item(
                report,
                observations.subordinate_gids.state(),
                &plan.desired,
                &subordinate_ids.subordinate_gids,
                SubordinateIdKind::Gid,
                candidates,
                blockers,
            );
            let changed_mapping_ids = [
                &subordinate_ids.subordinate_uids,
                &subordinate_ids.subordinate_gids,
            ]
            .into_iter()
            .filter(|item| item.disposition == SubordinatePlanDisposition::Required)
            .filter_map(|item| item.mutation.as_ref().map(|mutation| mutation.id.clone()))
            .collect::<Vec<_>>();
            Some(AccountPlanContext {
                observations,
                desired: &plan.desired,
                uid_item: &subordinate_ids.subordinate_uids,
                gid_item: &subordinate_ids.subordinate_gids,
                migration: &subordinate_ids.podman_migration,
                mapping_changed: !changed_mapping_ids.is_empty(),
                changed_mapping_ids,
            })
        }
    }
}

fn validate_account_items(
    report: &HostReadinessReport,
    observations: &RunnerAccountObservations,
    plan: &RunnerAccountPlan,
    candidates: &mut Vec<CandidateAction>,
    blockers: &mut Vec<HostPreparationBlocker>,
) {
    if plan.schema_version != RUNNER_ACCOUNT_PLAN_SCHEMA_VERSION {
        push_blocker(
            blockers,
            HostPreparationResource::RunnerAccount,
            HostPreparationBlockerCode::UnsupportedSchema,
            "the runner account plan schema is unsupported",
        );
    }
    let mut items = BTreeMap::new();
    for item in &plan.items {
        if items.insert(resource_rank(item.kind), item).is_some() {
            inconsistent(
                blockers,
                HostPreparationResource::RunnerAccount,
                "the runner account plan contains duplicate resource items",
            );
        }
    }
    let expected = [
        (
            RunnerAccountResourceKind::Group,
            observations.group.state(),
            ActionSlot::Group,
            "ensure-runner-group",
            LaneCommandKind::EnsureSystemGroup,
        ),
        (
            RunnerAccountResourceKind::User,
            observations.user.state(),
            ActionSlot::User,
            "ensure-runner-user",
            LaneCommandKind::EnsureSystemUser,
        ),
        (
            RunnerAccountResourceKind::HomeDirectory,
            observations.home.state(),
            ActionSlot::Home,
            "ensure-runner-home",
            LaneCommandKind::EnsureHomeDirectory,
        ),
        (
            RunnerAccountResourceKind::Linger,
            observations.linger.state(),
            ActionSlot::Linger,
            "enable-runner-linger",
            LaneCommandKind::EnableLinger,
        ),
    ];
    for (kind, state, slot, action_id, command_kind) in expected {
        let Some(item) = items.get(&resource_rank(kind)).copied() else {
            inconsistent(
                blockers,
                HostPreparationResource::RunnerAccount,
                "the runner account plan is missing a required resource item",
            );
            continue;
        };
        validate_account_item(
            report,
            state,
            item,
            slot,
            action_id,
            command_kind,
            plan,
            candidates,
            blockers,
        );
    }
    for (kind, state, action_id, command_kind) in [
        (
            RunnerAccountResourceKind::SubordinateUids,
            observations.subordinate_uids.state(),
            "ensure-runner-subordinate-uids",
            LaneCommandKind::EnsureSubordinateUids,
        ),
        (
            RunnerAccountResourceKind::SubordinateGids,
            observations.subordinate_gids.state(),
            "ensure-runner-subordinate-gids",
            LaneCommandKind::EnsureSubordinateGids,
        ),
    ] {
        let Some(item) = items.get(&resource_rank(kind)).copied() else {
            inconsistent(
                blockers,
                HostPreparationResource::RunnerAccount,
                "the runner account plan is missing a subordinate-ID resource item",
            );
            continue;
        };
        validate_account_subordinate_item(
            state,
            item,
            kind,
            action_id,
            command_kind,
            &plan.desired,
            blockers,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_account_item(
    report: &HostReadinessReport,
    state: PreparationObservationState,
    item: &RunnerAccountPlanItem,
    slot: ActionSlot,
    action_id: &str,
    command_kind: LaneCommandKind,
    plan: &RunnerAccountPlan,
    candidates: &mut Vec<CandidateAction>,
    blockers: &mut Vec<HostPreparationBlocker>,
) {
    if item.disposition != account_disposition(state) {
        inconsistent(
            blockers,
            HostPreparationResource::RunnerAccount,
            "a runner account observation conflicts with its plan disposition",
        );
        return;
    }
    match item.disposition {
        RunnerAccountPlanDisposition::Satisfied => require_no_binding(
            item.mutation.as_ref(),
            item.command.as_ref(),
            blockers,
            HostPreparationResource::RunnerAccount,
        ),
        RunnerAccountPlanDisposition::NeedsInspection => {
            require_no_binding(
                item.mutation.as_ref(),
                item.command.as_ref(),
                blockers,
                HostPreparationResource::RunnerAccount,
            );
            push_blocker(
                blockers,
                HostPreparationResource::RunnerAccount,
                HostPreparationBlockerCode::NeedsInspection,
                "runner account evidence remains incomplete",
            );
        }
        RunnerAccountPlanDisposition::Blocked => {
            require_no_binding(
                item.mutation.as_ref(),
                item.command.as_ref(),
                blockers,
                HostPreparationResource::RunnerAccount,
            );
            push_blocker(
                blockers,
                HostPreparationResource::RunnerAccount,
                HostPreparationBlockerCode::ConflictingEvidence,
                "runner account evidence conflicts with the reviewed account policy",
            );
        }
        RunnerAccountPlanDisposition::Required => {
            let desired = &plan.desired;
            let summary = match slot {
                ActionSlot::Group => format!(
                    "ensure dedicated runner group {}",
                    desired.primary_group().as_str()
                ),
                ActionSlot::User => {
                    format!(
                        "ensure dedicated runner user {}",
                        desired.username().as_str()
                    )
                }
                ActionSlot::Home => format!("ensure runner home directory {}", desired.home()),
                ActionSlot::Linger => {
                    format!("ensure systemd linger for {}", desired.username().as_str())
                }
                _ => unreachable!("account item slot is fixed"),
            };
            if let Some(candidate) = root_candidate(
                report,
                slot,
                action_id,
                command_kind,
                summary,
                item.mutation.as_ref(),
                item.command.as_ref(),
                |mutation| match slot {
                    ActionSlot::Group => {
                        LaneCommand::ensure_system_group(mutation, desired.primary_group())
                    }
                    ActionSlot::User => LaneCommand::ensure_system_user(
                        mutation,
                        desired.username(),
                        desired.primary_group(),
                        desired.home(),
                    ),
                    ActionSlot::Home => LaneCommand::ensure_home_directory(
                        mutation,
                        desired.username(),
                        desired.primary_group(),
                        desired.home(),
                    ),
                    ActionSlot::Linger => LaneCommand::enable_linger(mutation, desired.username()),
                    _ => unreachable!("account item slot is fixed"),
                },
                blockers,
                HostPreparationResource::RunnerAccount,
            ) {
                candidates.push(candidate);
            }
        }
    }
}

fn validate_account_subordinate_item(
    state: PreparationObservationState,
    item: &RunnerAccountPlanItem,
    kind: RunnerAccountResourceKind,
    action_id: &str,
    command_kind: LaneCommandKind,
    desired: &DesiredRunnerAccount,
    blockers: &mut Vec<HostPreparationBlocker>,
) {
    let subordinate_kind = match kind {
        RunnerAccountResourceKind::SubordinateUids => SubordinateIdKind::Uid,
        RunnerAccountResourceKind::SubordinateGids => SubordinateIdKind::Gid,
        _ => unreachable!("subordinate account item kind is fixed"),
    };
    let resource = subordinate_resource(subordinate_kind);
    if item.kind != kind || item.disposition != account_disposition(state) {
        inconsistent(
            blockers,
            resource,
            "a runner account subordinate-ID item conflicts with its observation",
        );
        return;
    }
    match item.disposition {
        RunnerAccountPlanDisposition::Satisfied
        | RunnerAccountPlanDisposition::NeedsInspection
        | RunnerAccountPlanDisposition::Blocked => require_no_binding(
            item.mutation.as_ref(),
            item.command.as_ref(),
            blockers,
            resource,
        ),
        RunnerAccountPlanDisposition::Required => {
            let (Some(mutation), Some(command)) = (item.mutation.as_ref(), item.command.as_ref())
            else {
                invalid_binding(blockers, resource);
                return;
            };
            if !valid_source_binding(
                mutation,
                command,
                action_id,
                crate::journal::ExecutionLane::Root,
                command_kind,
            ) {
                invalid_binding(blockers, resource);
                return;
            }
            let expected = match subordinate_kind {
                SubordinateIdKind::Uid => LaneCommand::ensure_subordinate_uids(
                    mutation,
                    desired.username(),
                    desired.subordinate_uids().start(),
                    desired.subordinate_uids().count(),
                ),
                SubordinateIdKind::Gid => LaneCommand::ensure_subordinate_gids(
                    mutation,
                    desired.username(),
                    desired.subordinate_gids().start(),
                    desired.subordinate_gids().count(),
                ),
            };
            if !matches!(expected, Ok(expected) if &expected == command) {
                invalid_binding(blockers, resource);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_subordinate_item(
    report: &HostReadinessReport,
    state: PreparationObservationState,
    desired: &DesiredRunnerAccount,
    item: &SubordinateMappingPlanItem,
    kind: SubordinateIdKind,
    candidates: &mut Vec<CandidateAction>,
    blockers: &mut Vec<HostPreparationBlocker>,
) {
    let resource = subordinate_resource(kind);
    if item.kind != kind || item.disposition != subordinate_disposition(state) {
        inconsistent(
            blockers,
            resource,
            "a subordinate-ID observation conflicts with its reconciliation disposition",
        );
        return;
    }
    let expected_range = desired_range(desired, kind);
    if matches!(
        item.disposition,
        SubordinatePlanDisposition::Satisfied | SubordinatePlanDisposition::Required
    ) && !item.selected_range.is_some_and(|range| {
        range.start() == expected_range.start() && range.count() == expected_range.count()
    }) {
        inconsistent(
            blockers,
            resource,
            "the selected subordinate-ID range differs from the reviewed account policy",
        );
    }
    match item.disposition {
        SubordinatePlanDisposition::Satisfied => {
            require_no_binding(
                item.mutation.as_ref(),
                item.command.as_ref(),
                blockers,
                resource,
            );
            if item.fresh_observation.is_some() {
                inconsistent(
                    blockers,
                    resource,
                    "a satisfied subordinate-ID item carries an unexpected observation barrier",
                );
            }
        }
        SubordinatePlanDisposition::NeedsInspection => {
            require_no_binding(
                item.mutation.as_ref(),
                item.command.as_ref(),
                blockers,
                resource,
            );
            push_blocker(
                blockers,
                resource,
                HostPreparationBlockerCode::NeedsInspection,
                "subordinate-ID authority evidence remains incomplete",
            );
        }
        SubordinatePlanDisposition::Blocked => {
            require_no_binding(
                item.mutation.as_ref(),
                item.command.as_ref(),
                blockers,
                resource,
            );
            push_blocker(
                blockers,
                resource,
                HostPreparationBlockerCode::ConflictingEvidence,
                "subordinate-ID authority conflicts with the reviewed allocation",
            );
        }
        SubordinatePlanDisposition::Required => {
            let (slot, action_id, command_kind, label) = match kind {
                SubordinateIdKind::Uid => (
                    ActionSlot::SubordinateUids,
                    "ensure-runner-subordinate-uids",
                    LaneCommandKind::EnsureSubordinateUids,
                    "UID",
                ),
                SubordinateIdKind::Gid => (
                    ActionSlot::SubordinateGids,
                    "ensure-runner-subordinate-gids",
                    LaneCommandKind::EnsureSubordinateGids,
                    "GID",
                ),
            };
            validate_fresh_authority(
                item.fresh_observation.as_ref(),
                action_id,
                kind,
                desired.username().as_str(),
                expected_range,
                blockers,
            );
            let summary = format!(
                "reconcile subordinate {label} range {}-{} for {}",
                expected_range.start(),
                expected_range.end_inclusive(),
                desired.username().as_str()
            );
            if let Some(candidate) = root_candidate(
                report,
                slot,
                action_id,
                command_kind,
                summary,
                item.mutation.as_ref(),
                item.command.as_ref(),
                |mutation| match kind {
                    SubordinateIdKind::Uid => LaneCommand::ensure_subordinate_uids(
                        mutation,
                        desired.username(),
                        expected_range.start(),
                        expected_range.count(),
                    ),
                    SubordinateIdKind::Gid => LaneCommand::ensure_subordinate_gids(
                        mutation,
                        desired.username(),
                        expected_range.start(),
                        expected_range.count(),
                    ),
                },
                blockers,
                resource,
            ) {
                candidates.push(candidate);
            }
        }
    }
}

fn validate_fresh_authority(
    observation: Option<&FreshAuthorityObservation>,
    action_id: &str,
    kind: SubordinateIdKind,
    owner: &str,
    range: PlannedSubordinateRange,
    blockers: &mut Vec<HostPreparationBlocker>,
) {
    let Some(observation) = observation else {
        inconsistent(
            blockers,
            subordinate_resource(kind),
            "a subordinate-ID mutation lacks its required fresh authority observation",
        );
        return;
    };
    let expected_path = match kind {
        SubordinateIdKind::Uid => "/etc/subuid",
        SubordinateIdKind::Gid => "/etc/subgid",
    };
    if observation.after_action_id != action_id
        || observation.authority_path != expected_path
        || observation.required_owner != owner
        || observation.required_range.start() != range.start()
        || observation.required_range.count() != range.count()
    {
        inconsistent(
            blockers,
            subordinate_resource(kind),
            "a subordinate-ID observation barrier conflicts with the reviewed mutation",
        );
    }
}

pub(super) fn subordinate_barrier(
    desired: &DesiredRunnerAccount,
    uid_item: &SubordinateMappingPlanItem,
    gid_item: &SubordinateMappingPlanItem,
    changed_mapping_ids: &[String],
) -> FreshObservationBarrier {
    let uid_range = selected_range(uid_item, desired.subordinate_uids());
    let gid_range = selected_range(gid_item, desired.subordinate_gids());
    FreshObservationBarrier {
        id: SUBORDINATE_BARRIER_ID.to_owned(),
        after_action_ids: changed_mapping_ids.to_vec(),
        requirements: vec![
            FreshObservationRequirement::SubordinateAuthority {
                authority: SubordinateIdKind::Uid,
                path: "/etc/subuid".to_owned(),
                owner: desired.username().as_str().to_owned(),
                range_start: uid_range.start(),
                range_count: uid_range.count(),
            },
            FreshObservationRequirement::SubordinateAuthority {
                authority: SubordinateIdKind::Gid,
                path: "/etc/subgid".to_owned(),
                owner: desired.username().as_str().to_owned(),
                range_start: gid_range.start(),
                range_count: gid_range.count(),
            },
            FreshObservationRequirement::RunnerIdentity {
                username: desired.username().as_str().to_owned(),
                primary_group: desired.primary_group().as_str().to_owned(),
                home: desired.home().to_owned(),
                require_non_root_uid_and_gid: true,
            },
            FreshObservationRequirement::RunnerRuntimeDirectory {
                path_from_fresh_uid: true,
                require_runner_ownership: true,
                forbid_group_or_other_write: true,
            },
        ],
        summary: "re-observe both subordinate-ID authority files and exact runner account/runtime evidence; command completion alone does not establish observed state"
            .to_owned(),
    }
}

fn selected_range(
    item: &SubordinateMappingPlanItem,
    fallback: PlannedSubordinateRange,
) -> SubordinateIdRange {
    item.selected_range.unwrap_or_else(|| {
        SubordinateIdRange::new(fallback.start(), fallback.count())
            .expect("reviewed subordinate range is valid")
    })
}

fn desired_range(
    desired: &DesiredRunnerAccount,
    kind: SubordinateIdKind,
) -> PlannedSubordinateRange {
    match kind {
        SubordinateIdKind::Uid => desired.subordinate_uids(),
        SubordinateIdKind::Gid => desired.subordinate_gids(),
    }
}

pub(super) fn account_observations_matching(observations: &RunnerAccountObservations) -> bool {
    [
        observations.group.state(),
        observations.user.state(),
        observations.home.state(),
        observations.subordinate_uids.state(),
        observations.subordinate_gids.state(),
        observations.linger.state(),
    ]
    .iter()
    .all(|state| *state == PreparationObservationState::Matching)
}

const fn account_disposition(state: PreparationObservationState) -> RunnerAccountPlanDisposition {
    match state {
        PreparationObservationState::Matching => RunnerAccountPlanDisposition::Satisfied,
        PreparationObservationState::Absent => RunnerAccountPlanDisposition::Required,
        PreparationObservationState::Unknown => RunnerAccountPlanDisposition::NeedsInspection,
        PreparationObservationState::Conflicting => RunnerAccountPlanDisposition::Blocked,
    }
}

const fn subordinate_disposition(state: PreparationObservationState) -> SubordinatePlanDisposition {
    match state {
        PreparationObservationState::Matching => SubordinatePlanDisposition::Satisfied,
        PreparationObservationState::Absent => SubordinatePlanDisposition::Required,
        PreparationObservationState::Unknown => SubordinatePlanDisposition::NeedsInspection,
        PreparationObservationState::Conflicting => SubordinatePlanDisposition::Blocked,
    }
}

const fn subordinate_resource(kind: SubordinateIdKind) -> HostPreparationResource {
    match kind {
        SubordinateIdKind::Uid => HostPreparationResource::SubordinateUids,
        SubordinateIdKind::Gid => HostPreparationResource::SubordinateGids,
    }
}

const fn resource_rank(kind: RunnerAccountResourceKind) -> u8 {
    match kind {
        RunnerAccountResourceKind::Group => 0,
        RunnerAccountResourceKind::User => 1,
        RunnerAccountResourceKind::HomeDirectory => 2,
        RunnerAccountResourceKind::SubordinateUids => 3,
        RunnerAccountResourceKind::SubordinateGids => 4,
        RunnerAccountResourceKind::Linger => 5,
    }
}
