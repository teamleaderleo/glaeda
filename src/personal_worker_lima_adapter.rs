use std::fmt;

use serde::Serialize;

use crate::lima_lifecycle::LimaLifecycleObservation;
use crate::lima_lifecycle_executor::{
    AcceptedLimaLifecycleAction, LimaLifecycleExecution, LimaLifecycleExecutionFailure,
    LimaLifecycleExecutionInput, LimaLifecycleExecutionPhase, LimaLifecycleExecutionRefusalCode,
    LimaLifecycleExecutor, LimaLifecycleObservationSource,
};
use crate::lima_observation::{
    LimaGuestObservation, LimaObservationClock, LimaObservationRequest, LimaPersistentIdentity,
};
use crate::personal_worker_host_broker::HostBrokerStateRevision;
use crate::personal_worker_mac_observation::PersonalWorkerMacObservation;
use crate::personal_worker_queue::PersonalWorkerQueueGeneration;
use crate::personal_worker_store::PersonalWorkerStoreRevision;
use crate::personal_worker_tick::PersonalWorkerTickPlan;
use crate::process::CommandExecutor;

pub const PERSONAL_WORKER_LIMA_ADAPTER_SCHEMA_VERSION: u8 = 1;

const REDACTED_LIFECYCLE_FAILURE: &str = "<private-lima-lifecycle-failure>";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerLimaRefusalCode {
    InvalidInput,
    LimaSourceMismatch,
    PersistentIdentityMismatch,
    LifecycleRefusal,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerLimaFailure {
    pub code: PersonalWorkerLimaRefusalCode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle_code: Option<LimaLifecycleExecutionRefusalCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle_phase: Option<LimaLifecycleExecutionPhase>,
    pub public_message: &'static str,
    #[serde(skip)]
    lifecycle_failure: Option<LimaLifecycleExecutionFailure>,
}

impl PersonalWorkerLimaFailure {
    fn input(code: PersonalWorkerLimaRefusalCode, public_message: &'static str) -> Self {
        Self {
            code,
            lifecycle_code: None,
            lifecycle_phase: None,
            public_message,
            lifecycle_failure: None,
        }
    }

    fn lifecycle(failure: LimaLifecycleExecutionFailure) -> Self {
        Self {
            code: PersonalWorkerLimaRefusalCode::LifecycleRefusal,
            lifecycle_code: Some(failure.code),
            lifecycle_phase: Some(failure.phase),
            public_message: failure.public_message,
            lifecycle_failure: Some(failure),
        }
    }

    #[must_use]
    pub const fn lifecycle_failure(&self) -> Option<&LimaLifecycleExecutionFailure> {
        self.lifecycle_failure.as_ref()
    }
}

impl fmt::Debug for PersonalWorkerLimaFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersonalWorkerLimaFailure")
            .field("code", &self.code)
            .field("lifecycle_code", &self.lifecycle_code)
            .field("lifecycle_phase", &self.lifecycle_phase)
            .field("public_message", &self.public_message)
            .field("lifecycle_failure", &REDACTED_LIFECYCLE_FAILURE)
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
    pub plan: &'a PersonalWorkerTickPlan,
    pub current_store_revision: PersonalWorkerStoreRevision,
    pub current_queue_generation: PersonalWorkerQueueGeneration,
    pub lifecycle: &'a LimaLifecycleObservation,
    pub mac: &'a PersonalWorkerMacObservation,
    pub expected_persistent_identity: &'a LimaPersistentIdentity,
    pub observation_request: &'a LimaObservationRequest,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerLimaExecution {
    schema_version: u8,
    store_revision: PersonalWorkerStoreRevision,
    lifecycle: LimaLifecycleExecution,
}

impl PersonalWorkerLimaExecution {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn store_revision(&self) -> PersonalWorkerStoreRevision {
        self.store_revision
    }

    #[must_use]
    pub const fn lifecycle(&self) -> &LimaLifecycleExecution {
        &self.lifecycle
    }
}

impl fmt::Debug for PersonalWorkerLimaExecution {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersonalWorkerLimaExecution")
            .field("schema_version", &self.schema_version)
            .field("store_revision", &self.store_revision)
            .field("lifecycle", &self.lifecycle)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PersonalWorkerLimaAdapter;

impl PersonalWorkerLimaAdapter {
    /// Execute one sealed lifecycle tick through the fixed reviewed Lima executor.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal before mutation for an unsupported tick, stale durable authority,
    /// private Lima source drift, lifecycle/profile/resource drift, active work, or expected
    /// persistent-identity mismatch. Post-mutation command and verification failures retain the
    /// existing lifecycle executor's exact bounded classification.
    pub fn execute<O, E, C>(
        &self,
        input: PersonalWorkerLimaInput<'_>,
        lifecycle_executor: &LimaLifecycleExecutor,
        observation_source: &O,
        command_executor: &E,
        clock: &C,
    ) -> Result<PersonalWorkerLimaExecution, PersonalWorkerLimaFailure>
    where
        O: LimaLifecycleObservationSource,
        E: CommandExecutor,
        C: LimaObservationClock,
    {
        let accepted = AcceptedLimaLifecycleAction::from_personal_worker_tick(input.plan)
            .map_err(PersonalWorkerLimaFailure::lifecycle)?;
        if input.mac.lima_source_identity() != &input.observation_request.source_identity() {
            return Err(PersonalWorkerLimaFailure::input(
                PersonalWorkerLimaRefusalCode::LimaSourceMismatch,
                "the sealed Mac observation and lifecycle request use different private Lima sources",
            ));
        }
        if let LimaGuestObservation::Observed(guest) = &input.mac.report().lima.guest
            && &guest.persistent_identity != input.expected_persistent_identity
        {
            return Err(PersonalWorkerLimaFailure::input(
                PersonalWorkerLimaRefusalCode::PersistentIdentityMismatch,
                "the sealed pre-mutation guest identity differs from the expected persistent identity",
            ));
        }
        let current_broker_state_revision = HostBrokerStateRevision::new(
            input.current_store_revision.get(),
        )
        .map_err(|_| {
            PersonalWorkerLimaFailure::input(
                PersonalWorkerLimaRefusalCode::InvalidInput,
                "the current personal-worker store revision is not supported by the Lima executor",
            )
        })?;
        let lifecycle = lifecycle_executor
            .execute(
                LimaLifecycleExecutionInput {
                    accepted: &accepted,
                    current_broker_state_revision,
                    current_queue_generation: input.current_queue_generation,
                    lifecycle: input.lifecycle,
                    current: &input.mac.report().lima,
                    expected_persistent_identity: input.expected_persistent_identity,
                    observation_request: input.observation_request,
                },
                observation_source,
                command_executor,
                clock,
            )
            .map_err(PersonalWorkerLimaFailure::lifecycle)?;
        Ok(PersonalWorkerLimaExecution {
            schema_version: PERSONAL_WORKER_LIMA_ADAPTER_SCHEMA_VERSION,
            store_revision: input.plan.store_revision(),
            lifecycle,
        })
    }
}
