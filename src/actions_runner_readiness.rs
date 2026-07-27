use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::artifact::Sha256Digest;
use crate::lima_observation::{
    LimaFilesystemObjectIdentity, LimaGuestObservation, LimaInstanceName,
    LimaInstanceObservationReport, LimaObservationClock, LimaObservationFreshness,
    LimaObservationTiming, LimaRuntimeState,
};
use crate::process::{CommandExecutor, CommandSpec, ExecutionRecord};

pub const ACTIONS_RUNNER_READINESS_SCHEMA_VERSION: u8 = 1;
pub const MAX_ACTIONS_RUNNER_OUTPUT_BYTES: usize = 65_536;
const MAX_RUNNER_NAME_BYTES: usize = 128;
const MAX_PATH_BYTES: usize = 1_024;
const MAX_PROCESS_MATCHES: usize = 2;
const GUEST_STAT: &str = "/usr/bin/stat";
const GUEST_SHA256SUM: &str = "/usr/bin/sha256sum";
const GUEST_TEST: &str = "/usr/bin/test";
const GUEST_PGREP: &str = "/usr/bin/pgrep";
const GUEST_READLINK: &str = "/usr/bin/readlink";
const LISTENER_NAME: &str = "Runner.Listener";
const WORKER_NAME: &str = "Runner.Worker";
const REDACTED: &str = "<private-actions-runner-evidence>";
const PROCESS_REDACTED_PATH: &str = "[REDACTED]";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionsRunnerReadinessState {
    Offline,
    Starting,
    IdleReady,
    Busy,
    Draining,
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionsRunnerReadinessRefusalCode {
    InvalidInput,
    SourceInstanceMismatch,
    SourceGuestMismatch,
    SourceUnavailable,
    ClockFailure,
    CommandFailed,
    CommandIdentityMismatch,
    UnboundedOutput,
    MissingIdentityEvidence,
    MalformedIdentityEvidence,
    ConfigurationIdentityMismatch,
    AmbiguousListener,
    AmbiguousWorker,
    ProcessIdentityMismatch,
    ProcessStateInconsistent,
    ProcessDrift,
    IdentityDrift,
    DrainStateDrift,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionsRunnerReadinessPhase {
    InputValidation,
    SourceObservation,
    RunnerRootIdentity,
    RunnerConfigurationIdentity,
    DrainMarker,
    ListenerDiscovery,
    WorkerDiscovery,
    ListenerIdentity,
    WorkerIdentity,
    FinalObservation,
    Freshness,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ObservationProblem {
    code: ActionsRunnerReadinessRefusalCode,
    phase: ActionsRunnerReadinessPhase,
    message: &'static str,
}

impl ObservationProblem {
    const fn new(
        code: ActionsRunnerReadinessRefusalCode,
        phase: ActionsRunnerReadinessPhase,
        message: &'static str,
    ) -> Self {
        Self {
            code,
            phase,
            message,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct ActionsRunnerReadinessFailure {
    pub code: ActionsRunnerReadinessRefusalCode,
    pub phase: ActionsRunnerReadinessPhase,
    pub public_message: &'static str,
    #[serde(skip)]
    private_evidence: ActionsRunnerReadinessPrivateEvidence,
}

impl ActionsRunnerReadinessFailure {
    fn from_problem(
        problem: ObservationProblem,
        private_evidence: ActionsRunnerReadinessPrivateEvidence,
    ) -> Self {
        Self {
            code: problem.code,
            phase: problem.phase,
            public_message: problem.message,
            private_evidence,
        }
    }

    #[must_use]
    pub const fn private_evidence(&self) -> &ActionsRunnerReadinessPrivateEvidence {
        &self.private_evidence
    }
}

impl fmt::Debug for ActionsRunnerReadinessFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ActionsRunnerReadinessFailure")
            .field("code", &self.code)
            .field("phase", &self.phase)
            .field("public_message", &self.public_message)
            .field("private_evidence", &REDACTED)
            .finish()
    }
}

impl fmt::Display for ActionsRunnerReadinessFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.public_message)
    }
}

impl std::error::Error for ActionsRunnerReadinessFailure {}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct ActionsRunnerName(String);

impl ActionsRunnerName {
    /// Validate one bounded configured official Actions runner name.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for empty, untrimmed, control-bearing, or oversized names.
    pub fn parse(value: &str) -> Result<Self, ActionsRunnerReadinessFailure> {
        if value.is_empty()
            || value.len() > MAX_RUNNER_NAME_BYTES
            || value.trim() != value
            || value.chars().any(char::is_control)
        {
            return Err(input_failure(
                "the configured Actions runner name is not one bounded canonical value",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ActionsRunnerReadinessRequest {
    instance: LimaInstanceName,
    runner_name: ActionsRunnerName,
    lima_home: PathBuf,
    runner_root: PathBuf,
    configuration_path: PathBuf,
    listener_path: PathBuf,
    worker_path: PathBuf,
    drain_marker_path: PathBuf,
    expected_configuration_digest: Sha256Digest,
}

impl ActionsRunnerReadinessRequest {
    /// Build one exact read-only observation request for a configured official runner.
    ///
    /// # Errors
    ///
    /// Returns a bounded error unless every private path is canonical, absolute, exact UTF-8, and
    /// the drain marker remains beneath the reviewed runner root.
    pub fn new(
        instance: LimaInstanceName,
        runner_name: ActionsRunnerName,
        lima_home: impl Into<PathBuf>,
        runner_root: impl Into<PathBuf>,
        drain_marker_path: impl Into<PathBuf>,
        expected_configuration_digest: Sha256Digest,
    ) -> Result<Self, ActionsRunnerReadinessFailure> {
        let lima_home = validate_private_path(lima_home.into(), false)?;
        let runner_root = validate_private_path(runner_root.into(), false)?;
        let drain_marker_path = validate_private_path(drain_marker_path.into(), false)?;
        if !strict_descendant(&drain_marker_path, &runner_root) {
            return Err(input_failure(
                "the reviewed drain marker must be beneath the exact runner root",
            ));
        }
        let configuration_path = runner_root.join(".runner");
        let listener_path = runner_root.join("bin/Runner.Listener");
        let worker_path = runner_root.join("bin/Runner.Worker");
        for path in [&configuration_path, &listener_path, &worker_path] {
            if !valid_absolute_path(path, false) {
                return Err(input_failure(
                    "derived official runner paths are not canonical absolute paths",
                ));
            }
        }
        Ok(Self {
            instance,
            runner_name,
            lima_home,
            runner_root,
            configuration_path,
            listener_path,
            worker_path,
            drain_marker_path,
            expected_configuration_digest,
        })
    }

    #[must_use]
    pub const fn instance(&self) -> &LimaInstanceName {
        &self.instance
    }

    #[must_use]
    pub const fn runner_name(&self) -> &ActionsRunnerName {
        &self.runner_name
    }
}

impl fmt::Debug for ActionsRunnerReadinessRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ActionsRunnerReadinessRequest")
            .field("instance", &self.instance)
            .field("runner_name", &self.runner_name)
            .field("lima_home", &"<private-lima-home>")
            .field("runner_root", &"<private-runner-root>")
            .field("configuration_path", &"<private-runner-configuration>")
            .field("listener_path", &"<private-listener-path>")
            .field("worker_path", &"<private-worker-path>")
            .field("drain_marker_path", &"<private-drain-marker>")
            .field(
                "expected_configuration_digest",
                &self.expected_configuration_digest,
            )
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActionsRunnerConfiguredIdentity {
    pub runner_name: ActionsRunnerName,
    pub configuration_digest: Sha256Digest,
    pub runner_root: LimaFilesystemObjectIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActionsRunnerReadinessReport {
    pub schema_version: u8,
    pub instance: LimaInstanceName,
    pub runner_name: ActionsRunnerName,
    pub state: ActionsRunnerReadinessState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configured_identity: Option<ActionsRunnerConfiguredIdentity>,
    pub timing: LimaObservationTiming,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct ActionsRunnerReadinessObservation {
    #[serde(flatten)]
    public: ActionsRunnerReadinessReport,
    #[serde(skip)]
    private_evidence: ActionsRunnerReadinessPrivateEvidence,
}

impl ActionsRunnerReadinessObservation {
    #[must_use]
    pub const fn report(&self) -> &ActionsRunnerReadinessReport {
        &self.public
    }

    #[must_use]
    pub const fn private_evidence(&self) -> &ActionsRunnerReadinessPrivateEvidence {
        &self.private_evidence
    }
}

impl fmt::Debug for ActionsRunnerReadinessObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ActionsRunnerReadinessObservation")
            .field("public", &self.public)
            .field("private_evidence", &REDACTED)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Default)]
pub struct ActionsRunnerReadinessPrivateEvidence {
    commands: Vec<ActionsRunnerPrivateCommandEvidence>,
}

impl ActionsRunnerReadinessPrivateEvidence {
    #[must_use]
    pub fn commands(&self) -> &[ActionsRunnerPrivateCommandEvidence] {
        &self.commands
    }
}

impl fmt::Debug for ActionsRunnerReadinessPrivateEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ActionsRunnerReadinessPrivateEvidence")
            .field("command_count", &self.commands.len())
            .field("raw_process_and_path_evidence", &REDACTED)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ActionsRunnerPrivateCommandEvidence {
    phase: ActionsRunnerReadinessPhase,
    record: ExecutionRecord,
}

impl ActionsRunnerPrivateCommandEvidence {
    #[must_use]
    pub const fn phase(&self) -> ActionsRunnerReadinessPhase {
        self.phase
    }

    #[must_use]
    pub const fn record(&self) -> &ExecutionRecord {
        &self.record
    }
}

impl fmt::Debug for ActionsRunnerPrivateCommandEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ActionsRunnerPrivateCommandEvidence")
            .field("phase", &self.phase)
            .field("record", &REDACTED)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
struct IdentitySnapshot {
    root: LimaFilesystemObjectIdentity,
    configuration_digest: Sha256Digest,
}

#[derive(Clone, PartialEq, Eq)]
struct ProcessIdentity {
    pid: u32,
    proc_object: LimaFilesystemObjectIdentity,
}

#[derive(Clone, PartialEq, Eq)]
struct ProcessSnapshot {
    listener: Option<ProcessIdentity>,
    worker: Option<ProcessIdentity>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ActionsRunnerReadinessAdapter {
    limactl_program: PathBuf,
}

impl ActionsRunnerReadinessAdapter {
    /// Bind one reviewed absolute `limactl` executable.
    ///
    /// # Errors
    ///
    /// Returns a bounded error unless the executable path is canonical and absolute.
    pub fn new(
        limactl_program: impl Into<PathBuf>,
    ) -> Result<Self, ActionsRunnerReadinessFailure> {
        let limactl_program = validate_private_path(limactl_program.into(), false)?;
        Ok(Self { limactl_program })
    }

    /// Observe one configured official Actions runner without registration or lifecycle mutation.
    ///
    /// # Errors
    ///
    /// Returns bounded typed failures for source, command, configured identity, process identity,
    /// output, or intra-observation drift problems.
    pub fn observe(
        &self,
        request: &ActionsRunnerReadinessRequest,
        source: &LimaInstanceObservationReport,
        executor: &impl CommandExecutor,
        clock: &impl LimaObservationClock,
    ) -> Result<ActionsRunnerReadinessObservation, ActionsRunnerReadinessFailure> {
        let mut evidence = ActionsRunnerReadinessPrivateEvidence::default();
        let result = self.observe_inner(request, source, executor, clock, &mut evidence);
        match result {
            Ok(public) => Ok(ActionsRunnerReadinessObservation {
                public,
                private_evidence: evidence,
            }),
            Err(problem) => Err(ActionsRunnerReadinessFailure::from_problem(
                problem, evidence,
            )),
        }
    }

    fn observe_inner(
        &self,
        request: &ActionsRunnerReadinessRequest,
        source: &LimaInstanceObservationReport,
        executor: &impl CommandExecutor,
        clock: &impl LimaObservationClock,
        evidence: &mut ActionsRunnerReadinessPrivateEvidence,
    ) -> Result<ActionsRunnerReadinessReport, ObservationProblem> {
        if source.instance != request.instance {
            return Err(ObservationProblem::new(
                ActionsRunnerReadinessRefusalCode::SourceInstanceMismatch,
                ActionsRunnerReadinessPhase::SourceObservation,
                "the runner-readiness request does not match the exact Lima instance observation",
            ));
        }
        let started_at = clock.unix_seconds().map_err(|_| clock_problem())?;
        let source_freshness = source.timing.freshness_at(started_at);
        if source_freshness != LimaObservationFreshness::Fresh {
            return Ok(report(
                request,
                ActionsRunnerReadinessState::Stale,
                None,
                timing(started_at, started_at, source, source_freshness)?,
            ));
        }

        match source.configured.runtime_state {
            LimaRuntimeState::Stopped => {
                return Ok(report(
                    request,
                    ActionsRunnerReadinessState::Offline,
                    None,
                    timing(
                        started_at,
                        started_at,
                        source,
                        LimaObservationFreshness::Fresh,
                    )?,
                ));
            }
            LimaRuntimeState::Uninitialized | LimaRuntimeState::Installing => {
                return Ok(report(
                    request,
                    ActionsRunnerReadinessState::Starting,
                    None,
                    timing(
                        started_at,
                        started_at,
                        source,
                        LimaObservationFreshness::Fresh,
                    )?,
                ));
            }
            LimaRuntimeState::Broken => {
                return Err(ObservationProblem::new(
                    ActionsRunnerReadinessRefusalCode::SourceUnavailable,
                    ActionsRunnerReadinessPhase::SourceObservation,
                    "the exact Lima source observation reports an unavailable instance",
                ));
            }
            LimaRuntimeState::Running => {}
        }
        if !matches!(&source.guest, LimaGuestObservation::Observed(_)) {
            return Err(ObservationProblem::new(
                ActionsRunnerReadinessRefusalCode::SourceGuestMismatch,
                ActionsRunnerReadinessPhase::SourceObservation,
                "the running Lima source observation lacks matching guest evidence",
            ));
        }

        let initial_identity = self.observe_identity(request, executor, evidence)?;
        let initial_draining = self.observe_drain_marker(request, executor, evidence)?;
        let initial_processes = self.observe_processes(request, executor, evidence, false)?;

        let final_processes = self.observe_processes(request, executor, evidence, true)?;
        if final_processes != initial_processes {
            return Err(ObservationProblem::new(
                ActionsRunnerReadinessRefusalCode::ProcessDrift,
                ActionsRunnerReadinessPhase::FinalObservation,
                "the official runner process identity changed during observation",
            ));
        }
        let final_identity = self.observe_identity(request, executor, evidence)?;
        if final_identity != initial_identity {
            return Err(ObservationProblem::new(
                ActionsRunnerReadinessRefusalCode::IdentityDrift,
                ActionsRunnerReadinessPhase::FinalObservation,
                "the configured official runner identity changed during observation",
            ));
        }
        let final_draining = self.observe_drain_marker(request, executor, evidence)?;
        if final_draining != initial_draining {
            return Err(ObservationProblem::new(
                ActionsRunnerReadinessRefusalCode::DrainStateDrift,
                ActionsRunnerReadinessPhase::FinalObservation,
                "the reviewed runner drain marker changed during observation",
            ));
        }

        let state = classify_state(&initial_processes, initial_draining)?;
        let observed_at = clock.unix_seconds().map_err(|_| clock_problem())?;
        let freshness = source.timing.freshness_at(observed_at);
        if freshness != LimaObservationFreshness::Fresh {
            return Ok(report(
                request,
                ActionsRunnerReadinessState::Stale,
                None,
                timing(started_at, observed_at, source, freshness)?,
            ));
        }
        let configured_identity = Some(ActionsRunnerConfiguredIdentity {
            runner_name: request.runner_name.clone(),
            configuration_digest: initial_identity.configuration_digest,
            runner_root: initial_identity.root,
        });
        Ok(report(
            request,
            state,
            configured_identity,
            timing(started_at, observed_at, source, freshness)?,
        ))
    }

    fn observe_identity(
        &self,
        request: &ActionsRunnerReadinessRequest,
        executor: &impl CommandExecutor,
        evidence: &mut ActionsRunnerReadinessPrivateEvidence,
    ) -> Result<IdentitySnapshot, ObservationProblem> {
        let root = parse_filesystem_identity(
            &self.run_success(
                request,
                executor,
                evidence,
                ActionsRunnerReadinessPhase::RunnerRootIdentity,
                self.guest_private_path_command(
                    request,
                    GUEST_STAT,
                    ["-Lc", "%d:%i", "--"],
                    &request.runner_root,
                ),
            )?,
            ActionsRunnerReadinessPhase::RunnerRootIdentity,
        )?;
        let configuration_digest = parse_private_sha256(
            &self.run_success(
                request,
                executor,
                evidence,
                ActionsRunnerReadinessPhase::RunnerConfigurationIdentity,
                self.guest_private_path_command(
                    request,
                    GUEST_SHA256SUM,
                    ["--"],
                    &request.configuration_path,
                ),
            )?,
        )?;
        if configuration_digest != request.expected_configuration_digest {
            return Err(ObservationProblem::new(
                ActionsRunnerReadinessRefusalCode::ConfigurationIdentityMismatch,
                ActionsRunnerReadinessPhase::RunnerConfigurationIdentity,
                "the official runner configuration digest differs from the reviewed identity",
            ));
        }
        Ok(IdentitySnapshot {
            root,
            configuration_digest,
        })
    }

    fn observe_drain_marker(
        &self,
        request: &ActionsRunnerReadinessRequest,
        executor: &impl CommandExecutor,
        evidence: &mut ActionsRunnerReadinessPrivateEvidence,
    ) -> Result<bool, ObservationProblem> {
        let record = self.execute_record(
            executor,
            evidence,
            ActionsRunnerReadinessPhase::DrainMarker,
            self.guest_private_path_command(
                request,
                GUEST_TEST,
                ["-e"],
                &request.drain_marker_path,
            ),
        )?;
        if !record.stdout.is_empty() || !record.stderr.is_empty() {
            return Err(malformed_identity(
                ActionsRunnerReadinessPhase::DrainMarker,
                "the reviewed drain marker probe returned unexpected output",
            ));
        }
        match (record.status, record.success) {
            (Some(0), true) => Ok(true),
            (Some(1), false) => Ok(false),
            _ => Err(command_failed(
                ActionsRunnerReadinessPhase::DrainMarker,
                "the reviewed drain marker probe did not complete cleanly",
            )),
        }
    }

    fn observe_processes(
        &self,
        request: &ActionsRunnerReadinessRequest,
        executor: &impl CommandExecutor,
        evidence: &mut ActionsRunnerReadinessPrivateEvidence,
        final_observation: bool,
    ) -> Result<ProcessSnapshot, ObservationProblem> {
        let listener_phase = if final_observation {
            ActionsRunnerReadinessPhase::FinalObservation
        } else {
            ActionsRunnerReadinessPhase::ListenerDiscovery
        };
        let worker_phase = if final_observation {
            ActionsRunnerReadinessPhase::FinalObservation
        } else {
            ActionsRunnerReadinessPhase::WorkerDiscovery
        };
        let listener_pids = self.observe_named_processes(
            request,
            executor,
            evidence,
            listener_phase,
            LISTENER_NAME,
        )?;
        let worker_pids = self.observe_named_processes(
            request,
            executor,
            evidence,
            worker_phase,
            WORKER_NAME,
        )?;
        if listener_pids.len() > 1 {
            return Err(ObservationProblem::new(
                ActionsRunnerReadinessRefusalCode::AmbiguousListener,
                listener_phase,
                "more than one official runner listener matched the reviewed identity",
            ));
        }
        if worker_pids.len() > 1 {
            return Err(ObservationProblem::new(
                ActionsRunnerReadinessRefusalCode::AmbiguousWorker,
                worker_phase,
                "more than one official runner worker matched the reviewed identity",
            ));
        }
        let listener = listener_pids
            .first()
            .copied()
            .map(|pid| {
                self.verify_process(
                    request,
                    executor,
                    evidence,
                    ActionsRunnerReadinessPhase::ListenerIdentity,
                    pid,
                    &request.listener_path,
                )
            })
            .transpose()?;
        let worker = worker_pids
            .first()
            .copied()
            .map(|pid| {
                self.verify_process(
                    request,
                    executor,
                    evidence,
                    ActionsRunnerReadinessPhase::WorkerIdentity,
                    pid,
                    &request.worker_path,
                )
            })
            .transpose()?;
        Ok(ProcessSnapshot { listener, worker })
    }

    fn observe_named_processes(
        &self,
        request: &ActionsRunnerReadinessRequest,
        executor: &impl CommandExecutor,
        evidence: &mut ActionsRunnerReadinessPrivateEvidence,
        phase: ActionsRunnerReadinessPhase,
        process_name: &str,
    ) -> Result<Vec<u32>, ObservationProblem> {
        let record = self.execute_record(
            executor,
            evidence,
            phase,
            self.guest_plain_command(request, GUEST_PGREP, ["-x", process_name]),
        )?;
        match (record.status, record.success) {
            (Some(1), false) if record.stdout.is_empty() && record.stderr.is_empty() => {
                return Ok(Vec::new());
            }
            (Some(0), true) if record.stderr.is_empty() => {}
            _ => {
                return Err(command_failed(
                    phase,
                    "the exact official runner process query did not complete cleanly",
                ));
            }
        }
        parse_pid_lines(&record.stdout, phase)
    }

    fn verify_process(
        &self,
        request: &ActionsRunnerReadinessRequest,
        executor: &impl CommandExecutor,
        evidence: &mut ActionsRunnerReadinessPrivateEvidence,
        phase: ActionsRunnerReadinessPhase,
        pid: u32,
        expected_executable: &Path,
    ) -> Result<ProcessIdentity, ObservationProblem> {
        let proc_root = format!("/proc/{pid}");
        let proc_exe = format!("{proc_root}/exe");
        let proc_cwd = format!("{proc_root}/cwd");
        let executable = parse_single_line(
            &self.run_success(
                request,
                executor,
                evidence,
                phase,
                self.guest_plain_command(
                    request,
                    GUEST_READLINK,
                    ["-e", "--", proc_exe.as_str()],
                ),
            )?,
            phase,
        )?;
        if Path::new(executable) != expected_executable {
            return Err(ObservationProblem::new(
                ActionsRunnerReadinessRefusalCode::ProcessIdentityMismatch,
                phase,
                "the official runner process executable differs from the reviewed identity",
            ));
        }
        let cwd = parse_single_line(
            &self.run_success(
                request,
                executor,
                evidence,
                phase,
                self.guest_plain_command(
                    request,
                    GUEST_READLINK,
                    ["-e", "--", proc_cwd.as_str()],
                ),
            )?,
            phase,
        )?;
        if Path::new(cwd) != request.runner_root {
            return Err(ObservationProblem::new(
                ActionsRunnerReadinessRefusalCode::ProcessIdentityMismatch,
                phase,
                "the official runner process working directory differs from the reviewed root",
            ));
        }
        let proc_object = parse_filesystem_identity(
            &self.run_success(
                request,
                executor,
                evidence,
                phase,
                self.guest_plain_command(
                    request,
                    GUEST_STAT,
                    ["-Lc", "%d:%i", "--", proc_root.as_str()],
                ),
            )?,
            phase,
        )?;
        Ok(ProcessIdentity { pid, proc_object })
    }

    fn run_success(
        &self,
        _request: &ActionsRunnerReadinessRequest,
        executor: &impl CommandExecutor,
        evidence: &mut ActionsRunnerReadinessPrivateEvidence,
        phase: ActionsRunnerReadinessPhase,
        command: CommandSpec,
    ) -> Result<String, ObservationProblem> {
        let record = self.execute_record(executor, evidence, phase, command)?;
        if record.status != Some(0) || !record.success || !record.stderr.is_empty() {
            return Err(command_failed(
                phase,
                "the reviewed official runner observation command did not complete cleanly",
            ));
        }
        Ok(record.stdout)
    }

    fn execute_record(
        &self,
        executor: &impl CommandExecutor,
        evidence: &mut ActionsRunnerReadinessPrivateEvidence,
        phase: ActionsRunnerReadinessPhase,
        command: CommandSpec,
    ) -> Result<ExecutionRecord, ObservationProblem> {
        let record = executor.execute(&command).map_err(|_| {
            command_failed(
                phase,
                "the reviewed official runner observation command could not be executed",
            )
        })?;
        evidence.commands.push(ActionsRunnerPrivateCommandEvidence {
            phase,
            record: record.clone(),
        });
        if record.argv != command.displayed_argv()
            || record.environment_keys != command.environment.keys().cloned().collect::<Vec<_>>()
        {
            return Err(ObservationProblem::new(
                ActionsRunnerReadinessRefusalCode::CommandIdentityMismatch,
                phase,
                "the subprocess record does not match the reviewed runner observation command",
            ));
        }
        if record.stdout.len() > MAX_ACTIONS_RUNNER_OUTPUT_BYTES
            || record.stderr.len() > MAX_ACTIONS_RUNNER_OUTPUT_BYTES
        {
            return Err(ObservationProblem::new(
                ActionsRunnerReadinessRefusalCode::UnboundedOutput,
                phase,
                "the official runner observation output exceeded the reviewed bound",
            ));
        }
        if record
            .stdout
            .chars()
            .chain(record.stderr.chars())
            .any(|character| matches!(character, '\0' | '\u{fffd}'))
        {
            return Err(malformed_identity(
                phase,
                "the official runner observation returned malformed text evidence",
            ));
        }
        Ok(record)
    }

    fn base_command(&self, request: &ActionsRunnerReadinessRequest) -> CommandSpec {
        CommandSpec::new(&self.limactl_program)
            .environment("LIMA_HOME", exact_path(&request.lima_home))
            .environment("LANG", "C")
            .environment("LC_ALL", "C")
    }

    fn guest_plain_command<const N: usize>(
        &self,
        request: &ActionsRunnerReadinessRequest,
        program: &str,
        arguments: [&str; N],
    ) -> CommandSpec {
        let mut command = self
            .base_command(request)
            .argument("--tty=false")
            .argument("shell")
            .argument(request.instance.as_str())
            .argument("--")
            .argument(program);
        for argument in arguments {
            command = command.argument(argument);
        }
        command
    }

    fn guest_private_path_command<const N: usize>(
        &self,
        request: &ActionsRunnerReadinessRequest,
        program: &str,
        arguments: [&str; N],
        path: &Path,
    ) -> CommandSpec {
        let mut command = self.guest_plain_command(request, program, arguments);
        command = command.secret_argument(exact_path(path));
        command
    }
}

impl fmt::Debug for ActionsRunnerReadinessAdapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ActionsRunnerReadinessAdapter")
            .field("limactl_program", &"<reviewed-absolute-limactl>")
            .finish()
    }
}

fn classify_state(
    processes: &ProcessSnapshot,
    draining: bool,
) -> Result<ActionsRunnerReadinessState, ObservationProblem> {
    match (
        draining,
        processes.listener.is_some(),
        processes.worker.is_some(),
    ) {
        (true, true, _) => Ok(ActionsRunnerReadinessState::Draining),
        (false, false, false) => Ok(ActionsRunnerReadinessState::Starting),
        (false, true, false) => Ok(ActionsRunnerReadinessState::IdleReady),
        (false, true, true) => Ok(ActionsRunnerReadinessState::Busy),
        _ => Err(ObservationProblem::new(
            ActionsRunnerReadinessRefusalCode::ProcessStateInconsistent,
            ActionsRunnerReadinessPhase::FinalObservation,
            "the official runner listener, worker, and drain evidence are inconsistent",
        )),
    }
}

fn report(
    request: &ActionsRunnerReadinessRequest,
    state: ActionsRunnerReadinessState,
    configured_identity: Option<ActionsRunnerConfiguredIdentity>,
    timing: LimaObservationTiming,
) -> ActionsRunnerReadinessReport {
    ActionsRunnerReadinessReport {
        schema_version: ACTIONS_RUNNER_READINESS_SCHEMA_VERSION,
        instance: request.instance.clone(),
        runner_name: request.runner_name.clone(),
        state,
        configured_identity,
        timing,
    }
}

fn timing(
    started_at: u64,
    observed_at: u64,
    source: &LimaInstanceObservationReport,
    freshness: LimaObservationFreshness,
) -> Result<LimaObservationTiming, ObservationProblem> {
    let duration_seconds = observed_at.checked_sub(started_at).ok_or_else(clock_problem)?;
    Ok(LimaObservationTiming {
        started_at_unix_seconds: started_at,
        observed_at_unix_seconds: observed_at,
        expires_at_unix_seconds: source.timing.expires_at_unix_seconds,
        duration_seconds,
        freshness,
    })
}

fn parse_pid_lines(
    output: &str,
    phase: ActionsRunnerReadinessPhase,
) -> Result<Vec<u32>, ObservationProblem> {
    let Some(body) = output.strip_suffix('\n') else {
        return Err(malformed_identity(
            phase,
            "the official runner PID evidence is not newline terminated",
        ));
    };
    if body.is_empty() || body.contains('\r') {
        return Err(malformed_identity(
            phase,
            "the official runner PID evidence is malformed",
        ));
    }
    let mut pids = Vec::new();
    for line in body.split('\n') {
        if pids.len() >= MAX_PROCESS_MATCHES {
            return Err(malformed_identity(
                phase,
                "the official runner PID evidence exceeded the reviewed match bound",
            ));
        }
        let pid = parse_canonical_u64(line)
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value > 0)
            .ok_or_else(|| {
                malformed_identity(phase, "the official runner PID evidence is not canonical")
            })?;
        if pids.contains(&pid) {
            return Err(malformed_identity(
                phase,
                "the official runner PID evidence contains a duplicate process",
            ));
        }
        pids.push(pid);
    }
    pids.sort_unstable();
    Ok(pids)
}

fn parse_single_line(
    output: &str,
    phase: ActionsRunnerReadinessPhase,
) -> Result<&str, ObservationProblem> {
    let Some(value) = output.strip_suffix('\n') else {
        return Err(malformed_identity(
            phase,
            "the official runner evidence is not one complete line",
        ));
    };
    if value.is_empty() || value.chars().any(|character| matches!(character, '\n' | '\r' | '\0'))
    {
        return Err(malformed_identity(
            phase,
            "the official runner evidence is not one canonical line",
        ));
    }
    Ok(value)
}

fn parse_filesystem_identity(
    output: &str,
    phase: ActionsRunnerReadinessPhase,
) -> Result<LimaFilesystemObjectIdentity, ObservationProblem> {
    let value = parse_single_line(output, phase)?;
    let Some((device, inode)) = value.split_once(':') else {
        return Err(malformed_identity(
            phase,
            "the official runner filesystem identity is malformed",
        ));
    };
    if inode.contains(':') {
        return Err(malformed_identity(
            phase,
            "the official runner filesystem identity is malformed",
        ));
    }
    let device_id = parse_canonical_u64(device).ok_or_else(|| {
        malformed_identity(phase, "the official runner device identity is malformed")
    })?;
    let inode = parse_canonical_u64(inode).filter(|value| *value > 0).ok_or_else(|| {
        malformed_identity(phase, "the official runner inode identity is malformed")
    })?;
    Ok(LimaFilesystemObjectIdentity { device_id, inode })
}

fn parse_private_sha256(output: &str) -> Result<Sha256Digest, ObservationProblem> {
    let value = parse_single_line(
        output,
        ActionsRunnerReadinessPhase::RunnerConfigurationIdentity,
    )?;
    let Some((digest, path)) = value.split_once("  ") else {
        return Err(malformed_identity(
            ActionsRunnerReadinessPhase::RunnerConfigurationIdentity,
            "the official runner configuration digest is malformed",
        ));
    };
    if path != PROCESS_REDACTED_PATH
        || digest.len() != 64
        || !digest.bytes().all(is_lower_hex)
    {
        return Err(malformed_identity(
            ActionsRunnerReadinessPhase::RunnerConfigurationIdentity,
            "the official runner configuration digest is malformed",
        ));
    }
    Sha256Digest::parse(&format!("sha256:{digest}")).map_err(|_| {
        malformed_identity(
            ActionsRunnerReadinessPhase::RunnerConfigurationIdentity,
            "the official runner configuration digest is malformed",
        )
    })
}

fn parse_canonical_u64(value: &str) -> Option<u64> {
    if value.is_empty()
        || value.len() > 20
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || value.len() > 1 && value.starts_with('0')
    {
        return None;
    }
    value.parse().ok()
}

const fn is_lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')
}

fn validate_private_path(
    path: PathBuf,
    allow_root: bool,
) -> Result<PathBuf, ActionsRunnerReadinessFailure> {
    if !valid_absolute_path(&path, allow_root) {
        return Err(input_failure(
            "reviewed runner paths must be bounded canonical absolute UTF-8 paths",
        ));
    }
    Ok(path)
}

fn valid_absolute_path(path: &Path, allow_root: bool) -> bool {
    let Some(value) = path.to_str() else {
        return false;
    };
    if !path.is_absolute()
        || value.len() > MAX_PATH_BYTES
        || value.bytes().any(|byte| byte.is_ascii_control())
        || value.ends_with('/') && value != "/"
        || value.get(1..).is_some_and(|rest| rest.contains("//"))
        || value
            .split('/')
            .any(|component| matches!(component, "." | ".."))
        || !allow_root && value == "/"
    {
        return false;
    }
    path.components()
        .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

fn strict_descendant(path: &Path, root: &Path) -> bool {
    path != root && path.starts_with(root)
}

fn exact_path(path: &Path) -> &str {
    path.to_str()
        .expect("reviewed official runner paths are validated UTF-8")
}

fn input_failure(message: &'static str) -> ActionsRunnerReadinessFailure {
    ActionsRunnerReadinessFailure::from_problem(
        ObservationProblem::new(
            ActionsRunnerReadinessRefusalCode::InvalidInput,
            ActionsRunnerReadinessPhase::InputValidation,
            message,
        ),
        ActionsRunnerReadinessPrivateEvidence::default(),
    )
}

const fn command_failed(
    phase: ActionsRunnerReadinessPhase,
    message: &'static str,
) -> ObservationProblem {
    ObservationProblem::new(
        ActionsRunnerReadinessRefusalCode::CommandFailed,
        phase,
        message,
    )
}

const fn malformed_identity(
    phase: ActionsRunnerReadinessPhase,
    message: &'static str,
) -> ObservationProblem {
    ObservationProblem::new(
        ActionsRunnerReadinessRefusalCode::MalformedIdentityEvidence,
        phase,
        message,
    )
}

const fn clock_problem() -> ObservationProblem {
    ObservationProblem::new(
        ActionsRunnerReadinessRefusalCode::ClockFailure,
        ActionsRunnerReadinessPhase::Freshness,
        "the runner-readiness observation clock is unavailable or reversed",
    )
}

#[cfg(test)]
mod tests;
