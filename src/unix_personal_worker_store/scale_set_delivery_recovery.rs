// This store is consumed only through the bounded #429 controller and its recovery transactions.
#![allow(dead_code)]

use super::*;

use crate::github_scale_set_delivery_state::{
    MAX_SCALE_SET_DELIVERY_RECOVERY_BYTES, ScaleSetDeliveryRecoveryError,
    ScaleSetDeliveryRecoveryErrorKind, ScaleSetDeliveryRecoveryPhase,
    ScaleSetDeliveryRecoveryState, decode_scale_set_delivery_recovery,
    encode_scale_set_delivery_recovery,
};

mod controller_transaction;
mod reconcile_transaction;
mod settlement_transaction;

pub(crate) use controller_transaction::ScaleSetExternalTransaction;

pub(super) const DELIVERY_RECOVERY_DOCUMENT: &str = "scale-set-delivery-recovery.json";
pub(super) const STAGED_DELIVERY_RECOVERY_DOCUMENT: &str = ".scale-set-delivery-recovery.next.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryPlan {
    Clean,
    PublishStaged { no_replace: bool },
    RemoveStaleStaged,
}

impl UnixPersonalWorkerStore {
    /// Open or create the shared private store and recover Scale Set delivery state.
    ///
    /// Other cooperating mutators refuse a staged or live delivery. The later consumer transaction
    /// will define the exact acknowledgement/acquisition proof that permits retirement.
    pub(crate) fn open_or_create_scale_set_delivery_recovery(
        root_path: impl AsRef<Path>,
    ) -> Result<Self, PersonalWorkerStoreError> {
        let root = fs::open(root_path.as_ref(), DIRECTORY_FLAGS, Mode::empty())
            .map_err(map_root_open_error)?;
        let root_stat = inspect_directory(&root, "Scale Set delivery state root", None)?;
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
        synchronize_directory(&store._root, "Scale Set delivery state root")?;
        synchronize_directory(&store.directory, "personal worker store directory")?;
        store.refuse_other_unsettled_scale_set_state()?;
        store.recover_scale_set_delivery_locked()?;
        Ok(store)
    }

    /// Read the current exact recovery state without promoting a staged publication.
    pub(crate) fn load_scale_set_delivery_recovery(
        &self,
    ) -> Result<Option<ScaleSetDeliveryRecoveryState>, PersonalWorkerStoreError> {
        let _lock = self.acquire_read_lock()?;
        if self
            .read_named_bytes_bounded(
                STAGED_DELIVERY_RECOVERY_DOCUMENT,
                MAX_SCALE_SET_DELIVERY_RECOVERY_BYTES,
            )?
            .is_some()
        {
            return Err(store_error(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "Scale Set delivery state requires recovery",
            ));
        }
        self.load_scale_set_delivery_named(DELIVERY_RECOVERY_DOCUMENT)
    }

    /// Publish the first exact `reconciled` recovery document.
    pub(crate) fn create_scale_set_delivery_recovery(
        &mut self,
        state: &ScaleSetDeliveryRecoveryState,
    ) -> Result<ScaleSetDeliveryRecoveryState, PersonalWorkerStoreError> {
        if state.revision() != 1
            || !matches!(state.phase(), ScaleSetDeliveryRecoveryPhase::Reconciled)
        {
            return Err(store_error(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "initial Scale Set delivery recovery state is invalid",
            ));
        }
        let _lock = self.acquire_mutation_lock()?;
        synchronize_directory(&self.directory, "personal worker store directory")?;
        self.refuse_other_unsettled_scale_set_state()?;
        self.recover_scale_set_delivery_locked()?;
        if self
            .load_scale_set_delivery_named(DELIVERY_RECOVERY_DOCUMENT)?
            .is_some()
        {
            return Err(store_error(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "Scale Set delivery recovery state already exists",
            ));
        }
        let mut staged = self.stage_scale_set_delivery(state)?;
        self.publish_named_staged(&mut staged, DELIVERY_RECOVERY_DOCUMENT, true)?;
        Ok(state.clone())
    }

    /// Publish one exact pure successor from the currently durable recovery state.
    pub(crate) fn replace_scale_set_delivery_recovery(
        &mut self,
        expected_revision: u64,
        successor: &ScaleSetDeliveryRecoveryState,
    ) -> Result<ScaleSetDeliveryRecoveryState, PersonalWorkerStoreError> {
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
        if current.revision() != expected_revision || !exact_recovery_successor(&current, successor)
        {
            return Err(store_error(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "Scale Set delivery recovery successor no longer matches durable state",
            ));
        }
        let mut staged = self.stage_scale_set_delivery(successor)?;
        self.publish_named_staged(&mut staged, DELIVERY_RECOVERY_DOCUMENT, false)?;
        Ok(successor.clone())
    }

    fn refuse_other_unsettled_scale_set_state(&self) -> Result<(), PersonalWorkerStoreError> {
        match self.recovery_plan()? {
            StoreRecoveryPlan::Clean { .. } => {}
            StoreRecoveryPlan::PublishStaged { .. }
            | StoreRecoveryPlan::RemoveStaleStaged { .. } => {
                return Err(store_error(
                    PersonalWorkerStoreErrorKind::RevisionConflict,
                    "personal-worker recovery must complete before Scale Set delivery mutation",
                ));
            }
        }
        disposable_attempt_catalog::refuse_unsettled(self)?;
        disposable_template_generation::refuse_unsettled(self)?;
        lima_authority::refuse_unsettled_lima_authority(self)?;
        Ok(())
    }

    fn load_scale_set_delivery_named(
        &self,
        name: &str,
    ) -> Result<Option<ScaleSetDeliveryRecoveryState>, PersonalWorkerStoreError> {
        self.read_named_bytes_bounded(name, MAX_SCALE_SET_DELIVERY_RECOVERY_BYTES)?
            .map(|bytes| decode_scale_set_delivery_recovery(&bytes).map_err(map_recovery_error))
            .transpose()
    }

    fn stage_scale_set_delivery(
        &self,
        state: &ScaleSetDeliveryRecoveryState,
    ) -> Result<StagedDocument<'_>, PersonalWorkerStoreError> {
        let bytes = encode_scale_set_delivery_recovery(state).map_err(map_recovery_error)?;
        self.stage_named_bytes(STAGED_DELIVERY_RECOVERY_DOCUMENT, &bytes)
    }

    fn scale_set_delivery_recovery_plan(&self) -> Result<RecoveryPlan, PersonalWorkerStoreError> {
        let Some(staged) = self.load_scale_set_delivery_named(STAGED_DELIVERY_RECOVERY_DOCUMENT)?
        else {
            return Ok(RecoveryPlan::Clean);
        };
        let current = self.load_scale_set_delivery_named(DELIVERY_RECOVERY_DOCUMENT)?;
        match current {
            None if staged.revision() == 1
                && matches!(staged.phase(), ScaleSetDeliveryRecoveryPhase::Reconciled) =>
            {
                Ok(RecoveryPlan::PublishStaged { no_replace: true })
            }
            None => Err(PersonalWorkerStoreError::corrupt_state()),
            Some(current) if staged == current => Ok(RecoveryPlan::RemoveStaleStaged),
            Some(current) if exact_recovery_successor(&current, &staged) => {
                Ok(RecoveryPlan::PublishStaged { no_replace: false })
            }
            Some(_) => Err(PersonalWorkerStoreError::corrupt_state()),
        }
    }

    fn synchronize_existing_scale_set_delivery_stage(
        &self,
        expected: &ScaleSetDeliveryRecoveryState,
    ) -> Result<(), PersonalWorkerStoreError> {
        let file = fs::openat(
            &self.directory,
            STAGED_DELIVERY_RECOVERY_DOCUMENT,
            EXISTING_FILE_FLAGS,
            Mode::empty(),
        )
        .map_err(map_document_open_error)?;
        inspect_private_file(
            &file,
            self.owner,
            "staged Scale Set delivery recovery state",
            None,
        )?;
        let mut file = File::from(file);
        let mut bytes = Vec::new();
        std::io::Read::by_ref(&mut file)
            .take((MAX_SCALE_SET_DELIVERY_RECOVERY_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| {
                store_error(
                    PersonalWorkerStoreErrorKind::Io,
                    "could not read staged Scale Set delivery recovery state",
                )
            })?;
        if bytes.len() > MAX_SCALE_SET_DELIVERY_RECOVERY_BYTES
            || decode_scale_set_delivery_recovery(&bytes).as_ref() != Ok(expected)
        {
            return Err(PersonalWorkerStoreError::corrupt_state());
        }
        file.sync_all().map_err(|_| {
            store_error(
                PersonalWorkerStoreErrorKind::Io,
                "could not synchronize staged Scale Set delivery recovery state",
            )
        })?;
        inspect_private_file(
            file.as_fd(),
            self.owner,
            "staged Scale Set delivery recovery state",
            Some(bytes.len()),
        )?;
        Ok(())
    }

    fn recover_scale_set_delivery_locked(&mut self) -> Result<(), PersonalWorkerStoreError> {
        match self.scale_set_delivery_recovery_plan()? {
            RecoveryPlan::Clean => Ok(()),
            RecoveryPlan::PublishStaged { no_replace } => {
                let staged = self
                    .load_scale_set_delivery_named(STAGED_DELIVERY_RECOVERY_DOCUMENT)?
                    .ok_or_else(PersonalWorkerStoreError::corrupt_state)?;
                self.synchronize_existing_scale_set_delivery_stage(&staged)?;
                let mut guard = StagedDocument::existing(
                    self.directory.as_fd(),
                    STAGED_DELIVERY_RECOVERY_DOCUMENT,
                );
                self.publish_named_staged(&mut guard, DELIVERY_RECOVERY_DOCUMENT, no_replace)
            }
            RecoveryPlan::RemoveStaleStaged => match fs::unlinkat(
                &self.directory,
                STAGED_DELIVERY_RECOVERY_DOCUMENT,
                AtFlags::empty(),
            ) {
                Ok(()) => synchronize_directory(&self.directory, "personal worker store directory"),
                Err(Errno::NOENT) => Ok(()),
                Err(_) => Err(store_error(
                    PersonalWorkerStoreErrorKind::Io,
                    "could not remove stale Scale Set delivery recovery state",
                )),
            },
        }
    }
}

/// Refuse unrelated store recovery or mutation while one delivery transaction is unsettled.
///
/// Both the staged marker and current document bind the catalog revision that the consumer must
/// preserve through acknowledgement/acquisition recovery. Only the paired delivery transaction may
/// classify or advance them.
pub(super) fn refuse_unsettled(
    store: &UnixPersonalWorkerStore,
) -> Result<(), PersonalWorkerStoreError> {
    // Close any prior rename/directory-fsync ambiguity before treating absence as authoritative.
    synchronize_directory(&store.directory, "personal worker store directory")?;
    let staged = store.read_named_bytes_bounded(
        STAGED_DELIVERY_RECOVERY_DOCUMENT,
        MAX_SCALE_SET_DELIVERY_RECOVERY_BYTES,
    )?;
    let current = store.read_named_bytes_bounded(
        DELIVERY_RECOVERY_DOCUMENT,
        MAX_SCALE_SET_DELIVERY_RECOVERY_BYTES,
    )?;
    if staged.is_some() || current.is_some() {
        return Err(store_error(
            PersonalWorkerStoreErrorKind::RevisionConflict,
            "Scale Set delivery reconciliation must settle before unrelated mutation",
        ));
    }
    Ok(())
}

fn exact_recovery_successor(
    current: &ScaleSetDeliveryRecoveryState,
    candidate: &ScaleSetDeliveryRecoveryState,
) -> bool {
    if candidate.revision() != current.revision().saturating_add(1)
        || candidate.catalog_revision() != current.catalog_revision()
        || candidate.delivery() != current.delivery()
    {
        return false;
    }
    let expected = match candidate.phase() {
        ScaleSetDeliveryRecoveryPhase::AcknowledgementStarted => current.begin_ack(),
        ScaleSetDeliveryRecoveryPhase::Acknowledged { acquired } => {
            current.record_ack_response(acquired)
        }
        ScaleSetDeliveryRecoveryPhase::AcquisitionRecoveryObserved { acquired } => {
            current.record_recovery_acquire(acquired)
        }
        ScaleSetDeliveryRecoveryPhase::LifecycleAcknowledgementStarted { .. } => {
            current.begin_lifecycle_ack()
        }
        ScaleSetDeliveryRecoveryPhase::LifecycleAcknowledged { .. } => {
            current.record_lifecycle_ack()
        }
        ScaleSetDeliveryRecoveryPhase::SettlementPrepared {
            prior_catalog,
            catalog_revision,
            catalog_digest,
            ..
        } => current.prepare_settlement_binding(
            prior_catalog.clone(),
            *catalog_revision,
            catalog_digest.clone(),
        ),
        ScaleSetDeliveryRecoveryPhase::Reconciled
        | ScaleSetDeliveryRecoveryPhase::LifecycleReconciled { .. } => return false,
    };
    expected.is_ok_and(|expected| expected == *candidate)
}

fn map_recovery_error(error: ScaleSetDeliveryRecoveryError) -> PersonalWorkerStoreError {
    let kind = match error.kind() {
        ScaleSetDeliveryRecoveryErrorKind::VersionIncompatible => {
            PersonalWorkerStoreErrorKind::VersionIncompatible
        }
        ScaleSetDeliveryRecoveryErrorKind::Conflict => {
            PersonalWorkerStoreErrorKind::RevisionConflict
        }
        ScaleSetDeliveryRecoveryErrorKind::InvalidDocument
        | ScaleSetDeliveryRecoveryErrorKind::DocumentTooLarge
        | ScaleSetDeliveryRecoveryErrorKind::NonCanonical
        | ScaleSetDeliveryRecoveryErrorKind::CorruptState => {
            PersonalWorkerStoreErrorKind::CorruptState
        }
    };
    store_error(kind, "Scale Set delivery recovery state is invalid")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::disposable_attempt_catalog::DisposableAttemptCatalogDocument;
    use crate::github_scale_set_bridge::{
        ScaleSetBridgeEvent, ScaleSetBridgeJobEvidence, ScaleSetBridgePoll, ScaleSetStatistics,
    };
    use crate::github_scale_set_delivery::ScaleSetDelivery;
    use crate::github_scale_set_delivery_state::ScaleSetDeliveryRecoveryState;
    use crate::github_scale_set_protocol::{ScaleSetJobId, ScaleSetRunnerRequestId};

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
                "smolrunner-scale-set-delivery-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).unwrap();
            let metadata = fs::symlink_metadata(&path).unwrap();
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

    fn job(request_id: u64, job_id: &str) -> ScaleSetBridgeJobEvidence {
        ScaleSetBridgeJobEvidence {
            runner_request_id: request_id,
            repository: "project".to_owned(),
            owner: "example".to_owned(),
            job_id: ScaleSetJobId::parse(job_id).unwrap(),
            workflow_run_id: 99,
            request_labels: vec!["smolrunner".to_owned()],
        }
    }

    fn initial() -> ScaleSetDeliveryRecoveryState {
        let delivery = ScaleSetDelivery::from_bridge_poll(&ScaleSetBridgePoll::Message {
            message_id: 7,
            statistics: ScaleSetStatistics {
                available_jobs: 1,
                acquired_jobs: 0,
                assigned_jobs: 0,
                running_jobs: 0,
                registered_runners: 0,
                busy_runners: 0,
                idle_runners: 0,
            },
            events: vec![ScaleSetBridgeEvent::Available(job(41, "job-1"))],
        })
        .unwrap()
        .unwrap();
        ScaleSetDeliveryRecoveryState::reconciled(
            delivery,
            &DisposableAttemptCatalogDocument::empty(),
            &DisposableAttemptCatalogDocument::empty(),
        )
        .unwrap()
    }

    fn write_private(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[test]
    fn create_replace_and_reopen_exact_recovery_state() {
        let root = TempRoot::new("publish");
        let mut store =
            UnixPersonalWorkerStore::open_or_create_scale_set_delivery_recovery(root.path())
                .unwrap();
        let initial = initial();
        store.create_scale_set_delivery_recovery(&initial).unwrap();
        let started = initial.begin_ack().unwrap();
        store
            .replace_scale_set_delivery_recovery(initial.revision(), &started)
            .unwrap();
        assert_eq!(
            store.load_scale_set_delivery_recovery().unwrap(),
            Some(started.clone())
        );

        drop(store);
        let reopened =
            UnixPersonalWorkerStore::open_or_create_scale_set_delivery_recovery(root.path())
                .unwrap();
        assert_eq!(
            reopened.load_scale_set_delivery_recovery().unwrap(),
            Some(started)
        );
    }

    #[test]
    fn stale_or_skipped_successor_is_refused() {
        let root = TempRoot::new("stale");
        let mut store =
            UnixPersonalWorkerStore::open_or_create_scale_set_delivery_recovery(root.path())
                .unwrap();
        let initial = initial();
        store.create_scale_set_delivery_recovery(&initial).unwrap();
        let started = initial.begin_ack().unwrap();
        store
            .replace_scale_set_delivery_recovery(initial.revision(), &started)
            .unwrap();
        assert_eq!(
            store
                .replace_scale_set_delivery_recovery(initial.revision(), &started)
                .unwrap_err()
                .kind(),
            PersonalWorkerStoreErrorKind::RevisionConflict
        );
    }

    #[test]
    fn restart_publishes_exact_initial_stage() {
        let root = TempRoot::new("recover-initial");
        let store =
            UnixPersonalWorkerStore::open_or_create_scale_set_delivery_recovery(root.path())
                .unwrap();
        let initial = initial();
        write_private(
            &root
                .store_directory()
                .join(STAGED_DELIVERY_RECOVERY_DOCUMENT),
            &encode_scale_set_delivery_recovery(&initial).unwrap(),
        );
        drop(store);

        let recovered =
            UnixPersonalWorkerStore::open_or_create_scale_set_delivery_recovery(root.path())
                .unwrap();
        assert_eq!(
            recovered.load_scale_set_delivery_recovery().unwrap(),
            Some(initial)
        );
        assert!(
            !root
                .store_directory()
                .join(STAGED_DELIVERY_RECOVERY_DOCUMENT)
                .exists()
        );
    }

    #[test]
    fn restart_publishes_exact_staged_successor() {
        let root = TempRoot::new("recover-successor");
        let mut store =
            UnixPersonalWorkerStore::open_or_create_scale_set_delivery_recovery(root.path())
                .unwrap();
        let initial = initial();
        store.create_scale_set_delivery_recovery(&initial).unwrap();
        let started = initial.begin_ack().unwrap();
        write_private(
            &root
                .store_directory()
                .join(STAGED_DELIVERY_RECOVERY_DOCUMENT),
            &encode_scale_set_delivery_recovery(&started).unwrap(),
        );
        drop(store);

        let recovered =
            UnixPersonalWorkerStore::open_or_create_scale_set_delivery_recovery(root.path())
                .unwrap();
        assert_eq!(
            recovered.load_scale_set_delivery_recovery().unwrap(),
            Some(started)
        );
        assert!(
            !root
                .store_directory()
                .join(STAGED_DELIVERY_RECOVERY_DOCUMENT)
                .exists()
        );
    }

    #[test]
    fn duplicate_staged_current_is_removed_after_restart() {
        let root = TempRoot::new("duplicate-stage");
        let mut store =
            UnixPersonalWorkerStore::open_or_create_scale_set_delivery_recovery(root.path())
                .unwrap();
        let initial = initial();
        store.create_scale_set_delivery_recovery(&initial).unwrap();
        write_private(
            &root
                .store_directory()
                .join(STAGED_DELIVERY_RECOVERY_DOCUMENT),
            &encode_scale_set_delivery_recovery(&initial).unwrap(),
        );
        drop(store);

        let reopened =
            UnixPersonalWorkerStore::open_or_create_scale_set_delivery_recovery(root.path())
                .unwrap();
        assert_eq!(
            reopened.load_scale_set_delivery_recovery().unwrap(),
            Some(initial)
        );
        assert!(
            !root
                .store_directory()
                .join(STAGED_DELIVERY_RECOVERY_DOCUMENT)
                .exists()
        );
    }

    #[test]
    fn conflicting_stage_is_preserved_and_refused() {
        let root = TempRoot::new("conflict");
        let mut store =
            UnixPersonalWorkerStore::open_or_create_scale_set_delivery_recovery(root.path())
                .unwrap();
        let initial = initial();
        store.create_scale_set_delivery_recovery(&initial).unwrap();
        let foreign = ScaleSetDeliveryRecoveryState::reconciled(
            ScaleSetDelivery::from_bridge_poll(&ScaleSetBridgePoll::Message {
                message_id: 8,
                statistics: ScaleSetStatistics {
                    available_jobs: 0,
                    acquired_jobs: 0,
                    assigned_jobs: 0,
                    running_jobs: 0,
                    registered_runners: 0,
                    busy_runners: 0,
                    idle_runners: 0,
                },
                events: vec![],
            })
            .unwrap()
            .unwrap(),
            &DisposableAttemptCatalogDocument::empty(),
            &DisposableAttemptCatalogDocument::empty(),
        )
        .unwrap();
        write_private(
            &root
                .store_directory()
                .join(STAGED_DELIVERY_RECOVERY_DOCUMENT),
            &encode_scale_set_delivery_recovery(&foreign).unwrap(),
        );
        drop(store);

        assert_eq!(
            UnixPersonalWorkerStore::open_or_create_scale_set_delivery_recovery(root.path())
                .unwrap_err()
                .kind(),
            PersonalWorkerStoreErrorKind::CorruptState
        );
        assert!(
            root.store_directory()
                .join(STAGED_DELIVERY_RECOVERY_DOCUMENT)
                .exists()
        );
    }

    #[test]
    fn writer_lock_blocks_delivery_reads() {
        let root = TempRoot::new("busy");
        let store =
            UnixPersonalWorkerStore::open_or_create_scale_set_delivery_recovery(root.path())
                .unwrap();
        let _guard = store.acquire_mutation_lock().unwrap();
        assert_eq!(
            store.load_scale_set_delivery_recovery().unwrap_err().kind(),
            PersonalWorkerStoreErrorKind::Busy
        );
    }

    #[test]
    fn staged_symlink_is_refused_and_preserved() {
        let root = TempRoot::new("symlink-stage");
        let store =
            UnixPersonalWorkerStore::open_or_create_scale_set_delivery_recovery(root.path())
                .unwrap();
        let staged = root
            .store_directory()
            .join(STAGED_DELIVERY_RECOVERY_DOCUMENT);
        std::os::unix::fs::symlink(DELIVERY_RECOVERY_DOCUMENT, &staged).unwrap();
        drop(store);

        assert_eq!(
            UnixPersonalWorkerStore::open_or_create_scale_set_delivery_recovery(root.path())
                .unwrap_err()
                .kind(),
            PersonalWorkerStoreErrorKind::UnsafeFilesystem
        );
        assert!(
            fs::symlink_metadata(staged)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn hard_linked_current_is_refused() {
        let root = TempRoot::new("hard-link-current");
        let mut store =
            UnixPersonalWorkerStore::open_or_create_scale_set_delivery_recovery(root.path())
                .unwrap();
        let initial = initial();
        store.create_scale_set_delivery_recovery(&initial).unwrap();
        let current = root.store_directory().join(DELIVERY_RECOVERY_DOCUMENT);
        fs::hard_link(&current, root.store_directory().join("current-alias")).unwrap();

        assert_eq!(
            store.load_scale_set_delivery_recovery().unwrap_err().kind(),
            PersonalWorkerStoreErrorKind::UnsafeFilesystem
        );
    }

    #[test]
    fn permissive_current_mode_is_refused() {
        let root = TempRoot::new("permissive-current");
        let mut store =
            UnixPersonalWorkerStore::open_or_create_scale_set_delivery_recovery(root.path())
                .unwrap();
        let initial = initial();
        store.create_scale_set_delivery_recovery(&initial).unwrap();
        let current = root.store_directory().join(DELIVERY_RECOVERY_DOCUMENT);
        fs::set_permissions(&current, fs::Permissions::from_mode(0o644)).unwrap();

        assert_eq!(
            store.load_scale_set_delivery_recovery().unwrap_err().kind(),
            PersonalWorkerStoreErrorKind::UnsafeFilesystem
        );
    }

    #[test]
    fn recovery_acquisition_successor_persists_positive_subset() {
        let root = TempRoot::new("replay-acquire");
        let mut store =
            UnixPersonalWorkerStore::open_or_create_scale_set_delivery_recovery(root.path())
                .unwrap();
        let initial = initial();
        store.create_scale_set_delivery_recovery(&initial).unwrap();
        let started = initial.begin_ack().unwrap();
        store
            .replace_scale_set_delivery_recovery(initial.revision(), &started)
            .unwrap();
        let observed = started
            .record_recovery_acquire(&[ScaleSetRunnerRequestId::new(41).unwrap()])
            .unwrap();
        store
            .replace_scale_set_delivery_recovery(started.revision(), &observed)
            .unwrap();
        assert_eq!(
            store.load_scale_set_delivery_recovery().unwrap(),
            Some(observed)
        );
    }
}
