use super::*;

use crate::disposable_attempt_catalog::{
    DisposableAttemptCatalogCodecError, DisposableAttemptCatalogCodecErrorKind,
    DisposableAttemptCatalogDocument, DisposableAttemptCatalogError,
    DisposableAttemptCatalogErrorKind, DisposableAttemptCatalogRevision,
    DisposableAttemptCatalogStore, DisposableAttemptCatalogWriteDisposition,
    DisposableAttemptCatalogWriteReceipt, MAX_DISPOSABLE_ATTEMPT_CATALOG_DOCUMENT_BYTES,
    decode_disposable_attempt_catalog, encode_disposable_attempt_catalog,
};

const CATALOG_DOCUMENT: &str = "disposable-attempt-catalog.json";
const STAGED_CATALOG_DOCUMENT: &str = ".disposable-attempt-catalog.next.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryPlan {
    Clean,
    PublishStaged { no_replace: bool },
    RemoveStaleStaged,
}

impl UnixPersonalWorkerStore {
    /// Open or create the shared private store authority and recover the disposable catalog.
    ///
    /// This creates no legacy queue document. The two products share only the accepted directory,
    /// persistent lock, no-follow inspection, and atomic publication machinery.
    pub fn open_or_create_disposable_catalog(
        root_path: impl AsRef<Path>,
    ) -> Result<Self, DisposableAttemptCatalogError> {
        let root = fs::open(root_path.as_ref(), DIRECTORY_FLAGS, Mode::empty())
            .map_err(|error| map_personal_error(map_root_open_error(error)))?;
        let root_stat = inspect_directory(&root, "disposable-attempt state root", None)
            .map_err(map_personal_error)?;
        let owner = (root_stat.st_uid, root_stat.st_gid);
        let (directory, publication_lock) =
            open_or_publish_initialization_directory(&root, owner).map_err(map_personal_error)?;
        let mut store = Self {
            _root: root,
            directory,
            owner,
        };
        let _lock = match publication_lock {
            Some(lock) => lock,
            None => store.acquire_mutation_lock().map_err(map_personal_error)?,
        };
        synchronize_directory(&store._root, "disposable-attempt state root")
            .map_err(map_personal_error)?;
        synchronize_catalog_directory(&store)?;
        refuse_unsettled_lima_authority(&store)?;
        store.recover_catalog_locked()?;
        Ok(store)
    }

    fn load_catalog_named(
        &self,
        name: &str,
    ) -> Result<Option<DisposableAttemptCatalogDocument>, DisposableAttemptCatalogError> {
        self.read_named_bytes_bounded(name, MAX_DISPOSABLE_ATTEMPT_CATALOG_DOCUMENT_BYTES)
            .map_err(map_personal_error)?
            .map(|bytes| decode_disposable_attempt_catalog(&bytes).map_err(map_codec_error))
            .transpose()
    }

    fn catalog_recovery_plan(&self) -> Result<RecoveryPlan, DisposableAttemptCatalogError> {
        let Some(staged) = self.load_catalog_named(STAGED_CATALOG_DOCUMENT)? else {
            return Ok(RecoveryPlan::Clean);
        };
        let current = self.load_catalog_named(CATALOG_DOCUMENT)?;
        match current {
            None => {
                if staged != DisposableAttemptCatalogDocument::empty() {
                    return Err(corrupt());
                }
                Ok(RecoveryPlan::PublishStaged { no_replace: true })
            }
            Some(current) if staged == current => Ok(RecoveryPlan::RemoveStaleStaged),
            Some(current)
                if current
                    .revision()
                    .get()
                    .checked_add(1)
                    .is_some_and(|next| staged.revision().get() == next) =>
            {
                staged
                    .validate_successor_of(&current)
                    .map_err(|_| corrupt())?;
                Ok(RecoveryPlan::PublishStaged { no_replace: false })
            }
            Some(_) => Err(corrupt()),
        }
    }

    fn refuse_unsettled_personal_worker_state(&self) -> Result<(), DisposableAttemptCatalogError> {
        match self.recovery_plan().map_err(map_personal_error)? {
            StoreRecoveryPlan::Clean { .. } => Ok(()),
            StoreRecoveryPlan::PublishStaged { .. }
            | StoreRecoveryPlan::RemoveStaleStaged { .. } => Err(public(
                DisposableAttemptCatalogErrorKind::RecoveryRequired,
                "personal-worker recovery must complete before disposable-attempt mutation",
            )),
        }
    }

    fn stage_catalog(
        &self,
        document: &DisposableAttemptCatalogDocument,
    ) -> Result<StagedDocument<'_>, DisposableAttemptCatalogError> {
        let encoded = encode_disposable_attempt_catalog(document).map_err(map_codec_error)?;
        self.stage_named_bytes(STAGED_CATALOG_DOCUMENT, &encoded)
            .map_err(map_personal_error)
    }

    fn synchronize_existing_catalog_stage(
        &self,
        expected: &DisposableAttemptCatalogDocument,
    ) -> Result<(), DisposableAttemptCatalogError> {
        let file = fs::openat(
            &self.directory,
            STAGED_CATALOG_DOCUMENT,
            EXISTING_FILE_FLAGS,
            Mode::empty(),
        )
        .map_err(|error| map_personal_error(map_document_open_error(error)))?;
        inspect_private_file(&file, self.owner, "staged disposable-attempt catalog", None)
            .map_err(map_personal_error)?;
        let mut file = File::from(file);
        let mut bytes = Vec::new();
        std::io::Read::by_ref(&mut file)
            .take((MAX_DISPOSABLE_ATTEMPT_CATALOG_DOCUMENT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| io("could not read staged disposable-attempt catalog"))?;
        if bytes.len() > MAX_DISPOSABLE_ATTEMPT_CATALOG_DOCUMENT_BYTES {
            return Err(corrupt());
        }
        let decoded = decode_disposable_attempt_catalog(&bytes).map_err(map_codec_error)?;
        if &decoded != expected {
            return Err(corrupt());
        }
        file.sync_all()
            .map_err(|_| io("could not synchronize staged disposable-attempt catalog"))?;
        inspect_private_file(
            file.as_fd(),
            self.owner,
            "staged disposable-attempt catalog",
            Some(bytes.len()),
        )
        .map_err(map_personal_error)?;
        Ok(())
    }

    fn remove_catalog_stage(&self) -> Result<(), DisposableAttemptCatalogError> {
        match fs::unlinkat(&self.directory, STAGED_CATALOG_DOCUMENT, AtFlags::empty()) {
            Ok(()) => synchronize_directory(&self.directory, "personal worker store directory")
                .map_err(map_personal_error),
            Err(Errno::NOENT) => Ok(()),
            Err(_) => Err(io("could not remove stale disposable-attempt catalog")),
        }
    }

    fn recover_catalog_locked(&mut self) -> Result<(), DisposableAttemptCatalogError> {
        self.refuse_unsettled_personal_worker_state()?;
        match self.catalog_recovery_plan()? {
            RecoveryPlan::Clean => Ok(()),
            RecoveryPlan::PublishStaged { no_replace } => {
                let staged = self
                    .load_catalog_named(STAGED_CATALOG_DOCUMENT)?
                    .ok_or_else(corrupt)?;
                self.synchronize_existing_catalog_stage(&staged)?;
                let mut guard =
                    StagedDocument::existing(self.directory.as_fd(), STAGED_CATALOG_DOCUMENT);
                self.publish_named_staged(&mut guard, CATALOG_DOCUMENT, no_replace)
                    .map_err(map_personal_error)
            }
            RecoveryPlan::RemoveStaleStaged => self.remove_catalog_stage(),
        }
    }
}

impl DisposableAttemptCatalogStore for UnixPersonalWorkerStore {
    fn recover(&mut self) -> Result<(), DisposableAttemptCatalogError> {
        let _lock = self.acquire_mutation_lock().map_err(map_personal_error)?;
        synchronize_catalog_directory(self)?;
        refuse_unsettled_lima_authority(self)?;
        self.recover_catalog_locked()
    }

    fn load(
        &self,
    ) -> Result<Option<DisposableAttemptCatalogDocument>, DisposableAttemptCatalogError> {
        let _lock = self.acquire_read_lock().map_err(map_personal_error)?;
        if self
            .read_named_bytes_bounded(
                STAGED_CATALOG_DOCUMENT,
                MAX_DISPOSABLE_ATTEMPT_CATALOG_DOCUMENT_BYTES,
            )
            .map_err(map_personal_error)?
            .is_some()
        {
            return Err(public(
                DisposableAttemptCatalogErrorKind::RecoveryRequired,
                "disposable-attempt catalog requires recovery",
            ));
        }
        self.load_catalog_named(CATALOG_DOCUMENT)
    }

    fn create(
        &mut self,
        document: &DisposableAttemptCatalogDocument,
    ) -> Result<DisposableAttemptCatalogWriteReceipt, DisposableAttemptCatalogError> {
        if document != &DisposableAttemptCatalogDocument::empty() {
            return Err(public(
                DisposableAttemptCatalogErrorKind::Conflict,
                "initial disposable-attempt catalog must be empty revision one",
            ));
        }
        let _lock = self.acquire_mutation_lock().map_err(map_personal_error)?;
        synchronize_catalog_directory(self)?;
        refuse_unsettled_lima_authority(self)?;
        self.recover_catalog_locked()?;
        if self.load_catalog_named(CATALOG_DOCUMENT)?.is_some() {
            return Err(public(
                DisposableAttemptCatalogErrorKind::AlreadyExists,
                "disposable-attempt catalog already exists",
            ));
        }
        let mut staged = self.stage_catalog(document)?;
        self.publish_named_staged(&mut staged, CATALOG_DOCUMENT, true)
            .map_err(map_personal_error)?;
        Ok(DisposableAttemptCatalogWriteReceipt::new(
            DisposableAttemptCatalogWriteDisposition::Created,
            document.revision(),
            None,
        ))
    }

    fn replace_if_revision(
        &mut self,
        expected_revision: DisposableAttemptCatalogRevision,
        document: &DisposableAttemptCatalogDocument,
    ) -> Result<DisposableAttemptCatalogWriteReceipt, DisposableAttemptCatalogError> {
        let _lock = self.acquire_mutation_lock().map_err(map_personal_error)?;
        synchronize_catalog_directory(self)?;
        refuse_unsettled_lima_authority(self)?;
        self.recover_catalog_locked()?;
        let current = self.load_catalog_named(CATALOG_DOCUMENT)?.ok_or_else(|| {
            public(
                DisposableAttemptCatalogErrorKind::Missing,
                "disposable-attempt catalog does not exist",
            )
        })?;
        if current.revision() != expected_revision {
            return Err(public(
                DisposableAttemptCatalogErrorKind::Conflict,
                "disposable-attempt catalog revision changed before publication",
            ));
        }
        document.validate_successor_of(&current)?;
        let mut staged = self.stage_catalog(document)?;
        self.publish_named_staged(&mut staged, CATALOG_DOCUMENT, false)
            .map_err(map_personal_error)?;
        Ok(DisposableAttemptCatalogWriteReceipt::new(
            DisposableAttemptCatalogWriteDisposition::Replaced,
            document.revision(),
            None,
        ))
    }
}

pub(super) fn refuse_unsettled(
    store: &UnixPersonalWorkerStore,
) -> Result<(), PersonalWorkerStoreError> {
    if store
        .read_named_bytes_bounded(
            STAGED_CATALOG_DOCUMENT,
            MAX_DISPOSABLE_ATTEMPT_CATALOG_DOCUMENT_BYTES,
        )?
        .is_some()
    {
        return Err(store_error(
            PersonalWorkerStoreErrorKind::RevisionConflict,
            "disposable-attempt recovery must complete before personal-worker mutation",
        ));
    }
    Ok(())
}

fn map_personal_error(error: PersonalWorkerStoreError) -> DisposableAttemptCatalogError {
    let kind = match error.kind() {
        PersonalWorkerStoreErrorKind::InvalidDocument
        | PersonalWorkerStoreErrorKind::CorruptState => {
            DisposableAttemptCatalogErrorKind::CorruptState
        }
        PersonalWorkerStoreErrorKind::RevisionConflict => {
            DisposableAttemptCatalogErrorKind::Conflict
        }
        PersonalWorkerStoreErrorKind::Busy => DisposableAttemptCatalogErrorKind::Busy,
        PersonalWorkerStoreErrorKind::Missing => DisposableAttemptCatalogErrorKind::Missing,
        PersonalWorkerStoreErrorKind::Io => DisposableAttemptCatalogErrorKind::Io,
        PersonalWorkerStoreErrorKind::UnsafeFilesystem => {
            DisposableAttemptCatalogErrorKind::UnsafeFilesystem
        }
        PersonalWorkerStoreErrorKind::VersionIncompatible => {
            DisposableAttemptCatalogErrorKind::VersionIncompatible
        }
    };
    public(kind, error.message())
}

fn synchronize_catalog_directory(
    store: &UnixPersonalWorkerStore,
) -> Result<(), DisposableAttemptCatalogError> {
    // A prior writer may have renamed the stage successfully and then received an ambiguous
    // directory-fsync error. Every later recovery or mutation closes that durability window under
    // the same canonical lock before it classifies current/staged state or returns a receipt.
    synchronize_directory(&store.directory, "personal worker store directory")
        .map_err(map_personal_error)
}

fn refuse_unsettled_lima_authority(
    store: &UnixPersonalWorkerStore,
) -> Result<(), DisposableAttemptCatalogError> {
    lima_authority::refuse_unsettled_lima_authority(store).map_err(|error| {
        if error.kind() == PersonalWorkerStoreErrorKind::RevisionConflict {
            public(
                DisposableAttemptCatalogErrorKind::RecoveryRequired,
                "Lima lifecycle recovery must complete before disposable-attempt mutation",
            )
        } else {
            map_personal_error(error)
        }
    })
}

fn map_codec_error(error: DisposableAttemptCatalogCodecError) -> DisposableAttemptCatalogError {
    let kind = if error.kind() == DisposableAttemptCatalogCodecErrorKind::VersionIncompatible {
        DisposableAttemptCatalogErrorKind::VersionIncompatible
    } else {
        DisposableAttemptCatalogErrorKind::CorruptState
    };
    public(
        kind,
        "disposable-attempt catalog is invalid or noncanonical",
    )
}

fn public(
    kind: DisposableAttemptCatalogErrorKind,
    message: &'static str,
) -> DisposableAttemptCatalogError {
    DisposableAttemptCatalogError::from_store(kind, message)
}

fn io(message: &'static str) -> DisposableAttemptCatalogError {
    public(DisposableAttemptCatalogErrorKind::Io, message)
}

fn corrupt() -> DisposableAttemptCatalogError {
    public(
        DisposableAttemptCatalogErrorKind::CorruptState,
        "disposable-attempt catalog is corrupt or noncanonical",
    )
}

#[cfg(test)]
mod tests {
    use std::fs::{self, OpenOptions};
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;

    use rustix::fs::{FlockOperation, flock};

    use super::*;
    use crate::artifact::Sha256Digest;
    use crate::disposable_attempt_catalog::{
        DisposableAttemptCatalog, DisposableAttemptCatalogAction,
        DisposableAttemptCatalogWriteDisposition, DisposableAttemptReservation,
        MemoryDisposableAttemptCatalogStore,
    };
    use crate::disposable_attempt_state::DisposableAttemptState;
    use crate::disposable_worker_reconciler::{
        CapacityClaimId, DisposableAttemptId, DisposableVmId, DisposableWorkerResources,
    };
    use crate::execution_admission::EpochMillis;
    use crate::github_scale_set_protocol::ScaleSetRunnerName;

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(label: &str) -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "smolrunner-disposable-catalog-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create temporary state root");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o750))
                .expect("set private root mode");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn store_directory(&self) -> PathBuf {
            self.0.join(STORE_DIRECTORY)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn reservation(index: usize) -> DisposableAttemptReservation {
        DisposableAttemptReservation::new(
            DisposableAttemptState::reserved(
                DisposableAttemptId::parse(&format!("attempt-{index}")).unwrap(),
                CapacityClaimId::parse(&format!("claim-{index}")).unwrap(),
                DisposableVmId::parse(&format!("vm-{index}")).unwrap(),
                ScaleSetRunnerName::parse(&format!("smol-attempt-{index}")).unwrap(),
                EpochMillis::new(100_000 + u64::try_from(index).unwrap()).unwrap(),
            ),
            DisposableWorkerResources::new(1_000, 2_000, 3_000).unwrap(),
            Sha256Digest::parse(&format!("sha256:{}", "ab".repeat(32))).unwrap(),
        )
        .unwrap()
    }

    fn successor(index: usize) -> DisposableAttemptCatalogDocument {
        let mut catalog =
            DisposableAttemptCatalog::new(MemoryDisposableAttemptCatalogStore::default());
        let (empty, _) = catalog.initialize().unwrap();
        catalog
            .reserve(empty.revision(), reservation(index))
            .unwrap()
            .0
    }

    fn progressed_successor(index: usize) -> DisposableAttemptCatalogDocument {
        let mut catalog =
            DisposableAttemptCatalog::new(MemoryDisposableAttemptCatalogStore::default());
        let (empty, _) = catalog.initialize().unwrap();
        let (reserved, _) = catalog
            .reserve(empty.revision(), reservation(index))
            .unwrap();
        let attempt_id = DisposableAttemptId::parse(&format!("attempt-{index}")).unwrap();
        catalog
            .transition(
                reserved.revision(),
                &attempt_id,
                reserved
                    .find_active(&attempt_id)
                    .unwrap()
                    .attempt()
                    .revision(),
                DisposableAttemptCatalogAction::AuthorizeClone,
            )
            .unwrap()
            .0
    }

    fn write_private(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).expect("write private fixture");
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .expect("set private file mode");
    }

    #[test]
    fn unix_catalog_initializes_transitions_and_replays_without_writes() {
        let root = TempRoot::new("round-trip");
        let store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        let mut catalog = DisposableAttemptCatalog::new(store);
        let (empty, created) = catalog.initialize().unwrap();
        assert_eq!(
            created.disposition,
            DisposableAttemptCatalogWriteDisposition::Created
        );
        let first = reservation(1);
        let (reserved, written) = catalog.reserve(empty.revision(), first.clone()).unwrap();
        assert_eq!(
            written.disposition,
            DisposableAttemptCatalogWriteDisposition::Replaced
        );
        let (same, duplicate) = catalog.reserve(reserved.revision(), first).unwrap();
        assert_eq!(same, reserved);
        assert_eq!(
            duplicate.disposition,
            DisposableAttemptCatalogWriteDisposition::Satisfied
        );

        let attempt_id = DisposableAttemptId::parse("attempt-1").unwrap();
        let attempt_revision = same.find_active(&attempt_id).unwrap().attempt().revision();
        let (provisioning, _) = catalog
            .transition(
                same.revision(),
                &attempt_id,
                attempt_revision,
                DisposableAttemptCatalogAction::AuthorizeClone,
            )
            .unwrap();
        drop(catalog);

        let store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        let catalog = DisposableAttemptCatalog::new(store);
        assert_eq!(catalog.load().unwrap(), provisioning);
    }

    #[test]
    fn restart_publishes_exact_stage_removes_stale_stage_and_preserves_corruption() {
        let root = TempRoot::new("recovery");
        let store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        let mut catalog = DisposableAttemptCatalog::new(store);
        catalog.initialize().unwrap();
        drop(catalog);

        let next = successor(1);
        write_private(
            &root.store_directory().join(STAGED_CATALOG_DOCUMENT),
            &encode_disposable_attempt_catalog(&next).unwrap(),
        );
        let store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        assert_eq!(DisposableAttemptCatalog::new(store).load().unwrap(), next);

        let read_only_store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        write_private(
            &root.store_directory().join(STAGED_CATALOG_DOCUMENT),
            &encode_disposable_attempt_catalog(&next).unwrap(),
        );
        let read_only_catalog = DisposableAttemptCatalog::new(read_only_store);
        assert_eq!(
            read_only_catalog.load().unwrap_err().kind(),
            DisposableAttemptCatalogErrorKind::RecoveryRequired
        );
        assert!(
            root.store_directory()
                .join(STAGED_CATALOG_DOCUMENT)
                .exists()
        );
        drop(read_only_catalog);
        UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        assert!(
            !root
                .store_directory()
                .join(STAGED_CATALOG_DOCUMENT)
                .exists()
        );

        let forged_successor = progressed_successor(2);
        assert_eq!(forged_successor.revision().get(), next.revision().get() + 1);
        let mut direct_store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        assert_eq!(
            DisposableAttemptCatalogStore::replace_if_revision(
                &mut direct_store,
                next.revision(),
                &forged_successor,
            )
            .unwrap_err()
            .kind(),
            DisposableAttemptCatalogErrorKind::Conflict
        );
        write_private(
            &root.store_directory().join(STAGED_CATALOG_DOCUMENT),
            &encode_disposable_attempt_catalog(&forged_successor).unwrap(),
        );
        assert_eq!(
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path())
                .unwrap_err()
                .kind(),
            DisposableAttemptCatalogErrorKind::CorruptState
        );
        assert!(
            root.store_directory()
                .join(STAGED_CATALOG_DOCUMENT)
                .exists()
        );
        fs::remove_file(root.store_directory().join(STAGED_CATALOG_DOCUMENT)).unwrap();

        let conflicting = successor(2);
        assert_eq!(conflicting.revision(), next.revision());
        write_private(
            &root.store_directory().join(STAGED_CATALOG_DOCUMENT),
            &encode_disposable_attempt_catalog(&conflicting).unwrap(),
        );
        assert_eq!(
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path())
                .unwrap_err()
                .kind(),
            DisposableAttemptCatalogErrorKind::CorruptState
        );
        assert!(
            root.store_directory()
                .join(STAGED_CATALOG_DOCUMENT)
                .exists()
        );
        fs::remove_file(root.store_directory().join(STAGED_CATALOG_DOCUMENT)).unwrap();

        write_private(
            &root.store_directory().join(STAGED_CATALOG_DOCUMENT),
            b"not canonical JSON\n",
        );
        assert_eq!(
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path())
                .unwrap_err()
                .kind(),
            DisposableAttemptCatalogErrorKind::CorruptState
        );
        assert!(
            root.store_directory()
                .join(STAGED_CATALOG_DOCUMENT)
                .exists()
        );
    }

    #[test]
    fn missing_lock_and_concurrent_writers_fail_closed() {
        let unsafe_root = TempRoot::new("missing-lock");
        fs::create_dir(unsafe_root.store_directory()).unwrap();
        fs::set_permissions(
            unsafe_root.store_directory(),
            fs::Permissions::from_mode(0o750),
        )
        .unwrap();
        assert_eq!(
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(unsafe_root.path())
                .unwrap_err()
                .kind(),
            DisposableAttemptCatalogErrorKind::UnsafeFilesystem
        );

        let root = TempRoot::new("concurrent");
        let initializer =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        let mut initializer = DisposableAttemptCatalog::new(initializer);
        let (empty, _) = initializer.initialize().unwrap();
        drop(initializer);
        let first =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        let second =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let revision = empty.revision();
        let run = |store: UnixPersonalWorkerStore,
                   candidate: DisposableAttemptReservation,
                   barrier: Arc<Barrier>| {
            thread::spawn(move || {
                let mut catalog = DisposableAttemptCatalog::new(store);
                barrier.wait();
                catalog.reserve(revision, candidate)
            })
        };
        let left = run(first, reservation(1), Arc::clone(&barrier));
        let right = run(second, reservation(2), Arc::clone(&barrier));
        barrier.wait();
        let outcomes = [left.join().unwrap(), right.join().unwrap()];
        assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
        assert!(
            outcomes
                .iter()
                .filter_map(|result| result.as_ref().err())
                .all(|error| {
                    matches!(
                        error.kind(),
                        DisposableAttemptCatalogErrorKind::Busy
                            | DisposableAttemptCatalogErrorKind::Conflict
                    )
                })
        );
    }

    #[test]
    fn persistent_lock_excludes_catalog_open_and_recovery() {
        let root = TempRoot::new("lock");
        UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.store_directory().join(STORE_LOCK_FILE))
            .unwrap();
        flock(&lock, FlockOperation::NonBlockingLockExclusive).unwrap();
        assert_eq!(
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path())
                .unwrap_err()
                .kind(),
            DisposableAttemptCatalogErrorKind::Busy
        );
    }
}
