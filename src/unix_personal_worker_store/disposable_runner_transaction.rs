//! Canonical-lock transaction for one JIT registration and one disposable runner command.
//!
//! The transaction distinguishes proven GitHub registration absence from observation failure,
//! repeatedly reconfirms the complete running Lima target, publishes the exact service-assigned
//! runner ID, then publishes a no-replay Started checkpoint before the only secret-bearing command.
//! A discovered registration is retained for exact cleanup and never receives a second JIT value.

use super::*;

use crate::disposable_attempt_catalog::DisposableAttemptCatalogAction;
use crate::disposable_clone_runtime::{CloneRuntimeClock, DisposableCloneRuntime};
use crate::disposable_runner_runtime::{
    DisposableRunnerCommandReceipt, DisposableRunnerRegistrationSource, DisposableRunnerRuntime,
    DisposableRunnerRuntimeError,
};
use crate::disposable_worker_reconciler::DisposableAttemptId;
use crate::github_scale_set_bridge::ScaleSetRunnerLookup;
use crate::process::TimedCommandExecutor;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DisposableRunnerTransactionOutcome {
    RegistrationRecovered { attempt_id: String },
    CommandCompleted(DisposableRunnerCommandReceipt),
}

impl UnixPersonalWorkerStore {
    pub(crate) fn execute_disposable_runner_transaction(
        &mut self,
        runtime: &DisposableRunnerRuntime,
        clone_runtime: &DisposableCloneRuntime,
        attempt_id: &DisposableAttemptId,
        registration: &mut impl DisposableRunnerRegistrationSource,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<DisposableRunnerTransactionOutcome, DisposableRunnerRuntimeError> {
        let _lock = self
            .acquire_mutation_lock()
            .map_err(|_| DisposableRunnerRuntimeError::durable("runner_store_lock_unavailable"))?;
        synchronize_directory(&self.directory, "personal worker store directory")
            .map_err(|_| DisposableRunnerRuntimeError::durable("runner_store_sync_failed"))?;
        super::disposable_template_generation::refuse_unsettled(self).map_err(|_| {
            DisposableRunnerRuntimeError::recovery("runner_template_recovery_required")
        })?;
        super::github_scale_set_inbox::refuse_unsettled(self).map_err(|_| {
            DisposableRunnerRuntimeError::recovery("runner_inbox_recovery_required")
        })?;
        super::github_scale_set_inbox::require_settled_source(
            self,
            registration.scale_set_source_identity().as_str(),
        )
        .map_err(|_| DisposableRunnerRuntimeError::recovery("runner_inbox_source_unavailable"))?;
        super::lima_authority::refuse_unsettled_lima_authority(self)
            .map_err(|_| DisposableRunnerRuntimeError::recovery("runner_lima_recovery_required"))?;
        self.refuse_unsettled_personal_worker_state().map_err(|_| {
            DisposableRunnerRuntimeError::recovery("runner_worker_recovery_required")
        })?;
        self.recover_catalog_locked().map_err(|_| {
            DisposableRunnerRuntimeError::recovery("runner_catalog_recovery_failed")
        })?;

        let current = self
            .load_catalog_named(super::disposable_attempt_catalog::CATALOG_DOCUMENT)
            .map_err(|_| DisposableRunnerRuntimeError::durable("runner_catalog_unavailable"))?
            .ok_or_else(|| DisposableRunnerRuntimeError::durable("runner_catalog_missing"))?;
        let reservation = current
            .find_active(attempt_id)
            .ok_or_else(|| DisposableRunnerRuntimeError::durable("runner_attempt_missing"))?;
        let now = clock
            .epoch_millis()
            .map_err(|_| DisposableRunnerRuntimeError::observation("runner_clock_unavailable"))?;
        runtime.validate_candidate(reservation, now)?;
        let ready = clone_runtime
            .confirm_ready_worker(reservation, executor, clock)
            .map_err(|_| DisposableRunnerRuntimeError::observation("runner_target_not_ready"))?;
        ready.confirm_current().map_err(|_| {
            DisposableRunnerRuntimeError::observation("runner_target_identity_drift")
        })?;

        match registration.observe_runner(reservation.attempt().runner_name())? {
            ScaleSetRunnerLookup::Present(runner) => {
                let recovered = current
                    .replace_attempt(
                        attempt_id,
                        reservation.attempt().revision(),
                        DisposableAttemptCatalogAction::RecordRegistration(runner),
                    )
                    .map_err(|_| {
                        DisposableRunnerRuntimeError::recovery(
                            "runner_registration_recovery_refused",
                        )
                    })?;
                recovered.validate_successor_of(&current).map_err(|_| {
                    DisposableRunnerRuntimeError::recovery("runner_registration_recovery_invalid")
                })?;
                self.publish_runner_catalog(
                    &recovered,
                    "runner_registration_recovery_stage_failed",
                    "runner_registration_recovery_publish_ambiguous",
                )?;
                return Ok(DisposableRunnerTransactionOutcome::RegistrationRecovered {
                    attempt_id: attempt_id.as_str().to_owned(),
                });
            }
            ScaleSetRunnerLookup::Absent => {}
        }

        let ready_after_lookup = clone_runtime
            .confirm_ready_worker(reservation, executor, clock)
            .map_err(|_| DisposableRunnerRuntimeError::observation("runner_target_not_ready"))?;
        ready.confirm_current().map_err(|_| {
            DisposableRunnerRuntimeError::observation("runner_target_identity_drift")
        })?;
        ready_after_lookup.confirm_current().map_err(|_| {
            DisposableRunnerRuntimeError::observation("runner_target_identity_drift")
        })?;
        let before_jit = clock
            .epoch_millis()
            .map_err(|_| DisposableRunnerRuntimeError::observation("runner_clock_unavailable"))?;
        runtime.validate_candidate(reservation, before_jit)?;

        let jit = registration.generate_jit(reservation.attempt().runner_name())?;
        let plan = runtime.plan_launch(reservation, before_jit, jit)?;
        let registered = current
            .replace_attempt(
                attempt_id,
                reservation.attempt().revision(),
                DisposableAttemptCatalogAction::RecordRegistration(plan.runner().clone()),
            )
            .map_err(|_| {
                DisposableRunnerRuntimeError::recovery("runner_registration_checkpoint_refused")
            })?;
        registered.validate_successor_of(&current).map_err(|_| {
            DisposableRunnerRuntimeError::recovery("runner_registration_checkpoint_invalid")
        })?;
        self.publish_runner_catalog(
            &registered,
            "runner_registration_stage_failed",
            "runner_registration_publish_ambiguous",
        )?;

        let registered_reservation = registered
            .find_active(attempt_id)
            .ok_or_else(|| DisposableRunnerRuntimeError::durable("runner_attempt_missing"))?;
        let ready_after_jit = clone_runtime
            .confirm_ready_worker(registered_reservation, executor, clock)
            .map_err(|_| DisposableRunnerRuntimeError::observation("runner_target_not_ready"))?;
        ready_after_lookup.confirm_current().map_err(|_| {
            DisposableRunnerRuntimeError::observation("runner_target_identity_drift")
        })?;
        ready_after_jit.confirm_current().map_err(|_| {
            DisposableRunnerRuntimeError::observation("runner_target_identity_drift")
        })?;
        let before_checkpoint = clock
            .epoch_millis()
            .map_err(|_| DisposableRunnerRuntimeError::observation("runner_clock_unavailable"))?;
        plan.validate_registered(registered_reservation, before_checkpoint)?;

        let started = registered
            .replace_attempt(
                attempt_id,
                registered_reservation.attempt().revision(),
                DisposableAttemptCatalogAction::RecordRunnerStartStarted,
            )
            .map_err(|_| {
                DisposableRunnerRuntimeError::recovery("runner_start_checkpoint_refused")
            })?;
        started.validate_successor_of(&registered).map_err(|_| {
            DisposableRunnerRuntimeError::recovery("runner_start_checkpoint_invalid")
        })?;
        self.publish_runner_catalog(
            &started,
            "runner_start_stage_failed",
            "runner_start_publish_ambiguous",
        )?;

        let started_reservation = started
            .find_active(attempt_id)
            .ok_or_else(|| DisposableRunnerRuntimeError::durable("runner_attempt_missing"))?;
        ready_after_jit.confirm_current().map_err(|_| {
            DisposableRunnerRuntimeError::observation("runner_target_identity_drift")
        })?;
        let ready_after_checkpoint = clone_runtime
            .confirm_ready_worker(started_reservation, executor, clock)
            .map_err(|_| DisposableRunnerRuntimeError::observation("runner_target_not_ready"))?;
        ready_after_checkpoint.confirm_current().map_err(|_| {
            DisposableRunnerRuntimeError::observation("runner_target_identity_drift")
        })?;
        let command_started_at = clock
            .epoch_millis()
            .map_err(|_| DisposableRunnerRuntimeError::observation("runner_clock_unavailable"))?;
        let receipt = plan.execute_started(started_reservation, command_started_at, executor)?;
        ready_after_checkpoint.confirm_current().map_err(|_| {
            DisposableRunnerRuntimeError::recovery("runner_target_post_command_drift")
        })?;
        Ok(DisposableRunnerTransactionOutcome::CommandCompleted(
            receipt,
        ))
    }

    fn publish_runner_catalog(
        &mut self,
        document: &crate::disposable_attempt_catalog::DisposableAttemptCatalogDocument,
        stage_code: &'static str,
        publish_code: &'static str,
    ) -> Result<(), DisposableRunnerRuntimeError> {
        let mut staged = self
            .stage_catalog(document)
            .map_err(|_| DisposableRunnerRuntimeError::durable(stage_code))?;
        self.publish_named_staged(
            &mut staged,
            super::disposable_attempt_catalog::CATALOG_DOCUMENT,
            false,
        )
        .map_err(|_| DisposableRunnerRuntimeError::recovery(publish_code))
    }
}
