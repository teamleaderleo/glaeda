use std::fmt;

use serde::Serialize;

use crate::debian_package_plan::PackagePlanDisposition;
use crate::durable_lane_execution::DurableLanePlan;
use crate::host_readiness::{HostObservationState, HostReadinessReport, RunnerAccountReadiness};
use crate::host_rootless_podman::HostRootlessPodmanReadiness;
use crate::journal::{ExecutionLane, PlannedMutation, RollbackClass};
use crate::lane_command::{LaneCommand, LaneCommandKind};
use crate::rootless_podman_preflight::{
    RootlessPodmanPreflightDisposition, RootlessPodmanPreflightState,
};
use crate::runner_account_plan::PreparationObservationState;
use crate::subordinate_id::{PodmanMigrationPlan, SubordinateIdKind};

mod account;
mod planner;
mod render;
mod rootless;
#[cfg(test)]
mod tests;

pub const HOST_PREPARATION_PLAN_SCHEMA_VERSION: u8 = 1;

pub(super) const ROOT_PHASE_ID: &str = "host-preparation-root-phase";
pub(super) const RUNNER_MIGRATION_PHASE_ID: &str = "host-preparation-runner-migration-phase";
pub(super) const SUBORDINATE_BARRIER_ID: &str = "reobserve-subordinate-ids-and-runner-runtime";
pub(super) const MIGRATION_ACTION_ID: &str = "migrate-runner-podman-after-subordinate-id-change";

/// A deterministic proposal that retains the exact reviewed source report in memory.
///
/// JSON and human output expose normalized source identity. The complete source report stays
/// available through [`HostPreparationSource::report`], keeping lane command specifications,
/// environment values, and raw observation evidence outside public proposal output.
#[derive(Debug, Clone, Serialize)]
pub struct HostPreparationProposal {
    pub schema_version: u8,
    pub source: HostPreparationSource,
    pub result: HostPreparationResult,
}

#[derive(Clone, Serialize)]
pub struct HostPreparationSource {
    pub identity: HostReadinessSourceIdentity,
    #[serde(skip)]
    report: HostReadinessReport,
}

impl HostPreparationSource {
    #[must_use]
    pub fn report(&self) -> &HostReadinessReport {
        &self.report
    }
}

impl fmt::Debug for HostPreparationSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostPreparationSource")
            .field("identity", &self.identity)
            .field("report", &"<retained reviewed report>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostReadinessSourceIdentity {
    pub kind: String,
    pub schema_version: u8,
    pub repository: String,
    pub executables: Vec<SourceExecutableIdentity>,
    pub package_plan_schema_version: u8,
    pub package_disposition: PackagePlanDisposition,
    pub runner_account: SourceRunnerAccountIdentity,
    pub rootless_podman: SourceRootlessPodmanIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceExecutableIdentity {
    pub name: String,
    pub path: String,
    pub state: HostObservationState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "disposition", rename_all = "snake_case")]
pub enum SourceRunnerAccountIdentity {
    NeedsConfiguration,
    Planned {
        plan_schema_version: u8,
        group: PreparationObservationState,
        user: PreparationObservationState,
        home: PreparationObservationState,
        subordinate_uids: PreparationObservationState,
        subordinate_gids: PreparationObservationState,
        linger: PreparationObservationState,
        podman_migration: SourcePodmanMigrationIdentity,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourcePodmanMigrationIdentity {
    NotRequired,
    Required,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "source_state", rename_all = "snake_case")]
pub enum SourceRootlessPodmanIdentity {
    Deferred {
        state: RootlessPodmanPreflightState,
    },
    Observed {
        preflight_schema_version: u8,
        disposition: RootlessPodmanPreflightDisposition,
        packages: RootlessPodmanPreflightState,
        runner_account: RootlessPodmanPreflightState,
        runtime_directory: RootlessPodmanPreflightState,
        configuration: RootlessPodmanPreflightState,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "disposition", rename_all = "snake_case")]
pub enum HostPreparationResult {
    Ready,
    Executable {
        phase: ExecutableHostPreparationPhase,
        continuation_barriers: Vec<FreshObservationBarrier>,
        deferred_actions: Vec<DeferredHostPreparationAction>,
    },
    Blocked {
        blockers: Vec<HostPreparationBlocker>,
    },
}

#[derive(Clone, Serialize)]
pub struct ExecutableHostPreparationPhase {
    pub id: String,
    pub actions: Vec<ExecutableHostPreparationAction>,
    #[serde(skip)]
    durable_plan: DurableLanePlan,
}

impl ExecutableHostPreparationPhase {
    #[must_use]
    pub fn durable_plan(&self) -> DurableLanePlan {
        self.durable_plan.clone()
    }

    #[must_use]
    pub fn into_durable_plan(self) -> DurableLanePlan {
        self.durable_plan
    }
}

impl fmt::Debug for ExecutableHostPreparationPhase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExecutableHostPreparationPhase")
            .field("id", &self.id)
            .field("actions", &self.actions)
            .field("durable_plan", &"<validated typed durable lane plan>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecutableHostPreparationAction {
    pub id: String,
    pub lane: ExecutionLane,
    pub command_kind: LaneCommandKind,
    pub rollback: RollbackClass,
    pub summary: String,
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FreshObservationBarrier {
    pub id: String,
    pub after_action_ids: Vec<String>,
    pub requirements: Vec<FreshObservationRequirement>,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FreshObservationRequirement {
    SubordinateAuthority {
        authority: SubordinateIdKind,
        path: String,
        owner: String,
        range_start: u32,
        range_count: u32,
    },
    RunnerIdentity {
        username: String,
        primary_group: String,
        home: String,
        require_non_root_uid_and_gid: bool,
    },
    RunnerRuntimeDirectory {
        path_from_fresh_uid: bool,
        require_runner_ownership: bool,
        forbid_group_or_other_write: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeferredHostPreparationAction {
    pub id: String,
    pub lane: ExecutionLane,
    pub command_kind: LaneCommandKind,
    pub summary: String,
    pub depends_on: Vec<String>,
    pub reason: DeferredActionReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeferredActionReason {
    FreshObservationRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct HostPreparationBlocker {
    pub resource: HostPreparationResource,
    pub code: HostPreparationBlockerCode,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostPreparationResource {
    HostReadinessReport,
    ExactExecutables,
    DebianPackages,
    RunnerAccount,
    SubordinateUids,
    SubordinateGids,
    RootlessPodman,
    DurableLanePlan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostPreparationBlockerCode {
    UnsupportedSchema,
    NeedsInspection,
    ConflictingEvidence,
    MissingConfiguration,
    UnsupportedMutation,
    InconsistentObservation,
    InvalidCommandBinding,
    InvalidExecutablePhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum ActionSlot {
    Package,
    Group,
    User,
    Home,
    Linger,
    SubordinateUids,
    SubordinateGids,
    PodmanMigration,
}

#[derive(Debug, Clone)]
pub(super) struct CandidateAction {
    pub slot: ActionSlot,
    pub mutation: PlannedMutation,
    pub command: LaneCommand,
}

/// Convert one reviewed host-readiness report into a pure durable preparation proposal.
#[must_use]
pub fn plan_host_preparation(report: HostReadinessReport) -> HostPreparationProposal {
    let identity = source_identity(&report);
    let result = planner::build_result(&report);
    HostPreparationProposal {
        schema_version: HOST_PREPARATION_PLAN_SCHEMA_VERSION,
        source: HostPreparationSource { identity, report },
        result,
    }
}

#[must_use]
pub fn render_human(proposal: &HostPreparationProposal) -> String {
    render::render_human(proposal)
}

fn source_identity(report: &HostReadinessReport) -> HostReadinessSourceIdentity {
    let mut executables = report
        .executables
        .iter()
        .map(|observation| SourceExecutableIdentity {
            name: observation.name.clone(),
            path: observation.path.display().to_string(),
            state: observation.state,
        })
        .collect::<Vec<_>>();
    executables.sort_by(|left, right| (&left.name, &left.path).cmp(&(&right.name, &right.path)));
    let runner_account = match &report.runner_account {
        RunnerAccountReadiness::NeedsConfiguration { .. } => {
            SourceRunnerAccountIdentity::NeedsConfiguration
        }
        RunnerAccountReadiness::Planned {
            observations,
            plan,
            subordinate_ids,
        } => SourceRunnerAccountIdentity::Planned {
            plan_schema_version: plan.schema_version,
            group: observations.group.state(),
            user: observations.user.state(),
            home: observations.home.state(),
            subordinate_uids: observations.subordinate_uids.state(),
            subordinate_gids: observations.subordinate_gids.state(),
            linger: observations.linger.state(),
            podman_migration: match &subordinate_ids.podman_migration {
                PodmanMigrationPlan::NotRequired => SourcePodmanMigrationIdentity::NotRequired,
                PodmanMigrationPlan::Required { .. } => SourcePodmanMigrationIdentity::Required,
                PodmanMigrationPlan::Blocked { .. } => SourcePodmanMigrationIdentity::Blocked,
            },
        },
    };
    let rootless_podman = match &report.rootless_podman {
        HostRootlessPodmanReadiness::Deferred { state, .. } => {
            SourceRootlessPodmanIdentity::Deferred { state: *state }
        }
        HostRootlessPodmanReadiness::Observed { preflight, .. } => {
            SourceRootlessPodmanIdentity::Observed {
                preflight_schema_version: preflight.schema_version,
                disposition: preflight.disposition,
                packages: preflight.packages.state,
                runner_account: preflight.runner_account.state,
                runtime_directory: preflight.runtime_directory.state,
                configuration: preflight.configuration.state,
            }
        }
    };
    HostReadinessSourceIdentity {
        kind: "host_readiness_report".to_owned(),
        schema_version: report.schema_version,
        repository: report.repository.clone(),
        executables,
        package_plan_schema_version: report.package_plan.schema_version,
        package_disposition: report.package_plan.disposition,
        runner_account,
        rootless_podman,
    }
}

impl fmt::Display for HostPreparationProposal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&render_human(self))
    }
}
