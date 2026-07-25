use std::fmt;
use std::io::Read as _;
use std::os::fd::OwnedFd;
use std::path::{Component, Path, PathBuf};

use rustix::fs::{self as rustix_fs, FileType, Mode, OFlags};
use rustix::io::Errno;
use serde::Serialize;

use crate::debian_package_plan::DebianPackagePlan;
use crate::rootless_podman_config::{
    MAX_ROOTLESS_PODMAN_CONFIG_BYTES, RootlessPodmanConfigError, RootlessPodmanConfigErrorKind,
    RootlessPodmanContainersConfig, RootlessPodmanStorageConfig,
    parse_rootless_podman_containers_config, parse_rootless_podman_storage_config,
};
use crate::rootless_podman_config_resolution::{
    RootlessPodmanConfigAssessment, RootlessPodmanConfigContext, RootlessPodmanConfigPolicy,
    RootlessPodmanConfigSource, RootlessPodmanConfigSourceState, RootlessPodmanResolvedConfig,
    assess_rootless_podman_config, resolve_rootless_podman_config,
};
use crate::rootless_podman_preflight::{
    RootlessPodmanPreflightPaths, RootlessPodmanStaticPreflightReport,
    observe_rootless_podman_static_preflight,
};
use crate::runner_account_observation::ObservedRunnerIdentity;
use crate::runner_account_plan::{PreparationObservationState, RunnerAccountObservations};

pub const ROOTLESS_PODMAN_CONFIG_OBSERVATION_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RootlessPodmanConfigObservationPaths {
    vendor_containers: PathBuf,
    system_containers: PathBuf,
    system_storage: PathBuf,
}

impl RootlessPodmanConfigObservationPaths {
    #[must_use]
    pub fn system_default() -> Self {
        Self {
            vendor_containers: "/usr/share/containers/containers.conf".into(),
            system_containers: "/etc/containers/containers.conf".into(),
            system_storage: "/etc/containers/storage.conf".into(),
        }
    }

    /// Build relocated system configuration paths for an explicitly trusted host root.
    ///
    /// Runner-specific paths are always derived from the reviewed runner home and cannot be
    /// supplied independently.
    ///
    /// # Errors
    ///
    /// Returns an error unless every path is a canonical non-root absolute path.
    pub fn new(
        vendor_containers: impl Into<PathBuf>,
        system_containers: impl Into<PathBuf>,
        system_storage: impl Into<PathBuf>,
    ) -> Result<Self, RootlessPodmanConfigObservationError> {
        Ok(Self {
            vendor_containers: canonical_observation_path(
                "vendor containers configuration",
                vendor_containers.into(),
            )?,
            system_containers: canonical_observation_path(
                "system containers configuration",
                system_containers.into(),
            )?,
            system_storage: canonical_observation_path(
                "system storage configuration",
                system_storage.into(),
            )?,
        })
    }

    #[must_use]
    pub fn vendor_containers(&self) -> &Path {
        &self.vendor_containers
    }

    #[must_use]
    pub fn system_containers(&self) -> &Path {
        &self.system_containers
    }

    #[must_use]
    pub fn system_storage(&self) -> &Path {
        &self.system_storage
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RootlessPodmanConfigSourceRole {
    VendorContainers,
    SystemContainers,
    RunnerContainers,
    SystemStorage,
    RunnerStorage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RootlessPodmanConfigSourceObservationState {
    Missing,
    Present,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RootlessPodmanConfigSourceObservation {
    pub role: RootlessPodmanConfigSourceRole,
    pub path: PathBuf,
    pub state: RootlessPodmanConfigSourceObservationState,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RootlessPodmanConfigObservationReport {
    pub schema_version: u8,
    pub sources: Vec<RootlessPodmanConfigSourceObservation>,
    pub resolved: RootlessPodmanResolvedConfig,
    pub assessment: RootlessPodmanConfigAssessment,
}

impl RootlessPodmanConfigObservationReport {
    #[must_use]
    pub fn assessment(&self) -> &RootlessPodmanConfigAssessment {
        &self.assessment
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RootlessPodmanObservedStaticPreflightReport {
    pub schema_version: u8,
    pub configuration: RootlessPodmanConfigObservationReport,
    pub preflight: RootlessPodmanStaticPreflightReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RootlessPodmanConfigObservationError {
    pub problems: Vec<String>,
}

impl RootlessPodmanConfigObservationError {
    fn single(problem: impl Into<String>) -> Self {
        Self {
            problems: vec![problem.into()],
        }
    }

    fn from_problem(problem: String) -> Self {
        Self::single(problem)
    }
}

impl fmt::Display for RootlessPodmanConfigObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "rootless Podman configuration observation failed")?;
        for problem in &self.problems {
            writeln!(formatter, "- {problem}")?;
        }
        Ok(())
    }
}

impl std::error::Error for RootlessPodmanConfigObservationError {}

/// Observe reviewed rootless Podman configuration sources without invoking Podman or a child process.
///
/// System files must be root-owned. Runner files are inspected only after the exact non-root runner
/// identity and a matching reviewed home have been proven. Missing, present, and unknown source
/// states remain distinct through parsing, precedence resolution, and policy assessment.
///
/// # Errors
///
/// Returns an error only when caller-supplied reviewed paths or the derived XDG context are invalid.
pub fn observe_rootless_podman_configuration(
    account_observations: &RunnerAccountObservations,
    identity: Option<ObservedRunnerIdentity>,
    reviewed_home: &Path,
    policy: &RootlessPodmanConfigPolicy,
    paths: &RootlessPodmanConfigObservationPaths,
) -> Result<RootlessPodmanConfigObservationReport, RootlessPodmanConfigObservationError> {
    let identity = identity.map(RunnerConfigIdentity::from);
    observe_with(
        account_observations,
        identity,
        reviewed_home,
        policy,
        paths,
        &LinuxConfigFilesystem,
    )
}

/// Observe configuration and compose it into the existing static-preflight report.
///
/// This entrypoint performs bounded filesystem inspection only. It accepts no command executor and
/// never invokes Podman, user services, a shell, or another child process.
///
/// # Errors
///
/// Returns an error when the reviewed configuration path context is invalid.
pub fn observe_rootless_podman_static_preflight_from_sources(
    package_plan: &DebianPackagePlan,
    account_observations: &RunnerAccountObservations,
    identity: Option<ObservedRunnerIdentity>,
    reviewed_home: &Path,
    policy: &RootlessPodmanConfigPolicy,
    config_paths: &RootlessPodmanConfigObservationPaths,
    preflight_paths: &RootlessPodmanPreflightPaths,
) -> Result<RootlessPodmanObservedStaticPreflightReport, RootlessPodmanConfigObservationError> {
    let configuration = observe_rootless_podman_configuration(
        account_observations,
        identity,
        reviewed_home,
        policy,
        config_paths,
    )?;
    let preflight = observe_rootless_podman_static_preflight(
        package_plan,
        account_observations,
        identity,
        configuration.assessment(),
        preflight_paths,
    );
    Ok(RootlessPodmanObservedStaticPreflightReport {
        schema_version: ROOTLESS_PODMAN_CONFIG_OBSERVATION_SCHEMA_VERSION,
        configuration,
        preflight,
    })
}

#[must_use]
pub fn render_human(report: &RootlessPodmanConfigObservationReport) -> String {
    let mut output = format!(
        "Rootless Podman configuration assessment: {}\nSources:\n",
        assessment_state_name(report.assessment.state)
    );
    for source in &report.sources {
        output.push_str(&format!(
            "- {} {}: {}\n",
            source_role_name(source.role),
            source.path.display(),
            source_state_name(source.state)
        ));
        for evidence in &source.evidence {
            output.push_str(&format!("  - {evidence}\n"));
        }
    }
    output
}

fn observe_with(
    account_observations: &RunnerAccountObservations,
    identity: Option<RunnerConfigIdentity>,
    reviewed_home: &Path,
    policy: &RootlessPodmanConfigPolicy,
    paths: &RootlessPodmanConfigObservationPaths,
    filesystem: &impl ConfigFilesystem,
) -> Result<RootlessPodmanConfigObservationReport, RootlessPodmanConfigObservationError> {
    let home = canonical_observation_path("reviewed runner home", reviewed_home.to_path_buf())?;
    let trusted_identity = trusted_runner_identity(account_observations, identity);
    let context_identity = trusted_identity.unwrap_or(RunnerConfigIdentity {
        uid: u32::MAX,
        gid: u32::MAX,
        group_gid: u32::MAX,
    });
    let runtime_uid = context_identity.uid;
    let context = RootlessPodmanConfigContext::new(
        home.clone(),
        home.join(".config"),
        home.join(".local/share"),
        PathBuf::from(format!("/run/user/{runtime_uid}")),
    )
    .map_err(|problem| {
        RootlessPodmanConfigObservationError::single(format!(
            "reviewed rootless Podman path context is invalid: {problem}"
        ))
    })?;
    let runner_containers_path = context
        .xdg_config_home()
        .join("containers/containers.conf");
    let runner_storage_path = context.xdg_config_home().join("containers/storage.conf");

    let vendor_containers = observe_source(
        RootlessPodmanConfigSourceRole::VendorContainers,
        paths.vendor_containers(),
        ExpectedOwner::Root,
        filesystem,
        parse_rootless_podman_containers_config,
    )?;
    let system_containers = observe_source(
        RootlessPodmanConfigSourceRole::SystemContainers,
        paths.system_containers(),
        ExpectedOwner::Root,
        filesystem,
        parse_rootless_podman_containers_config,
    )?;
    let system_storage = observe_source(
        RootlessPodmanConfigSourceRole::SystemStorage,
        paths.system_storage(),
        ExpectedOwner::Root,
        filesystem,
        parse_rootless_podman_storage_config,
    )?;

    let runner_containers = match trusted_identity {
        Some(identity) => observe_source(
            RootlessPodmanConfigSourceRole::RunnerContainers,
            &runner_containers_path,
            ExpectedOwner::Runner(identity),
            filesystem,
            parse_rootless_podman_containers_config,
        )?,
        None => blocked_runner_source(
            RootlessPodmanConfigSourceRole::RunnerContainers,
            &runner_containers_path,
        )?,
    };
    let runner_storage = match trusted_identity {
        Some(identity) => observe_source(
            RootlessPodmanConfigSourceRole::RunnerStorage,
            &runner_storage_path,
            ExpectedOwner::Runner(identity),
            filesystem,
            parse_rootless_podman_storage_config,
        )?,
        None => blocked_runner_source(
            RootlessPodmanConfigSourceRole::RunnerStorage,
            &runner_storage_path,
        )?,
    };

    let resolved = resolve_rootless_podman_config(
        &context,
        &vendor_containers.source,
        &system_containers.source,
        &runner_containers.source,
        &system_storage.source,
        &runner_storage.source,
    );
    let assessment = assess_rootless_podman_config(&resolved, policy);

    Ok(RootlessPodmanConfigObservationReport {
        schema_version: ROOTLESS_PODMAN_CONFIG_OBSERVATION_SCHEMA_VERSION,
        sources: vec![
            vendor_containers.observation,
            system_containers.observation,
            runner_containers.observation,
            system_storage.observation,
            runner_storage.observation,
        ],
        resolved,
        assessment,
    })
}

fn trusted_runner_identity(
    observations: &RunnerAccountObservations,
    identity: Option<RunnerConfigIdentity>,
) -> Option<RunnerConfigIdentity> {
    identity.filter(|identity| {
        identity.uid != 0
            && identity.gid != 0
            && identity.gid == identity.group_gid
            && observations.home.state() == PreparationObservationState::Matching
    })
}

struct ObservedConfigSource<T> {
    source: RootlessPodmanConfigSource<T>,
    observation: RootlessPodmanConfigSourceObservation,
}

fn observe_source<T>(
    role: RootlessPodmanConfigSourceRole,
    path: &Path,
    owner: ExpectedOwner,
    filesystem: &impl ConfigFilesystem,
    parser: fn(&str) -> Result<T, RootlessPodmanConfigError>,
) -> Result<ObservedConfigSource<T>, RootlessPodmanConfigObservationError> {
    let read = filesystem.read_trusted(path, owner, MAX_ROOTLESS_PODMAN_CONFIG_BYTES);
    let (state, observation_state, evidence) = match read {
        TrustedConfigFile::Missing => (
            RootlessPodmanConfigSourceState::Missing,
            RootlessPodmanConfigSourceObservationState::Missing,
            "reviewed configuration source is absent".to_owned(),
        ),
        TrustedConfigFile::Unknown(problem) => {
            let evidence = problem.evidence().to_owned();
            (
                RootlessPodmanConfigSourceState::Unknown {
                    evidence: evidence.clone(),
                },
                RootlessPodmanConfigSourceObservationState::Unknown,
                evidence,
            )
        }
        TrustedConfigFile::Present(bytes) => match String::from_utf8(bytes) {
            Ok(input) => match parser(&input) {
                Ok(parsed) => (
                    RootlessPodmanConfigSourceState::Present(parsed),
                    RootlessPodmanConfigSourceObservationState::Present,
                    "source metadata, ownership, size, UTF-8, and relevant syntax are valid"
                        .to_owned(),
                ),
                Err(error) => {
                    let evidence = parse_failure_evidence(role, &error);
                    (
                        RootlessPodmanConfigSourceState::Unknown {
                            evidence: evidence.clone(),
                        },
                        RootlessPodmanConfigSourceObservationState::Unknown,
                        evidence,
                    )
                }
            },
            Err(_) => {
                let evidence =
                    "reviewed configuration source is not valid UTF-8".to_owned();
                (
                    RootlessPodmanConfigSourceState::Unknown {
                        evidence: evidence.clone(),
                    },
                    RootlessPodmanConfigSourceObservationState::Unknown,
                    evidence,
                )
            }
        },
    };
    let source = RootlessPodmanConfigSource::new(path.to_path_buf(), state).map_err(|problem| {
        RootlessPodmanConfigObservationError::single(format!(
            "reviewed configuration source path or evidence is invalid: {problem}"
        ))
    })?;
    Ok(ObservedConfigSource {
        source,
        observation: RootlessPodmanConfigSourceObservation {
            role,
            path: path.to_path_buf(),
            state: observation_state,
            evidence: vec![evidence],
        },
    })
}

fn blocked_runner_source<T>(
    role: RootlessPodmanConfigSourceRole,
    path: &Path,
) -> Result<ObservedConfigSource<T>, RootlessPodmanConfigObservationError> {
    let evidence = "runner-specific source inspection is blocked until an exact non-root runner identity and matching reviewed home are proven".to_owned();
    let source = RootlessPodmanConfigSource::new(
        path.to_path_buf(),
        RootlessPodmanConfigSourceState::Unknown {
            evidence: evidence.clone(),
        },
    )
    .map_err(RootlessPodmanConfigObservationError::from_problem)?;
    Ok(ObservedConfigSource {
        source,
        observation: RootlessPodmanConfigSourceObservation {
            role,
            path: path.to_path_buf(),
            state: RootlessPodmanConfigSourceObservationState::Unknown,
            evidence: vec![evidence],
        },
    })
}

fn parse_failure_evidence(
    role: RootlessPodmanConfigSourceRole,
    error: &RootlessPodmanConfigError,
) -> String {
    let line = error
        .line()
        .map_or_else(String::new, |line| format!(" at line {line}"));
    format!(
        "{} source has {}{}",
        source_role_name(role),
        config_error_kind_name(error.kind()),
        line
    )
}

fn canonical_observation_path(
    field: &str,
    path: PathBuf,
) -> Result<PathBuf, RootlessPodmanConfigObservationError> {
    let Some(value) = path.to_str() else {
        return Err(RootlessPodmanConfigObservationError::single(format!(
            "{field} must be valid UTF-8"
        )));
    };
    if value.is_empty()
        || value == "/"
        || value.len() > 4_096
        || value.ends_with('/')
        || value.contains("//")
        || value.chars().any(char::is_control)
        || !path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(RootlessPodmanConfigObservationError::single(format!(
            "{field} must be a canonical non-root absolute path"
        )));
    }
    Ok(path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RunnerConfigIdentity {
    uid: u32,
    gid: u32,
    group_gid: u32,
}

impl From<ObservedRunnerIdentity> for RunnerConfigIdentity {
    fn from(identity: ObservedRunnerIdentity) -> Self {
        Self {
            uid: identity.uid(),
            gid: identity.primary_gid(),
            group_gid: identity.group_gid(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpectedOwner {
    Root,
    Runner(RunnerConfigIdentity),
}

impl ExpectedOwner {
    fn matches(self, uid: u32, gid: u32) -> bool {
        match self {
            Self::Root => uid == 0 && gid == 0,
            Self::Runner(identity) => uid == identity.uid && gid == identity.gid,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrustedConfigFileProblem {
    UnsafeTraversal,
    MetadataUnavailable,
    NotRegularFile,
    WrongOwner,
    MultipleLinks,
    WritableByUntrusted,
    Oversized,
    ReadFailed,
}

impl TrustedConfigFileProblem {
    const fn evidence(self) -> &'static str {
        match self {
            Self::UnsafeTraversal => {
                "configuration source could not be opened through the reviewed no-symlink path"
            }
            Self::MetadataUnavailable => {
                "configuration source metadata could not be inspected safely"
            }
            Self::NotRegularFile => "configuration source is not a regular file",
            Self::WrongOwner => "configuration source ownership does not match the reviewed source",
            Self::MultipleLinks => "configuration source does not have exactly one hard link",
            Self::WritableByUntrusted => {
                "configuration source is writable by group or other users"
            }
            Self::Oversized => "configuration source exceeds the bounded size limit",
            Self::ReadFailed => "configuration source could not be read within the bounded limit",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TrustedConfigFile {
    Missing,
    Present(Vec<u8>),
    Unknown(TrustedConfigFileProblem),
}

trait ConfigFilesystem {
    fn read_trusted(
        &self,
        path: &Path,
        owner: ExpectedOwner,
        max_bytes: usize,
    ) -> TrustedConfigFile;
}

struct LinuxConfigFilesystem;

impl ConfigFilesystem for LinuxConfigFilesystem {
    fn read_trusted(
        &self,
        path: &Path,
        owner: ExpectedOwner,
        max_bytes: usize,
    ) -> TrustedConfigFile {
        let descriptor = match open_traversed(path, OFlags::RDONLY) {
            Ok(descriptor) => descriptor,
            Err(Errno::NOENT) => return TrustedConfigFile::Missing,
            Err(_) => {
                return TrustedConfigFile::Unknown(TrustedConfigFileProblem::UnsafeTraversal);
            }
        };
        let stat = match rustix_fs::fstat(&descriptor) {
            Ok(stat) => stat,
            Err(_) => {
                return TrustedConfigFile::Unknown(TrustedConfigFileProblem::MetadataUnavailable);
            }
        };
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
            return TrustedConfigFile::Unknown(TrustedConfigFileProblem::NotRegularFile);
        }
        if !owner.matches(stat.st_uid, stat.st_gid) {
            return TrustedConfigFile::Unknown(TrustedConfigFileProblem::WrongOwner);
        }
        if canonical_nlink(stat.st_nlink) != 1 {
            return TrustedConfigFile::Unknown(TrustedConfigFileProblem::MultipleLinks);
        }
        if stat.st_mode & 0o022 != 0 {
            return TrustedConfigFile::Unknown(TrustedConfigFileProblem::WritableByUntrusted);
        }
        if stat.st_size < 0
            || usize::try_from(stat.st_size).map_or(true, |size| size > max_bytes)
        {
            return TrustedConfigFile::Unknown(TrustedConfigFileProblem::Oversized);
        }

        let mut bytes = Vec::new();
        if std::fs::File::from(descriptor)
            .take((max_bytes as u64).saturating_add(1))
            .read_to_end(&mut bytes)
            .is_err()
            || bytes.len() > max_bytes
        {
            return TrustedConfigFile::Unknown(TrustedConfigFileProblem::ReadFailed);
        }
        TrustedConfigFile::Present(bytes)
    }
}

fn canonical_nlink(value: impl Into<u64>) -> u64 {
    value.into()
}

fn open_traversed(path: &Path, final_flags: OFlags) -> Result<OwnedFd, Errno> {
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
            final_flags.union(OFlags::NOFOLLOW).union(OFlags::CLOEXEC)
        };
        current = rustix_fs::openat(&current, name, flags, Mode::empty())?;
    }
    Ok(current)
}

const fn source_role_name(role: RootlessPodmanConfigSourceRole) -> &'static str {
    match role {
        RootlessPodmanConfigSourceRole::VendorContainers => "vendor containers.conf",
        RootlessPodmanConfigSourceRole::SystemContainers => "system containers.conf",
        RootlessPodmanConfigSourceRole::RunnerContainers => "runner containers.conf",
        RootlessPodmanConfigSourceRole::SystemStorage => "system storage.conf",
        RootlessPodmanConfigSourceRole::RunnerStorage => "runner storage.conf",
    }
}

const fn source_state_name(state: RootlessPodmanConfigSourceObservationState) -> &'static str {
    match state {
        RootlessPodmanConfigSourceObservationState::Missing => "missing",
        RootlessPodmanConfigSourceObservationState::Present => "present",
        RootlessPodmanConfigSourceObservationState::Unknown => "unknown",
    }
}

const fn assessment_state_name(
    state: crate::rootless_podman_config_resolution::RootlessPodmanConfigAssessmentState,
) -> &'static str {
    use crate::rootless_podman_config_resolution::RootlessPodmanConfigAssessmentState;
    match state {
        RootlessPodmanConfigAssessmentState::Matching => "matching",
        RootlessPodmanConfigAssessmentState::Absent => "absent",
        RootlessPodmanConfigAssessmentState::Unknown => "unknown",
        RootlessPodmanConfigAssessmentState::Conflicting => "conflicting",
    }
}

const fn config_error_kind_name(kind: RootlessPodmanConfigErrorKind) -> &'static str {
    match kind {
        RootlessPodmanConfigErrorKind::Oversized => "oversized configuration",
        RootlessPodmanConfigErrorKind::TooManyLines => "too many configuration lines",
        RootlessPodmanConfigErrorKind::LineTooLong => "an oversized configuration line",
        RootlessPodmanConfigErrorKind::InvalidControlCharacter => {
            "an invalid configuration control character"
        }
        RootlessPodmanConfigErrorKind::MalformedTable => "a malformed relevant table",
        RootlessPodmanConfigErrorKind::MalformedRelevantAssignment => {
            "a malformed relevant assignment"
        }
        RootlessPodmanConfigErrorKind::DuplicateRelevantKey => "a duplicate relevant key",
    }
}

#[cfg(test)]
mod tests;
