use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
#[cfg(target_os = "linux")]
use rustix::rand::{GetRandomFlags, getrandom};
use serde::Serialize;
use smolrunner::doctor::{inspect_host, render_human as render_doctor};
#[cfg(target_os = "linux")]
use smolrunner::durable_journal::StateStoreJournalCheckpoint;
#[cfg(target_os = "linux")]
use smolrunner::durable_lane_execution::SystemLaneCommandRunner;
#[cfg(target_os = "linux")]
use smolrunner::host_preparation_command::{
    HostPreparationCommandDecision, HostPreparationCommandDisposition, decide_host_preparation,
    render_human as render_host_prepare_decision,
};
#[cfg(target_os = "linux")]
use smolrunner::host_preparation_execution::{
    HostPreparationExecutionDisposition, HostPreparationExecutionError,
    execute_confirmed_host_preparation, render_human as render_host_prepare_execution,
};
#[cfg(target_os = "linux")]
use smolrunner::host_preparation_plan::plan_host_preparation;
#[cfg(target_os = "linux")]
use smolrunner::host_readiness::inspect_host_readiness;
#[cfg(target_os = "linux")]
use smolrunner::host_readiness_verdict::{assess, render_human as render_host_plan};
#[cfg(target_os = "linux")]
use smolrunner::journal::ExecutionLane;
#[cfg(target_os = "linux")]
use smolrunner::linux_installation_catalog::{InstallationLookup, find_default_installation};
#[cfg(target_os = "linux")]
use smolrunner::linux_state::LinuxStateRoot;
use smolrunner::manifest::{ManifestError, load};
#[cfg(target_os = "linux")]
use smolrunner::ownership::ProjectIdentity;
use smolrunner::plan::{build, render_human as render_plan};
#[cfg(target_os = "linux")]
use smolrunner::process::ProcessExecutor;
#[cfg(target_os = "linux")]
use smolrunner::state::JournalId;

#[derive(Debug, Parser)]
#[command(
    name = "smolrunner",
    version,
    about = "Tend a small fleet of self-hosted GitHub Actions runners"
)]
struct Cli {
    #[arg(long, global = true, value_enum, default_value_t = OutputFormat::Human)]
    output: OutputFormat,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputFormat {
    Human,
    Json,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Inspect whether the current host is ready for SmolRunner.
    Doctor {
        /// Treat warnings as a non-zero result.
        #[arg(long)]
        strict: bool,
    },
    /// Validate desired state and show the changes SmolRunner would eventually make.
    Plan {
        /// Manifest to validate and plan.
        #[arg(long, default_value = "smolrunner.yml")]
        file: PathBuf,
    },
    /// Inspect, plan, or explicitly prepare host-level state.
    Host {
        #[command(subcommand)]
        command: HostCommand,
    },
}

#[derive(Debug, Subcommand)]
enum HostCommand {
    /// Compare bounded host observations with a project manifest.
    Plan {
        /// Manifest to inspect against the current host.
        #[arg(long, default_value = "smolrunner.yml")]
        file: PathBuf,
        /// Explicit runner account policy. Defaults to MANIFEST.account.yml when present.
        #[arg(long)]
        account_file: Option<PathBuf>,
    },
    /// Execute one exactly confirmed root-lane host-preparation phase.
    Prepare {
        /// Manifest to inspect and prepare against the current host.
        #[arg(long, default_value = "smolrunner.yml")]
        file: PathBuf,
        /// Explicit runner account policy. Defaults to MANIFEST.account.yml when present.
        #[arg(long)]
        account_file: Option<PathBuf>,
        /// Exact deterministic confirmation emitted by an immediately preceding prepare proposal.
        #[arg(long)]
        confirm: Option<String>,
    },
}

#[derive(Debug, Serialize)]
struct ErrorReport<'a> {
    schema_version: u8,
    error: &'a ManifestError,
}

#[derive(Debug, Serialize)]
struct RuntimeErrorReport {
    schema_version: u8,
    kind: &'static str,
    message: String,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Command::Doctor { strict } => run_doctor(cli.output, strict),
        Command::Plan { file } => run_plan(cli.output, &file),
        Command::Host { command } => match command {
            HostCommand::Plan { file, account_file } => {
                run_host_plan(cli.output, &file, account_file.as_deref())
            }
            HostCommand::Prepare {
                file,
                account_file,
                confirm,
            } => run_host_prepare(
                cli.output,
                &file,
                account_file.as_deref(),
                confirm.as_deref(),
            ),
        },
    }
}

fn run_doctor(output: OutputFormat, strict: bool) -> ExitCode {
    let report = inspect_host();

    match output {
        OutputFormat::Human => print!("{}", render_doctor(&report)),
        OutputFormat::Json => {
            if print_json(&report).is_err() {
                return ExitCode::from(2);
            }
        }
    }

    if report.has_failures() || (strict && report.has_warnings()) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn run_plan(output: OutputFormat, file: &Path) -> ExitCode {
    let manifest = match load_manifest(output, file) {
        Ok(manifest) => manifest,
        Err(code) => return code,
    };
    let report = build(&manifest, file);

    match output {
        OutputFormat::Human => print!("{}", render_plan(&report)),
        OutputFormat::Json => {
            if print_json(&report).is_err() {
                return ExitCode::from(2);
            }
        }
    }

    ExitCode::SUCCESS
}

#[cfg(target_os = "linux")]
fn run_host_plan(output: OutputFormat, file: &Path, account_file: Option<&Path>) -> ExitCode {
    let manifest = match load_manifest(output, file) {
        Ok(manifest) => manifest,
        Err(code) => return code,
    };
    let report = match inspect_host_readiness(&manifest, file, account_file, &ProcessExecutor) {
        Ok(report) => report,
        Err(error) => {
            return emit_runtime_error(
                output,
                "host_readiness_probe",
                format!("failed to inspect host readiness: {error}"),
            );
        }
    };
    let assessment = assess(&report);

    match output {
        OutputFormat::Human => print!("{}", render_host_plan(&assessment)),
        OutputFormat::Json => {
            if print_json(&assessment).is_err() {
                return ExitCode::from(2);
            }
        }
    }

    ExitCode::SUCCESS
}

#[cfg(not(target_os = "linux"))]
fn run_host_plan(output: OutputFormat, file: &Path, _account_file: Option<&Path>) -> ExitCode {
    if let Err(code) = load_manifest(output, file) {
        return code;
    }
    emit_runtime_error(
        output,
        "host_readiness_probe",
        "host planning currently supports Linux only".to_owned(),
    )
}

#[cfg(target_os = "linux")]
fn run_host_prepare(
    output: OutputFormat,
    file: &Path,
    account_file: Option<&Path>,
    supplied_confirmation: Option<&str>,
) -> ExitCode {
    let manifest = match load_manifest(output, file) {
        Ok(manifest) => manifest,
        Err(code) => return code,
    };
    let readiness = match inspect_host_readiness(&manifest, file, account_file, &ProcessExecutor) {
        Ok(report) => report,
        Err(error) => {
            return emit_runtime_error(
                output,
                "host_readiness_probe",
                format!("failed to inspect host readiness: {error}"),
            );
        }
    };
    let proposal = plan_host_preparation(readiness);
    let decision = match decide_host_preparation(proposal, supplied_confirmation) {
        Ok(decision) => decision,
        Err(error) => {
            return emit_runtime_error(
                output,
                "host_preparation_decision",
                error.message().to_owned(),
            );
        }
    };

    match decision.disposition() {
        HostPreparationCommandDisposition::Ready => {
            return emit_host_prepare_decision(output, &decision, ExitCode::SUCCESS);
        }
        HostPreparationCommandDisposition::Blocked
        | HostPreparationCommandDisposition::ConfirmationRequired
        | HostPreparationCommandDisposition::ConfirmationMismatch => {
            return emit_host_prepare_decision(output, &decision, ExitCode::FAILURE);
        }
        HostPreparationCommandDisposition::Confirmed => {}
    }

    let confirmed_phase = decision
        .confirmed_phase()
        .expect("confirmed host-preparation decisions contain one executable phase");
    if confirmed_phase
        .actions
        .iter()
        .any(|action| action.lane != ExecutionLane::Root)
    {
        return emit_runtime_error(
            output,
            "runner_user_phase_unsupported",
            "this host-preparation slice executes root-lane phases only; re-observe after root preparation and use the reviewed runner-user evidence adapter once available"
                .to_owned(),
        );
    }

    if emit_pre_mutation_decision(output, &decision).is_err() {
        return ExitCode::from(2);
    }

    let project = ProjectIdentity::from(&manifest);
    let installation_id = match find_default_installation(&project) {
        Ok(InstallationLookup::Found(installation_id)) => installation_id,
        Ok(InstallationLookup::Missing) => {
            return emit_runtime_error(
                output,
                "installation_missing",
                "no enrolled SmolRunner installation matches the manifest project; host preparation does not bootstrap state or enroll projects"
                    .to_owned(),
            );
        }
        Err(error) => {
            return emit_runtime_error(
                output,
                "installation_lookup",
                format!(
                    "could not resolve the manifest project installation: {}",
                    error.message()
                ),
            );
        }
    };
    let journal_id = match generate_host_prepare_journal_id() {
        Ok(journal_id) => journal_id,
        Err(message) => return emit_runtime_error(output, "journal_id", message),
    };
    let mut store = match LinuxStateRoot::open_default() {
        Ok(store) => store,
        Err(error) => {
            return emit_runtime_error(
                output,
                "state_store",
                format!(
                    "could not open the reviewed state root: {}",
                    error.message()
                ),
            );
        }
    };
    let mut checkpoint = StateStoreJournalCheckpoint::new(&mut store, installation_id, journal_id);
    let mut runner = SystemLaneCommandRunner::root_only();
    let report = match execute_confirmed_host_preparation(decision, &mut runner, &mut checkpoint) {
        Ok(report) => report,
        Err(error) => return emit_host_prepare_execution_error(output, &error),
    };

    match output {
        OutputFormat::Human => print!("{}", render_host_prepare_execution(&report)),
        OutputFormat::Json => {
            if print_json(&report).is_err() {
                return ExitCode::from(2);
            }
        }
    }

    match report.disposition {
        HostPreparationExecutionDisposition::ActionFailed => ExitCode::FAILURE,
        HostPreparationExecutionDisposition::Completed
        | HostPreparationExecutionDisposition::FreshObservationRequired => ExitCode::SUCCESS,
    }
}

#[cfg(not(target_os = "linux"))]
fn run_host_prepare(
    output: OutputFormat,
    file: &Path,
    _account_file: Option<&Path>,
    _supplied_confirmation: Option<&str>,
) -> ExitCode {
    if let Err(code) = load_manifest(output, file) {
        return code;
    }
    emit_runtime_error(
        output,
        "host_preparation_execution",
        "host preparation currently supports Linux only".to_owned(),
    )
}

#[cfg(target_os = "linux")]
fn emit_host_prepare_decision(
    output: OutputFormat,
    decision: &HostPreparationCommandDecision,
    exit_code: ExitCode,
) -> ExitCode {
    match output {
        OutputFormat::Human => print!("{}", render_host_prepare_decision(decision)),
        OutputFormat::Json => {
            if print_json(decision).is_err() {
                return ExitCode::from(2);
            }
        }
    }
    exit_code
}

#[cfg(target_os = "linux")]
fn emit_host_prepare_execution_error(
    output: OutputFormat,
    error: &HostPreparationExecutionError,
) -> ExitCode {
    match output {
        OutputFormat::Human => {
            eprintln!("{}", error.message());
            if let Some(checkpoint) = error.checkpoint() {
                eprintln!("Checkpoint phase: {:?}", checkpoint.phase());
                if let Some(action_id) = checkpoint.action_id() {
                    eprintln!("Checkpoint action: {action_id}");
                }
                eprintln!("Checkpoint failure: {}", checkpoint.failure().message());
                eprintln!(
                    "Last durable snapshot present: {}",
                    checkpoint.last_durable().is_some()
                );
                eprintln!(
                    "Attempted snapshot records: {}",
                    checkpoint.attempted().records.len()
                );
            }
        }
        OutputFormat::Json => {
            if print_json(error).is_err() {
                return ExitCode::from(2);
            }
        }
    }
    ExitCode::from(2)
}

#[cfg(target_os = "linux")]
fn emit_pre_mutation_decision(
    output: OutputFormat,
    decision: &HostPreparationCommandDecision,
) -> Result<(), serde_json::Error> {
    match output {
        OutputFormat::Human => {
            eprint!("{}", render_host_prepare_decision(decision));
            Ok(())
        }
        OutputFormat::Json => match serde_json::to_string_pretty(decision) {
            Ok(json) => {
                eprintln!("{json}");
                Ok(())
            }
            Err(error) => {
                eprintln!("failed to serialize pre-mutation command output: {error}");
                Err(error)
            }
        },
    }
}

#[cfg(target_os = "linux")]
fn generate_host_prepare_journal_id() -> Result<JournalId, String> {
    const RANDOM_BYTES: usize = 16;
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let mut random = [0_u8; RANDOM_BYTES];
    let filled = getrandom(&mut random, GetRandomFlags::empty()).map_err(|_| {
        "could not obtain operating-system randomness for a host-preparation journal ID".to_owned()
    })?;
    if filled != random.len() {
        return Err(
            "operating-system randomness returned an incomplete host-preparation journal ID"
                .to_owned(),
        );
    }

    let mut value = String::from("host-prepare-");
    value.reserve(random.len() * 2);
    for byte in random {
        value.push(HEX[(byte >> 4) as usize] as char);
        value.push(HEX[(byte & 0x0f) as usize] as char);
    }
    JournalId::parse(&value)
        .map_err(|_| "generated host-preparation journal ID was invalid".to_owned())
}

fn emit_runtime_error(output: OutputFormat, kind: &'static str, message: String) -> ExitCode {
    match output {
        OutputFormat::Human => eprintln!("{message}"),
        OutputFormat::Json => {
            let report = RuntimeErrorReport {
                schema_version: 1,
                kind,
                message,
            };
            if print_json(&report).is_err() {
                return ExitCode::from(2);
            }
        }
    }
    ExitCode::from(2)
}

fn load_manifest(
    output: OutputFormat,
    file: &Path,
) -> Result<smolrunner::manifest::Manifest, ExitCode> {
    load(file).map_err(|error| {
        match output {
            OutputFormat::Human => eprint!("{error}"),
            OutputFormat::Json => {
                let report = ErrorReport {
                    schema_version: 1,
                    error: &error,
                };
                if print_json(&report).is_err() {
                    return ExitCode::from(2);
                }
            }
        }
        ExitCode::from(2)
    })
}

fn print_json(value: &impl Serialize) -> Result<(), serde_json::Error> {
    match serde_json::to_string_pretty(value) {
        Ok(json) => {
            println!("{json}");
            Ok(())
        }
        Err(error) => {
            eprintln!("failed to serialize command output: {error}");
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use clap::Parser;

    use super::{Cli, Command, HostCommand};

    #[test]
    fn host_prepare_accepts_explicit_confirmation_and_account_policy() {
        let cli = Cli::try_parse_from([
            "smolrunner",
            "host",
            "prepare",
            "--file",
            "project.yml",
            "--account-file",
            "runner.account.yml",
            "--confirm",
            "host-preparation-v1.00",
        ])
        .expect("parse host prepare command");

        let Command::Host {
            command:
                HostCommand::Prepare {
                    file,
                    account_file,
                    confirm,
                },
        } = cli.command
        else {
            panic!("expected host prepare command");
        };
        assert_eq!(file, PathBuf::from("project.yml"));
        assert_eq!(account_file, Some(PathBuf::from("runner.account.yml")));
        assert_eq!(confirm.as_deref(), Some("host-preparation-v1.00"));
    }
}
