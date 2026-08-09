use std::fmt;
use std::io;
use std::time::Duration;

use serde::Serialize;

use crate::execution_admission::EpochMillis;
use crate::lima_lifecycle::{
    LimaLifecycleObservation, LimaLifecycleObservationDefinition, LimaLifecycleState,
    LimaObservedResources, LimaProfileGeneration, LimaResourceProfile,
};
use crate::lima_lifecycle_executor::{
    AcceptedLimaLifecycleAction, LimaLifecycleExecution, LimaLifecycleExecutionAction,
    LimaLifecycleExecutionCheckpoint, LimaLifecycleExecutionFailure, LimaLifecycleExecutionInput,
    LimaLifecycleExecutionJournal, LimaLifecycleExecutionJournalError, LimaLifecycleExecutionPhase,
    LimaLifecycleExecutionRefusalCode, LimaLifecycleExecutor,
};
use crate::lima_observation::{
    LimaInstanceObservationReport, LimaObservationAdapter, LimaObservationClock,
    LimaObservationRequest,
};
use crate::operator_config::OperatorConfig;
use crate::personal_worker_host_broker::HostBrokerStateRevision;
use crate::personal_worker_lima_authority::{
    PersonalWorkerLimaAttemptGeneration, PersonalWorkerLimaAttemptInput,
    PersonalWorkerLimaAttemptPhase, PersonalWorkerLimaAuthorityDocument,
    PersonalWorkerLimaAuthorityError, PersonalWorkerLimaRecoveryReport,
};
use crate::personal_worker_mac_observation::{
    PersonalWorkerMacObservation, PersonalWorkerMacObservationAdapter,
    PersonalWorkerMacObservationClock, PersonalWorkerMacObservationError,
    PersonalWorkerMacObservationErrorKind,
};
use crate::personal_worker_queue::PersonalWorkerQueueGeneration;
use crate::personal_worker_store::PersonalWorkerStoreRevision;
use crate::personal_worker_tick::PersonalWorkerTickPlan;
use crate::process::{CommandExecutor, CommandSpec, ExecutionRecord, TimedCommandExecutor};
use crate::unix_personal_worker_store::lima_authority::{
    UnixPersonalWorkerLimaAuthorityError, UnixPersonalWorkerLimaAuthorityErrorKind,
    UnixPersonalWorkerLimaAuthorityGuard,
};

pub const PERSONAL_WORKER_LIMA_ADAPTER_SCHEMA_VERSION: u8 = 1;

const REDACTED_PRIVATE_FAILURE: &str = "<private-personal-worker-lima-failure>";
const LIMA_VERIFICATION_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerLimaRefusalCode {
    InvalidInput,
    SourceIdentityDrift,
    DurableStateUnavailable,
    RecoveryRequired,
    LifecycleRefusal,
    PostObservationFailed,
    CompletionFailed,
    SettlementFailed,
}

#[derive(Serialize)]
pub struct PersonalWorkerLimaFailure {
    pub code: PersonalWorkerLimaRefusalCode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle_code: Option<LimaLifecycleExecutionRefusalCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle_phase: Option<LimaLifecycleExecutionPhase>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub durable_code: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation_kind: Option<PersonalWorkerMacObservationErrorKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery: Option<Box<PersonalWorkerLimaRecoveryReport>>,
    pub public_message: &'static str,
    #[serde(skip)]
    private: Option<PrivateFailure>,
}

enum PrivateFailure {
    Durable(UnixPersonalWorkerLimaAuthorityError),
    Authority(PersonalWorkerLimaAuthorityError),
    Lifecycle {
        failure: Box<LimaLifecycleExecutionFailure>,
        checkpoint: Option<Box<PrivateFailure>>,
    },
    Observation(Box<PersonalWorkerMacObservationError>),
    MacTiming,
    Clock(io::Error),
}

impl PrivateFailure {
    const fn observation_kind(&self) -> Option<PersonalWorkerMacObservationErrorKind> {
        match self {
            Self::Lifecycle { checkpoint, .. } => match checkpoint {
                Some(checkpoint) => checkpoint.observation_kind(),
                None => None,
            },
            Self::Observation(error) => Some(error.kind),
            Self::Durable(_) | Self::Authority(_) | Self::MacTiming | Self::Clock(_) => None,
        }
    }

    fn durable_kind(&self) -> Option<UnixPersonalWorkerLimaAuthorityErrorKind> {
        match self {
            Self::Durable(error) => Some(error.kind()),
            Self::Lifecycle { checkpoint, .. } => {
                checkpoint.as_deref().and_then(PrivateFailure::durable_kind)
            }
            Self::Authority(_) | Self::Observation(_) | Self::MacTiming | Self::Clock(_) => None,
        }
    }

    fn retain(&self) {
        match self {
            Self::Durable(error) => {
                let _ = error;
            }
            Self::Authority(error) => {
                let _ = error;
            }
            Self::Lifecycle {
                failure,
                checkpoint,
            } => {
                let _ = failure;
                if let Some(checkpoint) = checkpoint {
                    checkpoint.retain();
                }
            }
            Self::Observation(error) => {
                let _ = error;
            }
            Self::MacTiming => {}
            Self::Clock(error) => {
                let _ = error;
            }
        }
    }
}

impl fmt::Debug for PersonalWorkerLimaFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(private) = &self.private {
            private.retain();
        }
        formatter
            .debug_struct("PersonalWorkerLimaFailure")
            .field("code", &self.code)
            .field("lifecycle_code", &self.lifecycle_code)
            .field("lifecycle_phase", &self.lifecycle_phase)
            .field("durable_code", &self.durable_code)
            .field("observation_kind", &self.observation_kind)
            .field("recovery", &self.recovery)
            .field("public_message", &self.public_message)
            .field("private", &REDACTED_PRIVATE_FAILURE)
            .finish()
    }
}

impl fmt::Display for PersonalWorkerLimaFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.public_message)
    }
}

impl std::error::Error for PersonalWorkerLimaFailure {}

pub struct PersonalWorkerLimaInput<'a> {
    pub config: &'a OperatorConfig,
    pub tick: &'a PersonalWorkerTickPlan,
    pub lifecycle: &'a LimaLifecycleObservation,
    pub mac: &'a PersonalWorkerMacObservation,
    pub observation_request: &'a LimaObservationRequest,
}

#[derive(Serialize)]
pub struct PersonalWorkerLimaExecution {
    schema_version: u8,
    action: LimaLifecycleExecutionAction,
    before_store_revision: PersonalWorkerStoreRevision,
    before_queue_generation: PersonalWorkerQueueGeneration,
    after_store_revision: PersonalWorkerStoreRevision,
    after_queue_generation: PersonalWorkerQueueGeneration,
    after_state: LimaLifecycleState,
    after_profile: LimaResourceProfile,
    after_profile_generation: LimaProfileGeneration,
}

impl PersonalWorkerLimaExecution {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn action(&self) -> LimaLifecycleExecutionAction {
        self.action
    }

    #[must_use]
    pub const fn before_store_revision(&self) -> PersonalWorkerStoreRevision {
        self.before_store_revision
    }

    #[must_use]
    pub const fn before_queue_generation(&self) -> PersonalWorkerQueueGeneration {
        self.before_queue_generation
    }

    #[must_use]
    pub const fn after_store_revision(&self) -> PersonalWorkerStoreRevision {
        self.after_store_revision
    }

    #[must_use]
    pub const fn after_queue_generation(&self) -> PersonalWorkerQueueGeneration {
        self.after_queue_generation
    }

    #[must_use]
    pub const fn after_state(&self) -> LimaLifecycleState {
        self.after_state
    }

    #[must_use]
    pub const fn after_profile(&self) -> LimaResourceProfile {
        self.after_profile
    }

    #[must_use]
    pub const fn after_profile_generation(&self) -> LimaProfileGeneration {
        self.after_profile_generation
    }
}

impl fmt::Debug for PersonalWorkerLimaExecution {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersonalWorkerLimaExecution")
            .field("schema_version", &self.schema_version)
            .field("action", &self.action)
            .field("before_store_revision", &self.before_store_revision)
            .field("before_queue_generation", &self.before_queue_generation)
            .field("after_store_revision", &self.after_store_revision)
            .field("after_queue_generation", &self.after_queue_generation)
            .field("after_state", &self.after_state)
            .field("after_profile", &self.after_profile)
            .field("after_profile_generation", &self.after_profile_generation)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PersonalWorkerLimaAdapter;

impl PersonalWorkerLimaAdapter {
    /// Execute one exact B01 lifecycle tick while retaining the canonical durable writer lock.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal for missing or unsettled durable authority, stale/forged source
    /// evidence, active work, any checkpoint or command failure, post-observation drift, or a
    /// completion/settlement failure. Once `Prepared` is durable, every error retains an explicit
    /// recovery disposition and no command is silently retried.
    #[allow(clippy::too_many_arguments)]
    pub fn execute<E, C>(
        &self,
        input: PersonalWorkerLimaInput<'_>,
        lifecycle_executor: &LimaLifecycleExecutor,
        mac_adapter: &PersonalWorkerMacObservationAdapter,
        lima_adapter: &LimaObservationAdapter,
        command_executor: &E,
        clock: &C,
    ) -> Result<PersonalWorkerLimaExecution, PersonalWorkerLimaFailure>
    where
        E: TimedCommandExecutor,
        C: PersonalWorkerMacObservationClock,
    {
        let accepted = AcceptedLimaLifecycleAction::from_personal_worker_tick(input.tick)
            .map_err(PersonalWorkerLimaFailure::lifecycle_without_recovery)?;
        let mut guard =
            UnixPersonalWorkerLimaAuthorityGuard::open(input.config.state_root().as_path())
                .map_err(PersonalWorkerLimaFailure::durable)?;
        if guard.recovery_required() {
            return Err(PersonalWorkerLimaFailure::recovery(
                recovery_report(&guard),
                "durable Lima authority requires recovery before lifecycle execution",
            ));
        }
        if guard.has_active_work() {
            return Err(PersonalWorkerLimaFailure::invalid(
                "Lima lifecycle execution is forbidden while durable work remains active",
            ));
        }
        let authority = guard.authority().cloned().ok_or_else(|| {
            PersonalWorkerLimaFailure::recovery(
                None,
                "durable Lima enrollment is required before lifecycle execution",
            )
        })?;
        if authority.attempt().is_some() {
            return Err(PersonalWorkerLimaFailure::recovery(
                Some(authority.recovery_report()),
                "an existing Lima lifecycle attempt requires explicit recovery",
            ));
        }

        let before_store_revision = guard.store_revision();
        let before_queue_generation = guard.queue_generation();
        let current_broker_state_revision =
            HostBrokerStateRevision::new(before_store_revision.get()).map_err(|_| {
                PersonalWorkerLimaFailure::invalid(
                    "the durable store revision is not supported by the Lima executor",
                )
            })?;
        let persistent_identity = authority.persistent_identity().clone();
        input
            .mac
            .confirm_lima_host_identity(input.observation_request)
            .map_err(PersonalWorkerLimaFailure::source_identity)?;
        let execution_time = read_time(clock)?;
        if !mac_execution_time_is_fresh(input.mac, execution_time) {
            return Err(PersonalWorkerLimaFailure::invalid(
                "the sealed Mac/Lima observation is not fresh at lifecycle execution time",
            ));
        }
        let execution_input = LimaLifecycleExecutionInput {
            accepted: &accepted,
            current_broker_state_revision,
            current_queue_generation: before_queue_generation,
            lifecycle: input.lifecycle,
            current: &input.mac.report().lima,
            expected_persistent_identity: &persistent_identity,
            observation_request: input.observation_request,
        };
        let checkpoint_preflight_input = LimaLifecycleExecutionInput {
            accepted: &accepted,
            current_broker_state_revision,
            current_queue_generation: before_queue_generation,
            lifecycle: input.lifecycle,
            current: &input.mac.report().lima,
            expected_persistent_identity: &persistent_identity,
            observation_request: input.observation_request,
        };
        lifecycle_executor
            .validate_input_at(&execution_input, execution_time)
            .map_err(PersonalWorkerLimaFailure::lifecycle_without_recovery)?;

        let prepared = authority
            .begin_attempt(PersonalWorkerLimaAttemptInput {
                config: input.config,
                mac: input.mac,
                request: input.observation_request,
                lifecycle: input.lifecycle,
                tick: input.tick,
            })
            .map_err(PersonalWorkerLimaFailure::authority_invalid)?;
        let attempt_generation = prepared
            .attempt()
            .expect("a prepared authority contains one exact attempt")
            .generation();
        guard
            .replace_authority(authority.authority_generation(), &prepared)
            .map_err(|error| {
                PersonalWorkerLimaFailure::durable_after_guard(
                    PersonalWorkerLimaRefusalCode::DurableStateUnavailable,
                    error,
                    &guard,
                    "the prepared Lima attempt could not be durably published",
                )
            })?;

        let lima_clock = LimaClockBridge { clock };
        let bounded_commands = LifecycleCommandBridge {
            executor: command_executor,
        };
        let mut journal = DurableExecutionJournal {
            guard: &mut guard,
            clock,
            attempt_generation,
            mac: input.mac,
            observation_request: input.observation_request,
            lifecycle_executor,
            checkpoint_preflight_input,
            private_failure: None,
        };
        let lifecycle_result = lifecycle_executor.execute_with_journal_at(
            execution_input,
            lima_adapter,
            &bounded_commands,
            &lima_clock,
            &mut journal,
            execution_time,
        );
        let lifecycle = match lifecycle_result {
            Ok(lifecycle) => lifecycle,
            Err(failure) => {
                let recovery = journal.recovery_report();
                let private = journal.private_failure.take();
                return Err(PersonalWorkerLimaFailure::lifecycle(
                    failure, recovery, private,
                ));
            }
        };
        drop(journal);

        let post_mac = mac_adapter
            .observe(
                input.config,
                input.observation_request,
                lima_adapter,
                command_executor,
                clock,
            )
            .map_err(|error| {
                PersonalWorkerLimaFailure::observation(error, recovery_report(&guard))
            })?;
        validate_same_successor(lifecycle.post_observation(), &post_mac.report().lima).map_err(
            |message| PersonalWorkerLimaFailure::completion(message, recovery_report(&guard), None),
        )?;
        let post_lifecycle = post_lifecycle_projection(input.lifecycle, &lifecycle, &post_mac)
            .map_err(|message| {
                PersonalWorkerLimaFailure::completion(message, recovery_report(&guard), None)
            })?;
        let completed_at = read_time(clock).map_err(|mut failure| {
            failure.code = PersonalWorkerLimaRefusalCode::CompletionFailed;
            failure.recovery = boxed_recovery(recovery_report(&guard));
            failure
        })?;
        let current = guard.authority().cloned().ok_or_else(|| {
            PersonalWorkerLimaFailure::completion(
                "durable Lima authority disappeared before completion",
                recovery_report(&guard),
                None,
            )
        })?;
        let completed = current
            .complete_attempt(
                attempt_generation,
                input.config,
                &post_mac,
                input.observation_request,
                &post_lifecycle,
                completed_at,
            )
            .map_err(|error| {
                PersonalWorkerLimaFailure::completion(
                    "fresh evidence did not complete the exact durable Lima attempt",
                    recovery_report(&guard),
                    Some(error),
                )
            })?;
        guard
            .replace_authority(current.authority_generation(), &completed)
            .map_err(|error| {
                PersonalWorkerLimaFailure::durable_after_guard(
                    PersonalWorkerLimaRefusalCode::CompletionFailed,
                    error,
                    &guard,
                    "the completed Lima attempt could not be durably published",
                )
            })?;
        guard.settle_completed_attempt().map_err(|error| {
            PersonalWorkerLimaFailure::durable_after_guard(
                PersonalWorkerLimaRefusalCode::SettlementFailed,
                error,
                &guard,
                "the completed Lima attempt could not be atomically settled",
            )
        })?;

        let receipt = lifecycle.receipt();
        Ok(PersonalWorkerLimaExecution {
            schema_version: PERSONAL_WORKER_LIMA_ADAPTER_SCHEMA_VERSION,
            action: receipt.action,
            before_store_revision,
            before_queue_generation,
            after_store_revision: guard.store_revision(),
            after_queue_generation: guard.queue_generation(),
            after_state: receipt.after_state,
            after_profile: receipt.after_profile,
            after_profile_generation: receipt.after_generation,
        })
    }
}

struct LimaClockBridge<'a, C> {
    clock: &'a C,
}

impl<C: PersonalWorkerMacObservationClock> LimaObservationClock for LimaClockBridge<'_, C> {
    fn unix_seconds(&self) -> io::Result<u64> {
        self.clock.unix_millis().map(|millis| millis / 1_000)
    }
}

struct LifecycleCommandBridge<'a, E> {
    executor: &'a E,
}

impl<E: TimedCommandExecutor> CommandExecutor for LifecycleCommandBridge<'_, E> {
    fn execute(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord> {
        self.executor
            .execute_with_timeout(spec, LIMA_VERIFICATION_COMMAND_TIMEOUT)
    }
}

impl<E: TimedCommandExecutor> TimedCommandExecutor for LifecycleCommandBridge<'_, E> {
    fn execute_with_timeout(
        &self,
        spec: &CommandSpec,
        timeout: Duration,
    ) -> io::Result<ExecutionRecord> {
        self.executor.execute_with_timeout(spec, timeout)
    }
}

struct DurableExecutionJournal<'a, C> {
    guard: &'a mut UnixPersonalWorkerLimaAuthorityGuard,
    clock: &'a C,
    attempt_generation: PersonalWorkerLimaAttemptGeneration,
    mac: &'a PersonalWorkerMacObservation,
    observation_request: &'a LimaObservationRequest,
    lifecycle_executor: &'a LimaLifecycleExecutor,
    checkpoint_preflight_input: LimaLifecycleExecutionInput<'a>,
    private_failure: Option<PrivateFailure>,
}

impl<C: PersonalWorkerMacObservationClock> DurableExecutionJournal<'_, C> {
    fn recovery_report(&self) -> Option<PersonalWorkerLimaRecoveryReport> {
        recovery_report(self.guard)
    }
}

impl<C: PersonalWorkerMacObservationClock> LimaLifecycleExecutionJournal
    for DurableExecutionJournal<'_, C>
{
    fn checkpoint(
        &mut self,
        checkpoint: LimaLifecycleExecutionCheckpoint,
    ) -> Result<(), LimaLifecycleExecutionJournalError> {
        if let Err(error) = self
            .mac
            .confirm_lima_host_identity(self.observation_request)
        {
            self.private_failure = Some(PrivateFailure::Observation(Box::new(error)));
            return Err(LimaLifecycleExecutionJournalError);
        }
        let checkpoint_at = match read_time(self.clock) {
            Ok(time) => time,
            Err(error) => {
                self.private_failure = error.private;
                return Err(LimaLifecycleExecutionJournalError);
            }
        };
        let phase = match checkpoint {
            LimaLifecycleExecutionCheckpoint::StopStarted => {
                PersonalWorkerLimaAttemptPhase::StopStarted
            }
            LimaLifecycleExecutionCheckpoint::StopCompleted => {
                PersonalWorkerLimaAttemptPhase::StopCompleted
            }
            LimaLifecycleExecutionCheckpoint::EditStarted => {
                PersonalWorkerLimaAttemptPhase::EditStarted
            }
            LimaLifecycleExecutionCheckpoint::EditCompleted => {
                PersonalWorkerLimaAttemptPhase::EditCompleted
            }
            LimaLifecycleExecutionCheckpoint::StartStarted => {
                PersonalWorkerLimaAttemptPhase::StartStarted
            }
            LimaLifecycleExecutionCheckpoint::StartCompleted => {
                PersonalWorkerLimaAttemptPhase::StartCompleted
            }
            LimaLifecycleExecutionCheckpoint::VerifyStarted => {
                PersonalWorkerLimaAttemptPhase::VerifyStarted
            }
        };
        let Some(current) = self.guard.authority().cloned() else {
            return Err(LimaLifecycleExecutionJournalError);
        };
        let first_started = current
            .attempt()
            .is_some_and(|attempt| attempt.phase() == PersonalWorkerLimaAttemptPhase::Prepared);
        if first_started && !mac_execution_time_is_fresh(self.mac, checkpoint_at) {
            self.private_failure = Some(PrivateFailure::MacTiming);
            return Err(LimaLifecycleExecutionJournalError);
        }
        if first_started
            && let Err(error) = self
                .lifecycle_executor
                .validate_input_at(&self.checkpoint_preflight_input, checkpoint_at)
        {
            self.private_failure = Some(PrivateFailure::Lifecycle {
                failure: Box::new(error),
                checkpoint: None,
            });
            return Err(LimaLifecycleExecutionJournalError);
        }
        let next = match current.checkpoint(self.attempt_generation, phase, checkpoint_at) {
            Ok(next) => next,
            Err(error) => {
                self.private_failure = Some(PrivateFailure::Authority(error));
                return Err(LimaLifecycleExecutionJournalError);
            }
        };
        if let Err(error) = self
            .guard
            .replace_authority(current.authority_generation(), &next)
        {
            self.private_failure = Some(PrivateFailure::Durable(error));
            return Err(LimaLifecycleExecutionJournalError);
        }
        Ok(())
    }
}

fn read_time(
    clock: &impl PersonalWorkerMacObservationClock,
) -> Result<EpochMillis, PersonalWorkerLimaFailure> {
    let millis = clock
        .unix_millis()
        .map_err(|error| PersonalWorkerLimaFailure {
            code: PersonalWorkerLimaRefusalCode::InvalidInput,
            lifecycle_code: None,
            lifecycle_phase: None,
            durable_code: None,
            observation_kind: None,
            recovery: None,
            public_message: "the exact lifecycle clock could not be read",
            private: Some(PrivateFailure::Clock(error)),
        })?;
    EpochMillis::new(millis).map_err(|_| {
        PersonalWorkerLimaFailure::invalid(
            "the exact lifecycle clock is outside the supported range",
        )
    })
}

fn mac_execution_time_is_fresh(
    mac: &PersonalWorkerMacObservation,
    execution_time: EpochMillis,
) -> bool {
    let timing = mac.report().timing;
    execution_time.get() >= timing.observed_at_millis
        && execution_time.get() <= timing.expires_at_millis
}

fn validate_same_successor(
    first: &LimaInstanceObservationReport,
    second: &LimaInstanceObservationReport,
) -> Result<(), &'static str> {
    if first.instance != second.instance
        || first.configured != second.configured
        || first.guest != second.guest
    {
        return Err("the sealed post-observation drifted from the exact executor successor");
    }
    Ok(())
}

fn post_lifecycle_projection(
    previous: &LimaLifecycleObservation,
    execution: &LimaLifecycleExecution,
    mac: &PersonalWorkerMacObservation,
) -> Result<LimaLifecycleObservation, &'static str> {
    let receipt = execution.receipt();
    let observed_at = EpochMillis::new(mac.report().timing.observed_at_millis)
        .map_err(|_| "the sealed post-observation time is outside the supported range")?;
    let idle_deadline = previous
        .last_activity_at()
        .get()
        .checked_add(receipt.after_profile.idle_deadline_offset_millis())
        .and_then(|value| EpochMillis::new(value).ok())
        .ok_or("the exact post-lifecycle idle deadline cannot be represented")?;
    LimaLifecycleObservation::new(LimaLifecycleObservationDefinition {
        identity: receipt.identity.clone(),
        state: receipt.after_state,
        profile: receipt.after_profile,
        profile_generation: receipt.after_generation,
        observed_resources: LimaObservedResources::for_profile(receipt.after_profile),
        observed_at,
        active_reservation_id: None,
        last_activity_at: previous.last_activity_at(),
        idle_deadline,
        graceful_stop_acknowledgement: None,
    })
    .map_err(|_| "the exact post-lifecycle successor projection is invalid")
}

fn recovery_report(
    guard: &UnixPersonalWorkerLimaAuthorityGuard,
) -> Option<PersonalWorkerLimaRecoveryReport> {
    if guard.recovery_required() {
        return None;
    }
    guard
        .authority()
        .map(PersonalWorkerLimaAuthorityDocument::recovery_report)
}

fn boxed_recovery(
    recovery: Option<PersonalWorkerLimaRecoveryReport>,
) -> Option<Box<PersonalWorkerLimaRecoveryReport>> {
    recovery.map(Box::new)
}

const fn durable_error_code(kind: UnixPersonalWorkerLimaAuthorityErrorKind) -> &'static str {
    match kind {
        UnixPersonalWorkerLimaAuthorityErrorKind::InvalidDocument => "invalid_document",
        UnixPersonalWorkerLimaAuthorityErrorKind::RevisionConflict => "revision_conflict",
        UnixPersonalWorkerLimaAuthorityErrorKind::RecoveryRequired => "recovery_required",
        UnixPersonalWorkerLimaAuthorityErrorKind::Busy => "busy",
        UnixPersonalWorkerLimaAuthorityErrorKind::Missing => "missing",
        UnixPersonalWorkerLimaAuthorityErrorKind::Io => "io",
        UnixPersonalWorkerLimaAuthorityErrorKind::UnsafeFilesystem => "unsafe_filesystem",
        UnixPersonalWorkerLimaAuthorityErrorKind::VersionIncompatible => {
            "durable_state_version_incompatible"
        }
        UnixPersonalWorkerLimaAuthorityErrorKind::CorruptState => "corrupt_state",
    }
}

impl PersonalWorkerLimaFailure {
    fn invalid(public_message: &'static str) -> Self {
        Self {
            code: PersonalWorkerLimaRefusalCode::InvalidInput,
            lifecycle_code: None,
            lifecycle_phase: None,
            durable_code: None,
            observation_kind: None,
            recovery: None,
            public_message,
            private: None,
        }
    }

    fn recovery(
        recovery: Option<PersonalWorkerLimaRecoveryReport>,
        public_message: &'static str,
    ) -> Self {
        Self {
            code: PersonalWorkerLimaRefusalCode::RecoveryRequired,
            lifecycle_code: None,
            lifecycle_phase: None,
            durable_code: None,
            observation_kind: None,
            recovery: boxed_recovery(recovery),
            public_message,
            private: None,
        }
    }

    fn durable(error: UnixPersonalWorkerLimaAuthorityError) -> Self {
        let code = if error.kind() == UnixPersonalWorkerLimaAuthorityErrorKind::RecoveryRequired {
            PersonalWorkerLimaRefusalCode::RecoveryRequired
        } else {
            PersonalWorkerLimaRefusalCode::DurableStateUnavailable
        };
        Self::durable_with_recovery(
            code,
            error,
            None,
            "durable Lima authority could not be opened or published",
        )
    }

    fn durable_with_recovery(
        code: PersonalWorkerLimaRefusalCode,
        error: UnixPersonalWorkerLimaAuthorityError,
        recovery: Option<PersonalWorkerLimaRecoveryReport>,
        public_message: &'static str,
    ) -> Self {
        Self {
            code,
            lifecycle_code: None,
            lifecycle_phase: None,
            durable_code: Some(durable_error_code(error.kind())),
            observation_kind: None,
            recovery: boxed_recovery(recovery),
            public_message,
            private: Some(PrivateFailure::Durable(error)),
        }
    }

    fn durable_after_guard(
        code: PersonalWorkerLimaRefusalCode,
        error: UnixPersonalWorkerLimaAuthorityError,
        guard: &UnixPersonalWorkerLimaAuthorityGuard,
        public_message: &'static str,
    ) -> Self {
        let uncertain = guard.recovery_required();
        Self::durable_with_recovery(
            if uncertain {
                PersonalWorkerLimaRefusalCode::RecoveryRequired
            } else {
                code
            },
            error,
            recovery_report(guard),
            if uncertain {
                "durable Lima publication is ambiguous and requires recovery"
            } else {
                public_message
            },
        )
    }

    fn authority_invalid(error: PersonalWorkerLimaAuthorityError) -> Self {
        Self {
            code: PersonalWorkerLimaRefusalCode::InvalidInput,
            lifecycle_code: None,
            lifecycle_phase: None,
            durable_code: None,
            observation_kind: None,
            recovery: None,
            public_message: "the sealed tick does not authorize one exact Lima lifecycle attempt",
            private: Some(PrivateFailure::Authority(error)),
        }
    }

    fn source_identity(error: PersonalWorkerMacObservationError) -> Self {
        let observation_kind = error.kind;
        Self {
            code: PersonalWorkerLimaRefusalCode::SourceIdentityDrift,
            lifecycle_code: None,
            lifecycle_phase: None,
            durable_code: None,
            observation_kind: Some(observation_kind),
            recovery: None,
            public_message: "the sealed Lima host identity changed before lifecycle execution",
            private: Some(PrivateFailure::Observation(Box::new(error))),
        }
    }

    fn lifecycle_without_recovery(error: LimaLifecycleExecutionFailure) -> Self {
        Self::lifecycle(error, None, None)
    }

    fn lifecycle(
        error: LimaLifecycleExecutionFailure,
        recovery: Option<PersonalWorkerLimaRecoveryReport>,
        private: Option<PrivateFailure>,
    ) -> Self {
        let lifecycle_code = error.code;
        let lifecycle_phase = error.phase;
        let observation_kind = private.as_ref().and_then(PrivateFailure::observation_kind);
        let durable_kind = private.as_ref().and_then(PrivateFailure::durable_kind);
        let ambiguous_publication = recovery.is_none() && durable_kind.is_some();
        Self {
            code: if ambiguous_publication {
                PersonalWorkerLimaRefusalCode::RecoveryRequired
            } else {
                PersonalWorkerLimaRefusalCode::LifecycleRefusal
            },
            lifecycle_code: Some(lifecycle_code),
            lifecycle_phase: Some(lifecycle_phase),
            durable_code: durable_kind.map(durable_error_code),
            observation_kind,
            recovery: boxed_recovery(recovery),
            public_message: if ambiguous_publication {
                "durable Lima checkpoint publication is ambiguous and requires recovery"
            } else {
                error.public_message
            },
            private: Some(PrivateFailure::Lifecycle {
                failure: Box::new(error),
                checkpoint: private.map(Box::new),
            }),
        }
    }

    fn observation(
        error: PersonalWorkerMacObservationError,
        recovery: Option<PersonalWorkerLimaRecoveryReport>,
    ) -> Self {
        let kind = error.kind;
        Self {
            code: PersonalWorkerLimaRefusalCode::PostObservationFailed,
            lifecycle_code: None,
            lifecycle_phase: None,
            durable_code: None,
            observation_kind: Some(kind),
            recovery: boxed_recovery(recovery),
            public_message: "fresh sealed post-action observation could not be established",
            private: Some(PrivateFailure::Observation(Box::new(error))),
        }
    }

    fn completion(
        public_message: &'static str,
        recovery: Option<PersonalWorkerLimaRecoveryReport>,
        error: Option<PersonalWorkerLimaAuthorityError>,
    ) -> Self {
        Self {
            code: PersonalWorkerLimaRefusalCode::CompletionFailed,
            lifecycle_code: None,
            lifecycle_phase: None,
            durable_code: None,
            observation_kind: None,
            recovery: boxed_recovery(recovery),
            public_message,
            private: error.map(PrivateFailure::Authority),
        }
    }
}
