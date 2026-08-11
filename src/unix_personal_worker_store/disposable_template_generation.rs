use super::*;

use crate::disposable_template_generation::{
    DisposableTemplateGenerationDisposition, DisposableTemplateGenerationDocument,
    DisposableTemplateGenerationError, DisposableTemplateGenerationErrorKind,
    DisposableTemplateGenerationPhase, DisposableTemplateGenerationPlan,
    DisposableTemplateObservation, MAX_DISPOSABLE_TEMPLATE_GENERATION_BYTES,
    decode_disposable_template_generation, encode_disposable_template_generation,
};

pub(super) const GENERATION_DOCUMENT: &str = "disposable-template-generation.json";
pub(super) const STAGED_GENERATION_DOCUMENT: &str = ".disposable-template-generation.next.json";
pub(crate) const TEMPLATE_INPUT_DOCUMENT: &str = "disposable-template-input-v2.yaml";
const STAGED_TEMPLATE_INPUT_DOCUMENT: &str = ".disposable-template-input-v2.next.yaml";

struct ExactTemplateInput {
    file: File,
    snapshot: TemplateInputSnapshot,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct TemplateInputSnapshot {
    device: u64,
    inode: u64,
    owner: u32,
    group: u32,
    mode: u32,
    links: u64,
    size: i64,
    modified_seconds: i64,
    modified_nanoseconds: u64,
    changed_seconds: i64,
    changed_nanoseconds: u64,
}

impl TemplateInputSnapshot {
    // rustix exposes these libc stat fields with different integer types on macOS and Linux.
    #[allow(clippy::useless_conversion)]
    fn from_stat(stat: &rustix::fs::Stat) -> Self {
        Self {
            device: u64::try_from(stat.st_dev).unwrap_or(u64::MAX),
            inode: stat.st_ino,
            owner: stat.st_uid,
            group: stat.st_gid,
            mode: u32::from(stat.st_mode),
            links: u64::from(stat.st_nlink),
            size: stat.st_size,
            modified_seconds: stat.st_mtime,
            modified_nanoseconds: u64::try_from(stat.st_mtime_nsec).unwrap_or(u64::MAX),
            changed_seconds: stat.st_ctime,
            changed_nanoseconds: u64::try_from(stat.st_ctime_nsec).unwrap_or(u64::MAX),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryPlan {
    Clean,
    PublishStaged { no_replace: bool },
    RemoveStaleStaged,
}

impl UnixPersonalWorkerStore {
    /// Open or create the shared private store and recover prepared-template generation state.
    pub fn open_or_create_disposable_template_generation(
        root_path: impl AsRef<Path>,
    ) -> Result<Self, PersonalWorkerStoreError> {
        let root = fs::open(root_path.as_ref(), DIRECTORY_FLAGS, Mode::empty())
            .map_err(map_root_open_error)?;
        let root_stat = inspect_directory(&root, "disposable-template state root", None)?;
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
        synchronize_directory(&store._root, "disposable-template state root")?;
        synchronize_directory(&store.directory, "personal worker store directory")?;
        store.refuse_other_unsettled_state()?;
        store.recover_template_generation_locked()?;
        Ok(store)
    }

    /// Load the current canonical generation document without recovering a staged publication.
    pub fn load_disposable_template_generation(
        &self,
    ) -> Result<Option<DisposableTemplateGenerationDocument>, PersonalWorkerStoreError> {
        let _lock = self.acquire_read_lock()?;
        if self
            .read_named_bytes_bounded(
                STAGED_GENERATION_DOCUMENT,
                MAX_DISPOSABLE_TEMPLATE_GENERATION_BYTES,
            )?
            .is_some()
        {
            return Err(store_error(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "disposable-template generation requires recovery",
            ));
        }
        self.load_template_generation_named(GENERATION_DOCUMENT)
    }

    /// Publish only an exact revision-one pending generation document.
    pub fn create_disposable_template_generation(
        &mut self,
        document: &DisposableTemplateGenerationDocument,
    ) -> Result<(), PersonalWorkerStoreError> {
        if document.revision() != 1
            || document.phase() != DisposableTemplateGenerationPhase::Pending
            || document.owned_object_identity().is_some()
        {
            return Err(store_error(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "initial disposable-template generation is invalid",
            ));
        }
        let _lock = self.acquire_mutation_lock()?;
        synchronize_directory(&self.directory, "personal worker store directory")?;
        self.refuse_other_unsettled_state()?;
        self.recover_template_generation_locked()?;
        if self
            .load_template_generation_named(GENERATION_DOCUMENT)?
            .is_some()
        {
            return Err(store_error(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "disposable-template generation already exists",
            ));
        }
        let mut staged = self.stage_template_generation(document)?;
        self.publish_named_staged(&mut staged, GENERATION_DOCUMENT, true)?;
        Ok(())
    }

    /// Reconfirm and publish one advisory persistence decision under the canonical lock.
    ///
    /// Candidate VM commands are deliberately rejected here. The future runtime service must keep
    /// this lock across its second private observation, started checkpoint, and bounded command.
    pub fn persist_confirmed_disposable_template_generation(
        &mut self,
        plan: DisposableTemplateGenerationPlan,
        confirmation: DisposableTemplateObservation,
    ) -> Result<DisposableTemplateGenerationDocument, PersonalWorkerStoreError> {
        if !matches!(
            plan.disposition(),
            DisposableTemplateGenerationDisposition::Persist { .. }
        ) {
            return Err(store_error(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "disposable-template runtime candidate is not persistence authority",
            ));
        }
        let _lock = self.acquire_mutation_lock()?;
        synchronize_directory(&self.directory, "personal worker store directory")?;
        self.refuse_other_unsettled_state()?;
        self.recover_template_generation_locked()?;
        let current = self
            .load_template_generation_named(GENERATION_DOCUMENT)?
            .ok_or_else(|| {
                store_error(
                    PersonalWorkerStoreErrorKind::Missing,
                    "disposable-template generation does not exist",
                )
            })?;
        let successor = plan
            .confirmed_persist_successor(&current, confirmation)
            .map_err(map_generation_error)?;
        successor
            .validate_successor_of(&current)
            .map_err(map_generation_error)?;
        let mut staged = self.stage_template_generation(&successor)?;
        self.publish_named_staged(&mut staged, GENERATION_DOCUMENT, false)?;
        Ok(successor)
    }

    /// Reconfirm one advisory command candidate, durably publish its Started checkpoint, and run
    /// the fixed command while retaining the canonical writer lock.
    ///
    /// The confirmation closure runs only after current state has been recovered and reopened
    /// under the lock. The command closure runs only after the exact Started successor is durable.
    /// Its success or failure never advances the document again; a later fresh observation must
    /// reconcile the externally visible result. This makes a process crash or command error
    /// recovery-required rather than replay authority.
    pub(crate) fn execute_confirmed_disposable_template_candidate<T, E, C>(
        &mut self,
        plan: DisposableTemplateGenerationPlan,
        state_root: &Path,
        template_input: &[u8],
        confirm: impl FnOnce(
            &DisposableTemplateGenerationDocument,
        ) -> Result<(DisposableTemplateObservation, C), E>,
        execute: impl FnOnce(&DisposableTemplateGenerationDocument, C) -> Result<T, E>,
    ) -> Result<Result<(DisposableTemplateGenerationDocument, T), E>, PersonalWorkerStoreError>
    {
        if !matches!(
            plan.disposition(),
            DisposableTemplateGenerationDisposition::CreateCandidate
                | DisposableTemplateGenerationDisposition::StopCandidate
                | DisposableTemplateGenerationDisposition::DiscardCandidate
        ) {
            return Err(store_error(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "disposable-template plan is not a runtime command candidate",
            ));
        }
        let _lock = self.acquire_mutation_lock()?;
        synchronize_directory(&self.directory, "personal worker store directory")?;
        self.refuse_other_unsettled_state()?;
        self.recover_template_generation_locked()?;
        let current = self
            .load_template_generation_named(GENERATION_DOCUMENT)?
            .ok_or_else(|| {
                store_error(
                    PersonalWorkerStoreErrorKind::Missing,
                    "disposable-template generation does not exist",
                )
            })?;
        if !plan.is_bound_to_document(&current) {
            return Err(store_error(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "disposable-template runtime plan no longer matches durable state",
            ));
        }
        let mut exact_template_input =
            self.ensure_exact_template_input(state_root, template_input)?;
        let (confirmation, retained_evidence) = match confirm(&current) {
            Ok(confirmation) => confirmation,
            Err(error) => return Ok(Err(error)),
        };
        let successor = plan
            .confirmed_runtime_successor(&current, confirmation)
            .map_err(map_generation_error)?;
        successor
            .validate_successor_of(&current)
            .map_err(map_generation_error)?;
        self.confirm_exact_template_input(state_root, template_input, &mut exact_template_input)?;
        let mut staged = self.stage_template_generation(&successor)?;
        self.publish_named_staged(&mut staged, GENERATION_DOCUMENT, false)?;
        self.confirm_exact_template_input(state_root, template_input, &mut exact_template_input)?;
        let result = execute(&successor, retained_evidence);
        self.confirm_exact_template_input(state_root, template_input, &mut exact_template_input)?;
        Ok(result.map(|value| (successor, value)))
    }

    fn ensure_exact_template_input(
        &self,
        state_root: &Path,
        expected: &[u8],
    ) -> Result<ExactTemplateInput, PersonalWorkerStoreError> {
        if expected.is_empty() || expected.len() > MAX_DISPOSABLE_TEMPLATE_GENERATION_BYTES * 4 {
            return Err(PersonalWorkerStoreError::corrupt_state());
        }
        let current = self.read_named_bytes_bounded(
            TEMPLATE_INPUT_DOCUMENT,
            MAX_DISPOSABLE_TEMPLATE_GENERATION_BYTES * 4,
        )?;
        let staged = self.read_named_bytes_bounded(
            STAGED_TEMPLATE_INPUT_DOCUMENT,
            MAX_DISPOSABLE_TEMPLATE_GENERATION_BYTES * 4,
        )?;
        if let Some(actual) = current {
            if actual != expected {
                return Err(store_error(
                    PersonalWorkerStoreErrorKind::RevisionConflict,
                    "the durable Lima template input differs from the current prepared template",
                ));
            }
            if staged.is_some() {
                return Err(store_error(
                    PersonalWorkerStoreErrorKind::RevisionConflict,
                    "durable Lima template input has an unexpected staged publication",
                ));
            }
            let mut exact = self.open_exact_template_input(TEMPLATE_INPUT_DOCUMENT, expected)?;
            exact.file.sync_all().map_err(|_| {
                store_error(
                    PersonalWorkerStoreErrorKind::Io,
                    "could not synchronize the durable Lima template input",
                )
            })?;
            synchronize_directory(&self.directory, "personal worker store directory")?;
            self.confirm_exact_template_input(state_root, expected, &mut exact)?;
            return Ok(exact);
        }

        match staged {
            Some(bytes) if bytes == expected => {
                let exact_stage =
                    self.open_exact_template_input(STAGED_TEMPLATE_INPUT_DOCUMENT, expected)?;
                exact_stage.file.sync_all().map_err(|_| {
                    store_error(
                        PersonalWorkerStoreErrorKind::Io,
                        "could not synchronize the staged Lima template input",
                    )
                })?;
                let mut guard = StagedDocument::existing(
                    self.directory.as_fd(),
                    STAGED_TEMPLATE_INPUT_DOCUMENT,
                );
                self.publish_named_staged(&mut guard, TEMPLATE_INPUT_DOCUMENT, true)?;
            }
            Some(_) => {
                fs::unlinkat(
                    &self.directory,
                    STAGED_TEMPLATE_INPUT_DOCUMENT,
                    AtFlags::empty(),
                )
                .map_err(|_| {
                    store_error(
                        PersonalWorkerStoreErrorKind::Io,
                        "could not remove incomplete staged Lima template input",
                    )
                })?;
                synchronize_directory(&self.directory, "personal worker store directory")?;
                let mut created =
                    self.stage_named_bytes(STAGED_TEMPLATE_INPUT_DOCUMENT, expected)?;
                self.publish_named_staged(&mut created, TEMPLATE_INPUT_DOCUMENT, true)?;
            }
            None => {
                let mut created =
                    self.stage_named_bytes(STAGED_TEMPLATE_INPUT_DOCUMENT, expected)?;
                self.publish_named_staged(&mut created, TEMPLATE_INPUT_DOCUMENT, true)?;
            }
        }
        let mut exact = self.open_exact_template_input(TEMPLATE_INPUT_DOCUMENT, expected)?;
        self.confirm_exact_template_input(state_root, expected, &mut exact)?;
        Ok(exact)
    }

    fn open_exact_template_input(
        &self,
        name: &'static str,
        expected: &[u8],
    ) -> Result<ExactTemplateInput, PersonalWorkerStoreError> {
        let file = fs::openat(&self.directory, name, EXISTING_FILE_FLAGS, Mode::empty())
            .map_err(map_document_open_error)?;
        inspect_private_file(
            &file,
            self.owner,
            "durable Lima template input",
            Some(expected.len()),
        )?;
        let mut file = File::from(file);
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(|_| {
            store_error(
                PersonalWorkerStoreErrorKind::Io,
                "could not read the durable Lima template input",
            )
        })?;
        if bytes != expected {
            return Err(PersonalWorkerStoreError::corrupt_state());
        }
        let snapshot =
            TemplateInputSnapshot::from_stat(&fs::fstat(file.as_fd()).map_err(|_| {
                store_error(
                    PersonalWorkerStoreErrorKind::Io,
                    "could not inspect the durable Lima template input",
                )
            })?);
        let path_snapshot = TemplateInputSnapshot::from_stat(
            &fs::statat(&self.directory, name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(map_document_open_error)?,
        );
        if path_snapshot != snapshot {
            return Err(store_error(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "durable Lima template input changed while it was opened",
            ));
        }
        Ok(ExactTemplateInput { file, snapshot })
    }

    fn confirm_exact_template_input(
        &self,
        state_root: &Path,
        expected: &[u8],
        exact: &mut ExactTemplateInput,
    ) -> Result<(), PersonalWorkerStoreError> {
        let root =
            fs::open(state_root, DIRECTORY_FLAGS, Mode::empty()).map_err(map_root_open_error)?;
        let root_stat = inspect_directory(&root, "disposable-template state root", None)?;
        let held_root_stat =
            inspect_directory(&self._root, "disposable-template state root", None)?;
        if (root_stat.st_dev, root_stat.st_ino) != (held_root_stat.st_dev, held_root_stat.st_ino) {
            return Err(store_error(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "disposable-template state root changed during command authorization",
            ));
        }
        let directory = fs::openat(&root, STORE_DIRECTORY, DIRECTORY_FLAGS, Mode::empty())
            .map_err(map_store_directory_open_error)?;
        let directory_stat = inspect_directory(
            &directory,
            "personal worker store directory",
            Some(self.owner),
        )?;
        let held_directory_stat = inspect_directory(
            &self.directory,
            "personal worker store directory",
            Some(self.owner),
        )?;
        if (directory_stat.st_dev, directory_stat.st_ino)
            != (held_directory_stat.st_dev, held_directory_stat.st_ino)
        {
            return Err(store_error(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "personal worker store directory changed during command authorization",
            ));
        }

        let path = self.open_exact_template_input(TEMPLATE_INPUT_DOCUMENT, expected)?;
        if path.snapshot != exact.snapshot {
            return Err(store_error(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "durable Lima template input changed during command authorization",
            ));
        }
        exact.file.rewind().map_err(|_| {
            store_error(
                PersonalWorkerStoreErrorKind::Io,
                "could not reread the durable Lima template input",
            )
        })?;
        let mut bytes = Vec::new();
        exact.file.read_to_end(&mut bytes).map_err(|_| {
            store_error(
                PersonalWorkerStoreErrorKind::Io,
                "could not reread the durable Lima template input",
            )
        })?;
        let retained_snapshot =
            TemplateInputSnapshot::from_stat(&fs::fstat(exact.file.as_fd()).map_err(|_| {
                store_error(
                    PersonalWorkerStoreErrorKind::Io,
                    "could not reinspect the durable Lima template input",
                )
            })?);
        if bytes != expected || retained_snapshot != exact.snapshot {
            return Err(store_error(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "durable Lima template input changed during command authorization",
            ));
        }
        Ok(())
    }

    /// Recover a safely classified staged prepared-template generation publication.
    pub fn recover_disposable_template_generation(
        &mut self,
    ) -> Result<(), PersonalWorkerStoreError> {
        let _lock = self.acquire_mutation_lock()?;
        synchronize_directory(&self.directory, "personal worker store directory")?;
        self.refuse_other_unsettled_state()?;
        self.recover_template_generation_locked()
    }

    pub(super) fn refuse_other_unsettled_state(&self) -> Result<(), PersonalWorkerStoreError> {
        disposable_attempt_catalog::refuse_unsettled(self)?;
        super::github_scale_set_inbox::refuse_unsettled(self)?;
        lima_authority::refuse_unsettled_lima_authority(self)?;
        match self.recovery_plan()? {
            StoreRecoveryPlan::Clean { .. } => Ok(()),
            StoreRecoveryPlan::PublishStaged { .. }
            | StoreRecoveryPlan::RemoveStaleStaged { .. } => Err(store_error(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "personal-worker recovery must complete before disposable-template mutation",
            )),
        }
    }

    pub(super) fn load_template_generation_named(
        &self,
        name: &str,
    ) -> Result<Option<DisposableTemplateGenerationDocument>, PersonalWorkerStoreError> {
        self.read_named_bytes_bounded(name, MAX_DISPOSABLE_TEMPLATE_GENERATION_BYTES)?
            .map(|bytes| {
                decode_disposable_template_generation(&bytes).map_err(map_generation_error)
            })
            .transpose()
    }

    fn template_generation_recovery_plan(&self) -> Result<RecoveryPlan, PersonalWorkerStoreError> {
        let Some(staged) = self.load_template_generation_named(STAGED_GENERATION_DOCUMENT)? else {
            return Ok(RecoveryPlan::Clean);
        };
        let current = self.load_template_generation_named(GENERATION_DOCUMENT)?;
        match current {
            None if staged.revision() == 1
                && staged.phase() == DisposableTemplateGenerationPhase::Pending
                && staged.owned_object_identity().is_none() =>
            {
                Ok(RecoveryPlan::PublishStaged { no_replace: true })
            }
            None => Err(PersonalWorkerStoreError::corrupt_state()),
            Some(current) if staged == current => Ok(RecoveryPlan::RemoveStaleStaged),
            Some(current)
                if current
                    .revision()
                    .checked_add(1)
                    .is_some_and(|next| staged.revision() == next) =>
            {
                staged
                    .validate_successor_of(&current)
                    .map_err(map_generation_error)?;
                Ok(RecoveryPlan::PublishStaged { no_replace: false })
            }
            Some(_) => Err(PersonalWorkerStoreError::corrupt_state()),
        }
    }

    fn stage_template_generation(
        &self,
        document: &DisposableTemplateGenerationDocument,
    ) -> Result<StagedDocument<'_>, PersonalWorkerStoreError> {
        let bytes =
            encode_disposable_template_generation(document).map_err(map_generation_error)?;
        self.stage_named_bytes(STAGED_GENERATION_DOCUMENT, &bytes)
    }

    fn synchronize_existing_template_stage(
        &self,
        expected: &DisposableTemplateGenerationDocument,
    ) -> Result<(), PersonalWorkerStoreError> {
        let file = fs::openat(
            &self.directory,
            STAGED_GENERATION_DOCUMENT,
            EXISTING_FILE_FLAGS,
            Mode::empty(),
        )
        .map_err(map_document_open_error)?;
        inspect_private_file(
            &file,
            self.owner,
            "staged disposable-template generation",
            None,
        )?;
        let mut file = File::from(file);
        let mut bytes = Vec::new();
        std::io::Read::by_ref(&mut file)
            .take((MAX_DISPOSABLE_TEMPLATE_GENERATION_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| {
                store_error(
                    PersonalWorkerStoreErrorKind::Io,
                    "could not read staged disposable-template generation",
                )
            })?;
        if bytes.len() > MAX_DISPOSABLE_TEMPLATE_GENERATION_BYTES {
            return Err(PersonalWorkerStoreError::corrupt_state());
        }
        let decoded =
            decode_disposable_template_generation(&bytes).map_err(map_generation_error)?;
        if &decoded != expected {
            return Err(PersonalWorkerStoreError::corrupt_state());
        }
        file.sync_all().map_err(|_| {
            store_error(
                PersonalWorkerStoreErrorKind::Io,
                "could not synchronize staged disposable-template generation",
            )
        })?;
        inspect_private_file(
            file.as_fd(),
            self.owner,
            "staged disposable-template generation",
            Some(bytes.len()),
        )?;
        Ok(())
    }

    fn recover_template_generation_locked(&mut self) -> Result<(), PersonalWorkerStoreError> {
        match self.template_generation_recovery_plan()? {
            RecoveryPlan::Clean => Ok(()),
            RecoveryPlan::PublishStaged { no_replace } => {
                let staged = self
                    .load_template_generation_named(STAGED_GENERATION_DOCUMENT)?
                    .ok_or_else(PersonalWorkerStoreError::corrupt_state)?;
                self.synchronize_existing_template_stage(&staged)?;
                let mut guard =
                    StagedDocument::existing(self.directory.as_fd(), STAGED_GENERATION_DOCUMENT);
                self.publish_named_staged(&mut guard, GENERATION_DOCUMENT, no_replace)
            }
            RecoveryPlan::RemoveStaleStaged => {
                match fs::unlinkat(
                    &self.directory,
                    STAGED_GENERATION_DOCUMENT,
                    AtFlags::empty(),
                ) {
                    Ok(()) => {
                        synchronize_directory(&self.directory, "personal worker store directory")
                    }
                    Err(Errno::NOENT) => Ok(()),
                    Err(_) => Err(store_error(
                        PersonalWorkerStoreErrorKind::Io,
                        "could not remove stale disposable-template generation",
                    )),
                }
            }
        }
    }
}

pub(super) fn refuse_unsettled(
    store: &UnixPersonalWorkerStore,
) -> Result<(), PersonalWorkerStoreError> {
    if store
        .read_named_bytes_bounded(
            STAGED_GENERATION_DOCUMENT,
            MAX_DISPOSABLE_TEMPLATE_GENERATION_BYTES,
        )?
        .is_some()
    {
        return Err(store_error(
            PersonalWorkerStoreErrorKind::RevisionConflict,
            "disposable-template recovery must complete before another state mutation",
        ));
    }
    Ok(())
}

fn map_generation_error(error: DisposableTemplateGenerationError) -> PersonalWorkerStoreError {
    let kind = match error.kind() {
        DisposableTemplateGenerationErrorKind::VersionIncompatible => {
            PersonalWorkerStoreErrorKind::VersionIncompatible
        }
        DisposableTemplateGenerationErrorKind::StaleRevision
        | DisposableTemplateGenerationErrorKind::InvalidTransition
        | DisposableTemplateGenerationErrorKind::RevisionExhausted => {
            PersonalWorkerStoreErrorKind::RevisionConflict
        }
        DisposableTemplateGenerationErrorKind::InvalidDocument
        | DisposableTemplateGenerationErrorKind::NonCanonical => {
            PersonalWorkerStoreErrorKind::CorruptState
        }
    };
    store_error(kind, "disposable-template generation state is invalid")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::disposable_prepared_template::current_disposable_prepared_template;
    use crate::disposable_template_generation::{
        DisposableTemplateGenerationAction, DisposableTemplateGenerationId,
        DisposableTemplateObservedState, DisposableTemplatePriorOperationState,
        DisposableTemplateSourceIdentity, reconcile_disposable_template_generation,
        test_disposable_template_observation,
    };
    use crate::lima_observation::LimaInstanceName;

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
                "smolrunner-template-generation-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create temporary state root");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o750))
                .expect("set private root mode");
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

    fn source_identity() -> DisposableTemplateSourceIdentity {
        DisposableTemplateSourceIdentity::parse(&format!("sha256:{}", "a".repeat(64))).unwrap()
    }

    fn initial() -> DisposableTemplateGenerationDocument {
        DisposableTemplateGenerationDocument::initial(
            DisposableTemplateGenerationId::parse("prepared-template-1").unwrap(),
            current_disposable_prepared_template()
                .unwrap()
                .identity()
                .unwrap(),
            source_identity(),
            LimaInstanceName::parse("smolrunner-prepared-template").unwrap(),
        )
    }

    fn absent_observation(
        document: &DisposableTemplateGenerationDocument,
    ) -> DisposableTemplateObservation {
        test_disposable_template_observation(
            document,
            source_identity(),
            None,
            None,
            DisposableTemplatePriorOperationState::NoPriorOperation,
            DisposableTemplateObservedState::Absent,
        )
    }

    fn write_private(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[test]
    fn exact_second_observation_publishes_one_successor_and_stale_plan_refuses() {
        let root = TempRoot::new("confirmed");
        let mut store =
            UnixPersonalWorkerStore::open_or_create_disposable_template_generation(root.path())
                .unwrap();
        let initial = initial();
        store
            .create_disposable_template_generation(&initial)
            .unwrap();

        let plan =
            reconcile_disposable_template_generation(&initial, &absent_observation(&initial));
        let authorized = store
            .persist_confirmed_disposable_template_generation(plan, absent_observation(&initial))
            .unwrap();
        assert_eq!(
            authorized.phase(),
            DisposableTemplateGenerationPhase::CreateAuthorized
        );
        assert_eq!(
            store.load_disposable_template_generation().unwrap(),
            Some(authorized.clone())
        );

        let stale =
            reconcile_disposable_template_generation(&initial, &absent_observation(&initial));
        assert_eq!(
            store
                .persist_confirmed_disposable_template_generation(
                    stale,
                    absent_observation(&initial),
                )
                .unwrap_err()
                .kind(),
            PersonalWorkerStoreErrorKind::RevisionConflict
        );
    }

    #[test]
    fn candidate_command_is_not_persistence_authority() {
        let root = TempRoot::new("candidate");
        let mut store =
            UnixPersonalWorkerStore::open_or_create_disposable_template_generation(root.path())
                .unwrap();
        let initial = initial();
        store
            .create_disposable_template_generation(&initial)
            .unwrap();
        let authorize =
            reconcile_disposable_template_generation(&initial, &absent_observation(&initial));
        let authorized = store
            .persist_confirmed_disposable_template_generation(
                authorize,
                absent_observation(&initial),
            )
            .unwrap();
        let candidate =
            reconcile_disposable_template_generation(&authorized, &absent_observation(&authorized));
        assert_eq!(
            candidate.disposition(),
            DisposableTemplateGenerationDisposition::CreateCandidate
        );
        assert_eq!(
            store
                .persist_confirmed_disposable_template_generation(
                    candidate,
                    absent_observation(&authorized),
                )
                .unwrap_err()
                .kind(),
            PersonalWorkerStoreErrorKind::RevisionConflict
        );
    }

    #[test]
    fn candidate_recovers_partial_template_stage_and_returns_exact_started_document() {
        let root = TempRoot::new("template-input-recovery");
        let mut store =
            UnixPersonalWorkerStore::open_or_create_disposable_template_generation(root.path())
                .unwrap();
        let initial = initial();
        store
            .create_disposable_template_generation(&initial)
            .unwrap();
        let authorize =
            reconcile_disposable_template_generation(&initial, &absent_observation(&initial));
        let authorized = store
            .persist_confirmed_disposable_template_generation(
                authorize,
                absent_observation(&initial),
            )
            .unwrap();
        write_private(
            &root.store_directory().join(STAGED_TEMPLATE_INPUT_DOCUMENT),
            b"partial crash bytes",
        );
        let candidate =
            reconcile_disposable_template_generation(&authorized, &absent_observation(&authorized));
        let expected = b"exact template bytes\n";
        let (started, ()) = store
            .execute_confirmed_disposable_template_candidate(
                candidate,
                root.path(),
                expected,
                |current| Ok::<_, ()>((absent_observation(current), ())),
                |_started, ()| Ok::<_, ()>(()),
            )
            .unwrap()
            .unwrap();

        assert_eq!(
            started.phase(),
            DisposableTemplateGenerationPhase::CreateStarted
        );
        assert_eq!(
            fs::read(root.store_directory().join(TEMPLATE_INPUT_DOCUMENT)).unwrap(),
            expected
        );
        assert!(
            !root
                .store_directory()
                .join(STAGED_TEMPLATE_INPUT_DOCUMENT)
                .exists()
        );
    }

    #[test]
    fn candidate_reports_state_root_rebind_during_command() {
        let root = TempRoot::new("template-input-rebind");
        let mut store =
            UnixPersonalWorkerStore::open_or_create_disposable_template_generation(root.path())
                .unwrap();
        let initial = initial();
        store
            .create_disposable_template_generation(&initial)
            .unwrap();
        let authorize =
            reconcile_disposable_template_generation(&initial, &absent_observation(&initial));
        let authorized = store
            .persist_confirmed_disposable_template_generation(
                authorize,
                absent_observation(&initial),
            )
            .unwrap();
        let candidate =
            reconcile_disposable_template_generation(&authorized, &absent_observation(&authorized));
        let moved = root.path().with_extension("held");
        let result = store.execute_confirmed_disposable_template_candidate(
            candidate,
            root.path(),
            b"exact template bytes\n",
            |current| Ok::<_, ()>((absent_observation(current), ())),
            |_started, ()| {
                fs::rename(root.path(), &moved).unwrap();
                fs::create_dir(root.path()).unwrap();
                fs::set_permissions(root.path(), fs::Permissions::from_mode(0o750)).unwrap();
                Ok::<_, ()>(())
            },
        );
        fs::remove_dir(root.path()).unwrap();
        fs::rename(&moved, root.path()).unwrap();

        assert_eq!(
            result.unwrap_err().kind(),
            PersonalWorkerStoreErrorKind::RevisionConflict
        );
    }

    #[test]
    fn restart_recovers_exact_stage_and_other_mutators_refuse_unsettled_stage() {
        let root = TempRoot::new("recovery");
        let mut store =
            UnixPersonalWorkerStore::open_or_create_disposable_template_generation(root.path())
                .unwrap();
        let initial = initial();
        store
            .create_disposable_template_generation(&initial)
            .unwrap();
        let successor = initial
            .transition(
                initial.revision(),
                DisposableTemplateGenerationAction::AuthorizeCreate,
                None,
            )
            .unwrap();
        write_private(
            &root.store_directory().join(STAGED_GENERATION_DOCUMENT),
            &encode_disposable_template_generation(&successor).unwrap(),
        );

        assert_eq!(
            UnixPersonalWorkerStore::open_or_create(root.path())
                .unwrap_err()
                .kind(),
            PersonalWorkerStoreErrorKind::RevisionConflict
        );
        assert_eq!(
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path())
                .unwrap_err()
                .kind(),
            crate::disposable_attempt_catalog::DisposableAttemptCatalogErrorKind::Conflict
        );
        let recovered =
            UnixPersonalWorkerStore::open_or_create_disposable_template_generation(root.path())
                .unwrap();
        assert_eq!(
            recovered.load_disposable_template_generation().unwrap(),
            Some(successor)
        );
        assert!(
            !root
                .store_directory()
                .join(STAGED_GENERATION_DOCUMENT)
                .exists()
        );
    }
}
