use super::*;

use crate::disposable_attempt_catalog::{
    DisposableAttemptCatalogDocument, DisposableAttemptCatalogError,
    DisposableAttemptCatalogErrorKind, DisposableAttemptCatalogRevision,
};
use crate::github_scale_set_delivery::ScaleSetDelivery;

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
    /// `catalog_successor` may be the exact current catalog (no catalog mutation) or one ordinary
    /// catalog successor. The initial delivery recovery state is always constructed here and bound
    /// to the resulting catalog revision. Both changed documents are staged and fsynced before the
    /// first rename. Network acknowledgement remains outside this transaction.
    pub(crate) fn publish_scale_set_reconciled_delivery(
        &mut self,
        expected_catalog_revision: DisposableAttemptCatalogRevision,
        catalog_successor: &DisposableAttemptCatalogDocument,
        delivery: &ScaleSetDelivery,
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
                || expected_catalog_revision
                    .get()
                    .checked_add(1)
                    .is_some_and(|revision| revision == current_catalog.revision().get());
            if expected_is_replay
                && current_delivery.catalog_revision() == current_catalog.revision()
                && current_delivery.delivery() == delivery
                && catalog_successor == &current_catalog
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

        let catalog_changes = catalog_successor != &current_catalog;
        if catalog_changes {
            catalog_successor
                .validate_successor_of(&current_catalog)
                .map_err(map_catalog_error)?;
        }
        let recovery = ScaleSetDeliveryRecoveryState::reconciled(
            delivery.clone(),
            catalog_successor.revision(),
        )
        .map_err(map_recovery_error)?;

        // The delivery is staged first. A crash before the catalog stage exists therefore leaves a
        // future catalog revision that recovery can identify as an uncommitted paired transaction.
        let mut staged_delivery = self.stage_scale_set_delivery(&recovery)?;
        if catalog_changes {
            let mut staged_catalog = self
                .stage_catalog(catalog_successor)
                .map_err(map_catalog_error)?;

            // Preserve the durable delivery stage across every catalog-publication error. If the
            // catalog rename never happened, recovery rolls this future stage back. If rename did
            // happen and only the directory sync failed, recovery sees the target catalog revision
            // current and completes the delivery publication.
            staged_delivery.disarm();
            self.publish_named_staged(&mut staged_catalog, CATALOG_DOCUMENT, false)?;
        }
        self.publish_named_staged(
            &mut staged_delivery,
            DELIVERY_RECOVERY_DOCUMENT,
            true,
        )?;
        Ok((catalog_successor.clone(), recovery))
    }

    fn recover_scale_set_reconcile_transaction_locked(
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
            let current_catalog = current_catalog.ok_or_else(PersonalWorkerStoreError::corrupt_state)?;
            if current_delivery.catalog_revision() != current_catalog.revision() {
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
            || !matches!(staged_delivery.phase(), ScaleSetDeliveryRecoveryPhase::Reconciled)
        {
            return Err(PersonalWorkerStoreError::corrupt_state());
        }
        let current_catalog = current_catalog.ok_or_else(PersonalWorkerStoreError::corrupt_state)?;

        if let Some(staged_catalog) = staged_catalog {
            let target_matches = staged_delivery.catalog_revision() == staged_catalog.revision();
            let catalog_stage_is_stale = staged_catalog == current_catalog;
            let catalog_stage_is_successor = staged_catalog
                .validate_successor_of(&current_catalog)
                .is_ok();
            if !target_matches || (!catalog_stage_is_stale && !catalog_stage_is_successor) {
                return Err(PersonalWorkerStoreError::corrupt_state());
            }

            // Re-establish durability for the delivery stage before allowing catalog recovery to
            // publish the first half of the pair.
            self.synchronize_existing_scale_set_delivery_stage(&staged_delivery)?;
            self.recover_catalog_locked().map_err(map_catalog_error)?;
            let mut delivery_guard = StagedDocument::existing(
                self.directory.as_fd(),
                STAGED_DELIVERY_RECOVERY_DOCUMENT,
            );
            self.publish_named_staged(
                &mut delivery_guard,
                DELIVERY_RECOVERY_DOCUMENT,
                true,
            )?;
            return Ok(());
        }

        if staged_delivery.catalog_revision() == current_catalog.revision() {
            // The catalog rename is already current (or this was an intentional zero-change
            // reconciliation). Complete the second half of the transaction.
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

        let future_catalog_revision = current_catalog.revision().get().checked_add(1);
        if future_catalog_revision == Some(staged_delivery.catalog_revision().get()) {
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
        DisposableAttemptCatalogErrorKind::CorruptState => PersonalWorkerStoreErrorKind::CorruptState,
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
mod tests {
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::disposable_attempt_catalog::{
        DisposableAttemptCatalog, DisposableAttemptReservation, MemoryDisposableAttemptCatalogStore,
        encode_disposable_attempt_catalog,
    };
    use crate::disposable_attempt_state::DisposableAttemptState;
    use crate::disposable_prepared_template::{
        DisposablePreparedTemplateIdentity, current_disposable_prepared_template,
    };
    use crate::disposable_worker_reconciler::{
        CapacityClaimId, DisposableAttemptId, DisposableVmId, DisposableWorkerResources,
    };
    use crate::execution_admission::EpochMillis;
    use crate::github_scale_set_bridge::{
        ScaleSetBridgeEvent, ScaleSetBridgeJobEvidence, ScaleSetBridgePoll, ScaleSetStatistics,
    };
    use crate::github_scale_set_protocol::{ScaleSetJobId, ScaleSetRunnerName};

    use super::super::super::publication_fault::{
        PublicationFaultPoint, inject_publication_fault,
    };
    use super::*;

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TempRoot {
        path: PathBuf,
        device: u64,
        inode: u64,
    }

    impl TempRoot {
        fn new(label: &str) -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "smolrunner-scale-set-reconcile-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create temporary state root");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o750))
                .expect("set temporary root mode");
            let metadata = fs::symlink_metadata(&path).expect("inspect temporary state root");
            Self {
                path,
                device: metadata.dev(),
                inode: metadata.ino(),
            }
        }

        fn path(&self) -> &Path {
            &self.path
        }

        fn store_directory(&self) -> PathBuf {
            self.path.join(STORE_DIRECTORY)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let Ok(metadata) = fs::symlink_metadata(&self.path) else {
                return;
            };
            if metadata.file_type().is_dir()
                && !metadata.file_type().is_symlink()
                && metadata.dev() == self.device
                && metadata.ino() == self.inode
            {
                let _ = fs::remove_dir_all(&self.path);
            }
        }
    }

    fn initialize_catalog(root: &TempRoot) -> DisposableAttemptCatalogDocument {
        let store = UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path())
            .expect("open disposable catalog");
        let mut catalog = DisposableAttemptCatalog::new(store);
        catalog.initialize().expect("initialize catalog").0
    }

    fn successor(index: usize) -> DisposableAttemptCatalogDocument {
        let mut catalog =
            DisposableAttemptCatalog::new(MemoryDisposableAttemptCatalogStore::default());
        let (empty, _) = catalog.initialize().expect("initialize memory catalog");
        catalog
            .reserve(empty.revision(), reservation(index))
            .expect("reserve successor")
            .0
    }

    fn reservation(index: usize) -> DisposableAttemptReservation {
        DisposableAttemptReservation::new(
            DisposableAttemptState::reserved(
                DisposableAttemptId::parse(&format!("attempt-{index}")).expect("attempt id"),
                CapacityClaimId::parse(&format!("claim-{index}")).expect("claim id"),
                DisposableVmId::parse(&format!("vm-{index}")).expect("vm id"),
                ScaleSetRunnerName::parse(&format!("smol-attempt-{index}")).expect("runner name"),
                EpochMillis::new(100_000 + u64::try_from(index).expect("bounded index"))
                    .expect("expiry"),
            ),
            DisposableWorkerResources::new(1_000, 2_000, 3_000).expect("resources"),
            prepared_template_identity(),
        )
        .expect("reservation")
    }

    fn prepared_template_identity() -> DisposablePreparedTemplateIdentity {
        current_disposable_prepared_template()
            .expect("current prepared template")
            .identity()
            .expect("prepared template identity")
    }

    fn delivery(message_id: u32, request_id: u64) -> ScaleSetDelivery {
        ScaleSetDelivery::from_bridge_poll(&ScaleSetBridgePoll::Message {
            message_id,
            statistics: ScaleSetStatistics {
                available_jobs: 1,
                acquired_jobs: 0,
                assigned_jobs: 0,
                running_jobs: 0,
                registered_runners: 0,
                busy_runners: 0,
                idle_runners: 0,
            },
            events: vec![ScaleSetBridgeEvent::Available(ScaleSetBridgeJobEvidence {
                runner_request_id: request_id,
                repository: "project".to_owned(),
                owner: "example".to_owned(),
                job_id: ScaleSetJobId::parse(&format!("job-{request_id}")).expect("job id"),
                workflow_run_id: 99,
                request_labels: vec!["smolrunner".to_owned()],
            })],
        })
        .expect("canonical delivery")
        .expect("message delivery")
    }

    fn write_private(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).expect("write private fixture");
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .expect("set private fixture mode");
    }

    #[test]
    fn publishes_catalog_and_delivery_under_one_recoverable_transaction() {
        let root = TempRoot::new("publish");
        let empty = initialize_catalog(&root);
        let next = successor(1);
        let delivery = delivery(7, 41);

        let mut store = UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(
            root.path(),
        )
        .expect("open reconcile transaction");
        let (written_catalog, recovery) = store
            .publish_scale_set_reconciled_delivery(empty.revision(), &next, &delivery)
            .expect("publish paired reconciliation");
        assert_eq!(written_catalog, next);
        assert_eq!(recovery.catalog_revision(), next.revision());
        assert_eq!(recovery.delivery(), &delivery);
        drop(store);

        let reopened = UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(
            root.path(),
        )
        .expect("reopen paired transaction");
        assert_eq!(
            reopened
                .load_catalog_named(CATALOG_DOCUMENT)
                .expect("load catalog"),
            Some(next)
        );
        assert_eq!(
            reopened
                .load_scale_set_delivery_recovery()
                .expect("load delivery recovery"),
            Some(recovery)
        );
    }

    #[test]
    fn delivery_only_future_stage_rolls_back_without_advancing_catalog() {
        let root = TempRoot::new("delivery-only-future");
        let empty = initialize_catalog(&root);
        let next = successor(1);
        let recovery = ScaleSetDeliveryRecoveryState::reconciled(delivery(8, 42), next.revision())
            .expect("recovery state");
        let store = UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(
            root.path(),
        )
        .expect("open reconcile transaction");
        let mut staged = store
            .stage_scale_set_delivery(&recovery)
            .expect("stage delivery");
        staged.disarm();
        drop(store);

        let reopened = UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(
            root.path(),
        )
        .expect("recover future delivery stage");
        assert_eq!(
            reopened
                .load_catalog_named(CATALOG_DOCUMENT)
                .expect("load catalog"),
            Some(empty)
        );
        assert_eq!(
            reopened
                .load_scale_set_delivery_recovery()
                .expect("load recovery"),
            None
        );
        assert!(!root
            .store_directory()
            .join(STAGED_DELIVERY_RECOVERY_DOCUMENT)
            .exists());
    }

    #[test]
    fn both_exact_stages_recover_catalog_then_delivery() {
        let root = TempRoot::new("both-stages");
        initialize_catalog(&root);
        let next = successor(1);
        let recovery = ScaleSetDeliveryRecoveryState::reconciled(delivery(9, 43), next.revision())
            .expect("recovery state");
        let store = UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(
            root.path(),
        )
        .expect("open reconcile transaction");
        let mut staged_delivery = store
            .stage_scale_set_delivery(&recovery)
            .expect("stage delivery");
        staged_delivery.disarm();
        let mut staged_catalog = store.stage_catalog(&next).expect("stage catalog");
        staged_catalog.disarm();
        drop(store);

        let reopened = UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(
            root.path(),
        )
        .expect("recover paired stages");
        assert_eq!(
            reopened
                .load_catalog_named(CATALOG_DOCUMENT)
                .expect("load catalog"),
            Some(next)
        );
        assert_eq!(
            reopened
                .load_scale_set_delivery_recovery()
                .expect("load recovery"),
            Some(recovery)
        );
    }

    #[test]
    fn catalog_only_stage_keeps_ordinary_catalog_recovery_semantics() {
        let root = TempRoot::new("catalog-only");
        initialize_catalog(&root);
        let next = successor(1);
        let store = UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(
            root.path(),
        )
        .expect("open reconcile transaction");
        let mut staged_catalog = store.stage_catalog(&next).expect("stage catalog");
        staged_catalog.disarm();
        drop(store);

        let reopened = UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(
            root.path(),
        )
        .expect("recover ordinary catalog stage");
        assert_eq!(
            reopened
                .load_catalog_named(CATALOG_DOCUMENT)
                .expect("load catalog"),
            Some(next)
        );
        assert_eq!(
            reopened
                .load_scale_set_delivery_recovery()
                .expect("load recovery"),
            None
        );
    }

    #[test]
    fn rename_failure_retains_delivery_marker_then_recovery_rolls_it_back() {
        let root = TempRoot::new("rename-fault");
        let empty = initialize_catalog(&root);
        let next = successor(1);
        let delivery = delivery(10, 44);
        let mut store = UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(
            root.path(),
        )
        .expect("open reconcile transaction");
        let fault = inject_publication_fault(PublicationFaultPoint::PublishRename);
        let error = store
            .publish_scale_set_reconciled_delivery(empty.revision(), &next, &delivery)
            .expect_err("catalog rename fault");
        assert_eq!(error.kind(), PersonalWorkerStoreErrorKind::Io);
        drop(fault);
        assert!(root
            .store_directory()
            .join(STAGED_DELIVERY_RECOVERY_DOCUMENT)
            .exists());
        assert!(!root.store_directory().join(STAGED_CATALOG_DOCUMENT).exists());
        drop(store);

        let reopened = UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(
            root.path(),
        )
        .expect("recover rename failure");
        assert_eq!(
            reopened
                .load_catalog_named(CATALOG_DOCUMENT)
                .expect("load catalog"),
            Some(empty)
        );
        assert_eq!(
            reopened
                .load_scale_set_delivery_recovery()
                .expect("load recovery"),
            None
        );
    }

    #[test]
    fn directory_sync_failure_after_catalog_rename_completes_delivery_on_reopen() {
        let root = TempRoot::new("directory-sync-fault");
        let empty = initialize_catalog(&root);
        let next = successor(1);
        let delivery = delivery(11, 45);
        let mut store = UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(
            root.path(),
        )
        .expect("open reconcile transaction");
        let fault = inject_publication_fault(PublicationFaultPoint::PublicationDirectorySync);
        let error = store
            .publish_scale_set_reconciled_delivery(empty.revision(), &next, &delivery)
            .expect_err("catalog directory sync fault");
        assert_eq!(error.kind(), PersonalWorkerStoreErrorKind::Io);
        drop(fault);
        assert_eq!(
            store
                .load_catalog_named(CATALOG_DOCUMENT)
                .expect("load visible catalog"),
            Some(next.clone())
        );
        assert!(root
            .store_directory()
            .join(STAGED_DELIVERY_RECOVERY_DOCUMENT)
            .exists());
        drop(store);

        let reopened = UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(
            root.path(),
        )
        .expect("recover directory sync ambiguity");
        let recovered = reopened
            .load_scale_set_delivery_recovery()
            .expect("load recovered delivery")
            .expect("recovered delivery exists");
        assert_eq!(recovered.catalog_revision(), next.revision());
        assert_eq!(recovered.delivery(), &delivery);
    }

    #[test]
    fn zero_change_reconciliation_and_exact_replay_are_idempotent() {
        let root = TempRoot::new("zero-change");
        let empty = initialize_catalog(&root);
        let delivery = delivery(12, 46);
        let mut store = UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(
            root.path(),
        )
        .expect("open reconcile transaction");
        let (_, initial) = store
            .publish_scale_set_reconciled_delivery(empty.revision(), &empty, &delivery)
            .expect("publish zero-change reconciliation");
        let (same_catalog, same_recovery) = store
            .publish_scale_set_reconciled_delivery(empty.revision(), &empty, &delivery)
            .expect("replay exact reconciliation");
        assert_eq!(same_catalog, empty);
        assert_eq!(same_recovery, initial);
    }

    #[test]
    fn foreign_live_delivery_blocks_another_reconciliation() {
        let root = TempRoot::new("live-conflict");
        let empty = initialize_catalog(&root);
        let first = delivery(13, 47);
        let second = delivery(14, 48);
        let mut store = UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(
            root.path(),
        )
        .expect("open reconcile transaction");
        store
            .publish_scale_set_reconciled_delivery(empty.revision(), &empty, &first)
            .expect("publish first delivery");
        let error = store
            .publish_scale_set_reconciled_delivery(empty.revision(), &empty, &second)
            .expect_err("foreign live delivery must block");
        assert_eq!(error.kind(), PersonalWorkerStoreErrorKind::RevisionConflict);
    }

    #[test]
    fn malformed_delivery_only_stage_is_preserved_as_corruption() {
        let root = TempRoot::new("malformed-stage");
        initialize_catalog(&root);
        let store = UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(
            root.path(),
        )
        .expect("open reconcile transaction");
        drop(store);
        write_private(
            &root
                .store_directory()
                .join(STAGED_DELIVERY_RECOVERY_DOCUMENT),
            b"not canonical JSON\n",
        );
        assert_eq!(
            UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
                .expect_err("malformed stage must fail closed")
                .kind(),
            PersonalWorkerStoreErrorKind::CorruptState
        );
        assert!(root
            .store_directory()
            .join(STAGED_DELIVERY_RECOVERY_DOCUMENT)
            .exists());
    }

    #[test]
    fn encoded_catalog_stage_fixture_remains_canonical() {
        let root = TempRoot::new("catalog-fixture");
        initialize_catalog(&root);
        let next = successor(1);
        write_private(
            &root.store_directory().join(STAGED_CATALOG_DOCUMENT),
            &encode_disposable_attempt_catalog(&next).expect("encode catalog"),
        );
        let reopened = UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(
            root.path(),
        )
        .expect("recover encoded catalog stage");
        assert_eq!(
            reopened
                .load_catalog_named(CATALOG_DOCUMENT)
                .expect("load catalog"),
            Some(next)
        );
    }
}
