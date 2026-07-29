use std::fs::File;
use std::io::{Read as _, Write as _};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::Path;

use rustix::fs::{self, AtFlags, FileType, FlockOperation, Mode, OFlags, RenameFlags};
use rustix::io::Errno;

use crate::personal_worker_store::{
    MAX_PERSONAL_WORKER_STORE_BYTES, PersonalWorkerStore, PersonalWorkerStoreDocument,
    PersonalWorkerStoreError, PersonalWorkerStoreErrorKind,
    PersonalWorkerStoreInitializationDisposition, PersonalWorkerStoreInitializationReceipt,
    PersonalWorkerStoreRecovery, PersonalWorkerStoreRecoveryDisposition,
    PersonalWorkerStoreRevision, PersonalWorkerStoreWriteDisposition,
    PersonalWorkerStoreWriteReceipt, decode_personal_worker_store_document,
    encode_personal_worker_store_document,
};

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const EXISTING_FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const EXISTING_LOCK_FLAGS: OFlags = OFlags::RDWR.union(OFlags::NOFOLLOW).union(OFlags::CLOEXEC);
const NEW_FILE_FLAGS: OFlags = OFlags::WRONLY
    .union(OFlags::CREATE)
    .union(OFlags::EXCL)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const NEW_LOCK_FLAGS: OFlags = EXISTING_LOCK_FLAGS
    .union(OFlags::CREATE)
    .union(OFlags::EXCL);
const MANAGED_DIRECTORY_MODE: Mode = Mode::RUSR
    .union(Mode::WUSR)
    .union(Mode::XUSR)
    .union(Mode::RGRP)
    .union(Mode::XGRP);
const PRIVATE_FILE_MODE: Mode = Mode::RUSR.union(Mode::WUSR);
const STORE_DIRECTORY: &str = "personal-worker";
const STORE_LOCK_FILE: &str = "store.lock";
const CURRENT_DOCUMENT: &str = "current.json";
const STAGED_DOCUMENT: &str = ".next.json";

#[derive(Debug)]
pub struct UnixPersonalWorkerStore {
    _root: OwnedFd,
    directory: OwnedFd,
    owner: (u32, u32),
}

impl UnixPersonalWorkerStore {
    pub fn open_or_create(
        root_path: impl AsRef<Path>,
    ) -> Result<(Self, PersonalWorkerStoreRecovery), PersonalWorkerStoreError> {
        let root = fs::open(root_path.as_ref(), DIRECTORY_FLAGS, Mode::empty())
            .map_err(map_root_open_error)?;
        let root_stat = inspect_directory(&root, "personal worker state root", None)?;
        let owner = (root_stat.st_uid, root_stat.st_gid);
        let directory = ensure_store_directory(&root, owner)?;
        ensure_lock_file(&directory, owner)?;
        let mut store = Self {
            _root: root,
            directory,
            owner,
        };
        let recovery = store.recover()?;
        Ok((store, recovery))
    }

    /// Open one already-created personal-worker store without taking the writer lock or recovering.
    ///
    /// This constructor never creates the managed directory, lock, current document, or staged
    /// document. Callers receive only the existing canonical `current.json` view through `load`.
    pub fn open_existing_read_only(
        root_path: impl AsRef<Path>,
    ) -> Result<Self, PersonalWorkerStoreError> {
        let root = fs::open(root_path.as_ref(), DIRECTORY_FLAGS, Mode::empty())
            .map_err(map_root_open_error)?;
        let root_stat = inspect_directory(&root, "personal worker state root", None)?;
        let owner = (root_stat.st_uid, root_stat.st_gid);
        let directory = fs::openat(&root, STORE_DIRECTORY, DIRECTORY_FLAGS, Mode::empty())
            .map_err(map_existing_store_directory_open_error)?;
        inspect_directory(&directory, "personal worker store directory", Some(owner))?;
        Ok(Self {
            _root: root,
            directory,
            owner,
        })
    }

    /// Create the exact initial document only when no current or staged state exists.
    ///
    /// The writer lock is acquired before inspecting durable state. Any valid staged
    /// recovery state is reported without publication or cleanup, and an existing current
    /// document is returned as an idempotent result without changing its bytes.
    pub fn initialize_if_clean(
        root_path: impl AsRef<Path>,
        document: &PersonalWorkerStoreDocument,
    ) -> Result<PersonalWorkerStoreInitializationReceipt, PersonalWorkerStoreError> {
        if document.revision().get() != 1 || !document.history().is_empty() {
            return Err(store_error(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "initial personal worker state must use revision one without history",
            ));
        }
        let root = fs::open(root_path.as_ref(), DIRECTORY_FLAGS, Mode::empty())
            .map_err(map_root_open_error)?;
        let root_stat = inspect_directory(&root, "personal worker state root", None)?;
        let owner = (root_stat.st_uid, root_stat.st_gid);
        let directory = ensure_store_directory(&root, owner)?;
        ensure_lock_file(&directory, owner)?;
        let store = Self {
            _root: root,
            directory,
            owner,
        };
        let _lock = store.acquire_mutation_lock()?;
        match store.recovery_plan()? {
            StoreRecoveryPlan::Clean {
                revision: Some(revision),
            } => Ok(PersonalWorkerStoreInitializationReceipt::new(
                PersonalWorkerStoreInitializationDisposition::AlreadyExists,
                Some(revision),
                0,
            )),
            StoreRecoveryPlan::Clean { revision: None } => {
                let bytes_written = encode_personal_worker_store_document(document)?.len();
                let mut staged = store.stage_document(document)?;
                store.publish_staged(&mut staged, true)?;
                Ok(PersonalWorkerStoreInitializationReceipt::new(
                    PersonalWorkerStoreInitializationDisposition::Created,
                    Some(document.revision()),
                    bytes_written,
                ))
            }
            StoreRecoveryPlan::PublishStaged { revision, .. }
            | StoreRecoveryPlan::RemoveStaleStaged { revision } => {
                Ok(PersonalWorkerStoreInitializationReceipt::new(
                    PersonalWorkerStoreInitializationDisposition::RecoveryRequired,
                    Some(revision),
                    0,
                ))
            }
        }
    }

    fn acquire_mutation_lock(&self) -> Result<StoreMutationLock, PersonalWorkerStoreError> {
        let lock = fs::openat(
            &self.directory,
            STORE_LOCK_FILE,
            EXISTING_LOCK_FLAGS,
            Mode::empty(),
        )
        .map_err(map_lock_open_error)?;
        inspect_private_file(&lock, self.owner, "personal worker store lock", Some(0))?;
        match fs::flock(&lock, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(StoreMutationLock { _lock: lock }),
            Err(Errno::AGAIN) => Err(store_error(
                PersonalWorkerStoreErrorKind::Busy,
                "another personal worker store mutation holds the writer lock",
            )),
            Err(_) => Err(store_error(
                PersonalWorkerStoreErrorKind::Io,
                "could not acquire the personal worker store writer lock",
            )),
        }
    }

    fn load_named(
        &self,
        name: &str,
    ) -> Result<Option<PersonalWorkerStoreDocument>, PersonalWorkerStoreError> {
        inspect_directory(
            &self.directory,
            "personal worker store directory",
            Some(self.owner),
        )?;
        let file = match fs::openat(&self.directory, name, EXISTING_FILE_FLAGS, Mode::empty()) {
            Ok(file) => file,
            Err(Errno::NOENT) => return Ok(None),
            Err(error) => return Err(map_document_open_error(error)),
        };
        inspect_private_file(&file, self.owner, "personal worker state document", None)?;
        let mut bytes = Vec::new();
        File::from(file)
            .take((MAX_PERSONAL_WORKER_STORE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| {
                store_error(
                    PersonalWorkerStoreErrorKind::Io,
                    "could not read the personal worker state document",
                )
            })?;
        if bytes.len() > MAX_PERSONAL_WORKER_STORE_BYTES {
            return Err(PersonalWorkerStoreError::corrupt_state());
        }
        decode_personal_worker_store_document(&bytes).map(Some)
    }

    fn stage_document(
        &self,
        document: &PersonalWorkerStoreDocument,
    ) -> Result<StagedDocument<'_>, PersonalWorkerStoreError> {
        let encoded = encode_personal_worker_store_document(document)?;
        let file = fs::openat(
            &self.directory,
            STAGED_DOCUMENT,
            NEW_FILE_FLAGS,
            PRIVATE_FILE_MODE,
        )
        .map_err(map_stage_create_error)?;
        let mut staged = StagedDocument {
            directory: self.directory.as_fd(),
            file: Some(file),
            armed: true,
        };
        let opened = staged.file.as_ref().expect("staged file is present");
        fs::fchmod(opened, PRIVATE_FILE_MODE).map_err(|_| {
            store_error(
                PersonalWorkerStoreErrorKind::Io,
                "could not set private staged-state permissions",
            )
        })?;
        inspect_private_file(
            opened,
            self.owner,
            "staged personal worker document",
            Some(0),
        )?;
        let mut file = File::from(staged.file.take().expect("staged file is present"));
        file.write_all(&encoded).map_err(|_| {
            store_error(
                PersonalWorkerStoreErrorKind::Io,
                "could not write the staged personal worker document",
            )
        })?;
        file.sync_all().map_err(|_| {
            store_error(
                PersonalWorkerStoreErrorKind::Io,
                "could not synchronize the staged personal worker document",
            )
        })?;
        inspect_private_file(
            file.as_fd(),
            self.owner,
            "staged personal worker document",
            Some(encoded.len()),
        )?;
        staged.file = Some(file.into());
        Ok(staged)
    }

    fn publish_staged(
        &self,
        staged: &mut StagedDocument<'_>,
        no_replace: bool,
    ) -> Result<(), PersonalWorkerStoreError> {
        let flags = if no_replace {
            RenameFlags::NOREPLACE
        } else {
            RenameFlags::empty()
        };
        fs::renameat_with(
            &self.directory,
            STAGED_DOCUMENT,
            &self.directory,
            CURRENT_DOCUMENT,
            flags,
        )
        .map_err(|error| map_publish_error(error, no_replace))?;
        staged.disarm();
        synchronize_directory(&self.directory, "personal worker store directory")
    }

    fn remove_staged(&self) -> Result<(), PersonalWorkerStoreError> {
        match fs::unlinkat(&self.directory, STAGED_DOCUMENT, AtFlags::empty()) {
            Ok(()) => synchronize_directory(&self.directory, "personal worker store directory"),
            Err(Errno::NOENT) => Ok(()),
            Err(_) => Err(store_error(
                PersonalWorkerStoreErrorKind::Io,
                "could not remove stale staged personal worker state",
            )),
        }
    }

    fn recovery_plan(&self) -> Result<StoreRecoveryPlan, PersonalWorkerStoreError> {
        let Some(staged) = self.load_named(STAGED_DOCUMENT)? else {
            return Ok(StoreRecoveryPlan::Clean {
                revision: self
                    .load_named(CURRENT_DOCUMENT)?
                    .map(|document| document.revision()),
            });
        };
        let current = self.load_named(CURRENT_DOCUMENT)?;
        match current {
            None => {
                if staged.revision().get() != 1 || !staged.history().is_empty() {
                    return Err(PersonalWorkerStoreError::corrupt_state());
                }
                Ok(StoreRecoveryPlan::PublishStaged {
                    revision: staged.revision(),
                    no_replace: true,
                })
            }
            Some(current) if staged.revision() <= current.revision() => {
                Ok(StoreRecoveryPlan::RemoveStaleStaged {
                    revision: current.revision(),
                })
            }
            Some(current) => {
                staged
                    .validate_successor_of(&current)
                    .map_err(|_| PersonalWorkerStoreError::corrupt_state())?;
                Ok(StoreRecoveryPlan::PublishStaged {
                    revision: staged.revision(),
                    no_replace: false,
                })
            }
        }
    }

    fn recover_locked(&mut self) -> Result<PersonalWorkerStoreRecovery, PersonalWorkerStoreError> {
        match self.recovery_plan()? {
            StoreRecoveryPlan::Clean { revision } => Ok(PersonalWorkerStoreRecovery::new(
                PersonalWorkerStoreRecoveryDisposition::Clean,
                revision,
            )),
            StoreRecoveryPlan::PublishStaged {
                revision,
                no_replace,
            } => {
                let mut staged_guard = StagedDocument::existing(self.directory.as_fd());
                self.publish_staged(&mut staged_guard, no_replace)?;
                Ok(PersonalWorkerStoreRecovery::new(
                    PersonalWorkerStoreRecoveryDisposition::PublishedStaged,
                    Some(revision),
                ))
            }
            StoreRecoveryPlan::RemoveStaleStaged { revision } => {
                self.remove_staged()?;
                Ok(PersonalWorkerStoreRecovery::new(
                    PersonalWorkerStoreRecoveryDisposition::RemovedStaleStaged,
                    Some(revision),
                ))
            }
        }
    }
}

impl PersonalWorkerStore for UnixPersonalWorkerStore {
    fn load(&self) -> Result<Option<PersonalWorkerStoreDocument>, PersonalWorkerStoreError> {
        self.load_named(CURRENT_DOCUMENT)
    }

    fn create(
        &mut self,
        document: &PersonalWorkerStoreDocument,
    ) -> Result<PersonalWorkerStoreWriteReceipt, PersonalWorkerStoreError> {
        if document.revision().get() != 1 || !document.history().is_empty() {
            return Err(store_error(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "initial personal worker state must use revision one without history",
            ));
        }
        let _lock = self.acquire_mutation_lock()?;
        self.recover_locked()?;
        if self.load_named(CURRENT_DOCUMENT)?.is_some() {
            return Err(store_error(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "personal worker state already exists",
            ));
        }
        let bytes_written = encode_personal_worker_store_document(document)?.len();
        let mut staged = self.stage_document(document)?;
        self.publish_staged(&mut staged, true)?;
        Ok(PersonalWorkerStoreWriteReceipt::new(
            PersonalWorkerStoreWriteDisposition::Created,
            document.revision(),
            bytes_written,
        ))
    }

    fn replace_if_revision(
        &mut self,
        expected_revision: PersonalWorkerStoreRevision,
        document: &PersonalWorkerStoreDocument,
    ) -> Result<PersonalWorkerStoreWriteReceipt, PersonalWorkerStoreError> {
        let _lock = self.acquire_mutation_lock()?;
        self.recover_locked()?;
        let current = self.load_named(CURRENT_DOCUMENT)?.ok_or_else(|| {
            store_error(
                PersonalWorkerStoreErrorKind::Missing,
                "personal worker state does not exist",
            )
        })?;
        if current.revision() != expected_revision {
            return Err(store_error(
                PersonalWorkerStoreErrorKind::RevisionConflict,
                "personal worker state revision changed before publication",
            ));
        }
        document.validate_successor_of(&current)?;
        let bytes_written = encode_personal_worker_store_document(document)?.len();
        let mut staged = self.stage_document(document)?;
        self.publish_staged(&mut staged, false)?;
        Ok(PersonalWorkerStoreWriteReceipt::new(
            PersonalWorkerStoreWriteDisposition::Replaced,
            document.revision(),
            bytes_written,
        ))
    }

    fn recover(&mut self) -> Result<PersonalWorkerStoreRecovery, PersonalWorkerStoreError> {
        let _lock = self.acquire_mutation_lock()?;
        self.recover_locked()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoreRecoveryPlan {
    Clean {
        revision: Option<PersonalWorkerStoreRevision>,
    },
    PublishStaged {
        revision: PersonalWorkerStoreRevision,
        no_replace: bool,
    },
    RemoveStaleStaged {
        revision: PersonalWorkerStoreRevision,
    },
}

#[derive(Debug)]
struct StoreMutationLock {
    _lock: OwnedFd,
}

struct StagedDocument<'a> {
    directory: BorrowedFd<'a>,
    file: Option<OwnedFd>,
    armed: bool,
}

impl<'a> StagedDocument<'a> {
    fn existing(directory: BorrowedFd<'a>) -> Self {
        Self {
            directory,
            file: None,
            armed: false,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StagedDocument<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::unlinkat(self.directory, STAGED_DOCUMENT, AtFlags::empty());
        }
    }
}

fn ensure_store_directory(
    root: &OwnedFd,
    owner: (u32, u32),
) -> Result<OwnedFd, PersonalWorkerStoreError> {
    match fs::openat(root, STORE_DIRECTORY, DIRECTORY_FLAGS, Mode::empty()) {
        Ok(directory) => {
            inspect_directory(&directory, "personal worker store directory", Some(owner))?;
            Ok(directory)
        }
        Err(Errno::NOENT) => {
            let created = match fs::mkdirat(root, STORE_DIRECTORY, MANAGED_DIRECTORY_MODE) {
                Ok(()) => true,
                Err(Errno::EXIST) => false,
                Err(_) => {
                    return Err(store_error(
                        PersonalWorkerStoreErrorKind::Io,
                        "could not create the personal worker store directory",
                    ));
                }
            };
            let directory = fs::openat(root, STORE_DIRECTORY, DIRECTORY_FLAGS, Mode::empty())
                .map_err(map_store_directory_open_error)?;
            if created {
                fs::fchmod(&directory, MANAGED_DIRECTORY_MODE).map_err(|_| {
                    store_error(
                        PersonalWorkerStoreErrorKind::Io,
                        "could not set personal worker store directory permissions",
                    )
                })?;
            }
            inspect_directory(&directory, "personal worker store directory", Some(owner))?;
            if created {
                synchronize_directory(root, "personal worker state root")?;
            }
            Ok(directory)
        }
        Err(error) => Err(map_store_directory_open_error(error)),
    }
}

fn ensure_lock_file(
    directory: &OwnedFd,
    owner: (u32, u32),
) -> Result<(), PersonalWorkerStoreError> {
    match fs::openat(
        directory,
        STORE_LOCK_FILE,
        NEW_LOCK_FLAGS,
        PRIVATE_FILE_MODE,
    ) {
        Ok(lock) => {
            let mut created = CreatedLockFile {
                directory: directory.as_fd(),
                armed: true,
            };
            fs::fchmod(&lock, PRIVATE_FILE_MODE).map_err(|_| {
                store_error(
                    PersonalWorkerStoreErrorKind::Io,
                    "could not set personal worker store lock permissions",
                )
            })?;
            inspect_private_file(&lock, owner, "personal worker store lock", Some(0))?;
            fs::fsync(&lock).map_err(|_| {
                store_error(
                    PersonalWorkerStoreErrorKind::Io,
                    "could not synchronize the personal worker store lock",
                )
            })?;
            synchronize_directory(directory, "personal worker store directory")?;
            created.armed = false;
            Ok(())
        }
        Err(Errno::EXIST) => {
            let lock = fs::openat(
                directory,
                STORE_LOCK_FILE,
                EXISTING_LOCK_FLAGS,
                Mode::empty(),
            )
            .map_err(map_lock_open_error)?;
            inspect_private_file(&lock, owner, "personal worker store lock", Some(0))
        }
        Err(error) => Err(map_lock_open_error(error)),
    }
}

struct CreatedLockFile<'a> {
    directory: BorrowedFd<'a>,
    armed: bool,
}

impl Drop for CreatedLockFile<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::unlinkat(self.directory, STORE_LOCK_FILE, AtFlags::empty());
        }
    }
}

fn inspect_directory(
    directory: impl AsFd,
    subject: &str,
    expected_owner: Option<(u32, u32)>,
) -> Result<rustix::fs::Stat, PersonalWorkerStoreError> {
    let stat = fs::fstat(directory.as_fd()).map_err(|_| {
        store_error(
            PersonalWorkerStoreErrorKind::Io,
            "could not inspect a personal worker state directory",
        )
    })?;
    if !FileType::from_raw_mode(stat.st_mode).is_dir() {
        return Err(store_error(
            PersonalWorkerStoreErrorKind::UnsafeFilesystem,
            "personal worker state path is not a directory",
        ));
    }
    if stat.st_mode & 0o7777 != 0o750 {
        return Err(store_error(
            PersonalWorkerStoreErrorKind::UnsafeFilesystem,
            "personal worker state directory does not have mode 0750",
        ));
    }
    if expected_owner.is_some_and(|owner| owner != (stat.st_uid, stat.st_gid)) {
        return Err(store_error(
            PersonalWorkerStoreErrorKind::UnsafeFilesystem,
            "personal worker state directory has an unexpected owner or group",
        ));
    }
    let _ = subject;
    Ok(stat)
}

fn inspect_private_file(
    file: impl AsFd,
    owner: (u32, u32),
    subject: &str,
    expected_size: Option<usize>,
) -> Result<(), PersonalWorkerStoreError> {
    let stat = fs::fstat(file.as_fd()).map_err(|_| {
        store_error(
            PersonalWorkerStoreErrorKind::Io,
            "could not inspect a personal worker state file",
        )
    })?;
    if !FileType::from_raw_mode(stat.st_mode).is_file() {
        return Err(store_error(
            PersonalWorkerStoreErrorKind::UnsafeFilesystem,
            "personal worker state object is not a regular file",
        ));
    }
    if stat.st_nlink != 1 {
        return Err(store_error(
            PersonalWorkerStoreErrorKind::UnsafeFilesystem,
            "personal worker state file has multiple hard links",
        ));
    }
    if stat.st_mode & 0o7777 != 0o600 {
        return Err(store_error(
            PersonalWorkerStoreErrorKind::UnsafeFilesystem,
            "personal worker state file does not have mode 0600",
        ));
    }
    if owner != (stat.st_uid, stat.st_gid) {
        return Err(store_error(
            PersonalWorkerStoreErrorKind::UnsafeFilesystem,
            "personal worker state file has an unexpected owner or group",
        ));
    }
    if expected_size.is_some_and(|expected| {
        stat.st_size < 0 || u64::try_from(expected).ok() != Some(stat.st_size as u64)
    }) {
        return Err(PersonalWorkerStoreError::corrupt_state());
    }
    let _ = subject;
    Ok(())
}

fn synchronize_directory(
    directory: impl AsFd,
    _subject: &str,
) -> Result<(), PersonalWorkerStoreError> {
    fs::fsync(directory.as_fd()).map_err(|_| {
        store_error(
            PersonalWorkerStoreErrorKind::Io,
            "could not synchronize a personal worker state directory",
        )
    })
}

fn store_error(
    kind: PersonalWorkerStoreErrorKind,
    message: &'static str,
) -> PersonalWorkerStoreError {
    PersonalWorkerStoreError::new(kind, message)
}

fn map_root_open_error(error: Errno) -> PersonalWorkerStoreError {
    match error {
        Errno::LOOP | Errno::NOTDIR => store_error(
            PersonalWorkerStoreErrorKind::UnsafeFilesystem,
            "personal worker state root is symlinked or is not a directory",
        ),
        Errno::NOENT => store_error(
            PersonalWorkerStoreErrorKind::Missing,
            "personal worker state root does not exist",
        ),
        _ => store_error(
            PersonalWorkerStoreErrorKind::Io,
            "could not open the personal worker state root",
        ),
    }
}

fn map_store_directory_open_error(error: Errno) -> PersonalWorkerStoreError {
    match error {
        Errno::LOOP | Errno::NOTDIR => store_error(
            PersonalWorkerStoreErrorKind::UnsafeFilesystem,
            "personal worker store directory is symlinked or invalid",
        ),
        _ => store_error(
            PersonalWorkerStoreErrorKind::Io,
            "could not open the personal worker store directory",
        ),
    }
}

fn map_existing_store_directory_open_error(error: Errno) -> PersonalWorkerStoreError {
    match error {
        Errno::NOENT => store_error(
            PersonalWorkerStoreErrorKind::Missing,
            "personal worker store directory does not exist",
        ),
        Errno::LOOP | Errno::NOTDIR => store_error(
            PersonalWorkerStoreErrorKind::UnsafeFilesystem,
            "personal worker store directory is symlinked or invalid",
        ),
        _ => store_error(
            PersonalWorkerStoreErrorKind::Io,
            "could not open the personal worker store directory",
        ),
    }
}

fn map_lock_open_error(error: Errno) -> PersonalWorkerStoreError {
    match error {
        Errno::LOOP | Errno::NOTDIR | Errno::ISDIR => store_error(
            PersonalWorkerStoreErrorKind::UnsafeFilesystem,
            "personal worker store lock is symlinked or invalid",
        ),
        _ => store_error(
            PersonalWorkerStoreErrorKind::Io,
            "could not open the personal worker store lock",
        ),
    }
}

fn map_document_open_error(error: Errno) -> PersonalWorkerStoreError {
    match error {
        Errno::LOOP | Errno::NOTDIR | Errno::ISDIR => store_error(
            PersonalWorkerStoreErrorKind::UnsafeFilesystem,
            "personal worker state document is symlinked or invalid",
        ),
        _ => store_error(
            PersonalWorkerStoreErrorKind::Io,
            "could not open the personal worker state document",
        ),
    }
}

fn map_stage_create_error(error: Errno) -> PersonalWorkerStoreError {
    match error {
        Errno::EXIST => store_error(
            PersonalWorkerStoreErrorKind::CorruptState,
            "staged personal worker state already exists after recovery",
        ),
        Errno::LOOP | Errno::NOTDIR | Errno::ISDIR => store_error(
            PersonalWorkerStoreErrorKind::UnsafeFilesystem,
            "staged personal worker state path is unsafe",
        ),
        _ => store_error(
            PersonalWorkerStoreErrorKind::Io,
            "could not create the staged personal worker state document",
        ),
    }
}

fn map_publish_error(error: Errno, no_replace: bool) -> PersonalWorkerStoreError {
    match error {
        Errno::EXIST if no_replace => store_error(
            PersonalWorkerStoreErrorKind::RevisionConflict,
            "personal worker state already exists",
        ),
        Errno::LOOP | Errno::NOTDIR | Errno::ISDIR => store_error(
            PersonalWorkerStoreErrorKind::UnsafeFilesystem,
            "personal worker state publication path is unsafe",
        ),
        _ => store_error(
            PersonalWorkerStoreErrorKind::Io,
            "could not atomically publish the personal worker state document",
        ),
    }
}
