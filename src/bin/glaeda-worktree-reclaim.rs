#[cfg(not(unix))]
compile_error!("glaeda-worktree-reclaim requires a Unix host");

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, ValueEnum};
use glaeda::linked_worktree_reclaim::{
    LinkedWorktreeFacts, LinkedWorktreeReclaimCompensation, LinkedWorktreeReclaimDecision,
    LinkedWorktreeReclaimOutcome, LinkedWorktreeReclaimPolicy, LinkedWorktreeReclaimVeto,
    list_linked_worktrees, observe_linked_worktrees, plan_linked_worktree_reclaim,
    reclaim_linked_worktree,
};
use glaeda::process::ProcessExecutor;
use glaeda::project_checkout_observation::ProjectCheckoutObserver;
use serde::Serialize;

const REPORT_SCHEMA_VERSION: u8 = 1;
const GIT_PROGRAM: &str = "/usr/bin/git";
const MAX_REPOSITORIES: usize = 32;

/// Default idle window: a day.
///
/// The Cargo target planner defaults to a week because a wrong reclaim costs a rebuild. Here every
/// eligible worktree is already clean and preserved, so recreating one costs a single
/// `git worktree add`. Agent scratch worktrees churn daily, and a week would leave most of them
/// out of reach.
const DEFAULT_IDLE_SECONDS: i64 = 24 * 60 * 60;

/// Default number of worktrees one run may remove. A budget keeps an unattended run bounded even
/// if every observation is wrong in the same way.
const DEFAULT_MAX_RECLAIMS: usize = 32;

// Ignored files are gone for good; everything else comes back with `git worktree add` at the
// preserved commit, and a detached HEAD is pinned under refs/glaeda/worktree-pins/ first.
const COMPENSATION: &str = "git_worktree_add_at_preserved_commit";

#[derive(Debug, Parser)]
#[command(
    name = "glaeda-worktree-reclaim",
    about = "Find linked Git worktrees whose removal loses nothing, and optionally remove them"
)]
struct Cli {
    /// Canonical absolute path of a repository's main worktree. Repeat for several repositories.
    #[arg(long = "repository", required = true)]
    repositories: Vec<PathBuf>,

    /// Remove eligible worktrees. Without it this only reports and changes nothing.
    #[arg(long)]
    apply: bool,

    /// Most worktrees one run may remove across all repositories.
    #[arg(long, default_value_t = DEFAULT_MAX_RECLAIMS)]
    max_reclaims: usize,

    /// Seconds without Git activity before a worktree may be reclaimed.
    #[arg(long, default_value_t = DEFAULT_IDLE_SECONDS)]
    minimum_idle_seconds: i64,

    /// List every worktree in human output, not only the actionable ones.
    #[arg(long)]
    all: bool,

    /// Select human or JSON output.
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    output: OutputFormat,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputFormat {
    Human,
    Json,
}

#[derive(Debug, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
enum WorktreeResult {
    Planned {
        facts: LinkedWorktreeFacts,
        decision: LinkedWorktreeReclaimDecision,
        #[serde(skip_serializing_if = "Option::is_none")]
        reclaim: Option<LinkedWorktreeReclaimOutcome>,
    },
    /// The registration is stale; see `LinkedWorktreeEntry::prunable`. Nothing here recommends
    /// pruning, which deletes per-worktree refs and logs.
    Prunable,
    Unobservable {
        code: &'static str,
    },
}

#[derive(Debug, Serialize)]
struct WorktreeReport {
    /// Position among linked worktrees in `git worktree list` order, starting at 1.
    ordinal: usize,
    #[serde(flatten)]
    result: WorktreeResult,
}

#[derive(Debug, Default, Serialize)]
struct Summary {
    linked: usize,
    eligible: usize,
    eligible_requiring_head_pin: usize,
    refused: usize,
    prunable: usize,
    unobservable: usize,
    reclaimed: usize,
}

#[derive(Debug, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
enum RepositoryResult {
    Listed {
        summary: Summary,
        worktrees: Vec<WorktreeReport>,
    },
    Unlisted {
        code: &'static str,
    },
}

#[derive(Debug, Serialize)]
struct RepositoryReport {
    /// Position of the repository among the `--repository` arguments, starting at 1.
    ordinal: usize,
    #[serde(flatten)]
    result: RepositoryResult,
}

#[derive(Debug, Serialize)]
struct Report {
    document_type: &'static str,
    schema_version: u8,
    mutation_performed: bool,
    compensation: &'static str,
    minimum_idle_seconds: i64,
    max_reclaims: usize,
    budget_exhausted: bool,
    circuit_breaker_tripped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    available_bytes_before: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    available_bytes_after: Option<u64>,
    repositories: Vec<RepositoryReport>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if cli.repositories.len() > MAX_REPOSITORIES {
        return fail(
            cli.output,
            "too_many_repositories",
            "one run accepts at most 32 repositories",
        );
    }
    let policy = match LinkedWorktreeReclaimPolicy::new(cli.minimum_idle_seconds) {
        Ok(policy) => policy,
        Err(error) => return fail(cli.output, error.code(), error.problem()),
    };
    let Some(now_seconds) = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|now| i64::try_from(now.as_secs()).ok())
    else {
        return fail(
            cli.output,
            "clock_unavailable",
            "the system clock is unusable",
        );
    };
    let observer = match ProjectCheckoutObserver::new(GIT_PROGRAM) {
        Ok(observer) => observer,
        Err(error) => return fail(cli.output, error.code, error.problem),
    };
    let executor = ProcessExecutor;

    let available_bytes_before = cli
        .apply
        .then(|| available_bytes(&cli.repositories[0]))
        .flatten();
    let mut remaining_budget = cli.max_reclaims;
    let mut budget_exhausted = false;
    let mut circuit_breaker_tripped = false;
    let mut mutation_performed = false;
    let mut repositories = Vec::with_capacity(cli.repositories.len());

    for (index, repository) in cli.repositories.iter().enumerate() {
        let inventory = match list_linked_worktrees(&observer, repository, &executor) {
            Ok(inventory) => inventory,
            Err(error) => {
                repositories.push(RepositoryReport {
                    ordinal: index + 1,
                    result: RepositoryResult::Unlisted { code: error.code() },
                });
                continue;
            }
        };
        let observations = observe_linked_worktrees(&observer, &inventory, &executor);
        let mut summary = Summary {
            linked: inventory.linked().len(),
            ..Summary::default()
        };
        let mut worktrees = Vec::with_capacity(inventory.linked().len());
        for (position, (entry, observation)) in
            inventory.linked().iter().zip(observations).enumerate()
        {
            let result = if entry.prunable() {
                summary.prunable += 1;
                WorktreeResult::Prunable
            } else {
                match observation.and_then(|facts| {
                    plan_linked_worktree_reclaim(&facts, policy, now_seconds)
                        .map(|plan| (facts, plan.decision().clone()))
                }) {
                    Ok((facts, decision)) => {
                        let mut reclaim = None;
                        match &decision {
                            LinkedWorktreeReclaimDecision::Eligible { compensation, .. } => {
                                summary.eligible += 1;
                                if *compensation == LinkedWorktreeReclaimCompensation::PinHeadCommit
                                {
                                    summary.eligible_requiring_head_pin += 1;
                                }
                                if cli.apply && !circuit_breaker_tripped {
                                    if remaining_budget == 0 {
                                        budget_exhausted = true;
                                    } else {
                                        remaining_budget -= 1;
                                        mutation_performed = true;
                                        let outcome = reclaim_linked_worktree(
                                            &observer,
                                            &inventory,
                                            entry,
                                            policy,
                                            now_seconds,
                                            &executor,
                                        );
                                        if matches!(
                                            outcome,
                                            LinkedWorktreeReclaimOutcome::Reclaimed { .. }
                                        ) {
                                            summary.reclaimed += 1;
                                        }
                                        circuit_breaker_tripped |= !outcome.batch_may_continue();
                                        reclaim = Some(outcome);
                                    }
                                }
                            }
                            LinkedWorktreeReclaimDecision::Refused { .. } => summary.refused += 1,
                        }
                        WorktreeResult::Planned {
                            facts,
                            decision,
                            reclaim,
                        }
                    }
                    Err(error) => {
                        summary.unobservable += 1;
                        WorktreeResult::Unobservable { code: error.code() }
                    }
                }
            };
            worktrees.push(WorktreeReport {
                ordinal: position + 1,
                result,
            });
        }
        repositories.push(RepositoryReport {
            ordinal: index + 1,
            result: RepositoryResult::Listed { summary, worktrees },
        });
    }

    let report = Report {
        document_type: if cli.apply {
            "glaeda-worktree-reclaim-receipt"
        } else {
            "glaeda-worktree-reclaim-plan"
        },
        schema_version: REPORT_SCHEMA_VERSION,
        mutation_performed,
        compensation: COMPENSATION,
        minimum_idle_seconds: policy.minimum_idle_seconds(),
        max_reclaims: cli.max_reclaims,
        budget_exhausted,
        circuit_breaker_tripped,
        available_bytes_before,
        available_bytes_after: cli
            .apply
            .then(|| available_bytes(&cli.repositories[0]))
            .flatten(),
        repositories,
    };
    match cli.output {
        OutputFormat::Json => match serde_json::to_string_pretty(&report) {
            Ok(text) => println!("{text}"),
            Err(_) => {
                return fail(
                    cli.output,
                    "serialization_failed",
                    "report could not be serialized",
                );
            }
        },
        OutputFormat::Human => render_human(&report, cli.apply, cli.all),
    }
    if circuit_breaker_tripped {
        ExitCode::from(3)
    } else {
        ExitCode::SUCCESS
    }
}

fn available_bytes(path: &Path) -> Option<u64> {
    let stat = rustix::fs::statvfs(path).ok()?;
    stat.f_bavail.checked_mul(stat.f_frsize)
}

fn render_human(report: &Report, apply: bool, all: bool) {
    let mut eligible_total = 0;
    for repository in &report.repositories {
        match &repository.result {
            RepositoryResult::Unlisted { code } => {
                println!("repository {}: not listed: {code}", repository.ordinal);
            }
            RepositoryResult::Listed { summary, worktrees } => {
                eligible_total += summary.eligible;
                println!(
                    "repository {}: {} linked worktrees: {} eligible ({} need a HEAD pin), {} refused, {} prunable, {} unobservable{}",
                    repository.ordinal,
                    summary.linked,
                    summary.eligible,
                    summary.eligible_requiring_head_pin,
                    summary.refused,
                    summary.prunable,
                    summary.unobservable,
                    if apply {
                        format!(", {} reclaimed", summary.reclaimed)
                    } else {
                        String::new()
                    },
                );
                for worktree in worktrees {
                    if let Some(line) = worktree_line(&worktree.result, all) {
                        println!("  #{}: {line}", worktree.ordinal);
                    }
                }
            }
        }
    }
    if apply {
        if let (Some(before), Some(after)) =
            (report.available_bytes_before, report.available_bytes_after)
        {
            println!(
                "free space: {:.1} GiB -> {:.1} GiB",
                gib(before),
                gib(after)
            );
        }
        if report.budget_exhausted {
            println!(
                "stopped at the --max-reclaims budget of {}; run again to continue",
                report.max_reclaims
            );
        }
        if report.circuit_breaker_tripped {
            println!(
                "stopped: a removal ended in an unexpected state; inspect it before rerunning"
            );
        }
    } else if eligible_total > 0 {
        println!("nothing was changed; run again with --apply to reclaim the eligible worktrees");
    } else {
        println!("nothing was changed");
    }
    if !all {
        println!("#N is the Nth linked entry of `git worktree list`; --all lists every worktree");
    }
}

fn worktree_line(result: &WorktreeResult, all: bool) -> Option<String> {
    match result {
        WorktreeResult::Planned {
            reclaim: Some(outcome),
            ..
        } => Some(match outcome {
            LinkedWorktreeReclaimOutcome::Reclaimed { pinned, .. } => format!(
                "reclaimed{}",
                if *pinned { " (HEAD pinned first)" } else { "" }
            ),
            LinkedWorktreeReclaimOutcome::Refused { vetoes } => {
                format!("kept: fresh check refused: {}", join_vetoes(vetoes))
            }
            LinkedWorktreeReclaimOutcome::Unobservable { code } => {
                format!("kept: fresh check failed: {code}")
            }
            LinkedWorktreeReclaimOutcome::GitRefused => {
                "kept: git worktree remove refused".to_owned()
            }
            LinkedWorktreeReclaimOutcome::Incomplete { code } => format!("INCOMPLETE: {code}"),
        }),
        WorktreeResult::Planned {
            decision:
                LinkedWorktreeReclaimDecision::Eligible {
                    compensation,
                    idle_seconds,
                    ..
                },
            ..
        } => Some(format!(
            "eligible, idle {}h{}",
            idle_seconds / 3_600,
            match compensation {
                LinkedWorktreeReclaimCompensation::NoneRequired => "",
                LinkedWorktreeReclaimCompensation::PinHeadCommit => ", pin HEAD first",
            }
        )),
        WorktreeResult::Planned {
            decision: LinkedWorktreeReclaimDecision::Refused { vetoes },
            ..
        } => all.then(|| format!("refused: {}", join_vetoes(vetoes))),
        WorktreeResult::Prunable => all
            .then(|| "prunable: registration is stale (directory or .git file missing)".to_owned()),
        WorktreeResult::Unobservable { code } => Some(format!("unobservable: {code}")),
    }
}

#[allow(clippy::cast_precision_loss)]
fn gib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0 * 1024.0)
}

fn join_vetoes(vetoes: &[LinkedWorktreeReclaimVeto]) -> String {
    vetoes
        .iter()
        .map(|veto| veto.code())
        .collect::<Vec<_>>()
        .join(", ")
}

fn fail(output: OutputFormat, code: &str, problem: &str) -> ExitCode {
    match output {
        OutputFormat::Json => println!(
            "{}",
            serde_json::json!({
                "document_type": "glaeda-worktree-reclaim-error",
                "code": code,
                "problem": problem,
            })
        ),
        OutputFormat::Human => eprintln!("{code}: {problem}"),
    }
    ExitCode::from(2)
}
