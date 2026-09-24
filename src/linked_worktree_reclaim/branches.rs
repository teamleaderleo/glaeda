//! Local branches left behind once their worktree is gone and their work has landed.
//!
//! Removing a worktree keeps its branch, so agent sessions leave one local branch per task. A
//! branch here is deleted only when every one of these holds:
//!
//! - no worktree has it checked out;
//! - it is not protected: a remote default branch's name (`main`, `master`, whatever a
//!   `refs/remotes/*/HEAD` names) or a name listed in `glaeda.keepBranch`;
//! - its work is finished: a merged pull request from a branch of that name contains the tip
//!   (asked of GitHub through `gh`, for every GitHub remote), or, by the test worktrees use, the
//!   tip is in a remote default branch or every file it changed is already identical there;
//! - its newest activity (branch reflog entry or tip commit time) is older than the finished window.
//!
//! Deletion is compare-and-swap on the observed tip, so a branch that moved since it was checked
//! is kept. The receipt names each deleted branch and its tip; `git branch <name> <commit>`
//! recreates it while Git still has the objects, and the finished test means the content is
//! already on a default branch regardless.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use super::{
    LinkedWorktreeInventory, LinkedWorktreeReclaimError, WorkState, git, git_bounded,
    invalid_output, newest_reflog_entry_seconds, observe_work_state,
};
use crate::process::{CommandSpec, TimedCommandExecutor};
use crate::project_checkout_observation::ProjectCheckoutObserver;

/// Most local branches one repository listing may hold. Larger listings fail closed.
pub const MAX_LOCAL_BRANCHES: usize = 4_096;

const BRANCH_PREFIX: &str = "refs/heads/";
const KEEP_CONFIG: &str = "glaeda.keepBranch";
/// Names kept even when no remote HEAD is recorded.
const ALWAYS_PROTECTED: [&str; 2] = ["main", "master"];
const LISTING_BYTES: usize = 4 * 1024 * 1024;
/// Branch names asked about in one GraphQL request.
const GITHUB_BATCH: usize = 50;
/// Most branch names one repository run asks GitHub about.
const MAX_GITHUB_LOOKUPS: usize = 1_000;
const GH_TIMEOUT: Duration = Duration::from_secs(60);

/// How to ask GitHub whether a branch's pull request merged. `None` in the run means git-only.
#[derive(Debug, Clone)]
pub struct GithubLookup {
    pub gh_program: PathBuf,
    /// Passed through so `gh` finds its configuration and stored credentials.
    pub environment: Vec<(String, String)>,
}

/// Why a branch counts as finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "evidence", rename_all = "snake_case")]
pub enum LocalBranchFinishedEvidence {
    /// The tip is in a remote default branch, or every file it changed is identical there.
    DefaultBranch,
    /// A merged pull request's head contains the tip.
    MergedPullRequest { repository: String, number: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalBranchKeepReason {
    CheckedOut,
    Protected,
    RecentlyActive,
    InProgress,
    Unobservable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum LocalBranchDeletion {
    /// Deleted; `git branch <name> <commit>` recreates it.
    Deleted,
    /// The fresh check found the branch moved or checked out; nothing was changed.
    Changed,
    /// Git refused the compare-and-swap delete; nothing was changed.
    GitRefused,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum LocalBranchDecision {
    Eligible {
        idle_seconds: i64,
        #[serde(flatten)]
        finished: LocalBranchFinishedEvidence,
    },
    Kept {
        reason: LocalBranchKeepReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalBranchReport {
    pub name: String,
    pub commit: String,
    #[serde(flatten)]
    pub decision: LocalBranchDecision,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub deletion: Option<LocalBranchDeletion>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalBranch {
    name: String,
    commit: String,
    checked_out: bool,
    committed_seconds: i64,
}

/// Plan, and with `apply` carry out, deletion of finished local branches in one repository.
///
/// `budget` caps deletions and is decremented for every attempt. Only eligible branches are
/// reported unless `all` is set, so a repository with hundreds of live branches stays readable.
///
/// # Errors
///
/// Returns an error when the branch listing, the protected names, or the common Git directory
/// cannot be read; nothing is deleted then.
#[allow(clippy::too_many_arguments)]
pub fn reclaim_local_branches(
    observer: &ProjectCheckoutObserver,
    inventory: &LinkedWorktreeInventory,
    finished_idle_seconds: i64,
    now_seconds: i64,
    apply: bool,
    budget: &mut usize,
    all: bool,
    github: Option<&GithubLookup>,
    executor: &impl TimedCommandExecutor,
) -> Result<Vec<LocalBranchReport>, LinkedWorktreeReclaimError> {
    let repository = inventory.repository.as_path();
    let branches = list_local_branches(observer, repository, executor)?;
    let protected = protected_names(observer, repository, executor)?;
    let common_dir = inventory.common_dir.as_path();

    let screened: Vec<Result<i64, LocalBranchKeepReason>> = branches
        .iter()
        .map(|branch| {
            screen(
                common_dir,
                branch,
                &protected,
                finished_idle_seconds,
                now_seconds,
            )
        })
        .collect();
    let candidates: Vec<&LocalBranch> = branches
        .iter()
        .zip(&screened)
        .filter(|(_, screen)| screen.is_ok())
        .map(|(branch, _)| branch)
        .take(MAX_GITHUB_LOOKUPS)
        .collect();
    let merged = github.map_or_else(BTreeMap::new, |github| {
        merged_pull_requests(observer, repository, github, &candidates, executor)
    });

    let mut reports = Vec::new();
    for (branch, screen) in branches.into_iter().zip(screened) {
        let decision = match screen {
            Err(reason) => LocalBranchDecision::Kept { reason },
            Ok(idle_seconds) => {
                let finished = match merged.get(&branch.name) {
                    Some(evidence) => Some(evidence.clone()),
                    None => {
                        (observe_work_state(observer, repository, &branch.commit, true, executor)
                            == WorkState::Finished)
                            .then_some(LocalBranchFinishedEvidence::DefaultBranch)
                    }
                };
                // A branch has no per-worktree reflog to tell authored work from a sync, and it
                // is not checked out, so nobody is working on it: landed content is finished.
                finished.map_or(
                    LocalBranchDecision::Kept {
                        reason: LocalBranchKeepReason::InProgress,
                    },
                    |finished| LocalBranchDecision::Eligible {
                        idle_seconds,
                        finished,
                    },
                )
            }
        };
        let eligible = matches!(decision, LocalBranchDecision::Eligible { .. });
        let deletion = if eligible && apply && *budget > 0 {
            *budget -= 1;
            Some(delete(observer, repository, &branch, executor))
        } else {
            None
        };
        if eligible || all {
            reports.push(LocalBranchReport {
                name: branch.name,
                commit: branch.commit,
                decision,
                deletion,
            });
        }
    }
    Ok(reports)
}

/// Cheap local checks; `Ok` carries the idle time of a branch that may be finished.
fn screen(
    common_dir: &Path,
    branch: &LocalBranch,
    protected: &[String],
    finished_idle_seconds: i64,
    now_seconds: i64,
) -> Result<i64, LocalBranchKeepReason> {
    if branch.checked_out {
        return Err(LocalBranchKeepReason::CheckedOut);
    }
    if protected.contains(&branch.name) {
        return Err(LocalBranchKeepReason::Protected);
    }
    let reflog = common_dir
        .join("logs")
        .join(BRANCH_PREFIX)
        .join(&branch.name);
    let reflog_seconds =
        newest_reflog_entry_seconds(&reflog).map_err(|_| LocalBranchKeepReason::Unobservable)?;
    let active = reflog_seconds.map_or(branch.committed_seconds, |seconds| {
        seconds.max(branch.committed_seconds)
    });
    let idle_seconds = now_seconds.saturating_sub(active);
    if idle_seconds < finished_idle_seconds {
        return Err(LocalBranchKeepReason::RecentlyActive);
    }
    Ok(idle_seconds)
}

/// Branches whose tip a merged pull request's head contains, keyed by branch name.
///
/// Every failure (no `gh`, not signed in, rate limited, a remote that is not GitHub) only leaves
/// branches to the git-only test.
fn merged_pull_requests(
    observer: &ProjectCheckoutObserver,
    repository: &Path,
    github: &GithubLookup,
    candidates: &[&LocalBranch],
    executor: &impl TimedCommandExecutor,
) -> BTreeMap<String, LocalBranchFinishedEvidence> {
    let mut found = BTreeMap::new();
    for slug in github_repositories(observer, repository, executor) {
        let pending: Vec<&LocalBranch> = candidates
            .iter()
            .copied()
            .filter(|branch| !found.contains_key(&branch.name))
            .collect();
        for batch in pending.chunks(GITHUB_BATCH) {
            let Some(heads) = merged_heads(github, &slug, batch, executor) else {
                break;
            };
            for (branch, pulls) in batch.iter().zip(heads) {
                if let Some(number) = pulls.into_iter().find_map(|(number, head)| {
                    tip_contained(
                        observer,
                        repository,
                        github,
                        &slug,
                        &branch.commit,
                        &head,
                        executor,
                    )
                    .then_some(number)
                }) {
                    found.insert(
                        branch.name.clone(),
                        LocalBranchFinishedEvidence::MergedPullRequest {
                            repository: slug.clone(),
                            number,
                        },
                    );
                }
            }
        }
    }
    found
}

/// `owner/name` of every distinct GitHub remote, in `git config` order.
fn github_repositories(
    observer: &ProjectCheckoutObserver,
    repository: &Path,
    executor: &impl TimedCommandExecutor,
) -> Vec<String> {
    let Ok(record) = git(
        observer,
        repository,
        &["config", "--get-regexp", r"^remote\..*\.url$"],
        executor,
    ) else {
        return Vec::new();
    };
    let mut slugs = Vec::new();
    for url in record
        .stdout
        .lines()
        .filter_map(|line| line.split_once(' '))
        .map(|(_, url)| url)
    {
        if let Some(slug) = github_slug(url)
            && !slugs.contains(&slug)
        {
            slugs.push(slug);
        }
    }
    slugs
}

fn github_slug(url: &str) -> Option<String> {
    let rest = [
        "git@github.com:",
        "ssh://git@github.com/",
        "https://github.com/",
    ]
    .iter()
    .find_map(|prefix| url.strip_prefix(prefix))?;
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let (owner, name) = rest.split_once('/')?;
    let valid = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
    };
    (valid(owner) && valid(name)).then(|| format!("{owner}/{name}"))
}

fn gh(github: &GithubLookup, arguments: &[&str]) -> CommandSpec {
    let mut spec = CommandSpec::new(&github.gh_program);
    for (key, value) in &github.environment {
        spec = spec.environment(key.clone(), value.clone());
    }
    spec = spec
        .environment("GH_PROMPT_DISABLED", "1")
        .environment("GH_NO_UPDATE_NOTIFIER", "1")
        .environment("NO_COLOR", "1");
    for argument in arguments {
        spec = spec.argument(*argument);
    }
    spec
}

/// For each branch in `batch`, the merged pull requests with that head branch name, as
/// `(number, head commit)`. `None` when GitHub could not be asked.
fn merged_heads(
    github: &GithubLookup,
    slug: &str,
    batch: &[&LocalBranch],
    executor: &impl TimedCommandExecutor,
) -> Option<Vec<Vec<(u64, String)>>> {
    let (owner, name) = slug.split_once('/')?;
    let mut declarations = vec!["$owner:String!".to_owned(), "$name:String!".to_owned()];
    let mut fields = String::new();
    for index in 0..batch.len() {
        declarations.push(format!("$b{index}:String!"));
        fields.push_str(&format!(
            "b{index}:pullRequests(headRefName:$b{index},states:MERGED,first:10){{nodes{{number headRefOid}}}} "
        ));
    }
    let query = format!(
        "query({}){{repository(owner:$owner,name:$name){{{fields}}}}}",
        declarations.join(",")
    );
    let owner_field = format!("owner={owner}");
    let name_field = format!("name={name}");
    let query_field = format!("query={query}");
    let branch_fields: Vec<String> = batch
        .iter()
        .enumerate()
        .map(|(index, branch)| format!("b{index}={}", branch.name))
        .collect();
    let mut arguments = vec![
        "api",
        "graphql",
        "-f",
        &owner_field,
        "-f",
        &name_field,
        "-f",
        &query_field,
    ];
    for field in &branch_fields {
        arguments.extend(["-f", field.as_str()]);
    }
    let record = executor
        .execute_with_timeout(&gh(github, &arguments), GH_TIMEOUT)
        .ok()?;
    if record.status != Some(0) {
        return None;
    }
    let document: serde_json::Value = serde_json::from_str(&record.stdout).ok()?;
    let repository = document.get("data")?.get("repository")?;
    (0..batch.len())
        .map(|index| {
            let nodes = repository
                .get(format!("b{index}"))?
                .get("nodes")?
                .as_array()?;
            Some(
                nodes
                    .iter()
                    .filter_map(|node| {
                        let number = node.get("number")?.as_u64()?;
                        let head = node.get("headRefOid")?.as_str()?;
                        crate::artifact::CommitId::parse(head).ok()?;
                        Some((number, head.to_owned()))
                    })
                    .collect(),
            )
        })
        .collect()
}

/// Whether `head` contains `tip`: equal, locally an ancestor when Git has `head`, or else as
/// GitHub's compare reports it.
fn tip_contained(
    observer: &ProjectCheckoutObserver,
    repository: &Path,
    github: &GithubLookup,
    slug: &str,
    tip: &str,
    head: &str,
    executor: &impl TimedCommandExecutor,
) -> bool {
    if tip == head {
        return true;
    }
    let present = format!("{head}^{{commit}}");
    if git(
        observer,
        repository,
        &["cat-file", "-e", &present],
        executor,
    )
    .is_ok()
    {
        return observer
            .git(
                repository,
                &["merge-base", "--is-ancestor", tip, head],
                executor,
            )
            .is_ok_and(|record| record.status == Some(0));
    }
    let path = format!("repos/{slug}/compare/{tip}...{head}");
    executor
        .execute_with_timeout(&gh(github, &["api", &path, "--jq", ".status"]), GH_TIMEOUT)
        .is_ok_and(|record| {
            record.status == Some(0) && matches!(record.stdout.trim(), "ahead" | "identical")
        })
}

fn delete(
    observer: &ProjectCheckoutObserver,
    repository: &Path,
    branch: &LocalBranch,
    executor: &impl TimedCommandExecutor,
) -> LocalBranchDeletion {
    let reference = format!("{BRANCH_PREFIX}{}", branch.name);
    // Re-check right before acting: still at the observed tip and still not checked out.
    let fresh = git(
        observer,
        repository,
        &[
            "for-each-ref",
            "--format=%(objectname)%00%(worktreepath)",
            &reference,
        ],
        executor,
    );
    let expected = format!("{}\0\n", branch.commit);
    match fresh {
        Ok(record) if record.stdout == expected => {}
        _ => return LocalBranchDeletion::Changed,
    }
    // Compare-and-swap: Git deletes only if the ref still points at the observed commit.
    if git(
        observer,
        repository,
        &["update-ref", "-d", &reference, &branch.commit],
        executor,
    )
    .is_err()
    {
        return LocalBranchDeletion::GitRefused;
    }
    // `git branch -D` also drops the branch's config section; absent sections fail harmlessly.
    let section = format!("branch.{}", branch.name);
    let _ = observer.git(
        repository,
        &["config", "--remove-section", &section],
        executor,
    );
    LocalBranchDeletion::Deleted
}

fn list_local_branches(
    observer: &ProjectCheckoutObserver,
    repository: &Path,
    executor: &impl TimedCommandExecutor,
) -> Result<Vec<LocalBranch>, LinkedWorktreeReclaimError> {
    let record = git_bounded(
        observer,
        repository,
        &[
            "for-each-ref",
            "--format=%(refname)%00%(objectname)%00%(worktreepath)%00%(committerdate:unix)",
            BRANCH_PREFIX,
        ],
        LISTING_BYTES,
        executor,
    )?;
    parse_local_branches(&record.stdout)
}

fn parse_local_branches(stdout: &str) -> Result<Vec<LocalBranch>, LinkedWorktreeReclaimError> {
    let mut branches = Vec::new();
    for line in stdout.lines() {
        let fields: Vec<&str> = line.split('\0').collect();
        let [reference, commit, worktree, committed] = fields[..] else {
            return Err(invalid_output());
        };
        let name = reference
            .strip_prefix(BRANCH_PREFIX)
            .filter(|name| !name.is_empty())
            .ok_or_else(invalid_output)?;
        crate::artifact::CommitId::parse(commit).map_err(|_| invalid_output())?;
        let committed_seconds = committed.parse().map_err(|_| invalid_output())?;
        branches.push(LocalBranch {
            name: name.to_owned(),
            commit: commit.to_owned(),
            checked_out: !worktree.is_empty(),
            committed_seconds,
        });
        if branches.len() > MAX_LOCAL_BRANCHES {
            return Err(invalid_output());
        }
    }
    Ok(branches)
}

fn protected_names(
    observer: &ProjectCheckoutObserver,
    repository: &Path,
    executor: &impl TimedCommandExecutor,
) -> Result<Vec<String>, LinkedWorktreeReclaimError> {
    let mut names: Vec<String> = ALWAYS_PROTECTED
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    let heads = git(
        observer,
        repository,
        &[
            "for-each-ref",
            "--format=%(symref:lstrip=3)",
            "refs/remotes/*/HEAD",
        ],
        executor,
    )?;
    names.extend(
        heads
            .stdout
            .lines()
            .filter(|name| !name.is_empty())
            .map(str::to_owned),
    );
    // `git config --get-all` exits 1 when the key is unset; only its stdout matters.
    let keep = observer
        .git(repository, &["config", "--get-all", KEEP_CONFIG], executor)
        .map_err(|_| super::unavailable())?;
    match keep.status {
        Some(0) => names.extend(keep.stdout.lines().map(str::to_owned)),
        Some(1) => {}
        _ => return Err(super::unavailable()),
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn listing_marks_checked_out_branches() {
        let stdout = format!(
            "refs/heads/done\0{COMMIT}\0\x001700000000\nrefs/heads/live\0{COMMIT}\0/w/live\x001700000001\n"
        );
        let branches = parse_local_branches(&stdout).expect("parse");
        assert_eq!(branches.len(), 2);
        assert!(!branches[0].checked_out);
        assert_eq!(branches[0].name, "done");
        assert!(branches[1].checked_out);
        assert_eq!(branches[1].committed_seconds, 1_700_000_001);
    }

    #[test]
    fn github_remotes_parse_to_slugs() {
        for url in [
            "git@github.com:manaflow-ai/cmux.git",
            "https://github.com/manaflow-ai/cmux",
            "ssh://git@github.com/manaflow-ai/cmux.git",
        ] {
            assert_eq!(
                github_slug(url).as_deref(),
                Some("manaflow-ai/cmux"),
                "{url}"
            );
        }
        assert_eq!(github_slug("git@gitlab.com:a/b.git"), None);
        assert_eq!(github_slug("https://github.com/a/b/c"), None);
        assert_eq!(github_slug("/srv/git/cmux.git"), None);
    }

    #[test]
    fn malformed_listing_fails_closed() {
        assert!(parse_local_branches("refs/heads/x\0abc\0\x001\n").is_err());
        assert!(parse_local_branches(&format!("refs/tags/x\0{COMMIT}\0\x001\n")).is_err());
        assert!(parse_local_branches(&format!("refs/heads/x\0{COMMIT}\0\n")).is_err());
    }
}
