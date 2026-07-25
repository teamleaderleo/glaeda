use crate::journal::ExecutionLane;
use crate::lane_command::LaneCommandKind;

use super::{
    FreshObservationRequirement, HostPreparationProposal, HostPreparationResource,
    HostPreparationResult,
};

pub(super) fn render_human(proposal: &HostPreparationProposal) -> String {
    let mut output = format!(
        "SmolRunner host preparation proposal\n\nSource: {} schema {} for {}\n",
        proposal.source.identity.kind,
        proposal.source.identity.schema_version,
        proposal.source.identity.repository
    );
    match &proposal.result {
        HostPreparationResult::Ready => {
            output
                .push_str("\n[READY] The reviewed host report requires no preparation actions.\n");
        }
        HostPreparationResult::Executable {
            phase,
            continuation_barriers,
            deferred_actions,
        } => {
            output.push_str(&format!("\nExecutable phase: {}\n", phase.id));
            for action in &phase.actions {
                output.push_str(&format!(
                    "[EXECUTE] {} ({}, {}, irreversible)\n",
                    action.id,
                    lane_name(action.lane),
                    command_kind_name(action.command_kind)
                ));
                if !action.depends_on.is_empty() {
                    output.push_str(&format!("  Depends on: {}\n", action.depends_on.join(", ")));
                }
            }
            for barrier in continuation_barriers {
                output.push_str(&format!("\n[FRESH OBSERVATION] {}\n", barrier.id));
                output.push_str(&format!("  {}\n", barrier.summary));
                for requirement in &barrier.requirements {
                    output.push_str(&format!(
                        "  Requirement: {}\n",
                        observation_requirement_name(requirement)
                    ));
                }
            }
            for action in deferred_actions {
                output.push_str(&format!(
                    "\n[DEFERRED] {} ({})\n  Fresh reviewed observation is required before planning this action.\n",
                    action.id,
                    command_kind_name(action.command_kind)
                ));
            }
        }
        HostPreparationResult::Blocked { blockers } => {
            output.push_str("\n[BLOCKED] No mutation phase can be proposed.\n");
            for blocker in blockers {
                output.push_str(&format!(
                    "- {}: {}\n",
                    resource_name(blocker.resource),
                    blocker.evidence.join("; ")
                ));
            }
        }
    }
    output
}

const fn lane_name(lane: ExecutionLane) -> &'static str {
    match lane {
        ExecutionLane::Operator => "operator",
        ExecutionLane::Root => "root",
        ExecutionLane::RunnerUser => "runner_user",
        ExecutionLane::Github => "github",
    }
}

const fn command_kind_name(kind: LaneCommandKind) -> &'static str {
    match kind {
        LaneCommandKind::AptInstall => "apt_install",
        LaneCommandKind::EnsureSystemGroup => "ensure_system_group",
        LaneCommandKind::EnsureSystemUser => "ensure_system_user",
        LaneCommandKind::EnsureHomeDirectory => "ensure_home_directory",
        LaneCommandKind::EnsureSubordinateUids => "ensure_subordinate_uids",
        LaneCommandKind::EnsureSubordinateGids => "ensure_subordinate_gids",
        LaneCommandKind::EnableLinger => "enable_linger",
        LaneCommandKind::RunnerPodmanInfo => "runner_podman_info",
        LaneCommandKind::RunnerPodmanMigrate => "runner_podman_migrate",
        LaneCommandKind::RunnerGitVersion => "runner_git_version",
    }
}

const fn observation_requirement_name(requirement: &FreshObservationRequirement) -> &'static str {
    match requirement {
        FreshObservationRequirement::SubordinateAuthority {
            authority: crate::subordinate_id::SubordinateIdKind::Uid,
            ..
        } => "complete /etc/subuid authority matches the reviewed owner and range",
        FreshObservationRequirement::SubordinateAuthority {
            authority: crate::subordinate_id::SubordinateIdKind::Gid,
            ..
        } => "complete /etc/subgid authority matches the reviewed owner and range",
        FreshObservationRequirement::RunnerIdentity { .. } => {
            "exact non-root runner identity, primary group, and home match"
        }
        FreshObservationRequirement::RunnerRuntimeDirectory { .. } => {
            "runtime directory derived from the fresh UID has reviewed ownership and mode"
        }
    }
}

const fn resource_name(resource: HostPreparationResource) -> &'static str {
    match resource {
        HostPreparationResource::HostReadinessReport => "host readiness report",
        HostPreparationResource::ExactExecutables => "exact executables",
        HostPreparationResource::DebianPackages => "Debian packages",
        HostPreparationResource::RunnerAccount => "runner account",
        HostPreparationResource::SubordinateUids => "subordinate UIDs",
        HostPreparationResource::SubordinateGids => "subordinate GIDs",
        HostPreparationResource::RootlessPodman => "rootless Podman",
        HostPreparationResource::DurableLanePlan => "durable lane plan",
    }
}
