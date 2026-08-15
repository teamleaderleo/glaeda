use super::*;

use crate::disposable_attempt_catalog::{
    DisposableAttemptCatalogDocument, DisposableAttemptCatalogError,
    DisposableAttemptCatalogErrorKind, DisposableAttemptCatalogRevision,
};
use crate::execution_admission::EpochMillis;
use crate::github_scale_set_delivery::ScaleSetDelivery;
use crate::github_scale_set_delivery_consumer::{
    ScaleSetDeliveryConsumerPolicy, reconcile_scale_set_delivery,
};

use super::super::disposable_attempt_catalog::{CATALOG_DOCUMENT, STAGED_CATALOG_DOCUMENT};

impl UnixPersonalWorkerStore {
    /// Open the shared private store and recover a paired catalog/delivery reconciliation.
    ///
    /// The paired transaction stages the delivery first and the catalog second, then publishes the
    /// catalog before the delivery. That ordering leaves every crash point distinguishable:
    /// delivery-only future state rolls back, both stages complete in order, and delivery-only
    /// state bound to the current catalog completes the second publication.
    pub(crate) fn open_or_create_scale_set_reconcile_transaction(
        root_path: impl AsRef<Path>,
    ) -> Result<Self, PersonalWorkerStoreError> {
        let root = fs::open(root_path.as_ref(), DIRECTORY_FLAGS, Mode::empty())
            .map_err(map_root_open_error)?;
        let root_stat = inspect_directory(&root, "Scale Set reconcile state root", None)?;
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
        synchronize_directory(&store._root, "Scale Set reconcile state root")?;
        synchronize_directory(&store.directory, "personal worker store directory")?;
        store.recover_scale_set_reconcile_transaction_locked()?;
        Ok(store)
    }

    /// Publish one bounded durable reconciliation under the canonical writer lock.
    ///
    /// The exact successor is derived from every retained delivery event while the canonical lock
    /// is held, so one message may advance several catalog revisions. The recovery state binds the
    /// canonical bytes of both the prior and resulting catalogs. Both changed documents are staged
    /// and fsynced before the first rename. Network acknowledgement remains outside this
    /// transaction.
    pub(crate) fn publish_scale_set_reconciled_delivery(
        &mut self,
        expected_catalog_revision: DisposableAttemptCatalogRevision,
        policy: &ScaleSetDeliveryConsumerPolicy,
        delivery: &ScaleSetDelivery,
        observed_at: EpochMillis,
    ) -> Result<
        (
            DisposableAttemptCatalogDocument,
            ScaleSetDeliveryRecoveryState,
        ),
        PersonalWorkerStoreError,
    > {
        let _lock = self.acquire_mutation_lock()?;
        synchronize_directory(&self.directory, "personal worker store directory")?;
        self.recover_scale_set_reconcile_transaction_locked()?;

        let current_catalog = self
            .load_catalog_named(CATALOG_DOCUMENT)
            .map_err(map_catalog_error)?
            .ok_or_else(|| {
                store_error(
                    PersonalWorkerStoreErrorKind::Missing,
                    "disposable-attempt catalog is not initialized",
                )
            })?;

        if let Some(current_delivery) =
            self.load_scale_set_delivery_named(DELIVERY_RECOVERY_DOCUMENT)?
        {
            let expected_is_replay = expected_catalog_revision == current_catalog.revision()
                || expected_catalog_revision == current_delivery.prior_catalog_revision();
            if expected_is_replay
                && current_delivery.matches_catalog(&current_catalog)
                && current_delivery.delivery() == delivery
            {
                return Ok((current_catalog, current_delivery));
            }
            return Err(store_error(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "a different Scale Set delivery remains unsettled",
            ));
        }

        if current_catalog.revision() != expected_catalog_revision {
            return Err(store_error(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "disposable-attempt catalog revision changed before Scale Set reconciliation",
            ));
        }

        let catalog_successor =
            reconcile_scale_set_delivery(policy, delivery, &current_catalog, observed_at).map_err(
                |_| {
                    store_error(
                        PersonalWorkerStoreErrorKind::RevisionConflict,
                        "Scale Set delivery conflicts with the disposable-attempt catalog",
                    )
                },
            )?;
        let catalog_changes = catalog_successor != current_catalog;
        let recovery = ScaleSetDeliveryRecoveryState::reconciled(
            delivery.clone(),
            &current_catalog,
            &catalog_successor,
        )
        .map_err(map_recovery_error)?;

        // The delivery is staged first. A crash before the catalog stage exists therefore leaves a
        // future catalog revision that recovery can identify as an uncommitted paired transaction.
        let mut staged_delivery = self.stage_scale_set_delivery(&recovery)?;
        if catalog_changes {
            // Make the delivery stage name durable before a catalog stage can exist. This prevents
            // a power loss from exposing a catalog-only stage created by this transaction, which
            // would otherwise be indistinguishable from ordinary catalog recovery.
            synchronize_directory(
                &self.directory,
                "paired Scale Set reconciliation delivery stage",
            )?;
            // From this point onward the durable delivery marker carries recovery intent across
            // every catalog-stage or catalog-publication error.
            staged_delivery.disarm();
            let mut staged_catalog = self
                .stage_catalog(&catalog_successor)
                .map_err(map_catalog_error)?;
            // Persist both stage names before publishing either current document.
            synchronize_directory(&self.directory, "paired Scale Set reconciliation stages")?;
            self.publish_named_staged(&mut staged_catalog, CATALOG_DOCUMENT, false)?;
        }
        self.publish_named_staged(&mut staged_delivery, DELIVERY_RECOVERY_DOCUMENT, true)?;
        Ok((catalog_successor, recovery))
    }

    pub(super) fn recover_scale_set_reconcile_transaction_locked(
        &mut self,
    ) -> Result<(), PersonalWorkerStoreError> {
        self.refuse_unsettled_scale_set_neighbors()?;

        let current_catalog = self
            .load_catalog_named(CATALOG_DOCUMENT)
            .map_err(map_catalog_error)?;
        let staged_catalog = self
            .load_catalog_named(STAGED_CATALOG_DOCUMENT)
            .map_err(map_catalog_error)?;
        let current_delivery = self.load_scale_set_delivery_named(DELIVERY_RECOVERY_DOCUMENT)?;
        let staged_delivery =
            self.load_scale_set_delivery_named(STAGED_DELIVERY_RECOVERY_DOCUMENT)?;

        if let Some(current_delivery) = current_delivery {
            let current_catalog =
                current_catalog.ok_or_else(PersonalWorkerStoreError::corrupt_state)?;
            if !current_delivery.matches_catalog(&current_catalog) {
                return Err(PersonalWorkerStoreError::corrupt_state());
            }
            if staged_catalog.is_some() {
                return Err(store_error(
                    PersonalWorkerStoreErrorKind::RevisionConflict,
                    "disposable-attempt recovery conflicts with a live Scale Set delivery",
                ));
            }
            if staged_delivery.is_some() {
                self.recover_scale_set_delivery_locked()?;
            }
            return Ok(());
        }

        let Some(staged_delivery) = staged_delivery else {
            if staged_catalog.is_some() {
                self.recover_catalog_locked().map_err(map_catalog_error)?;
            }
            return Ok(());
        };
        if staged_delivery.revision() != 1
            || !matches!(
                staged_delivery.phase(),
                ScaleSetDeliveryRecoveryPhase::Reconciled
            )
        {
            return Err(PersonalWorkerStoreError::corrupt_state());
        }
        let current_catalog =
            current_catalog.ok_or_else(PersonalWorkerStoreError::corrupt_state)?;

        if let Some(staged_catalog) = staged_catalog {
            if staged_delivery.matches_catalog(&current_catalog)
                && staged_catalog == current_catalog
            {
                self.remove_catalog_stage().map_err(map_catalog_error)?;
                self.synchronize_existing_scale_set_delivery_stage(&staged_delivery)?;
                let mut delivery_guard = StagedDocument::existing(
                    self.directory.as_fd(),
                    STAGED_DELIVERY_RECOVERY_DOCUMENT,
                );
                return self.publish_named_staged(
                    &mut delivery_guard,
                    DELIVERY_RECOVERY_DOCUMENT,
                    true,
                );
            }
            if !staged_delivery.matches_prior_catalog(&current_catalog)
                || !staged_delivery.matches_catalog(&staged_catalog)
            {
                return Err(PersonalWorkerStoreError::corrupt_state());
            }

            // Re-establish durability for the delivery stage before allowing catalog recovery to
            // publish the first half of the pair.
            self.synchronize_existing_scale_set_delivery_stage(&staged_delivery)?;
            self.synchronize_existing_catalog_stage(&staged_catalog)
                .map_err(map_catalog_error)?;
            let mut catalog_guard =
                StagedDocument::existing(self.directory.as_fd(), STAGED_CATALOG_DOCUMENT);
            self.publish_named_staged(&mut catalog_guard, CATALOG_DOCUMENT, false)?;
            let mut delivery_guard =
                StagedDocument::existing(self.directory.as_fd(), STAGED_DELIVERY_RECOVERY_DOCUMENT);
            self.publish_named_staged(&mut delivery_guard, DELIVERY_RECOVERY_DOCUMENT, true)?;
            return Ok(());
        }

        if staged_delivery.matches_catalog(&current_catalog) {
            // The catalog rename is already current (or this was an intentional zero-change
            // reconciliation). Complete the second half of the transaction.
            self.synchronize_existing_scale_set_delivery_stage(&staged_delivery)?;
            let mut delivery_guard =
                StagedDocument::existing(self.directory.as_fd(), STAGED_DELIVERY_RECOVERY_DOCUMENT);
            return self.publish_named_staged(
                &mut delivery_guard,
                DELIVERY_RECOVERY_DOCUMENT,
                true,
            );
        }

        if staged_delivery.matches_prior_catalog(&current_catalog) {
            // Delivery-first staging crashed before the catalog stage became durable. No catalog
            // authority changed, so remove only this uncommitted transaction marker.
            return remove_uncommitted_delivery_stage(self);
        }

        Err(PersonalWorkerStoreError::corrupt_state())
    }

    fn refuse_unsettled_scale_set_neighbors(&self) -> Result<(), PersonalWorkerStoreError> {
        match self.recovery_plan()? {
            StoreRecoveryPlan::Clean { .. } => {}
            StoreRecoveryPlan::PublishStaged { .. }
            | StoreRecoveryPlan::RemoveStaleStaged { .. } => {
                return Err(store_error(
                    PersonalWorkerStoreErrorKind::RevisionConflict,
                    "personal-worker recovery must complete before Scale Set reconciliation",
                ));
            }
        }
        super::super::disposable_template_generation::refuse_unsettled(self)?;
        super::super::lima_authority::refuse_unsettled_lima_authority(self)?;
        Ok(())
    }
}

fn remove_uncommitted_delivery_stage(
    store: &UnixPersonalWorkerStore,
) -> Result<(), PersonalWorkerStoreError> {
    match fs::unlinkat(
        &store.directory,
        STAGED_DELIVERY_RECOVERY_DOCUMENT,
        AtFlags::empty(),
    ) {
        Ok(()) => synchronize_directory(&store.directory, "personal worker store directory"),
        Err(Errno::NOENT) => Ok(()),
        Err(_) => Err(store_error(
            PersonalWorkerStoreErrorKind::Io,
            "could not remove uncommitted Scale Set delivery stage",
        )),
    }
}

fn map_catalog_error(error: DisposableAttemptCatalogError) -> PersonalWorkerStoreError {
    let kind = match error.kind() {
        DisposableAttemptCatalogErrorKind::CorruptState => {
            PersonalWorkerStoreErrorKind::CorruptState
        }
        DisposableAttemptCatalogErrorKind::Busy => PersonalWorkerStoreErrorKind::Busy,
        DisposableAttemptCatalogErrorKind::Missing => PersonalWorkerStoreErrorKind::Missing,
        DisposableAttemptCatalogErrorKind::Io => PersonalWorkerStoreErrorKind::Io,
        DisposableAttemptCatalogErrorKind::UnsafeFilesystem => {
            PersonalWorkerStoreErrorKind::UnsafeFilesystem
        }
        DisposableAttemptCatalogErrorKind::VersionIncompatible => {
            PersonalWorkerStoreErrorKind::VersionIncompatible
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
    PersonalWorkerStoreError::new(kind, error.message())
}

#[cfg(test)]
mod tests;
