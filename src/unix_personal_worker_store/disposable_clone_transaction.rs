use super::*;

use crate::disposable_clone_runtime::{
    CloneRuntimeClock, DisposableCloneAdmissionSource, DisposableCloneRuntime,
    DisposableCloneRuntimeError, DisposableCloneRuntimeReceipt,
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
    ) -> Result<(), DisposableCloneRuntimeError> {
        let _lock = self
            .acquire_mutation_lock()
            .map_err(|_| DisposableCloneRuntimeError::durable("clone_store_lock_unavailable"))?;
        synchronize_directory(&self.directory, "personal worker store directory")
            .map_err(|_| DisposableCloneRuntimeError::durable("clone_store_sync_failed"))?;
        super::disposable_template_generation::refuse_unsettled(self).map_err(|_| {
            DisposableCloneRuntimeError::recovery("clone_template_recovery_required")
        })?;
        super::github_scale_set_inbox::refuse_unsettled(self)
            .map_err(|_| DisposableCloneRuntimeError::recovery("clone_inbox_recovery_required"))?;
        super::lima_authority::refuse_unsettled_lima_authority(self)
            .map_err(|_| DisposableCloneRuntimeError::recovery("clone_lima_recovery_required"))?;
        self.refuse_unsettled_personal_worker_state()
            .map_err(|_| DisposableCloneRuntimeError::recovery("clone_worker_recovery_required"))?;
        self.recover_catalog_locked()
            .map_err(|_| DisposableCloneRuntimeError::recovery("clone_catalog_recovery_failed"))?;

        let current = self
            .load_catalog_named(super::disposable_attempt_catalog::CATALOG_DOCUMENT)
            .map_err(|_| DisposableCloneRuntimeError::durable("clone_catalog_unavailable"))?
            .ok_or_else(|| DisposableCloneRuntimeError::durable("clone_catalog_missing"))?;
        if let Some(binding) =
            crate::disposable_clone_runtime::admission_seal::Sealed::scale_set_admission_binding(
                admission,
            )
        {
            let reservation = current
                .find_active(attempt_id)
                .ok_or_else(|| DisposableCloneRuntimeError::durable("clone_attempt_missing"))?;
            super::github_scale_set_inbox::require_settled_clone_admission(
                self,
                binding,
                &current,
                reservation,
            )
            .map_err(|_| DisposableCloneRuntimeError::recovery("clone_inbox_source_unavailable"))?;
        }
        let generation = self
            .load_template_generation_named(
                super::disposable_template_generation::GENERATION_DOCUMENT,
            )
            .map_err(|_| DisposableCloneRuntimeError::durable("clone_generation_unavailable"))?
            .ok_or_else(|| DisposableCloneRuntimeError::durable("clone_generation_missing"))?;
        runtime.authorize_locked(
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
        let authorized = current
            .replace_attempt(
                attempt_id,
                expected_attempt_revision,
                crate::disposable_attempt_catalog::DisposableAttemptCatalogAction::AuthorizeClone,
            )
            .map_err(|_| DisposableCloneRuntimeError::recovery("clone_authorization_refused"))?;
        authorized
            .validate_successor_of(&current)
            .map_err(|_| DisposableCloneRuntimeError::recovery("clone_authorization_invalid"))?;
        let mut staged = self.stage_catalog(&authorized).map_err(|_| {
            DisposableCloneRuntimeError::durable("clone_authorization_stage_failed")
        })?;
        self.publish_named_staged(
            &mut staged,
            super::disposable_attempt_catalog::CATALOG_DOCUMENT,
            false,
        )
        .map_err(|_| {
            DisposableCloneRuntimeError::recovery("clone_authorization_publish_ambiguous")
        })?;
        Ok(())
    }

    pub(crate) fn execute_disposable_clone_transaction(
        &mut self,
        runtime: &DisposableCloneRuntime,
        attempt_id: &DisposableAttemptId,
        admission: &impl DisposableCloneAdmissionSource,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<DisposableCloneRuntimeReceipt, DisposableCloneRuntimeError> {
        let _lock = self
            .acquire_mutation_lock()
            .map_err(|_| DisposableCloneRuntimeError::durable("clone_store_lock_unavailable"))?;
        synchronize_directory(&self.directory, "personal worker store directory")
            .map_err(|_| DisposableCloneRuntimeError::durable("clone_store_sync_failed"))?;
        super::disposable_template_generation::refuse_unsettled(self).map_err(|_| {
            DisposableCloneRuntimeError::recovery("clone_template_recovery_required")
        })?;
        super::github_scale_set_inbox::refuse_unsettled(self)
            .map_err(|_| DisposableCloneRuntimeError::recovery("clone_inbox_recovery_required"))?;
        super::lima_authority::refuse_unsettled_lima_authority(self)
            .map_err(|_| DisposableCloneRuntimeError::recovery("clone_lima_recovery_required"))?;
        self.refuse_unsettled_personal_worker_state()
            .map_err(|_| DisposableCloneRuntimeError::recovery("clone_worker_recovery_required"))?;
        self.recover_catalog_locked()
            .map_err(|_| DisposableCloneRuntimeError::recovery("clone_catalog_recovery_failed"))?;

        let current = self
            .load_catalog_named(super::disposable_attempt_catalog::CATALOG_DOCUMENT)
            .map_err(|_| DisposableCloneRuntimeError::durable("clone_catalog_unavailable"))?
            .ok_or_else(|| DisposableCloneRuntimeError::durable("clone_catalog_missing"))?;
        if let Some(binding) =
            crate::disposable_clone_runtime::admission_seal::Sealed::scale_set_admission_binding(
                admission,
            )
        {
            let reservation = current
                .find_active(attempt_id)
                .ok_or_else(|| DisposableCloneRuntimeError::durable("clone_attempt_missing"))?;
            super::github_scale_set_inbox::require_settled_clone_admission(
                self,
                binding,
                &current,
                reservation,
            )
            .map_err(|_| DisposableCloneRuntimeError::recovery("clone_inbox_source_unavailable"))?;
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
        let mut staged = self
            .stage_catalog(&started)
            .map_err(|_| DisposableCloneRuntimeError::durable("clone_checkpoint_stage_failed"))?;
        self.publish_named_staged(
            &mut staged,
            super::disposable_attempt_catalog::CATALOG_DOCUMENT,
            false,
        )
        .map_err(|_| DisposableCloneRuntimeError::recovery("clone_checkpoint_publish_ambiguous"))?;

        let identity =
            runtime.execute_locked(&started, attempt_id, &prepared, admission, executor, clock)?;
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
        runtime.receipt(&bound, attempt_id, &prepared.plan)
    }
}
