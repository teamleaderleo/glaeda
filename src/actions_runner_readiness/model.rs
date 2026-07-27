use std::fmt;
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
