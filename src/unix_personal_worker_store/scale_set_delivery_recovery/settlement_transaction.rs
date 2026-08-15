use super::*;

use crate::disposable_attempt_catalog::{
    DisposableAttemptCatalogDocument, DisposableAttemptCatalogError,
    DisposableAttemptCatalogErrorKind,
};
use crate::github_scale_set_delivery_settlement::settle_scale_set_delivery_catalog;

use super::super::disposable_attempt_catalog::{CATALOG_DOCUMENT, STAGED_CATALOG_DOCUMENT};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScaleSetDeliverySettlement {
    catalog: DisposableAttemptCatalogDocument,
    prepared: ScaleSetDeliveryRecoveryState,
    acquired: usize,
}

impl ScaleSetDeliverySettlement {
    pub(crate) fn acquired(&self) -> usize {
        self.acquired
    }

    fn matches_expected(&self, expected: &ScaleSetDeliveryRecoveryState) -> bool {
        expected == &self.prepared
            || self
                .prepared
                .settlement_prior_catalog()
                .is_some_and(|prior| {
                    expected
                        .prepare_settlement(prior, &self.catalog)
                        .is_ok_and(|prepared| prepared == self.prepared)
                })
    }
}

impl UnixPersonalWorkerStore {
    /// Open the controller store and jointly recover reconciliation or settlement debt.
    pub(crate) fn open_or_create_scale_set_delivery_controller(
        root_path: impl AsRef<Path>,
    ) -> Result<Self, PersonalWorkerStoreError> {
        let root = fs::open(root_path.as_ref(), DIRECTORY_FLAGS, Mode::empty())
            .map_err(map_root_open_error)?;
        let root_stat = inspect_directory(&root, "Scale Set delivery controller root", None)?;
        let owner = (root_stat.st_uid, root_stat.st_gid);
        let (directory, publication_lock) = open_or_publish_initialization_directory(&root, owner)?;
        let mut store = Self {
            _root: root,
            directory,
            owner,
        };
        let _lock = match publication_lock {
            Some(lock) => lock,
            None => store.acquire_mutation_lock()?,
        };
        synchronize_directory(&store._root, "Scale Set delivery controller root")?;
        synchronize_directory(&store.directory, "personal worker store directory")?;
        store.recover_scale_set_settlement_locked()?;
        store.recover_scale_set_reconcile_transaction_locked()?;
        Ok(store)
    }

    /// Atomically remove one conclusive delivery fence and publish its exact catalog settlement.
    pub(crate) fn settle_scale_set_delivery_locked(
        &mut self,
        expected: &ScaleSetDeliveryRecoveryState,
    ) -> Result<ScaleSetDeliverySettlement, PersonalWorkerStoreError> {
        let _lock = self.acquire_mutation_lock()?;
        synchronize_directory(&self.directory, "personal worker store directory")?;
        if let Some(settled) = self.recover_scale_set_settlement_locked()? {
            if settled.matches_expected(expected) {
                return Ok(settled);
            }
            return Err(store_error(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "a different Scale Set delivery settlement was recovered",
            ));
        }
        self.recover_scale_set_reconcile_transaction_locked()?;

        let current = self
            .load_scale_set_delivery_named(DELIVERY_RECOVERY_DOCUMENT)?
            .ok_or_else(|| {
                store_error(
                    PersonalWorkerStoreErrorKind::RevisionConflict,
                    "Scale Set delivery was already settled",
                )
            })?;
        if current != *expected {
            return Err(store_error(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "Scale Set delivery changed before settlement",
            ));
        }
        let catalog = self
            .load_catalog_named(CATALOG_DOCUMENT)
            .map_err(map_catalog_error)?
            .ok_or_else(PersonalWorkerStoreError::corrupt_state)?;
        let target = settle_scale_set_delivery_catalog(&current, &catalog)
            .map_err(|_| PersonalWorkerStoreError::corrupt_state())?;
        let prepared = current
            .prepare_settlement(&catalog, &target)
            .map_err(map_recovery_error)?;

        let mut staged_delivery = self.stage_scale_set_delivery(&prepared)?;
        // The delivery intent is durable before a target catalog stage may exist, preventing a
        // catalog-only crash shape that could be mistaken for an ordinary catalog transition.
        synchronize_directory(&self.directory, "Scale Set settlement delivery stage")?;
        let mut staged_catalog = if target != catalog {
            let staged = self.stage_catalog(&target).map_err(map_catalog_error)?;
            synchronize_directory(&self.directory, "Scale Set settlement stages")?;
            Some(staged)
        } else {
            None
        };

        self.publish_named_staged(&mut staged_delivery, DELIVERY_RECOVERY_DOCUMENT, false)?;
        if let Some(staged_catalog) = staged_catalog.as_mut() {
            self.publish_named_staged(staged_catalog, CATALOG_DOCUMENT, false)?;
        }
        self.remove_settled_delivery(&prepared)?;
        let acquired = prepared
            .settlement_acquired()
            .ok_or_else(PersonalWorkerStoreError::corrupt_state)?
            .len();
        Ok(ScaleSetDeliverySettlement {
            catalog: target,
            prepared,
            acquired,
        })
    }

    fn recover_scale_set_settlement_locked(
        &mut self,
    ) -> Result<Option<ScaleSetDeliverySettlement>, PersonalWorkerStoreError> {
        let current = self.load_scale_set_delivery_named(DELIVERY_RECOVERY_DOCUMENT)?;
        let staged = self.load_scale_set_delivery_named(STAGED_DELIVERY_RECOVERY_DOCUMENT)?;
        let settlement_is_present = current.as_ref().is_some_and(|state| {
            matches!(
                state.phase(),
                ScaleSetDeliveryRecoveryPhase::SettlementPrepared { .. }
            )
        }) || staged.as_ref().is_some_and(|state| {
            matches!(
                state.phase(),
                ScaleSetDeliveryRecoveryPhase::SettlementPrepared { .. }
            )
        });
        if !settlement_is_present {
            return Ok(None);
        }

        // Only the catalog stage may coexist with settlement. Every other durable neighbor remains
        // a veto before this transaction classifies or mutates either document.
        match self.recovery_plan()? {
            StoreRecoveryPlan::Clean { .. } => {}
            StoreRecoveryPlan::PublishStaged { .. }
            | StoreRecoveryPlan::RemoveStaleStaged { .. } => {
                return Err(store_error(
                    PersonalWorkerStoreErrorKind::RevisionConflict,
                    "personal-worker recovery conflicts with Scale Set settlement",
                ));
            }
        }
        super::super::disposable_template_generation::refuse_unsettled(self)?;
        super::super::lima_authority::refuse_unsettled_lima_authority(self)?;

        if staged.is_some() {
            self.recover_scale_set_delivery_locked()?;
        }
        let prepared = self
            .load_scale_set_delivery_named(DELIVERY_RECOVERY_DOCUMENT)?
            .ok_or_else(PersonalWorkerStoreError::corrupt_state)?;
        if !matches!(
            prepared.phase(),
            ScaleSetDeliveryRecoveryPhase::SettlementPrepared { .. }
        ) {
            return Err(PersonalWorkerStoreError::corrupt_state());
        }
        let catalog = self
            .load_catalog_named(CATALOG_DOCUMENT)
            .map_err(map_catalog_error)?
            .ok_or_else(PersonalWorkerStoreError::corrupt_state)?;
        let staged_catalog = self
            .load_catalog_named(STAGED_CATALOG_DOCUMENT)
            .map_err(map_catalog_error)?;

        let target = if prepared.matches_catalog(&catalog) {
            let target = settle_scale_set_delivery_catalog(&prepared, &catalog)
                .map_err(|_| PersonalWorkerStoreError::corrupt_state())?;
            if !prepared.matches_settlement_catalog(&target) {
                return Err(PersonalWorkerStoreError::corrupt_state());
            }
            if target != catalog {
                match staged_catalog {
                    Some(ref staged) if staged == &target => {
                        self.synchronize_existing_catalog_stage(staged)
                            .map_err(map_catalog_error)?;
                    }
                    Some(_) => return Err(PersonalWorkerStoreError::corrupt_state()),
                    None => {
                        let mut staged = self.stage_catalog(&target).map_err(map_catalog_error)?;
                        synchronize_directory(
                            &self.directory,
                            "reconstructed Scale Set settlement catalog stage",
                        )?;
                        self.publish_named_staged(&mut staged, CATALOG_DOCUMENT, false)?;
                        return self.finish_recovered_settlement(prepared, target);
                    }
                }
                let mut guard =
                    StagedDocument::existing(self.directory.as_fd(), STAGED_CATALOG_DOCUMENT);
                self.publish_named_staged(&mut guard, CATALOG_DOCUMENT, false)?;
            } else if let Some(staged) = staged_catalog {
                if staged != catalog {
                    return Err(PersonalWorkerStoreError::corrupt_state());
                }
                self.remove_catalog_stage().map_err(map_catalog_error)?;
            }
            target
        } else if prepared.matches_settlement_catalog(&catalog) {
            let prior = prepared
                .settlement_prior_catalog()
                .ok_or_else(PersonalWorkerStoreError::corrupt_state)?;
            let derived = settle_scale_set_delivery_catalog(&prepared, prior)
                .map_err(|_| PersonalWorkerStoreError::corrupt_state())?;
            if derived != catalog || !prepared.matches_settlement_catalog(&derived) {
                return Err(PersonalWorkerStoreError::corrupt_state());
            }
            if let Some(staged) = staged_catalog {
                if staged != catalog {
                    return Err(PersonalWorkerStoreError::corrupt_state());
                }
                self.remove_catalog_stage().map_err(map_catalog_error)?;
            }
            catalog
        } else {
            return Err(PersonalWorkerStoreError::corrupt_state());
        };
        self.finish_recovered_settlement(prepared, target)
    }

    fn finish_recovered_settlement(
        &self,
        prepared: ScaleSetDeliveryRecoveryState,
        target: DisposableAttemptCatalogDocument,
    ) -> Result<Option<ScaleSetDeliverySettlement>, PersonalWorkerStoreError> {
        let acquired = prepared
            .settlement_acquired()
            .ok_or_else(PersonalWorkerStoreError::corrupt_state)?
            .len();
        self.remove_settled_delivery(&prepared)?;
        Ok(Some(ScaleSetDeliverySettlement {
            catalog: target,
            prepared,
            acquired,
        }))
    }

    fn remove_settled_delivery(
        &self,
        expected: &ScaleSetDeliveryRecoveryState,
    ) -> Result<(), PersonalWorkerStoreError> {
        let current = self
            .load_scale_set_delivery_named(DELIVERY_RECOVERY_DOCUMENT)?
            .ok_or_else(PersonalWorkerStoreError::corrupt_state)?;
        if current != *expected
            || !matches!(
                current.phase(),
                ScaleSetDeliveryRecoveryPhase::SettlementPrepared { .. }
            )
        {
            return Err(PersonalWorkerStoreError::corrupt_state());
        }
        fs::unlinkat(
            &self.directory,
            DELIVERY_RECOVERY_DOCUMENT,
            AtFlags::empty(),
        )
        .map_err(|_| {
            store_error(
                PersonalWorkerStoreErrorKind::Io,
                "could not remove settled Scale Set delivery",
            )
        })?;
        synchronize_directory(&self.directory, "settled Scale Set delivery removal")
    }
}

fn map_catalog_error(error: DisposableAttemptCatalogError) -> PersonalWorkerStoreError {
    let kind = match error.kind() {
        DisposableAttemptCatalogErrorKind::Busy => PersonalWorkerStoreErrorKind::Busy,
        DisposableAttemptCatalogErrorKind::Missing => PersonalWorkerStoreErrorKind::Missing,
        DisposableAttemptCatalogErrorKind::Io => PersonalWorkerStoreErrorKind::Io,
        DisposableAttemptCatalogErrorKind::UnsafeFilesystem => {
            PersonalWorkerStoreErrorKind::UnsafeFilesystem
        }
        DisposableAttemptCatalogErrorKind::VersionIncompatible => {
            PersonalWorkerStoreErrorKind::VersionIncompatible
        }
        DisposableAttemptCatalogErrorKind::CorruptState => {
            PersonalWorkerStoreErrorKind::CorruptState
        }
        DisposableAttemptCatalogErrorKind::AlreadyExists
        | DisposableAttemptCatalogErrorKind::Conflict
        | DisposableAttemptCatalogErrorKind::IdentityDrift
        | DisposableAttemptCatalogErrorKind::InvalidAction
        | DisposableAttemptCatalogErrorKind::LimitExceeded
        | DisposableAttemptCatalogErrorKind::RecoveryRequired => {
            PersonalWorkerStoreErrorKind::RevisionConflict
        }
    };
    store_error(kind, "disposable-attempt catalog is invalid")
}

#[cfg(test)]
mod tests;
