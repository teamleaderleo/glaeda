use std::fmt;

use serde::Serialize;

use crate::actions_runner_readiness::{
    ACTIONS_RUNNER_READINESS_SCHEMA_VERSION, ActionsRunnerReadinessReport,
    ActionsRunnerReadinessState,
};
use crate::lima_observation::{
    LIMA_OBSERVATION_SCHEMA_VERSION, LimaGuestObservation, LimaInstanceObservationReport,
    LimaObservationFreshness, LimaObservationTiming, LimaRuntimeState,
};
use crate::operator_config::OperatorConfig;
use crate::operator_error::{OperatorErrorCode, OperatorPublicError};
use crate::operator_status::{
    OperatorActiveJobSummary, OperatorConfigurationCompatibility, OperatorConfigurationStatus,
    OperatorMachineSummary, OperatorStatusReport, OperatorTerminalResult, OperatorTerminalSummary,
    OperatorWorkerSummary,
};
use crate::personal_worker_operator_read::{
    PersonalWorkerOperatorJobRead, PersonalWorkerOperatorStatusRead,
};

pub const OPERATOR_STATUS_SERVICE_SCHEMA_VERSION: u8 = 1;
const REDACTED_EVIDENCE: &str = "<validated-operator-status-evidence>";

#[derive(Clone, PartialEq, Eq)]
pub struct OperatorStatusTerminalEvidence {
    job: PersonalWorkerOperatorJobRead,
    result: OperatorTerminalResult,
}

impl OperatorStatusTerminalEvidence {
    #[must_use]
    pub const fn new(job: PersonalWorkerOperatorJobRead, result: OperatorTerminalResult) -> Self {
        Self { job, result }
    }

    #[must_use]
    pub const fn job(&self) -> &PersonalWorkerOperatorJobRead {
        &self.job
    }

    #[must_use]
    pub const fn result(&self) -> OperatorTerminalResult {
        self.result
    }
}

impl fmt::Debug for OperatorStatusTerminalEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OperatorStatusTerminalEvidence")
            .field("config_identity", self.job.config_identity())
            .field("job", &REDACTED_EVIDENCE)
            .field("result", &self.result)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct OperatorStatusWorkerEvidence {
    status: PersonalWorkerOperatorStatusRead,
    active_job: Option<PersonalWorkerOperatorJobRead>,
    latest_terminal: Option<OperatorStatusTerminalEvidence>,
}

impl OperatorStatusWorkerEvidence {
    #[must_use]
    pub const fn new(
        status: PersonalWorkerOperatorStatusRead,
        active_job: Option<PersonalWorkerOperatorJobRead>,
        latest_terminal: Option<OperatorStatusTerminalEvidence>,
    ) -> Self {
        Self {
            status,
            active_job,
            latest_terminal,
        }
    }

    #[must_use]
    pub const fn status(&self) -> &PersonalWorkerOperatorStatusRead {
        &self.status
    }

    #[must_use]
    pub const fn active_job(&self) -> Option<&PersonalWorkerOperatorJobRead> {
        self.active_job.as_ref()
    }

    #[must_use]
    pub const fn latest_terminal(&self) -> Option<&OperatorStatusTerminalEvidence> {
        self.latest_terminal.as_ref()
    }
}

impl fmt::Debug for OperatorStatusWorkerEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OperatorStatusWorkerEvidence")
            .field("config_identity", self.status.config_identity())
            .field("status", &REDACTED_EVIDENCE)
            .field("has_active_job", &self.active_job.is_some())
            .field("has_latest_terminal", &self.latest_terminal.is_some())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct OperatorStatusServiceEvidence {
    config: OperatorConfig,
    compatibility: OperatorConfigurationCompatibility,
    worker: OperatorStatusWorkerEvidence,
    lima: LimaInstanceObservationReport,
    runner: ActionsRunnerReadinessReport,
    observed_at_unix_seconds: u64,
    blockers: Vec<OperatorPublicError>,
}

impl OperatorStatusServiceEvidence {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        config: OperatorConfig,
        compatibility: OperatorConfigurationCompatibility,
        worker: OperatorStatusWorkerEvidence,
        lima: LimaInstanceObservationReport,
        runner: ActionsRunnerReadinessReport,
        observed_at_unix_seconds: u64,
        blockers: Vec<OperatorPublicError>,
    ) -> Self {
        Self {
            config,
            compatibility,
            worker,
            lima,
            runner,
            observed_at_unix_seconds,
            blockers,
        }
    }

    #[must_use]
    pub const fn config(&self) -> &OperatorConfig {
        &self.config
    }

    #[must_use]
    pub const fn worker(&self) -> &OperatorStatusWorkerEvidence {
        &self.worker
    }

    #[must_use]
    pub const fn lima(&self) -> &LimaInstanceObservationReport {
        &self.lima
    }

    #[must_use]
    pub const fn runner(&self) -> &ActionsRunnerReadinessReport {
        &self.runner
    }

    #[must_use]
    pub const fn observed_at_unix_seconds(&self) -> u64 {
        self.observed_at_unix_seconds
    }
}

impl fmt::Debug for OperatorStatusServiceEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OperatorStatusServiceEvidence")
            .field("schema_version", &OPERATOR_STATUS_SERVICE_SCHEMA_VERSION)
            .field("config_identity", self.config.identity())
            .field("compatibility", &self.compatibility)
            .field("worker", &self.worker)
            .field("lima_instance", &self.lima.instance)
            .field("lima_state", &self.lima.configured.runtime_state)
            .field("runner_state", &self.runner.state)
            .field("observed_at_unix_seconds", &self.observed_at_unix_seconds)
            .field("blocker_count", &self.blockers.len())
            .finish()
    }
}

pub trait OperatorStatusEvidenceReader {
    /// Return one complete, already-observed operator status evidence bundle.
    ///
    /// # Errors
    ///
    /// Returns the exact bounded public failure produced while acquiring the bundle.
    fn read_evidence(&mut self) -> Result<OperatorStatusServiceEvidence, OperatorPublicError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorStatusServiceErrorKind {
    EvidenceUnavailable,
    ConfigurationMismatch,
    StaleRevision,
    StaleGeneration,
    LimaIdentityMismatch,
    RunnerIdentityMismatch,
    InvalidTiming,
    InvalidActiveJob,
    InvalidTerminal,
    InvalidStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct OperatorStatusServiceError {
    kind: OperatorStatusServiceErrorKind,
    public_error: OperatorPublicError,
}

impl OperatorStatusServiceError {
    const fn new(kind: OperatorStatusServiceErrorKind, code: OperatorErrorCode) -> Self {
        Self {
            kind,
            public_error: OperatorPublicError::from_code(code),
        }
    }

    const fn from_reader(public_error: OperatorPublicError) -> Self {
        Self {
            kind: OperatorStatusServiceErrorKind::EvidenceUnavailable,
            public_error,
        }
    }

    #[must_use]
    pub const fn kind(self) -> OperatorStatusServiceErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn public_error(self) -> OperatorPublicError {
        self.public_error
    }
}

impl fmt::Display for OperatorStatusServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.public_error.fmt(formatter)
    }
}

impl std::error::Error for OperatorStatusServiceError {}

pub struct OperatorStatusService;

impl OperatorStatusService {
    /// Read one complete bundle exactly once and compose a unified status report.
    ///
    /// # Errors
    ///
    /// Returns a closed, bounded failure when evidence acquisition fails or the bundle does not
    /// prove one coherent configuration, durable snapshot, machine identity, and time boundary.
    pub fn read(
        reader: &mut impl OperatorStatusEvidenceReader,
    ) -> Result<OperatorStatusReport, OperatorStatusServiceError> {
        let evidence = reader
            .read_evidence()
            .map_err(OperatorStatusServiceError::from_reader)?;
        Self::compose(evidence)
    }

    /// Compose one unified report without performing observation or mutation.
    ///
    /// # Errors
    ///
    /// Returns a closed, bounded failure unless all injected evidence belongs to one coherent
    /// configuration, durable snapshot, machine identity, and time boundary.
    pub fn compose(
        evidence: OperatorStatusServiceEvidence,
    ) -> Result<OperatorStatusReport, OperatorStatusServiceError> {
        validate_configuration_identity(&evidence)?;
        validate_job_snapshots(&evidence)?;
        validate_job_shape(&evidence)?;
        validate_machine_identity(&evidence)?;
        validate_timing(&evidence)?;

        let worker = OperatorWorkerSummary::from_status(evidence.worker.status().view());
        let active_job = evidence
            .worker
            .active_job()
            .map(|job| OperatorActiveJobSummary::from_job_view(job.view()))
            .transpose()
            .map_err(|_| invalid_active_job())?;
        let latest_terminal = project_terminal(&evidence)?;

        let mut blockers = evidence.blockers;
        if evidence.compatibility == OperatorConfigurationCompatibility::Incompatible {
            blockers.push(OperatorPublicError::from_code(
                OperatorErrorCode::ConfigurationIncompatible,
            ));
        }
        if evidence.lima.configured.runtime_state == LimaRuntimeState::Broken {
            blockers.push(OperatorPublicError::from_code(
                OperatorErrorCode::LimaBroken,
            ));
        }
        if evidence
            .lima
            .timing
            .freshness_at(evidence.observed_at_unix_seconds)
            == LimaObservationFreshness::Stale
        {
            blockers.push(OperatorPublicError::from_code(
                OperatorErrorCode::LimaObservationStale,
            ));
        }
        if evidence.runner.state == ActionsRunnerReadinessState::Stale
            || evidence
                .runner
                .timing
                .freshness_at(evidence.observed_at_unix_seconds)
                == LimaObservationFreshness::Stale
        {
            blockers.push(OperatorPublicError::from_code(
                OperatorErrorCode::RunnerObservationStale,
            ));
        }

        OperatorStatusReport::new(
            OperatorConfigurationStatus::new(
                evidence.config.identity().clone(),
                evidence.compatibility,
            ),
            worker,
            OperatorMachineSummary::new(
                evidence.lima.configured.runtime_state,
                evidence.runner.state,
            ),
            active_job,
            latest_terminal,
            blockers,
        )
        .map_err(|_| {
            OperatorStatusServiceError::new(
                OperatorStatusServiceErrorKind::InvalidStatus,
                OperatorErrorCode::ServiceUnavailable,
            )
        })
    }
}

fn validate_configuration_identity(
    evidence: &OperatorStatusServiceEvidence,
) -> Result<(), OperatorStatusServiceError> {
    let expected = evidence.config.identity();
    if evidence.worker.status().config_identity() != expected
        || evidence
            .worker
            .active_job()
            .is_some_and(|job| job.config_identity() != expected)
        || evidence
            .worker
            .latest_terminal()
            .is_some_and(|terminal| terminal.job().config_identity() != expected)
    {
        return Err(OperatorStatusServiceError::new(
            OperatorStatusServiceErrorKind::ConfigurationMismatch,
            OperatorErrorCode::ConfigurationIncompatible,
        ));
    }
    Ok(())
}

fn validate_job_snapshots(
    evidence: &OperatorStatusServiceEvidence,
) -> Result<(), OperatorStatusServiceError> {
    let status = evidence.worker.status().view();
    for job in worker_job_reads(&evidence.worker) {
        if job.view().store_revision() != status.store_revision() {
            return Err(OperatorStatusServiceError::new(
                OperatorStatusServiceErrorKind::StaleRevision,
                OperatorErrorCode::DurableStateRevisionStale,
            ));
        }
    }
    for job in worker_job_reads(&evidence.worker) {
        if job.view().queue_generation() != status.queue_generation() {
            return Err(OperatorStatusServiceError::new(
                OperatorStatusServiceErrorKind::StaleGeneration,
                OperatorErrorCode::DurableStateGenerationStale,
            ));
        }
    }
    Ok(())
}

fn worker_job_reads(
    worker: &OperatorStatusWorkerEvidence,
) -> impl Iterator<Item = &PersonalWorkerOperatorJobRead> {
    worker.active_job().into_iter().chain(
        worker
            .latest_terminal()
            .map(OperatorStatusTerminalEvidence::job),
    )
}

fn validate_job_shape(
    evidence: &OperatorStatusServiceEvidence,
) -> Result<(), OperatorStatusServiceError> {
    if (evidence.worker.status().view().active_count() == 0)
        != evidence.worker.active_job().is_none()
    {
        return Err(invalid_active_job());
    }
    if evidence.worker.latest_terminal().is_some()
        && evidence.worker.status().view().terminal_tombstone_count() == 0
    {
        return Err(OperatorStatusServiceError::new(
            OperatorStatusServiceErrorKind::InvalidTerminal,
            OperatorErrorCode::DurableStateCorrupt,
        ));
    }
    Ok(())
}

fn validate_machine_identity(
    evidence: &OperatorStatusServiceEvidence,
) -> Result<(), OperatorStatusServiceError> {
    if evidence.lima.schema_version != LIMA_OBSERVATION_SCHEMA_VERSION
        || evidence.lima.instance != *evidence.config.lima_instance()
        || !lima_guest_matches_state(&evidence.lima)
    {
        return Err(OperatorStatusServiceError::new(
            OperatorStatusServiceErrorKind::LimaIdentityMismatch,
            OperatorErrorCode::LimaIdentityMismatch,
        ));
    }
    if evidence.runner.schema_version != ACTIONS_RUNNER_READINESS_SCHEMA_VERSION
        || evidence.runner.instance != *evidence.config.lima_instance()
        || evidence.runner.instance != evidence.lima.instance
        || !runner_identity_matches_state(&evidence.runner)
        || !runner_state_matches_lima_source(&evidence.runner, &evidence.lima)
    {
        return Err(OperatorStatusServiceError::new(
            OperatorStatusServiceErrorKind::RunnerIdentityMismatch,
            OperatorErrorCode::RunnerIdentityMismatch,
        ));
    }
    Ok(())
}

fn runner_state_matches_lima_source(
    runner: &ActionsRunnerReadinessReport,
    lima: &LimaInstanceObservationReport,
) -> bool {
    if runner.state == ActionsRunnerReadinessState::Stale {
        return true;
    }
    match lima.configured.runtime_state {
        LimaRuntimeState::Uninitialized | LimaRuntimeState::Installing => {
            runner.state == ActionsRunnerReadinessState::Starting
        }
        LimaRuntimeState::Stopped => runner.state == ActionsRunnerReadinessState::Offline,
        LimaRuntimeState::Running => matches!(
            runner.state,
            ActionsRunnerReadinessState::Starting
                | ActionsRunnerReadinessState::IdleReady
                | ActionsRunnerReadinessState::Busy
                | ActionsRunnerReadinessState::Draining
        ),
        LimaRuntimeState::Broken => false,
    }
}

fn lima_guest_matches_state(report: &LimaInstanceObservationReport) -> bool {
    match (&report.guest, report.configured.runtime_state) {
        (LimaGuestObservation::Observed(_), LimaRuntimeState::Running) => true,
        (
            LimaGuestObservation::NotRunning { runtime_state },
            LimaRuntimeState::Uninitialized
            | LimaRuntimeState::Installing
            | LimaRuntimeState::Broken
            | LimaRuntimeState::Stopped,
        ) => *runtime_state == report.configured.runtime_state,
        _ => false,
    }
}

fn runner_identity_matches_state(report: &ActionsRunnerReadinessReport) -> bool {
    match report.state {
        ActionsRunnerReadinessState::IdleReady
        | ActionsRunnerReadinessState::Busy
        | ActionsRunnerReadinessState::Draining => report
            .configured_identity
            .as_ref()
            .is_some_and(|identity| identity.runner_name == report.runner_name),
        ActionsRunnerReadinessState::Offline
        | ActionsRunnerReadinessState::Starting
        | ActionsRunnerReadinessState::Stale => report.configured_identity.is_none(),
    }
}

fn validate_timing(
    evidence: &OperatorStatusServiceEvidence,
) -> Result<(), OperatorStatusServiceError> {
    let lima = &evidence.lima.timing;
    let runner = &evidence.runner.timing;
    if !canonical_timing(lima)
        || !canonical_timing(runner)
        || lima.observed_at_unix_seconds >= lima.expires_at_unix_seconds
        || lima.freshness != LimaObservationFreshness::Fresh
        || runner.started_at_unix_seconds < lima.observed_at_unix_seconds
        || runner.expires_at_unix_seconds != lima.expires_at_unix_seconds
        || runner.freshness != lima.freshness_at(runner.observed_at_unix_seconds)
        || (evidence.runner.state == ActionsRunnerReadinessState::Stale)
            != (runner.freshness != LimaObservationFreshness::Fresh)
        || lima.freshness_at(evidence.observed_at_unix_seconds) == LimaObservationFreshness::Future
        || runner.freshness_at(evidence.observed_at_unix_seconds)
            == LimaObservationFreshness::Future
    {
        return Err(OperatorStatusServiceError::new(
            OperatorStatusServiceErrorKind::InvalidTiming,
            OperatorErrorCode::ServiceUnavailable,
        ));
    }
    Ok(())
}

fn canonical_timing(timing: &LimaObservationTiming) -> bool {
    timing.started_at_unix_seconds <= timing.observed_at_unix_seconds
        && timing
            .observed_at_unix_seconds
            .checked_sub(timing.started_at_unix_seconds)
            == Some(timing.duration_seconds)
}

fn project_terminal(
    evidence: &OperatorStatusServiceEvidence,
) -> Result<Option<OperatorTerminalSummary>, OperatorStatusServiceError> {
    let Some(terminal) = evidence.worker.latest_terminal() else {
        return Ok(None);
    };
    if evidence.worker.status().view().terminal_tombstone_count() == 0 {
        return Err(OperatorStatusServiceError::new(
            OperatorStatusServiceErrorKind::InvalidTerminal,
            OperatorErrorCode::DurableStateCorrupt,
        ));
    }
    let view = terminal.job().view().terminal().ok_or_else(|| {
        OperatorStatusServiceError::new(
            OperatorStatusServiceErrorKind::InvalidTerminal,
            OperatorErrorCode::DurableStateCorrupt,
        )
    })?;
    Ok(Some(OperatorTerminalSummary::from_terminal_view(
        view,
        terminal.result(),
    )))
}

const fn invalid_active_job() -> OperatorStatusServiceError {
    OperatorStatusServiceError::new(
        OperatorStatusServiceErrorKind::InvalidActiveJob,
        OperatorErrorCode::DurableStateCorrupt,
    )
}
