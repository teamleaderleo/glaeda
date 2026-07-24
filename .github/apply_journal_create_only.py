from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    file = Path(path)
    text = file.read_text()
    count = text.count(old)
    if count != 1:
        raise RuntimeError(
            f"expected exactly one match in {path}, found {count}: {old[:120]!r}"
        )
    file.write_text(text.replace(old, new, 1))


replace_once(
    "src/state_store.rs",
    """pub enum StateStoreErrorKind {
    Busy,
    Io,
    UnsafeFilesystem,
    CorruptState,
}""",
    """pub enum StateStoreErrorKind {
    Busy,
    Conflict,
    Io,
    UnsafeFilesystem,
    CorruptState,
}""",
)

replace_once(
    "src/state_store.rs",
    """/// Narrow persistence boundary for canonical state paths and prevalidated document bytes.
pub trait StateStore {
    fn read(&self, path: &StatePath) -> Result<StateRead, StateStoreError>;

    fn write_atomic(&mut self, record: &StateRecord) -> Result<StateWriteReceipt, StateStoreError>;
}""",
    """/// Narrow persistence boundary for canonical state paths and prevalidated document bytes.
pub trait StateStore {
    fn read(&self, path: &StatePath) -> Result<StateRead, StateStoreError>;

    /// Atomically create one record without replacing an existing destination.
    fn create_atomic(
        &mut self,
        record: &StateRecord,
    ) -> Result<StateWriteReceipt, StateStoreError>;

    /// Atomically create or replace one record.
    fn write_atomic(&mut self, record: &StateRecord) -> Result<StateWriteReceipt, StateStoreError>;
}""",
)

replace_once(
    "src/state_store.rs",
    """        fn write_atomic(
            &mut self,
            record: &StateRecord,
        ) -> Result<StateWriteReceipt, StateStoreError> {
            let key = record
                .path()
                .components()
                .iter()
                .map(StateComponent::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let disposition = if self.entries.insert(key, record.bytes().to_vec()).is_some() {
                StateWriteDisposition::Replaced
            } else {
                StateWriteDisposition::Created
            };
            Ok(StateWriteReceipt::new(disposition, record.bytes().len()))
        }""",
    """        fn create_atomic(
            &mut self,
            record: &StateRecord,
        ) -> Result<StateWriteReceipt, StateStoreError> {
            let key = record
                .path()
                .components()
                .iter()
                .map(StateComponent::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            if self.entries.contains_key(&key) {
                return Err(StateStoreError::public(
                    super::StateStoreErrorKind::Conflict,
                    "state destination already exists",
                ));
            }
            self.entries.insert(key, record.bytes().to_vec());
            Ok(StateWriteReceipt::new(
                StateWriteDisposition::Created,
                record.bytes().len(),
            ))
        }

        fn write_atomic(
            &mut self,
            record: &StateRecord,
        ) -> Result<StateWriteReceipt, StateStoreError> {
            let key = record
                .path()
                .components()
                .iter()
                .map(StateComponent::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let disposition = if self.entries.insert(key, record.bytes().to_vec()).is_some() {
                StateWriteDisposition::Replaced
            } else {
                StateWriteDisposition::Created
            };
            Ok(StateWriteReceipt::new(disposition, record.bytes().len()))
        }""",
)

replace_once(
    "src/state_store.rs",
    """        let mut store = MemoryStore::default();
        let first = store.write_atomic(&record).expect("first write");
        assert_eq!(first.disposition(), StateWriteDisposition::Created);
        assert_eq!(first.bytes_written(), record.bytes().len());
        let second = store.write_atomic(&record).expect("replacement write");
        assert_eq!(second.disposition(), StateWriteDisposition::Replaced);""",
    """        let mut store = MemoryStore::default();
        let first = store.create_atomic(&record).expect("first create");
        assert_eq!(first.disposition(), StateWriteDisposition::Created);
        assert_eq!(first.bytes_written(), record.bytes().len());
        let conflict = store
            .create_atomic(&record)
            .expect_err("create-only publication must not replace");
        assert_eq!(conflict.kind(), super::StateStoreErrorKind::Conflict);
        let second = store.write_atomic(&record).expect("replacement write");
        assert_eq!(second.disposition(), StateWriteDisposition::Replaced);""",
)

replace_once(
    "tests/state_store_contract.rs",
    """    fn write_atomic(&mut self, record: &StateRecord) -> Result<StateWriteReceipt, StateStoreError> {
        Ok(StateWriteReceipt::new(
            StateWriteDisposition::Created,
            record.bytes().len(),
        ))
    }""",
    """    fn create_atomic(
        &mut self,
        record: &StateRecord,
    ) -> Result<StateWriteReceipt, StateStoreError> {
        Ok(StateWriteReceipt::new(
            StateWriteDisposition::Created,
            record.bytes().len(),
        ))
    }

    fn write_atomic(&mut self, record: &StateRecord) -> Result<StateWriteReceipt, StateStoreError> {
        Ok(StateWriteReceipt::new(
            StateWriteDisposition::Created,
            record.bytes().len(),
        ))
    }""",
)

replace_once(
    "src/linux_state.rs",
    "use rustix::fs::{self, AtFlags, FileType, FlockOperation, Mode, OFlags};",
    "use rustix::fs::{self, AtFlags, FileType, FlockOperation, Mode, OFlags, RenameFlags};",
)

replace_once(
    "src/linux_state.rs",
    """    /// Atomically publish one validated state record inside an already-prepared state tree.
    ///
    /// The installation and destination parent directories must already exist. Writers are
    /// serialized through a persistent installation-local lock file. Publication writes a random
    /// exclusive temporary file, sets mode `0600`, synchronizes the file, renames it within the
    /// destination directory, and synchronizes that directory.
    ///
    /// # Errors
    ///
    /// Returns `Busy` when another SmolRunner writer holds the installation lock,
    /// `UnsafeFilesystem` for symlinked or incompatible state objects, and `Io` for bounded
    /// creation, write, synchronization, or rename failures.
    pub fn write_atomic(""",
    """    /// Atomically create one validated state record without replacing existing state.
    ///
    /// Publication uses the installation-local writer lock and `RENAME_NOREPLACE`, so a journal-ID
    /// collision fails before existing recovery evidence can be replaced.
    ///
    /// # Errors
    ///
    /// Returns `Conflict` when the destination already exists, `Busy` when another SmolRunner
    /// writer holds the installation lock, `UnsafeFilesystem` for symlinked or incompatible state
    /// objects, and `Io` for bounded creation, write, synchronization, or rename failures.
    pub fn create_atomic(
        &mut self,
        record: &StateRecord,
    ) -> Result<StateWriteReceipt, StateStoreError> {
        verify_managed_directory(&self.root, "state root", Some(self.owner))?;
        let _lock = self.acquire_installation_lock(record.path())?;
        let (parent, file_name) = self.open_required_parent(record.path())?;
        if inspect_destination(&parent, file_name, self.owner)? == StateWriteDisposition::Replaced {
            return Err(StateStoreError::public(
                StateStoreErrorKind::Conflict,
                "state destination already exists",
            ));
        }
        let (temporary, temporary_name) = create_temporary_file(&parent)?;
        let mut temporary_path = TemporaryPath::new(parent.as_fd(), temporary_name);

        fs::fchmod(&temporary, PRIVATE_FILE_MODE).map_err(|_| {
            StateStoreError::public(
                StateStoreErrorKind::Io,
                "could not set private state-file permissions",
            )
        })?;
        verify_managed_file(&temporary, "temporary state file", self.owner, false)?;
        let mut faults = NoWriteFaults;
        write_and_sync(temporary, record.bytes(), &mut faults)?;

        fs::renameat_with(
            &parent,
            temporary_path.name(),
            &parent,
            file_name.as_str(),
            RenameFlags::NOREPLACE,
        )
        .map_err(map_create_rename_error)?;
        temporary_path.disarm();

        fs::fsync(&parent).map_err(|_| {
            StateStoreError::public(
                StateStoreErrorKind::Io,
                "state file was published but its parent directory could not be synchronized",
            )
        })?;

        Ok(StateWriteReceipt::new(
            StateWriteDisposition::Created,
            record.bytes().len(),
        ))
    }

    /// Atomically publish one validated state record inside an already-prepared state tree.
    ///
    /// The installation and destination parent directories must already exist. Writers are
    /// serialized through a persistent installation-local lock file. Publication writes a random
    /// exclusive temporary file, sets mode `0600`, synchronizes the file, renames it within the
    /// destination directory, and synchronizes that directory.
    ///
    /// # Errors
    ///
    /// Returns `Busy` when another SmolRunner writer holds the installation lock,
    /// `UnsafeFilesystem` for symlinked or incompatible state objects, and `Io` for bounded
    /// creation, write, synchronization, or rename failures.
    pub fn write_atomic(""",
)

replace_once(
    "src/linux_state.rs",
    """impl StateStore for LinuxStateRoot {
    fn read(&self, path: &StatePath) -> Result<StateRead, StateStoreError> {
        Self::read(self, path)
    }

    fn write_atomic(&mut self, record: &StateRecord) -> Result<StateWriteReceipt, StateStoreError> {
        Self::write_atomic(self, record)
    }
}""",
    """impl StateStore for LinuxStateRoot {
    fn read(&self, path: &StatePath) -> Result<StateRead, StateStoreError> {
        Self::read(self, path)
    }

    fn create_atomic(
        &mut self,
        record: &StateRecord,
    ) -> Result<StateWriteReceipt, StateStoreError> {
        Self::create_atomic(self, record)
    }

    fn write_atomic(&mut self, record: &StateRecord) -> Result<StateWriteReceipt, StateStoreError> {
        Self::write_atomic(self, record)
    }
}""",
)

replace_once(
    "src/linux_state.rs",
    """fn map_rename_error(error: Errno) -> StateStoreError {
    match error {
        Errno::ISDIR | Errno::NOTDIR | Errno::LOOP => StateStoreError::public(""",
    """fn map_create_rename_error(error: Errno) -> StateStoreError {
    match error {
        Errno::EXIST => StateStoreError::public(
            StateStoreErrorKind::Conflict,
            "state destination already exists",
        ),
        Errno::ISDIR | Errno::NOTDIR | Errno::LOOP => StateStoreError::public(
            StateStoreErrorKind::UnsafeFilesystem,
            "state destination changed to an incompatible filesystem object",
        ),
        _ => StateStoreError::public(
            StateStoreErrorKind::Io,
            "could not publish new temporary state file",
        ),
    }
}

fn map_rename_error(error: Errno) -> StateStoreError {
    match error {
        Errno::ISDIR | Errno::NOTDIR | Errno::LOOP => StateStoreError::public(""",
)

replace_once(
    "src/linux_state.rs",
    """    #[test]
    fn prepublication_failures_preserve_existing_state_and_remove_temporary_files() {""",
    """    #[test]
    fn create_only_publication_refuses_to_replace_existing_state() {
        let root = TempTree::new("create-only");
        let parent = create_project_parent(root.path());
        let mut store = LinuxStateRoot::open(root.path()).expect("open state root");
        let original = project_record("example/original");
        let receipt = store.create_atomic(&original).expect("create state");
        assert_eq!(receipt.disposition(), StateWriteDisposition::Created);

        let replacement = project_record("example/replacement");
        let error = store
            .create_atomic(&replacement)
            .expect_err("create-only publication must not replace");
        assert_eq!(error.kind(), StateStoreErrorKind::Conflict);
        assert_eq!(
            store.read(original.path()).expect("read preserved state"),
            StateRead::Present(original.bytes().to_vec())
        );
        assert_no_temporary_files(&parent);
    }

    #[test]
    fn prepublication_failures_preserve_existing_state_and_remove_temporary_files() {""",
)

replace_once(
    "src/durable_journal.rs",
    """pub struct StateStoreJournalCheckpoint<'a, S> {
    store: &'a mut S,
    installation_id: InstallationId,
    journal_id: JournalId,
}""",
    """pub struct StateStoreJournalCheckpoint<'a, S> {
    store: &'a mut S,
    installation_id: InstallationId,
    journal_id: JournalId,
    initialized: bool,
}""",
)

replace_once(
    "src/durable_journal.rs",
    """        Self {
            store,
            installation_id,
            journal_id,
        }""",
    """        Self {
            store,
            installation_id,
            journal_id,
            initialized: false,
        }""",
)

replace_once(
    "src/durable_journal.rs",
    """        self.store.write_atomic(&record).map_err(|error| {
            JournalCheckpointFailure::public(format!(
                "journal snapshot could not be atomically persisted: {}",
                error.message()
            ))
        })?;
        Ok(())""",
    """        let publication = if self.initialized {
            self.store.write_atomic(&record)
        } else {
            self.store.create_atomic(&record)
        };
        publication.map_err(|error| {
            JournalCheckpointFailure::public(format!(
                "journal snapshot could not be atomically persisted: {}",
                error.message()
            ))
        })?;
        self.initialized = true;
        Ok(())""",
)

replace_once(
    "src/durable_journal.rs",
    """        StateRead, StateRecord, StateStore, StateStoreError, StateWriteDisposition,
        StateWriteReceipt,""",
    """        StateRead, StateRecord, StateStore, StateStoreError, StateStoreErrorKind,
        StateWriteDisposition, StateWriteReceipt,""",
)

replace_once(
    "src/durable_journal.rs",
    """        fn write_atomic(
            &mut self,
            record: &StateRecord,
        ) -> Result<StateWriteReceipt, StateStoreError> {
            let key = record
                .path()
                .components()
                .iter()
                .map(StateComponent::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let disposition = if self.entries.insert(key, record.bytes().to_vec()).is_some() {
                StateWriteDisposition::Replaced
            } else {
                StateWriteDisposition::Created
            };
            Ok(StateWriteReceipt::new(disposition, record.bytes().len()))
        }""",
    """        fn create_atomic(
            &mut self,
            record: &StateRecord,
        ) -> Result<StateWriteReceipt, StateStoreError> {
            let key = record
                .path()
                .components()
                .iter()
                .map(StateComponent::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            if self.entries.contains_key(&key) {
                return Err(StateStoreError::public(
                    StateStoreErrorKind::Conflict,
                    "state destination already exists",
                ));
            }
            self.entries.insert(key, record.bytes().to_vec());
            Ok(StateWriteReceipt::new(
                StateWriteDisposition::Created,
                record.bytes().len(),
            ))
        }

        fn write_atomic(
            &mut self,
            record: &StateRecord,
        ) -> Result<StateWriteReceipt, StateStoreError> {
            let key = record
                .path()
                .components()
                .iter()
                .map(StateComponent::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let disposition = if self.entries.insert(key, record.bytes().to_vec()).is_some() {
                StateWriteDisposition::Replaced
            } else {
                StateWriteDisposition::Created
            };
            Ok(StateWriteReceipt::new(disposition, record.bytes().len()))
        }""",
)

replace_once(
    "src/durable_journal.rs",
    """    #[test]
    fn state_store_adapter_replaces_one_canonical_journal_document() {""",
    """    #[test]
    fn state_store_adapter_refuses_to_clobber_an_existing_journal() {
        let mut store = MemoryStore::default();
        let installation_id =
            InstallationId::parse("0123456789abcdef").expect("installation ID");
        let journal_id = JournalId::parse("apply-00000001").expect("journal ID");
        let path = crate::state::StateLayout::journal_document(&installation_id, &journal_id);
        let key = path
            .components()
            .iter()
            .map(StateComponent::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let existing = b"existing recovery evidence".to_vec();
        store.entries.insert(key.clone(), existing.clone());

        let mut executor = FakeExecutor::default();
        let error = {
            let mut checkpoint =
                StateStoreJournalCheckpoint::new(&mut store, installation_id, journal_id);
            execute_plan_durably(
                vec![action("one", RollbackClass::Reversible)],
                &mut executor,
                &mut checkpoint,
                false,
            )
            .expect_err("journal collision must fail")
        };

        assert!(executor.executions.is_empty());
        let DurableExecutionError::Checkpoint(error) = error else {
            panic!("expected checkpoint error");
        };
        assert_eq!(error.phase(), super::JournalCheckpointPhase::Initial);
        assert!(error.last_durable().is_none());
        assert_eq!(store.entries.get(&key), Some(&existing));
    }

    #[test]
    fn state_store_adapter_replaces_one_canonical_journal_document() {""",
)

replace_once(
    "docs/adr/0014-durable-execution-journal-checkpoints.md",
    """The state-store adapter rebuilds and validates the complete journal document for every checkpoint, then delegates publication to the existing atomic `StateStore` boundary. Linux therefore inherits the existing installation-local lock, private temporary file, file synchronization, atomic rename, and parent-directory synchronization behavior.
""",
    """The state-store adapter rebuilds and validates the complete journal document for every checkpoint. Its first publication is create-only: an existing journal ID is a conflict and its recovery evidence is not replaced. Later checkpoints replace only the journal created by that adapter. Linux performs create-only publication with the installation-local lock and `RENAME_NOREPLACE`; both creation and replacement retain private temporary files, file synchronization, atomic rename, and parent-directory synchronization.
""",
)

replace_once(
    "docs/adr/0014-durable-execution-journal-checkpoints.md",
    """- Persistence failure after an executor returns can leave the durable snapshot intentionally conservative (`executing` or `rollback_in_progress`).
""",
    """- Persistence failure after an executor returns can leave the durable snapshot intentionally conservative (`executing` or `rollback_in_progress`).
- A duplicate journal ID fails before existing recovery evidence can be replaced.
""",
)
