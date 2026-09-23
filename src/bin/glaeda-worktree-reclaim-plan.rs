#[cfg(not(unix))]
compile_error!("glaeda-worktree-reclaim-plan requires a Unix host");

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, ValueEnum};
use glaeda::linked_worktree_reclaim::{
    LinkedWorktreeFacts, LinkedWorktreeReclaimCompensation, LinkedWorktreeReclaimDecision,
    LinkedWorktreeReclaimPolicy, LinkedWorktreeReclaimVeto, list_linked_worktrees,
    observe_linked_worktree, plan_linked_worktree_reclaim,
};
use glaeda::process::ProcessExecutor;
use glaeda::project_checkout_observation::ProjectCheckoutObserver;
use serde::Serialize;

const REPORT_SCHEMA_VERSION: u8 = 1;
const GIT_PROGRAM: &str = "/usr/bin/git";

/// Default idle window: a day.
///
/// The Cargo target planner defaults to a week because a wrong reclaim costs a rebuild. Here every
/// eligible worktree is already clean and preserved, so recreating one costs a single
/// `git worktree add`. Agent scratch worktrees churn daily, and a week would leave most of them
/// out of reach.
const DEFAULT_IDLE_SECONDS: i64 = 24 * 60 * 60;

// Nothing is removed. The compensation an executor would owe is recorded per decision: none when a
// shared ref already reaches HEAD, or a HEAD pin before removal.
const MUTATION_PERFORMED: bool = false;

#[derive(Debug, Parser)]
#[command(
    name = "glaeda-worktree-reclaim-plan",
    about = "Plan which linked Git worktrees of one repository are safe to reclaim, without mutation"
)]
struct Cli {
    /// Explicit canonical absolute path of the repository's main worktree.
    #[arg(long)]
    repository: PathBuf,

    /// Seconds without Git activity before a worktree may be reclaimed.
    #[arg(long, default_value_t = DEFAULT_IDLE_SECONDS)]
    minimum_idle_seconds: i64,

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
}

#[derive(Debug, Serialize)]
struct PlanReport {
    document_type: &'static str,
    schema_version: u8,
    mutation_performed: bool,
    minimum_idle_seconds: i64,
    summary: Summary,
    worktrees: Vec<WorktreeReport>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
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
    let inventory = match list_linked_worktrees(&observer, &cli.repository, &executor) {
        Ok(inventory) => inventory,
        Err(error) => return fail(cli.output, error.code(), error.problem()),
    };

    let mut summary = Summary {
        linked: inventory.linked().len(),
        ..Summary::default()
    };
    let mut worktrees = Vec::with_capacity(inventory.linked().len());
    for (index, entry) in inventory.linked().iter().enumerate() {
        let result = if entry.prunable() {
            summary.prunable += 1;
            WorktreeResult::Prunable
        } else {
            match observe_linked_worktree(&observer, &inventory, entry, &executor).and_then(
                |facts| {
                    plan_linked_worktree_reclaim(&facts, policy, now_seconds)
                        .map(|plan| (facts, plan))
                },
            ) {
                Ok((facts, plan)) => {
                    let decision = plan.decision().clone();
                    match &decision {
                        LinkedWorktreeReclaimDecision::Eligible { compensation, .. } => {
                            summary.eligible += 1;
                            if *compensation == LinkedWorktreeReclaimCompensation::PinHeadCommit {
                                summary.eligible_requiring_head_pin += 1;
                            }
                        }
                        LinkedWorktreeReclaimDecision::Refused { .. } => summary.refused += 1,
                    }
                    WorktreeResult::Planned { facts, decision }
                }
                Err(error) => {
                    summary.unobservable += 1;
                    WorktreeResult::Unobservable { code: error.code() }
                }
            }
        };
        worktrees.push(WorktreeReport {
            ordinal: index + 1,
            result,
        });
    }

    let report = PlanReport {
        document_type: "glaeda-worktree-reclaim-plan",
        schema_version: REPORT_SCHEMA_VERSION,
        mutation_performed: MUTATION_PERFORMED,
        minimum_idle_seconds: policy.minimum_idle_seconds(),
        summary,
        worktrees,
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
        OutputFormat::Human => render_human(&report),
    }
    ExitCode::SUCCESS
}

fn render_human(report: &PlanReport) {
    let summary = &report.summary;
    println!(
        "{} linked worktrees: {} eligible ({} need a HEAD pin), {} refused, {} prunable, {} unobservable",
        summary.linked,
        summary.eligible,
        summary.eligible_requiring_head_pin,
        summary.refused,
        summary.prunable,
        summary.unobservable,
    );
    for worktree in &report.worktrees {
        let line = match &worktree.result {
            WorktreeResult::Planned {
                decision:
                    LinkedWorktreeReclaimDecision::Eligible {
                        compensation,
                        idle_seconds,
                        ..
                    },
                ..
            } => format!(
                "eligible, idle {}h{}",
                idle_seconds / 3_600,
                match compensation {
                    LinkedWorktreeReclaimCompensation::NoneRequired => "",
                    LinkedWorktreeReclaimCompensation::PinHeadCommit => ", pin HEAD first",
                }
            ),
            WorktreeResult::Planned {
                decision: LinkedWorktreeReclaimDecision::Refused { vetoes },
                ..
            } => format!("refused: {}", join_vetoes(vetoes)),
            WorktreeResult::Prunable => {
                "prunable: registration is stale (directory or .git file missing)".to_owned()
            }
            WorktreeResult::Unobservable { code } => format!("unobservable: {code}"),
        };
        println!("  #{}: {line}", worktree.ordinal);
    }
    println!("no worktree was changed; #N is the Nth linked entry of `git worktree list`");
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
            "{{\"document_type\":\"glaeda-worktree-reclaim-plan-error\",\"code\":\"{code}\",\"problem\":\"{problem}\"}}"
        ),
        OutputFormat::Human => eprintln!("{code}: {problem}"),
    }
    ExitCode::from(2)
}
