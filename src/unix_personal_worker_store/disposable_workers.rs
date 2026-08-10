use super::*;

use crate::disposable_worker_store::{
    DisposableWorkerStore, DisposableWorkerStoreDocument, DisposableWorkerStoreError,
    DisposableWorkerStoreErrorKind, DisposableWorkerStoreRecovery,
    DisposableWorkerStoreRecoveryDisposition, DisposableWorkerStoreRevision,
    DisposableWorkerStoreWriteDisposition, DisposableWorkerStoreWriteReceipt,
    MAX_DISPOSABLE_WORKER_STORE_BYTES, decode_disposable_worker_store_document,
    encode_disposable_worker_store_document,
};

const DISPOSABLE_CURRENT_DOCUMENT: &str = "disposable-current.json";
const DISPOSABLE_STAGED_DOCUMENT: &str = ".disposable-next.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryPlan {
    Clean {
        revision: Option<DisposableWorkerStoreRevision>,
    },
    PublishStaged {
        revision: DisposableWorkerStoreRevision,
        no_replace: bool,
    },
    RemoveStaleStaged {
        revision: DisposableWorkerStoreRevision,
    },
}

impl UnixPersonalWorkerStore {
    /// Open or create the shared canonical store authority and recover disposable-worker state.
    ///
    /// Unlike the legacy constructor, this does not require or create a personal-worker queue
    /// document. Both products share the proven directory and lock but keep independent schemas.
    pub fn open_or_create_disposable(
        root_path: impl AsRef<Path>,
    ) -> Result<(Self, DisposableWorkerStoreRecovery), DisposableWorkerStoreError> {
        let root = fs::open(root_path.as_ref(), DIRECTORY_FLAGS, Mode::empty())
            .map_err(map_personal_error_from_root)?;
        let root_stat = inspect_directory(&root, "disposable-worker state root", None)
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
        synchronize_directory(&store._root, "disposable-worker state root")
            .map_err(map_personal_error)?;
        lima_authority::refuse_unsettled_lima_authority(&store).map_err(map_personal_error)?;
        let recovery = store.recover_disposable_locked()?;
        Ok((store, recovery))
    }

    fn load_disposable_named(
        &self,
        name: &str,
    ) -> Result<Option<DisposableWorkerStoreDocument>, DisposableWorkerStoreError> {
        self.read_named_bytes(name)
            .map_err(map_personal_error)?
            .map(|bytes| decode_disposable_worker_store_document(&bytes))
            .transpose()
    }

    fn disposable_recovery_plan(&self) -> Result<RecoveryPlan, DisposableWorkerStoreError> {
        let Some(staged) = self.load_disposable_named(DISPOSABLE_STAGED_DOCUMENT)? else {
            return Ok(RecoveryPlan::Clean {
                revision: self
                    .load_disposable_named(DISPOSABLE_CURRENT_DOCUMENT)?
                    .map(|document| document.revision()),
            });
        };
        let current = self.load_disposable_named(DISPOSABLE_CURRENT_DOCUMENT)?;
        match current {
            None => {
                if staged.revision().get() != 1
                    || !staged.attempts().is_empty()
                    || !staged.completed_attempt_ids().is_empty()
                {
                    return Err(corrupt());
                }
                Ok(RecoveryPlan::PublishStaged {
                    revision: staged.revision(),
                    no_replace: true,
                })
            }
            Some(current) if staged.revision() <= current.revision() => {
                Ok(RecoveryPlan::RemoveStaleStaged {
                    revision: current.revision(),
                })
            }
            Some(current) => {
                staged
                    .validate_successor_of(&current)
                    .map_err(|_| corrupt())?;
                Ok(RecoveryPlan::PublishStaged {
                    revision: staged.revision(),
                    no_replace: false,
                })
            }
        }
    }

    fn refuse_unsettled_personal_worker_state(&self) -> Result<(), DisposableWorkerStoreError> {
        match self.recovery_plan().map_err(map_personal_error)? {
            StoreRecoveryPlan::Clean { .. } => Ok(()),
            StoreRecoveryPlan::PublishStaged { .. }
            | StoreRecoveryPlan::RemoveStaleStaged { .. } => {
                Err(DisposableWorkerStoreError::public(
                    DisposableWorkerStoreErrorKind::RevisionConflict,
                    "personal-worker recovery must complete before disposable-worker mutation",
                ))
            }
        }
    }

    fn stage_disposable_document(
        &self,
        document: &DisposableWorkerStoreDocument,
    ) -> Result<StagedDocument<'_>, DisposableWorkerStoreError> {
        let encoded = encode_disposable_worker_store_document(document)?;
        self.stage_named_bytes(DISPOSABLE_STAGED_DOCUMENT, &encoded)
            .map_err(map_personal_error)
    }

    fn synchronize_existing_disposable_stage(
        &self,
        expected: &DisposableWorkerStoreDocument,
    ) -> Result<(), DisposableWorkerStoreError> {
        let file = fs::openat(
            &self.directory,
            DISPOSABLE_STAGED_DOCUMENT,
            EXISTING_FILE_FLAGS,
            Mode::empty(),
        )
        .map_err(|error| map_personal_error(map_document_open_error(error)))?;
        inspect_private_file(&file, self.owner, "staged disposable-worker document", None)
            .map_err(map_personal_error)?;
        let mut file = File::from(file);
        let mut bytes = Vec::new();
        std::io::Read::by_ref(&mut file)
            .take((MAX_DISPOSABLE_WORKER_STORE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| io("could not read staged disposable-worker state"))?;
        if bytes.len() > MAX_DISPOSABLE_WORKER_STORE_BYTES
            || decode_disposable_worker_store_document(&bytes).as_ref() != Ok(expected)
        {
            return Err(corrupt());
        }
        file.sync_all()
            .map_err(|_| io("could not synchronize staged disposable-worker state"))?;
        inspect_private_file(
            file.as_fd(),
            self.owner,
            "staged disposable-worker document",
            Some(bytes.len()),
        )
        .map_err(map_personal_error)?;
        Ok(())
    }

    fn remove_disposable_stage(&self) -> Result<(), DisposableWorkerStoreError> {
        match fs::unlinkat(
            &self.directory,
            DISPOSABLE_STAGED_DOCUMENT,
            AtFlags::empty(),
        ) {
            Ok(()) => synchronize_directory(&self.directory, "personal worker store directory")
                .map_err(map_personal_error),
            Err(Errno::NOENT) => Ok(()),
            Err(_) => Err(io("could not remove stale disposable-worker state")),
        }
    }

    fn recover_disposable_locked(
        &mut self,
    ) -> Result<DisposableWorkerStoreRecovery, DisposableWorkerStoreError> {
        self.refuse_unsettled_personal_worker_state()?;
        match self.disposable_recovery_plan()? {
            RecoveryPlan::Clean { revision } => Ok(DisposableWorkerStoreRecovery::new(
                DisposableWorkerStoreRecoveryDisposition::Clean,
                revision,
            )),
            RecoveryPlan::PublishStaged {
                revision,
                no_replace,
            } => {
                let staged = self
                    .load_disposable_named(DISPOSABLE_STAGED_DOCUMENT)?
                    .ok_or_else(corrupt)?;
                self.synchronize_existing_disposable_stage(&staged)?;
                let mut guard =
                    StagedDocument::existing(self.directory.as_fd(), DISPOSABLE_STAGED_DOCUMENT);
                self.publish_named_staged(&mut guard, DISPOSABLE_CURRENT_DOCUMENT, no_replace)
                    .map_err(map_personal_error)?;
                Ok(DisposableWorkerStoreRecovery::new(
                    DisposableWorkerStoreRecoveryDisposition::PublishedStaged,
                    Some(revision),
                ))
            }
            RecoveryPlan::RemoveStaleStaged { revision } => {
                self.remove_disposable_stage()?;
                Ok(DisposableWorkerStoreRecovery::new(
                    DisposableWorkerStoreRecoveryDisposition::RemovedStaleStaged,
                    Some(revision),
                ))
            }
        }
    }
}

impl DisposableWorkerStore for UnixPersonalWorkerStore {
    fn load(&self) -> Result<Option<DisposableWorkerStoreDocument>, DisposableWorkerStoreError> {
        let _lock = self.acquire_read_lock().map_err(map_personal_error)?;
        self.load_disposable_named(DISPOSABLE_CURRENT_DOCUMENT)
    }

    fn create(
        &mut self,
        document: &DisposableWorkerStoreDocument,
    ) -> Result<DisposableWorkerStoreWriteReceipt, DisposableWorkerStoreError> {
        if document.revision().get() != 1
            || !document.attempts().is_empty()
            || !document.completed_attempt_ids().is_empty()
        {
            return Err(DisposableWorkerStoreError::public(
                DisposableWorkerStoreErrorKind::RevisionConflict,
                "initial disposable-worker state must be empty revision one",
            ));
        }
        let _lock = self.acquire_mutation_lock().map_err(map_personal_error)?;
        lima_authority::refuse_unsettled_lima_authority(self).map_err(map_personal_error)?;
        self.recover_disposable_locked()?;
        if self
            .load_disposable_named(DISPOSABLE_CURRENT_DOCUMENT)?
            .is_some()
        {
            return Err(DisposableWorkerStoreError::public(
                DisposableWorkerStoreErrorKind::RevisionConflict,
                "disposable-worker state already exists",
            ));
        }
        let bytes_written = encode_disposable_worker_store_document(document)?.len();
        let mut staged = self.stage_disposable_document(document)?;
        self.publish_named_staged(&mut staged, DISPOSABLE_CURRENT_DOCUMENT, true)
            .map_err(map_personal_error)?;
        Ok(DisposableWorkerStoreWriteReceipt::new(
            DisposableWorkerStoreWriteDisposition::Created,
            document.revision(),
            bytes_written,
        ))
    }

    fn replace_if_revision(
        &mut self,
        expected_revision: DisposableWorkerStoreRevision,
        document: &DisposableWorkerStoreDocument,
    ) -> Result<DisposableWorkerStoreWriteReceipt, DisposableWorkerStoreError> {
        let _lock = self.acquire_mutation_lock().map_err(map_personal_error)?;
        lima_authority::refuse_unsettled_lima_authority(self).map_err(map_personal_error)?;
        self.recover_disposable_locked()?;
        let current = self
            .load_disposable_named(DISPOSABLE_CURRENT_DOCUMENT)?
            .ok_or_else(|| {
                DisposableWorkerStoreError::public(
                    DisposableWorkerStoreErrorKind::Missing,
                    "disposable-worker state does not exist",
                )
            })?;
        if current.revision() != expected_revision {
            return Err(DisposableWorkerStoreError::public(
                DisposableWorkerStoreErrorKind::RevisionConflict,
                "disposable-worker state revision changed before publication",
            ));
        }
        document.validate_successor_of(&current)?;
        let bytes_written = encode_disposable_worker_store_document(document)?.len();
        let mut staged = self.stage_disposable_document(document)?;
        self.publish_named_staged(&mut staged, DISPOSABLE_CURRENT_DOCUMENT, false)
            .map_err(map_personal_error)?;
        Ok(DisposableWorkerStoreWriteReceipt::new(
            DisposableWorkerStoreWriteDisposition::Replaced,
            document.revision(),
            bytes_written,
        ))
    }

    fn recover(&mut self) -> Result<DisposableWorkerStoreRecovery, DisposableWorkerStoreError> {
        let _lock = self.acquire_mutation_lock().map_err(map_personal_error)?;
        lima_authority::refuse_unsettled_lima_authority(self).map_err(map_personal_error)?;
        self.recover_disposable_locked()
    }
}

pub(super) fn refuse_unsettled(
    store: &UnixPersonalWorkerStore,
) -> Result<(), PersonalWorkerStoreError> {
    if store
        .read_named_bytes(DISPOSABLE_STAGED_DOCUMENT)?
        .is_some()
    {
        return Err(store_error(
            PersonalWorkerStoreErrorKind::RevisionConflict,
            "disposable-worker recovery must complete before personal-worker mutation",
        ));
    }
    Ok(())
}

fn map_personal_error(error: PersonalWorkerStoreError) -> DisposableWorkerStoreError {
    let kind = match error.kind() {
        PersonalWorkerStoreErrorKind::InvalidDocument
        | PersonalWorkerStoreErrorKind::CorruptState => {
            DisposableWorkerStoreErrorKind::CorruptState
        }
        PersonalWorkerStoreErrorKind::RevisionConflict => {
            DisposableWorkerStoreErrorKind::RevisionConflict
        }
        PersonalWorkerStoreErrorKind::Busy => DisposableWorkerStoreErrorKind::Busy,
        PersonalWorkerStoreErrorKind::Missing => DisposableWorkerStoreErrorKind::Missing,
        PersonalWorkerStoreErrorKind::Io => DisposableWorkerStoreErrorKind::Io,
        PersonalWorkerStoreErrorKind::UnsafeFilesystem => {
            DisposableWorkerStoreErrorKind::UnsafeFilesystem
        }
        PersonalWorkerStoreErrorKind::VersionIncompatible => {
            DisposableWorkerStoreErrorKind::VersionIncompatible
        }
    };
    DisposableWorkerStoreError::public(kind, error.message())
}

fn map_personal_error_from_root(error: Errno) -> DisposableWorkerStoreError {
    map_personal_error(map_root_open_error(error))
}

fn io(message: &'static str) -> DisposableWorkerStoreError {
    DisposableWorkerStoreError::public(DisposableWorkerStoreErrorKind::Io, message)
}

fn corrupt() -> DisposableWorkerStoreError {
    DisposableWorkerStoreError::public(
        DisposableWorkerStoreErrorKind::CorruptState,
        "durable disposable-worker state is corrupt or noncanonical",
    )
}
