use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::debian_package_plan::{DebianPackagePlan, PackagePlanDisposition};
use crate::runner_account_observation::ObservedRunnerIdentity;
use crate::runner_account_plan::{PreparationObservationState, RunnerAccountObservations};

mod support;
#[cfg(test)]
mod tests;

use support::{
    ExecutableProbe, LinuxRuntimeFilesystem, RuntimeFilesystem, RuntimeIdentity, RuntimePathKind,
    RuntimePathObservation, canonical_non_root_path, verify_reviewed_executable,
};

pub const ROOTLESS_PODMAN_PREFLIGHT_SCHEMA_VERSION: u8 = 1;

const REVIEWED_EXECUTABLES: [(&str, &str); 8] = [
    ("podman", "/usr/bin/podman"),
    ("runuser", "/usr/sbin/runuser"),
    ("env", "/usr/bin/env"),
    ("systemctl", "/usr/bin/systemctl"),
    ("newuidmap", "/usr/bin/newuidmap"),
    ("newgidmap", "/usr/bin/newgidmap"),
    ("slirp4netns", "/usr/bin/slirp4netns"),
    ("fuse-overlayfs", "/usr/bin/fuse-overlayfs"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RootlessPodmanPreflightState {
    Matching,
    Absent,
    Unknown,
    Conflicting,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RootlessPodmanPreflightDisposition {
    ReadyForSmokeVerification,
    ChangesRequired,
    NeedsInspection,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RootlessPodmanPreflightObservation {
    pub state: RootlessPodmanPreflightState,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RootlessPodmanExecutableObservation {
    pub name: String,
    pub path: PathBuf,
    pub state: RootlessPodmanPreflightState,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RootlessPodmanStaticPreflightReport {
    pub schema_version: u8,
    pub disposition: RootlessPodmanPreflightDisposition,
    pub packages: RootlessPodmanPreflightObservation,
    pub runner_account: RootlessPodmanPreflightObservation,
    pub runtime_directory: RootlessPodmanPreflightObservation,
    pub executables: Vec<RootlessPodmanExecutableObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RootlessPodmanPreflightPaths {
    runtime_root: PathBuf,
}

impl RootlessPodmanPreflightPaths {
    #[must_use]
    pub fn system_default() -> Self {
        Self {
            runtime_root: "/run/user".into(),
        }
    }

    /// Build relocated static-preflight paths for an explicitly trusted host root.
    ///
    /// # Errors
    ///
    /// Returns an error unless the runtime root is a canonical non-root absolute path.
    pub fn new(runtime_root: impl Into<PathBuf>) -> Result<Self, String> {
        let runtime_root = canonical_non_root_path(runtime_root.into())?;
        Ok(Self { runtime_root })
    }

    #[must_use]
    pub fn runtime_directory(&self, uid: u32) -> PathBuf {
        self.runtime_root.join(uid.to_string())
    }
}

/// Inspect non-mutating prerequisites for a later rootless Podman smoke verification.
///
/// This function never executes Podman or another child process. It classifies the existing package
/// plan, runner-account evidence, exact helper metadata, and the runner runtime directory. A
/// matching report proves only that an explicit, journaled first-run smoke action may be planned.
#[must_use]
pub fn observe_rootless_podman_static_preflight(
    package_plan: &DebianPackagePlan,
    account_observations: &RunnerAccountObservations,
    identity: Option<ObservedRunnerIdentity>,
    paths: &RootlessPodmanPreflightPaths,
) -> RootlessPodmanStaticPreflightReport {
    let runtime_identity = identity.map(|identity| RuntimeIdentity {
        uid: identity.uid(),
    });
    observe_with(
        package_plan,
        account_observations,
        runtime_identity,
        paths,
        &LinuxRuntimeFilesystem,
        &verify_reviewed_executable,
    )
}

fn observe_with(
    package_plan: &DebianPackagePlan,
    account_observations: &RunnerAccountObservations,
    identity: Option<RuntimeIdentity>,
    paths: &RootlessPodmanPreflightPaths,
    filesystem: &impl RuntimeFilesystem,
    executable_probe: &impl Fn(&Path) -> ExecutableProbe,
) -> RootlessPodmanStaticPreflightReport {
    let packages = classify_packages(package_plan);
    let runner_account = classify_runner_account(account_observations, identity);
    let runtime_directory = classify_runtime_directory(identity, paths, filesystem);
    let executables = REVIEWED_EXECUTABLES
        .into_iter()
        .map(|(name, path)| {
            let path = PathBuf::from(path);
            let probe = executable_probe(&path);
            RootlessPodmanExecutableObservation {
                name: name.to_owned(),
                path,
                state: probe.state,
                evidence: probe.evidence,
            }
        })
        .collect::<Vec<_>>();

    let disposition = disposition(
        [&packages, &runner_account, &runtime_directory]
            .into_iter()
            .map(|observation| observation.state)
            .chain(executables.iter().map(|observation| observation.state)),
    );

    RootlessPodmanStaticPreflightReport {
        schema_version: ROOTLESS_PODMAN_PREFLIGHT_SCHEMA_VERSION,
        disposition,
        packages,
        runner_account,
        runtime_directory,
        executables,
    }
}

fn classify_packages(package_plan: &DebianPackagePlan) -> RootlessPodmanPreflightObservation {
    match package_plan.disposition {
        PackagePlanDisposition::Ready => observation(
            RootlessPodmanPreflightState::Matching,
            "all reviewed rootless Podman prerequisite packages are present",
        ),
        PackagePlanDisposition::Required => observation(
            RootlessPodmanPreflightState::Absent,
            format!(
                "reviewed prerequisite packages are absent: {}",
                package_plan
                    .missing_packages
                    .iter()
                    .map(|package| package.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ),
        PackagePlanDisposition::NeedsInspection => observation(
            RootlessPodmanPreflightState::Unknown,
            format!(
                "reviewed prerequisite package state is unknown: {}",
                package_plan
                    .unknown_packages
                    .iter()
                    .map(|package| package.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ),
    }
}

fn classify_runner_account(
    observations: &RunnerAccountObservations,
    identity: Option<RuntimeIdentity>,
) -> RootlessPodmanPreflightObservation {
    let states = [
        observations.group.state(),
        observations.user.state(),
        observations.home.state(),
        observations.subordinate_uids.state(),
        observations.subordinate_gids.state(),
        observations.linger.state(),
    ];

    if states.contains(&PreparationObservationState::Conflicting) {
        observation(
            RootlessPodmanPreflightState::Conflicting,
            "runner account preparation evidence conflicts with the reviewed policy",
        )
    } else if states.contains(&PreparationObservationState::Unknown) {
        observation(
            RootlessPodmanPreflightState::Unknown,
            "runner account preparation evidence is incomplete",
        )
    } else if states.contains(&PreparationObservationState::Absent) {
        observation(
            RootlessPodmanPreflightState::Absent,
            "runner account preparation is not complete",
        )
    } else if identity.is_none() {
        observation(
            RootlessPodmanPreflightState::Unknown,
            "matching runner account observations did not include a resolved non-root identity",
        )
    } else {
        observation(
            RootlessPodmanPreflightState::Matching,
            "runner account, home, subordinate IDs, and linger match the reviewed policy",
        )
    }
}

fn classify_runtime_directory(
    identity: Option<RuntimeIdentity>,
    paths: &RootlessPodmanPreflightPaths,
    filesystem: &impl RuntimeFilesystem,
) -> RootlessPodmanPreflightObservation {
    let Some(identity) = identity else {
        return observation(
            RootlessPodmanPreflightState::Blocked,
            "runtime-directory inspection is blocked until the exact runner UID is known",
        );
    };
    if identity.uid == 0 {
        return observation(
            RootlessPodmanPreflightState::Conflicting,
            "runner runtime directory cannot be associated with UID zero",
        );
    }

    let path = paths.runtime_directory(identity.uid);
    match filesystem.inspect(&path) {
        RuntimePathObservation::Missing => observation(
            RootlessPodmanPreflightState::Absent,
            format!("runner runtime directory {} is absent", path.display()),
        ),
        RuntimePathObservation::Unknown => observation(
            RootlessPodmanPreflightState::Unknown,
            format!(
                "runner runtime directory {} could not be inspected safely",
                path.display()
            ),
        ),
        RuntimePathObservation::Present(metadata)
            if metadata.kind == RuntimePathKind::Directory
                && metadata.uid == identity.uid
                && metadata.mode & 0o022 == 0 =>
        {
            observation(
                RootlessPodmanPreflightState::Matching,
                format!(
                    "runner runtime directory {} is owned by UID {} and is not writable by group or others",
                    path.display(),
                    identity.uid
                ),
            )
        }
        RuntimePathObservation::Present(_) => observation(
            RootlessPodmanPreflightState::Conflicting,
            format!(
                "runner runtime directory {} has incompatible type, ownership, or mode",
                path.display()
            ),
        ),
    }
}

fn disposition(
    states: impl IntoIterator<Item = RootlessPodmanPreflightState>,
) -> RootlessPodmanPreflightDisposition {
    let states = states.into_iter().collect::<Vec<_>>();
    if states.contains(&RootlessPodmanPreflightState::Conflicting) {
        RootlessPodmanPreflightDisposition::Blocked
    } else if states.contains(&RootlessPodmanPreflightState::Unknown) {
        RootlessPodmanPreflightDisposition::NeedsInspection
    } else if states.contains(&RootlessPodmanPreflightState::Absent) {
        RootlessPodmanPreflightDisposition::ChangesRequired
    } else if states.contains(&RootlessPodmanPreflightState::Blocked) {
        RootlessPodmanPreflightDisposition::NeedsInspection
    } else {
        RootlessPodmanPreflightDisposition::ReadyForSmokeVerification
    }
}

fn observation(
    state: RootlessPodmanPreflightState,
    evidence: impl Into<String>,
) -> RootlessPodmanPreflightObservation {
    RootlessPodmanPreflightObservation {
        state,
        evidence: vec![evidence.into()],
    }
}
