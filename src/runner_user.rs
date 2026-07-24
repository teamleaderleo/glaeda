use std::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::lane_command::{LinuxAccountName, RunnerUserContext};

pub const MIN_SUBORDINATE_ID_COUNT: u64 = 65_536;
const EXPECTED_RUNNER_SHELL: &str = "/usr/sbin/nologin";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PasswdRecord {
    username: LinuxAccountName,
    uid: u32,
    primary_gid: u32,
    home: String,
    shell: String,
}

impl PasswdRecord {
    #[must_use]
    pub fn username(&self) -> &LinuxAccountName {
        &self.username
    }

    #[must_use]
    pub fn uid(&self) -> u32 {
        self.uid
    }

    #[must_use]
    pub fn primary_gid(&self) -> u32 {
        self.primary_gid
    }

    #[must_use]
    pub fn home(&self) -> &str {
        &self.home
    }

    #[must_use]
    pub fn shell(&self) -> &str {
        &self.shell
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SubordinateRange {
    start: u32,
    count: u32,
}

impl SubordinateRange {
    #[must_use]
    pub fn start(&self) -> u32 {
        self.start
    }

    #[must_use]
    pub fn count(&self) -> u32 {
        self.count
    }

    fn end_exclusive(self) -> u64 {
        u64::from(self.start) + u64::from(self.count)
    }

    fn contains(self, value: u32) -> bool {
        let value = u64::from(value);
        value >= u64::from(self.start) && value < self.end_exclusive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuntimeDirectoryObservation {
    path: PathBuf,
    owner_uid: u32,
    owner_gid: u32,
    mode: u32,
}

impl RuntimeDirectoryObservation {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn owner_uid(&self) -> u32 {
        self.owner_uid
    }

    #[must_use]
    pub fn owner_gid(&self) -> u32 {
        self.owner_gid
    }

    #[must_use]
    pub fn mode(&self) -> u32 {
        self.mode
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerifiedRunnerUser {
    username: LinuxAccountName,
    uid: u32,
    primary_gid: u32,
    home: String,
    runtime_directory: PathBuf,
    subordinate_uid_count: u64,
    subordinate_gid_count: u64,
}

impl VerifiedRunnerUser {
    #[must_use]
    pub fn username(&self) -> &LinuxAccountName {
        &self.username
    }

    #[must_use]
    pub fn uid(&self) -> u32 {
        self.uid
    }

    #[must_use]
    pub fn primary_gid(&self) -> u32 {
        self.primary_gid
    }

    #[must_use]
    pub fn home(&self) -> &str {
        &self.home
    }

    #[must_use]
    pub fn runtime_directory(&self) -> &Path {
        &self.runtime_directory
    }

    #[must_use]
    pub fn subordinate_uid_count(&self) -> u64 {
        self.subordinate_uid_count
    }

    #[must_use]
    pub fn subordinate_gid_count(&self) -> u64 {
        self.subordinate_gid_count
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerUserVerificationErrorKind {
    MissingAccount,
    MalformedAccount,
    IdentityMismatch,
    UnsafeShell,
    MissingRuntimeDirectory,
    RuntimeDirectorySymlink,
    RuntimeDirectoryType,
    RuntimeDirectoryOwner,
    RuntimeDirectoryGroup,
    RuntimeDirectoryMode,
    MalformedSubordinateRange,
    MissingSubordinateRange,
    InsufficientSubordinateIds,
    OverlappingSubordinateRanges,
    SubordinateIdentityOverlap,
    Metadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunnerUserVerificationError {
    kind: RunnerUserVerificationErrorKind,
    public_message: String,
}

impl RunnerUserVerificationError {
    #[must_use]
    pub fn kind(&self) -> RunnerUserVerificationErrorKind {
        self.kind
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.public_message
    }

    fn new(kind: RunnerUserVerificationErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            public_message: message.into(),
        }
    }
}

impl fmt::Display for RunnerUserVerificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.public_message)
    }
}

impl std::error::Error for RunnerUserVerificationError {}

/// Parse the exact single-line passwd record returned for one reviewed runner account.
///
/// # Errors
///
/// Returns a bounded error for missing or multiple records, unsafe account names, noncanonical
/// numeric IDs, or noncanonical absolute home and shell paths.
pub fn parse_passwd_record(input: &str) -> Result<PasswdRecord, RunnerUserVerificationError> {
    let records = input
        .lines()
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if records.is_empty() {
        return Err(RunnerUserVerificationError::new(
            RunnerUserVerificationErrorKind::MissingAccount,
            "runner-user account record is missing",
        ));
    }
    if records.len() != 1 {
        return Err(RunnerUserVerificationError::new(
            RunnerUserVerificationErrorKind::MalformedAccount,
            "runner-user account lookup returned more than one record",
        ));
    }

    let fields = records[0].split(':').collect::<Vec<_>>();
    if fields.len() != 7 {
        return Err(RunnerUserVerificationError::new(
            RunnerUserVerificationErrorKind::MalformedAccount,
            "runner-user passwd record must contain exactly seven fields",
        ));
    }
    let username = LinuxAccountName::parse(fields[0]).map_err(|_| {
        RunnerUserVerificationError::new(
            RunnerUserVerificationErrorKind::MalformedAccount,
            "runner-user passwd record contains an unsafe account name",
        )
    })?;
    let uid = parse_canonical_u32("runner-user UID", fields[2])?;
    let primary_gid = parse_canonical_u32("runner-user primary GID", fields[3])?;
    let home = canonical_absolute_path("runner-user home", fields[5])?;
    let shell = canonical_absolute_path("runner-user shell", fields[6])?;

    Ok(PasswdRecord {
        username,
        uid,
        primary_gid,
        home,
        shell,
    })
}

/// Parse all subordinate-ID ranges assigned to one exact runner account.
///
/// Unrelated account entries are ignored. Matching entries must contain a canonical decimal start
/// and positive count, must not overflow, and must not overlap one another.
///
/// # Errors
///
/// Returns a bounded error for malformed or overlapping matching entries.
pub fn parse_subordinate_ranges(
    input: &str,
    username: &LinuxAccountName,
) -> Result<Vec<SubordinateRange>, RunnerUserVerificationError> {
    let mut ranges = Vec::new();
    for line in input.lines().filter(|line| !line.is_empty()) {
        let mut fields = line.split(':');
        let Some(owner) = fields.next() else {
            continue;
        };
        if owner != username.as_str() {
            continue;
        }
        let Some(start) = fields.next() else {
            return Err(malformed_subordinate_range());
        };
        let Some(count) = fields.next() else {
            return Err(malformed_subordinate_range());
        };
        if fields.next().is_some() {
            return Err(malformed_subordinate_range());
        }
        let start = parse_subordinate_u32("subordinate range start", start)?;
        let count = parse_subordinate_u32("subordinate range count", count)?;
        if start == 0 || count == 0 || u64::from(start) + u64::from(count) > u64::from(u32::MAX) + 1
        {
            return Err(malformed_subordinate_range());
        }
        ranges.push(SubordinateRange { start, count });
    }

    ranges.sort_by_key(|range| range.start);
    for pair in ranges.windows(2) {
        if pair[0].end_exclusive() > u64::from(pair[1].start) {
            return Err(RunnerUserVerificationError::new(
                RunnerUserVerificationErrorKind::OverlappingSubordinateRanges,
                "runner-user subordinate ranges overlap",
            ));
        }
    }
    Ok(ranges)
}

/// Inspect the exact runtime directory recorded in a runner-user context without following a final
/// symlink.
///
/// # Errors
///
/// Returns a bounded error when the path is missing, symlinked, not a directory, or cannot be
/// inspected.
pub fn inspect_runtime_directory(
    context: &RunnerUserContext,
) -> Result<RuntimeDirectoryObservation, RunnerUserVerificationError> {
    let path = Path::new(context.runtime_directory());
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(RunnerUserVerificationError::new(
                RunnerUserVerificationErrorKind::MissingRuntimeDirectory,
                "runner-user runtime directory is missing",
            ));
        }
        Err(_) => {
            return Err(RunnerUserVerificationError::new(
                RunnerUserVerificationErrorKind::Metadata,
                "could not inspect runner-user runtime directory",
            ));
        }
    };
    if metadata.file_type().is_symlink() {
        return Err(RunnerUserVerificationError::new(
            RunnerUserVerificationErrorKind::RuntimeDirectorySymlink,
            "runner-user runtime directory is a symlink",
        ));
    }
    if !metadata.is_dir() {
        return Err(RunnerUserVerificationError::new(
            RunnerUserVerificationErrorKind::RuntimeDirectoryType,
            "runner-user runtime path is not a directory",
        ));
    }
    Ok(RuntimeDirectoryObservation {
        path: path.to_path_buf(),
        owner_uid: metadata.uid(),
        owner_gid: metadata.gid(),
        mode: metadata.mode() & 0o7777,
    })
}

/// Verify exact account, runtime-directory, and subordinate-ID evidence for one runner user.
///
/// # Errors
///
/// Returns a bounded error on any mismatch, unsafe shell, insufficient subordinate capacity,
/// overlap with the runner's own identity, or incompatible runtime-directory ownership and mode.
pub fn verify_runner_user(
    context: &RunnerUserContext,
    passwd: &PasswdRecord,
    subordinate_uids: &[SubordinateRange],
    subordinate_gids: &[SubordinateRange],
    runtime: &RuntimeDirectoryObservation,
) -> Result<VerifiedRunnerUser, RunnerUserVerificationError> {
    if &passwd.username != context.username()
        || passwd.uid != context.uid()
        || passwd.primary_gid != context.primary_gid()
        || passwd.home != context.home()
    {
        return Err(RunnerUserVerificationError::new(
            RunnerUserVerificationErrorKind::IdentityMismatch,
            "runner-user account evidence does not match the requested identity",
        ));
    }
    if passwd.shell != EXPECTED_RUNNER_SHELL {
        return Err(RunnerUserVerificationError::new(
            RunnerUserVerificationErrorKind::UnsafeShell,
            "runner-user account does not use the reviewed nologin shell",
        ));
    }
    if runtime.path != Path::new(context.runtime_directory()) {
        return Err(RunnerUserVerificationError::new(
            RunnerUserVerificationErrorKind::IdentityMismatch,
            "runner-user runtime-directory evidence names another path",
        ));
    }
    if runtime.owner_uid != context.uid() {
        return Err(RunnerUserVerificationError::new(
            RunnerUserVerificationErrorKind::RuntimeDirectoryOwner,
            "runner-user runtime directory has an unexpected owner",
        ));
    }
    if runtime.owner_gid != context.primary_gid() {
        return Err(RunnerUserVerificationError::new(
            RunnerUserVerificationErrorKind::RuntimeDirectoryGroup,
            "runner-user runtime directory has an unexpected group",
        ));
    }
    if runtime.mode != 0o700 {
        return Err(RunnerUserVerificationError::new(
            RunnerUserVerificationErrorKind::RuntimeDirectoryMode,
            "runner-user runtime directory does not have mode 0700",
        ));
    }

    let subordinate_uid_count =
        verify_subordinate_capacity(subordinate_uids, context.uid(), "UID")?;
    let subordinate_gid_count =
        verify_subordinate_capacity(subordinate_gids, context.primary_gid(), "GID")?;

    Ok(VerifiedRunnerUser {
        username: passwd.username.clone(),
        uid: passwd.uid,
        primary_gid: passwd.primary_gid,
        home: passwd.home.clone(),
        runtime_directory: runtime.path.clone(),
        subordinate_uid_count,
        subordinate_gid_count,
    })
}

fn verify_subordinate_capacity(
    ranges: &[SubordinateRange],
    own_id: u32,
    label: &str,
) -> Result<u64, RunnerUserVerificationError> {
    if ranges.is_empty() {
        return Err(RunnerUserVerificationError::new(
            RunnerUserVerificationErrorKind::MissingSubordinateRange,
            format!("runner user has no subordinate {label} range"),
        ));
    }
    if ranges.iter().any(|range| range.contains(own_id)) {
        return Err(RunnerUserVerificationError::new(
            RunnerUserVerificationErrorKind::SubordinateIdentityOverlap,
            format!("runner-user subordinate {label} range overlaps its own identity"),
        ));
    }
    let total = ranges
        .iter()
        .map(|range| u64::from(range.count))
        .sum::<u64>();
    if total < MIN_SUBORDINATE_ID_COUNT {
        return Err(RunnerUserVerificationError::new(
            RunnerUserVerificationErrorKind::InsufficientSubordinateIds,
            format!(
                "runner user requires at least {MIN_SUBORDINATE_ID_COUNT} subordinate {label} values"
            ),
        ));
    }
    Ok(total)
}

fn parse_subordinate_u32(field: &str, value: &str) -> Result<u32, RunnerUserVerificationError> {
    let parsed = value
        .parse::<u32>()
        .map_err(|_| malformed_subordinate_range())?;
    if parsed.to_string() != value {
        return Err(RunnerUserVerificationError::new(
            RunnerUserVerificationErrorKind::MalformedSubordinateRange,
            format!("{field} must contain a canonical decimal integer"),
        ));
    }
    Ok(parsed)
}

fn malformed_subordinate_range() -> RunnerUserVerificationError {
    RunnerUserVerificationError::new(
        RunnerUserVerificationErrorKind::MalformedSubordinateRange,
        "runner-user subordinate range is malformed",
    )
}

fn parse_canonical_u32(field: &str, value: &str) -> Result<u32, RunnerUserVerificationError> {
    let parsed = value.parse::<u32>().map_err(|_| {
        RunnerUserVerificationError::new(
            RunnerUserVerificationErrorKind::MalformedAccount,
            format!("{field} must contain a canonical decimal integer"),
        )
    })?;
    if parsed.to_string() != value {
        return Err(RunnerUserVerificationError::new(
            RunnerUserVerificationErrorKind::MalformedAccount,
            format!("{field} must contain a canonical decimal integer"),
        ));
    }
    Ok(parsed)
}

fn canonical_absolute_path(
    field: &str,
    value: &str,
) -> Result<String, RunnerUserVerificationError> {
    let path = Path::new(value);
    if value.is_empty()
        || value.len() > 4_096
        || value.ends_with('/')
        || value.chars().any(char::is_control)
        || !path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(RunnerUserVerificationError::new(
            RunnerUserVerificationErrorKind::MalformedAccount,
            format!("{field} must be a canonical absolute path without aliases"),
        ));
    }
    Ok(value.to_owned())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    use crate::lane_command::{LinuxAccountName, RunnerUserContext};

    use super::{
        MIN_SUBORDINATE_ID_COUNT, PasswdRecord, RunnerUserVerificationErrorKind,
        RuntimeDirectoryObservation, SubordinateRange, inspect_runtime_directory,
        parse_passwd_record, parse_subordinate_ranges, verify_runner_user,
    };

    fn account_name() -> LinuxAccountName {
        LinuxAccountName::parse("project-runner").expect("account name")
    }

    fn context(uid: u32, gid: u32, home: &str) -> RunnerUserContext {
        RunnerUserContext::new(account_name(), uid, gid, home).expect("runner context")
    }

    fn passwd() -> PasswdRecord {
        parse_passwd_record(
            "project-runner:x:1001:1001:Project Runner:/srv/project-runner:/usr/sbin/nologin\n",
        )
        .expect("passwd record")
    }

    fn runtime() -> RuntimeDirectoryObservation {
        RuntimeDirectoryObservation {
            path: "/run/user/1001".into(),
            owner_uid: 1001,
            owner_gid: 1001,
            mode: 0o700,
        }
    }

    fn full_range(start: u32) -> SubordinateRange {
        SubordinateRange {
            start,
            count: MIN_SUBORDINATE_ID_COUNT as u32,
        }
    }

    #[test]
    fn parses_strict_single_passwd_record() {
        let record = passwd();
        assert_eq!(record.username().as_str(), "project-runner");
        assert_eq!(record.uid(), 1001);
        assert_eq!(record.primary_gid(), 1001);
        assert_eq!(record.home(), "/srv/project-runner");
        assert_eq!(record.shell(), "/usr/sbin/nologin");

        parse_passwd_record("").expect_err("missing record must fail");
        parse_passwd_record("a:x:1:1::/a:/bin/false\nb:x:2:2::/b:/bin/false\n")
            .expect_err("multiple records must fail");
        parse_passwd_record("project-runner:x:01001:1001::/srv/project-runner:/usr/sbin/nologin")
            .expect_err("noncanonical UID must fail");
        parse_passwd_record("project-runner:x:1001:1001::/srv/../root:/usr/sbin/nologin")
            .expect_err("aliased home must fail");
    }

    #[test]
    fn parses_only_matching_nonoverlapping_subordinate_ranges() {
        let ranges = parse_subordinate_ranges(
            "other:100000:65536\nproject-runner:200000:32768\nproject-runner:300000:32768\n",
            &account_name(),
        )
        .expect("subordinate ranges");
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0].start(), 200000);
        assert_eq!(ranges[1].count(), 32768);

        parse_subordinate_ranges("project-runner:1000:0\n", &account_name())
            .expect_err("zero count must fail");
        let error = parse_subordinate_ranges(
            "project-runner:100000:40000\nproject-runner:120000:40000\n",
            &account_name(),
        )
        .expect_err("overlap must fail");
        assert_eq!(
            error.kind(),
            RunnerUserVerificationErrorKind::OverlappingSubordinateRanges
        );
    }

    #[test]
    fn verifies_exact_account_runtime_and_subordinate_capacity() {
        let verified = verify_runner_user(
            &context(1001, 1001, "/srv/project-runner"),
            &passwd(),
            &[full_range(100000)],
            &[full_range(200000)],
            &runtime(),
        )
        .expect("verified runner user");
        assert_eq!(verified.username().as_str(), "project-runner");
        assert_eq!(verified.uid(), 1001);
        assert_eq!(verified.primary_gid(), 1001);
        assert_eq!(verified.home(), "/srv/project-runner");
        assert_eq!(
            verified.runtime_directory().to_str(),
            Some("/run/user/1001")
        );
        assert_eq!(verified.subordinate_uid_count(), MIN_SUBORDINATE_ID_COUNT);
        assert_eq!(verified.subordinate_gid_count(), MIN_SUBORDINATE_ID_COUNT);
    }

    #[test]
    fn rejects_identity_shell_runtime_and_subordinate_mismatches() {
        let mut wrong_shell = passwd();
        wrong_shell.shell = "/bin/bash".to_owned();
        let error = verify_runner_user(
            &context(1001, 1001, "/srv/project-runner"),
            &wrong_shell,
            &[full_range(100000)],
            &[full_range(200000)],
            &runtime(),
        )
        .expect_err("interactive shell must fail");
        assert_eq!(error.kind(), RunnerUserVerificationErrorKind::UnsafeShell);

        let mut wrong_mode = runtime();
        wrong_mode.mode = 0o755;
        let error = verify_runner_user(
            &context(1001, 1001, "/srv/project-runner"),
            &passwd(),
            &[full_range(100000)],
            &[full_range(200000)],
            &wrong_mode,
        )
        .expect_err("broad runtime mode must fail");
        assert_eq!(
            error.kind(),
            RunnerUserVerificationErrorKind::RuntimeDirectoryMode
        );

        let error = verify_runner_user(
            &context(1001, 1001, "/srv/project-runner"),
            &passwd(),
            &[SubordinateRange {
                start: 1000,
                count: MIN_SUBORDINATE_ID_COUNT as u32,
            }],
            &[full_range(200000)],
            &runtime(),
        )
        .expect_err("own UID overlap must fail");
        assert_eq!(
            error.kind(),
            RunnerUserVerificationErrorKind::SubordinateIdentityOverlap
        );
    }

    #[test]
    fn inspects_live_runtime_directory_when_available() {
        let host_metadata = fs::metadata(std::env::temp_dir()).expect("host metadata");
        if host_metadata.uid() == 0 || host_metadata.gid() == 0 {
            return;
        }
        let context = context(
            host_metadata.uid(),
            host_metadata.gid(),
            "/srv/project-runner",
        );
        let runtime_path = Path::new(context.runtime_directory());
        let metadata = match fs::metadata(runtime_path) {
            Ok(metadata) => metadata,
            Err(_) => return,
        };
        if metadata.uid() != context.uid()
            || metadata.gid() != context.primary_gid()
            || metadata.mode() & 0o7777 != 0o700
        {
            return;
        }
        let observation = inspect_runtime_directory(&context).expect("inspect runtime directory");
        assert_eq!(observation.path(), runtime_path);
        assert_eq!(observation.owner_uid(), context.uid());
        assert_eq!(observation.owner_gid(), context.primary_gid());
        assert_eq!(observation.mode(), 0o700);
    }
}
