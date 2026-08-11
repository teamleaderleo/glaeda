// The coordinator stays private until operator enrollment and the launchd service entry point are
// wired around this bounded phase dispatcher.
#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::disposable_clone_runtime::{
    CloneRuntimeClock, DisposableCleanupRunnerSource, DisposableCleanupTransactionOutcome,
    DisposableCloneAdmissionObservation, DisposableCloneAdmissionSource, DisposableCloneRuntime,
    DisposableCloneRuntimeError, DisposableCloneTransactionOutcome, PendingCloneScaleSetMessage,
    admission_seal,
};
#[cfg(target_os = "macos")]
use crate::disposable_network_gate::observe_disposable_network_gate;
use crate::disposable_network_policy::DisposableNetworkPolicyPlan;
use crate::disposable_runner_runtime::{
    DisposableRunnerRegistrationSource, DisposableRunnerRuntime, DisposableRunnerRuntimeError,
};
use crate::disposable_template_runtime::{
    DisposableTemplateRuntime, DisposableTemplateRuntimeDisposition,
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
    apply_scale_set_event, should_acquire_scale_set_available,
};
use crate::github_scale_set_inbox::{PendingScaleSetMessage, ScaleSetInboxError};
use crate::github_scale_set_protocol::{ScaleSetRunnerName, ScaleSetRunnerReference};
use crate::process::TimedCommandExecutor;
use crate::unix_personal_worker_store::UnixPersonalWorkerStore;
use crate::unix_personal_worker_store::{
    DisposableOrphanCleanupOutcome, DisposableRunnerTransactionOutcome,
};

const MESSAGE_FRESHNESS_MILLIS: u64 = 30_000;

pub(crate) trait ScaleSetBridgeSession {
    fn poll(&mut self, available_capacity: u16) -> Result<ScaleSetBridgePoll, ScaleSetBridgeError>;
    fn ack(
        &mut self,
        message_id: u32,
        acquire_available: bool,
    ) -> Result<Vec<u64>, ScaleSetBridgeError>;
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

    fn ack(
        &mut self,
        message_id: u32,
        acquire_available: bool,
    ) -> Result<Vec<u64>, ScaleSetBridgeError> {
        ScaleSetBridgeClient::ack(self, message_id, acquire_available)
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

pub(crate) struct SystemScaleSetServiceClock;

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
    AdmissionHeld {
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
    OrphanAuditSatisfied {
        attempt_id: String,
    },
    OrphanVmDestroyed {
        attempt_id: String,
    },
    OrphanRunnerDeleted {
        attempt_id: String,
    },
    TemplateAdvanced {
        disposition: DisposableTemplateRuntimeDisposition,
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

fn template_required_for(operation: Option<SupervisedOperation>) -> bool {
    operation.is_none_or(|operation| matches!(operation, SupervisedOperation::ExecuteClone))
}

fn advance_template_if_required(
    operation: Option<SupervisedOperation>,
    reconcile: impl FnOnce() -> Result<DisposableTemplateRuntimeDisposition, ScaleSetServiceError>,
) -> Result<Option<ScaleSetServiceDisposition>, ScaleSetServiceError> {
    if !template_required_for(operation) {
        return Ok(None);
    }
    match reconcile()? {
        DisposableTemplateRuntimeDisposition::Satisfied => Ok(None),
        disposition @ (DisposableTemplateRuntimeDisposition::Persisted
        | DisposableTemplateRuntimeDisposition::CommandCompleted { .. }) => {
            Ok(Some(ScaleSetServiceDisposition::TemplateAdvanced {
                disposition,
            }))
        }
        DisposableTemplateRuntimeDisposition::RebuildRequired => {
            Err(ScaleSetServiceError::new("template_rebuild_required"))
        }
        DisposableTemplateRuntimeDisposition::Refused => {
            Err(ScaleSetServiceError::new("template_refused"))
        }
    }
}

fn apply_startup_orphan_outcome(
    audit: &mut VecDeque<DisposableAttemptId>,
    attempt_id: &DisposableAttemptId,
    outcome: DisposableOrphanCleanupOutcome,
) -> Result<ScaleSetServiceDisposition, ScaleSetServiceError> {
    if audit.front() != Some(attempt_id) {
        return Err(ScaleSetServiceError::new(
            "scale_set_orphan_audit_cursor_invalid",
        ));
    }
    let public_attempt_id = attempt_id.as_str().to_owned();
    Ok(match outcome {
        DisposableOrphanCleanupOutcome::Satisfied => {
            audit.pop_front();
            ScaleSetServiceDisposition::OrphanAuditSatisfied {
                attempt_id: public_attempt_id,
            }
        }
        DisposableOrphanCleanupOutcome::VmDestroyed => {
            ScaleSetServiceDisposition::OrphanVmDestroyed {
                attempt_id: public_attempt_id,
            }
        }
        DisposableOrphanCleanupOutcome::RunnerDeleted => {
            ScaleSetServiceDisposition::OrphanRunnerDeleted {
                attempt_id: public_attempt_id,
            }
        }
    })
}

pub(crate) struct ScaleSetService<B, C> {
    store: UnixPersonalWorkerStore,
    bridge: B,
    policy: ScaleSetConsumerPolicy,
    network_policy: Option<DisposableNetworkPolicyPlan>,
    source_identity: ScaleSetBridgeIdentity,
    clock: C,
    startup_orphan_audit: VecDeque<DisposableAttemptId>,
}

pub(crate) struct PreparedScaleSetService {
    store: UnixPersonalWorkerStore,
    policy: ScaleSetConsumerPolicy,
    network_policy: DisposableNetworkPolicyPlan,
    source_identity: ScaleSetBridgeIdentity,
    last_acked_message_id: u32,
    pending: Option<PendingScaleSetMessage>,
    startup_orphan_audit: VecDeque<DisposableAttemptId>,
}

fn network_admission_permitted(policy: Option<&DisposableNetworkPolicyPlan>) -> bool {
    let Some(policy) = policy else {
        // Only the cfg(test) constructor omits the production network policy.
        return true;
    };
    #[cfg(target_os = "macos")]
    {
        observe_disposable_network_gate(policy).is_ok()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = policy;
        false
    }
}

impl ScaleSetService<ScaleSetBridgeClient, SystemScaleSetServiceClock> {
    /// Prepare and recover every durable Scale Set document before a credential-bearing bridge
    /// process is started.
    pub(crate) fn prepare(
        mut store: UnixPersonalWorkerStore,
        policy: ScaleSetConsumerPolicy,
        network_policy: DisposableNetworkPolicyPlan,
    ) -> Result<PreparedScaleSetService, ScaleSetServiceError> {
        let source_identity = policy.source_identity().clone();
        store
            .initialize_scale_set_inbox(&source_identity)
            .map_err(ScaleSetServiceError::from_inbox)?;
        let (inbox, catalog) = store
            .load_scale_set_control_state(&source_identity)
            .map_err(ScaleSetServiceError::from_inbox)?;
        if inbox
            .pending()
            .is_some_and(PendingScaleSetMessage::ack_started)
        {
            return Err(ScaleSetServiceError::new("scale_set_ack_outcome_unknown"));
        }
        let last_acked_message_id = inbox
            .last_ack()
            .map(|receipt| receipt.message_id())
            .unwrap_or(0);
        let pending = inbox.pending().cloned();
        let startup_orphan_audit = catalog
            .tombstones()
            .iter()
            .map(|attempt| attempt.attempt_id().clone())
            .collect();
        Ok(PreparedScaleSetService {
            store,
            policy,
            network_policy,
            source_identity,
            last_acked_message_id,
            pending,
            startup_orphan_audit,
        })
    }

    pub(crate) fn start(
        prepared: PreparedScaleSetService,
        mut bridge: ScaleSetBridgeClient,
    ) -> Result<Self, ScaleSetServiceError> {
        bridge
            .resume(
                prepared.last_acked_message_id,
                prepared
                    .pending
                    .as_ref()
                    .map(|pending| (pending.message_id(), pending.events())),
            )
            .map_err(ScaleSetServiceError::from_bridge)?;
        Ok(Self {
            store: prepared.store,
            bridge,
            policy: prepared.policy,
            network_policy: Some(prepared.network_policy),
            source_identity: prepared.source_identity,
            clock: SystemScaleSetServiceClock,
            startup_orphan_audit: prepared.startup_orphan_audit,
        })
    }

    pub(crate) fn new(
        store: UnixPersonalWorkerStore,
        bridge: ScaleSetBridgeClient,
        policy: ScaleSetConsumerPolicy,
        network_policy: DisposableNetworkPolicyPlan,
    ) -> Result<Self, ScaleSetServiceError> {
        let prepared = Self::prepare(store, policy, network_policy)?;
        Self::start(prepared, bridge)
    }
}

impl<B: ScaleSetBridgeSession, C: ScaleSetServiceClock> ScaleSetService<B, C> {
    #[cfg(test)]
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
        let (_, catalog) = store
            .load_scale_set_control_state(&source_identity)
            .map_err(ScaleSetServiceError::from_inbox)?;
        let startup_orphan_audit = catalog
            .tombstones()
            .iter()
            .map(|attempt| attempt.attempt_id().clone())
            .collect();
        Ok(Self {
            store,
            bridge,
            policy,
            network_policy: None,
            source_identity,
            clock,
            startup_orphan_audit,
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
            let clock = &self.clock;
            let policy = &self.policy;
            let network_policy = self.network_policy.as_ref();
            self.store
                .acknowledge_scale_set_message(
                    &self.source_identity,
                    inbox.revision(),
                    catalog.revision(),
                    |message_id, pending, catalog, admission_held| {
                        // Sample the acquisition deadline only after the durable ack-started
                        // checkpoint while the canonical store lock still binds this exact
                        // pending message and catalog revision.
                        let now = clock.now()?;
                        let acquire_available = !admission_held
                            && network_admission_permitted(network_policy)
                            && should_acquire_scale_set_available(policy, pending, catalog, now)
                                .map_err(ScaleSetServiceError::from_consumer)?;
                        bridge
                            .ack(message_id, acquire_available)
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
        let network_policy = self.network_policy.as_ref();
        let (response, attempt_id) = self
            .store
            .poll_and_record_scale_set(&self.source_identity, |available_capacity| {
                let available_capacity = if network_admission_permitted(network_policy) {
                    available_capacity
                } else {
                    0
                };
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
        template_runtime: &DisposableTemplateRuntime,
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
        if catalog.active().is_empty()
            && let Some(attempt_id) = self.startup_orphan_audit.front().cloned()
        {
            drop(catalog);
            drop(inbox);
            let mut cleanup = LiveScaleSetCleanup {
                bridge: &mut self.bridge,
                source_identity: &self.source_identity,
            };
            let outcome = self
                .store
                .execute_disposable_orphan_cleanup_transaction(
                    clone_runtime,
                    &attempt_id,
                    &mut cleanup,
                    executor,
                    clock,
                )
                .map_err(ScaleSetServiceError::from_clone)?;
            return apply_startup_orphan_outcome(
                &mut self.startup_orphan_audit,
                &attempt_id,
                outcome,
            );
        }
        let selected = catalog.active().first().map(|reservation| {
            let attempt_id = reservation.attempt().attempt_id().clone();
            let operation = supervised_operation(
                reservation.attempt().phase(),
                reservation.attempt().runner_start_started(),
            )?;
            Ok::<_, ScaleSetServiceError>((
                attempt_id,
                operation,
                reservation.attempt().github_job_id().is_some(),
                reservation.attempt().not_after(),
            ))
        });
        let selected = selected.transpose()?;
        let operation = selected.as_ref().map(|(_, operation, _, _)| *operation);
        drop(catalog);
        drop(inbox);

        if let Some((attempt_id, operation, acquired, not_after)) = selected.as_ref()
            && matches!(
                operation,
                SupervisedOperation::AuthorizeClone | SupervisedOperation::ExecuteClone
            )
        {
            let now = clock
                .epoch_millis()
                .map_err(|_| ScaleSetServiceError::new("scale_set_clock_unavailable"))?;
            let released = self
                .store
                .checkpoint_expired_scale_set_preclone_attempt(
                    &self.source_identity,
                    attempt_id,
                    now,
                )
                .map_err(ScaleSetServiceError::from_inbox)?;
            if released {
                return Ok(ScaleSetServiceDisposition::CleanupCheckpointed {
                    attempt_id: attempt_id.as_str().to_owned(),
                    phase: DisposableAttemptPhase::UnprovisionedReleasing,
                });
            }
            if *acquired && now > *not_after {
                // Acquisition is an external obligation. Once the exact offer was acquired, an
                // elapsed local start deadline cannot manufacture an unprovisioned release. Keep
                // capacity closed and poll until GitHub supplies the exact cancellation event.
                return self.reconcile_once();
            }
        }

        if let Some(disposition) = advance_template_if_required(operation, || {
            template_runtime
                .reconcile_once(executor, clock)
                .map(|receipt| receipt.disposition)
                .map_err(|error| ScaleSetServiceError::new(error.code()))
        })? {
            return Ok(disposition);
        }

        let Some((attempt_id, operation, _, _)) = selected else {
            return self.reconcile_once();
        };
        match operation {
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
        let admission = LiveScaleSetCloneAdmission::with_network_policy(
            &mut self.bridge,
            &self.source_identity,
            clock,
            self.network_policy.as_ref(),
        );
        match self
            .store
            .execute_disposable_clone_transaction(runtime, attempt_id, &admission, executor, clock)
            .map_err(ScaleSetServiceError::from_clone)?
        {
            DisposableCloneTransactionOutcome::AdmissionHeld { attempt_id } => {
                Ok(ScaleSetServiceDisposition::AdmissionHeld { attempt_id })
            }
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
        let admission = LiveScaleSetCloneAdmission::with_network_policy(
            &mut self.bridge,
            &self.source_identity,
            clock,
            self.network_policy.as_ref(),
        );
        match self
            .store
            .authorize_disposable_clone_transaction(
                runtime, attempt_id, &admission, executor, clock,
            )
            .map_err(ScaleSetServiceError::from_clone)?
        {
            DisposableCloneTransactionOutcome::AdmissionHeld { attempt_id } => {
                Ok(ScaleSetServiceDisposition::AdmissionHeld { attempt_id })
            }
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
        let admission = LiveScaleSetCloneAdmission::with_network_policy(
            &mut self.bridge,
            &self.source_identity,
            clock,
            self.network_policy.as_ref(),
        );
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
            DisposableCloneTransactionOutcome::AdmissionHeld { .. }
            | DisposableCloneTransactionOutcome::CloneAuthorized { .. }
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
    network_policy: Option<&'a DisposableNetworkPolicyPlan>,
    pending: RefCell<Option<PendingCloneScaleSetMessage>>,
}

impl<'a, B, C> LiveScaleSetCloneAdmission<'a, B, C> {
    #[cfg(test)]
    pub(crate) fn new(
        bridge: &'a mut B,
        source_identity: &'a ScaleSetBridgeIdentity,
        clock: &'a C,
    ) -> Self {
        Self {
            bridge: RefCell::new(bridge),
            source_identity,
            clock,
            network_policy: None,
            pending: RefCell::new(None),
        }
    }

    pub(crate) fn with_network_policy(
        bridge: &'a mut B,
        source_identity: &'a ScaleSetBridgeIdentity,
        clock: &'a C,
        network_policy: Option<&'a DisposableNetworkPolicyPlan>,
    ) -> Self {
        Self {
            bridge: RefCell::new(bridge),
            source_identity,
            clock,
            network_policy,
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

    fn host_admission_permitted(&self) -> Result<bool, DisposableCloneRuntimeError> {
        Ok(network_admission_permitted(self.network_policy))
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
    use std::cell::Cell;
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

    struct AckCheckpointClock {
        now: EpochMillis,
        calls: Cell<u8>,
        inbox_path: PathBuf,
    }

    impl ScaleSetServiceClock for AckCheckpointClock {
        fn now(&self) -> Result<EpochMillis, ScaleSetServiceError> {
            let call = self.calls.get().saturating_add(1);
            self.calls.set(call);
            if call == 3 {
                let inbox = crate::github_scale_set_inbox::decode_scale_set_inbox(
                    &std::fs::read(&self.inbox_path).unwrap(),
                )
                .unwrap();
                assert!(inbox.pending().unwrap().ack_started());
            }
            Ok(self.now)
        }
    }

    struct FakeBridge {
        polls: VecDeque<ScaleSetBridgePoll>,
        acquired: VecDeque<Vec<u64>>,
        capacities: Vec<u16>,
        acknowledgements: Vec<(u32, bool)>,
    }

    impl ScaleSetBridgeSession for FakeBridge {
        fn poll(
            &mut self,
            available_capacity: u16,
        ) -> Result<ScaleSetBridgePoll, ScaleSetBridgeError> {
            self.capacities.push(available_capacity);
            Ok(self.polls.pop_front().expect("expected fake poll"))
        }

        fn ack(
            &mut self,
            message_id: u32,
            acquire_available: bool,
        ) -> Result<Vec<u64>, ScaleSetBridgeError> {
            self.acknowledgements.push((message_id, acquire_available));
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

    fn consumer_policy() -> ScaleSetConsumerPolicy {
        ScaleSetConsumerPolicy::new(
            source_identity(),
            23,
            "project",
            "example",
            &["smolrunner".to_owned()],
            DisposableWorkerResources::new(2_000, 2 << 30, 20 << 30).unwrap(),
            &current_disposable_prepared_template().unwrap(),
        )
        .unwrap()
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
        assert!(template_required_for(None));
        assert!(!template_required_for(Some(Operation::AuthorizeClone)));
        assert!(template_required_for(Some(Operation::ExecuteClone)));
        for operation in [
            Operation::CheckpointRegistration,
            Operation::RunRegistered,
            Operation::Cleanup,
            Operation::Control,
        ] {
            assert!(
                !template_required_for(Some(operation)),
                "post-clone recovery must not depend on source-template health: {operation:?}"
            );
        }

        let cleanup = advance_template_if_required(Some(Operation::Cleanup), || {
            panic!("broken source template must not be observed while cleanup debt exists")
        })
        .unwrap();
        assert!(cleanup.is_none());
        assert_eq!(
            advance_template_if_required(None, || {
                Ok(DisposableTemplateRuntimeDisposition::Refused)
            })
            .unwrap_err()
            .code(),
            "template_refused"
        );
    }

    #[test]
    fn startup_orphan_audit_rechecks_after_each_mutation_before_advancing() {
        let attempt_id = DisposableAttemptId::parse("attempt-orphan-audit").unwrap();
        let mut audit = VecDeque::from([attempt_id.clone()]);
        assert!(matches!(
            apply_startup_orphan_outcome(
                &mut audit,
                &attempt_id,
                DisposableOrphanCleanupOutcome::VmDestroyed,
            )
            .unwrap(),
            ScaleSetServiceDisposition::OrphanVmDestroyed { .. }
        ));
        assert_eq!(audit.front(), Some(&attempt_id));
        assert!(matches!(
            apply_startup_orphan_outcome(
                &mut audit,
                &attempt_id,
                DisposableOrphanCleanupOutcome::RunnerDeleted,
            )
            .unwrap(),
            ScaleSetServiceDisposition::OrphanRunnerDeleted { .. }
        ));
        assert_eq!(audit.front(), Some(&attempt_id));
        assert!(matches!(
            apply_startup_orphan_outcome(
                &mut audit,
                &attempt_id,
                DisposableOrphanCleanupOutcome::Satisfied,
            )
            .unwrap(),
            ScaleSetServiceDisposition::OrphanAuditSatisfied { .. }
        ));
        assert!(audit.is_empty());
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
            policy.clone(),
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
        assert_eq!(bridge.acknowledgements, [(7, true)]);
        assert!(DisposableAttemptCatalogStore::recover(&mut store).is_ok());
        let restarted = ScaleSetService::with_parts(
            store,
            bridge,
            policy,
            FixedClock(EpochMillis::new(100_001).unwrap()),
        )
        .unwrap();
        assert_eq!(restarted.startup_orphan_audit.len(), 1);
        assert_eq!(
            restarted.startup_orphan_audit.front(),
            catalog
                .tombstones()
                .first()
                .map(|attempt| attempt.attempt_id())
        );
    }

    #[test]
    fn unavailable_host_network_gate_advertises_zero_and_refuses_acquisition() {
        let root = TempRoot::new();
        let store = UnixPersonalWorkerStore::open_or_create_disposable_catalog(&root.0).unwrap();
        let mut catalog = DisposableAttemptCatalog::new(store);
        catalog.initialize().unwrap();
        let store = catalog.into_store();
        let bridge = FakeBridge {
            // A nonconforming upstream response is still persisted, but the under-lock ack gate
            // must refuse acquisition after capacity zero was advertised.
            polls: VecDeque::from([ScaleSetBridgePoll::Message {
                message_id: 7,
                statistics: statistics(),
                events: vec![event()],
            }]),
            acquired: VecDeque::from([Vec::new()]),
            capacities: Vec::new(),
            acknowledgements: Vec::new(),
        };
        let mut service = ScaleSetService::with_parts(
            store,
            bridge,
            consumer_policy(),
            FixedClock(EpochMillis::new(100_000).unwrap()),
        )
        .unwrap();
        let other_uid = rustix::process::geteuid().as_raw().checked_add(1).unwrap();
        service.network_policy = Some(
            crate::disposable_network_policy::plan_disposable_network_policy(
                other_uid,
                &current_disposable_prepared_template().unwrap(),
            )
            .unwrap(),
        );

        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::MessagePersisted { .. }
        ));
        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::EventApplied { .. }
        ));
        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::MessageAcknowledged { .. }
        ));
        let (_, bridge) = service.into_parts();
        assert_eq!(bridge.capacities, [0]);
        assert_eq!(bridge.acknowledgements, [(7, false)]);
    }

    #[test]
    fn six_hour_outage_acknowledges_without_acquiring_and_releases_capacity() {
        let root = TempRoot::new();
        let store = UnixPersonalWorkerStore::open_or_create_disposable_catalog(&root.0).unwrap();
        let mut catalog = DisposableAttemptCatalog::new(store);
        catalog.initialize().unwrap();
        let store = catalog.into_store();
        let policy = consumer_policy();
        let bridge = FakeBridge {
            polls: VecDeque::from([ScaleSetBridgePoll::Message {
                message_id: 7,
                statistics: statistics(),
                events: vec![event()],
            }]),
            acquired: VecDeque::from([Vec::new()]),
            capacities: Vec::new(),
            acknowledgements: Vec::new(),
        };
        let mut service = ScaleSetService::with_parts(
            store,
            bridge,
            policy.clone(),
            FixedClock(EpochMillis::new(100_000).unwrap()),
        )
        .unwrap();
        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::MessagePersisted { .. }
        ));
        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::EventApplied { .. }
        ));

        let (store, bridge) = service.into_parts();
        let mut service = ScaleSetService::with_parts(
            store,
            bridge,
            policy,
            FixedClock(EpochMillis::new(21_700_001).unwrap()),
        )
        .unwrap();
        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::MessageAcknowledged { .. }
        ));
        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::AckOutcomeApplied { .. }
        ));
        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::UnprovisionedReleased { .. }
        ));
        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::AttemptRetired { .. }
        ));
        let (store, bridge) = service.into_parts();
        assert_eq!(bridge.acknowledgements, [(7, false)]);
        let catalog = DisposableAttemptCatalogStore::load(&store)
            .unwrap()
            .unwrap();
        assert!(catalog.active().is_empty());
        assert_eq!(catalog.tombstones().len(), 1);
    }

    #[test]
    fn hold_set_after_message_persistence_refuses_acquisition_and_releases_capacity() {
        let root = TempRoot::new();
        let store = UnixPersonalWorkerStore::open_or_create_disposable_catalog(&root.0).unwrap();
        let mut catalog = DisposableAttemptCatalog::new(store);
        catalog.initialize().unwrap();
        let store = catalog.into_store();
        let policy = consumer_policy();
        let bridge = FakeBridge {
            polls: VecDeque::from([ScaleSetBridgePoll::Message {
                message_id: 7,
                statistics: statistics(),
                events: vec![event()],
            }]),
            acquired: VecDeque::from([Vec::new()]),
            capacities: Vec::new(),
            acknowledgements: Vec::new(),
        };
        let mut service = ScaleSetService::with_parts(
            store,
            bridge,
            policy.clone(),
            FixedClock(EpochMillis::new(100_000).unwrap()),
        )
        .unwrap();
        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::MessagePersisted { .. }
        ));
        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::EventApplied { .. }
        ));

        let (mut store, bridge) = service.into_parts();
        store.set_disposable_worker_admission_hold(true).unwrap();
        let mut service = ScaleSetService::with_parts(
            store,
            bridge,
            policy,
            FixedClock(EpochMillis::new(100_001).unwrap()),
        )
        .unwrap();
        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::MessageAcknowledged { .. }
        ));
        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::AckOutcomeApplied { .. }
        ));
        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::UnprovisionedReleased { .. }
        ));
        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::AttemptRetired { .. }
        ));

        let (store, bridge) = service.into_parts();
        assert_eq!(bridge.acknowledgements, [(7, false)]);
        assert!(
            store
                .inspect_disposable_worker_admission()
                .unwrap()
                .admission_held()
        );
        let catalog = DisposableAttemptCatalogStore::load(&store)
            .unwrap()
            .unwrap();
        assert!(catalog.active().is_empty());
        assert_eq!(catalog.tombstones().len(), 1);
    }

    #[test]
    fn acquisition_deadline_is_sampled_after_the_durable_ack_checkpoint() {
        let root = TempRoot::new();
        let store = UnixPersonalWorkerStore::open_or_create_disposable_catalog(&root.0).unwrap();
        let mut catalog = DisposableAttemptCatalog::new(store);
        catalog.initialize().unwrap();
        let store = catalog.into_store();
        let bridge = FakeBridge {
            polls: VecDeque::from([ScaleSetBridgePoll::Message {
                message_id: 7,
                statistics: statistics(),
                events: vec![event()],
            }]),
            acquired: VecDeque::from([vec![41]]),
            capacities: Vec::new(),
            acknowledgements: Vec::new(),
        };
        let clock = AckCheckpointClock {
            now: EpochMillis::new(100_000).unwrap(),
            calls: Cell::new(0),
            inbox_path: root
                .0
                .join("personal-worker")
                .join("github-scale-set-inbox.json"),
        };
        let mut service =
            ScaleSetService::with_parts(store, bridge, consumer_policy(), clock).unwrap();

        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::MessagePersisted { .. }
        ));
        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::EventApplied { .. }
        ));
        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::MessageAcknowledged { .. }
        ));
        let (_, bridge) = service.into_parts();
        assert_eq!(bridge.acknowledgements, [(7, true)]);
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
    fn acquired_job_cancellation_remains_durably_releasable_during_operator_hold() {
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
            if message_id == 7 {
                service
                    .store
                    .set_disposable_worker_admission_hold(true)
                    .unwrap();
            }
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
        assert_eq!(bridge.acknowledgements, [(7, true), (8, false), (9, false)]);
    }
}
