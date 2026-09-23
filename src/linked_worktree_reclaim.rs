//! Read-only observation and pure reclaim planning for linked Git worktrees.
//!
//! A linked worktree is not reconstructible the way a Cargo target is. It can hold the only copy of
//! uncommitted edits, untracked files, initialized submodule history, an interrupted rebase, or a
//! commit that no ref reaches. Authority here therefore comes from preservation: everything the
//! worktree holds must already exist elsewhere in the repository, or be preservable by exactly one
//! named compensation -- pinning a detached HEAD under a ref before removal. Apparent idleness and a
//! path that looks like scratch grant nothing; the idle window is a grace period, not evidence.
//!
//! History that only a worktree's own reflog reaches is not preserved. That matches Git's own
//! reachability model, where reflog-only commits are already garbage once the reflog expires.
//!
//! Planning performs no mutation and emits no paths. It consumes observations the caller took and
//! returns one decision with every veto that applied, so a refusal explains itself completely.
//! Nothing here removes a worktree; an executor must re-observe immediately before acting and
//! belongs behind its own destructive review.

use std::fmt;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::process::{ExecutionRecord, TimedCommandExecutor};
use crate::project_checkout_observation::{ProjectBranchState, ProjectCheckoutObserver};

pub const LINKED_WORKTREE_RECLAIM_SCHEMA_VERSION: u8 = 1;

/// Most linked worktrees one inventory may describe. Larger inventories fail closed.
pub const MAX_LINKED_WORKTREES: usize = 256;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HeadReachability {
    /// A branch, tag, remote-tracking, or other shared ref contains the HEAD commit.
    ReachableFromSharedRef,
    /// Only this worktree's detached HEAD reaches the commit.
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
    pub initialized_submodules_present: bool,
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
    /// Submodule repositories are initialized inside this worktree and may hold unique commits.
    InitializedSubmodulesPresent,
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
            Self::InitializedSubmodulesPresent => "initialized_submodules_present",
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
pub enum LinkedWorktreeReclaimCompensation {
    /// Every commit the worktree reaches is already reachable from a shared ref.
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
    if facts.initialized_submodules_present {
        vetoes.push(LinkedWorktreeReclaimVeto::InitializedSubmodulesPresent);
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

    /// Git reports the checkout directory as missing; only metadata remains.
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
    let record = git(
        observer,
        repository,
        &["worktree", "list", "--porcelain", "-z"],
        executor,
    )?;
    let mut records = parse_worktree_list(&record.stdout)?.into_iter();
    let main = records.next().ok_or_else(invalid_output)?;
    if main.path != repository {
        return Err(not_main_worktree());
    }
    let linked = records.collect::<Vec<_>>();
    if linked.len() > MAX_LINKED_WORKTREES {
        return Err(too_many_worktrees());
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
/// Refuses a prunable entry, a checkout the project observer cannot observe consistently, a
/// checkout bound to a different repository, and malformed or oversized Git output.
pub fn observe_linked_worktree(
    observer: &ProjectCheckoutObserver,
    inventory: &LinkedWorktreeInventory,
    entry: &LinkedWorktreeEntry,
    executor: &impl TimedCommandExecutor,
) -> Result<LinkedWorktreeFacts, LinkedWorktreeReclaimError> {
    if entry.prunable {
        return Err(checkout_missing());
    }
    // Registered paths may pass through benign system aliases such as macOS `/tmp`. The project
    // observer refuses anything non-canonical, and the common-directory check below binds the
    // resolved checkout back to this repository.
    let checkout = std::fs::canonicalize(&entry.path).map_err(|_| checkout_missing())?;
    let observation = observer
        .observe(&checkout, executor)
        .map_err(|_| checkout_unobservable())?;

    let common_dir = absolute_git_path(observer, &checkout, "--git-common-dir", executor)?;
    if common_dir != inventory.common_dir {
        return Err(foreign_repository());
    }
    let git_dir = absolute_git_path(observer, &checkout, "--git-dir", executor)?;

    let head_reachability = match observation.branch() {
        // Removing a worktree never deletes its branch, so the branch ref keeps HEAD reachable.
        ProjectBranchState::Attached { .. } => HeadReachability::ReachableFromSharedRef,
        ProjectBranchState::Detached => {
            // Asked from the main worktree so this worktree's own per-worktree refs, which vanish
            // with it, cannot count as preservation.
            let contains = git(
                observer,
                &inventory.repository,
                &[
                    "for-each-ref",
                    "--count=1",
                    "--format=%(refname)",
                    "--contains",
                    observation.commit().as_str(),
                ],
                executor,
            )?;
            if contains.stdout.is_empty() {
                HeadReachability::OnlyFromThisWorktree
            } else {
                HeadReachability::ReachableFromSharedRef
            }
        }
    };

    let operation_in_progress = OPERATION_MARKERS
        .iter()
        .map(|marker| entry_present(&git_dir.join(marker)))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .any(|present| present);
    let initialized_submodules_present = directory_nonempty(&git_dir.join("modules"))?;
    let locked = entry.locked || entry_present(&git_dir.join("locked"))?;
    let last_activity_seconds = last_activity_seconds(&git_dir, &checkout)?;

    Ok(LinkedWorktreeFacts {
        linked: git_dir != common_dir,
        locked,
        operation_in_progress,
        tracked_changes_present: observation.tracked_changes_present(),
        untracked_entry_count: observation.untracked_entry_count(),
        initialized_submodules_present,
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

fn entry_present(path: &Path) -> Result<bool, LinkedWorktreeReclaimError> {
    match std::fs::symlink_metadata(path) {
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

const fn checkout_missing() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::Unavailable,
        "checkout_missing",
        "the registered worktree directory is missing",
    )
}

const fn checkout_unobservable() -> LinkedWorktreeReclaimError {
    error(
        LinkedWorktreeReclaimErrorKind::Unavailable,
        "checkout_unobservable",
        "the checkout could not be observed consistently",
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
            initialized_submodules_present: false,
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
            initialized_submodules_present: true,
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
                LinkedWorktreeReclaimVeto::InitializedSubmodulesPresent,
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
}
