use std::fmt;
use std::io;
use std::time::Duration;

use serde::Serialize;

use crate::actions_runner_readiness::{
    ACTIONS_RUNNER_READINESS_SCHEMA_VERSION, ActionsRunnerConfiguredIdentity,
    ActionsRunnerReadinessAdapter, ActionsRunnerReadinessFailure,
    ActionsRunnerReadinessObservation, ActionsRunnerReadinessRefusalCode,
    ActionsRunnerReadinessReport, ActionsRunnerReadinessRequest, ActionsRunnerReadinessState,
};
use crate::execution_admission::{ExecutionAdmissionState, ExecutionRequestId};
use crate::lima_lifecycle::LimaResourceProfile;
use crate::lima_observation::{
    LIMA_OBSERVATION_SCHEMA_VERSION, LimaGuestObservation, LimaInstanceName,
    LimaInstanceObservationReport, LimaObservationClock, LimaObservationFreshness,
    LimaObservationTiming, LimaRuntimeState,
};
use crate::mac_availability::ObservationFreshness;
use crate::macos_resource_observation::MACOS_RESOURCE_OBSERVATION_SCHEMA_VERSION;
use crate::operator_config::{OperatorConfig, OperatorConfigIdentity};
use crate::personal_worker_mac_observation::{
    PERSONAL_WORKER_MAC_OBSERVATION_SCHEMA_VERSION, PersonalWorkerMacObservation,
    PersonalWorkerMacObservationReport,
};
use crate::personal_worker_operator_read::{
    PERSONAL_WORKER_OPERATOR_READ_SCHEMA_VERSION, PersonalWorkerOperatorJobRead,
    PersonalWorkerOperatorStatusRead,
};
use crate::personal_worker_queue::{
    PersonalWorkerProfile, PersonalWorkerQueueEntryState, PersonalWorkerQueueGeneration,
};
use crate::personal_worker_read_model::PersonalWorkerJobStateView;
use crate::personal_worker_store::PersonalWorkerStoreRevision;
use crate::process::{CommandExecutor, CommandSpec, ExecutionRecord, TimedCommandExecutor};

pub const PERSONAL_WORKER_RUNNER_READINESS_SCHEMA_VERSION: u8 = 1;
pub const MAX_PERSONAL_WORKER_RUNNER_COMMAND_TIMEOUT_MILLIS: u64 = 30_000;

const REDACTED_PRIVATE_EVIDENCE: &str = "<private-personal-worker-runner-readiness-evidence>";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerRunnerReadinessDisposition {
    Ready,
    Observe,
    Blocked,
    RepairRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerRunnerReadinessReason {
    IdleReady,
    LimaOffline,
    LimaStarting,
    LimaUnavailable,
    RunnerStarting,
    RunnerStale,
    RunnerEvidenceUnavailable,
    ReservedJobReady,
    JobTransitionPending,
    ActiveJobRunning,
    ActiveJobDraining,
    ConfigurationMismatch,
    MacEvidenceInvalid,
    MacEvidenceStale,
    WorkerSnapshotMismatch,
    RunnerIdentityMismatch,
    RunnerStateMismatch,
    ActiveJobMismatch,
    DrainMismatch,
    ObservationBoundaryViolation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerRunnerActiveEvidence {
    pub request_id: ExecutionRequestId,
    pub admission_state: ExecutionAdmissionState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerRunnerReadinessReport {
    pub schema_version: u8,
    pub config_identity: OperatorConfigIdentity,
    pub store_revision: PersonalWorkerStoreRevision,
    pub queue_generation: PersonalWorkerQueueGeneration,
    pub instance: LimaInstanceName,
    pub lima_profile: LimaResourceProfile,
    pub expected_runner_identity: ActionsRunnerConfiguredIdentity,
    pub disposition: PersonalWorkerRunnerReadinessDisposition,
    pub reason: PersonalWorkerRunnerReadinessReason,
    pub lima: LimaInstanceObservationReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner: Option<ActionsRunnerReadinessReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<PersonalWorkerRunnerActiveEvidence>,
}

pub struct PersonalWorkerRunnerReadinessObservation {
    report: PersonalWorkerRunnerReadinessReport,
    private_runner_evidence: PrivateRunnerEvidence,
}

impl PersonalWorkerRunnerReadinessObservation {
    #[must_use]
    pub const fn report(&self) -> &PersonalWorkerRunnerReadinessReport {
        &self.report
    }

    #[must_use]
    pub const fn private_runner_failure(&self) -> Option<&ActionsRunnerReadinessFailure> {
        match &self.private_runner_evidence {
            PrivateRunnerEvidence::Failure(failure) => Some(failure),
            PrivateRunnerEvidence::Observation(_) | PrivateRunnerEvidence::NotAttempted => None,
        }
    }

    #[must_use]
    pub const fn private_runner_observation(&self) -> Option<&ActionsRunnerReadinessObservation> {
        match &self.private_runner_evidence {
            PrivateRunnerEvidence::Observation(observation) => Some(observation),
            PrivateRunnerEvidence::Failure(_) | PrivateRunnerEvidence::NotAttempted => None,
        }
    }
}

impl fmt::Debug for PersonalWorkerRunnerReadinessObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersonalWorkerRunnerReadinessObservation")
            .field("report", &self.report)
            .field("private_runner_evidence", &REDACTED_PRIVATE_EVIDENCE)
            .finish()
    }
}

enum PrivateRunnerEvidence {
    NotAttempted,
    Observation(ActionsRunnerReadinessObservation),
    Failure(ActionsRunnerReadinessFailure),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerRunnerReadinessErrorKind {
    Policy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerRunnerReadinessError {
    pub kind: PersonalWorkerRunnerReadinessErrorKind,
    pub field: &'static str,
    pub code: &'static str,
    pub message: &'static str,
}

impl fmt::Display for PersonalWorkerRunnerReadinessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for PersonalWorkerRunnerReadinessError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonalWorkerRunnerReadinessAdapter {
    command_timeout: Duration,
}

impl PersonalWorkerRunnerReadinessAdapter {
    /// Construct the read-only personal-worker runner-readiness adapter.
    ///
    /// # Errors
    ///
    /// Returns a bounded error unless the delegated command timeout is positive and remains within
    /// the reviewed maximum.
    pub fn new(command_timeout: Duration) -> Result<Self, PersonalWorkerRunnerReadinessError> {
        if command_timeout.is_zero()
            || command_timeout
                > Duration::from_millis(MAX_PERSONAL_WORKER_RUNNER_COMMAND_TIMEOUT_MILLIS)
        {
            return Err(PersonalWorkerRunnerReadinessError {
                kind: PersonalWorkerRunnerReadinessErrorKind::Policy,
                field: "command_timeout",
                code: "invalid_command_timeout",
                message: "the runner-readiness command timeout is outside the reviewed range",
            });
        }
        Ok(Self { command_timeout })
    }

    /// Observe and classify one exact personal-worker runner state without mutation.
    ///
    /// The sealed Mac/Lima observation and durable reads come from the accepted B02 and O03
    /// boundaries. Its opaque private Lima-source identity must equal the runner request source
    /// before any command. Every runner subprocess is delegated to the accepted official-runner
    /// observer through the same bounded timeout. Operational observation failures become one of
    /// the closed dispositions while the raw failure remains private.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn observe(
        &self,
        config: &OperatorConfig,
        mac_observation: &PersonalWorkerMacObservation,
        runner_request: &ActionsRunnerReadinessRequest,
        expected_runner_identity: &ActionsRunnerConfiguredIdentity,
        status: &PersonalWorkerOperatorStatusRead,
        active_job: Option<&PersonalWorkerOperatorJobRead>,
        runner_adapter: &ActionsRunnerReadinessAdapter,
        executor: &impl TimedCommandExecutor,
        clock: &impl LimaObservationClock,
    ) -> PersonalWorkerRunnerReadinessObservation {
        let mac = mac_observation.report();
        let snapshot = durable_snapshot(status, active_job);

        if mac.schema_version != PERSONAL_WORKER_MAC_OBSERVATION_SCHEMA_VERSION
            || mac.config_identity != *config.identity()
            || mac.requested_availability != config.availability()
            || mac.lima.instance != *config.lima_instance()
            || !valid_mac_evidence(mac)
        {
            return observation(
                config,
                mac,
                expected_runner_identity,
                status,
                None,
                PersonalWorkerRunnerReadinessDisposition::RepairRequired,
                PersonalWorkerRunnerReadinessReason::MacEvidenceInvalid,
                None,
                PrivateRunnerEvidence::NotAttempted,
            );
        }
        if status.config_identity() != config.identity()
            || active_job.is_some_and(|job| job.config_identity() != config.identity())
        {
            return observation(
                config,
                mac,
                expected_runner_identity,
                status,
                None,
                PersonalWorkerRunnerReadinessDisposition::RepairRequired,
                PersonalWorkerRunnerReadinessReason::ConfigurationMismatch,
                None,
                PrivateRunnerEvidence::NotAttempted,
            );
        }
        if !worker_profile_matches_lima(status, mac) {
            return observation(
                config,
                mac,
                expected_runner_identity,
                status,
                None,
                PersonalWorkerRunnerReadinessDisposition::RepairRequired,
                PersonalWorkerRunnerReadinessReason::WorkerSnapshotMismatch,
                None,
                PrivateRunnerEvidence::NotAttempted,
            );
        }
        if runner_request.instance() != config.lima_instance()
            || runner_request.runner_name() != &expected_runner_identity.runner_name
            || mac_observation.lima_source_identity() != &runner_request.source_identity()
        {
            return observation(
                config,
                mac,
                expected_runner_identity,
                status,
                None,
                PersonalWorkerRunnerReadinessDisposition::RepairRequired,
                PersonalWorkerRunnerReadinessReason::ConfigurationMismatch,
                None,
                PrivateRunnerEvidence::NotAttempted,
            );
        }
        let snapshot = match snapshot {
            Ok(snapshot) => snapshot,
            Err(()) => {
                return observation(
                    config,
                    mac,
                    expected_runner_identity,
                    status,
                    None,
                    PersonalWorkerRunnerReadinessDisposition::RepairRequired,
                    PersonalWorkerRunnerReadinessReason::WorkerSnapshotMismatch,
                    None,
                    PrivateRunnerEvidence::NotAttempted,
                );
            }
        };

        let bounded = TimeoutExecutor {
            executor,
            timeout: self.command_timeout,
        };
        match runner_adapter.observe(runner_request, &mac.lima, &bounded, clock) {
            Ok(runner) => {
                if !valid_runner_report(runner.report(), &mac.lima, expected_runner_identity) {
                    return observation(
                        config,
                        mac,
                        expected_runner_identity,
                        status,
                        snapshot.active,
                        PersonalWorkerRunnerReadinessDisposition::RepairRequired,
                        PersonalWorkerRunnerReadinessReason::RunnerIdentityMismatch,
                        Some(runner.report().clone()),
                        PrivateRunnerEvidence::Observation(runner),
                    );
                }
                match mac_freshness_at_runner(mac, runner.report()) {
                    MacCompositionTiming::Fresh => {}
                    MacCompositionTiming::Stale => {
                        return observation(
                            config,
                            mac,
                            expected_runner_identity,
                            status,
                            snapshot.active,
                            PersonalWorkerRunnerReadinessDisposition::Observe,
                            PersonalWorkerRunnerReadinessReason::MacEvidenceStale,
                            Some(runner.report().clone()),
                            PrivateRunnerEvidence::Observation(runner),
                        );
                    }
                    MacCompositionTiming::Invalid => {
                        return observation(
                            config,
                            mac,
                            expected_runner_identity,
                            status,
                            snapshot.active,
                            PersonalWorkerRunnerReadinessDisposition::RepairRequired,
                            PersonalWorkerRunnerReadinessReason::RunnerStateMismatch,
                            Some(runner.report().clone()),
                            PrivateRunnerEvidence::Observation(runner),
                        );
                    }
                }
                let (disposition, reason) = classify(runner.report(), snapshot.active.as_ref());
                observation(
                    config,
                    mac,
                    expected_runner_identity,
                    status,
                    snapshot.active,
                    disposition,
                    reason,
                    Some(runner.report().clone()),
                    PrivateRunnerEvidence::Observation(runner),
                )
            }
            Err(failure) => {
                let (disposition, reason) = classify_failure(&failure, snapshot.active.as_ref());
                observation(
                    config,
                    mac,
                    expected_runner_identity,
                    status,
                    snapshot.active,
                    disposition,
                    reason,
                    None,
                    PrivateRunnerEvidence::Failure(failure),
                )
            }
        }
    }
}

#[derive(Clone)]
struct DurableSnapshot {
    active: Option<PersonalWorkerRunnerActiveEvidence>,
}

fn durable_snapshot(
    status: &PersonalWorkerOperatorStatusRead,
    active_job: Option<&PersonalWorkerOperatorJobRead>,
) -> Result<DurableSnapshot, ()> {
    if status.schema_version() != PERSONAL_WORKER_OPERATOR_READ_SCHEMA_VERSION
        || status.view().active_count() > 1
        || status.view().draining_count() > status.view().active_count()
        || (status.view().active_count() == 0) != active_job.is_none()
    {
        return Err(());
    }
    let Some(job) = active_job else {
        if status.view().draining_count() != 0 {
            return Err(());
        }
        return Ok(DurableSnapshot { active: None });
    };
    if job.schema_version() != PERSONAL_WORKER_OPERATOR_READ_SCHEMA_VERSION
        || job.config_identity() != status.config_identity()
        || job.view().store_revision() != status.view().store_revision()
        || job.view().queue_generation() != status.view().queue_generation()
    {
        return Err(());
    }
    let PersonalWorkerJobStateView::Active {
        entry, admission, ..
    } = job.view().state()
    else {
        return Err(());
    };
    let admission_state = admission.state();
    let entry_matches = matches!(
        (entry.state, admission_state),
        (
            PersonalWorkerQueueEntryState::Reserved,
            ExecutionAdmissionState::Reserved
        ) | (
            PersonalWorkerQueueEntryState::Starting,
            ExecutionAdmissionState::Starting
        ) | (
            PersonalWorkerQueueEntryState::Running,
            ExecutionAdmissionState::Running
        ) | (
            PersonalWorkerQueueEntryState::Draining,
            ExecutionAdmissionState::Draining
        )
    );
    if !entry_matches
        || (status.view().draining_count() == 1)
            != (admission_state == ExecutionAdmissionState::Draining)
    {
        return Err(());
    }
    Ok(DurableSnapshot {
        active: Some(PersonalWorkerRunnerActiveEvidence {
            request_id: entry.request_id.clone(),
            admission_state,
        }),
    })
}

fn valid_mac_evidence(mac: &PersonalWorkerMacObservationReport) -> bool {
    let timing = &mac.timing;
    let Some(duration) = timing
        .observed_at_millis
        .checked_sub(timing.started_at_millis)
    else {
        return false;
    };
    let Some(window) = timing
        .expires_at_millis
        .checked_sub(timing.observed_at_millis)
    else {
        return false;
    };
    if timing.started_at_millis == 0
        || duration != timing.duration_millis
        || window == 0
        || duration > window
        || mac.host_headroom.available_memory_bytes == 0
        || mac.host_headroom.logical_cpu_count == 0
        || mac.host_resources.schema_version != MACOS_RESOURCE_OBSERVATION_SCHEMA_VERSION
        || mac.host_resources.observed_at_millis != timing.observed_at_millis
        || mac.host_resources.freshness != ObservationFreshness::Fresh
        || mac.lima.schema_version != LIMA_OBSERVATION_SCHEMA_VERSION
        || !canonical_timing(&mac.lima.timing)
        || mac.lima.timing.freshness != LimaObservationFreshness::Fresh
        || mac.lima.timing.observed_at_unix_seconds >= mac.lima.timing.expires_at_unix_seconds
        || mac
            .lima
            .timing
            .expires_at_unix_seconds
            .checked_sub(mac.lima.timing.observed_at_unix_seconds)
            .is_none_or(|lima_window| mac.lima.timing.duration_seconds > lima_window)
        || mac.lima.timing.started_at_unix_seconds < timing.started_at_millis / 1_000
        || mac.lima.timing.observed_at_unix_seconds > timing.observed_at_millis / 1_000
        || mac
            .lima
            .timing
            .freshness_at(timing.observed_at_millis / 1_000)
            != LimaObservationFreshness::Fresh
    {
        return false;
    }
    let envelope = mac.lima_profile.envelope();
    if mac.lima.configured.cpus != envelope.vcpus
        || mac.lima.configured.memory_bytes != envelope.memory_bytes
    {
        return false;
    }
    match (&mac.lima.guest, mac.lima.configured.runtime_state) {
        (LimaGuestObservation::Observed(guest), LimaRuntimeState::Running) => {
            guest.resources.architecture == mac.lima.configured.architecture
                && guest.resources.cpus == mac.lima.configured.cpus
                && guest.resources.memory_bytes <= mac.lima.configured.memory_bytes
                && mac
                    .lima
                    .configured
                    .memory_bytes
                    .saturating_sub(guest.resources.memory_bytes)
                    <= 64 * 1_024 * 1_024
        }
        (
            LimaGuestObservation::NotRunning { runtime_state },
            LimaRuntimeState::Uninitialized
            | LimaRuntimeState::Installing
            | LimaRuntimeState::Broken
            | LimaRuntimeState::Stopped,
        ) => *runtime_state == mac.lima.configured.runtime_state,
        _ => false,
    }
}

fn valid_runner_report(
    runner: &ActionsRunnerReadinessReport,
    lima: &LimaInstanceObservationReport,
    expected: &ActionsRunnerConfiguredIdentity,
) -> bool {
    if runner.schema_version != ACTIONS_RUNNER_READINESS_SCHEMA_VERSION
        || runner.instance != lima.instance
        || runner.runner_name != expected.runner_name
        || !canonical_timing(&runner.timing)
        || runner.timing.started_at_unix_seconds < lima.timing.observed_at_unix_seconds
        || runner.timing.expires_at_unix_seconds != lima.timing.expires_at_unix_seconds
        || runner.timing.freshness
            != lima
                .timing
                .freshness_at(runner.timing.observed_at_unix_seconds)
        || runner.timing.freshness == LimaObservationFreshness::Future
        || (runner.state == ActionsRunnerReadinessState::Stale)
            != (runner.timing.freshness != LimaObservationFreshness::Fresh)
    {
        return false;
    }
    match runner.state {
        ActionsRunnerReadinessState::Offline => {
            lima.configured.runtime_state == LimaRuntimeState::Stopped
                && runner.configured_identity.is_none()
                && runner.timing.duration_seconds == 0
        }
        ActionsRunnerReadinessState::Starting
            if lima.configured.runtime_state != LimaRuntimeState::Running =>
        {
            matches!(
                lima.configured.runtime_state,
                LimaRuntimeState::Uninitialized | LimaRuntimeState::Installing
            ) && runner.configured_identity.is_none()
                && runner.timing.duration_seconds == 0
        }
        ActionsRunnerReadinessState::Starting
        | ActionsRunnerReadinessState::IdleReady
        | ActionsRunnerReadinessState::Busy
        | ActionsRunnerReadinessState::Draining => {
            lima.configured.runtime_state == LimaRuntimeState::Running
                && runner.configured_identity.as_ref() == Some(expected)
        }
        ActionsRunnerReadinessState::Stale => runner.configured_identity.is_none(),
    }
}

enum MacCompositionTiming {
    Fresh,
    Stale,
    Invalid,
}

fn mac_freshness_at_runner(
    mac: &PersonalWorkerMacObservationReport,
    runner: &ActionsRunnerReadinessReport,
) -> MacCompositionTiming {
    let Some(started_lower_millis) = runner.timing.started_at_unix_seconds.checked_mul(1_000)
    else {
        return MacCompositionTiming::Invalid;
    };
    let Some(observed_lower_millis) = runner.timing.observed_at_unix_seconds.checked_mul(1_000)
    else {
        return MacCompositionTiming::Invalid;
    };
    let Some(observed_upper_millis) = observed_lower_millis.checked_add(999) else {
        return MacCompositionTiming::Invalid;
    };
    if started_lower_millis < mac.timing.observed_at_millis {
        return MacCompositionTiming::Invalid;
    }
    if observed_upper_millis <= mac.timing.expires_at_millis {
        MacCompositionTiming::Fresh
    } else {
        MacCompositionTiming::Stale
    }
}

fn worker_profile_matches_lima(
    status: &PersonalWorkerOperatorStatusRead,
    mac: &PersonalWorkerMacObservationReport,
) -> bool {
    match mac.lima.configured.runtime_state {
        LimaRuntimeState::Running => match status.view().current_profile() {
            Some(PersonalWorkerProfile::Interactive) => {
                mac.lima_profile == LimaResourceProfile::Interactive
            }
            Some(PersonalWorkerProfile::Work) => mac.lima_profile == LimaResourceProfile::Work,
            Some(PersonalWorkerProfile::Stopped) | None => false,
        },
        LimaRuntimeState::Uninitialized
        | LimaRuntimeState::Installing
        | LimaRuntimeState::Stopped => {
            status.view().current_profile() == Some(PersonalWorkerProfile::Stopped)
        }
        LimaRuntimeState::Broken => true,
    }
}

fn canonical_timing(timing: &LimaObservationTiming) -> bool {
    timing.started_at_unix_seconds > 0
        && timing.started_at_unix_seconds <= timing.observed_at_unix_seconds
        && timing
            .observed_at_unix_seconds
            .checked_sub(timing.started_at_unix_seconds)
            == Some(timing.duration_seconds)
}

fn classify(
    runner: &ActionsRunnerReadinessReport,
    active: Option<&PersonalWorkerRunnerActiveEvidence>,
) -> (
    PersonalWorkerRunnerReadinessDisposition,
    PersonalWorkerRunnerReadinessReason,
) {
    use PersonalWorkerRunnerReadinessDisposition::{Blocked, Observe, Ready, RepairRequired};
    use PersonalWorkerRunnerReadinessReason::{
        ActiveJobDraining, ActiveJobMismatch, ActiveJobRunning, DrainMismatch, IdleReady,
        JobTransitionPending, LimaOffline, LimaStarting, ReservedJobReady, RunnerStale,
        RunnerStarting,
    };

    match (runner.state, active.map(|active| active.admission_state)) {
        (ActionsRunnerReadinessState::Offline, None) => (Observe, LimaOffline),
        (ActionsRunnerReadinessState::Starting, None) if runner.configured_identity.is_none() => {
            (Observe, LimaStarting)
        }
        (ActionsRunnerReadinessState::Starting, None) => (Observe, RunnerStarting),
        (ActionsRunnerReadinessState::Starting, Some(_))
            if runner.configured_identity.is_none() =>
        {
            (RepairRequired, ActiveJobMismatch)
        }
        (ActionsRunnerReadinessState::IdleReady, Some(ExecutionAdmissionState::Reserved)) => {
            (Ready, ReservedJobReady)
        }
        (
            ActionsRunnerReadinessState::Starting | ActionsRunnerReadinessState::IdleReady,
            Some(ExecutionAdmissionState::Starting),
        ) => (Observe, JobTransitionPending),
        (ActionsRunnerReadinessState::Starting, Some(ExecutionAdmissionState::Reserved)) => {
            (Observe, JobTransitionPending)
        }
        (ActionsRunnerReadinessState::Stale, _) => (Observe, RunnerStale),
        (ActionsRunnerReadinessState::IdleReady, None) => (Ready, IdleReady),
        (
            ActionsRunnerReadinessState::Busy,
            Some(ExecutionAdmissionState::Starting | ExecutionAdmissionState::Running),
        ) => (Blocked, ActiveJobRunning),
        (ActionsRunnerReadinessState::Draining, Some(ExecutionAdmissionState::Draining)) => {
            (Blocked, ActiveJobDraining)
        }
        (ActionsRunnerReadinessState::Draining, _) => (RepairRequired, DrainMismatch),
        _ => (RepairRequired, ActiveJobMismatch),
    }
}

fn classify_failure(
    failure: &ActionsRunnerReadinessFailure,
    active: Option<&PersonalWorkerRunnerActiveEvidence>,
) -> (
    PersonalWorkerRunnerReadinessDisposition,
    PersonalWorkerRunnerReadinessReason,
) {
    use PersonalWorkerRunnerReadinessDisposition::{Blocked, Observe, RepairRequired};
    use PersonalWorkerRunnerReadinessReason::{
        ActiveJobMismatch, LimaUnavailable, ObservationBoundaryViolation,
        RunnerEvidenceUnavailable, RunnerIdentityMismatch, RunnerStateMismatch,
    };

    match failure.code {
        ActionsRunnerReadinessRefusalCode::SourceUnavailable if active.is_some() => {
            (RepairRequired, ActiveJobMismatch)
        }
        ActionsRunnerReadinessRefusalCode::SourceUnavailable => (Blocked, LimaUnavailable),
        ActionsRunnerReadinessRefusalCode::ClockFailure
        | ActionsRunnerReadinessRefusalCode::CommandFailed => (Observe, RunnerEvidenceUnavailable),
        ActionsRunnerReadinessRefusalCode::CommandIdentityMismatch
        | ActionsRunnerReadinessRefusalCode::UnboundedOutput => {
            (RepairRequired, ObservationBoundaryViolation)
        }
        ActionsRunnerReadinessRefusalCode::SourceInstanceMismatch
        | ActionsRunnerReadinessRefusalCode::SourceGuestMismatch
        | ActionsRunnerReadinessRefusalCode::MissingIdentityEvidence
        | ActionsRunnerReadinessRefusalCode::MalformedIdentityEvidence
        | ActionsRunnerReadinessRefusalCode::ConfigurationIdentityMismatch
        | ActionsRunnerReadinessRefusalCode::IdentityDrift => {
            (RepairRequired, RunnerIdentityMismatch)
        }
        ActionsRunnerReadinessRefusalCode::InvalidInput
        | ActionsRunnerReadinessRefusalCode::AmbiguousListener
        | ActionsRunnerReadinessRefusalCode::AmbiguousWorker
        | ActionsRunnerReadinessRefusalCode::ProcessIdentityMismatch
        | ActionsRunnerReadinessRefusalCode::ProcessStateInconsistent
        | ActionsRunnerReadinessRefusalCode::ProcessDrift
        | ActionsRunnerReadinessRefusalCode::DrainStateDrift => {
            (RepairRequired, RunnerStateMismatch)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn observation(
    config: &OperatorConfig,
    mac: &PersonalWorkerMacObservationReport,
    expected_runner_identity: &ActionsRunnerConfiguredIdentity,
    status: &PersonalWorkerOperatorStatusRead,
    active: Option<PersonalWorkerRunnerActiveEvidence>,
    disposition: PersonalWorkerRunnerReadinessDisposition,
    reason: PersonalWorkerRunnerReadinessReason,
    runner: Option<ActionsRunnerReadinessReport>,
    private_runner_evidence: PrivateRunnerEvidence,
) -> PersonalWorkerRunnerReadinessObservation {
    PersonalWorkerRunnerReadinessObservation {
        report: PersonalWorkerRunnerReadinessReport {
            schema_version: PERSONAL_WORKER_RUNNER_READINESS_SCHEMA_VERSION,
            config_identity: config.identity().clone(),
            store_revision: status.view().store_revision(),
            queue_generation: status.view().queue_generation(),
            instance: mac.lima.instance.clone(),
            lima_profile: mac.lima_profile,
            expected_runner_identity: expected_runner_identity.clone(),
            disposition,
            reason,
            lima: mac.lima.clone(),
            runner,
            active,
        },
        private_runner_evidence,
    }
}

struct TimeoutExecutor<'a, E> {
    executor: &'a E,
    timeout: Duration,
}

impl<E: TimedCommandExecutor> CommandExecutor for TimeoutExecutor<'_, E> {
    fn execute(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord> {
        self.executor.execute_with_timeout(spec, self.timeout)
    }
}
