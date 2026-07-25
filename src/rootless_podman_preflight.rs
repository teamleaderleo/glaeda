use std::path::{Component, Path, PathBuf};

use rustix::fs::{self as rustix_fs, FileType, Mode, OFlags};
use rustix::io::Errno;
use serde::Serialize;

use crate::debian_package_plan::{DebianPackagePlan, PackagePlanDisposition};
use crate::lane_executable::{ExecutableVerificationErrorKind, verify_executable};
use crate::runner_account_observation::ObservedRunnerIdentity;
use crate::runner_account_plan::{PreparationObservationState, RunnerAccountObservations};

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
                    path.display(), identity.uid
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

fn verify_reviewed_executable(path: &Path) -> ExecutableProbe {
    match verify_executable(path) {
        Ok(verified) => ExecutableProbe {
            state: RootlessPodmanPreflightState::Matching,
            evidence: vec![format!(
                "reviewed executable {} is a protected root-owned file with mode {:04o}",
                verified.path().display(),
                verified.mode()
            )],
        },
        Err(error) => {
            let state = match error.kind() {
                ExecutableVerificationErrorKind::Missing => RootlessPodmanPreflightState::Absent,
                ExecutableVerificationErrorKind::Metadata => RootlessPodmanPreflightState::Unknown,
                ExecutableVerificationErrorKind::Symlink
                | ExecutableVerificationErrorKind::NonRegularFile
                | ExecutableVerificationErrorKind::WrongOwner
                | ExecutableVerificationErrorKind::WritableByNonOwner
                | ExecutableVerificationErrorKind::NotExecutable => {
                    RootlessPodmanPreflightState::Conflicting
                }
            };
            ExecutableProbe {
                state,
                evidence: vec![error.message().to_owned()],
            }
        }
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

fn canonical_non_root_path(path: PathBuf) -> Result<PathBuf, String> {
    let Some(value) = path.to_str() else {
        return Err("runtime root must be valid UTF-8".to_owned());
    };
    if value.is_empty()
        || value == "/"
        || value.len() > 4_096
        || value.ends_with('/')
        || value.chars().any(char::is_control)
        || !path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err("runtime root must be a canonical non-root absolute path".to_owned());
    }
    Ok(path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RuntimeIdentity {
    uid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExecutableProbe {
    state: RootlessPodmanPreflightState,
    evidence: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimePathKind {
    Directory,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RuntimePathMetadata {
    kind: RuntimePathKind,
    uid: u32,
    mode: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimePathObservation {
    Missing,
    Present(RuntimePathMetadata),
    Unknown,
}

trait RuntimeFilesystem {
    fn inspect(&self, path: &Path) -> RuntimePathObservation;
}

struct LinuxRuntimeFilesystem;

impl RuntimeFilesystem for LinuxRuntimeFilesystem {
    fn inspect(&self, path: &Path) -> RuntimePathObservation {
        let descriptor = match open_traversed(path) {
            Ok(descriptor) => descriptor,
            Err(Errno::NOENT) => return RuntimePathObservation::Missing,
            Err(_) => return RuntimePathObservation::Unknown,
        };
        let stat = match rustix_fs::fstat(&descriptor) {
            Ok(stat) => stat,
            Err(_) => return RuntimePathObservation::Unknown,
        };
        let kind = match FileType::from_raw_mode(stat.st_mode) {
            FileType::Directory => RuntimePathKind::Directory,
            _ => RuntimePathKind::Other,
        };
        RuntimePathObservation::Present(RuntimePathMetadata {
            kind,
            uid: stat.st_uid,
            mode: stat.st_mode & 0o7777,
        })
    }
}

fn open_traversed(path: &Path) -> Result<std::os::fd::OwnedFd, Errno> {
    let mut components = path.components();
    if components.next() != Some(Component::RootDir) {
        return Err(Errno::INVAL);
    }
    let mut current = rustix_fs::open(
        "/",
        OFlags::PATH.union(OFlags::DIRECTORY).union(OFlags::CLOEXEC),
        Mode::empty(),
    )?;
    let mut remaining = components.peekable();
    while let Some(component) = remaining.next() {
        let Component::Normal(name) = component else {
            return Err(Errno::INVAL);
        };
        let flags = if remaining.peek().is_some() {
            OFlags::PATH
                .union(OFlags::DIRECTORY)
                .union(OFlags::NOFOLLOW)
                .union(OFlags::CLOEXEC)
        } else {
            OFlags::PATH.union(OFlags::NOFOLLOW).union(OFlags::CLOEXEC)
        };
        current = rustix_fs::openat(&current, name, flags, Mode::empty())?;
    }
    Ok(current)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use crate::debian_package_plan::{build_package_plan, parse_os_release};
    use crate::host::Presence;
    use crate::runner_account_plan::{
        PreparationObservation, PreparationObservationState, RunnerAccountObservations,
    };

    use super::{
        ExecutableProbe, RootlessPodmanPreflightDisposition, RootlessPodmanPreflightPaths,
        RootlessPodmanPreflightState, RuntimeFilesystem, RuntimeIdentity, RuntimePathKind,
        RuntimePathMetadata, RuntimePathObservation, observe_with,
    };

    struct FakeFilesystem {
        observation: RuntimePathObservation,
    }

    impl RuntimeFilesystem for FakeFilesystem {
        fn inspect(&self, _path: &Path) -> RuntimePathObservation {
            self.observation
        }
    }

    fn package_plan(state: Presence) -> crate::debian_package_plan::DebianPackagePlan {
        let distribution =
            parse_os_release("ID=ubuntu\nVERSION_ID=24.04\n").expect("distribution");
        let observed = [
            "git",
            "podman",
            "uidmap",
            "slirp4netns",
            "fuse-overlayfs",
            "dbus-user-session",
        ]
        .into_iter()
        .map(|package| (package.to_owned(), state))
        .collect::<BTreeMap<_, _>>();
        build_package_plan(distribution, &observed).expect("package plan")
    }

    fn account_observations(state: PreparationObservationState) -> RunnerAccountObservations {
        let make = || {
            PreparationObservation::new(state, ["bounded evidence"]).expect("observation")
        };
        RunnerAccountObservations {
            group: make(),
            user: make(),
            home: make(),
            subordinate_uids: make(),
            subordinate_gids: make(),
            linger: make(),
        }
    }

    fn matching_executable(_path: &Path) -> ExecutableProbe {
        ExecutableProbe {
            state: RootlessPodmanPreflightState::Matching,
            evidence: vec!["matching executable".to_owned()],
        }
    }

    #[test]
    fn matching_static_state_is_ready_only_for_later_smoke_verification() {
        let report = observe_with(
            &package_plan(Presence::Present),
            &account_observations(PreparationObservationState::Matching),
            Some(RuntimeIdentity { uid: 1001 }),
            &RootlessPodmanPreflightPaths::new("/run/user").expect("paths"),
            &FakeFilesystem {
                observation: RuntimePathObservation::Present(RuntimePathMetadata {
                    kind: RuntimePathKind::Directory,
                    uid: 1001,
                    mode: 0o700,
                }),
            },
            &matching_executable,
        );

        assert_eq!(
            report.disposition,
            RootlessPodmanPreflightDisposition::ReadyForSmokeVerification
        );
        assert_eq!(report.executables.len(), 8);
        assert!(report.executables.iter().all(|item| {
            item.state == RootlessPodmanPreflightState::Matching
                && item.path.is_absolute()
                && item.path != PathBuf::from("podman")
        }));
    }

    #[test]
    fn unknown_account_blocks_runtime_inspection_and_fails_closed() {
        let report = observe_with(
            &package_plan(Presence::Present),
            &account_observations(PreparationObservationState::Unknown),
            None,
            &RootlessPodmanPreflightPaths::system_default(),
            &FakeFilesystem {
                observation: RuntimePathObservation::Present(RuntimePathMetadata {
                    kind: RuntimePathKind::Directory,
                    uid: 1001,
                    mode: 0o700,
                }),
            },
            &matching_executable,
        );

        assert_eq!(
            report.runner_account.state,
            RootlessPodmanPreflightState::Unknown
        );
        assert_eq!(
            report.runtime_directory.state,
            RootlessPodmanPreflightState::Blocked
        );
        assert_eq!(
            report.disposition,
            RootlessPodmanPreflightDisposition::NeedsInspection
        );
    }

    #[test]
    fn conflicting_runtime_directory_blocks_preflight() {
        let report = observe_with(
            &package_plan(Presence::Present),
            &account_observations(PreparationObservationState::Matching),
            Some(RuntimeIdentity { uid: 1001 }),
            &RootlessPodmanPreflightPaths::system_default(),
            &FakeFilesystem {
                observation: RuntimePathObservation::Present(RuntimePathMetadata {
                    kind: RuntimePathKind::Directory,
                    uid: 2000,
                    mode: 0o777,
                }),
            },
            &matching_executable,
        );

        assert_eq!(
            report.runtime_directory.state,
            RootlessPodmanPreflightState::Conflicting
        );
        assert_eq!(
            report.disposition,
            RootlessPodmanPreflightDisposition::Blocked
        );
    }

    #[test]
    fn missing_packages_and_helpers_require_changes_without_running_podman() {
        let report = observe_with(
            &package_plan(Presence::Absent),
            &account_observations(PreparationObservationState::Absent),
            None,
            &RootlessPodmanPreflightPaths::system_default(),
            &FakeFilesystem {
                observation: RuntimePathObservation::Missing,
            },
            &|path| ExecutableProbe {
                state: if path == Path::new("/usr/bin/podman") {
                    RootlessPodmanPreflightState::Absent
                } else {
                    RootlessPodmanPreflightState::Matching
                },
                evidence: vec!["fixed fake metadata result".to_owned()],
            },
        );

        assert_eq!(
            report.disposition,
            RootlessPodmanPreflightDisposition::ChangesRequired
        );
        assert_eq!(
            report.executables[0].state,
            RootlessPodmanPreflightState::Absent
        );
    }

    #[test]
    fn unsafe_runtime_root_is_rejected() {
        for path in ["run/user", "/", "/run/../user", "/run/user/"] {
            assert!(RootlessPodmanPreflightPaths::new(path).is_err());
        }
    }
}
