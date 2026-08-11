mod personal_worker_cancel_command;
mod personal_worker_read_command;
mod personal_worker_submit_command;

use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;
#[cfg(target_os = "macos")]
use std::{
    fs::File,
    io::{Read as _, Seek as _, SeekFrom},
};

use clap::{Args, Parser, Subcommand, ValueEnum};
use personal_worker_cancel_command::{
    PersonalWorkerCancelCommandError, cancel_queued_job, render_cancel_receipt_human,
};
use personal_worker_read_command::{
    PersonalWorkerReadCommandError, read_job, read_queue_page, read_status, render_job_human,
    render_queue_page_human, render_status_human,
};
use personal_worker_submit_command::{
    PersonalWorkerSubmitCommandError, PersonalWorkerSubmitCommandInput,
    render_submit_receipt_human, submit_queued_job,
};
#[cfg(target_os = "linux")]
use rustix::rand::{GetRandomFlags, getrandom};
#[cfg(target_os = "macos")]
use rustix::{
    fs::{self as rustix_fs, AtFlags, FileType, Mode, OFlags},
    process::{getegid, geteuid},
};
use serde::Serialize;
use smolrunner::artifact::Sha256Digest;
use smolrunner::disposable_launchd_service::{
    DisposableLaunchdServiceDesiredState, plan_disposable_launchd_service,
};
#[cfg(target_os = "macos")]
use smolrunner::disposable_worker_enrollment::{
    MAX_DISPOSABLE_WORKER_ENROLLMENT_BYTES, decode_disposable_worker_enrollment,
};
#[cfg(target_os = "macos")]
use smolrunner::disposable_worker_service::serve_disposable_worker;
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
use smolrunner::host_preparation_plan::{ExecutableHostPreparationAction, plan_host_preparation};
#[cfg(target_os = "linux")]
use smolrunner::host_readiness::{RunnerAccountReadiness, inspect_host_readiness};
#[cfg(target_os = "linux")]
use smolrunner::host_readiness_verdict::{assess, render_human as render_host_plan};
#[cfg(target_os = "linux")]
use smolrunner::journal::ExecutionLane;
#[cfg(target_os = "linux")]
use smolrunner::lane_command::LaneCommandKind;
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
use smolrunner::runner_user_observation::observe_verified_runner_user;
#[cfg(target_os = "linux")]
use smolrunner::state::JournalId;
#[cfg(unix)]
use smolrunner::unix_personal_worker_store::UnixPersonalWorkerStore;

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
    /// Inspect one exact durable personal-worker snapshot.
    Worker {
        #[command(subcommand)]
        command: WorkerCommand,
    },
    /// Plan the operator-owned macOS service lifecycle without changing the host.
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    /// Inspect the exact live personal-worker queue.
    Queue {
        #[command(subcommand)]
        command: QueueCommand,
    },
    /// Inspect or cancel one exact personal-worker job.
    Job {
        #[command(subcommand)]
        command: JobCommand,
    },
}

#[derive(Debug, Subcommand)]
enum ServiceCommand {
    /// Build an exact LaunchAgent install or removal plan without applying it.
    Plan {
        /// Desired LaunchAgent state.
        #[arg(long, value_enum)]
        desired: ServiceDesiredState,
        /// Explicit absolute normalized operator home directory.
        #[arg(long)]
        operator_home: PathBuf,
        /// Exact absolute normalized SmolRunner executable path.
        #[arg(long)]
        program: PathBuf,
        /// Exact reviewed SmolRunner executable content digest.
        #[arg(long)]
        program_digest: String,
        /// Exact absolute normalized canonical enrollment document.
        #[arg(long)]
        enrollment: PathBuf,
        /// Exact canonical enrollment-document content digest.
        #[arg(long)]
        enrollment_digest: String,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ServiceDesiredState {
    Installed,
    Removed,
}

impl From<ServiceDesiredState> for DisposableLaunchdServiceDesiredState {
    fn from(value: ServiceDesiredState) -> Self {
        match value {
            ServiceDesiredState::Installed => Self::Installed,
            ServiceDesiredState::Removed => Self::Removed,
        }
    }
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
    /// Execute one exactly confirmed reviewed host-preparation phase.
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

#[derive(Debug, Subcommand)]
enum WorkerCommand {
    /// Show bounded status for one already-published durable snapshot.
    Status {
        /// Explicit absolute normalized personal-worker state root.
        #[arg(long)]
        store_root: PathBuf,
    },
    /// Read one bounded durable status report for the disposable-worker service.
    ServiceStatus {
        /// Explicit absolute normalized disposable-worker state root.
        #[arg(long)]
        store_root: PathBuf,
    },
    /// Inspect or change the admission hold for disposable workers.
    Admission {
        #[command(subcommand)]
        command: WorkerAdmissionCommand,
    },
    /// Run the enrolled disposable-worker reconciler until the process supervisor stops it.
    Serve {
        /// Explicit absolute normalized canonical enrollment document.
        #[arg(long)]
        enrollment: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum WorkerAdmissionCommand {
    /// Show whether new disposable workers are held.
    Status {
        /// Explicit absolute normalized disposable-worker state root.
        #[arg(long)]
        store_root: PathBuf,
    },
    /// Stop admitting new disposable workers while cleanup and recovery continue.
    Hold {
        /// Explicit absolute normalized disposable-worker state root.
        #[arg(long)]
        store_root: PathBuf,
    },
    /// Clear the admission hold; all other safety and recovery vetoes remain active.
    Resume {
        /// Explicit absolute normalized disposable-worker state root.
        #[arg(long)]
        store_root: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum QueueCommand {
    /// List one bounded page of the exact live queue.
    List {
        /// Explicit absolute normalized personal-worker state root.
        #[arg(long)]
        store_root: PathBuf,
        /// Exact expected durable store revision.
        #[arg(long)]
        revision: u64,
        /// Exact expected queue generation.
        #[arg(long)]
        generation: u64,
        /// Zero-based offset within the exact live snapshot.
        #[arg(long, default_value_t = 0)]
        offset: u32,
        /// Positive bounded page size.
        #[arg(long, default_value_t = 100)]
        limit: u16,
    },
    /// Submit one exact queued request using caller-supplied durable evidence.
    Submit(Box<QueueSubmitArgs>),
}

#[derive(Debug, Args)]
struct QueueSubmitArgs {
    /// Explicit absolute normalized personal-worker state root.
    #[arg(long)]
    store_root: PathBuf,
    /// Exact expected durable store revision.
    #[arg(long)]
    revision: String,
    /// Exact expected queue generation.
    #[arg(long)]
    generation: String,
    /// Explicit queue observation in epoch milliseconds.
    #[arg(long)]
    observed_at: String,
    /// Exact bounded personal-worker request ID.
    #[arg(long)]
    request_id: String,
    /// Exact verification-profile ID.
    #[arg(long)]
    verification_profile: String,
    /// Exact runner-profile ID.
    #[arg(long)]
    runner_profile: String,
    /// Exact owner/name repository identity.
    #[arg(long)]
    repository: String,
    /// Complete immutable Git commit object ID.
    #[arg(long)]
    commit: String,
    /// Complete immutable Git tree object ID.
    #[arg(long)]
    tree: String,
    /// Fixed priority: background, normal, or interactive.
    #[arg(long)]
    priority: String,
    /// Positive bounded requested CPU millicores.
    #[arg(long)]
    cpu_millis: String,
    /// Positive bounded requested memory bytes.
    #[arg(long)]
    memory_bytes: String,
    /// Positive bounded requested PID count.
    #[arg(long)]
    pids: String,
    /// Exact bounded cache ID.
    #[arg(long)]
    cache_id: String,
    /// Canonical cache namespace digest.
    #[arg(long)]
    cache_namespace_digest: String,
    /// Fixed cache access: read, write, or exclusive.
    #[arg(long)]
    cache_access: String,
    /// Explicit submission time in epoch milliseconds.
    #[arg(long)]
    submitted_at: String,
    /// Optional explicit operator deadline in epoch milliseconds.
    #[arg(long)]
    operator_deadline: Option<String>,
}

#[derive(Debug, Subcommand)]
enum JobCommand {
    /// Show one exact queued, active, or retained-terminal job.
    Show {
        /// Explicit absolute normalized personal-worker state root.
        #[arg(long)]
        store_root: PathBuf,
        /// Exact expected durable store revision.
        #[arg(long)]
        revision: u64,
        /// Exact expected queue generation.
        #[arg(long)]
        generation: u64,
        /// Exact bounded personal-worker request ID.
        request_id: String,
    },
    /// Cancel one exact queued job using caller-supplied durable evidence.
    Cancel {
        /// Explicit absolute normalized personal-worker state root.
        #[arg(long)]
        store_root: PathBuf,
        /// Exact expected durable store revision.
        #[arg(long)]
        revision: u64,
        /// Exact expected queue generation.
        #[arg(long)]
        generation: u64,
        /// Explicit cancellation observation in epoch milliseconds.
        #[arg(long)]
        cancelled_at: u64,
        /// Exact bounded personal-worker request ID.
        request_id: String,
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
        Command::Worker { command } => match command {
            WorkerCommand::Status { store_root } => run_worker_status(cli.output, &store_root),
            WorkerCommand::ServiceStatus { store_root } => {
                run_disposable_worker_service_status(cli.output, &store_root)
            }
            WorkerCommand::Admission { command } => match command {
                WorkerAdmissionCommand::Status { store_root } => {
                    run_worker_admission(cli.output, &store_root, None)
                }
                WorkerAdmissionCommand::Hold { store_root } => {
                    run_worker_admission(cli.output, &store_root, Some(true))
                }
                WorkerAdmissionCommand::Resume { store_root } => {
                    run_worker_admission(cli.output, &store_root, Some(false))
                }
            },
            WorkerCommand::Serve { enrollment } => run_worker_serve(cli.output, &enrollment),
        },
        Command::Service { command } => match command {
            ServiceCommand::Plan {
                desired,
                operator_home,
                program,
                program_digest,
                enrollment,
                enrollment_digest,
            } => run_disposable_launchd_service_plan(
                cli.output,
                desired.into(),
                &operator_home,
                &program,
                &program_digest,
                &enrollment,
                &enrollment_digest,
            ),
        },
        Command::Queue { command } => match command {
            QueueCommand::List {
                store_root,
                revision,
                generation,
                offset,
                limit,
            } => run_queue_list(cli.output, &store_root, revision, generation, offset, limit),
            QueueCommand::Submit(arguments) => run_queue_submit(
                cli.output,
                &arguments.store_root,
                PersonalWorkerSubmitCommandInput {
                    revision: &arguments.revision,
                    generation: &arguments.generation,
                    observed_at: &arguments.observed_at,
                    request_id: &arguments.request_id,
                    verification_profile: &arguments.verification_profile,
                    runner_profile: &arguments.runner_profile,
                    repository: &arguments.repository,
                    commit: &arguments.commit,
                    tree: &arguments.tree,
                    priority: &arguments.priority,
                    cpu_millis: &arguments.cpu_millis,
                    memory_bytes: &arguments.memory_bytes,
                    pids: &arguments.pids,
                    cache_id: &arguments.cache_id,
                    cache_namespace_digest: &arguments.cache_namespace_digest,
                    cache_access: &arguments.cache_access,
                    submitted_at: &arguments.submitted_at,
                    operator_deadline: arguments.operator_deadline.as_deref(),
                },
            ),
        },
        Command::Job { command } => match command {
            JobCommand::Show {
                store_root,
                revision,
                generation,
                request_id,
            } => run_job_show(cli.output, &store_root, revision, generation, &request_id),
            JobCommand::Cancel {
                store_root,
                revision,
                generation,
                cancelled_at,
                request_id,
            } => run_job_cancel(
                cli.output,
                &store_root,
                revision,
                generation,
                cancelled_at,
                &request_id,
            ),
        },
    }
}

#[cfg(target_os = "macos")]
fn run_disposable_launchd_service_plan(
    output: OutputFormat,
    desired: DisposableLaunchdServiceDesiredState,
    operator_home: &Path,
    program: &Path,
    program_digest: &str,
    enrollment: &Path,
    enrollment_digest: &str,
) -> ExitCode {
    let program_digest = match Sha256Digest::parse(program_digest) {
        Ok(digest) => digest,
        Err(_) => {
            return emit_runtime_error(
                output,
                "disposable_launchd_service_plan",
                "disposable-worker LaunchAgent plan inputs are invalid".to_owned(),
            );
        }
    };
    let enrollment_digest = match Sha256Digest::parse(enrollment_digest) {
        Ok(digest) => digest,
        Err(_) => {
            return emit_runtime_error(
                output,
                "disposable_launchd_service_plan",
                "disposable-worker LaunchAgent plan inputs are invalid".to_owned(),
            );
        }
    };
    let plan = match plan_disposable_launchd_service(
        desired,
        geteuid().as_raw(),
        operator_home,
        program,
        &program_digest,
        enrollment,
        &enrollment_digest,
    ) {
        Ok(plan) => plan,
        Err(_) => {
            return emit_runtime_error(
                output,
                "disposable_launchd_service_plan",
                "disposable-worker LaunchAgent plan inputs are invalid".to_owned(),
            );
        }
    };
    match output {
        OutputFormat::Json => {
            if print_json(plan.report()).is_err() {
                return ExitCode::from(2);
            }
        }
        OutputFormat::Human => {
            println!(
                "disposable worker service plan: {:?}",
                plan.report().desired_state()
            );
            println!(
                "domain={}, plan={}, approval=required",
                plan.report().launchd_domain(),
                plan.report().plan_identity().as_str(),
            );
            for action in plan.report().actions() {
                println!(
                    "{}. {:?}: {} (rollback={:?})",
                    action.sequence(),
                    action.kind(),
                    action.summary(),
                    action.rollback(),
                );
            }
            println!("No changes were made.");
        }
    }
    ExitCode::SUCCESS
}

#[cfg(not(target_os = "macos"))]
fn run_disposable_launchd_service_plan(
    output: OutputFormat,
    _desired: DisposableLaunchdServiceDesiredState,
    _operator_home: &Path,
    _program: &Path,
    _program_digest: &str,
    _enrollment: &Path,
    _enrollment_digest: &str,
) -> ExitCode {
    emit_runtime_error(
        output,
        "disposable_launchd_service_plan_unsupported",
        "disposable-worker LaunchAgent planning requires macOS".to_owned(),
    )
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

fn run_worker_status(output: OutputFormat, store_root: &Path) -> ExitCode {
    let view = match read_status(store_root) {
        Ok(view) => view,
        Err(error) => return emit_personal_worker_read_error(output, &error),
    };
    match output {
        OutputFormat::Human => print!("{}", render_status_human(&view)),
        OutputFormat::Json => {
            if print_json(&view).is_err() {
                return ExitCode::from(2);
            }
        }
    }
    ExitCode::SUCCESS
}

#[cfg(unix)]
fn run_disposable_worker_service_status(output: OutputFormat, store_root: &Path) -> ExitCode {
    if !explicit_normalized_absolute_path(store_root) {
        return emit_runtime_error(
            output,
            "disposable_worker_status",
            "disposable worker state root must be an explicit absolute normalized path".to_owned(),
        );
    }
    let store = match UnixPersonalWorkerStore::open_existing_read_only(store_root) {
        Ok(store) => store,
        Err(_) => {
            return emit_runtime_error(
                output,
                "disposable_worker_status",
                "disposable worker service status is unavailable".to_owned(),
            );
        }
    };
    let status = match store.inspect_disposable_worker_service_status() {
        Ok(status) => status,
        Err(_) => {
            return emit_runtime_error(
                output,
                "disposable_worker_status",
                "disposable worker service status is unavailable".to_owned(),
            );
        }
    };
    match output {
        OutputFormat::Json => {
            if print_json(&status).is_err() {
                return ExitCode::from(2);
            }
        }
        OutputFormat::Human => {
            println!("disposable worker service: {:?}", status.state());
            println!(
                "controller={}, admission={}, catalog_revision={}, inbox_revision={}, active_attempts={}, retained_tombstones={}",
                if status.controller_running() {
                    "running"
                } else {
                    "stopped"
                },
                if status.admission_held() {
                    "held"
                } else {
                    "open"
                },
                status
                    .catalog_revision()
                    .map_or_else(|| "missing".to_owned(), |value| value.to_string()),
                status
                    .inbox_revision()
                    .map_or_else(|| "missing".to_owned(), |value| value.to_string()),
                status.active_attempts().len(),
                status.retained_tombstones(),
            );
            for attempt in status.active_attempts() {
                println!(
                    "attempt {}: phase={:?}, vm_bound={}, runner_bound={}, job_bound={}",
                    attempt.attempt_id(),
                    attempt.phase(),
                    attempt.vm_bound(),
                    attempt.runner_bound(),
                    attempt.job_bound(),
                );
            }
            for blocker in status.blockers() {
                println!("blocker: {blocker}");
            }
        }
    }
    ExitCode::SUCCESS
}

#[cfg(not(unix))]
fn run_disposable_worker_service_status(output: OutputFormat, _store_root: &Path) -> ExitCode {
    emit_runtime_error(
        output,
        "disposable_worker_status_unsupported",
        "disposable worker status requires a Unix host".to_owned(),
    )
}

#[cfg(unix)]
fn run_worker_admission(
    output: OutputFormat,
    store_root: &Path,
    desired_hold: Option<bool>,
) -> ExitCode {
    if !explicit_normalized_absolute_path(store_root) {
        return emit_runtime_error(
            output,
            "disposable_worker_admission_control",
            "disposable worker state root must be an explicit absolute normalized path".to_owned(),
        );
    }
    let status = if let Some(held) = desired_hold {
        let mut store = match UnixPersonalWorkerStore::open_or_create_disposable_catalog(store_root)
        {
            Ok(store) => store,
            Err(_) => {
                return emit_runtime_error(
                    output,
                    "disposable_worker_admission_control",
                    "disposable worker store is unavailable".to_owned(),
                );
            }
        };
        match store.set_disposable_worker_admission_hold(held) {
            Ok(status) => status,
            Err(_) => {
                return emit_runtime_error(
                    output,
                    "disposable_worker_admission_control",
                    "disposable worker admission control failed".to_owned(),
                );
            }
        }
    } else {
        let store = match UnixPersonalWorkerStore::open_existing_read_only(store_root) {
            Ok(store) => store,
            Err(_) => {
                return emit_runtime_error(
                    output,
                    "disposable_worker_admission_control",
                    "disposable worker admission status is unavailable".to_owned(),
                );
            }
        };
        match store.inspect_disposable_worker_admission() {
            Ok(status) => status,
            Err(_) => {
                return emit_runtime_error(
                    output,
                    "disposable_worker_admission_control",
                    "disposable worker admission status is unavailable".to_owned(),
                );
            }
        }
    };
    match output {
        OutputFormat::Human => println!(
            "disposable worker admission: {}",
            if status.admission_held() {
                "held"
            } else {
                "open"
            }
        ),
        OutputFormat::Json => {
            if print_json(&status).is_err() {
                return ExitCode::from(2);
            }
        }
    }
    ExitCode::SUCCESS
}

fn explicit_normalized_absolute_path(path: &Path) -> bool {
    let normalized = path.components().collect::<PathBuf>();
    path.is_absolute()
        && normalized.as_os_str() == path.as_os_str()
        && !path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
}

#[cfg(target_os = "macos")]
fn run_worker_serve(output: OutputFormat, enrollment_path: &Path) -> ExitCode {
    if !explicit_normalized_absolute_path(enrollment_path) {
        return emit_runtime_error(
            output,
            "disposable_worker_service",
            "disposable worker enrollment path must be explicit, absolute, and normalized"
                .to_owned(),
        );
    }
    let bytes = match read_private_disposable_worker_enrollment(enrollment_path) {
        Ok(bytes) => bytes,
        Err(()) => {
            return emit_runtime_error(
                output,
                "disposable_worker_service",
                "disposable worker enrollment is unavailable".to_owned(),
            );
        }
    };
    let enrollment = match decode_disposable_worker_enrollment(&bytes) {
        Ok(enrollment) => enrollment,
        Err(error) => {
            return emit_runtime_error(
                output,
                error.code(),
                "disposable worker enrollment was refused".to_owned(),
            );
        }
    };
    match serve_disposable_worker(enrollment) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => emit_runtime_error(
            output,
            error.code(),
            "disposable worker service stopped with a durable blocker".to_owned(),
        ),
    }
}

#[cfg(target_os = "macos")]
const ENROLLMENT_DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

#[cfg(target_os = "macos")]
const ENROLLMENT_FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC);

#[cfg(target_os = "macos")]
fn read_private_disposable_worker_enrollment(path: &Path) -> Result<Vec<u8>, ()> {
    if !explicit_normalized_absolute_path(path) {
        return Err(());
    }
    let parent_path = path.parent().ok_or(())?;
    let file_name = path.file_name().ok_or(())?;
    let parent = open_enrollment_directory_chain(parent_path)?;
    let parent_before = rustix_fs::fstat(&parent).map_err(|_| ())?;
    inspect_enrollment_parent(&parent_before)?;
    let resolved_parent = rustix_fs::stat(parent_path).map_err(|_| ())?;
    if !same_enrollment_directory(&parent_before, &resolved_parent) {
        return Err(());
    }

    let held = rustix_fs::openat(&parent, file_name, ENROLLMENT_FILE_FLAGS, Mode::empty())
        .map_err(|_| ())?;
    let mut file = File::from(held);
    let before = rustix_fs::fstat(&file).map_err(|_| ())?;
    inspect_enrollment_file(&before)?;
    let path_before =
        rustix_fs::statat(&parent, file_name, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| ())?;
    if !same_enrollment_snapshot(&before, &path_before) {
        return Err(());
    }

    let mut bytes = Vec::with_capacity(MAX_DISPOSABLE_WORKER_ENROLLMENT_BYTES);
    file.by_ref()
        .take((MAX_DISPOSABLE_WORKER_ENROLLMENT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() > MAX_DISPOSABLE_WORKER_ENROLLMENT_BYTES {
        return Err(());
    }
    file.seek(SeekFrom::Start(0)).map_err(|_| ())?;
    let mut confirmation = Vec::with_capacity(bytes.len());
    file.by_ref()
        .take((MAX_DISPOSABLE_WORKER_ENROLLMENT_BYTES + 1) as u64)
        .read_to_end(&mut confirmation)
        .map_err(|_| ())?;
    if confirmation != bytes {
        return Err(());
    }

    let after = rustix_fs::fstat(&file).map_err(|_| ())?;
    let path_after =
        rustix_fs::statat(&parent, file_name, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| ())?;
    let parent_after = rustix_fs::fstat(&parent).map_err(|_| ())?;
    let resolved_parent_after = rustix_fs::stat(parent_path).map_err(|_| ())?;
    if !same_enrollment_snapshot(&before, &after)
        || !same_enrollment_snapshot(&before, &path_after)
        || !same_enrollment_directory(&parent_before, &parent_after)
        || !same_enrollment_directory(&parent_before, &resolved_parent_after)
    {
        return Err(());
    }
    Ok(bytes)
}

#[cfg(target_os = "macos")]
fn open_enrollment_directory_chain(path: &Path) -> Result<std::os::fd::OwnedFd, ()> {
    let mut directory =
        rustix_fs::open("/", ENROLLMENT_DIRECTORY_FLAGS, Mode::empty()).map_err(|_| ())?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                directory =
                    rustix_fs::openat(&directory, name, ENROLLMENT_DIRECTORY_FLAGS, Mode::empty())
                        .map_err(|_| ())?;
            }
            _ => return Err(()),
        }
    }
    Ok(directory)
}

#[cfg(target_os = "macos")]
fn inspect_enrollment_parent(stat: &rustix_fs::Stat) -> Result<(), ()> {
    if !FileType::from_raw_mode(stat.st_mode).is_dir()
        || stat.st_uid != geteuid().as_raw()
        || stat.st_gid != getegid().as_raw()
        || stat.st_mode & 0o022 != 0
    {
        return Err(());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn inspect_enrollment_file(stat: &rustix_fs::Stat) -> Result<(), ()> {
    if !FileType::from_raw_mode(stat.st_mode).is_file()
        || stat.st_nlink != 1
        || stat.st_uid != geteuid().as_raw()
        || stat.st_gid != getegid().as_raw()
        || stat.st_mode & 0o7777 != 0o600
        || stat.st_size < 0
        || usize::try_from(stat.st_size)
            .ok()
            .is_none_or(|size| size > MAX_DISPOSABLE_WORKER_ENROLLMENT_BYTES)
    {
        return Err(());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn same_enrollment_snapshot(left: &rustix_fs::Stat, right: &rustix_fs::Stat) -> bool {
    left.st_dev == right.st_dev
        && left.st_ino == right.st_ino
        && left.st_mode == right.st_mode
        && left.st_nlink == right.st_nlink
        && left.st_uid == right.st_uid
        && left.st_gid == right.st_gid
        && left.st_size == right.st_size
        && left.st_mtime == right.st_mtime
        && left.st_mtime_nsec == right.st_mtime_nsec
        && left.st_ctime == right.st_ctime
        && left.st_ctime_nsec == right.st_ctime_nsec
}

#[cfg(target_os = "macos")]
fn same_enrollment_directory(left: &rustix_fs::Stat, right: &rustix_fs::Stat) -> bool {
    left.st_dev == right.st_dev
        && left.st_ino == right.st_ino
        && left.st_mode == right.st_mode
        && left.st_uid == right.st_uid
        && left.st_gid == right.st_gid
}

#[cfg(not(target_os = "macos"))]
fn run_worker_serve(output: OutputFormat, _enrollment_path: &Path) -> ExitCode {
    emit_runtime_error(
        output,
        "disposable_worker_service_unsupported",
        "disposable worker service currently requires macOS".to_owned(),
    )
}

#[cfg(not(unix))]
fn run_worker_admission(
    output: OutputFormat,
    _store_root: &Path,
    _desired_hold: Option<bool>,
) -> ExitCode {
    emit_runtime_error(
        output,
        "disposable_worker_admission_control",
        "disposable worker admission control requires a Unix host".to_owned(),
    )
}

fn run_queue_list(
    output: OutputFormat,
    store_root: &Path,
    revision: u64,
    generation: u64,
    offset: u32,
    limit: u16,
) -> ExitCode {
    let view = match read_queue_page(store_root, revision, generation, offset, limit) {
        Ok(view) => view,
        Err(error) => return emit_personal_worker_read_error(output, &error),
    };
    match output {
        OutputFormat::Human => print!("{}", render_queue_page_human(&view)),
        OutputFormat::Json => {
            if print_json(&view).is_err() {
                return ExitCode::from(2);
            }
        }
    }
    ExitCode::SUCCESS
}

fn run_queue_submit(
    output: OutputFormat,
    store_root: &Path,
    input: PersonalWorkerSubmitCommandInput<'_>,
) -> ExitCode {
    let receipt = match submit_queued_job(store_root, input) {
        Ok(receipt) => receipt,
        Err(error) => return emit_personal_worker_submit_error(output, &error),
    };
    match output {
        OutputFormat::Human => print!("{}", render_submit_receipt_human(&receipt)),
        OutputFormat::Json => {
            if print_json(&receipt).is_err() {
                return ExitCode::from(2);
            }
        }
    }
    ExitCode::SUCCESS
}

fn run_job_show(
    output: OutputFormat,
    store_root: &Path,
    revision: u64,
    generation: u64,
    request_id: &str,
) -> ExitCode {
    let view = match read_job(store_root, revision, generation, request_id) {
        Ok(view) => view,
        Err(error) => return emit_personal_worker_read_error(output, &error),
    };
    match output {
        OutputFormat::Human => print!("{}", render_job_human(&view)),
        OutputFormat::Json => {
            if print_json(&view).is_err() {
                return ExitCode::from(2);
            }
        }
    }
    ExitCode::SUCCESS
}

fn run_job_cancel(
    output: OutputFormat,
    store_root: &Path,
    revision: u64,
    generation: u64,
    cancelled_at: u64,
    request_id: &str,
) -> ExitCode {
    let receipt =
        match cancel_queued_job(store_root, revision, generation, cancelled_at, request_id) {
            Ok(receipt) => receipt,
            Err(error) => return emit_personal_worker_cancel_error(output, &error),
        };
    match output {
        OutputFormat::Human => print!("{}", render_cancel_receipt_human(&receipt)),
        OutputFormat::Json => {
            if print_json(&receipt).is_err() {
                return ExitCode::from(2);
            }
        }
    }
    ExitCode::SUCCESS
}

fn emit_personal_worker_read_error(
    output: OutputFormat,
    error: &PersonalWorkerReadCommandError,
) -> ExitCode {
    match output {
        OutputFormat::Human => eprintln!("{}", error.message()),
        OutputFormat::Json => {
            if print_json(error).is_err() {
                return ExitCode::from(2);
            }
        }
    }
    ExitCode::from(2)
}

fn emit_personal_worker_submit_error(
    output: OutputFormat,
    error: &PersonalWorkerSubmitCommandError,
) -> ExitCode {
    match output {
        OutputFormat::Human => eprintln!("{}", error.message()),
        OutputFormat::Json => {
            if print_json(error).is_err() {
                return ExitCode::from(2);
            }
        }
    }
    ExitCode::from(2)
}

fn emit_personal_worker_cancel_error(
    output: OutputFormat,
    error: &PersonalWorkerCancelCommandError,
) -> ExitCode {
    match output {
        OutputFormat::Human => eprintln!("{}", error.message()),
        OutputFormat::Json => {
            if print_json(error).is_err() {
                return ExitCode::from(2);
            }
        }
    }
    ExitCode::from(2)
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
    let phase_kind = match classify_host_prepare_actions(&confirmed_phase.actions) {
        Ok(kind) => kind,
        Err(message) => {
            return emit_runtime_error(
                output,
                "unsupported_host_preparation_phase",
                message.to_owned(),
            );
        }
    };

    if emit_pre_mutation_decision(output, &decision).is_err() {
        return ExitCode::from(2);
    }

    let verified_runner_user = match phase_kind {
        HostPreparePhaseKind::Root => None,
        HostPreparePhaseKind::RunnerUserMigration => {
            let desired = match &decision.proposal().source.report().runner_account {
                RunnerAccountReadiness::Planned { plan, .. } => &plan.desired,
                RunnerAccountReadiness::NeedsConfiguration { .. } => {
                    return emit_runtime_error(
                        output,
                        "runner_user_evidence",
                        "runner-user migration requires an exact configured runner account"
                            .to_owned(),
                    );
                }
            };
            match observe_verified_runner_user(desired, &ProcessExecutor) {
                Ok(verified) => Some(verified),
                Err(error) => {
                    return emit_runtime_error(
                        output,
                        "runner_user_evidence",
                        error.message().to_owned(),
                    );
                }
            }
        }
    };

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
    let mut runner = match verified_runner_user.as_ref() {
        Some(verified) => SystemLaneCommandRunner::with_runner_user(verified),
        None => SystemLaneCommandRunner::root_only(),
    };
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

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostPreparePhaseKind {
    Root,
    RunnerUserMigration,
}

#[cfg(target_os = "linux")]
fn classify_host_prepare_actions(
    actions: &[ExecutableHostPreparationAction],
) -> Result<HostPreparePhaseKind, &'static str> {
    if !actions.is_empty()
        && actions
            .iter()
            .all(|action| action.lane == ExecutionLane::Root)
    {
        return Ok(HostPreparePhaseKind::Root);
    }
    if actions.len() == 1
        && actions[0].lane == ExecutionLane::RunnerUser
        && actions[0].command_kind == LaneCommandKind::RunnerPodmanMigrate
    {
        return Ok(HostPreparePhaseKind::RunnerUserMigration);
    }
    Err(
        "host preparation executes only an all-root phase or exactly one reviewed runner-user Podman migration action",
    )
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
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use clap::Parser;

    #[cfg(target_os = "linux")]
    use smolrunner::host_preparation_plan::ExecutableHostPreparationAction;
    #[cfg(target_os = "linux")]
    use smolrunner::journal::{ExecutionLane, RollbackClass};
    #[cfg(target_os = "linux")]
    use smolrunner::lane_command::LaneCommandKind;

    use super::{
        Cli, Command, HostCommand, JobCommand, OutputFormat, QueueCommand, ServiceCommand,
        ServiceDesiredState, WorkerAdmissionCommand, WorkerCommand,
        run_disposable_worker_service_status, run_worker_admission,
    };
    #[cfg(target_os = "linux")]
    use super::{HostPreparePhaseKind, classify_host_prepare_actions};

    static NEXT_DISPOSABLE_ROOT: AtomicU64 = AtomicU64::new(1);

    struct DisposableTempRoot(PathBuf);

    impl DisposableTempRoot {
        fn new() -> Self {
            let sequence = NEXT_DISPOSABLE_ROOT.fetch_add(1, Ordering::Relaxed);
            let temporary_parent = fs::canonicalize(std::env::temp_dir())
                .expect("canonicalize disposable CLI temporary parent");
            let path = temporary_parent.join(format!(
                "smolrunner-cli-disposable-admission-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create disposable CLI state root");
            #[cfg(unix)]
            fs::set_permissions(&path, fs::Permissions::from_mode(0o750))
                .expect("set disposable CLI root mode");
            Self(path)
        }
    }

    impl Drop for DisposableTempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

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

    #[test]
    fn personal_worker_read_commands_parse_exact_snapshot_arguments() {
        let status = Cli::try_parse_from([
            "smolrunner",
            "worker",
            "status",
            "--store-root",
            "/tmp/worker-state",
        ])
        .expect("parse worker status");
        let Command::Worker {
            command: WorkerCommand::Status { store_root },
        } = status.command
        else {
            panic!("expected worker status command");
        };
        assert_eq!(store_root, PathBuf::from("/tmp/worker-state"));

        let service_status = Cli::try_parse_from([
            "smolrunner",
            "worker",
            "service-status",
            "--store-root",
            "/tmp/disposable-state",
        ])
        .expect("parse disposable service status");
        let Command::Worker {
            command: WorkerCommand::ServiceStatus { store_root },
        } = service_status.command
        else {
            panic!("expected disposable service status command");
        };
        assert_eq!(store_root, PathBuf::from("/tmp/disposable-state"));

        let admission = Cli::try_parse_from([
            "smolrunner",
            "worker",
            "admission",
            "hold",
            "--store-root",
            "/tmp/disposable-state",
        ])
        .expect("parse disposable admission hold");
        let Command::Worker {
            command:
                WorkerCommand::Admission {
                    command: WorkerAdmissionCommand::Hold { store_root },
                },
        } = admission.command
        else {
            panic!("expected disposable admission hold command");
        };
        assert_eq!(store_root, PathBuf::from("/tmp/disposable-state"));

        let serve = Cli::try_parse_from([
            "smolrunner",
            "worker",
            "serve",
            "--enrollment",
            "/etc/smolrunner/disposable-worker.json",
        ])
        .expect("parse disposable worker service");
        let Command::Worker {
            command: WorkerCommand::Serve { enrollment },
        } = serve.command
        else {
            panic!("expected disposable worker serve command");
        };
        assert_eq!(
            enrollment,
            PathBuf::from("/etc/smolrunner/disposable-worker.json")
        );

        let queue = Cli::try_parse_from([
            "smolrunner",
            "queue",
            "list",
            "--store-root",
            "/tmp/worker-state",
            "--revision",
            "7",
            "--generation",
            "11",
            "--offset",
            "2",
            "--limit",
            "5",
        ])
        .expect("parse queue list");
        let Command::Queue {
            command:
                QueueCommand::List {
                    store_root,
                    revision,
                    generation,
                    offset,
                    limit,
                },
        } = queue.command
        else {
            panic!("expected queue list command");
        };
        assert_eq!(store_root, PathBuf::from("/tmp/worker-state"));
        assert_eq!(revision, 7);
        assert_eq!(generation, 11);
        assert_eq!(offset, 2);
        assert_eq!(limit, 5);

        let job = Cli::try_parse_from([
            "smolrunner",
            "job",
            "show",
            "--store-root",
            "/tmp/worker-state",
            "--revision",
            "7",
            "--generation",
            "11",
            "job-one",
        ])
        .expect("parse job show");
        let Command::Job {
            command:
                JobCommand::Show {
                    store_root,
                    revision,
                    generation,
                    request_id,
                },
        } = job.command
        else {
            panic!("expected job show command");
        };
        assert_eq!(store_root, PathBuf::from("/tmp/worker-state"));
        assert_eq!(revision, 7);
        assert_eq!(generation, 11);
        assert_eq!(request_id, "job-one");

        let cancel = Cli::try_parse_from([
            "smolrunner",
            "job",
            "cancel",
            "--store-root",
            "/tmp/worker-state",
            "--revision",
            "7",
            "--generation",
            "11",
            "--cancelled-at",
            "123456",
            "job-one",
        ])
        .expect("parse job cancel");
        let Command::Job {
            command:
                JobCommand::Cancel {
                    store_root,
                    revision,
                    generation,
                    cancelled_at,
                    request_id,
                },
        } = cancel.command
        else {
            panic!("expected job cancel command");
        };
        assert_eq!(store_root, PathBuf::from("/tmp/worker-state"));
        assert_eq!(revision, 7);
        assert_eq!(generation, 11);
        assert_eq!(cancelled_at, 123456);
        assert_eq!(request_id, "job-one");
    }

    #[test]
    fn disposable_launchd_service_plan_parses_explicit_private_inputs() {
        let cli = Cli::try_parse_from([
            "smolrunner",
            "service",
            "plan",
            "--desired",
            "installed",
            "--operator-home",
            "/Users/operator",
            "--program",
            "/opt/smolrunner/bin/smolrunner",
            "--program-digest",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "--enrollment",
            "/Users/operator/.config/smolrunner/enrollment.json",
            "--enrollment-digest",
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ])
        .expect("parse disposable LaunchAgent plan");
        let Command::Service {
            command:
                ServiceCommand::Plan {
                    desired,
                    operator_home,
                    program,
                    program_digest,
                    enrollment,
                    enrollment_digest,
                },
        } = cli.command
        else {
            panic!("expected disposable LaunchAgent plan");
        };
        assert!(matches!(desired, ServiceDesiredState::Installed));
        assert_eq!(operator_home, PathBuf::from("/Users/operator"));
        assert_eq!(program, PathBuf::from("/opt/smolrunner/bin/smolrunner"));
        assert_eq!(
            program_digest,
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(
            enrollment,
            PathBuf::from("/Users/operator/.config/smolrunner/enrollment.json")
        );
        assert_eq!(
            enrollment_digest,
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        );
    }

    #[cfg(unix)]
    #[test]
    fn disposable_admission_cli_toggles_the_typed_durable_hold() {
        use smolrunner::unix_personal_worker_store::UnixPersonalWorkerStore;

        let root = DisposableTempRoot::new();
        let _ = run_worker_admission(OutputFormat::Json, &root.0, Some(true));
        let store = UnixPersonalWorkerStore::open_existing_read_only(&root.0).unwrap();
        assert!(
            store
                .inspect_disposable_worker_admission()
                .unwrap()
                .admission_held()
        );
        drop(store);

        let _ = run_worker_admission(OutputFormat::Json, &root.0, Some(false));
        let store = UnixPersonalWorkerStore::open_existing_read_only(&root.0).unwrap();
        assert!(
            !store
                .inspect_disposable_worker_admission()
                .unwrap()
                .admission_held()
        );
        assert_eq!(
            run_disposable_worker_service_status(OutputFormat::Json, &root.0),
            std::process::ExitCode::SUCCESS
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn disposable_worker_enrollment_loader_requires_one_private_regular_file() {
        use std::os::unix::fs::symlink;

        use super::read_private_disposable_worker_enrollment;

        let root = DisposableTempRoot::new();
        let enrollment = root.0.join("enrollment.json");
        fs::write(&enrollment, b"exact-enrollment").expect("write enrollment");
        fs::set_permissions(&enrollment, fs::Permissions::from_mode(0o600))
            .expect("set enrollment mode");
        assert_eq!(
            read_private_disposable_worker_enrollment(&enrollment).unwrap(),
            b"exact-enrollment"
        );

        let alias = root.0.join("enrollment-alias.json");
        symlink(&enrollment, &alias).expect("create enrollment symlink");
        assert!(read_private_disposable_worker_enrollment(&alias).is_err());

        let hardlink = root.0.join("enrollment-hardlink.json");
        fs::hard_link(&enrollment, &hardlink).expect("create enrollment hard link");
        assert!(read_private_disposable_worker_enrollment(&enrollment).is_err());
        fs::remove_file(&hardlink).expect("remove enrollment hard link");

        fs::set_permissions(&enrollment, fs::Permissions::from_mode(0o640))
            .expect("weaken enrollment mode");
        assert!(read_private_disposable_worker_enrollment(&enrollment).is_err());
    }

    #[cfg(target_os = "linux")]
    fn phase_action(
        lane: ExecutionLane,
        command_kind: LaneCommandKind,
    ) -> ExecutableHostPreparationAction {
        ExecutableHostPreparationAction {
            id: "test-action".to_owned(),
            lane,
            command_kind,
            rollback: RollbackClass::Irreversible,
            summary: "test action".to_owned(),
            depends_on: Vec::new(),
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn host_prepare_phase_classification_is_narrow() {
        assert_eq!(
            classify_host_prepare_actions(&[phase_action(
                ExecutionLane::Root,
                LaneCommandKind::AptInstall,
            )]),
            Ok(HostPreparePhaseKind::Root)
        );
        assert_eq!(
            classify_host_prepare_actions(&[phase_action(
                ExecutionLane::RunnerUser,
                LaneCommandKind::RunnerPodmanMigrate,
            )]),
            Ok(HostPreparePhaseKind::RunnerUserMigration)
        );
        assert!(
            classify_host_prepare_actions(&[phase_action(
                ExecutionLane::RunnerUser,
                LaneCommandKind::RunnerGitVersion,
            )])
            .is_err()
        );
        assert!(
            classify_host_prepare_actions(&[
                phase_action(ExecutionLane::Root, LaneCommandKind::AptInstall),
                phase_action(
                    ExecutionLane::RunnerUser,
                    LaneCommandKind::RunnerPodmanMigrate,
                ),
            ])
            .is_err()
        );
        assert!(classify_host_prepare_actions(&[]).is_err());
    }
}
