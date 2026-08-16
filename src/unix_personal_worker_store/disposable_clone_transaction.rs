use super::*;

use crate::disposable_clone_runtime::{
    CloneRuntimeClock, DisposableCloneAdmissionSource, DisposableCloneRuntime,
    DisposableCloneRuntimeError, DisposableCloneTransactionOutcome,
};
use crate::disposable_worker_reconciler::DisposableAttemptId;
use crate::process::TimedCommandExecutor;

impl UnixPersonalWorkerStore {
    pub(crate) fn authorize_disposable_clone_transaction(
        &mut self,
        runtime: &DisposableCloneRuntime,
        attempt_id: &DisposableAttemptId,
        admission: &impl DisposableCloneAdmissionSource,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<DisposableCloneTransactionOutcome, DisposableCloneRuntimeError> {
        let _lock = self.prepare_clone_transaction()?;
        let current = self.load_clone_catalog()?;
        let transition =
            runtime.authorize_locked(&current, attempt_id, admission, executor, clock)?;
        let reservation = current
            .find_active(attempt_id)
            .ok_or_else(|| DisposableCloneRuntimeError::durable("clone_attempt_missing"))?;
        let next = current
            .replace_attempt(attempt_id, reservation.attempt().revision(), transition)
            .map_err(|_| DisposableCloneRuntimeError::recovery("clone_authorization_refused"))?;
        next.validate_successor_of(&current)
            .map_err(|_| DisposableCloneRuntimeError::recovery("clone_authorization_invalid"))?;
        self.publish_clone_catalog(
            &next,
            "clone_authorization_stage_failed",
            "clone_authorization_publish_ambiguous",
        )?;
        let phase = next
            .find_active(attempt_id)
            .ok_or_else(|| DisposableCloneRuntimeError::durable("clone_attempt_missing"))?
            .attempt()
            .phase();
        Ok(DisposableCloneTransactionOutcome::PrecloneCheckpointed {
            attempt_id: attempt_id.as_str().to_owned(),
            phase,
        })
    }

    pub(crate) fn execute_disposable_clone_transaction(
        &mut self,
        runtime: &DisposableCloneRuntime,
        attempt_id: &DisposableAttemptId,
        admission: &impl DisposableCloneAdmissionSource,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<DisposableCloneTransactionOutcome, DisposableCloneRuntimeError> {
        let _lock = self.prepare_clone_transaction()?;
        let current = self.load_clone_catalog()?;
        if let Some(transition) = runtime
            .authorize_clone_execution_locked(&current, attempt_id, admission, executor, clock)?
        {
            let reservation = current
                .find_active(attempt_id)
                .ok_or_else(|| DisposableCloneRuntimeError::durable("clone_attempt_missing"))?;
            let next = current
                .replace_attempt(attempt_id, reservation.attempt().revision(), transition)
                .map_err(|_| {
                    DisposableCloneRuntimeError::recovery("clone_precommand_cleanup_refused")
                })?;
            next.validate_successor_of(&current).map_err(|_| {
                DisposableCloneRuntimeError::recovery("clone_precommand_cleanup_invalid")
            })?;
            let phase = next
                .find_active(attempt_id)
                .ok_or_else(|| DisposableCloneRuntimeError::durable("clone_attempt_missing"))?
                .attempt()
                .phase();
            self.publish_clone_catalog(
                &next,
                "clone_precommand_cleanup_stage_failed",
                "clone_precommand_cleanup_publish_ambiguous",
            )?;
            return Ok(DisposableCloneTransactionOutcome::PrecloneCheckpointed {
                attempt_id: attempt_id.as_str().to_owned(),
                phase,
            });
        }
        let generation = self
            .load_template_generation_named(
                super::disposable_template_generation::GENERATION_DOCUMENT,
            )
            .map_err(|_| DisposableCloneRuntimeError::durable("clone_generation_unavailable"))?
            .ok_or_else(|| DisposableCloneRuntimeError::durable("clone_generation_missing"))?;
        let prepared = runtime.prepare_locked(
            &current,
            &generation,
            attempt_id,
            admission,
            executor,
            clock,
        )?;
        let expected_attempt_revision = current
            .find_active(attempt_id)
            .ok_or_else(|| DisposableCloneRuntimeError::durable("clone_attempt_missing"))?
            .attempt()
            .revision();
        let started = current
            .checkpoint_clone_started(attempt_id, expected_attempt_revision)
            .map_err(|_| DisposableCloneRuntimeError::recovery("clone_checkpoint_refused"))?;
        started
            .validate_successor_of(&current)
            .map_err(|_| DisposableCloneRuntimeError::recovery("clone_checkpoint_invalid"))?;
        let checkpoint_ready = runtime.preflight_checkpointed_clone_locked(
            &started, attempt_id, prepared, admission, executor, clock,
        )?;
        let mut staged = self
            .stage_catalog(&started)
            .map_err(|_| DisposableCloneRuntimeError::durable("clone_checkpoint_stage_failed"))?;
        self.publish_named_staged(
            &mut staged,
            super::disposable_attempt_catalog::CATALOG_DOCUMENT,
            false,
        )
        .map_err(|_| DisposableCloneRuntimeError::recovery("clone_checkpoint_publish_ambiguous"))?;

        let (identity, plan) =
            runtime.execute_checkpointed_clone_locked(checkpoint_ready, executor, clock)?;
        let started_attempt_revision = started
            .find_active(attempt_id)
            .ok_or_else(|| DisposableCloneRuntimeError::durable("clone_attempt_missing"))?
            .attempt()
            .revision();
        let bound = started
            .bind_vm_identity_after_clone(attempt_id, started_attempt_revision, identity)
            .map_err(|_| DisposableCloneRuntimeError::recovery("clone_identity_bind_refused"))?;
        bound
            .validate_recovery_successor_of(&started)
            .map_err(|_| DisposableCloneRuntimeError::recovery("clone_identity_bind_invalid"))?;
        let mut staged = self
            .stage_catalog(&bound)
            .map_err(|_| DisposableCloneRuntimeError::durable("clone_identity_stage_failed"))?;
        self.publish_named_staged(
            &mut staged,
            super::disposable_attempt_catalog::CATALOG_DOCUMENT,
            false,
        )
        .map_err(|_| DisposableCloneRuntimeError::recovery("clone_identity_publish_ambiguous"))?;
        runtime
            .receipt(&bound, attempt_id, &plan)
            .map(DisposableCloneTransactionOutcome::Completed)
    }

    pub(crate) fn checkpoint_disposable_registration_transaction(
        &mut self,
        runtime: &DisposableCloneRuntime,
        attempt_id: &DisposableAttemptId,
        admission: &impl DisposableCloneAdmissionSource,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<DisposableCloneTransactionOutcome, DisposableCloneRuntimeError> {
        let _lock = self.prepare_clone_transaction()?;
        let current = self.load_clone_catalog()?;
        let transition = runtime
            .authorize_registration_locked(&current, attempt_id, admission, executor, clock)?;
        let reservation = current
            .find_active(attempt_id)
            .ok_or_else(|| DisposableCloneRuntimeError::durable("clone_attempt_missing"))?;
        let next = current
            .replace_attempt(attempt_id, reservation.attempt().revision(), transition)
            .map_err(|_| {
                DisposableCloneRuntimeError::recovery("clone_registration_checkpoint_refused")
            })?;
        next.validate_successor_of(&current).map_err(|_| {
            DisposableCloneRuntimeError::recovery("clone_registration_checkpoint_invalid")
        })?;
        let phase = next
            .find_active(attempt_id)
            .ok_or_else(|| DisposableCloneRuntimeError::durable("clone_attempt_missing"))?
            .attempt()
            .phase();
        self.publish_clone_catalog(
            &next,
            "clone_registration_checkpoint_stage_failed",
            "clone_registration_checkpoint_publish_ambiguous",
        )?;
        Ok(
            DisposableCloneTransactionOutcome::RegistrationCheckpointed {
                attempt_id: attempt_id.as_str().to_owned(),
                phase,
            },
        )
    }

    fn prepare_clone_transaction(
        &mut self,
    ) -> Result<StoreMutationLock, DisposableCloneRuntimeError> {
        let lock = self
            .acquire_mutation_lock()
            .map_err(|_| DisposableCloneRuntimeError::durable("clone_store_lock_unavailable"))?;
        synchronize_directory(&self.directory, "personal worker store directory")
            .map_err(|_| DisposableCloneRuntimeError::durable("clone_store_sync_failed"))?;
        super::disposable_template_generation::refuse_unsettled(self).map_err(|_| {
            DisposableCloneRuntimeError::recovery("clone_template_recovery_required")
        })?;
        super::lima_authority::refuse_unsettled_lima_authority(self)
            .map_err(|_| DisposableCloneRuntimeError::recovery("clone_lima_recovery_required"))?;
        super::scale_set_delivery_recovery::refuse_unsettled(self).map_err(|_| {
            DisposableCloneRuntimeError::recovery("clone_delivery_recovery_required")
        })?;
        self.refuse_unsettled_personal_worker_state()
            .map_err(|_| DisposableCloneRuntimeError::recovery("clone_worker_recovery_required"))?;
        self.recover_catalog_locked()
            .map_err(|_| DisposableCloneRuntimeError::recovery("clone_catalog_recovery_failed"))?;
        Ok(lock)
    }

    fn load_clone_catalog(
        &self,
    ) -> Result<
        crate::disposable_attempt_catalog::DisposableAttemptCatalogDocument,
        DisposableCloneRuntimeError,
    > {
        self.load_catalog_named(super::disposable_attempt_catalog::CATALOG_DOCUMENT)
            .map_err(|_| DisposableCloneRuntimeError::durable("clone_catalog_unavailable"))?
            .ok_or_else(|| DisposableCloneRuntimeError::durable("clone_catalog_missing"))
    }

    fn publish_clone_catalog(
        &mut self,
        document: &crate::disposable_attempt_catalog::DisposableAttemptCatalogDocument,
        stage_code: &'static str,
        publish_code: &'static str,
    ) -> Result<(), DisposableCloneRuntimeError> {
        let mut staged = self
            .stage_catalog(document)
            .map_err(|_| DisposableCloneRuntimeError::durable(stage_code))?;
        self.publish_named_staged(
            &mut staged,
            super::disposable_attempt_catalog::CATALOG_DOCUMENT,
            false,
        )
        .map_err(|_| DisposableCloneRuntimeError::recovery(publish_code))
    }
}
