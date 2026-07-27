use std::fmt;

use serde::Serialize;

use crate::execution_admission::EpochMillis;
use crate::lima_lifecycle::{LimaLifecycleObservation, MAX_LIMA_OBSERVATION_AGE_MILLIS};
use crate::lima_observation::{
    LimaInstanceName, LimaInstanceObservationReport, LimaObservationClock, LimaObservationRequest,
    LimaPersistentIdentity,
};
use crate::personal_worker_host_broker::{
    HostBrokerAction, HostBrokerPlan, HostBrokerStateRevision,
};
use crate::personal_worker_queue::PersonalWorkerQueueGeneration;
use crate::process::CommandExecutor;

#[path = "lima_lifecycle_executor_inner.rs"]
mod lima_lifecycle_executor_inner;

use self::lima_lifecycle_executor_inner as inner;

pub use inner::{
    LIMA_LIFECYCLE_EXECUTOR_SCHEMA_VERSION, LimaLifecycleExecution, LimaLifecycleExecutionAction,
    LimaLifecycleExecutionPhase, LimaLifecycleExecutionPrivateEvidence,
    LimaLifecycleExecutionReceipt, LimaLifecycleExecutionRefusalCode,
    LimaLifecycleObservationSource, LimaLifecycleObservationSourceError,
    LimaLifecyclePrivateCommandEvidence, MAX_LIMA_LIFECYCLE_ACTION_AGE_MILLIS,
    MAX_LIMA_LIFECYCLE_EXECUTOR_OUTPUT_BYTES,
};

const REDACTED_EXECUTION_EVIDENCE: &str = "<private-lima-lifecycle-execution-evidence>";

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct LimaLifecycleExecutionFailure {
    pub code: LimaLifecycleExecutionRefusalCode,
    pub phase: LimaLifecycleExecutionPhase,
    pub public_message: &'static str,
    #[serde(skip)]
    private_evidence: LimaLifecycleExecutionPrivateEvidence,
}

impl LimaLifecycleExecutionFailure {
    fn guard(code: LimaLifecycleExecutionRefusalCode, public_message: &'static str) -> Self {
        Self {
            code,
            phase: LimaLifecycleExecutionPhase::InputValidation,
            public_message,
            private_evidence: LimaLifecycleExecutionPrivateEvidence::default(),
        }
    }

    fn from_inner(error: inner::LimaLifecycleExecutionFailure) -> Self {
        Self {
            code: error.code,
            phase: error.phase,
            public_message: error.public_message,
            private_evidence: error.private_evidence().clone(),
        }
    }

    #[must_use]
    pub const fn private_evidence(&self) -> &LimaLifecycleExecutionPrivateEvidence {
        &self.private_evidence
    }
}

impl fmt::Debug for LimaLifecycleExecutionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LimaLifecycleExecutionFailure")
            .field("code", &self.code)
            .field("phase", &self.phase)
            .field("public_message", &self.public_message)
            .field("private_evidence", &REDACTED_EXECUTION_EVIDENCE)
            .finish()
    }
}

impl fmt::Display for LimaLifecycleExecutionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.public_message)
    }
}

impl std::error::Error for LimaLifecycleExecutionFailure {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedLimaLifecycleAction {
    inner: inner::AcceptedLimaLifecycleAction,
    state_revision: HostBrokerStateRevision,
    queue_generation: PersonalWorkerQueueGeneration,
    decision_at: EpochMillis,
}

impl AcceptedLimaLifecycleAction {
    /// Retain one executable action and its exact durable broker authority.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal unless the plan is accepted by the fixed Lima executor.
    pub fn from_plan(plan: &HostBrokerPlan) -> Result<Self, LimaLifecycleExecutionFailure> {
        Ok(Self {
            inner: inner::AcceptedLimaLifecycleAction::from_plan(plan)
                .map_err(LimaLifecycleExecutionFailure::from_inner)?,
            state_revision: plan.state_revision(),
            queue_generation: plan.queue_generation(),
            decision_at: plan.decision_at(),
        })
    }

    #[must_use]
    pub const fn state_revision(&self) -> HostBrokerStateRevision {
        self.state_revision
    }

    #[must_use]
    pub const fn queue_generation(&self) -> PersonalWorkerQueueGeneration {
        self.queue_generation
    }

    #[must_use]
    pub const fn decision_at(&self) -> EpochMillis {
        self.decision_at
    }

    #[must_use]
    pub const fn action(&self) -> &HostBrokerAction {
        self.inner.action()
    }
}

pub struct LimaLifecycleExecutionInput<'a> {
    pub accepted: &'a AcceptedLimaLifecycleAction,
    /// Exact durable broker/store revision read immediately before execution.
    pub current_state_revision: HostBrokerStateRevision,
    /// Exact durable queue generation read immediately before execution.
    pub current_queue_generation: PersonalWorkerQueueGeneration,
    pub lifecycle: &'a LimaLifecycleObservation,
    pub current: &'a LimaInstanceObservationReport,
    pub expected_persistent_identity: &'a LimaPersistentIdentity,
    pub observation_request: &'a LimaObservationRequest,
}

#[derive(Clone, PartialEq, Eq)]
pub struct LimaLifecycleExecutor {
    inner: inner::LimaLifecycleExecutor,
}

impl LimaLifecycleExecutor {
    /// Construct the fixed direct-command executor for one reviewed Lima instance.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal unless the private executable/home inputs are accepted.
    pub fn new(
        limactl_program: impl Into<std::path::PathBuf>,
        lima_home: impl Into<std::path::PathBuf>,
        instance: LimaInstanceName,
    ) -> Result<Self, LimaLifecycleExecutionFailure> {
        inner::LimaLifecycleExecutor::new(limactl_program, lima_home, instance)
            .map(|inner| Self { inner })
            .map_err(LimaLifecycleExecutionFailure::from_inner)
    }

    /// Rebind durable authority and lifecycle freshness, then execute one fixed action.
    ///
    /// # Errors
    ///
    /// Refuses before the first command on broker revision drift, queue-generation drift,
    /// lifecycle observation staleness/futurity, clock failure, or any inner executor refusal.
    pub fn execute<O, E, C>(
        &self,
        input: LimaLifecycleExecutionInput<'_>,
        observation_source: &O,
        command_executor: &E,
        clock: &C,
    ) -> Result<LimaLifecycleExecution, LimaLifecycleExecutionFailure>
    where
        O: LimaLifecycleObservationSource,
        E: CommandExecutor,
        C: LimaObservationClock,
    {
        let execution_unix_seconds = clock.unix_seconds().map_err(|_| {
            LimaLifecycleExecutionFailure::guard(
                LimaLifecycleExecutionRefusalCode::ClockFailure,
                "the lifecycle execution clock could not be read",
            )
        })?;

        guard_then(
            input.accepted.state_revision,
            input.accepted.queue_generation,
            input.current_state_revision,
            input.current_queue_generation,
            input.lifecycle.observed_at(),
            execution_unix_seconds,
            || {
                self.inner.execute(
                    inner::LimaLifecycleExecutionInput {
                        accepted: &input.accepted.inner,
                        lifecycle: input.lifecycle,
                        current: input.current,
                        expected_persistent_identity: input.expected_persistent_identity,
                        observation_request: input.observation_request,
                    },
                    observation_source,
                    command_executor,
                    clock,
                )
            },
        )
    }
}

impl fmt::Debug for LimaLifecycleExecutor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LimaLifecycleExecutor")
            .field("inner", &self.inner)
            .finish()
    }
}

fn guard_then<T, F>(
    accepted_state_revision: HostBrokerStateRevision,
    accepted_queue_generation: PersonalWorkerQueueGeneration,
    current_state_revision: HostBrokerStateRevision,
    current_queue_generation: PersonalWorkerQueueGeneration,
    lifecycle_observed_at: EpochMillis,
    execution_unix_seconds: u64,
    execute: F,
) -> Result<T, LimaLifecycleExecutionFailure>
where
    F: FnOnce() -> Result<T, inner::LimaLifecycleExecutionFailure>,
{
    if current_state_revision != accepted_state_revision {
        return Err(LimaLifecycleExecutionFailure::guard(
            LimaLifecycleExecutionRefusalCode::StateMismatch,
            "the durable broker/store revision changed after lifecycle planning",
        ));
    }
    if current_queue_generation != accepted_queue_generation {
        return Err(LimaLifecycleExecutionFailure::guard(
            LimaLifecycleExecutionRefusalCode::GenerationMismatch,
            "the durable queue generation changed after lifecycle planning",
        ));
    }

    let execution_millis = execution_unix_seconds.checked_mul(1_000).ok_or_else(|| {
        LimaLifecycleExecutionFailure::guard(
            LimaLifecycleExecutionRefusalCode::ClockFailure,
            "the lifecycle execution clock exceeded the reviewed range",
        )
    })?;
    let observed_millis = lifecycle_observed_at.get();
    let age = execution_millis
        .checked_sub(observed_millis)
        .ok_or_else(|| {
            LimaLifecycleExecutionFailure::guard(
                LimaLifecycleExecutionRefusalCode::StaleObservation,
                "the lifecycle observation is from the future at execution time",
            )
        })?;
    if age > MAX_LIMA_OBSERVATION_AGE_MILLIS {
        return Err(LimaLifecycleExecutionFailure::guard(
            LimaLifecycleExecutionRefusalCode::StaleObservation,
            "the lifecycle observation expired before execution",
        ));
    }

    execute().map_err(LimaLifecycleExecutionFailure::from_inner)
}

#[cfg(test)]
mod tests;
