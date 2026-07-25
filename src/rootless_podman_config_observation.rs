use std::ffi::OsStr;
use std::fmt;
use std::fs::File;
use std::io::{Read, Take};
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Component, Path, PathBuf};

use rustix::fs::{self, FileType, Mode, OFlags};
use rustix::io::Errno;
use serde::Serialize;

use crate::rootless_podman_config::{
    MAX_ROOTLESS_PODMAN_CONFIG_BYTES, RootlessPodmanConfigError,
    parse_rootless_podman_containers_config, parse_rootless_podman_storage_config,
};
use crate::rootless_podman_config_resolution::{
    RootlessPodmanConfigAssessment, RootlessPodmanConfigContext, RootlessPodmanConfigPolicy,
    RootlessPodmanConfigSource, RootlessPodmanConfigSourceState, assess_rootless_podman_config,
    resolve_rootless_podman_config,
};

pub const ROOTLESS_PODMAN_CONFIG_OBSERVATION_SCHEMA_VERSION: u8 = 1;

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NONBLOCK)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootlessPodmanConfigObservationContext {
    resolution: RootlessPodmanConfigContext,
    runner_uid: u32,
    runner_gid: u32,
}

impl RootlessPodmanConfigObservationContext {
    /// Build one exact runner identity and XDG path context for configuration observation.
    ///
    /// # Errors
    ///
    /// Returns an error when the runner identity is root or the reviewed paths are unsafe.
    pub fn new(
        home: impl Into<PathBuf>,
        xdg_config_home: impl Into<PathBuf>,
        xdg_data_home: impl Into<PathBuf>,
        xdg_runtime_dir: impl Into<PathBuf>,
        runner_uid: u32,
        runner_gid: u32,
    ) -> Result<Self, RootlessPodmanConfigObservationError> {
        if runner_uid == 0 || runner_gid == 0 {
            return Err(RootlessPodmanConfigObservationError::single(
                "rootless Podman configuration observation requires a non-root runner identity",
            ));
        }
        let resolution =
            RootlessPodmanConfigContext::new(home, xdg_config_home, xdg_data_home, xdg_runtime_dir)
                .map_err(RootlessPodmanConfigObservationError::single)?;
        Ok(Self {
            resolution,
            runner_uid,
            runner_gid,
        })
    }

    #[must_use]
    pub fn resolution_context(&self) -> &RootlessPodmanConfigContext {
        &self.resolution
    }

    #[must_use]
    pub fn runner_uid(&self) -> u32 {
        self.runner_uid
    }

    #[must_use]
    pub fn runner_gid(&self) -> u32 {
        self.runner_gid
    }

    fn runner_containers_path(&self) -> PathBuf {
        self.resolution
            .xdg_config_home()
            .join("containers/containers.conf")
    }

    fn runner_storage_path(&self) -> PathBuf {
        self.resolution
            .xdg_config_home()
            .join("containers/storage.conf")
    }
}

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

    /// Build relocated system paths beneath an explicitly trusted test or host root.
    ///
    /// # Errors
    ///
    /// Returns an error unless every path is canonical, absolute, and non-root.
    pub fn new(
        vendor_containers: impl Into<PathBuf>,
        system_containers: impl Into<PathBuf>,
        system_storage: impl Into<PathBuf>,
    ) -> Result<Self, RootlessPodmanConfigObservationError> {
        Ok(Self {
            vendor_containers: canonical_path(vendor_containers.into())?,
            system_containers: canonical_path(system_containers.into())?,
            system_storage: canonical_path(system_storage.into())?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RootlessPodmanObservedSourceKind {
    VendorContainers,
    SystemContainers,
    RunnerContainers,
    SystemStorage,
    RunnerStorage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RootlessPodmanObservedSourceState {
    Missing,
    Present,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RootlessPodmanConfigSourceProblemKind {
    InvalidPath,
    UnsafeParentDirectory,
    SymlinkOrInvalidObject,
    Unreadable,
    NonRegularFile,
    MultipleHardLinks,
    WrongOwner,
    WritableByUntrusted,
    Oversized,
    InvalidUtf8,
    InvalidReviewedSyntax,
}

impl RootlessPodmanConfigSourceProblemKind {
    const fn evidence(self) -> &'static str {
        match self {
            Self::InvalidPath => "configuration source path is not canonical",
            Self::UnsafeParentDirectory => "configuration source has an unsafe parent directory",
            Self::SymlinkOrInvalidObject => {
                "configuration source path is symlinked or has an invalid object type"
            }
            Self::Unreadable => "configuration source could not be read safely",
            Self::NonRegularFile => "configuration source is not a regular file",
            Self::MultipleHardLinks => "configuration source has multiple hard links",
            Self::WrongOwner => "configuration source has the wrong owner or group",
            Self::WritableByUntrusted => {
                "configuration source is writable by an untrusted group or user"
            }
            Self::Oversized => "configuration source exceeds the reviewed size limit",
            Self::InvalidUtf8 => "configuration source is not valid UTF-8",
            Self::InvalidReviewedSyntax => {
                "configuration source has invalid reviewed Podman syntax"
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RootlessPodmanConfigSourceObservation {
    pub kind: RootlessPodmanObservedSourceKind,
    pub path: PathBuf,
    pub state: RootlessPodmanObservedSourceState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<RootlessPodmanConfigSourceProblemKind>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RootlessPodmanConfigObservationReport {
    pub schema_version: u8,
    pub sources: Vec<RootlessPodmanConfigSourceObservation>,
    pub assessment: RootlessPodmanConfigAssessment,
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
}

impl fmt::Display for RootlessPodmanConfigObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            formatter,
            "rootless Podman configuration observation failed"
        )?;
        for problem in &self.problems {
            writeln!(formatter, "- {problem}")?;
        }
        Ok(())
    }
}

impl std::error::Error for RootlessPodmanConfigObservationError {}

/// Observe, resolve, and assess reviewed rootless Podman configuration without running Podman.
///
/// Every source is opened through descriptor-relative no-follow traversal. Missing, present, and
/// unknown remain distinct. Raw file contents and operating-system error text never enter the
/// returned report.
///
/// # Errors
///
/// Returns an error only when the reviewed context or normalized source representation is invalid.
pub fn observe_rootless_podman_config(
    context: &RootlessPodmanConfigObservationContext,
    paths: &RootlessPodmanConfigObservationPaths,
    policy: &RootlessPodmanConfigPolicy,
) -> Result<RootlessPodmanConfigObservationReport, RootlessPodmanConfigObservationError> {
    observe_with(context, paths, policy, &LinuxConfigFilesystem)
}

fn observe_with(
    context: &RootlessPodmanConfigObservationContext,
    paths: &RootlessPodmanConfigObservationPaths,
    policy: &RootlessPodmanConfigPolicy,
    filesystem: &impl ConfigFilesystem,
) -> Result<RootlessPodmanConfigObservationReport, RootlessPodmanConfigObservationError> {
    let runner_containers_path = context.runner_containers_path();
    let runner_storage_path = context.runner_storage_path();
    let root_owner = ExpectedOwner::Root;
    let runner_owner = ExpectedOwner::Runner {
        uid: context.runner_uid,
        gid: context.runner_gid,
    };

    let vendor_containers = observe_source(
        RootlessPodmanObservedSourceKind::VendorContainers,
        &paths.vendor_containers,
        root_owner,
        filesystem,
        parse_rootless_podman_containers_config,
    )?;
    let system_containers = observe_source(
        RootlessPodmanObservedSourceKind::SystemContainers,
        &paths.system_containers,
        root_owner,
        filesystem,
        parse_rootless_podman_containers_config,
    )?;
    let runner_containers = observe_source(
        RootlessPodmanObservedSourceKind::RunnerContainers,
        &runner_containers_path,
        runner_owner,
        filesystem,
        parse_rootless_podman_containers_config,
    )?;
    let system_storage = observe_source(
        RootlessPodmanObservedSourceKind::SystemStorage,
        &paths.system_storage,
        root_owner,
        filesystem,
        parse_rootless_podman_storage_config,
    )?;
    let runner_storage = observe_source(
        RootlessPodmanObservedSourceKind::RunnerStorage,
        &runner_storage_path,
        runner_owner,
        filesystem,
        parse_rootless_podman_storage_config,
    )?;

    let resolved = resolve_rootless_podman_config(
        context.resolution_context(),
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
        assessment,
    })
}

struct ObservedSource<T> {
    source: RootlessPodmanConfigSource<T>,
    observation: RootlessPodmanConfigSourceObservation,
}

fn observe_source<T>(
    kind: RootlessPodmanObservedSourceKind,
    path: &Path,
    expected_owner: ExpectedOwner,
    filesystem: &impl ConfigFilesystem,
    parser: fn(&str) -> Result<T, RootlessPodmanConfigError>,
) -> Result<ObservedSource<T>, RootlessPodmanConfigObservationError> {
    let read = filesystem.read(path, expected_owner);
    let (state, problem, source_state) = match read {
        TrustedConfigRead::Missing => (
            RootlessPodmanObservedSourceState::Missing,
            None,
            RootlessPodmanConfigSourceState::Missing,
        ),
        TrustedConfigRead::Unknown(problem) => (
            RootlessPodmanObservedSourceState::Unknown,
            Some(problem),
            RootlessPodmanConfigSourceState::Unknown {
                evidence: problem.evidence().to_owned(),
            },
        ),
        TrustedConfigRead::Present(bytes) => match std::str::from_utf8(&bytes) {
            Err(_) => (
                RootlessPodmanObservedSourceState::Unknown,
                Some(RootlessPodmanConfigSourceProblemKind::InvalidUtf8),
                RootlessPodmanConfigSourceState::Unknown {
                    evidence: RootlessPodmanConfigSourceProblemKind::InvalidUtf8
                        .evidence()
                        .to_owned(),
                },
            ),
            Ok(input) => match parser(input) {
                Ok(parsed) => (
                    RootlessPodmanObservedSourceState::Present,
                    None,
                    RootlessPodmanConfigSourceState::Present(parsed),
                ),
                Err(_) => (
                    RootlessPodmanObservedSourceState::Unknown,
                    Some(RootlessPodmanConfigSourceProblemKind::InvalidReviewedSyntax),
                    RootlessPodmanConfigSourceState::Unknown {
                        evidence: RootlessPodmanConfigSourceProblemKind::InvalidReviewedSyntax
                            .evidence()
                            .to_owned(),
                    },
                ),
            },
        },
    };
    let source = RootlessPodmanConfigSource::new(path.to_path_buf(), source_state)
        .map_err(RootlessPodmanConfigObservationError::single)?;
    Ok(ObservedSource {
        source,
        observation: RootlessPodmanConfigSourceObservation {
            kind,
            path: path.to_path_buf(),
            state,
            problem,
        },
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpectedOwner {
    Root,
    Runner { uid: u32, gid: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TrustedConfigRead {
    Missing,
    Present(Vec<u8>),
    Unknown(RootlessPodmanConfigSourceProblemKind),
}

trait ConfigFilesystem {
    fn read(&self, path: &Path, expected_owner: ExpectedOwner) -> TrustedConfigRead;
}

struct LinuxConfigFilesystem;

impl ConfigFilesystem for LinuxConfigFilesystem {
    fn read(&self, path: &Path, expected_owner: ExpectedOwner) -> TrustedConfigRead {
        read_linux_config(path, expected_owner)
    }
}

fn read_linux_config(path: &Path, expected_owner: ExpectedOwner) -> TrustedConfigRead {
    if canonical_path(path.to_path_buf()).is_err() {
        return TrustedConfigRead::Unknown(RootlessPodmanConfigSourceProblemKind::InvalidPath);
    }
    let components = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value),
            Component::RootDir => None,
            Component::CurDir | Component::ParentDir | Component::Prefix(_) => None,
        })
        .collect::<Vec<_>>();
    let Some((file_name, parents)) = components.split_last() else {
        return TrustedConfigRead::Unknown(RootlessPodmanConfigSourceProblemKind::InvalidPath);
    };

    let mut directory = match fs::open("/", DIRECTORY_FLAGS, Mode::empty()) {
        Ok(directory) => directory,
        Err(_) => {
            return TrustedConfigRead::Unknown(RootlessPodmanConfigSourceProblemKind::Unreadable);
        }
    };
    for component in parents {
        directory = match open_directory_component(&directory, component, expected_owner) {
            Ok(Some(directory)) => directory,
            Ok(None) => return TrustedConfigRead::Missing,
            Err(problem) => return TrustedConfigRead::Unknown(problem),
        };
    }

    let file = match fs::openat(directory.as_fd(), *file_name, FILE_FLAGS, Mode::empty()) {
        Ok(file) => file,
        Err(Errno::NOENT) => return TrustedConfigRead::Missing,
        Err(Errno::LOOP | Errno::NOTDIR) => {
            return TrustedConfigRead::Unknown(
                RootlessPodmanConfigSourceProblemKind::SymlinkOrInvalidObject,
            );
        }
        Err(_) => {
            return TrustedConfigRead::Unknown(RootlessPodmanConfigSourceProblemKind::Unreadable);
        }
    };
    let stat = match fs::fstat(&file) {
        Ok(stat) => stat,
        Err(_) => {
            return TrustedConfigRead::Unknown(RootlessPodmanConfigSourceProblemKind::Unreadable);
        }
    };
    let metadata = ConfigMetadata {
        kind: if FileType::from_raw_mode(stat.st_mode).is_file() {
            ConfigObjectKind::RegularFile
        } else {
            ConfigObjectKind::Other
        },
        uid: stat.st_uid,
        gid: stat.st_gid,
        mode: stat.st_mode & 0o7777,
        nlink: u128::from(stat.st_nlink),
        size: stat.st_size,
    };
    if let Err(problem) = validate_file_metadata(metadata, expected_owner) {
        return TrustedConfigRead::Unknown(problem);
    }
    read_bounded(file)
}

fn open_directory_component(
    parent: &OwnedFd,
    component: &OsStr,
    expected_owner: ExpectedOwner,
) -> Result<Option<OwnedFd>, RootlessPodmanConfigSourceProblemKind> {
    let directory = match fs::openat(parent.as_fd(), component, DIRECTORY_FLAGS, Mode::empty()) {
        Ok(directory) => directory,
        Err(Errno::NOENT) => return Ok(None),
        Err(Errno::LOOP | Errno::NOTDIR) => {
            return Err(RootlessPodmanConfigSourceProblemKind::SymlinkOrInvalidObject);
        }
        Err(_) => return Err(RootlessPodmanConfigSourceProblemKind::Unreadable),
    };
    let stat =
        fs::fstat(&directory).map_err(|_| RootlessPodmanConfigSourceProblemKind::Unreadable)?;
    if !FileType::from_raw_mode(stat.st_mode).is_dir()
        || stat.st_mode & 0o022 != 0
        || !directory_owner_is_trusted(stat.st_uid, stat.st_gid, expected_owner)
    {
        return Err(RootlessPodmanConfigSourceProblemKind::UnsafeParentDirectory);
    }
    Ok(Some(directory))
}

fn directory_owner_is_trusted(uid: u32, gid: u32, expected_owner: ExpectedOwner) -> bool {
    if uid == 0 {
        return true;
    }
    matches!(
        expected_owner,
        ExpectedOwner::Runner {
            uid: expected_uid,
            gid: expected_gid,
        } if uid == expected_uid && gid == expected_gid
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigObjectKind {
    RegularFile,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ConfigMetadata {
    kind: ConfigObjectKind,
    uid: u32,
    gid: u32,
    mode: u32,
    nlink: u128,
    size: i64,
}

fn validate_file_metadata(
    metadata: ConfigMetadata,
    expected_owner: ExpectedOwner,
) -> Result<(), RootlessPodmanConfigSourceProblemKind> {
    if metadata.kind != ConfigObjectKind::RegularFile {
        return Err(RootlessPodmanConfigSourceProblemKind::NonRegularFile);
    }
    if metadata.nlink != 1 {
        return Err(RootlessPodmanConfigSourceProblemKind::MultipleHardLinks);
    }
    let owner_matches = match expected_owner {
        ExpectedOwner::Root => metadata.uid == 0 && metadata.gid == 0,
        ExpectedOwner::Runner { uid, gid } => metadata.uid == uid && metadata.gid == gid,
    };
    if !owner_matches {
        return Err(RootlessPodmanConfigSourceProblemKind::WrongOwner);
    }
    if metadata.mode & 0o022 != 0 {
        return Err(RootlessPodmanConfigSourceProblemKind::WritableByUntrusted);
    }
    if metadata.size < 0 || metadata.size as u64 > MAX_ROOTLESS_PODMAN_CONFIG_BYTES as u64 {
        return Err(RootlessPodmanConfigSourceProblemKind::Oversized);
    }
    Ok(())
}

fn read_bounded(file: OwnedFd) -> TrustedConfigRead {
    let file = File::from(file);
    let mut reader: Take<File> = file.take((MAX_ROOTLESS_PODMAN_CONFIG_BYTES + 1) as u64);
    let mut bytes = Vec::new();
    if reader.read_to_end(&mut bytes).is_err() {
        return TrustedConfigRead::Unknown(RootlessPodmanConfigSourceProblemKind::Unreadable);
    }
    if bytes.len() > MAX_ROOTLESS_PODMAN_CONFIG_BYTES {
        return TrustedConfigRead::Unknown(RootlessPodmanConfigSourceProblemKind::Oversized);
    }
    TrustedConfigRead::Present(bytes)
}

fn canonical_path(path: PathBuf) -> Result<PathBuf, RootlessPodmanConfigObservationError> {
    let Some(value) = path.to_str() else {
        return Err(RootlessPodmanConfigObservationError::single(
            "configuration observation path must be valid UTF-8",
        ));
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
        return Err(RootlessPodmanConfigObservationError::single(
            "configuration observation path must be a canonical non-root absolute path",
        ));
    }
    Ok(path)
}

#[cfg(test)]
mod tests;
