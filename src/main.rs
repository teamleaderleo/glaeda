use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;
use smolrunner::doctor::{inspect_host, render_human as render_doctor};
#[cfg(target_os = "linux")]
use smolrunner::host_readiness::{
    inspect_host_readiness, render_human as render_host_plan,
};
use smolrunner::manifest::{ManifestError, load};
use smolrunner::plan::{build, render_human as render_plan};
#[cfg(target_os = "linux")]
use smolrunner::process::ProcessExecutor;

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
    /// Inspect and plan host-level state without making changes.
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
fn run_host_plan(
    output: OutputFormat,
    file: &Path,
    account_file: Option<&Path>,
) -> ExitCode {
    let manifest = match load_manifest(output, file) {
        Ok(manifest) => manifest,
        Err(code) => return code,
    };
    let report = match inspect_host_readiness(
        &manifest,
        file,
        account_file,
        &ProcessExecutor,
    ) {
        Ok(report) => report,
        Err(error) => {
            let message = format!("failed to inspect host readiness: {error}");
            match output {
                OutputFormat::Human => eprintln!("{message}"),
                OutputFormat::Json => {
                    let report = RuntimeErrorReport {
                        schema_version: 1,
                        kind: "host_readiness_probe",
                        message,
                    };
                    if print_json(&report).is_err() {
                        return ExitCode::from(2);
                    }
                }
            }
            return ExitCode::from(2);
        }
    };

    match output {
        OutputFormat::Human => print!("{}", render_host_plan(&report)),
        OutputFormat::Json => {
            if print_json(&report).is_err() {
                return ExitCode::from(2);
            }
        }
    }

    ExitCode::SUCCESS
}

#[cfg(not(target_os = "linux"))]
fn run_host_plan(
    output: OutputFormat,
    file: &Path,
    _account_file: Option<&Path>,
) -> ExitCode {
    if let Err(code) = load_manifest(output, file) {
        return code;
    }
    let message = "host planning currently supports Linux only".to_owned();
    match output {
        OutputFormat::Human => eprintln!("{message}"),
        OutputFormat::Json => {
            let report = RuntimeErrorReport {
                schema_version: 1,
                kind: "host_readiness_probe",
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
