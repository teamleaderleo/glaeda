use std::fmt;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::debian_package_plan::DebianPackagePlan;
use crate::rootless_podman_config_observation::{
    RootlessPodmanConfigObservationContext, RootlessPodmanConfigObservationPaths,
    RootlessPodmanConfigObservationReport, RootlessPodmanObservedSourceKind,
    RootlessPodmanObservedSourceState, observe_rootless_podman_config,
};
use crate::rootless_podman_config_resolution::RootlessPodmanConfigPolicy;
use crate::rootless_podman_preflight::{
    RootlessPodmanPreflightDisposition, RootlessPodmanPreflightPaths,
    RootlessPodmanPreflightState, RootlessPodmanStaticPreflightReport,
    observe_rootless_podman_static_preflight,
};
use crate::runner_account_observation::{ObservedRunnerIdentity, RunnerAccountObservationReport};
use crate::runner_account_plan::{
    DesiredRunnerAccount, PreparationObservationState, RunnerAccountObservations,
};

pub const HOST_ROOTLESS_PODMAN_SCHEMA_VERSION: u8 = 1;

const STORAGE_DRIVER: &str = "overlay";
const OVERLAY_MOUNT_PROGRAM: &str = "/usr/bin/fuse-overlayfs";
const CGROUP_MANAGER: &str = "systemd";
const NETWORK_BACKEND: &str = "netavark";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "disposition", rename_all = "snake_case")]
pub enum HostRootlessPodmanReadiness {
    Deferred {
        state: RootlessPodmanPreflightState,
        evidence: Vec<String>,
    },
    Observed {
        configuration: Box<RootlessPodmanConfigObservationReport>,
        preflight: Box<RootlessPodmanStaticPreflightReport>,
    },
}

impl HostRootlessPodmanReadiness {
    #[must_use]
    pub fn preflight_disposition(&self) -> Option<RootlessPodmanPreflightDisposition> {
        match self {
            Self::Deferred { .. } => None,
            Self::Observed { preflight, .. } => Some(preflight.disposition),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostRootlessPodmanErrorKind {
    ObservationContext,
    ReviewedPolicy,
    SourceObservation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostRootlessPodmanError {
    kind: HostRootlessPodmanErrorKind,
    public_message: String,
}

impl HostRootlessPodmanError {
    #[must_use]
    pub fn kind(&self) -> HostRootlessPodmanErrorKind {
        self.kind
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.public_message
    }

    fn new(kind: HostRootlessPodmanErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            public_message: message.into(),
        }
    }
}

impl fmt::Display for HostRootlessPodmanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.public_message)
    }
}

impl std::error::Error for HostRootlessPodmanError {}

/// Compose trusted Podman configuration observation into host static preflight.
///
/// The reviewed policy is explicit and code-owned: overlay storage, systemd cgroups, netavark,
/// and the reviewed fuse-overlayfs helper. Graph and runtime roots are derived from the exact
/// reviewed home and observed UID. No value is taken from `podman info` or installed defaults.
///
/// # Errors
///
/// Returns a bounded error only when reviewed context or policy construction fails, or when the
/// trusted source observer cannot represent its normalized result.
pub fn observe_host_rootless_podman(
    package_plan: &DebianPackagePlan,
    desired: &DesiredRunnerAccount,
    account_report: &RunnerAccountObservationReport,
) -> Result<HostRootlessPodmanReadiness, HostRootlessPodmanError> {
    observe_host_rootless_podman_with_paths(
        package_plan,
        desired,
        account_report,
        &RootlessPodmanConfigObservationPaths::system_default(),
        &RootlessPodmanPreflightPaths::system_default(),
    )
}

/// Compose host readiness with explicit relocated paths for deterministic integration tests.
///
/// # Errors
///
/// Returns the same bounded errors as [`observe_host_rootless_podman`].
pub fn observe_host_rootless_podman_with_paths(
    package_plan: &DebianPackagePlan,
    desired: &DesiredRunnerAccount,
    account_report: &RunnerAccountObservationReport,
    config_paths: &RootlessPodmanConfigObservationPaths,
    preflight_paths: &RootlessPodmanPreflightPaths,
) -> Result<HostRootlessPodmanReadiness, HostRootlessPodmanError> {
    let Some(identity) = ready_identity(&account_report.observations, account_report.identity())
    else {
        return Ok(deferred_account_readiness(
            &account_report.observations,
            account_report.identity(),
        ));
    };

    let home = PathBuf::from(desired.home());
    let xdg_config_home = home.join(".config");
    let xdg_data_home = home.join(".local/share");
    let xdg_runtime_dir = PathBuf::from(format!("/run/user/{}", identity.uid()));
    let context = RootlessPodmanConfigObservationContext::new(
        home,
        xdg_config_home,
        xdg_data_home.clone(),
        xdg_runtime_dir.clone(),
        identity.uid(),
        identity.primary_gid(),
    )
    .map_err(|_| {
        HostRootlessPodmanError::new(
            HostRootlessPodmanErrorKind::ObservationContext,
            "failed to construct the reviewed rootless Podman observation context",
        )
    })?;
    let policy = RootlessPodmanConfigPolicy::new(
        STORAGE_DRIVER,
        xdg_data_home.join("containers/storage"),
        xdg_runtime_dir.join("containers"),
        OVERLAY_MOUNT_PROGRAM,
        CGROUP_MANAGER,
        NETWORK_BACKEND,
    )
    .map_err(|_| {
        HostRootlessPodmanError::new(
            HostRootlessPodmanErrorKind::ReviewedPolicy,
            "failed to construct the explicit rootless Podman host policy",
        )
    })?;

    let configuration = observe_rootless_podman_config(&context, config_paths, &policy).map_err(
        |_| {
            HostRootlessPodmanError::new(
                HostRootlessPodmanErrorKind::SourceObservation,
                "failed to observe reviewed rootless Podman configuration sources",
            )
        },
    )?;
    let preflight = observe_rootless_podman_static_preflight(
        package_plan,
        &account_report.observations,
        Some(identity),
        &configuration.assessment,
        preflight_paths,
    );

    Ok(HostRootlessPodmanReadiness::Observed {
        configuration: Box::new(configuration),
        preflight: Box::new(preflight),
    })
}

#[must_use]
pub fn render_human(readiness: &HostRootlessPodmanReadiness) -> String {
    match readiness {
        HostRootlessPodmanReadiness::Deferred { state, evidence } => {
            let mut output = format!(
                "[{}] Rootless Podman configuration observation is deferred.\n",
                preflight_state_marker(*state)
            );
            for item in evidence {
                output.push_str(&format!("  {item}\n"));
            }
            output
        }
        HostRootlessPodmanReadiness::Observed {
            configuration,
            preflight,
        } => {
            let mut output = format!(
                "Static preflight: {}\nConfiguration sources:\n",
                preflight_disposition_name(preflight.disposition)
            );
            for source in &configuration.sources {
                output.push_str(&format!(
                    "[{}] {} at {}",
                    source_state_marker(source.state),
                    source_kind_name(source.kind),
                    source.path.display()
                ));
                if let Some(problem) = source.problem {
                    output.push_str(&format!(" ({problem:?})"));
                }
                output.push('\n');
            }
            for item in &preflight.configuration.evidence {
                output.push_str(&format!("  Configuration: {item}\n"));
            }
            output
        }
    }
}

fn ready_identity(
    observations: &RunnerAccountObservations,
    identity: Option<ObservedRunnerIdentity>,
) -> Option<ObservedRunnerIdentity> {
    let states = account_states(observations);
    identity.filter(|identity| {
        states
            .iter()
            .all(|state| *state == PreparationObservationState::Matching)
            && identity.uid() != 0
            && identity.primary_gid() != 0
            && identity.primary_gid() == identity.group_gid()
    })
}

fn deferred_account_readiness(
    observations: &RunnerAccountObservations,
    identity: Option<ObservedRunnerIdentity>,
) -> HostRootlessPodmanReadiness {
    let states = account_states(observations);
    let (state, evidence) = if states.contains(&PreparationObservationState::Conflicting) {
        (
            RootlessPodmanPreflightState::Conflicting,
            "runner account evidence conflicts with the reviewed policy; Podman configuration sources were not read",
        )
    } else if states.contains(&PreparationObservationState::Unknown) {
        (
            RootlessPodmanPreflightState::Unknown,
            "runner account evidence is incomplete; Podman configuration sources were not read",
        )
    } else if states.contains(&PreparationObservationState::Absent) {
        (
            RootlessPodmanPreflightState::Absent,
            "runner account preparation is incomplete; Podman configuration sources were not read",
        )
    } else if identity.is_none() {
        (
            RootlessPodmanPreflightState::Unknown,
            "matching runner account evidence did not include an exact identity; Podman configuration sources were not read",
        )
    } else {
        (
            RootlessPodmanPreflightState::Conflicting,
            "runner identity is root or its primary group conflicts with the reviewed group; Podman configuration sources were not read",
        )
    };
    HostRootlessPodmanReadiness::Deferred {
        state,
        evidence: vec![evidence.to_owned()],
    }
}

fn account_states(
    observations: &RunnerAccountObservations,
) -> [PreparationObservationState; 6] {
    [
        observations.group.state(),
        observations.user.state(),
        observations.home.state(),
        observations.subordinate_uids.state(),
        observations.subordinate_gids.state(),
        observations.linger.state(),
    ]
}

const fn preflight_state_marker(state: RootlessPodmanPreflightState) -> &'static str {
    match state {
        RootlessPodmanPreflightState::Matching => "READY",
        RootlessPodmanPreflightState::Absent => "REQUIRED",
        RootlessPodmanPreflightState::Unknown => "INSPECT",
        RootlessPodmanPreflightState::Conflicting | RootlessPodmanPreflightState::Blocked => {
            "BLOCKED"
        }
    }
}

const fn source_state_marker(state: RootlessPodmanObservedSourceState) -> &'static str {
    match state {
        RootlessPodmanObservedSourceState::Missing => "MISSING",
        RootlessPodmanObservedSourceState::Present => "PRESENT",
        RootlessPodmanObservedSourceState::Unknown => "UNKNOWN",
    }
}

const fn source_kind_name(kind: RootlessPodmanObservedSourceKind) -> &'static str {
    match kind {
        RootlessPodmanObservedSourceKind::VendorContainers => "vendor containers.conf",
        RootlessPodmanObservedSourceKind::SystemContainers => "system containers.conf",
        RootlessPodmanObservedSourceKind::RunnerContainers => "runner containers.conf",
        RootlessPodmanObservedSourceKind::SystemStorage => "system storage.conf",
        RootlessPodmanObservedSourceKind::RunnerStorage => "runner storage.conf",
    }
}

const fn preflight_disposition_name(
    disposition: RootlessPodmanPreflightDisposition,
) -> &'static str {
    match disposition {
        RootlessPodmanPreflightDisposition::ReadyForSmokeVerification => {
            "ready_for_smoke_verification"
        }
        RootlessPodmanPreflightDisposition::ChangesRequired => "changes_required",
        RootlessPodmanPreflightDisposition::NeedsInspection => "needs_inspection",
        RootlessPodmanPreflightDisposition::Blocked => "blocked",
    }
}

#[cfg(test)]
mod tests;
