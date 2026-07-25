use std::fmt;
use std::io::Read as _;
use std::os::fd::OwnedFd;
use std::path::{Component, Path, PathBuf};

use rustix::fs::{self as rustix_fs, FileType, Mode, OFlags};
use rustix::io::Errno;
use serde::Serialize;

use crate::lane_command::LinuxAccountName;
use crate::process::{CommandExecutor, CommandSpec, ExecutionRecord};
use crate::runner_account_plan::{
    DesiredRunnerAccount, PlannedSubordinateRange, PreparationObservation,
    PreparationObservationState, RunnerAccountObservations, RunnerAccountPlanError,
};
use crate::runner_user::{PasswdRecord, parse_passwd_record};
use crate::subordinate_id::{
    SubordinateAuthorityErrorKind, SubordinateIdOwner, SubordinateIdRange, SubordinateIdRequest,
    SubordinateMappingDisposition, parse_subordinate_authority,
    reconcile_subordinate_mapping_for_identity,
};

const GETENT: &str = "/usr/bin/getent";
const EXPECTED_SHELL: &str = "/usr/sbin/nologin";
const MAX_ACCOUNT_FILE_BYTES: usize = 1_048_576;
const MAX_LOOKUP_BYTES: usize = 16_384;
const DIRECTORY_MODE: u32 = 0o750;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunnerAccountObservationPaths {
    subordinate_uids: PathBuf,
    subordinate_gids: PathBuf,
    linger_directory: PathBuf,
}

impl RunnerAccountObservationPaths {
    #[must_use]
    pub fn system_default() -> Self {
        Self {
            subordinate_uids: "/etc/subuid".into(),
            subordinate_gids: "/etc/subgid".into(),
            linger_directory: "/var/lib/systemd/linger".into(),
        }
    }

    /// Build relocated observation paths for an explicitly trusted host root.
    ///
    /// # Errors
    ///
    /// Returns an error unless every path is a canonical absolute path without aliases.
    pub fn new(
        subordinate_uids: impl Into<PathBuf>,
        subordinate_gids: impl Into<PathBuf>,
        linger_directory: impl Into<PathBuf>,
    ) -> Result<Self, RunnerAccountObservationError> {
        Ok(Self {
            subordinate_uids: canonical_observation_path(
                "subordinate UID authority",
                subordinate_uids.into(),
            )?,
            subordinate_gids: canonical_observation_path(
                "subordinate GID authority",
                subordinate_gids.into(),
            )?,
            linger_directory: canonical_observation_path(
                "linger directory",
                linger_directory.into(),
            )?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ObservedRunnerIdentity {
    uid: u32,
    primary_gid: u32,
    group_gid: u32,
}

impl ObservedRunnerIdentity {
    #[must_use]
    pub fn uid(self) -> u32 {
        self.uid
    }

    #[must_use]
    pub fn primary_gid(self) -> u32 {
        self.primary_gid
    }

    #[must_use]
    pub fn group_gid(self) -> u32 {
        self.group_gid
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunnerAccountObservationReport {
    pub observations: RunnerAccountObservations,
    #[serde(skip_serializing_if = "Option::is_none")]
    identity: Option<ObservedRunnerIdentity>,
}

impl RunnerAccountObservationReport {
    #[must_use]
    pub fn identity(&self) -> Option<ObservedRunnerIdentity> {
        self.identity
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunnerAccountObservationError {
    pub problems: Vec<String>,
}

impl RunnerAccountObservationError {
    fn single(problem: impl Into<String>) -> Self {
        Self {
            problems: vec![problem.into()],
        }
    }
}

impl From<RunnerAccountPlanError> for RunnerAccountObservationError {
    fn from(error: RunnerAccountPlanError) -> Self {
        Self {
            problems: error.problems,
        }
    }
}

impl fmt::Display for RunnerAccountObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "runner account observation failed")?;
        for problem in &self.problems {
            writeln!(formatter, "- {problem}")?;
        }
        Ok(())
    }
}

impl std::error::Error for RunnerAccountObservationError {}

/// Observe runner group, user, home, subordinate ranges, and linger without mutation.
///
/// Unsafe or incomplete evidence is retained as `unknown` or `conflicting`; it is never converted
/// to absence. NSS lookups use exact absolute `getent` commands with an empty environment.
///
/// # Errors
///
/// Returns an error only when bounded public observation evidence cannot be represented by the
/// existing runner-account plan contract.
pub fn observe_runner_account(
    desired: &DesiredRunnerAccount,
    executor: &impl CommandExecutor,
    paths: &RunnerAccountObservationPaths,
) -> Result<RunnerAccountObservationReport, RunnerAccountObservationError> {
    observe_with(desired, executor, paths, &LinuxAccountFilesystem)
}

fn observe_with(
    desired: &DesiredRunnerAccount,
    executor: &impl CommandExecutor,
    paths: &RunnerAccountObservationPaths,
    filesystem: &impl AccountFilesystem,
) -> Result<RunnerAccountObservationReport, RunnerAccountObservationError> {
    let group_lookup = lookup(executor, "group", desired.primary_group());
    let user_lookup = lookup(executor, "passwd", desired.username());

    let parsed_group = parse_group_lookup(&group_lookup, desired.primary_group());
    let group = classify_group(
        &group_lookup,
        parsed_group.as_ref(),
        desired.primary_group(),
    )?;
    let parsed_user = parse_user_lookup(&user_lookup);
    let user = classify_user(
        &user_lookup,
        parsed_user.as_ref(),
        parsed_group.as_ref(),
        group.state(),
        desired,
    )?;

    let identity = if group.state() == PreparationObservationState::Matching
        && user.state() == PreparationObservationState::Matching
    {
        let group = parsed_group
            .as_ref()
            .expect("matching group has parsed record");
        let user = parsed_user
            .as_ref()
            .expect("matching user has parsed record");
        Some(ObservedRunnerIdentity {
            uid: user.uid(),
            primary_gid: user.primary_gid(),
            group_gid: group.gid,
        })
    } else {
        None
    };

    let home = classify_home(
        filesystem.inspect(Path::new(desired.home())),
        identity,
        desired.home(),
    )?;
    let subordinate_uids = classify_subordinate(
        filesystem.read_trusted(&paths.subordinate_uids, MAX_ACCOUNT_FILE_BYTES),
        desired.username(),
        desired.subordinate_uids(),
        identity,
        "UID",
    )?;
    let subordinate_gids = classify_subordinate(
        filesystem.read_trusted(&paths.subordinate_gids, MAX_ACCOUNT_FILE_BYTES),
        desired.username(),
        desired.subordinate_gids(),
        identity,
        "GID",
    )?;
    let linger_path = paths.linger_directory.join(desired.username().as_str());
    let linger = classify_linger(
        filesystem.inspect(&linger_path),
        identity.is_some(),
        &linger_path,
    )?;

    Ok(RunnerAccountObservationReport {
        observations: RunnerAccountObservations {
            group,
            user,
            home,
            subordinate_uids,
            subordinate_gids,
            linger,
        },
        identity,
    })
}

#[must_use]
pub fn getent_command(database: &str, name: &LinuxAccountName) -> Option<CommandSpec> {
    match database {
        "passwd" | "group" => Some(
            CommandSpec::new(GETENT)
                .argument(database)
                .argument(name.as_str()),
        ),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Lookup {
    Present(String),
    Absent,
    Unknown,
}

fn lookup(executor: &impl CommandExecutor, database: &str, name: &LinuxAccountName) -> Lookup {
    let command = getent_command(database, name).expect("fixed supported getent database");
    let Ok(receipt) = executor.execute(&command) else {
        return Lookup::Unknown;
    };
    classify_lookup_receipt(&command, receipt)
}

fn classify_lookup_receipt(command: &CommandSpec, receipt: ExecutionRecord) -> Lookup {
    if receipt.argv != command.displayed_argv()
        || !receipt.environment_keys.is_empty()
        || receipt.stdout.len() > MAX_LOOKUP_BYTES
        || receipt.stderr.len() > MAX_LOOKUP_BYTES
        || receipt.stdout.contains('\0')
        || receipt.stderr.contains('\0')
    {
        return Lookup::Unknown;
    }
    if receipt.status == Some(0)
        && receipt.success
        && receipt.stderr.is_empty()
        && !receipt.stdout.is_empty()
        && receipt.stdout.ends_with('\n')
    {
        Lookup::Present(receipt.stdout)
    } else if receipt.status == Some(2)
        && !receipt.success
        && receipt.stdout.is_empty()
        && receipt.stderr.is_empty()
    {
        Lookup::Absent
    } else {
        Lookup::Unknown
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GroupRecord {
    name: LinuxAccountName,
    gid: u32,
}

fn parse_group_lookup(lookup: &Lookup, desired: &LinuxAccountName) -> Option<GroupRecord> {
    let Lookup::Present(input) = lookup else {
        return None;
    };
    let lines = input
        .lines()
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if lines.len() != 1 {
        return None;
    }
    let fields = lines[0].split(':').collect::<Vec<_>>();
    if fields.len() != 4 {
        return None;
    }
    let name = LinuxAccountName::parse(fields[0]).ok()?;
    let gid = canonical_u32(fields[2])?;
    if gid == 0 || &name != desired || !fields[3].is_empty() {
        return None;
    }
    Some(GroupRecord { name, gid })
}

fn parse_user_lookup(lookup: &Lookup) -> Option<PasswdRecord> {
    let Lookup::Present(input) = lookup else {
        return None;
    };
    parse_passwd_record(input).ok()
}

fn group_record_is_well_formed(input: &str) -> bool {
    let lines = input
        .lines()
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if lines.len() != 1 {
        return false;
    }
    let fields = lines[0].split(':').collect::<Vec<_>>();
    fields.len() == 4
        && LinuxAccountName::parse(fields[0]).is_ok()
        && canonical_u32(fields[2]).is_some()
        && fields[3]
            .split(',')
            .filter(|member| !member.is_empty())
            .all(|member| LinuxAccountName::parse(member).is_ok())
}

fn classify_group(
    lookup: &Lookup,
    parsed: Option<&GroupRecord>,
    desired: &LinuxAccountName,
) -> Result<PreparationObservation, RunnerAccountObservationError> {
    let (state, evidence) = match lookup {
        Lookup::Absent => (
            PreparationObservationState::Absent,
            format!("getent group {} returned not found", desired.as_str()),
        ),
        Lookup::Unknown => (
            PreparationObservationState::Unknown,
            format!("getent group {} did not complete cleanly", desired.as_str()),
        ),
        Lookup::Present(input) => match parsed {
            Some(record) => (
                PreparationObservationState::Matching,
                format!(
                    "getent group {} returned canonical dedicated GID {}",
                    record.name.as_str(),
                    record.gid
                ),
            ),
            None if group_record_is_well_formed(input) => (
                PreparationObservationState::Conflicting,
                format!(
                    "getent group {} returned an incompatible name, GID, or member list",
                    desired.as_str()
                ),
            ),
            None => (
                PreparationObservationState::Unknown,
                format!(
                    "getent group {} returned malformed or unsafe data",
                    desired.as_str()
                ),
            ),
        },
    };
    Ok(PreparationObservation::new(state, [evidence])?)
}

fn classify_user(
    lookup: &Lookup,
    parsed: Option<&PasswdRecord>,
    group: Option<&GroupRecord>,
    group_state: PreparationObservationState,
    desired: &DesiredRunnerAccount,
) -> Result<PreparationObservation, RunnerAccountObservationError> {
    let (state, evidence) = match lookup {
        Lookup::Absent => (
            PreparationObservationState::Absent,
            format!(
                "getent passwd {} returned not found",
                desired.username().as_str()
            ),
        ),
        Lookup::Unknown => (
            PreparationObservationState::Unknown,
            format!(
                "getent passwd {} did not complete cleanly",
                desired.username().as_str()
            ),
        ),
        Lookup::Present(_) => match parsed {
            None => (
                PreparationObservationState::Unknown,
                format!(
                    "getent passwd {} returned malformed or unsafe data",
                    desired.username().as_str()
                ),
            ),
            Some(record) if user_record_matches_desired(record, desired) => match group_state {
                PreparationObservationState::Matching
                    if group.is_some_and(|group| group.gid == record.primary_gid()) =>
                {
                    (
                        PreparationObservationState::Matching,
                        format!(
                            "getent passwd {} matched UID {}, primary GID {}, home, and nologin shell",
                            record.username().as_str(),
                            record.uid(),
                            record.primary_gid()
                        ),
                    )
                }
                PreparationObservationState::Unknown => (
                    PreparationObservationState::Unknown,
                    format!(
                        "getent passwd {} matched its local fields but the primary group is unknown",
                        desired.username().as_str()
                    ),
                ),
                PreparationObservationState::Matching
                | PreparationObservationState::Absent
                | PreparationObservationState::Conflicting => (
                    PreparationObservationState::Conflicting,
                    format!(
                        "getent passwd {} conflicts with the desired primary group",
                        desired.username().as_str()
                    ),
                ),
            },
            Some(_) => (
                PreparationObservationState::Conflicting,
                format!(
                    "getent passwd {} conflicts with the desired account fields",
                    desired.username().as_str()
                ),
            ),
        },
    };
    Ok(PreparationObservation::new(state, [evidence])?)
}

fn user_record_matches_desired(record: &PasswdRecord, desired: &DesiredRunnerAccount) -> bool {
    record.username() == desired.username()
        && record.uid() > 0
        && record.primary_gid() > 0
        && record.home() == desired.home()
        && record.shell() == EXPECTED_SHELL
}

fn classify_home(
    observation: PathObservation,
    identity: Option<ObservedRunnerIdentity>,
    home: &str,
) -> Result<PreparationObservation, RunnerAccountObservationError> {
    let (state, evidence) = match observation {
        PathObservation::Missing => (
            PreparationObservationState::Absent,
            format!("runner home {home} is absent"),
        ),
        PathObservation::Unknown => (
            PreparationObservationState::Unknown,
            format!("runner home {home} could not be inspected safely"),
        ),
        PathObservation::Present(metadata) => match identity {
            Some(identity)
                if metadata.kind == ObservedPathKind::Directory
                    && metadata.uid == identity.uid
                    && metadata.gid == identity.primary_gid
                    && metadata.mode == DIRECTORY_MODE =>
            {
                (
                    PreparationObservationState::Matching,
                    format!(
                        "runner home {home} is a directory owned by {}:{} with mode 0750",
                        metadata.uid, metadata.gid
                    ),
                )
            }
            _ => (
                PreparationObservationState::Conflicting,
                format!("runner home {home} exists with incompatible type, ownership, or mode"),
            ),
        },
    };
    Ok(PreparationObservation::new(state, [evidence])?)
}

fn classify_subordinate(
    file: TrustedFile,
    username: &LinuxAccountName,
    desired: PlannedSubordinateRange,
    identity: Option<ObservedRunnerIdentity>,
    label: &str,
) -> Result<PreparationObservation, RunnerAccountObservationError> {
    let (state, evidence) = match file {
        TrustedFile::Missing | TrustedFile::Unknown => (
            PreparationObservationState::Unknown,
            format!("subordinate {label} authority could not be read safely"),
        ),
        TrustedFile::Present(input) => match parse_subordinate_authority(&input) {
            Err(error)
                if matches!(
                    error.kind(),
                    SubordinateAuthorityErrorKind::DuplicateOwner
                        | SubordinateAuthorityErrorKind::Overlap
                ) =>
            {
                (
                    PreparationObservationState::Conflicting,
                    format!(
                        "subordinate {label} authority contains conflicting global allocation state: {}",
                        error.message()
                    ),
                )
            }
            Err(error) => (
                PreparationObservationState::Unknown,
                format!(
                    "subordinate {label} authority could not be parsed completely: {}",
                    error.message()
                ),
            ),
            Ok(authority) => {
                let owner = SubordinateIdOwner::from(username);
                let range = SubordinateIdRange::new(desired.start(), desired.count())
                    .expect("planned subordinate range is already validated");
                let decision = reconcile_subordinate_mapping_for_identity(
                    &authority,
                    &owner,
                    identity.map(|identity| identity.uid),
                    SubordinateIdRequest::Exact { range },
                );
                match decision.disposition {
                    SubordinateMappingDisposition::Matching if identity.is_some() => (
                        PreparationObservationState::Matching,
                        format!(
                            "subordinate {label} range {}-{} exactly matches the desired allocation in a globally non-overlapping authority",
                            range.start(),
                            range.end_inclusive()
                        ),
                    ),
                    SubordinateMappingDisposition::Required => (
                        PreparationObservationState::Absent,
                        format!(
                            "no subordinate {label} range is assigned to {} and exact range {}-{} is globally free",
                            username.as_str(),
                            range.start(),
                            range.end_inclusive()
                        ),
                    ),
                    SubordinateMappingDisposition::Matching
                    | SubordinateMappingDisposition::Conflicting
                    | SubordinateMappingDisposition::Exhausted => (
                        PreparationObservationState::Conflicting,
                        format!(
                            "subordinate {label} authority conflicts with the desired single allocation for {}",
                            username.as_str()
                        ),
                    ),
                }
            }
        },
    };
    Ok(PreparationObservation::new(state, [evidence])?)
}

fn classify_linger(
    observation: PathObservation,
    user_matching: bool,
    path: &Path,
) -> Result<PreparationObservation, RunnerAccountObservationError> {
    let display = path.display();
    let (state, evidence) = match observation {
        PathObservation::Missing => (
            PreparationObservationState::Absent,
            format!("systemd linger marker {display} is absent"),
        ),
        PathObservation::Unknown => (
            PreparationObservationState::Unknown,
            format!("systemd linger marker {display} could not be inspected safely"),
        ),
        PathObservation::Present(metadata)
            if user_matching
                && metadata.kind == ObservedPathKind::File
                && metadata.uid == 0
                && metadata.gid == 0
                && metadata.mode & 0o022 == 0
                && metadata.size == 0
                && metadata.nlink == 1 =>
        {
            (
                PreparationObservationState::Matching,
                format!("systemd linger marker {display} is a protected empty root-owned file"),
            )
        }
        PathObservation::Present(_) => (
            PreparationObservationState::Conflicting,
            format!("systemd linger marker {display} exists with incompatible state"),
        ),
    };
    Ok(PreparationObservation::new(state, [evidence])?)
}

fn canonical_observation_path(
    field: &str,
    path: PathBuf,
) -> Result<PathBuf, RunnerAccountObservationError> {
    let Some(value) = path.to_str() else {
        return Err(RunnerAccountObservationError::single(format!(
            "{field} must be valid UTF-8"
        )));
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
        return Err(RunnerAccountObservationError::single(format!(
            "{field} must be a canonical non-root absolute path"
        )));
    }
    Ok(path)
}

fn canonical_u32(value: &str) -> Option<u32> {
    let parsed = value.parse::<u32>().ok()?;
    (parsed.to_string() == value).then_some(parsed)
}

fn canonical_nlink(value: impl Into<u64>) -> u64 {
    value.into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObservedPathKind {
    File,
    Directory,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ObservedPathMetadata {
    kind: ObservedPathKind,
    uid: u32,
    gid: u32,
    mode: u32,
    size: u64,
    nlink: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathObservation {
    Missing,
    Present(ObservedPathMetadata),
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TrustedFile {
    Missing,
    Present(String),
    Unknown,
}

trait AccountFilesystem {
    fn inspect(&self, path: &Path) -> PathObservation;
    fn read_trusted(&self, path: &Path, max_bytes: usize) -> TrustedFile;
}

struct LinuxAccountFilesystem;

impl AccountFilesystem for LinuxAccountFilesystem {
    fn inspect(&self, path: &Path) -> PathObservation {
        let descriptor = match open_traversed(path, OFlags::PATH) {
            Ok(descriptor) => descriptor,
            Err(Errno::NOENT) => return PathObservation::Missing,
            Err(_) => return PathObservation::Unknown,
        };
        let stat = match rustix_fs::fstat(&descriptor) {
            Ok(stat) => stat,
            Err(_) => return PathObservation::Unknown,
        };
        let kind = match FileType::from_raw_mode(stat.st_mode) {
            FileType::RegularFile => ObservedPathKind::File,
            FileType::Directory => ObservedPathKind::Directory,
            _ => ObservedPathKind::Other,
        };
        let Ok(size) = u64::try_from(stat.st_size) else {
            return PathObservation::Unknown;
        };
        PathObservation::Present(ObservedPathMetadata {
            kind,
            uid: stat.st_uid,
            gid: stat.st_gid,
            mode: stat.st_mode & 0o7777,
            size,
            nlink: canonical_nlink(stat.st_nlink),
        })
    }

    fn read_trusted(&self, path: &Path, max_bytes: usize) -> TrustedFile {
        let descriptor = match open_traversed(path, OFlags::RDONLY) {
            Ok(descriptor) => descriptor,
            Err(Errno::NOENT) => return TrustedFile::Missing,
            Err(_) => return TrustedFile::Unknown,
        };
        let stat = match rustix_fs::fstat(&descriptor) {
            Ok(stat) => stat,
            Err(_) => return TrustedFile::Unknown,
        };
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
            || stat.st_uid != 0
            || stat.st_gid != 0
            || stat.st_nlink != 1
            || stat.st_mode & 0o022 != 0
            || stat.st_size < 0
            || usize::try_from(stat.st_size).map_or(true, |size| size > max_bytes)
        {
            return TrustedFile::Unknown;
        }
        let mut bytes = Vec::new();
        if std::fs::File::from(descriptor)
            .take((max_bytes + 1) as u64)
            .read_to_end(&mut bytes)
            .is_err()
            || bytes.len() > max_bytes
        {
            return TrustedFile::Unknown;
        }
        match String::from_utf8(bytes) {
            Ok(value) if !value.contains('\0') && (value.is_empty() || value.ends_with('\n')) => {
                TrustedFile::Present(value)
            }
            _ => TrustedFile::Unknown,
        }
    }
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

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::{BTreeMap, VecDeque};
    use std::fs;
    use std::io;
    use std::os::unix::fs::symlink;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::lane_command::LinuxAccountName;
    use crate::process::{CommandExecutor, CommandSpec, ExecutionRecord};
    use crate::runner_account_plan::{
        DesiredRunnerAccount, PlannedSubordinateRange, PreparationObservationState,
    };

    use super::{
        AccountFilesystem, GETENT, LinuxAccountFilesystem, ObservedPathKind, ObservedPathMetadata,
        PathObservation, RunnerAccountObservationPaths, TrustedFile, getent_command, observe_with,
    };

    struct FakeExecutor {
        records: RefCell<VecDeque<ExecutionRecord>>,
        calls: RefCell<Vec<CommandSpec>>,
    }

    impl FakeExecutor {
        fn new(records: Vec<ExecutionRecord>) -> Self {
            Self {
                records: RefCell::new(records.into()),
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl CommandExecutor for FakeExecutor {
        fn execute(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord> {
            self.calls.borrow_mut().push(spec.clone());
            self.records
                .borrow_mut()
                .pop_front()
                .ok_or_else(|| io::Error::other("missing fake record"))
        }
    }

    #[derive(Default)]
    struct FakeFilesystem {
        paths: BTreeMap<PathBuf, PathObservation>,
        files: BTreeMap<PathBuf, TrustedFile>,
    }

    impl AccountFilesystem for FakeFilesystem {
        fn inspect(&self, path: &Path) -> PathObservation {
            self.paths
                .get(path)
                .copied()
                .unwrap_or(PathObservation::Missing)
        }

        fn read_trusted(&self, path: &Path, _max_bytes: usize) -> TrustedFile {
            self.files
                .get(path)
                .cloned()
                .unwrap_or(TrustedFile::Missing)
        }
    }

    fn account(name: &str) -> LinuxAccountName {
        LinuxAccountName::parse(name).expect("account name")
    }

    fn desired() -> DesiredRunnerAccount {
        DesiredRunnerAccount::new(
            account("project-runner"),
            account("project-runner"),
            "/var/lib/project-runner",
            PlannedSubordinateRange::new(100_000, 65_536).expect("subuid range"),
            PlannedSubordinateRange::new(200_000, 65_536).expect("subgid range"),
        )
        .expect("desired account")
    }

    fn paths() -> RunnerAccountObservationPaths {
        RunnerAccountObservationPaths::new("/test/subuid", "/test/subgid", "/test/linger")
            .expect("observation paths")
    }

    fn success(command: CommandSpec, stdout: &str) -> ExecutionRecord {
        ExecutionRecord {
            argv: command.displayed_argv(),
            environment_keys: Vec::new(),
            status: Some(0),
            success: true,
            stdout: stdout.to_owned(),
            stderr: String::new(),
        }
    }

    fn absent(command: CommandSpec) -> ExecutionRecord {
        ExecutionRecord {
            argv: command.displayed_argv(),
            environment_keys: Vec::new(),
            status: Some(2),
            success: false,
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    fn matching_executor() -> FakeExecutor {
        FakeExecutor::new(vec![
            success(
                getent_command("group", &account("project-runner")).expect("group command"),
                "project-runner:x:1001:\n",
            ),
            success(
                getent_command("passwd", &account("project-runner")).expect("passwd command"),
                "project-runner:x:1001:1001::/var/lib/project-runner:/usr/sbin/nologin\n",
            ),
        ])
    }

    fn matching_filesystem() -> FakeFilesystem {
        FakeFilesystem {
            paths: BTreeMap::from([
                (
                    "/var/lib/project-runner".into(),
                    PathObservation::Present(ObservedPathMetadata {
                        kind: ObservedPathKind::Directory,
                        uid: 1001,
                        gid: 1001,
                        mode: 0o750,
                        size: 0,
                        nlink: 1,
                    }),
                ),
                (
                    "/test/linger/project-runner".into(),
                    PathObservation::Present(ObservedPathMetadata {
                        kind: ObservedPathKind::File,
                        uid: 0,
                        gid: 0,
                        mode: 0o644,
                        size: 0,
                        nlink: 1,
                    }),
                ),
            ]),
            files: BTreeMap::from([
                (
                    "/test/subuid".into(),
                    TrustedFile::Present("project-runner:100000:65536\n".to_owned()),
                ),
                (
                    "/test/subgid".into(),
                    TrustedFile::Present("project-runner:200000:65536\n".to_owned()),
                ),
            ]),
        }
    }

    #[test]
    fn exact_matching_state_is_observed_with_identity() {
        let executor = matching_executor();
        let report = observe_with(&desired(), &executor, &paths(), &matching_filesystem())
            .expect("matching observation");
        let observations = &report.observations;
        assert_eq!(
            observations.group.state(),
            PreparationObservationState::Matching
        );
        assert_eq!(
            observations.user.state(),
            PreparationObservationState::Matching
        );
        assert_eq!(
            observations.home.state(),
            PreparationObservationState::Matching
        );
        assert_eq!(
            observations.subordinate_uids.state(),
            PreparationObservationState::Matching
        );
        assert_eq!(
            observations.linger.state(),
            PreparationObservationState::Matching
        );
        assert_eq!(report.identity().expect("identity").uid(), 1001);
        assert_eq!(executor.calls.borrow().len(), 2);
    }

    #[test]
    fn clean_missing_nss_records_and_paths_are_absent_but_missing_authorities_are_unknown() {
        let group = getent_command("group", &account("project-runner")).expect("group command");
        let passwd = getent_command("passwd", &account("project-runner")).expect("passwd command");
        let executor = FakeExecutor::new(vec![absent(group), absent(passwd)]);
        let report = observe_with(&desired(), &executor, &paths(), &FakeFilesystem::default())
            .expect("absent observation");
        assert_eq!(
            report.observations.group.state(),
            PreparationObservationState::Absent
        );
        assert_eq!(
            report.observations.user.state(),
            PreparationObservationState::Absent
        );
        assert_eq!(
            report.observations.home.state(),
            PreparationObservationState::Absent
        );
        assert_eq!(
            report.observations.subordinate_uids.state(),
            PreparationObservationState::Unknown
        );
        assert_eq!(
            report.observations.linger.state(),
            PreparationObservationState::Absent
        );
    }

    #[test]
    fn failed_lookup_never_becomes_absence_and_blocks_identity() {
        let group = getent_command("group", &account("project-runner")).expect("group command");
        let passwd = getent_command("passwd", &account("project-runner")).expect("passwd command");
        let mut bad_group = absent(group);
        bad_group.status = Some(1);
        let executor = FakeExecutor::new(vec![bad_group, absent(passwd)]);
        let report = observe_with(&desired(), &executor, &paths(), &FakeFilesystem::default())
            .expect("unknown observation");
        assert_eq!(
            report.observations.group.state(),
            PreparationObservationState::Unknown
        );
        assert!(report.identity().is_none());
    }

    #[test]
    fn supplementary_group_members_are_conflicting() {
        let group = success(
            getent_command("group", &account("project-runner")).expect("group command"),
            "project-runner:x:1001:other-user\n",
        );
        let passwd = success(
            getent_command("passwd", &account("project-runner")).expect("passwd command"),
            "project-runner:x:1001:1001::/var/lib/project-runner:/usr/sbin/nologin\n",
        );
        let report = observe_with(
            &desired(),
            &FakeExecutor::new(vec![group, passwd]),
            &paths(),
            &matching_filesystem(),
        )
        .expect("conflicting group observation");
        assert_eq!(
            report.observations.group.state(),
            PreparationObservationState::Conflicting
        );
        assert_eq!(
            report.observations.user.state(),
            PreparationObservationState::Conflicting
        );
    }

    #[test]
    fn matching_user_fields_remain_unknown_when_group_lookup_is_unknown() {
        let group_command =
            getent_command("group", &account("project-runner")).expect("group command");
        let mut group = absent(group_command);
        group.status = Some(1);
        let passwd = success(
            getent_command("passwd", &account("project-runner")).expect("passwd command"),
            "project-runner:x:1001:1001::/var/lib/project-runner:/usr/sbin/nologin
",
        );
        let executor = FakeExecutor::new(vec![group, passwd]);
        let report = observe_with(&desired(), &executor, &paths(), &matching_filesystem())
            .expect("unknown group observation");
        assert_eq!(
            report.observations.user.state(),
            PreparationObservationState::Unknown
        );
        assert!(report.identity().is_none());
    }

    #[test]
    fn foreign_overlapping_subordinate_range_is_conflicting() {
        let mut filesystem = matching_filesystem();
        filesystem.files.insert(
            "/test/subuid".into(),
            TrustedFile::Present("other-user:90000:65536\n".to_owned()),
        );
        let report = observe_with(&desired(), &matching_executor(), &paths(), &filesystem)
            .expect("overlap observation");
        assert_eq!(
            report.observations.subordinate_uids.state(),
            PreparationObservationState::Conflicting
        );
    }

    #[test]
    fn malformed_unrelated_subordinate_entry_keeps_authority_unknown() {
        let mut filesystem = matching_filesystem();
        filesystem.files.insert(
            "/test/subuid".into(),
            TrustedFile::Present("other-user:not-a-number:65536\n".to_owned()),
        );
        let report = observe_with(&desired(), &matching_executor(), &paths(), &filesystem)
            .expect("malformed authority observation");
        assert_eq!(
            report.observations.subordinate_uids.state(),
            PreparationObservationState::Unknown
        );
    }

    #[test]
    fn duplicate_owner_and_unrelated_global_overlap_are_conflicting() {
        for authority in [
            "other-user:500000:65536\nother-user:600000:65536\n",
            "other-user:500000:65536\nthird-user:520000:65536\n",
        ] {
            let mut filesystem = matching_filesystem();
            filesystem.files.insert(
                "/test/subuid".into(),
                TrustedFile::Present(authority.to_owned()),
            );
            let report = observe_with(&desired(), &matching_executor(), &paths(), &filesystem)
                .expect("conflicting authority observation");
            assert_eq!(
                report.observations.subordinate_uids.state(),
                PreparationObservationState::Conflicting
            );
        }
    }

    #[test]
    fn proven_numeric_owner_alias_matches_runner_identity() {
        let mut filesystem = matching_filesystem();
        filesystem.files.insert(
            "/test/subuid".into(),
            TrustedFile::Present("1001:100000:65536\n".to_owned()),
        );
        let report = observe_with(&desired(), &matching_executor(), &paths(), &filesystem)
            .expect("numeric alias observation");
        assert_eq!(
            report.observations.subordinate_uids.state(),
            PreparationObservationState::Matching
        );
    }

    #[test]
    fn username_and_numeric_owner_aliases_are_conflicting() {
        let mut filesystem = matching_filesystem();
        filesystem.files.insert(
            "/test/subuid".into(),
            TrustedFile::Present("project-runner:100000:65536\n1001:300000:65536\n".to_owned()),
        );
        let report = observe_with(&desired(), &matching_executor(), &paths(), &filesystem)
            .expect("duplicate alias observation");
        assert_eq!(
            report.observations.subordinate_uids.state(),
            PreparationObservationState::Conflicting
        );
    }

    #[test]
    fn incompatible_user_home_ranges_and_linger_are_conflicting() {
        let group = success(
            getent_command("group", &account("project-runner")).expect("group command"),
            "project-runner:x:1001:\n",
        );
        let passwd = success(
            getent_command("passwd", &account("project-runner")).expect("passwd command"),
            "project-runner:x:1001:1001::/wrong:/bin/bash\n",
        );
        let executor = FakeExecutor::new(vec![group, passwd]);
        let filesystem = FakeFilesystem {
            paths: BTreeMap::from([
                (
                    "/var/lib/project-runner".into(),
                    PathObservation::Present(ObservedPathMetadata {
                        kind: ObservedPathKind::Directory,
                        uid: 55,
                        gid: 55,
                        mode: 0o777,
                        size: 0,
                        nlink: 1,
                    }),
                ),
                (
                    "/test/linger/project-runner".into(),
                    PathObservation::Present(ObservedPathMetadata {
                        kind: ObservedPathKind::Other,
                        uid: 0,
                        gid: 0,
                        mode: 0o777,
                        size: 1,
                        nlink: 2,
                    }),
                ),
            ]),
            files: BTreeMap::from([
                (
                    "/test/subuid".into(),
                    TrustedFile::Present("project-runner:300000:65536\n".to_owned()),
                ),
                (
                    "/test/subgid".into(),
                    TrustedFile::Present("project-runner:400000:65536\n".to_owned()),
                ),
            ]),
        };
        let report = observe_with(&desired(), &executor, &paths(), &filesystem)
            .expect("conflicting observation");
        assert_eq!(
            report.observations.user.state(),
            PreparationObservationState::Conflicting
        );
        assert_eq!(
            report.observations.home.state(),
            PreparationObservationState::Conflicting
        );
        assert_eq!(
            report.observations.subordinate_uids.state(),
            PreparationObservationState::Conflicting
        );
        assert_eq!(
            report.observations.linger.state(),
            PreparationObservationState::Conflicting
        );
    }

    #[test]
    fn relocated_observation_paths_must_be_canonical_and_absolute() {
        RunnerAccountObservationPaths::new("relative/subuid", "/test/subgid", "/test/linger")
            .expect_err("relative authority path");
        RunnerAccountObservationPaths::new("/test/subuid", "/test/../subgid", "/test/linger")
            .expect_err("aliased authority path");
    }

    #[test]
    fn linux_filesystem_rejects_symlinked_parent_traversal() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "smolrunner-account-observation-{}-{suffix}",
            std::process::id()
        ));
        let real = root.join("real");
        fs::create_dir_all(&real).expect("create real directory");
        fs::write(real.join("marker"), b"").expect("create marker");
        symlink(&real, root.join("link")).expect("create parent symlink");

        let observation = LinuxAccountFilesystem.inspect(&root.join("link/marker"));
        assert_eq!(observation, PathObservation::Unknown);

        fs::remove_dir_all(&root).expect("remove test tree");
    }

    #[test]
    fn exact_getent_commands_are_absolute_and_environment_free() {
        for database in ["passwd", "group"] {
            let command = getent_command(database, &account("project-runner")).expect("command");
            assert_eq!(
                command.displayed_argv(),
                [GETENT, database, "project-runner"]
            );
            assert!(command.environment.is_empty());
        }
        assert!(getent_command("shadow", &account("project-runner")).is_none());
    }
}
