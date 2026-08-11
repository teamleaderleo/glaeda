//! Canonical-lock reconciliation for terminal disposable worker cleanup.
//!
//! Each invocation begins from current durable state and performs at most one external mutation.
//! VM and runner deletion are observation-first and idempotent: an ambiguous result leaves the
//! cleanup phase unchanged, so the next tick observes exact ownership or absence before retrying.

use super::*;

use crate::disposable_attempt_catalog::{
    DisposableAttemptCatalogAction, DisposableAttemptCatalogDocument,
};
use crate::disposable_clone_runtime::{
    CloneRuntimeClock, DisposableCleanupRunnerSource, DisposableCleanupTransactionOutcome,
    DisposableCleanupVmObservation, DisposableCloneRuntime, DisposableCloneRuntimeError,
};
use crate::disposable_worker_reconciler::{
    DisposableAttemptId, DisposableAttemptPhase, DisposableVmObservation, DisposableWorkerAction,
    DisposableWorkerReconcileInput, ScaleSetRunnerObservation, reconcile_attempt,
};
use crate::github_scale_set_bridge::ScaleSetRunnerLookup;
use crate::github_scale_set_protocol::ScaleSetRunnerReference;
use crate::process::TimedCommandExecutor;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DisposableOrphanCleanupOutcome {
    Satisfied,
    VmDestroyed,
    RunnerDeleted,
}

impl UnixPersonalWorkerStore {
    pub(crate) fn execute_disposable_orphan_cleanup_transaction(
        &mut self,
        runtime: &DisposableCloneRuntime,
        attempt_id: &DisposableAttemptId,
        runner_source: &mut impl DisposableCleanupRunnerSource,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<DisposableOrphanCleanupOutcome, DisposableCloneRuntimeError> {
        let _lock = self
            .acquire_mutation_lock()
            .map_err(|_| DisposableCloneRuntimeError::durable("orphan_store_lock_unavailable"))?;
        synchronize_directory(&self.directory, "personal worker store directory")
            .map_err(|_| DisposableCloneRuntimeError::durable("orphan_store_sync_failed"))?;
        super::disposable_template_generation::refuse_unsettled(self).map_err(|_| {
            DisposableCloneRuntimeError::recovery("orphan_template_recovery_required")
        })?;
        super::github_scale_set_inbox::refuse_unsettled(self)
            .map_err(|_| DisposableCloneRuntimeError::recovery("orphan_inbox_recovery_required"))?;
        super::github_scale_set_inbox::require_settled_source(
            self,
            runner_source.scale_set_source_identity().as_str(),
        )
        .map_err(|_| DisposableCloneRuntimeError::recovery("orphan_inbox_source_unavailable"))?;
        super::lima_authority::refuse_unsettled_lima_authority(self)
            .map_err(|_| DisposableCloneRuntimeError::recovery("orphan_lima_recovery_required"))?;
        self.refuse_unsettled_personal_worker_state().map_err(|_| {
            DisposableCloneRuntimeError::recovery("orphan_worker_recovery_required")
        })?;
        self.recover_catalog_locked()
            .map_err(|_| DisposableCloneRuntimeError::recovery("orphan_catalog_recovery_failed"))?;

        let current = self
            .load_catalog_named(super::disposable_attempt_catalog::CATALOG_DOCUMENT)
            .map_err(|_| DisposableCloneRuntimeError::durable("orphan_catalog_unavailable"))?
            .ok_or_else(|| DisposableCloneRuntimeError::durable("orphan_catalog_missing"))?;
        if !current.active().is_empty() {
            return Err(DisposableCloneRuntimeError::recovery(
                "orphan_active_attempt_present",
            ));
        }
        let Some(attempt) = current.find_tombstone(attempt_id) else {
            // Bounded FIFO retirement may evict an older startup audit candidate before it is
            // reached. With no retained authority, no external object may be mutated.
            return Ok(DisposableOrphanCleanupOutcome::Satisfied);
        };
        if attempt.phase() != DisposableAttemptPhase::Complete {
            return Err(DisposableCloneRuntimeError::recovery(
                "orphan_tombstone_phase_mismatch",
            ));
        }

        if attempt.vm_identity().is_some() {
            match runtime.observe_completed_orphan_worker(attempt, executor, clock)? {
                DisposableCleanupVmObservation::Absent => {}
                DisposableCleanupVmObservation::Present(confirmed) => {
                    runtime
                        .destroy_completed_orphan_worker(attempt, &confirmed, executor, clock)?;
                    return Ok(DisposableOrphanCleanupOutcome::VmDestroyed);
                }
            }
        }

        let observed_runner = runner_source.observe_runner(attempt.runner_name())?;
        match observed_runner {
            ScaleSetRunnerLookup::Absent => Ok(DisposableOrphanCleanupOutcome::Satisfied),
            ScaleSetRunnerLookup::Present(observed) => {
                let expected = attempt
                    .runner_id()
                    .map(|id| ScaleSetRunnerReference::new(id, attempt.runner_name().clone()))
                    .ok_or_else(|| {
                        DisposableCloneRuntimeError::recovery("orphan_runner_identity_unavailable")
                    })?;
                if observed != expected {
                    return Err(DisposableCloneRuntimeError::recovery(
                        "orphan_runner_identity_drift",
                    ));
                }
                runner_source.remove_runner(&expected)?;
                match runner_source.observe_runner(attempt.runner_name())? {
                    ScaleSetRunnerLookup::Absent => {
                        Ok(DisposableOrphanCleanupOutcome::RunnerDeleted)
                    }
                    ScaleSetRunnerLookup::Present(_) => Err(DisposableCloneRuntimeError::recovery(
                        "orphan_runner_delete_not_observed",
                    )),
                }
            }
        }
    }

    pub(crate) fn execute_disposable_cleanup_transaction(
        &mut self,
        runtime: &DisposableCloneRuntime,
        attempt_id: &DisposableAttemptId,
        runner_source: &mut impl DisposableCleanupRunnerSource,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<DisposableCleanupTransactionOutcome, DisposableCloneRuntimeError> {
        let _lock = self
            .acquire_mutation_lock()
            .map_err(|_| DisposableCloneRuntimeError::durable("cleanup_store_lock_unavailable"))?;
        synchronize_directory(&self.directory, "personal worker store directory")
            .map_err(|_| DisposableCloneRuntimeError::durable("cleanup_store_sync_failed"))?;
        super::disposable_template_generation::refuse_unsettled(self).map_err(|_| {
            DisposableCloneRuntimeError::recovery("cleanup_template_recovery_required")
        })?;
        super::github_scale_set_inbox::refuse_unsettled(self).map_err(|_| {
            DisposableCloneRuntimeError::recovery("cleanup_inbox_recovery_required")
        })?;
        super::github_scale_set_inbox::require_settled_source(
            self,
            runner_source.scale_set_source_identity().as_str(),
        )
        .map_err(|_| DisposableCloneRuntimeError::recovery("cleanup_inbox_source_unavailable"))?;
        super::lima_authority::refuse_unsettled_lima_authority(self)
            .map_err(|_| DisposableCloneRuntimeError::recovery("cleanup_lima_recovery_required"))?;
        self.refuse_unsettled_personal_worker_state().map_err(|_| {
            DisposableCloneRuntimeError::recovery("cleanup_worker_recovery_required")
        })?;
        self.recover_catalog_locked().map_err(|_| {
            DisposableCloneRuntimeError::recovery("cleanup_catalog_recovery_failed")
        })?;

        let current = self
            .load_catalog_named(super::disposable_attempt_catalog::CATALOG_DOCUMENT)
            .map_err(|_| DisposableCloneRuntimeError::durable("cleanup_catalog_unavailable"))?
            .ok_or_else(|| DisposableCloneRuntimeError::durable("cleanup_catalog_missing"))?;
        let reservation = current
            .find_active(attempt_id)
            .ok_or_else(|| DisposableCloneRuntimeError::durable("cleanup_attempt_missing"))?;
        let phase = reservation.attempt().phase();
        if phase == DisposableAttemptPhase::Terminal {
            return self.publish_cleanup_action(
                &current,
                attempt_id,
                DisposableAttemptCatalogAction::BeginCleanup,
            );
        }
        if !matches!(
            phase,
            DisposableAttemptPhase::Destroying
                | DisposableAttemptPhase::Deregistering
                | DisposableAttemptPhase::Releasing
        ) {
            return Err(DisposableCloneRuntimeError::recovery(
                "cleanup_phase_mismatch",
            ));
        }

        let now = clock
            .epoch_millis()
            .map_err(|_| DisposableCloneRuntimeError::observation("cleanup_clock_unavailable"))?;
        let vm = runtime.observe_cleanup_worker(reservation, executor, clock)?;
        let (vm_observation, vm_identity) = match &vm {
            DisposableCleanupVmObservation::Absent => (DisposableVmObservation::Absent, None),
            DisposableCleanupVmObservation::Present(_) => (
                DisposableVmObservation::Stopped,
                reservation.attempt().vm_identity(),
            ),
        };
        if let DisposableCleanupVmObservation::Present(confirmed) = &vm {
            let action = reconcile_attempt(DisposableWorkerReconcileInput {
                now,
                attempt: reservation.attempt(),
                vm: vm_observation,
                vm_identity,
                runner: ScaleSetRunnerObservation::Unknown,
                job_event: None,
                capacity_reserved: true,
                cancellation_requested: false,
            })
            .map_err(|_| DisposableCloneRuntimeError::recovery("cleanup_reconcile_failed"))?;
            if action != DisposableWorkerAction::DestroyVm {
                return Err(DisposableCloneRuntimeError::recovery(
                    "cleanup_destroy_not_authorized",
                ));
            }
            runtime.destroy_cleanup_worker(reservation, confirmed, executor, clock)?;
            return Ok(DisposableCleanupTransactionOutcome::VmDestroyed {
                attempt_id: attempt_id.as_str().to_owned(),
            });
        }

        let runner = if phase == DisposableAttemptPhase::Destroying {
            ScaleSetRunnerObservation::Unknown
        } else {
            match runner_source.observe_runner(reservation.attempt().runner_name())? {
                ScaleSetRunnerLookup::Absent => ScaleSetRunnerObservation::Absent,
                ScaleSetRunnerLookup::Present(runner) => {
                    ScaleSetRunnerObservation::RegistrationOnly { runner }
                }
            }
        };
        let action = reconcile_attempt(DisposableWorkerReconcileInput {
            now,
            attempt: reservation.attempt(),
            vm: DisposableVmObservation::Absent,
            vm_identity: None,
            runner,
            job_event: None,
            capacity_reserved: true,
            cancellation_requested: false,
        })
        .map_err(|_| DisposableCloneRuntimeError::recovery("cleanup_reconcile_failed"))?;
        match action {
            DisposableWorkerAction::Persist { transition } => {
                self.publish_cleanup_action(&current, attempt_id, transition)
            }
            DisposableWorkerAction::DeleteRunner { runner } => {
                runner_source.remove_runner(&runner)?;
                match runner_source.observe_runner(reservation.attempt().runner_name())? {
                    ScaleSetRunnerLookup::Absent => {
                        Ok(DisposableCleanupTransactionOutcome::RunnerDeleted {
                            attempt_id: attempt_id.as_str().to_owned(),
                        })
                    }
                    ScaleSetRunnerLookup::Present(_) => Err(DisposableCloneRuntimeError::recovery(
                        "cleanup_runner_delete_not_observed",
                    )),
                }
            }
            DisposableWorkerAction::ReleaseCapacity => self.publish_cleanup_action(
                &current,
                attempt_id,
                DisposableAttemptCatalogAction::AdvanceCleanup(DisposableAttemptPhase::Complete),
            ),
            _ => Err(DisposableCloneRuntimeError::recovery(
                "cleanup_action_not_executable",
            )),
        }
    }

    fn publish_cleanup_action(
        &mut self,
        current: &DisposableAttemptCatalogDocument,
        attempt_id: &DisposableAttemptId,
        action: DisposableAttemptCatalogAction,
    ) -> Result<DisposableCleanupTransactionOutcome, DisposableCloneRuntimeError> {
        let reservation = current
            .find_active(attempt_id)
            .ok_or_else(|| DisposableCloneRuntimeError::durable("cleanup_attempt_missing"))?;
        let next = current
            .replace_attempt(attempt_id, reservation.attempt().revision(), action)
            .map_err(|_| DisposableCloneRuntimeError::recovery("cleanup_checkpoint_refused"))?;
        next.validate_successor_of(current)
            .map_err(|_| DisposableCloneRuntimeError::recovery("cleanup_checkpoint_invalid"))?;
        let next_phase = next
            .find_active(attempt_id)
            .ok_or_else(|| DisposableCloneRuntimeError::durable("cleanup_attempt_missing"))?
            .attempt()
            .phase();
        let mut staged = self
            .stage_catalog(&next)
            .map_err(|_| DisposableCloneRuntimeError::durable("cleanup_checkpoint_stage_failed"))?;
        self.publish_named_staged(
            &mut staged,
            super::disposable_attempt_catalog::CATALOG_DOCUMENT,
            false,
        )
        .map_err(|_| {
            DisposableCloneRuntimeError::recovery("cleanup_checkpoint_publish_ambiguous")
        })?;
        if next_phase == DisposableAttemptPhase::Complete {
            Ok(DisposableCleanupTransactionOutcome::CapacityReleased {
                attempt_id: attempt_id.as_str().to_owned(),
            })
        } else {
            Ok(DisposableCleanupTransactionOutcome::CleanupCheckpointed {
                attempt_id: attempt_id.as_str().to_owned(),
                phase: next_phase,
            })
        }
    }
}
