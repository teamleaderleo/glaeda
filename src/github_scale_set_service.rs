// The coordinator stays private until operator enrollment and the launchd service entry point are
// wired around this bounded phase dispatcher.
#![allow(dead_code)]

use std::cell::RefCell;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::disposable_clone_runtime::{
    CloneRuntimeClock, DisposableCleanupRunnerSource, DisposableCleanupTransactionOutcome,
    DisposableCloneAdmissionObservation, DisposableCloneAdmissionSource, DisposableCloneRuntime,
    DisposableCloneRuntimeError, DisposableCloneTransactionOutcome, PendingCloneScaleSetMessage,
    admission_seal,
};
use crate::disposable_runner_runtime::{
    DisposableRunnerRegistrationSource, DisposableRunnerRuntime, DisposableRunnerRuntimeError,
};
use crate::disposable_worker_reconciler::DisposableAttemptId;
use crate::disposable_worker_reconciler::DisposableAttemptPhase;
use crate::execution_admission::EpochMillis;
use crate::github_scale_set_bridge::{
    ScaleSetBridgeClient, ScaleSetBridgeError, ScaleSetBridgeIdentity, ScaleSetBridgePoll,
    ScaleSetJitReceipt, ScaleSetRunnerLookup, ScaleSetStatistics,
};
use crate::github_scale_set_consumer::{
    ScaleSetConsumerError, ScaleSetConsumerPolicy, apply_scale_set_ack_outcome,
    apply_scale_set_event,
};
use crate::github_scale_set_inbox::ScaleSetInboxError;
use crate::github_scale_set_protocol::{ScaleSetRunnerName, ScaleSetRunnerReference};
use crate::process::TimedCommandExecutor;
use crate::unix_personal_worker_store::DisposableRunnerTransactionOutcome;
use crate::unix_personal_worker_store::UnixPersonalWorkerStore;

const MESSAGE_FRESHNESS_MILLIS: u64 = 30_000;

pub(crate) trait ScaleSetBridgeSession {
    fn poll(&mut self, available_capacity: u16) -> Result<ScaleSetBridgePoll, ScaleSetBridgeError>;
    fn ack(&mut self, message_id: u32) -> Result<Vec<u64>, ScaleSetBridgeError>;
}

pub(crate) trait ScaleSetRunnerBridgeSession {
    fn generate_jit(
        &mut self,
        runner_name: &ScaleSetRunnerName,
    ) -> Result<ScaleSetJitReceipt, ScaleSetBridgeError>;

    fn observe_runner(
        &mut self,
        runner_name: &ScaleSetRunnerName,
    ) -> Result<ScaleSetRunnerLookup, ScaleSetBridgeError>;

    fn remove_runner(
        &mut self,
        runner: &ScaleSetRunnerReference,
    ) -> Result<(), ScaleSetBridgeError>;
}

impl ScaleSetBridgeSession for ScaleSetBridgeClient {
    fn poll(&mut self, available_capacity: u16) -> Result<ScaleSetBridgePoll, ScaleSetBridgeError> {
        ScaleSetBridgeClient::poll(self, available_capacity)
    }

    fn ack(&mut self, message_id: u32) -> Result<Vec<u64>, ScaleSetBridgeError> {
        ScaleSetBridgeClient::ack(self, message_id)
    }
}

impl ScaleSetRunnerBridgeSession for ScaleSetBridgeClient {
    fn generate_jit(
        &mut self,
        runner_name: &ScaleSetRunnerName,
    ) -> Result<ScaleSetJitReceipt, ScaleSetBridgeError> {
        ScaleSetBridgeClient::generate_jit(self, runner_name)
    }

    fn observe_runner(
        &mut self,
        runner_name: &ScaleSetRunnerName,
    ) -> Result<ScaleSetRunnerLookup, ScaleSetBridgeError> {
        ScaleSetBridgeClient::observe_runner(self, runner_name)
    }

    fn remove_runner(
        &mut self,
        runner: &ScaleSetRunnerReference,
    ) -> Result<(), ScaleSetBridgeError> {
        ScaleSetBridgeClient::remove_runner(self, runner)
    }
}

pub(crate) trait ScaleSetServiceClock {
    fn now(&self) -> Result<EpochMillis, ScaleSetServiceError>;
}

struct SystemScaleSetServiceClock;

impl ScaleSetServiceClock for SystemScaleSetServiceClock {
    fn now(&self) -> Result<EpochMillis, ScaleSetServiceError> {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| ScaleSetServiceError::new("scale_set_clock_unavailable"))?
            .as_millis();
        let millis = u64::try_from(millis)
            .map_err(|_| ScaleSetServiceError::new("scale_set_clock_unavailable"))?;
        EpochMillis::new(millis)
            .map_err(|_| ScaleSetServiceError::new("scale_set_clock_unavailable"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScaleSetServiceDisposition {
    Idle(ScaleSetStatistics),
    IdleObservationRecorded {
        attempt_id: String,
    },
    MessagePersisted {
        message_id: u32,
    },
    CloneAuthorized {
        attempt_id: String,
    },
    CloneCompleted {
        attempt_id: String,
    },
    RegistrationCheckpointed {
        attempt_id: String,
    },
    RunnerRegistrationRecovered {
        attempt_id: String,
    },
    RunnerCommandCompleted {
        attempt_id: String,
    },
    CleanupCheckpointed {
        attempt_id: String,
        phase: DisposableAttemptPhase,
    },
    VmDestroyed {
        attempt_id: String,
    },
    RunnerDeleted {
        attempt_id: String,
    },
    CapacityReleased {
        attempt_id: String,
    },
    EventApplied {
        message_id: u32,
        event_index: usize,
    },
    MessageAcknowledged {
        message_id: u32,
    },
    AckOutcomeApplied {
        message_id: u32,
    },
    UnprovisionedReleased {
        attempt_id: String,
    },
    AttemptRetired {
        attempt_id: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SupervisedOperation {
    Control,
    AuthorizeClone,
    ExecuteClone,
    CheckpointRegistration,
    RunRegistered,
    Cleanup,
}

fn supervised_operation(
    phase: DisposableAttemptPhase,
    runner_started: bool,
) -> Result<SupervisedOperation, ScaleSetServiceError> {
    match phase {
        DisposableAttemptPhase::Reserved => Ok(SupervisedOperation::AuthorizeClone),
        DisposableAttemptPhase::CloneAuthorized => Ok(SupervisedOperation::ExecuteClone),
        DisposableAttemptPhase::CloneStarted => Ok(SupervisedOperation::CheckpointRegistration),
        DisposableAttemptPhase::Registering | DisposableAttemptPhase::Assigned
            if !runner_started =>
        {
            Ok(SupervisedOperation::RunRegistered)
        }
        DisposableAttemptPhase::Terminal
        | DisposableAttemptPhase::Destroying
        | DisposableAttemptPhase::Deregistering
        | DisposableAttemptPhase::Releasing => Ok(SupervisedOperation::Cleanup),
        DisposableAttemptPhase::UnprovisionedReleasing
        | DisposableAttemptPhase::Complete
        | DisposableAttemptPhase::Registering
        | DisposableAttemptPhase::Waiting
        | DisposableAttemptPhase::Assigned
        | DisposableAttemptPhase::Running => Ok(SupervisedOperation::Control),
        DisposableAttemptPhase::Provisioning => Err(ScaleSetServiceError::new(
            "scale_set_legacy_provisioning_recovery_required",
        )),
    }
}

pub(crate) struct ScaleSetService<B, C> {
    store: UnixPersonalWorkerStore,
    bridge: B,
    policy: ScaleSetConsumerPolicy,
    source_identity: ScaleSetBridgeIdentity,
    clock: C,
}

impl ScaleSetService<ScaleSetBridgeClient, SystemScaleSetServiceClock> {
    pub(crate) fn new(
        store: UnixPersonalWorkerStore,
        bridge: ScaleSetBridgeClient,
        policy: ScaleSetConsumerPolicy,
    ) -> Result<Self, ScaleSetServiceError> {
        Self::with_parts(store, bridge, policy, SystemScaleSetServiceClock)
    }
}

impl<B: ScaleSetBridgeSession, C: ScaleSetServiceClock> ScaleSetService<B, C> {
    fn with_parts(
        mut store: UnixPersonalWorkerStore,
        bridge: B,
        policy: ScaleSetConsumerPolicy,
        clock: C,
    ) -> Result<Self, ScaleSetServiceError> {
        let source_identity = policy.source_identity().clone();
        store
            .initialize_scale_set_inbox(&source_identity)
            .map_err(ScaleSetServiceError::from_inbox)?;
        Ok(Self {
            store,
            bridge,
            policy,
            source_identity,
            clock,
        })
    }

    pub(crate) fn reconcile_once(
        &mut self,
    ) -> Result<ScaleSetServiceDisposition, ScaleSetServiceError> {
        let (inbox, catalog) = self
            .store
            .load_scale_set_control_state(&self.source_identity)
            .map_err(ScaleSetServiceError::from_inbox)?;

        if let Some(pending) = inbox.pending() {
            if pending.ack_started() {
                return Err(ScaleSetServiceError::new("scale_set_ack_outcome_unknown"));
            }
            if pending.next_event().is_some() {
                let message_id = pending.message_id();
                let event_index = pending.next_event_index();
                let policy = &self.policy;
                let clock = &self.clock;
                self.store
                    .apply_next_scale_set_event(
                        &self.source_identity,
                        inbox.revision(),
                        |pending, event, catalog| {
                            let now = clock.now()?;
                            apply_scale_set_event(policy, pending, event, catalog, now)
                                .map(|next| (next, ()))
                                .map_err(ScaleSetServiceError::from_consumer)
                        },
                    )
                    .map_err(ScaleSetServiceError::from_inbox)??;
                return Ok(ScaleSetServiceDisposition::EventApplied {
                    message_id,
                    event_index,
                });
            }

            let message_id = pending.message_id();
            let bridge = &mut self.bridge;
            self.store
                .acknowledge_scale_set_message(
                    &self.source_identity,
                    inbox.revision(),
                    |message_id| {
                        bridge
                            .ack(message_id)
                            .map_err(ScaleSetServiceError::from_bridge)
                    },
                )
                .map_err(ScaleSetServiceError::from_inbox)??;
            return Ok(ScaleSetServiceDisposition::MessageAcknowledged { message_id });
        }

        if let Some(receipt) = inbox
            .last_ack()
            .filter(|receipt| !receipt.outcome_applied())
        {
            let message_id = receipt.message_id();
            let policy = &self.policy;
            self.store
                .apply_scale_set_ack_outcome(
                    &self.source_identity,
                    inbox.revision(),
                    |receipt, catalog| {
                        apply_scale_set_ack_outcome(policy, receipt, catalog)
                            .map(|next| (next, ()))
                            .map_err(ScaleSetServiceError::from_consumer)
                    },
                )
                .map_err(ScaleSetServiceError::from_inbox)??;
            return Ok(ScaleSetServiceDisposition::AckOutcomeApplied { message_id });
        }

        let mut releasing = catalog.active().iter().filter(|reservation| {
            reservation.attempt().phase() == DisposableAttemptPhase::UnprovisionedReleasing
        });
        if let Some(reservation) = releasing.next() {
            if releasing.next().is_some() {
                return Err(ScaleSetServiceError::new(
                    "scale_set_capacity_invariant_violated",
                ));
            }
            let attempt_id = reservation.attempt().attempt_id().clone();
            self.store
                .complete_scale_set_unprovisioned_attempt(&self.source_identity, &attempt_id)
                .map_err(ScaleSetServiceError::from_inbox)?;
            return Ok(ScaleSetServiceDisposition::UnprovisionedReleased {
                attempt_id: attempt_id.as_str().to_owned(),
            });
        }

        if let Some(reservation) = catalog
            .active()
            .iter()
            .find(|reservation| reservation.attempt().phase() == DisposableAttemptPhase::Complete)
        {
            let attempt_id = reservation.attempt().attempt_id().clone();
            self.store
                .retire_scale_set_complete_attempt(&self.source_identity, &attempt_id)
                .map_err(ScaleSetServiceError::from_inbox)?;
            return Ok(ScaleSetServiceDisposition::AttemptRetired {
                attempt_id: attempt_id.as_str().to_owned(),
            });
        }

        let bridge = &mut self.bridge;
        let clock = &self.clock;
        let (response, attempt_id) = self
            .store
            .poll_and_record_scale_set(&self.source_identity, |available_capacity| {
                let response = bridge
                    .poll(available_capacity)
                    .map_err(ScaleSetServiceError::from_bridge)?;
                let observed_at = clock.now()?;
                let not_after = observed_at
                    .get()
                    .checked_add(MESSAGE_FRESHNESS_MILLIS)
                    .and_then(|value| EpochMillis::new(value).ok())
                    .ok_or_else(|| ScaleSetServiceError::new("scale_set_clock_unavailable"))?;
                Ok((response, observed_at, not_after))
            })
            .map_err(ScaleSetServiceError::from_inbox)??;
        match response {
            ScaleSetBridgePoll::Idle { statistics } => match attempt_id {
                Some(attempt_id) => Ok(ScaleSetServiceDisposition::IdleObservationRecorded {
                    attempt_id: attempt_id.as_str().to_owned(),
                }),
                None => Ok(ScaleSetServiceDisposition::Idle(statistics)),
            },
            ScaleSetBridgePoll::Message { message_id, .. } => {
                Ok(ScaleSetServiceDisposition::MessagePersisted { message_id })
            }
        }
    }

    /// Advance exactly one durable lifecycle edge or one bounded external operation.
    ///
    /// This is the product-facing dispatcher used by the future supervised loop. It always drains
    /// persisted Scale Set work first, then selects the one active disposable attempt by its
    /// current durable phase. Every selected transaction reopens and revalidates the state under
    /// the canonical lock before it can mutate anything.
    pub(crate) fn supervise_once(
        &mut self,
        clone_runtime: &DisposableCloneRuntime,
        runner_runtime: &DisposableRunnerRuntime,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<ScaleSetServiceDisposition, ScaleSetServiceError>
    where
        B: ScaleSetRunnerBridgeSession,
    {
        let (inbox, catalog) = self
            .store
            .load_scale_set_control_state(&self.source_identity)
            .map_err(ScaleSetServiceError::from_inbox)?;
        if inbox.pending().is_some()
            || inbox
                .last_ack()
                .is_some_and(|receipt| !receipt.outcome_applied())
        {
            return self.reconcile_once();
        }
        if catalog.active().len() > 1 {
            return Err(ScaleSetServiceError::new(
                "scale_set_capacity_invariant_violated",
            ));
        }
        let Some(reservation) = catalog.active().first() else {
            return self.reconcile_once();
        };
        let attempt_id = reservation.attempt().attempt_id().clone();
        let phase = reservation.attempt().phase();
        let runner_started = reservation.attempt().runner_start_started();
        drop(catalog);
        drop(inbox);

        match supervised_operation(phase, runner_started)? {
            SupervisedOperation::AuthorizeClone => {
                self.authorize_reserved_once(clone_runtime, &attempt_id, executor, clock)
            }
            SupervisedOperation::ExecuteClone => {
                self.clone_authorized_once(clone_runtime, &attempt_id, executor, clock)
            }
            SupervisedOperation::CheckpointRegistration => {
                self.checkpoint_registration_once(clone_runtime, &attempt_id, executor, clock)
            }
            SupervisedOperation::RunRegistered => self.run_registered_once(
                runner_runtime,
                clone_runtime,
                &attempt_id,
                executor,
                clock,
            ),
            SupervisedOperation::Cleanup => {
                self.cleanup_once(clone_runtime, &attempt_id, executor, clock)
            }
            SupervisedOperation::Control => self.reconcile_once(),
        }
    }

    pub(crate) fn clone_authorized_once(
        &mut self,
        runtime: &DisposableCloneRuntime,
        attempt_id: &DisposableAttemptId,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<ScaleSetServiceDisposition, ScaleSetServiceError> {
        let admission =
            LiveScaleSetCloneAdmission::new(&mut self.bridge, &self.source_identity, clock);
        match self
            .store
            .execute_disposable_clone_transaction(runtime, attempt_id, &admission, executor, clock)
            .map_err(ScaleSetServiceError::from_clone)?
        {
            DisposableCloneTransactionOutcome::CloneAuthorized { .. } => {
                Err(ScaleSetServiceError::new("clone_phase_changed"))
            }
            DisposableCloneTransactionOutcome::RegistrationCheckpointed { .. } => {
                Err(ScaleSetServiceError::new("clone_phase_changed"))
            }
            DisposableCloneTransactionOutcome::Completed(receipt) => {
                Ok(ScaleSetServiceDisposition::CloneCompleted {
                    attempt_id: receipt.attempt_id().to_owned(),
                })
            }
            DisposableCloneTransactionOutcome::ScaleSetMessagePersisted { message_id } => {
                Ok(ScaleSetServiceDisposition::MessagePersisted { message_id })
            }
        }
    }

    pub(crate) fn authorize_reserved_once(
        &mut self,
        runtime: &DisposableCloneRuntime,
        attempt_id: &DisposableAttemptId,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<ScaleSetServiceDisposition, ScaleSetServiceError> {
        let admission =
            LiveScaleSetCloneAdmission::new(&mut self.bridge, &self.source_identity, clock);
        match self
            .store
            .authorize_disposable_clone_transaction(
                runtime, attempt_id, &admission, executor, clock,
            )
            .map_err(ScaleSetServiceError::from_clone)?
        {
            DisposableCloneTransactionOutcome::CloneAuthorized { attempt_id } => {
                Ok(ScaleSetServiceDisposition::CloneAuthorized { attempt_id })
            }
            DisposableCloneTransactionOutcome::ScaleSetMessagePersisted { message_id } => {
                Ok(ScaleSetServiceDisposition::MessagePersisted { message_id })
            }
            DisposableCloneTransactionOutcome::Completed(_) => {
                Err(ScaleSetServiceError::new("clone_phase_changed"))
            }
            DisposableCloneTransactionOutcome::RegistrationCheckpointed { .. } => {
                Err(ScaleSetServiceError::new("clone_phase_changed"))
            }
        }
    }

    pub(crate) fn checkpoint_registration_once(
        &mut self,
        runtime: &DisposableCloneRuntime,
        attempt_id: &DisposableAttemptId,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<ScaleSetServiceDisposition, ScaleSetServiceError> {
        let admission =
            LiveScaleSetCloneAdmission::new(&mut self.bridge, &self.source_identity, clock);
        match self
            .store
            .checkpoint_disposable_registration_transaction(
                runtime, attempt_id, &admission, executor, clock,
            )
            .map_err(ScaleSetServiceError::from_clone)?
        {
            DisposableCloneTransactionOutcome::RegistrationCheckpointed {
                attempt_id,
                phase: DisposableAttemptPhase::Registering,
            } => Ok(ScaleSetServiceDisposition::RegistrationCheckpointed { attempt_id }),
            DisposableCloneTransactionOutcome::RegistrationCheckpointed {
                attempt_id,
                phase: DisposableAttemptPhase::Destroying,
            } => Ok(ScaleSetServiceDisposition::CleanupCheckpointed {
                attempt_id,
                phase: DisposableAttemptPhase::Destroying,
            }),
            DisposableCloneTransactionOutcome::RegistrationCheckpointed { .. } => {
                Err(ScaleSetServiceError::new("clone_phase_changed"))
            }
            DisposableCloneTransactionOutcome::ScaleSetMessagePersisted { message_id } => {
                Ok(ScaleSetServiceDisposition::MessagePersisted { message_id })
            }
            DisposableCloneTransactionOutcome::CloneAuthorized { .. }
            | DisposableCloneTransactionOutcome::Completed(_) => {
                Err(ScaleSetServiceError::new("clone_phase_changed"))
            }
        }
    }

    pub(crate) fn run_registered_once(
        &mut self,
        runner_runtime: &DisposableRunnerRuntime,
        clone_runtime: &DisposableCloneRuntime,
        attempt_id: &DisposableAttemptId,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<ScaleSetServiceDisposition, ScaleSetServiceError>
    where
        B: ScaleSetRunnerBridgeSession,
    {
        let mut registration = LiveScaleSetRunnerRegistration {
            bridge: &mut self.bridge,
            source_identity: &self.source_identity,
        };
        match self
            .store
            .execute_disposable_runner_transaction(
                runner_runtime,
                clone_runtime,
                attempt_id,
                &mut registration,
                executor,
                clock,
            )
            .map_err(ScaleSetServiceError::from_runner)?
        {
            DisposableRunnerTransactionOutcome::RegistrationRecovered { attempt_id } => {
                Ok(ScaleSetServiceDisposition::RunnerRegistrationRecovered { attempt_id })
            }
            DisposableRunnerTransactionOutcome::CommandCompleted(receipt) => {
                Ok(ScaleSetServiceDisposition::RunnerCommandCompleted {
                    attempt_id: receipt.attempt_id().as_str().to_owned(),
                })
            }
        }
    }

    pub(crate) fn cleanup_once(
        &mut self,
        runtime: &DisposableCloneRuntime,
        attempt_id: &DisposableAttemptId,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<ScaleSetServiceDisposition, ScaleSetServiceError>
    where
        B: ScaleSetRunnerBridgeSession,
    {
        let mut cleanup = LiveScaleSetCleanup {
            bridge: &mut self.bridge,
            source_identity: &self.source_identity,
        };
        match self
            .store
            .execute_disposable_cleanup_transaction(
                runtime,
                attempt_id,
                &mut cleanup,
                executor,
                clock,
            )
            .map_err(ScaleSetServiceError::from_clone)?
        {
            DisposableCleanupTransactionOutcome::CleanupCheckpointed { attempt_id, phase } => {
                Ok(ScaleSetServiceDisposition::CleanupCheckpointed { attempt_id, phase })
            }
            DisposableCleanupTransactionOutcome::VmDestroyed { attempt_id } => {
                Ok(ScaleSetServiceDisposition::VmDestroyed { attempt_id })
            }
            DisposableCleanupTransactionOutcome::RunnerDeleted { attempt_id } => {
                Ok(ScaleSetServiceDisposition::RunnerDeleted { attempt_id })
            }
            DisposableCleanupTransactionOutcome::CapacityReleased { attempt_id } => {
                Ok(ScaleSetServiceDisposition::CapacityReleased { attempt_id })
            }
        }
    }

    #[cfg(test)]
    fn into_parts(self) -> (UnixPersonalWorkerStore, B) {
        (self.store, self.bridge)
    }
}

struct LiveScaleSetCleanup<'a, B> {
    bridge: &'a mut B,
    source_identity: &'a ScaleSetBridgeIdentity,
}

impl<B: ScaleSetRunnerBridgeSession> DisposableCleanupRunnerSource for LiveScaleSetCleanup<'_, B> {
    fn scale_set_source_identity(&self) -> &ScaleSetBridgeIdentity {
        self.source_identity
    }

    fn observe_runner(
        &mut self,
        runner_name: &ScaleSetRunnerName,
    ) -> Result<ScaleSetRunnerLookup, DisposableCloneRuntimeError> {
        self.bridge
            .observe_runner(runner_name)
            .map_err(|error| DisposableCloneRuntimeError::observation(error.code()))
    }

    fn remove_runner(
        &mut self,
        runner: &ScaleSetRunnerReference,
    ) -> Result<(), DisposableCloneRuntimeError> {
        self.bridge
            .remove_runner(runner)
            .map_err(|error| DisposableCloneRuntimeError::observation(error.code()))
    }
}

struct LiveScaleSetRunnerRegistration<'a, B> {
    bridge: &'a mut B,
    source_identity: &'a ScaleSetBridgeIdentity,
}

impl<B: ScaleSetRunnerBridgeSession> DisposableRunnerRegistrationSource
    for LiveScaleSetRunnerRegistration<'_, B>
{
    fn scale_set_source_identity(&self) -> &ScaleSetBridgeIdentity {
        self.source_identity
    }

    fn observe_runner(
        &mut self,
        runner_name: &ScaleSetRunnerName,
    ) -> Result<ScaleSetRunnerLookup, DisposableRunnerRuntimeError> {
        self.bridge
            .observe_runner(runner_name)
            .map_err(|error| DisposableRunnerRuntimeError::bridge(error.code()))
    }

    fn generate_jit(
        &mut self,
        runner_name: &ScaleSetRunnerName,
    ) -> Result<ScaleSetJitReceipt, DisposableRunnerRuntimeError> {
        self.bridge
            .generate_jit(runner_name)
            .map_err(|error| DisposableRunnerRuntimeError::bridge(error.code()))
    }
}

pub(crate) struct LiveScaleSetCloneAdmission<'a, B, C> {
    bridge: RefCell<&'a mut B>,
    source_identity: &'a ScaleSetBridgeIdentity,
    clock: &'a C,
    pending: RefCell<Option<PendingCloneScaleSetMessage>>,
}

impl<'a, B, C> LiveScaleSetCloneAdmission<'a, B, C> {
    pub(crate) fn new(
        bridge: &'a mut B,
        source_identity: &'a ScaleSetBridgeIdentity,
        clock: &'a C,
    ) -> Self {
        Self {
            bridge: RefCell::new(bridge),
            source_identity,
            clock,
            pending: RefCell::new(None),
        }
    }
}

impl<B, C> admission_seal::Sealed for LiveScaleSetCloneAdmission<'_, B, C> {}

impl<B: ScaleSetBridgeSession, C: CloneRuntimeClock> DisposableCloneAdmissionSource
    for LiveScaleSetCloneAdmission<'_, B, C>
{
    fn scale_set_source_identity(&self) -> Option<&ScaleSetBridgeIdentity> {
        Some(self.source_identity)
    }

    fn observe(
        &self,
        catalog: &crate::disposable_attempt_catalog::DisposableAttemptCatalogDocument,
        reservation: &crate::disposable_attempt_catalog::DisposableAttemptReservation,
    ) -> Result<DisposableCloneAdmissionObservation, DisposableCloneRuntimeError> {
        if self.pending.borrow().is_some() {
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_scale_set_message_pending",
            ));
        }
        let capacity_reserved = catalog
            .find_active(reservation.attempt().attempt_id())
            .is_some_and(|current| current == reservation)
            && catalog
                .host_usage()
                .map(|usage| usage.workers() == 1)
                .unwrap_or(false);
        let poll_started_at = self
            .clock
            .epoch_millis()
            .map_err(|_| DisposableCloneRuntimeError::observation("clone_clock_unavailable"))?;
        let poll_started_not_after = poll_started_at
            .get()
            .checked_add(MESSAGE_FRESHNESS_MILLIS)
            .and_then(|value| EpochMillis::new(value).ok())
            .ok_or_else(|| DisposableCloneRuntimeError::observation("clone_clock_unavailable"))?;
        let response =
            self.bridge.borrow_mut().poll(0).map_err(|_| {
                DisposableCloneRuntimeError::observation("clone_scale_set_poll_failed")
            })?;
        match response {
            ScaleSetBridgePoll::Idle { .. } => {
                let observed_at = self.clock.epoch_millis().map_err(|_| {
                    DisposableCloneRuntimeError::observation("clone_clock_unavailable")
                })?;
                let not_after = observed_at
                    .get()
                    .checked_add(MESSAGE_FRESHNESS_MILLIS)
                    .and_then(|value| EpochMillis::new(value).ok())
                    .ok_or_else(|| {
                        DisposableCloneRuntimeError::observation("clone_clock_unavailable")
                    })?;
                Ok(DisposableCloneAdmissionObservation::new(
                    catalog,
                    reservation,
                    observed_at,
                    not_after,
                    capacity_reserved,
                    false,
                ))
            }
            response @ ScaleSetBridgePoll::Message { .. } => {
                let observed_at = self.clock.epoch_millis().unwrap_or(poll_started_at);
                let not_after = observed_at
                    .get()
                    .checked_add(MESSAGE_FRESHNESS_MILLIS)
                    .and_then(|value| EpochMillis::new(value).ok())
                    .unwrap_or(poll_started_not_after);
                self.pending.replace(Some(PendingCloneScaleSetMessage {
                    source_identity: self.source_identity.clone(),
                    response,
                    observed_at,
                    not_after,
                }));
                Err(DisposableCloneRuntimeError::recovery(
                    "clone_scale_set_message_pending",
                ))
            }
        }
    }

    fn take_pending_scale_set_message(&self) -> Option<PendingCloneScaleSetMessage> {
        self.pending.borrow_mut().take()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScaleSetServiceError {
    code: &'static str,
}

impl ScaleSetServiceError {
    const fn new(code: &'static str) -> Self {
        Self { code }
    }

    #[cfg(test)]
    pub(crate) const fn for_test(code: &'static str) -> Self {
        Self::new(code)
    }

    const fn from_bridge(error: ScaleSetBridgeError) -> Self {
        Self::new(error.code())
    }

    fn from_inbox(error: ScaleSetInboxError) -> Self {
        Self::new(error.code())
    }

    fn from_consumer(error: ScaleSetConsumerError) -> Self {
        Self::new(error.code())
    }

    fn from_clone(error: DisposableCloneRuntimeError) -> Self {
        Self::new(error.code())
    }

    fn from_runner(error: DisposableRunnerRuntimeError) -> Self {
        Self::new(error.code())
    }

    pub(crate) const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for ScaleSetServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScaleSetServiceError")
            .field("code", &self.code)
            .finish()
    }
}

impl fmt::Display for ScaleSetServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code)
    }
}

impl std::error::Error for ScaleSetServiceError {}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::disposable_attempt_catalog::{
        DisposableAttemptCatalog, DisposableAttemptCatalogStore,
    };
    use crate::disposable_prepared_template::current_disposable_prepared_template;
    use crate::disposable_worker_reconciler::{
        DisposableAttemptId, DisposableAttemptPhase, DisposableWorkerResources,
    };
    use crate::github_scale_set_bridge::{ScaleSetBridgeEvent, ScaleSetBridgeJobEvidence};
    use crate::github_scale_set_protocol::ScaleSetJobId;

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new() -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "smolrunner-scale-set-service-{}-{sequence}",
                std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o750)).unwrap();
            Self(path)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Clone, Copy)]
    struct FixedClock(EpochMillis);

    impl ScaleSetServiceClock for FixedClock {
        fn now(&self) -> Result<EpochMillis, ScaleSetServiceError> {
            Ok(self.0)
        }
    }

    impl crate::lima_observation::LimaObservationClock for FixedClock {
        fn unix_seconds(&self) -> std::io::Result<u64> {
            Ok(self.0.get() / 1_000)
        }
    }

    impl CloneRuntimeClock for FixedClock {
        fn epoch_millis(&self) -> std::io::Result<EpochMillis> {
            Ok(self.0)
        }
    }

    struct FakeBridge {
        polls: VecDeque<ScaleSetBridgePoll>,
        acquired: VecDeque<Vec<u64>>,
        capacities: Vec<u16>,
        acknowledgements: Vec<u32>,
    }

    impl ScaleSetBridgeSession for FakeBridge {
        fn poll(
            &mut self,
            available_capacity: u16,
        ) -> Result<ScaleSetBridgePoll, ScaleSetBridgeError> {
            self.capacities.push(available_capacity);
            Ok(self.polls.pop_front().expect("expected fake poll"))
        }

        fn ack(&mut self, message_id: u32) -> Result<Vec<u64>, ScaleSetBridgeError> {
            self.acknowledgements.push(message_id);
            Ok(self.acquired.pop_front().unwrap_or_default())
        }
    }

    fn statistics() -> ScaleSetStatistics {
        ScaleSetStatistics {
            available_jobs: 1,
            acquired_jobs: 0,
            assigned_jobs: 0,
            running_jobs: 0,
            registered_runners: 0,
            busy_runners: 0,
            idle_runners: 0,
        }
    }

    fn source_identity() -> ScaleSetBridgeIdentity {
        ScaleSetBridgeIdentity::parse(&format!("sha256:{}", "55".repeat(32))).unwrap()
    }

    fn event() -> ScaleSetBridgeEvent {
        ScaleSetBridgeEvent::Available(ScaleSetBridgeJobEvidence {
            runner_request_id: 41,
            repository: "project".to_owned(),
            owner: "example".to_owned(),
            job_id: ScaleSetJobId::parse("job-1").unwrap(),
            workflow_run_id: 99,
            request_labels: vec!["smolrunner".to_owned()],
        })
    }

    fn assigned_event() -> ScaleSetBridgeEvent {
        let ScaleSetBridgeEvent::Available(job) = event() else {
            unreachable!();
        };
        ScaleSetBridgeEvent::Assigned(job)
    }

    fn canceled_event() -> ScaleSetBridgeEvent {
        let ScaleSetBridgeEvent::Available(job) = event() else {
            unreachable!();
        };
        ScaleSetBridgeEvent::Completed {
            job,
            runner: None,
            result: crate::github_scale_set_protocol::ScaleSetJobResult::parse("canceled").unwrap(),
        }
    }

    #[test]
    fn supervisor_routes_every_durable_phase_without_replaying_started_runner() {
        use SupervisedOperation as Operation;

        for (phase, started, operation) in [
            (
                DisposableAttemptPhase::Reserved,
                false,
                Operation::AuthorizeClone,
            ),
            (
                DisposableAttemptPhase::CloneAuthorized,
                false,
                Operation::ExecuteClone,
            ),
            (
                DisposableAttemptPhase::CloneStarted,
                false,
                Operation::CheckpointRegistration,
            ),
            (
                DisposableAttemptPhase::Registering,
                false,
                Operation::RunRegistered,
            ),
            (
                DisposableAttemptPhase::Assigned,
                false,
                Operation::RunRegistered,
            ),
            (
                DisposableAttemptPhase::Registering,
                true,
                Operation::Control,
            ),
            (DisposableAttemptPhase::Waiting, true, Operation::Control),
            (DisposableAttemptPhase::Assigned, true, Operation::Control),
            (DisposableAttemptPhase::Running, true, Operation::Control),
            (DisposableAttemptPhase::Terminal, true, Operation::Cleanup),
            (DisposableAttemptPhase::Destroying, true, Operation::Cleanup),
            (
                DisposableAttemptPhase::Deregistering,
                true,
                Operation::Cleanup,
            ),
            (DisposableAttemptPhase::Releasing, true, Operation::Cleanup),
            (
                DisposableAttemptPhase::UnprovisionedReleasing,
                false,
                Operation::Control,
            ),
            (DisposableAttemptPhase::Complete, true, Operation::Control),
        ] {
            assert_eq!(supervised_operation(phase, started).unwrap(), operation);
        }
        assert_eq!(
            supervised_operation(DisposableAttemptPhase::Provisioning, false)
                .unwrap_err()
                .code(),
            "scale_set_legacy_provisioning_recovery_required"
        );
    }

    #[test]
    fn one_tick_at_a_time_persists_applies_acks_and_reconciles_capacity() {
        let root = TempRoot::new();
        let store = UnixPersonalWorkerStore::open_or_create_disposable_catalog(&root.0).unwrap();
        let mut catalog = DisposableAttemptCatalog::new(store);
        catalog.initialize().unwrap();
        let store = catalog.into_store();
        let policy = ScaleSetConsumerPolicy::new(
            source_identity(),
            23,
            "project",
            "example",
            &["smolrunner".to_owned()],
            DisposableWorkerResources::new(2_000, 2 << 30, 20 << 30).unwrap(),
            &current_disposable_prepared_template().unwrap(),
        )
        .unwrap();
        let bridge = FakeBridge {
            polls: VecDeque::from([
                ScaleSetBridgePoll::Message {
                    message_id: 7,
                    statistics: statistics(),
                    events: vec![event()],
                },
                ScaleSetBridgePoll::Idle {
                    statistics: statistics(),
                },
            ]),
            acquired: VecDeque::from([Vec::new()]),
            capacities: Vec::new(),
            acknowledgements: Vec::new(),
        };
        let mut service = ScaleSetService::with_parts(
            store,
            bridge,
            policy,
            FixedClock(EpochMillis::new(100_000).unwrap()),
        )
        .unwrap();

        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::MessagePersisted { message_id: 7 }
        ));
        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::EventApplied {
                message_id: 7,
                event_index: 0
            }
        ));
        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::MessageAcknowledged { message_id: 7 }
        ));
        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::AckOutcomeApplied { message_id: 7 }
        ));
        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::UnprovisionedReleased { .. }
        ));
        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::AttemptRetired { .. }
        ));
        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::Idle(_)
        ));

        let (mut store, bridge) = service.into_parts();
        let catalog = DisposableAttemptCatalogStore::load(&store)
            .unwrap()
            .unwrap();
        assert!(catalog.active().is_empty());
        assert_eq!(catalog.tombstones().len(), 1);
        assert_eq!(bridge.capacities, [1, 1]);
        assert_eq!(bridge.acknowledgements, [7]);
        assert!(DisposableAttemptCatalogStore::recover(&mut store).is_ok());
    }

    #[test]
    fn zero_capacity_idle_poll_is_persisted_as_advisory_state_only() {
        let root = TempRoot::new();
        let store = UnixPersonalWorkerStore::open_or_create_disposable_catalog(&root.0).unwrap();
        let mut catalog = DisposableAttemptCatalog::new(store);
        catalog.initialize().unwrap();
        let store = catalog.into_store();
        let policy = ScaleSetConsumerPolicy::new(
            source_identity(),
            23,
            "project",
            "example",
            &["smolrunner".to_owned()],
            DisposableWorkerResources::new(2_000, 2 << 30, 20 << 30).unwrap(),
            &current_disposable_prepared_template().unwrap(),
        )
        .unwrap();
        let bridge = FakeBridge {
            polls: VecDeque::from([
                ScaleSetBridgePoll::Message {
                    message_id: 7,
                    statistics: statistics(),
                    events: vec![event()],
                },
                ScaleSetBridgePoll::Idle {
                    statistics: statistics(),
                },
            ]),
            acquired: VecDeque::from([vec![41]]),
            capacities: Vec::new(),
            acknowledgements: Vec::new(),
        };
        let mut service = ScaleSetService::with_parts(
            store,
            bridge,
            policy,
            FixedClock(EpochMillis::new(100_000).unwrap()),
        )
        .unwrap();

        for _ in 0..4 {
            service.reconcile_once().unwrap();
        }
        let attempt_id = match service.reconcile_once().unwrap() {
            ScaleSetServiceDisposition::IdleObservationRecorded { attempt_id } => {
                DisposableAttemptId::parse(&attempt_id).unwrap()
            }
            other => panic!("unexpected disposition: {other:?}"),
        };

        let (mut store, bridge) = service.into_parts();
        let (inbox, catalog) = store
            .load_scale_set_control_state(&source_identity())
            .unwrap();
        let reservation = &catalog.active()[0];
        let idle = inbox.last_idle().unwrap();
        assert_eq!(&attempt_id, reservation.attempt().attempt_id());
        assert_eq!(idle.catalog_revision(), catalog.revision());
        assert_eq!(idle.attempt_id(), reservation.attempt().attempt_id());
        assert_eq!(idle.attempt_revision(), reservation.attempt().revision());
        assert_eq!(bridge.capacities, [1, 0]);
    }

    #[test]
    fn live_clone_admission_polls_zero_capacity_instead_of_reusing_idle_evidence() {
        let root = TempRoot::new();
        let store = UnixPersonalWorkerStore::open_or_create_disposable_catalog(&root.0).unwrap();
        let mut catalog = DisposableAttemptCatalog::new(store);
        catalog.initialize().unwrap();
        let store = catalog.into_store();
        let policy = ScaleSetConsumerPolicy::new(
            source_identity(),
            23,
            "project",
            "example",
            &["smolrunner".to_owned()],
            DisposableWorkerResources::new(2_000, 2 << 30, 20 << 30).unwrap(),
            &current_disposable_prepared_template().unwrap(),
        )
        .unwrap();
        let bridge = FakeBridge {
            polls: VecDeque::from([
                ScaleSetBridgePoll::Message {
                    message_id: 7,
                    statistics: statistics(),
                    events: vec![event()],
                },
                ScaleSetBridgePoll::Idle {
                    statistics: statistics(),
                },
            ]),
            acquired: VecDeque::from([vec![41]]),
            capacities: Vec::new(),
            acknowledgements: Vec::new(),
        };
        let clock = FixedClock(EpochMillis::new(100_000).unwrap());
        let mut service = ScaleSetService::with_parts(store, bridge, policy, clock).unwrap();
        for _ in 0..4 {
            service.reconcile_once().unwrap();
        }

        let (mut store, mut bridge) = service.into_parts();
        let (_, catalog) = store
            .load_scale_set_control_state(&source_identity())
            .unwrap();
        let reservation = &catalog.active()[0];
        let source = source_identity();
        let admission = LiveScaleSetCloneAdmission::new(&mut bridge, &source, &clock);
        admission.observe(&catalog, reservation).unwrap();
        assert!(admission.take_pending_scale_set_message().is_none());
        drop(admission);

        assert_eq!(bridge.capacities, [1, 0]);
    }

    #[test]
    fn acquired_job_cancellation_before_clone_is_durably_releasable() {
        let root = TempRoot::new();
        let store = UnixPersonalWorkerStore::open_or_create_disposable_catalog(&root.0).unwrap();
        let mut catalog = DisposableAttemptCatalog::new(store);
        catalog.initialize().unwrap();
        let store = catalog.into_store();
        let policy = ScaleSetConsumerPolicy::new(
            source_identity(),
            23,
            "project",
            "example",
            &["smolrunner".to_owned()],
            DisposableWorkerResources::new(2_000, 2 << 30, 20 << 30).unwrap(),
            &current_disposable_prepared_template().unwrap(),
        )
        .unwrap();
        let bridge = FakeBridge {
            polls: VecDeque::from([
                ScaleSetBridgePoll::Message {
                    message_id: 7,
                    statistics: statistics(),
                    events: vec![event()],
                },
                ScaleSetBridgePoll::Message {
                    message_id: 8,
                    statistics: statistics(),
                    events: vec![assigned_event()],
                },
                ScaleSetBridgePoll::Message {
                    message_id: 9,
                    statistics: statistics(),
                    events: vec![canceled_event()],
                },
            ]),
            acquired: VecDeque::from([vec![41], Vec::new(), Vec::new()]),
            capacities: Vec::new(),
            acknowledgements: Vec::new(),
        };
        let mut service = ScaleSetService::with_parts(
            store,
            bridge,
            policy,
            FixedClock(EpochMillis::new(100_000).unwrap()),
        )
        .unwrap();

        for message_id in [7, 8, 9] {
            assert!(matches!(
                service.reconcile_once().unwrap(),
                ScaleSetServiceDisposition::MessagePersisted { message_id: actual }
                    if actual == message_id
            ));
            assert!(matches!(
                service.reconcile_once().unwrap(),
                ScaleSetServiceDisposition::EventApplied { message_id: actual, .. }
                    if actual == message_id
            ));
            assert!(matches!(
                service.reconcile_once().unwrap(),
                ScaleSetServiceDisposition::MessageAcknowledged { message_id: actual }
                    if actual == message_id
            ));
            assert!(matches!(
                service.reconcile_once().unwrap(),
                ScaleSetServiceDisposition::AckOutcomeApplied { message_id: actual }
                    if actual == message_id
            ));
        }

        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::UnprovisionedReleased { .. }
        ));
        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::AttemptRetired { .. }
        ));

        let (store, bridge) = service.into_parts();
        let catalog = DisposableAttemptCatalogStore::load(&store)
            .unwrap()
            .unwrap();
        let attempt = &catalog.tombstones()[0];
        assert_eq!(attempt.phase(), DisposableAttemptPhase::Complete);
        assert_eq!(attempt.github_job_id().unwrap().as_str(), "job-1");
        assert_eq!(attempt.result().unwrap().as_str(), "canceled");
        assert!(attempt.vm_identity().is_none());
        assert_eq!(bridge.capacities, [1, 0, 0]);
        assert_eq!(bridge.acknowledgements, [7, 8, 9]);
    }
}
