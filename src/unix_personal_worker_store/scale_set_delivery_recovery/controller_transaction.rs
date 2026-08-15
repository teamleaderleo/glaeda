use super::*;

use crate::github_scale_set_protocol::ScaleSetRunnerRequestId;

use super::super::disposable_attempt_catalog::CATALOG_DOCUMENT;

pub(crate) enum ScaleSetExternalTransaction<T, E> {
    Completed(T),
    ExternalFailed(E),
}

impl UnixPersonalWorkerStore {
    /// Checkpoint acknowledgement, execute it once, and publish its exact response under one lock.
    pub(crate) fn acknowledge_scale_set_delivery_locked<E, F>(
        &mut self,
        expected: &ScaleSetDeliveryRecoveryState,
        acknowledge: F,
    ) -> Result<
        ScaleSetExternalTransaction<ScaleSetDeliveryRecoveryState, E>,
        PersonalWorkerStoreError,
    >
    where
        F: FnOnce(u32) -> Result<Vec<ScaleSetRunnerRequestId>, E>,
    {
        let _lock = self.acquire_mutation_lock()?;
        synchronize_directory(&self.directory, "personal worker store directory")?;
        self.refuse_other_unsettled_scale_set_state()?;
        self.recover_scale_set_delivery_locked()?;
        let current = self
            .load_scale_set_delivery_named(DELIVERY_RECOVERY_DOCUMENT)?
            .ok_or_else(|| {
                store_error(
                    PersonalWorkerStoreErrorKind::Missing,
                    "Scale Set delivery recovery state does not exist",
                )
            })?;
        if current != *expected
            || !matches!(current.phase(), ScaleSetDeliveryRecoveryPhase::Reconciled)
        {
            return Err(store_error(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "Scale Set delivery is not the exact reconciled acknowledgement candidate",
            ));
        }
        self.require_scale_set_catalog_binding(&current)?;

        let started = current.begin_ack().map_err(map_recovery_error)?;
        self.publish_scale_set_delivery_successor_locked(&current, &started)?;
        let acquired = match acknowledge(started.delivery().message_id()) {
            Ok(acquired) => acquired,
            Err(error) => return Ok(ScaleSetExternalTransaction::ExternalFailed(error)),
        };
        let acknowledged = started
            .record_ack_response(&acquired)
            .map_err(map_recovery_error)?;
        self.publish_scale_set_delivery_successor_locked(&started, &acknowledged)?;
        Ok(ScaleSetExternalTransaction::Completed(acknowledged))
    }

    /// Replay acquisition once and publish the accumulated positive evidence under one lock.
    pub(crate) fn recover_scale_set_acquisition_locked<E, F>(
        &mut self,
        expected: &ScaleSetDeliveryRecoveryState,
        acquire: F,
    ) -> Result<
        ScaleSetExternalTransaction<ScaleSetDeliveryRecoveryState, E>,
        PersonalWorkerStoreError,
    >
    where
        F: FnOnce(&[ScaleSetRunnerRequestId]) -> Result<Vec<ScaleSetRunnerRequestId>, E>,
    {
        let _lock = self.acquire_mutation_lock()?;
        synchronize_directory(&self.directory, "personal worker store directory")?;
        self.refuse_other_unsettled_scale_set_state()?;
        self.recover_scale_set_delivery_locked()?;
        let current = self
            .load_scale_set_delivery_named(DELIVERY_RECOVERY_DOCUMENT)?
            .ok_or_else(|| {
                store_error(
                    PersonalWorkerStoreErrorKind::Missing,
                    "Scale Set delivery recovery state does not exist",
                )
            })?;
        if current != *expected
            || !matches!(
                current.phase(),
                ScaleSetDeliveryRecoveryPhase::AcknowledgementStarted
                    | ScaleSetDeliveryRecoveryPhase::AcquisitionRecoveryObserved { .. }
            )
        {
            return Err(store_error(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "Scale Set delivery is not the exact acquisition recovery candidate",
            ));
        }
        self.require_scale_set_catalog_binding(&current)?;
        let available = current
            .delivery()
            .available_request_ids()
            .map_err(|_| PersonalWorkerStoreError::corrupt_state())?;
        if available.is_empty() {
            return Err(store_error(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "Scale Set delivery has no replayable acquisition identities",
            ));
        }
        let acquired = match acquire(&available) {
            Ok(acquired) => acquired,
            Err(error) => return Ok(ScaleSetExternalTransaction::ExternalFailed(error)),
        };
        let successor = current
            .record_recovery_acquire(&acquired)
            .map_err(map_recovery_error)?;
        if successor != current {
            self.publish_scale_set_delivery_successor_locked(&current, &successor)?;
        }
        Ok(ScaleSetExternalTransaction::Completed(successor))
    }

    fn publish_scale_set_delivery_successor_locked(
        &self,
        current: &ScaleSetDeliveryRecoveryState,
        successor: &ScaleSetDeliveryRecoveryState,
    ) -> Result<(), PersonalWorkerStoreError> {
        if !exact_recovery_successor(current, successor) {
            return Err(store_error(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "Scale Set delivery recovery successor is invalid",
            ));
        }
        let mut staged = self.stage_scale_set_delivery(successor)?;
        self.publish_named_staged(&mut staged, DELIVERY_RECOVERY_DOCUMENT, false)
    }

    fn require_scale_set_catalog_binding(
        &self,
        delivery: &ScaleSetDeliveryRecoveryState,
    ) -> Result<(), PersonalWorkerStoreError> {
        let catalog = self
            .load_catalog_named(CATALOG_DOCUMENT)
            .map_err(|_| PersonalWorkerStoreError::corrupt_state())?
            .ok_or_else(PersonalWorkerStoreError::corrupt_state)?;
        if !delivery.matches_catalog(&catalog) {
            return Err(PersonalWorkerStoreError::corrupt_state());
        }
        Ok(())
    }
}
