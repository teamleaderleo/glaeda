use std::collections::BTreeSet;
use std::fmt;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::{CommitId, GitTreeId, Sha256Digest};
use crate::process::{CommandSpec, ExecutionRecord, TimedCommandExecutor};
use crate::project_catalog::{GitHubProjectSource, ProjectIdentity};

pub const PROJECT_CHECKOUT_OBSERVATION_SCHEMA_VERSION: u8 = 1;
pub const PROJECT_CHECKOUT_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
pub const MAX_PROJECT_CHECKOUT_OUTPUT_BYTES: usize = 65_536;
pub const MAX_PROJECT_REMOTES: usize = 16;
pub const MAX_REMOTE_NAME_BYTES: usize = 100;
pub const MAX_BRANCH_NAME_BYTES: usize = 512;

const MATERIALIZATION_ID_DOMAIN: &[u8] = b"smolrunner-project-materialization-v1\0";
const SHA256_PREFIX: &str = "sha256:";
const HEX: &[u8; 16] = b"0123456789abcdef";

#[derive(Clone, PartialEq, Eq)]
pub struct ProjectCheckoutLocationIdentity {
    device: u64,
    inode: u64,
    owner: u32,
}

impl ProjectCheckoutLocationIdentity {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            owner: metadata.uid(),
        }
    }

    fn matches(&self, metadata: &std::fs::Metadata) -> bool {
        metadata.is_dir()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
            && metadata.uid() == self.owner
    }

    fn materialization_id(&self) -> Result<Sha256Digest, ProjectCheckoutObservationError> {
        let mut hasher = Sha256::new();
        hasher.update(MATERIALIZATION_ID_DOMAIN);
        hasher.update(self.device.to_be_bytes());
        hasher.update(self.inode.to_be_bytes());
        hasher.update(self.owner.to_be_bytes());
        let digest = hasher.finalize();
        let mut value = String::with_capacity(SHA256_PREFIX.len() + digest.len() * 2);
        value.push_str(SHA256_PREFIX);
        for byte in digest {
            value.push(char::from(HEX[usize::from(byte >> 4)]));
            value.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        Sha256Digest::parse(&value).map_err(|_| invalid_output())
    }
}

impl fmt::Debug for ProjectCheckoutLocationIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<private-project-checkout-location>")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProjectBranchState {
    Attached { name: String },
    Detached,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectRemoteObservation {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectIdentity>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteFreshness {
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectCheckoutObservation {
    schema_version: u8,
    materialization_id: Sha256Digest,
    #[serde(skip_serializing_if = "Option::is_none")]
    primary_project: Option<ProjectIdentity>,
    remotes: Vec<ProjectRemoteObservation>,
    source_ambiguous: bool,
    commit: CommitId,
    tree: GitTreeId,
    branch: ProjectBranchState,
    tracked_changes_present: bool,
    untracked_entry_count: u32,
    upstream_configured: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    local_commits_ahead: Option<u32>,
    linked_worktree_count: u16,
    submodules_present: bool,
    owner_matches_parent: bool,
    remote_freshness: RemoteFreshness,
    #[serde(skip)]
    location_identity: ProjectCheckoutLocationIdentity,
}

impl ProjectCheckoutObservation {
    #[must_use]
    pub const fn materialization_id(&self) -> &Sha256Digest {
        &self.materialization_id
    }

    #[must_use]
    pub const fn primary_project(&self) -> Option<&ProjectIdentity> {
        self.primary_project.as_ref()
    }

    #[must_use]
    pub fn remotes(&self) -> &[ProjectRemoteObservation] {
        &self.remotes
    }

    #[must_use]
    pub const fn source_ambiguous(&self) -> bool {
        self.source_ambiguous
    }

    #[must_use]
    pub const fn branch(&self) -> &ProjectBranchState {
        &self.branch
    }

    #[must_use]
    pub const fn tracked_changes_present(&self) -> bool {
        self.tracked_changes_present
    }

    #[must_use]
    pub const fn untracked_entry_count(&self) -> u32 {
        self.untracked_entry_count
    }

    #[must_use]
    pub const fn upstream_configured(&self) -> bool {
        self.upstream_configured
    }

    #[must_use]
    pub const fn local_commits_ahead(&self) -> Option<u32> {
        self.local_commits_ahead
    }

    #[must_use]
    pub const fn linked_worktree_count(&self) -> u16 {
        self.linked_worktree_count
    }

    #[must_use]
    pub const fn submodules_present(&self) -> bool {
        self.submodules_present
    }

    #[must_use]
    pub const fn owner_matches_parent(&self) -> bool {
        self.owner_matches_parent
    }

    #[must_use]
    pub const fn location_identity(&self) -> &ProjectCheckoutLocationIdentity {
        &self.location_identity
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectCheckoutObservationErrorKind {
    NotWorktree,
    BareRepository,
    UnsafePath,
    SourceChanged,
    Unavailable,
    InvalidOutput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectCheckoutObservationError {
    pub kind: ProjectCheckoutObservationErrorKind,
    pub code: &'static str,
    pub problem: &'static str,
}

impl fmt::Display for ProjectCheckoutObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.problem)
    }
}

impl std::error::Error for ProjectCheckoutObservationError {}

#[derive(Clone, PartialEq, Eq)]
pub struct ProjectCheckoutObserver {
    git_program: PathBuf,
}

impl fmt::Debug for ProjectCheckoutObserver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectCheckoutObserver")
            .field("git_program", &"<reviewed-absolute-git-program>")
            .finish()
    }
}

impl ProjectCheckoutObserver {
    /// Create one read-only checkout observer using a reviewed absolute Git executable.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for a relative or aliased executable path.
    pub fn new(git_program: impl Into<PathBuf>) -> Result<Self, ProjectCheckoutObservationError> {
        let git_program = git_program.into();
        if !is_normalized_absolute_path(&git_program) {
            return Err(unsafe_path());
        }
        Ok(Self { git_program })
    }

    /// Observe one exact checkout without network access or repository mutation.
    ///
    /// Dirty files, detached HEAD, missing upstream, forks, and multiple remotes are successful
    /// observations because they are recovery evidence. A source change during observation fails.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for unsafe paths, non-worktrees, bare repositories, source drift,
    /// unavailable commands, or malformed bounded Git output.
    pub fn observe(
        &self,
        checkout: &Path,
        executor: &impl TimedCommandExecutor,
    ) -> Result<ProjectCheckoutObservation, ProjectCheckoutObservationError> {
        let source_metadata = std::fs::symlink_metadata(checkout).map_err(|_| unavailable())?;
        if source_metadata.file_type().is_symlink() || !source_metadata.is_dir() {
            return Err(unsafe_path());
        }
        let checkout = std::fs::canonicalize(checkout).map_err(|_| unavailable())?;
        if !is_normalized_absolute_path(&checkout) || checkout.to_str().is_none() {
            return Err(unsafe_path());
        }
        let metadata = std::fs::metadata(&checkout).map_err(|_| unavailable())?;
        let parent = checkout.parent().ok_or_else(unsafe_path)?;
        let parent_metadata = std::fs::metadata(parent).map_err(|_| unavailable())?;
        let owner_matches_parent = metadata.uid() == parent_metadata.uid();
        let location_identity = ProjectCheckoutLocationIdentity::from_metadata(&metadata);
        let materialization_id = location_identity.materialization_id()?;

        let bare = self.git(&checkout, &["rev-parse", "--is-bare-repository"], executor)?;
        if !bare.success {
            return Err(not_worktree());
        }
        require_success(&bare)?;
        match parse_single_line(&bare.stdout)? {
            "true" => return Err(bare_repository()),
            "false" => {}
            _ => return Err(invalid_output()),
        }

        let top_level = self.git(&checkout, &["rev-parse", "--show-toplevel"], executor)?;
        if !top_level.success {
            return Err(not_worktree());
        }
        require_success(&top_level)?;
        if PathBuf::from(parse_single_line(&top_level.stdout)?) != checkout {
            return Err(not_worktree());
        }

        let first = self.snapshot(&checkout, executor)?;
        let final_commit = self.read_commit(&checkout, executor)?;
        let final_tree = self.read_tree(&checkout, executor)?;
        let final_status = self.read_status(&checkout, executor)?;
        let final_metadata = std::fs::metadata(&checkout).map_err(|_| source_changed())?;
        if final_commit != first.commit
            || final_tree != first.tree
            || final_status != first.raw_status
            || !location_identity.matches(&final_metadata)
        {
            return Err(source_changed());
        }

        Ok(ProjectCheckoutObservation {
            schema_version: PROJECT_CHECKOUT_OBSERVATION_SCHEMA_VERSION,
            materialization_id,
            primary_project: first.primary_project,
            remotes: first.remotes,
            source_ambiguous: first.source_ambiguous,
            commit: first.commit,
            tree: first.tree,
            branch: first.status.branch,
            tracked_changes_present: first.status.tracked_changes_present,
            untracked_entry_count: first.status.untracked_entry_count,
            upstream_configured: first.status.upstream_configured,
            local_commits_ahead: first.status.local_commits_ahead,
            linked_worktree_count: first.linked_worktree_count,
            submodules_present: first.submodules_present,
            owner_matches_parent,
            remote_freshness: RemoteFreshness::Unknown,
            location_identity,
        })
    }

    fn snapshot(
        &self,
        checkout: &Path,
        executor: &impl TimedCommandExecutor,
    ) -> Result<ProjectCheckoutSnapshot, ProjectCheckoutObservationError> {
        let commit = self.read_commit(checkout, executor)?;
        let tree = self.read_tree(checkout, executor)?;
        let remotes = self.git(
            checkout,
            &[
                "config",
                "--no-includes",
                "--null",
                "--get-regexp",
                "^remote\\..*\\.url$",
            ],
            executor,
        )?;
        let remotes = parse_remotes(&remotes)?;
        let (primary_project, source_ambiguous) = select_primary_project(&remotes);
        let raw_status = self.read_status(checkout, executor)?;
        let status = parse_status(&raw_status)?;
        let modes = self.git(
            checkout,
            &["ls-files", "--format=%(objectmode)"],
            executor,
        )?;
        require_success(&modes)?;
        let submodules_present = parse_submodule_presence(&modes.stdout)?;
        let worktrees = self.git(
            checkout,
            &["worktree", "list", "--porcelain", "-z"],
            executor,
        )?;
        require_success(&worktrees)?;
        let linked_worktree_count = parse_worktree_count(&worktrees.stdout)?;

        Ok(ProjectCheckoutSnapshot {
            commit,
            tree,
            remotes,
            primary_project,
            source_ambiguous,
            status,
            raw_status,
            linked_worktree_count,
            submodules_present,
        })
    }

    fn read_commit(
        &self,
        checkout: &Path,
        executor: &impl TimedCommandExecutor,
    ) -> Result<CommitId, ProjectCheckoutObservationError> {
        let record = self.git(
            checkout,
            &["rev-parse", "--verify", "HEAD^{commit}"],
            executor,
        )?;
        require_success(&record)?;
        CommitId::parse(parse_single_line(&record.stdout)?).map_err(|_| invalid_output())
    }

    fn read_tree(
        &self,
        checkout: &Path,
        executor: &impl TimedCommandExecutor,
    ) -> Result<GitTreeId, ProjectCheckoutObservationError> {
        let record = self.git(
            checkout,
            &["rev-parse", "--verify", "HEAD^{tree}"],
            executor,
        )?;
        require_success(&record)?;
        GitTreeId::parse(parse_single_line(&record.stdout)?).map_err(|_| invalid_output())
    }

    fn read_status(
        &self,
        checkout: &Path,
        executor: &impl TimedCommandExecutor,
    ) -> Result<String, ProjectCheckoutObservationError> {
        let record = self.git(
            checkout,
            &[
                "status",
                "--porcelain=v2",
                "--branch",
                "-z",
                "--untracked-files=all",
                "--ignore-submodules=all",
            ],
            executor,
        )?;
        require_success(&record)?;
        Ok(record.stdout)
    }

    fn git(
        &self,
        checkout: &Path,
        arguments: &[&str],
        executor: &impl TimedCommandExecutor,
    ) -> Result<ExecutionRecord, ProjectCheckoutObservationError> {
        let checkout = checkout.to_str().ok_or_else(unsafe_path)?;
        let mut spec = CommandSpec::new(&self.git_program)
            .argument("--no-optional-locks")
            .argument("-c")
            .argument("credential.helper=")
            .argument("-c")
            .argument("core.fsmonitor=false")
            .argument("-c")
            .argument("core.hooksPath=/dev/null")
            .argument("-c")
            .argument("diff.external=")
            .argument("-C")
            .argument(checkout)
            .environment("GIT_CONFIG_NOSYSTEM", "1")
            .environment("GIT_CONFIG_GLOBAL", "/dev/null")
            .environment("GIT_NO_REPLACE_OBJECTS", "1")
            .environment("GIT_NO_LAZY_FETCH", "1")
            .environment("GIT_TERMINAL_PROMPT", "0")
            .environment("GIT_ASKPASS", "/bin/false")
            .environment("GIT_ALLOW_PROTOCOL", "")
            .environment("GIT_PROTOCOL_FROM_USER", "0")
            .environment("LC_ALL", "C")
            .environment("LANG", "C");
        for argument in arguments {
            spec = spec.argument(*argument);
        }
        let expected_argv = spec.displayed_argv();
        let expected_environment_keys = spec.environment.keys().cloned().collect::<Vec<_>>();
        let record = executor
            .execute_with_timeout(&spec, PROJECT_CHECKOUT_COMMAND_TIMEOUT)
            .map_err(|_| unavailable())?;
        if record.argv != expected_argv
            || record.environment_keys != expected_environment_keys
            || record.stdout.len() > MAX_PROJECT_CHECKOUT_OUTPUT_BYTES
            || record.stderr.len() > MAX_PROJECT_CHECKOUT_OUTPUT_BYTES
            || record.stdout.contains('\u{fffd}')
            || record.stderr.contains('\u{fffd}')
            || record.stdout.contains('\r')
        {
            return Err(invalid_output());
        }
        Ok(record)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectCheckoutSnapshot {
    commit: CommitId,
    tree: GitTreeId,
    remotes: Vec<ProjectRemoteObservation>,
    primary_project: Option<ProjectIdentity>,
    source_ambiguous: bool,
    status: ParsedStatus,
    raw_status: String,
    linked_worktree_count: u16,
    submodules_present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedStatus {
    branch: ProjectBranchState,
    tracked_changes_present: bool,
    untracked_entry_count: u32,
    upstream_configured: bool,
    local_commits_ahead: Option<u32>,
}

fn parse_remotes(
    record: &ExecutionRecord,
) -> Result<Vec<ProjectRemoteObservation>, ProjectCheckoutObservationError> {
    if !record.success {
        if record.status == Some(1) && record.stdout.is_empty() {
            return Ok(Vec::new());
        }
        return Err(invalid_output());
    }
    if record.status != Some(0) || !record.stderr.is_empty() {
        return Err(invalid_output());
    }
    let mut remotes = Vec::new();
    for entry in record.stdout.split_terminator('\0') {
        let Some((key, url)) = entry.split_once('\n') else {
            return Err(invalid_output());
        };
        let Some(name) = key
            .strip_prefix("remote.")
            .and_then(|key| key.strip_suffix(".url"))
        else {
            return Err(invalid_output());
        };
        if !valid_remote_name(name) || remotes.len() >= MAX_PROJECT_REMOTES {
            return Err(invalid_output());
        }
        let project = GitHubProjectSource::parse(url)
            .ok()
            .map(|source| source.project().clone());
        remotes.push(ProjectRemoteObservation {
            name: name.to_owned(),
            project,
        });
    }
    if !record.stdout.is_empty() && !record.stdout.ends_with('\0') {
        return Err(invalid_output());
    }
    remotes.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then(left.project.cmp(&right.project))
    });
    Ok(remotes)
}

fn select_primary_project(remotes: &[ProjectRemoteObservation]) -> (Option<ProjectIdentity>, bool) {
    let projects = remotes
        .iter()
        .filter_map(|remote| remote.project.clone())
        .collect::<BTreeSet<_>>();
    let origin = remotes
        .iter()
        .filter(|remote| remote.name == "origin")
        .collect::<Vec<_>>();
    let primary = if origin.len() == 1 {
        origin[0].project.clone()
    } else if projects.len() == 1 {
        projects.iter().next().cloned()
    } else {
        None
    };
    (primary, projects.len() > 1)
}

fn parse_status(value: &str) -> Result<ParsedStatus, ProjectCheckoutObservationError> {
    let mut branch = None;
    let mut upstream_configured = false;
    let mut local_commits_ahead = None;
    let mut tracked_changes_present = false;
    let mut untracked_entry_count = 0_u32;
    let mut skip_rename_source = false;

    for entry in value.split_terminator('\0') {
        if skip_rename_source {
            skip_rename_source = false;
            continue;
        }
        if let Some(head) = entry.strip_prefix("# branch.head ") {
            branch = Some(if head == "(detached)" {
                ProjectBranchState::Detached
            } else {
                validate_branch_name(head)?;
                ProjectBranchState::Attached {
                    name: head.to_owned(),
                }
            });
        } else if entry.starts_with("# branch.upstream ") {
            upstream_configured = true;
        } else if let Some(ab) = entry.strip_prefix("# branch.ab +") {
            let Some((ahead, behind)) = ab.split_once(" -") else {
                return Err(invalid_output());
            };
            let ahead = ahead.parse::<u32>().map_err(|_| invalid_output())?;
            let _behind = behind.parse::<u32>().map_err(|_| invalid_output())?;
            local_commits_ahead = Some(ahead);
        } else if entry.starts_with("1 ") || entry.starts_with("u ") {
            tracked_changes_present = true;
        } else if entry.starts_with("2 ") {
            tracked_changes_present = true;
            skip_rename_source = true;
        } else if entry.starts_with("? ") {
            untracked_entry_count = untracked_entry_count
                .checked_add(1)
                .ok_or_else(invalid_output)?;
        } else if entry.starts_with("! ") || entry.starts_with("# branch.oid ") {
        } else {
            return Err(invalid_output());
        }
    }
    let branch = branch.ok_or_else(invalid_output)?;
    if upstream_configured != local_commits_ahead.is_some() {
        return Err(invalid_output());
    }
    Ok(ParsedStatus {
        branch,
        tracked_changes_present,
        untracked_entry_count,
        upstream_configured,
        local_commits_ahead,
    })
}

fn parse_submodule_presence(value: &str) -> Result<bool, ProjectCheckoutObservationError> {
    if !value.is_empty() && !value.ends_with('\n') {
        return Err(invalid_output());
    }
    let mut present = false;
    for mode in value.lines() {
        match mode {
            "100644" | "100755" | "120000" => {}
            "160000" => present = true,
            _ => return Err(invalid_output()),
        }
    }
    Ok(present)
}

fn parse_worktree_count(value: &str) -> Result<u16, ProjectCheckoutObservationError> {
    if !value.is_empty() && !value.ends_with('\0') {
        return Err(invalid_output());
    }
    let count = value
        .split_terminator('\0')
        .filter(|entry| entry.starts_with("worktree "))
        .count();
    u16::try_from(count).map_err(|_| invalid_output())
}

fn validate_branch_name(value: &str) -> Result<(), ProjectCheckoutObservationError> {
    if value.is_empty()
        || value.len() > MAX_BRANCH_NAME_BYTES
        || value.contains("..")
        || value.contains("@{")
        || value.starts_with('.')
        || value.ends_with('.')
        || value.ends_with('/')
        || value.bytes().any(|byte| {
            byte.is_ascii_control()
                || byte == b' '
                || matches!(byte, b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
        })
    {
        Err(invalid_output())
    } else {
        Ok(())
    }
}

fn valid_remote_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_REMOTE_NAME_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn parse_single_line(value: &str) -> Result<&str, ProjectCheckoutObservationError> {
    let line = value.strip_suffix('\n').unwrap_or(value);
    if line.is_empty() || line.contains('\n') || line.contains('\0') {
        Err(invalid_output())
    } else {
        Ok(line)
    }
}

fn require_success(record: &ExecutionRecord) -> Result<(), ProjectCheckoutObservationError> {
    if record.success && record.status == Some(0) && record.stderr.is_empty() {
        Ok(())
    } else {
        Err(unavailable())
    }
}

fn is_normalized_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

fn error(
    kind: ProjectCheckoutObservationErrorKind,
    code: &'static str,
    problem: &'static str,
) -> ProjectCheckoutObservationError {
    ProjectCheckoutObservationError {
        kind,
        code,
        problem,
    }
}

fn not_worktree() -> ProjectCheckoutObservationError {
    error(
        ProjectCheckoutObservationErrorKind::NotWorktree,
        "not_worktree",
        "candidate is not an exact Git worktree root",
    )
}

fn bare_repository() -> ProjectCheckoutObservationError {
    error(
        ProjectCheckoutObservationErrorKind::BareRepository,
        "bare_repository",
        "candidate is a bare Git repository",
    )
}

fn unsafe_path() -> ProjectCheckoutObservationError {
    error(
        ProjectCheckoutObservationErrorKind::UnsafePath,
        "unsafe_path",
        "candidate path is unsafe for checkout observation",
    )
}

fn source_changed() -> ProjectCheckoutObservationError {
    error(
        ProjectCheckoutObservationErrorKind::SourceChanged,
        "source_changed",
        "checkout changed during observation",
    )
}

fn unavailable() -> ProjectCheckoutObservationError {
    error(
        ProjectCheckoutObservationErrorKind::Unavailable,
        "observation_unavailable",
        "checkout observation is unavailable",
    )
}

fn invalid_output() -> ProjectCheckoutObservationError {
    error(
        ProjectCheckoutObservationErrorKind::InvalidOutput,
        "invalid_git_output",
        "Git returned invalid or untrusted observation output",
    )
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::process::{CommandExecutor, CommandSpec, ExecutionRecord, TimedCommandExecutor};

    use super::{
        PROJECT_CHECKOUT_COMMAND_TIMEOUT, ProjectBranchState, ProjectCheckoutObservationErrorKind,
        ProjectCheckoutObserver,
    };

    const COMMIT: &str = "1111111111111111111111111111111111111111";
    const TREE: &str = "2222222222222222222222222222222222222222";
    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new(label: &str) -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "smolrunner-project-checkout-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create temporary checkout");
            Self(fs::canonicalize(path).expect("canonical temporary checkout"))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Clone)]
    struct Response {
        stdout: String,
        stderr: String,
        status: i32,
    }

    impl Response {
        fn success(stdout: impl Into<String>) -> Self {
            Self {
                stdout: stdout.into(),
                stderr: String::new(),
                status: 0,
            }
        }

        fn failed(status: i32, stderr: impl Into<String>) -> Self {
            Self {
                stdout: String::new(),
                stderr: stderr.into(),
                status,
            }
        }
    }

    struct ScriptedExecutor {
        responses: RefCell<VecDeque<Response>>,
        commands: RefCell<Vec<CommandSpec>>,
    }

    impl ScriptedExecutor {
        fn new(responses: Vec<Response>) -> Self {
            Self {
                responses: RefCell::new(responses.into()),
                commands: RefCell::new(Vec::new()),
            }
        }
    }

    impl CommandExecutor for ScriptedExecutor {
        fn execute(&self, _spec: &CommandSpec) -> io::Result<ExecutionRecord> {
            panic!("checkout observation must use the timed executor")
        }
    }

    impl TimedCommandExecutor for ScriptedExecutor {
        fn execute_with_timeout(
            &self,
            spec: &CommandSpec,
            timeout: std::time::Duration,
        ) -> io::Result<ExecutionRecord> {
            assert_eq!(timeout, PROJECT_CHECKOUT_COMMAND_TIMEOUT);
            self.commands.borrow_mut().push(spec.clone());
            let response = self
                .responses
                .borrow_mut()
                .pop_front()
                .expect("scripted response");
            Ok(ExecutionRecord {
                argv: spec.displayed_argv(),
                environment_keys: spec.environment.keys().cloned().collect(),
                status: Some(response.status),
                success: response.status == 0,
                stdout: response.stdout,
                stderr: response.stderr,
            })
        }
    }

    fn observer() -> ProjectCheckoutObserver {
        ProjectCheckoutObserver::new("/usr/bin/git").expect("observer")
    }

    fn script(
        root: &Path,
        remotes: &str,
        status: &str,
        modes: &str,
        worktrees: &str,
    ) -> Vec<Response> {
        vec![
            Response::success("false\n"),
            Response::success(format!("{}\n", root.display())),
            Response::success(format!("{COMMIT}\n")),
            Response::success(format!("{TREE}\n")),
            Response::success(remotes),
            Response::success(status),
            Response::success(modes),
            Response::success(worktrees),
            Response::success(format!("{COMMIT}\n")),
            Response::success(format!("{TREE}\n")),
            Response::success(status),
        ]
    }

    #[test]
    fn clean_checkout_reports_project_without_private_path() {
        let checkout = TempDirectory::new("clean");
        let remotes = "remote.origin.url\nhttps://github.com/TeamLeaderLeo/SmolRunner.git\0";
        let status = format!(
            "# branch.oid {COMMIT}\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +0 -0\0"
        );
        let worktrees = format!(
            "worktree {}\0HEAD {COMMIT}\0branch refs/heads/main\0\0",
            checkout.path().display()
        );
        let executor = ScriptedExecutor::new(script(
            checkout.path(),
            remotes,
            &status,
            "100644\n100755\n",
            &worktrees,
        ));
        let observation = observer()
            .observe(checkout.path(), &executor)
            .expect("checkout observation");

        assert_eq!(
            observation.primary_project().expect("project").as_str(),
            "github.com/teamleaderleo/smolrunner"
        );
        assert!(!observation.source_ambiguous());
        assert!(!observation.tracked_changes_present());
        assert_eq!(observation.local_commits_ahead(), Some(0));
        assert_eq!(observation.linked_worktree_count(), 1);
        let json = serde_json::to_string(&observation).expect("public JSON");
        assert!(!json.contains(checkout.path().to_string_lossy().as_ref()));
        assert!(
            !format!("{:?}", observation.location_identity())
                .contains(checkout.path().to_string_lossy().as_ref())
        );
        assert_eq!(executor.commands.borrow().len(), 11);
    }

    #[test]
    fn dirty_fork_and_local_ahead_are_recovery_evidence() {
        let checkout = TempDirectory::new("dirty-fork");
        let remotes = concat!(
            "remote.origin.url\nhttps://github.com/example/fork.git\0",
            "remote.upstream.url\nhttps://github.com/upstream/project.git\0"
        );
        let status = format!(
            "# branch.oid {COMMIT}\0# branch.head feature/x\0# branch.upstream origin/feature/x\0# branch.ab +3 -1\01 .M N... 100644 100644 100644 {TREE} {TREE} file.txt\0? secret.txt\0"
        );
        let executor = ScriptedExecutor::new(script(
            checkout.path(),
            remotes,
            &status,
            "100644\n160000\n",
            "worktree /private/path\0HEAD 1111111111111111111111111111111111111111\0\0worktree /private/other\0HEAD 1111111111111111111111111111111111111111\0\0",
        ));
        let observation = observer()
            .observe(checkout.path(), &executor)
            .expect("dirty checkout is observable");

        assert_eq!(
            observation
                .primary_project()
                .expect("origin project")
                .as_str(),
            "github.com/example/fork"
        );
        assert!(observation.source_ambiguous());
        assert!(observation.tracked_changes_present());
        assert_eq!(observation.untracked_entry_count(), 1);
        assert_eq!(observation.local_commits_ahead(), Some(3));
        assert_eq!(observation.linked_worktree_count(), 2);
        assert!(observation.submodules_present());
        assert!(matches!(
            observation.branch(),
            ProjectBranchState::Attached { name } if name == "feature/x"
        ));
        let json = serde_json::to_string(&observation).expect("public JSON");
        assert!(!json.contains("file.txt"));
        assert!(!json.contains("secret.txt"));
        assert!(!json.contains("/private/path"));
    }

    #[test]
    fn detached_and_non_git_states_are_bounded() {
        let detached = TempDirectory::new("detached");
        let status = format!("# branch.oid {COMMIT}\0# branch.head (detached)\0");
        let executor = ScriptedExecutor::new(script(
            detached.path(),
            "",
            &status,
            "100644\n",
            "worktree /private/path\0HEAD 1111111111111111111111111111111111111111\0detached\0\0",
        ));
        let observation = observer()
            .observe(detached.path(), &executor)
            .expect("detached checkout");
        assert!(matches!(observation.branch(), ProjectBranchState::Detached));
        assert!(!observation.upstream_configured());
        assert_eq!(observation.local_commits_ahead(), None);

        let ordinary = TempDirectory::new("ordinary-directory");
        let executor = ScriptedExecutor::new(vec![Response::failed(128, "fatal: private path")]);
        let error = observer()
            .observe(ordinary.path(), &executor)
            .expect_err("non Git directory");
        assert_eq!(error.kind, ProjectCheckoutObservationErrorKind::NotWorktree);
        assert!(
            !serde_json::to_string(&error)
                .expect("error JSON")
                .contains("private path")
        );
    }

    #[test]
    fn bare_and_symlink_candidates_are_classified() {
        let bare = TempDirectory::new("bare");
        let executor = ScriptedExecutor::new(vec![Response::success("true\n")]);
        let error = observer()
            .observe(bare.path(), &executor)
            .expect_err("bare repository");
        assert_eq!(
            error.kind,
            ProjectCheckoutObservationErrorKind::BareRepository
        );

        use std::os::unix::fs::symlink;
        let root = TempDirectory::new("symlink-root");
        let target = root.path().join("target");
        fs::create_dir(&target).expect("target directory");
        let link = root.path().join("link");
        symlink(&target, &link).expect("symlink");
        let executor = ScriptedExecutor::new(Vec::new());
        let error = observer()
            .observe(&link, &executor)
            .expect_err("symlink refused");
        assert_eq!(error.kind, ProjectCheckoutObservationErrorKind::UnsafePath);
        assert!(executor.commands.borrow().is_empty());
    }
}
