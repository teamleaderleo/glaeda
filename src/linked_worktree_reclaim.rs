//! Read-only observation and pure reclaim planning for linked Git worktrees.
//!
//! A linked worktree is not reconstructible the way a Cargo target is. It can hold the only copy of
//! uncommitted edits (including edits `git status` hides behind assume-unchanged or skip-worktree
//! index flags), untracked files, populated submodule repositories, an interrupted operation,
//! per-worktree refs, or a commit that no ref reaches. Authority here therefore comes from
//! preservation: everything the worktree holds must already exist elsewhere in the repository, or be
//! preservable by exactly one named compensation -- pinning a detached HEAD under a ref before
//! removal. Apparent idleness and a path that looks like scratch grant nothing; the idle window is a
//! grace period, not evidence.
//!
//! Outside the preservation guarantee, by design:
//!
//! - Ignored files. They are what makes worktrees expensive (`target/`, `node_modules/`), and
//!   `git worktree remove` discards them too, but ignore rules are repository-controlled, so an
//!   ignored file can still be unique local data such as a `.env`. Callers that act on a plan own
//!   that residual risk.
//! - History reached only through this worktree's reflog or `ORIG_HEAD`, which Git itself already
//!   treats as garbage once it expires.
//!
//! Shared refs mean branches, tags, and remote-tracking refs. `refs/stash`, prefetch refs, and other
//! worktrees' per-worktree refs are rewritten or dropped by routine commands, so they never count
//! as preservation.
//!
//! Planning performs no mutation and emits no paths. It consumes observations the caller took and
//! returns one decision with every veto that applied, so a refusal explains itself completely.
//! Observed facts are assembled from several reads at slightly different moments, so they are one
//! bounded observation, not an atomic snapshot. Nothing here removes a worktree; an executor must
//! re-observe immediately before acting and belongs behind its own destructive review.

use std::ffi::OsStr;
use std::fmt;
use std::io::Read as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::process::{ExecutionRecord, MAX_CAPTURED_STREAM_BYTES, TimedCommandExecutor};
use crate::project_checkout_observation::{ProjectBranchState, ProjectCheckoutObserver};

pub const LINKED_WORKTREE_RECLAIM_SCHEMA_VERSION: u8 = 1;

/// Most linked worktrees one inventory may describe. Larger inventories fail closed.
pub const MAX_LINKED_WORKTREES: usize = 256;

/// Largest per-worktree Git index this module reads. Larger indexes fail closed.
pub const MAX_LINKED_WORKTREE_INDEX_BYTES: u64 = 64 * 1024 * 1024;

/// Most submodule entries one index may declare. More fail closed.
pub const MAX_LINKED_WORKTREE_GITLINKS: usize = 1_024;

/// Smallest idle window a policy may require.
///
/// The window only protects a worktree someone is actively driving through Git between clean
/// states. Preservation is what grants authority, so the floor matches the Cargo target planner
/// rather than trying to prove anything about idleness.
pub const MIN_LINKED_WORKTREE_RECLAIM_IDLE_SECONDS: i64 = 3_600;

/// Per-worktree Git state that marks an interrupted operation holding unique intermediate state.
const OPERATION_MARKERS: [&str; 6] = [
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "BISECT_LOG",
    "rebase-merge",
    "rebase-apply",
];

/// Per-worktree Git entries whose modification time records Git activity in that worktree.
///
/// The index is deliberately absent: any tool running a plain `git status` rewrites it, so its
/// mtime measures who looked rather than who worked, and staged changes already veto as tracked
/// changes.
const ACTIVITY_ENTRIES: [&str; 2] = ["HEAD", "logs/HEAD"];

/// `rev-list` selectors for the ref namespaces whose tips survive routine Git commands and so can
/// preserve a commit: branches, tags, and remote-tracking refs.
const PRESERVING_REF_SELECTORS: [&str; 3] = ["--branches", "--tags", "--remotes"];

/// Per-worktree ref namespaces that vanish with the worktree.
const PER_WORKTREE_REF_PATTERNS: [&str; 3] = ["refs/worktree/", "refs/bisect/", "refs/rewritten/"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HeadReachability {
    /// A branch, tag, or remote-tracking ref contains the HEAD commit.
    ReachableFromSharedRef,
    /// No preserving ref reaches the commit; only this worktree's detached HEAD does.
    OnlyFromThisWorktree,
}

/// Facts about one checkout, observed by [`observe_linked_worktree`] or supplied by a caller.
///
/// Planning trusts these as observations, never as ownership proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LinkedWorktreeFacts {
    pub linked: bool,
    pub locked: bool,
    pub operation_in_progress: bool,
    pub tracked_changes_present: bool,
    pub untracked_entry_count: u32,
    /// At least one index entry carries assume-unchanged or skip-worktree, which hides edits from
    /// `git status`.
    pub hidden_index_entries_present: bool,
    /// A submodule path holds a `.git` entry, or per-worktree submodule repositories exist.
    pub populated_submodules_present: bool,
    /// Refs under this worktree's own `refs/worktree/`, `refs/bisect/`, or `refs/rewritten/`.
    pub per_worktree_refs_present: bool,
    /// The checkout directory belongs to the effective user that would remove it.
    pub owned_by_current_user: bool,
    pub head_reachability: HeadReachability,
    /// Newest modification time, in Unix seconds, across per-worktree Git activity entries and
    /// the checkout root directory.
    pub last_activity_seconds: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkedWorktreeReclaimVeto {
    /// The main worktree owns the repository and is never reclaimable here.
    MainWorktree,
    /// Someone ran `git worktree lock`, which is an explicit request to keep it.
    Locked,
    /// A merge, rebase, cherry-pick, revert, or bisect is in progress.
    OperationInProgress,
    /// Tracked files differ from HEAD or the index.
    TrackedChangesPresent,
    /// Untracked, non-ignored files exist and exist nowhere else.
    UntrackedEntriesPresent,
    /// Index flags hide possible edits from `git status`.
    HiddenIndexEntriesPresent,
    /// Submodule repositories are populated inside this worktree and may hold unique commits or
    /// edits that status deliberately does not inspect.
    PopulatedSubmodulesPresent,
    /// Per-worktree refs exist and would vanish with the worktree.
    PerWorktreeRefsPresent,
    /// The checkout belongs to a different user than the one planning its removal.
    NotOwnedByCurrentUser,
    /// Git activity happened inside the policy's idle window.
    RecentlyActive,
}

impl LinkedWorktreeReclaimVeto {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::MainWorktree => "main_worktree",
            Self::Locked => "locked",
            Self::OperationInProgress => "operation_in_progress",
            Self::TrackedChangesPresent => "tracked_changes_present",
            Self::UntrackedEntriesPresent => "untracked_entries_present",
            Self::HiddenIndexEntriesPresent => "hidden_index_entries_present",
            Self::PopulatedSubmodulesPresent => "populated_submodules_present",
            Self::PerWorktreeRefsPresent => "per_worktree_refs_present",
            Self::NotOwnedByCurrentUser => "not_owned_by_current_user",
            Self::RecentlyActive => "recently_active",
        }
    }
}

impl fmt::Display for LinkedWorktreeReclaimVeto {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkedWorktreeReclaimAuthority {
    /// Everything the worktree holds, ignored files aside, already exists elsewhere in the
    /// repository or is covered by the named compensation.
    PreservedInRepository,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkedWorktreeReclaimCompensation {
    /// A preserving ref already reaches HEAD.
    NoneRequired,
    /// The detached HEAD commit must be pinned under a ref before the worktree is removed.
    PinHeadCommit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkedWorktreeReclaimPolicy {
    minimum_idle_seconds: i64,
}

impl LinkedWorktreeReclaimPolicy {
    /// Build a policy from an explicit idle window.
    ///
    /// # Errors
    ///
    /// Refuses a window below [`MIN_LINKED_WORKTREE_RECLAIM_IDLE_SECONDS`].
    pub const fn new(minimum_idle_seconds: i64) -> Result<Self, LinkedWorktreeReclaimError> {
        if minimum_idle_seconds < MIN_LINKED_WORKTREE_RECLAIM_IDLE_SECONDS {
            return Err(idle_window_too_small());
        }
        Ok(Self {
            minimum_idle_seconds,
        })
    }

    #[must_use]
    pub const fn minimum_idle_seconds(self) -> i64 {
        self.minimum_idle_seconds
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum LinkedWorktreeReclaimDecision {
    Refused {
        vetoes: Vec<LinkedWorktreeReclaimVeto>,
    },
    Eligible {
        authority: LinkedWorktreeReclaimAuthority,
        compensation: LinkedWorktreeReclaimCompensation,
        idle_seconds: i64,
    },
}

impl LinkedWorktreeReclaimDecision {
    #[must_use]
    pub const fn is_eligible(&self) -> bool {
        matches!(self, Self::Eligible { .. })
    }

    #[must_use]
    pub fn vetoes(&self) -> &[LinkedWorktreeReclaimVeto] {
        match self {
            Self::Refused { vetoes } => vetoes,
            Self::Eligible { .. } => &[],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LinkedWorktreeReclaimPlan {
    schema_version: u8,
    decision: LinkedWorktreeReclaimDecision,
    minimum_idle_seconds: i64,
}

impl LinkedWorktreeReclaimPlan {
    #[must_use]
    pub const fn decision(&self) -> &LinkedWorktreeReclaimDecision {
        &self.decision
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn minimum_idle_seconds(&self) -> i64 {
        self.minimum_idle_seconds
    }
}

/// Decide whether one observed linked worktree may be reclaimed.
///
/// Every applicable veto is collected, so one refusal reports all of its reasons. `now_seconds` is
/// supplied rather than read, which keeps the decision a pure function of its observations.
///
/// # Errors
///
/// Refuses facts whose last activity is in the future relative to `now_seconds`, or whose idle
/// arithmetic would overflow. Disagreeing clocks are not evidence a decision should rest on.
pub fn plan_linked_worktree_reclaim(
    facts: &LinkedWorktreeFacts,
    policy: LinkedWorktreeReclaimPolicy,
    now_seconds: i64,
) -> Result<LinkedWorktreeReclaimPlan, LinkedWorktreeReclaimError> {
    let mut vetoes = Vec::new();
    if !facts.linked {
        vetoes.push(LinkedWorktreeReclaimVeto::MainWorktree);
    }
    if facts.locked {
        vetoes.push(LinkedWorktreeReclaimVeto::Locked);
    }
    if facts.operation_in_progress {
        vetoes.push(LinkedWorktreeReclaimVeto::OperationInProgress);
    }
    if facts.tracked_changes_present {
        vetoes.push(LinkedWorktreeReclaimVeto::TrackedChangesPresent);
    }
    if facts.untracked_entry_count > 0 {
        vetoes.push(LinkedWorktreeReclaimVeto::UntrackedEntriesPresent);
    }
    if facts.hidden_index_entries_present {
        vetoes.push(LinkedWorktreeReclaimVeto::HiddenIndexEntriesPresent);
    }
    if facts.populated_submodules_present {
        vetoes.push(LinkedWorktreeReclaimVeto::PopulatedSubmodulesPresent);
    }
    if facts.per_worktree_refs_present {
        vetoes.push(LinkedWorktreeReclaimVeto::PerWorktreeRefsPresent);
    }
    if !facts.owned_by_current_user {
        vetoes.push(LinkedWorktreeReclaimVeto::NotOwnedByCurrentUser);
    }

    let idle_seconds = now_seconds
        .checked_sub(facts.last_activity_seconds)
        .ok_or_else(idle_overflow)?;
    if idle_seconds < 0 {
        return Err(future_timestamp());
    }
    if idle_seconds < policy.minimum_idle_seconds {
        vetoes.push(LinkedWorktreeReclaimVeto::RecentlyActive);
    }

    vetoes.sort_unstable();
    vetoes.dedup();

    let decision = if vetoes.is_empty() {
        LinkedWorktreeReclaimDecision::Eligible {
            authority: LinkedWorktreeReclaimAuthority::PreservedInRepository,
            compensation: match facts.head_reachability {
                HeadReachability::ReachableFromSharedRef => {
                    LinkedWorktreeReclaimCompensation::NoneRequired
                }
                HeadReachability::OnlyFromThisWorktree => {
                    LinkedWorktreeReclaimCompensation::PinHeadCommit
                }
            },
            idle_seconds,
        }
    } else {
        LinkedWorktreeReclaimDecision::Refused { vetoes }
    };

    Ok(LinkedWorktreeReclaimPlan {
        schema_version: LINKED_WORKTREE_RECLAIM_SCHEMA_VERSION,
        decision,
        minimum_idle_seconds: policy.minimum_idle_seconds,
    })
}

// --- Observation -----------------------------------------------------------

/// One linked worktree registered with the repository, as `git worktree list` reported it.
#[derive(Clone, PartialEq, Eq)]
pub struct LinkedWorktreeEntry {
    path: PathBuf,
    locked: bool,
    prunable: bool,
    duplicate: bool,
}

impl LinkedWorktreeEntry {
    /// Registered checkout path. Private: callers must not publish it.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub const fn locked(&self) -> bool {
        self.locked
    }

    /// Git reports the registration as stale: the checkout directory or its `.git` file is
    /// missing. The directory may still hold data, and pruning deletes per-worktree refs and logs,
    /// so a stale registration grants no cleanup authority either.
    #[must_use]
    pub const fn prunable(&self) -> bool {
        self.prunable
    }
}

impl fmt::Debug for LinkedWorktreeEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LinkedWorktreeEntry")
            .field("path", &"<private-worktree-path>")
            .field("locked", &self.locked)
            .field("prunable", &self.prunable)
            .field("duplicate", &self.duplicate)
            .finish()
    }
}

/// The linked worktrees of one repository, bound to its common Git directory.
#[derive(Clone, PartialEq, Eq)]
pub struct LinkedWorktreeInventory {
    repository: PathBuf,
    common_dir: PathBuf,
    linked: Vec<LinkedWorktreeEntry>,
}

impl LinkedWorktreeInventory {
    #[must_use]
    pub fn linked(&self) -> &[LinkedWorktreeEntry] {
        &self.linked
    }
}

impl fmt::Debug for LinkedWorktreeInventory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LinkedWorktreeInventory")
            .field("repository", &"<private-repository-path>")
            .field("linked_count", &self.linked.len())
            .finish_non_exhaustive()
    }
}

/// List the linked worktrees of the repository whose main worktree is `repository`.
///
/// # Errors
///
/// Refuses a `repository` that is not the canonical main worktree, malformed or oversized Git
/// output, and more than [`MAX_LINKED_WORKTREES`] linked worktrees.
pub fn list_linked_worktrees(
    observer: &ProjectCheckoutObserver,
    repository: &Path,
    executor: &impl TimedCommandExecutor,
) -> Result<LinkedWorktreeInventory, LinkedWorktreeReclaimError> {
    let canonical = std::fs::canonicalize(repository).map_err(|_| unavailable())?;
    if canonical.as_path() != repository {
        return Err(not_main_worktree());
    }
    let common_dir = absolute_git_path(observer, repository, "--git-common-dir", executor)?;
    let git_dir = absolute_git_path(observer, repository, "--git-dir", executor)?;
    if git_dir != common_dir {
        return Err(not_main_worktree());
    }
    // The index layout scanned below embeds object names; only the reviewed SHA-1 width is read.
    let format = git(
        observer,
        repository,
        &["rev-parse", "--show-object-format"],
        executor,
    )?;
    if format.stdout != "sha1\n" {
        return Err(object_format_unsupported());
    }
    let record = git_bounded(
        observer,
        repository,
        &["worktree", "list", "--porcelain", "-z"],
        MAX_CAPTURED_STREAM_BYTES,
        executor,
    )?;
    let mut records = parse_worktree_list(&record.stdout)?.into_iter();
    let main = records.next().ok_or_else(invalid_output)?;
    if main.path != repository {
        return Err(not_main_worktree());
    }
    let mut linked = records.collect::<Vec<_>>();
    if linked.len() > MAX_LINKED_WORKTREES {
        return Err(too_many_worktrees());
    }
    // Two registrations naming one checkout cannot both be described by its facts.
    for index in 0..linked.len() {
        let path = linked[index].path.clone();
        let shared = path == main.path
            || linked
                .iter()
                .enumerate()
                .any(|(other, entry)| other != index && entry.path == path);
        linked[index].duplicate = shared;
    }
    Ok(LinkedWorktreeInventory {
        repository: repository.to_path_buf(),
        common_dir,
        linked,
    })
}

/// Observe the reclaim-relevant facts of one inventory entry without mutation or network access.
///
/// # Errors
///
/// Refuses a stale, duplicated, or aliased registration; a checkout the project observer cannot
/// observe consistently; a checkout bound to a different repository or administrative directory;
/// an unreadable or malformed index; and malformed or oversized Git output.
pub fn observe_linked_worktree(
    observer: &ProjectCheckoutObserver,
    inventory: &LinkedWorktreeInventory,
    entry: &LinkedWorktreeEntry,
    executor: &impl TimedCommandExecutor,
) -> Result<LinkedWorktreeFacts, LinkedWorktreeReclaimError> {
    if entry.prunable {
        return Err(registration_stale());
    }
    if entry.duplicate {
        return Err(registration_duplicated());
    }
    // Git records real paths when it adds a worktree. A registered path that no longer resolves to
    // itself has been replaced by a symlink or alias, and would let one checkout's facts stand in
    // for another registration.
    let checkout = entry.path.clone();
    if std::fs::canonicalize(&checkout).map_err(|_| registration_stale())? != checkout {
        return Err(registration_aliased());
    }
    let observation = observer
        .observe(&checkout, executor)
        .map_err(|_| checkout_unobservable())?;

    let common_dir = absolute_git_path(observer, &checkout, "--git-common-dir", executor)?;
    if common_dir != inventory.common_dir {
        return Err(foreign_repository());
    }
    let git_dir = absolute_git_path(observer, &checkout, "--git-dir", executor)?;
    let linked = git_dir != common_dir;
    if linked && read_gitdir_backlink(&git_dir)? != checkout.join(".git") {
        return Err(administrative_directory_mismatch());
    }

    let head_reachability = match observation.branch() {
        // Removing a worktree never deletes its branch, so the branch ref keeps HEAD reachable.
        ProjectBranchState::Attached { .. } => HeadReachability::ReachableFromSharedRef,
        ProjectBranchState::Detached => {
            // Asked from the main worktree, restricted to preserving namespaces, so neither this
            // worktree's own refs nor short-lived refs such as the stash count as preservation.
            // `rev-list --not` walks only back to the merge base. `for-each-ref --contains` walks
            // every ref's history when the answer is "none", which took about eight seconds on a
            // 9,000-ref repository and ran into the command timeout under load.
            let commit = observation.commit().as_str();
            let mut arguments = vec!["rev-list", "-n1", commit, "--not"];
            arguments.extend(PRESERVING_REF_SELECTORS);
            let unreached = git(observer, &inventory.repository, &arguments, executor)?;
            match unreached.stdout.strip_suffix('\n') {
                None if unreached.stdout.is_empty() => HeadReachability::ReachableFromSharedRef,
                Some(line) if line == commit => HeadReachability::OnlyFromThisWorktree,
                _ => return Err(invalid_output()),
            }
        }
    };

    let per_worktree_refs_present = if linked {
        let mut arguments = vec!["for-each-ref", "--count=1", "--format=%(refname)"];
        arguments.extend(PER_WORKTREE_REF_PATTERNS);
        !git(observer, &checkout, &arguments, executor)?
            .stdout
            .is_empty()
    } else {
        false
    };

    let index = scan_index(&read_index(&git_dir.join("index"))?)?;
    let mut populated_submodules_present = directory_nonempty(&git_dir.join("modules"))?;
    for path in &index.gitlink_paths {
        let relative = relative_index_path(path)?;
        populated_submodules_present |= submodule_path_populated(&checkout.join(relative))?;
    }

    let operation_in_progress = OPERATION_MARKERS
        .iter()
        .map(|marker| entry_present(&git_dir.join(marker)))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .any(|present| present);
    let locked = entry.locked || entry_present(&git_dir.join("locked"))?;
    let last_activity_seconds = last_activity_seconds(&git_dir, &checkout)?;

    Ok(LinkedWorktreeFacts {
        linked,
        locked,
        operation_in_progress,
        tracked_changes_present: observation.tracked_changes_present(),
        untracked_entry_count: observation.untracked_entry_count(),
        hidden_index_entries_present: index.hidden_entries_present,
        populated_submodules_present,
        per_worktree_refs_present,
        // Compared with the effective user rather than the parent directory: shared scratch roots
        // such as `/tmp` are root-owned, and the parent's owner says nothing about who may remove
        // a checkout beneath it.
        owned_by_current_user: std::fs::symlink_metadata(&checkout)
            .map_err(|_| unavailable())?
            .uid()
            == rustix::process::geteuid().as_raw(),
        head_reachability,
        last_activity_seconds,
    })
}

fn git(
    observer: &ProjectCheckoutObserver,
    checkout: &Path,
    arguments: &[&str],
    executor: &impl TimedCommandExecutor,
) -> Result<ExecutionRecord, LinkedWorktreeReclaimError> {
    let record = observer
        .git(checkout, arguments, executor)
        .map_err(|_| unavailable())?;
    require_success(record)
}

fn git_bounded(
    observer: &ProjectCheckoutObserver,
    checkout: &Path,
    arguments: &[&str],
    max_stdout_bytes: usize,
    executor: &impl TimedCommandExecutor,
) -> Result<ExecutionRecord, LinkedWorktreeReclaimError> {
    let record = observer
        .git_bounded(checkout, arguments, max_stdout_bytes, executor)
        .map_err(|_| unavailable())?;
    require_success(record)
}

fn require_success(record: ExecutionRecord) -> Result<ExecutionRecord, LinkedWorktreeReclaimError> {
    if record.success && record.status == Some(0) && record.stderr.is_empty() {
        Ok(record)
    } else {
        Err(unavailable())
    }
}

fn absolute_git_path(
    observer: &ProjectCheckoutObserver,
    checkout: &Path,
    selector: &str,
    executor: &impl TimedCommandExecutor,
) -> Result<PathBuf, LinkedWorktreeReclaimError> {
    let record = git(
        observer,
        checkout,
        &["rev-parse", "--path-format=absolute", selector],
        executor,
    )?;
    let line = record.stdout.strip_suffix('\n').unwrap_or(&record.stdout);
    if line.is_empty() || line.contains('\n') || line.contains('\0') {
        return Err(invalid_output());
    }
    let path = PathBuf::from(line);
    if !path.is_absolute() {
        return Err(invalid_output());
    }
    std::fs::canonicalize(&path).map_err(|_| unavailable())
}

/// Read the administrative directory's record of which checkout owns it.
fn read_gitdir_backlink(git_dir: &Path) -> Result<PathBuf, LinkedWorktreeReclaimError> {
    let mut contents = String::new();
    std::fs::File::open(git_dir.join("gitdir"))
        .map_err(|_| administrative_directory_mismatch())?
        .take(4_097)
        .read_to_string(&mut contents)
        .map_err(|_| administrative_directory_mismatch())?;
    let line = contents.strip_suffix('\n').unwrap_or(&contents);
    if line.is_empty() || line.len() > 4_096 || line.contains('\n') || line.contains('\0') {
        return Err(administrative_directory_mismatch());
    }
    // `worktree add --relative-paths` records the backlink relative to the administrative
    // directory; resolve it so both layouts compare against the same canonical checkout.
    let recorded = PathBuf::from(line);
    let resolved = if recorded.is_absolute() {
        recorded
    } else {
        git_dir.join(recorded)
    };
    std::fs::canonicalize(resolved).map_err(|_| administrative_directory_mismatch())
}

fn parse_worktree_list(
    value: &str,
) -> Result<Vec<LinkedWorktreeEntry>, LinkedWorktreeReclaimError> {
    if !value.is_empty() && !value.ends_with('\0') {
        return Err(invalid_output());
    }
    let mut entries = Vec::new();
    let mut current: Option<LinkedWorktreeEntry> = None;
    for field in value.split_terminator('\0') {
        if field.is_empty() {
            entries.extend(current.take());
            continue;
        }
        if let Some(path) = field.strip_prefix("worktree ") {
            entries.extend(current.take());
            let path = PathBuf::from(path);
            if !path.is_absolute() {
                return Err(invalid_output());
            }
            current = Some(LinkedWorktreeEntry {
                path,
                locked: false,
                prunable: false,
                duplicate: false,
            });
            continue;
        }
        let entry = current.as_mut().ok_or_else(invalid_output)?;
        if field == "locked" || field.starts_with("locked ") {
            entry.locked = true;
        } else if field == "prunable" || field.starts_with("prunable ") {
            entry.prunable = true;
        } else if field == "bare" {
            return Err(not_main_worktree());
        }
    }
    entries.extend(current);
    if entries.len() > MAX_LINKED_WORKTREES.saturating_add(1) {
        return Err(too_many_worktrees());
    }
    Ok(entries)
}

// --- Index scan ------------------------------------------------------------
//
// `git ls-files -v` would report the same flags, but its output scales with path length and exceeds
// the process capture ceiling on ordinary large repositories. The index file itself carries both
// facts this module needs in a fixed layout, so it is read directly and bounded. Only flags, modes,
// and paths are interpreted. The checksum is not verified; instead the extension chain must end
// exactly at the trailing SHA-1, which catches any misparsed layout. A split index keeps entries and
// their flags in a separate shared file, so its `link` extension is refused rather than trusted.

const INDEX_SIGNATURE: &[u8; 4] = b"DIRC";
const INDEX_HEADER_BYTES: usize = 12;
const INDEX_ENTRY_FIXED_BYTES: usize = 62;
const INDEX_ENTRY_MODE_OFFSET: usize = 24;
const INDEX_ENTRY_FLAGS_OFFSET: usize = 60;
const INDEX_ASSUME_VALID_FLAG: u16 = 0x8000;
const INDEX_EXTENDED_FLAG: u16 = 0x4000;
const INDEX_SKIP_WORKTREE_FLAG: u16 = 0x4000;
const INDEX_NAME_LENGTH_MASK: u16 = 0x0fff;
const INDEX_GITLINK_MODE: u32 = 0o160_000;
const INDEX_MAX_PATH_BYTES: usize = 4_096;
const INDEX_MAX_VARINT_BYTES: usize = 10;
const INDEX_EXTENSION_HEADER_BYTES: usize = 8;
const INDEX_SHA1_BYTES: usize = 20;
const INDEX_SPLIT_LINK_EXTENSION: &[u8; 4] = b"link";

#[derive(Debug, Default, PartialEq, Eq)]
struct IndexScan {
    hidden_entries_present: bool,
    gitlink_paths: Vec<Vec<u8>>,
}

fn read_index(path: &Path) -> Result<Vec<u8>, LinkedWorktreeReclaimError> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        // A worktree that has never staged anything may have no index; it hides nothing.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(unavailable()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_LINKED_WORKTREE_INDEX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unavailable())?;
    if bytes.len() as u64 > MAX_LINKED_WORKTREE_INDEX_BYTES {
        return Err(index_too_large());
    }
    Ok(bytes)
}

fn scan_index(bytes: &[u8]) -> Result<IndexScan, LinkedWorktreeReclaimError> {
    let mut scan = IndexScan::default();
    if bytes.is_empty() {
        return Ok(scan);
    }
    if bytes.len() < INDEX_HEADER_BYTES || &bytes[..4] != INDEX_SIGNATURE {
        return Err(invalid_index());
    }
    let version = read_u32(bytes, 4)?;
    if !(2..=4).contains(&version) {
        return Err(invalid_index());
    }
    let entries = read_u32(bytes, 8)?;
    let mut offset = INDEX_HEADER_BYTES;
    let mut previous: Vec<u8> = Vec::new();
    // A split index stores placeholder entries with stripped, empty names. They are tolerated only
    // until the extension walk proves the index is split, which is refused with its own code.
    let mut empty_path_seen = false;
    for _ in 0..entries {
        let fixed_end = offset
            .checked_add(INDEX_ENTRY_FIXED_BYTES)
            .ok_or_else(invalid_index)?;
        let mode = read_u32(bytes, offset + INDEX_ENTRY_MODE_OFFSET)?;
        let flags = read_u16(bytes, offset + INDEX_ENTRY_FLAGS_OFFSET)?;
        if flags & INDEX_ASSUME_VALID_FLAG != 0 {
            scan.hidden_entries_present = true;
        }
        let mut name_start = fixed_end;
        if flags & INDEX_EXTENDED_FLAG != 0 {
            if version < 3 {
                return Err(invalid_index());
            }
            if read_u16(bytes, fixed_end)? & INDEX_SKIP_WORKTREE_FLAG != 0 {
                scan.hidden_entries_present = true;
            }
            name_start = fixed_end + 2;
        }

        let (path, next) = if version == 4 {
            let (strip, varint_bytes) = read_index_varint(bytes, name_start)?;
            let suffix_start = name_start + varint_bytes;
            let terminator = find_nul(bytes, suffix_start)?;
            let keep = usize::try_from(strip)
                .ok()
                .and_then(|strip| previous.len().checked_sub(strip))
                .ok_or_else(invalid_index)?;
            let mut path = previous[..keep].to_vec();
            path.extend_from_slice(&bytes[suffix_start..terminator]);
            (path, terminator + 1)
        } else {
            let declared = usize::from(flags & INDEX_NAME_LENGTH_MASK);
            let terminator = if declared < usize::from(INDEX_NAME_LENGTH_MASK) {
                let terminator = name_start + declared;
                if bytes.get(terminator) != Some(&0) {
                    return Err(invalid_index());
                }
                terminator
            } else {
                find_nul(bytes, name_start + declared)?
            };
            // Entries are NUL-padded to a multiple of eight bytes, with at least one NUL.
            let entry_bytes = ((terminator - offset) + 8) & !7;
            let next = offset.checked_add(entry_bytes).ok_or_else(invalid_index)?;
            if next > bytes.len() {
                return Err(invalid_index());
            }
            (bytes[name_start..terminator].to_vec(), next)
        };
        if path.len() > INDEX_MAX_PATH_BYTES {
            return Err(invalid_index());
        }
        empty_path_seen |= path.is_empty();
        if mode == INDEX_GITLINK_MODE && !path.is_empty() {
            if scan.gitlink_paths.len() >= MAX_LINKED_WORKTREE_GITLINKS {
                return Err(invalid_index());
            }
            scan.gitlink_paths.push(path.clone());
        }
        previous = path;
        offset = next;
    }

    let trailer = bytes
        .len()
        .checked_sub(INDEX_SHA1_BYTES)
        .ok_or_else(invalid_index)?;
    while offset < trailer {
        let header_end = offset
            .checked_add(INDEX_EXTENSION_HEADER_BYTES)
            .ok_or_else(invalid_index)?;
        if header_end > trailer {
            return Err(invalid_index());
        }
        if &bytes[offset..offset + 4] == INDEX_SPLIT_LINK_EXTENSION {
            return Err(split_index_unsupported());
        }
        let size = usize::try_from(read_u32(bytes, offset + 4)?).map_err(|_| invalid_index())?;
        offset = header_end.checked_add(size).ok_or_else(invalid_index)?;
    }
    if offset != trailer || empty_path_seen {
        return Err(invalid_index());
    }
    Ok(scan)
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, LinkedWorktreeReclaimError> {
    let end = offset.checked_add(2).ok_or_else(invalid_index)?;
    let slice = bytes.get(offset..end).ok_or_else(invalid_index)?;
    Ok(u16::from_be_bytes([slice[0], slice[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, LinkedWorktreeReclaimError> {
    let end = offset.checked_add(4).ok_or_else(invalid_index)?;
    let slice = bytes.get(offset..end).ok_or_else(invalid_index)?;
    Ok(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn find_nul(bytes: &[u8], start: usize) -> Result<usize, LinkedWorktreeReclaimError> {
    let window_end = start
        .checked_add(INDEX_MAX_PATH_BYTES + 1)
        .map_or(bytes.len(), |end| end.min(bytes.len()));
    let window = bytes.get(start..window_end).ok_or_else(invalid_index)?;
    window
        .iter()
        .position(|byte| *byte == 0)
        .map(|position| start + position)
        .ok_or_else(invalid_index)
}

/// Decode Git's offset varint: big-endian groups of seven bits, adding one per continuation.
fn read_index_varint(
    bytes: &[u8],
    start: usize,
) -> Result<(u64, usize), LinkedWorktreeReclaimError> {
    let mut consumed = 0;
    let mut value: u64 = 0;
    loop {
        if consumed == INDEX_MAX_VARINT_BYTES {
            return Err(invalid_index());
        }
        let byte = *bytes.get(start + consumed).ok_or_else(invalid_index)?;
        if consumed > 0 {
            value = value
                .checked_add(1)
                .and_then(|value| value.checked_mul(128))
                .ok_or_else(invalid_index)?;
        }
        value |= u64::from(byte & 0x7f);
        consumed += 1;
        if byte & 0x80 == 0 {
            return Ok((value, consumed));
        }
    }
}

/// Accept only a plain relative path of normal components from an index entry.
fn relative_index_path(bytes: &[u8]) -> Result<&Path, LinkedWorktreeReclaimError> {
    let path = Path::new(OsStr::from_bytes(bytes));
    if path
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        Ok(path)
    } else {
        Err(invalid_index())
    }
}

fn entry_present(path: &Path) -> Result<bool, LinkedWorktreeReclaimError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(unavailable()),
    }
}

/// A submodule path holds data unless it is absent or an empty directory.
///
/// Status runs with submodules ignored, so anything at a gitlink path -- a repository, a gitfile, or
/// plain files left behind after its `.git` was deleted -- is invisible to it and must veto here.
fn submodule_path_populated(path: &Path) -> Result<bool, LinkedWorktreeReclaimError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => directory_nonempty(path),
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(unavailable()),
    }
}

fn directory_nonempty(path: &Path) -> Result<bool, LinkedWorktreeReclaimError> {
    match std::fs::read_dir(path) {
        Ok(mut entries) => Ok(entries.next().is_some()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(unavailable()),
    }
}

fn last_activity_seconds(
    git_dir: &Path,
    checkout: &Path,
) -> Result<i64, LinkedWorktreeReclaimError> {
    let mut newest = std::fs::symlink_metadata(checkout)
        .map_err(|_| unavailable())?
        .mtime();
    for name in ACTIVITY_ENTRIES {
        match std::fs::symlink_metadata(git_dir.join(name)) {
            Ok(metadata) => newest = newest.max(metadata.mtime()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(unavailable()),
        }
    }
    Ok(newest)
}

// --- Errors ----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkedWorktreeReclaimErrorKind {
    InvalidPolicy,
    InvalidRepository,
    InvalidRegistration,
    DisagreeingEvidence,
    Overflow,
    Unavailable,
    InvalidOutput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LinkedWorktreeReclaimError {
    kind: LinkedWorktreeReclaimErrorKind,
    code: &'static str,
    problem: &'static str,
}

impl LinkedWorktreeReclaimError {
    #[must_use]
    pub const fn kind(&self) -> LinkedWorktreeReclaimErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }

    #[must_use]
    pub const fn problem(&self) -> &'static str {
        self.problem
    }
}

impl fmt::Display for LinkedWorktreeReclaimError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.problem)
    }
}

impl std::error::Error for LinkedWorktreeReclaimError {}

const fn error(
    kind: LinkedWorktreeReclaimErrorKind,
    code: &'static str,
    problem: &'static str,
) -> LinkedWorktreeReclaimError {
    LinkedWorktreeReclaimError {
        kind,
        code,
        problem,
    }
}

const fn idle_window_too_small() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::InvalidPolicy,
        "idle_window_too_small",
        "the reclaim idle window is below the reviewed minimum",
    )
}

const fn not_main_worktree() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::InvalidRepository,
        "not_main_worktree",
        "the repository path is not the canonical main worktree of a non-bare repository",
    )
}

const fn foreign_repository() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::InvalidRepository,
        "foreign_repository",
        "the registered worktree resolves to a checkout of a different repository",
    )
}

const fn too_many_worktrees() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::InvalidOutput,
        "too_many_worktrees",
        "the repository has more linked worktrees than one inventory may describe",
    )
}

const fn registration_stale() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::InvalidRegistration,
        "registration_stale",
        "the worktree registration is stale: its directory or .git file is missing",
    )
}

const fn registration_duplicated() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::InvalidRegistration,
        "registration_duplicated",
        "more than one worktree registration names the same checkout",
    )
}

const fn registration_aliased() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::InvalidRegistration,
        "registration_aliased",
        "the registered worktree path no longer resolves to itself",
    )
}

const fn administrative_directory_mismatch() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::InvalidRegistration,
        "administrative_directory_mismatch",
        "the worktree's administrative directory does not point back to this checkout",
    )
}

const fn checkout_unobservable() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::Unavailable,
        "checkout_unobservable",
        "the checkout could not be observed consistently",
    )
}

const fn index_too_large() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::InvalidOutput,
        "index_too_large",
        "the worktree index exceeds the reviewed size bound",
    )
}

const fn split_index_unsupported() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::Unavailable,
        "split_index_unsupported",
        "the worktree uses a split index, whose shared entries this observer does not read",
    )
}

const fn object_format_unsupported() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::InvalidRepository,
        "object_format_unsupported",
        "the repository object format is outside the reviewed SHA-1 index layout",
    )
}

const fn invalid_index() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::InvalidOutput,
        "invalid_index",
        "the worktree index is malformed or outside the supported v2-v4 layout",
    )
}

const fn future_timestamp() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::DisagreeingEvidence,
        "future_timestamp",
        "the observed worktree activity is newer than the supplied clock reading",
    )
}

const fn idle_overflow() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::Overflow,
        "idle_overflow",
        "the idle window could not be computed without overflow",
    )
}

const fn unavailable() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::Unavailable,
        "unavailable",
        "worktree evidence could not be read",
    )
}

const fn invalid_output() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::InvalidOutput,
        "invalid_output",
        "Git returned malformed worktree evidence",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;
    const DAY: i64 = 86_400;

    fn policy() -> LinkedWorktreeReclaimPolicy {
        LinkedWorktreeReclaimPolicy::new(DAY).expect("valid policy")
    }

    fn clean_idle() -> LinkedWorktreeFacts {
        LinkedWorktreeFacts {
            linked: true,
            locked: false,
            operation_in_progress: false,
            tracked_changes_present: false,
            untracked_entry_count: 0,
            hidden_index_entries_present: false,
            populated_submodules_present: false,
            per_worktree_refs_present: false,
            owned_by_current_user: true,
            head_reachability: HeadReachability::ReachableFromSharedRef,
            last_activity_seconds: NOW - 2 * DAY,
        }
    }

    #[test]
    fn clean_preserved_idle_worktree_is_eligible_without_compensation() {
        let plan = plan_linked_worktree_reclaim(&clean_idle(), policy(), NOW).expect("plan");
        assert_eq!(
            plan.decision(),
            &LinkedWorktreeReclaimDecision::Eligible {
                authority: LinkedWorktreeReclaimAuthority::PreservedInRepository,
                compensation: LinkedWorktreeReclaimCompensation::NoneRequired,
                idle_seconds: 2 * DAY,
            }
        );
    }

    #[test]
    fn unreachable_detached_head_requires_a_pin_rather_than_refusing() {
        let facts = LinkedWorktreeFacts {
            head_reachability: HeadReachability::OnlyFromThisWorktree,
            ..clean_idle()
        };
        let plan = plan_linked_worktree_reclaim(&facts, policy(), NOW).expect("plan");
        assert!(matches!(
            plan.decision(),
            LinkedWorktreeReclaimDecision::Eligible {
                compensation: LinkedWorktreeReclaimCompensation::PinHeadCommit,
                ..
            }
        ));
    }

    #[test]
    fn every_applicable_veto_is_reported_in_order() {
        let facts = LinkedWorktreeFacts {
            linked: false,
            locked: true,
            operation_in_progress: true,
            tracked_changes_present: true,
            untracked_entry_count: 3,
            hidden_index_entries_present: true,
            populated_submodules_present: true,
            per_worktree_refs_present: true,
            owned_by_current_user: false,
            head_reachability: HeadReachability::OnlyFromThisWorktree,
            last_activity_seconds: NOW,
        };
        let plan = plan_linked_worktree_reclaim(&facts, policy(), NOW).expect("plan");
        assert_eq!(
            plan.decision().vetoes(),
            &[
                LinkedWorktreeReclaimVeto::MainWorktree,
                LinkedWorktreeReclaimVeto::Locked,
                LinkedWorktreeReclaimVeto::OperationInProgress,
                LinkedWorktreeReclaimVeto::TrackedChangesPresent,
                LinkedWorktreeReclaimVeto::UntrackedEntriesPresent,
                LinkedWorktreeReclaimVeto::HiddenIndexEntriesPresent,
                LinkedWorktreeReclaimVeto::PopulatedSubmodulesPresent,
                LinkedWorktreeReclaimVeto::PerWorktreeRefsPresent,
                LinkedWorktreeReclaimVeto::NotOwnedByCurrentUser,
                LinkedWorktreeReclaimVeto::RecentlyActive,
            ]
        );
    }

    #[test]
    fn idle_window_boundary_is_inclusive() {
        let facts = LinkedWorktreeFacts {
            last_activity_seconds: NOW - DAY,
            ..clean_idle()
        };
        assert!(
            plan_linked_worktree_reclaim(&facts, policy(), NOW)
                .expect("plan")
                .decision()
                .is_eligible()
        );
        let facts = LinkedWorktreeFacts {
            last_activity_seconds: NOW - DAY + 1,
            ..clean_idle()
        };
        assert_eq!(
            plan_linked_worktree_reclaim(&facts, policy(), NOW)
                .expect("plan")
                .decision()
                .vetoes(),
            &[LinkedWorktreeReclaimVeto::RecentlyActive]
        );
    }

    #[test]
    fn future_activity_is_disagreeing_evidence() {
        let facts = LinkedWorktreeFacts {
            last_activity_seconds: NOW + 1,
            ..clean_idle()
        };
        let error = plan_linked_worktree_reclaim(&facts, policy(), NOW).expect_err("future");
        assert_eq!(error.code(), "future_timestamp");
    }

    #[test]
    fn idle_window_below_floor_is_refused() {
        let error = LinkedWorktreeReclaimPolicy::new(MIN_LINKED_WORKTREE_RECLAIM_IDLE_SECONDS - 1)
            .expect_err("too small");
        assert_eq!(error.code(), "idle_window_too_small");
    }

    #[test]
    fn worktree_list_parses_locked_and_prunable_records() {
        let listing = "worktree /repo\0HEAD 1111111111111111111111111111111111111111\0branch refs/heads/main\0\0\
worktree /wt/a\0HEAD 2222222222222222222222222222222222222222\0detached\0locked keep me\0\0\
worktree /wt/b\0HEAD 3333333333333333333333333333333333333333\0branch refs/heads/b\0prunable gitdir file points to non-existent location\0\0";
        let entries = parse_worktree_list(listing).expect("parse");
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].path(), Path::new("/repo"));
        assert!(entries[1].locked() && !entries[1].prunable());
        assert!(!entries[2].locked() && entries[2].prunable());
    }

    #[test]
    fn worktree_list_refuses_relative_paths_and_unterminated_output() {
        assert!(parse_worktree_list("worktree relative\0\0").is_err());
        assert!(parse_worktree_list("worktree /repo").is_err());
        assert!(parse_worktree_list("HEAD 1111\0\0").is_err());
    }

    /// Build one index entry in the v2/v3 layout, padded as Git pads it.
    fn padded_entry(mode: u32, flags: u16, extended: Option<u16>, path: &[u8]) -> Vec<u8> {
        let mut entry = vec![0_u8; INDEX_ENTRY_FIXED_BYTES];
        entry[INDEX_ENTRY_MODE_OFFSET..INDEX_ENTRY_MODE_OFFSET + 4]
            .copy_from_slice(&mode.to_be_bytes());
        let length = u16::try_from(path.len()).expect("short fixture path");
        let flags = flags | length.min(INDEX_NAME_LENGTH_MASK);
        entry[INDEX_ENTRY_FLAGS_OFFSET..INDEX_ENTRY_FLAGS_OFFSET + 2]
            .copy_from_slice(&flags.to_be_bytes());
        if let Some(extended) = extended {
            entry.extend_from_slice(&extended.to_be_bytes());
        }
        entry.extend_from_slice(path);
        let padded = (entry.len() + 8) & !7;
        entry.resize(padded, 0);
        entry
    }

    fn index(version: u32, entries: &[Vec<u8>]) -> Vec<u8> {
        let mut bytes = INDEX_SIGNATURE.to_vec();
        bytes.extend_from_slice(&version.to_be_bytes());
        bytes.extend_from_slice(&u32::try_from(entries.len()).expect("count").to_be_bytes());
        for entry in entries {
            bytes.extend_from_slice(entry);
        }
        bytes.extend_from_slice(&[0_u8; 20]);
        bytes
    }

    #[test]
    fn index_scan_reports_assume_valid_skip_worktree_and_gitlinks() {
        let plain = padded_entry(0o100_644, 0, None, b"a.txt");
        let clean = scan_index(&index(2, std::slice::from_ref(&plain))).expect("scan");
        assert_eq!(clean, IndexScan::default());

        let assumed = padded_entry(0o100_644, INDEX_ASSUME_VALID_FLAG, None, b"b.txt");
        assert!(
            scan_index(&index(2, &[plain.clone(), assumed]))
                .expect("scan")
                .hidden_entries_present
        );

        let skipped = padded_entry(
            0o100_644,
            INDEX_EXTENDED_FLAG,
            Some(INDEX_SKIP_WORKTREE_FLAG),
            b"c.txt",
        );
        assert!(
            scan_index(&index(3, &[plain.clone(), skipped]))
                .expect("scan")
                .hidden_entries_present
        );

        let gitlink = padded_entry(INDEX_GITLINK_MODE, 0, None, b"vendor/sub");
        let scan = scan_index(&index(2, &[plain, gitlink])).expect("scan");
        assert_eq!(scan.gitlink_paths, vec![b"vendor/sub".to_vec()]);
    }

    #[test]
    fn index_scan_refuses_extended_flags_in_v2_and_truncation() {
        let skipped = padded_entry(
            0o100_644,
            INDEX_EXTENDED_FLAG,
            Some(INDEX_SKIP_WORKTREE_FLAG),
            b"c.txt",
        );
        assert!(scan_index(&index(2, std::slice::from_ref(&skipped))).is_err());
        let mut truncated = index(3, &[skipped]);
        truncated.truncate(INDEX_HEADER_BYTES + 20);
        assert!(scan_index(&truncated).is_err());
        assert!(scan_index(b"DIRX\0\0\0\x02\0\0\0\0").is_err());
    }

    #[test]
    fn index_extension_chain_must_end_at_the_trailer_and_refuses_split_index() {
        let plain = padded_entry(0o100_644, 0, None, b"a.txt");
        let mut with_tree = index(2, std::slice::from_ref(&plain));
        let trailer = with_tree.split_off(with_tree.len() - INDEX_SHA1_BYTES);
        with_tree.extend_from_slice(b"TREE");
        with_tree.extend_from_slice(&3_u32.to_be_bytes());
        with_tree.extend_from_slice(b"abc");
        let mut valid = with_tree.clone();
        valid.extend_from_slice(&trailer);
        assert!(scan_index(&valid).is_ok());

        let mut overrun = with_tree.clone();
        overrun.pop();
        overrun.extend_from_slice(&trailer);
        assert_eq!(
            scan_index(&overrun).expect_err("overrun").code(),
            "invalid_index"
        );

        let mut split = index(2, std::slice::from_ref(&plain));
        let trailer = split.split_off(split.len() - INDEX_SHA1_BYTES);
        split.extend_from_slice(INDEX_SPLIT_LINK_EXTENSION);
        split.extend_from_slice(&20_u32.to_be_bytes());
        split.extend_from_slice(&[0_u8; 20]);
        split.extend_from_slice(&trailer);
        assert_eq!(
            scan_index(&split).expect_err("split").code(),
            "split_index_unsupported"
        );
    }

    #[test]
    fn index_v4_reconstructs_prefix_compressed_paths() {
        let mut bytes = INDEX_SIGNATURE.to_vec();
        bytes.extend_from_slice(&4_u32.to_be_bytes());
        bytes.extend_from_slice(&2_u32.to_be_bytes());
        for (mode, strip, suffix) in [
            (0o100_644_u32, 0_u8, b"vendor/a".as_slice()),
            (INDEX_GITLINK_MODE, 1, b"sub".as_slice()),
        ] {
            let mut entry = vec![0_u8; INDEX_ENTRY_FIXED_BYTES];
            entry[INDEX_ENTRY_MODE_OFFSET..INDEX_ENTRY_MODE_OFFSET + 4]
                .copy_from_slice(&mode.to_be_bytes());
            entry.push(strip);
            entry.extend_from_slice(suffix);
            entry.push(0);
            bytes.extend_from_slice(&entry);
        }
        bytes.extend_from_slice(&[0_u8; INDEX_SHA1_BYTES]);
        let scan = scan_index(&bytes).expect("scan");
        assert_eq!(scan.gitlink_paths, vec![b"vendor/sub".to_vec()]);
    }

    #[test]
    fn index_varint_matches_git_offset_encoding() {
        assert_eq!(read_index_varint(&[0x05], 0).expect("one byte"), (5, 1));
        // Git encodes 128 as 0x80 0x00: (0 + 1) * 128 + 0.
        assert_eq!(
            read_index_varint(&[0x80, 0x00], 0).expect("two bytes"),
            (128, 2)
        );
        assert!(read_index_varint(&[0xff; 11], 0).is_err());
    }

    #[test]
    fn index_paths_must_be_plain_relative_components() {
        assert!(relative_index_path(b"vendor/sub").is_ok());
        assert!(relative_index_path(b"../escape").is_err());
        assert!(relative_index_path(b"/absolute").is_err());
    }
}
