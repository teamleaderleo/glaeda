//! Descriptor-bound, installation-local persistence for the resident-sandbox catalog.
//!
//! The store owns only canonical catalog bytes and the recovery of its own publication protocol.
//! It deliberately has no host observation, Lima/process/guest callback, project-disk, or
//! invocation authority.  When a future transaction needs both stores, it must acquire this
//! resident-sandbox writer lock first and the project-disk lock second; this module never acquires
//! the project-disk lock.

use std::fmt;
use std::fs::File;
use std::io::{Read as _, Write as _};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::Path;

use rustix::fs::{self, FileType, FlockOperation, Mode, OFlags, RenameFlags};
use rustix::io::Errno;

use crate::resident_sandbox_catalog::{
    ResidentSandboxActiveOperation, ResidentSandboxCatalog, ResidentSandboxCatalogError,
    ResidentSandboxCatalogErrorKind, ResidentSandboxCatalogRevision, ResidentSandboxOperationPhase,
    decode_resident_sandbox_catalog, encode_resident_sandbox_catalog,
};

pub const RESIDENT_SANDBOX_STORE_DIRECTORY: &str = "resident-sandbox";
pub const RESIDENT_SANDBOX_STORE_LOCK_FILE: &str = "store.lock";
pub const RESIDENT_SANDBOX_CURRENT_DOCUMENT: &str = "catalog.json";
pub const RESIDENT_SANDBOX_STAGED_DOCUMENT: &str = ".catalog.next.json";

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const EXISTING_DOCUMENT_FLAGS: OFlags = OFlags::RDWR.union(OFlags::NOFOLLOW).union(OFlags::CLOEXEC);
const EXISTING_LOCK_FLAGS: OFlags = OFlags::RDWR.union(OFlags::NOFOLLOW).union(OFlags::CLOEXEC);
const NEW_FILE_FLAGS: OFlags = OFlags::RDWR
    .union(OFlags::CREATE)
    .union(OFlags::EXCL)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const NEW_LOCK_FLAGS: OFlags = EXISTING_LOCK_FLAGS
    .union(OFlags::CREATE)
    .union(OFlags::EXCL);
const PRIVATE_DIRECTORY_MODE: Mode = Mode::RUSR.union(Mode::WUSR).union(Mode::XUSR);
const PRIVATE_FILE_MODE: Mode = Mode::RUSR.union(Mode::WUSR);

/// Errors from the resident-sandbox persistence boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentSandboxStoreErrorKind {
    Busy,
    Missing,
    AlreadyExists,
    Conflict,
    RecoveryRequired,
    CorruptState,
    InvalidSuccessor,
    LimitExceeded,
    UnsupportedVersion,
    NonCanonical,
    Io,
    UnsafeFilesystem,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResidentSandboxStoreError {
    kind: ResidentSandboxStoreErrorKind,
    message: &'static str,
}

impl ResidentSandboxStoreError {
    #[must_use]
    pub const fn kind(self) -> ResidentSandboxStoreErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        match self.kind {
            ResidentSandboxStoreErrorKind::Busy => "busy",
            ResidentSandboxStoreErrorKind::Missing => "missing",
            ResidentSandboxStoreErrorKind::AlreadyExists => "already_exists",
            ResidentSandboxStoreErrorKind::Conflict => "conflict",
            ResidentSandboxStoreErrorKind::RecoveryRequired => "recovery_required",
            ResidentSandboxStoreErrorKind::CorruptState => "corrupt_state",
            ResidentSandboxStoreErrorKind::InvalidSuccessor => "invalid_successor",
            ResidentSandboxStoreErrorKind::LimitExceeded => "limit_exceeded",
            ResidentSandboxStoreErrorKind::UnsupportedVersion => "unsupported_version",
            ResidentSandboxStoreErrorKind::NonCanonical => "noncanonical",
            ResidentSandboxStoreErrorKind::Io => "io",
            ResidentSandboxStoreErrorKind::UnsafeFilesystem => "unsafe_filesystem",
        }
    }
}

impl fmt::Display for ResidentSandboxStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ResidentSandboxStoreError {}

impl From<ResidentSandboxCatalogError> for ResidentSandboxStoreError {
    fn from(error: ResidentSandboxCatalogError) -> Self {
        let kind = match error.kind() {
            ResidentSandboxCatalogErrorKind::Missing => ResidentSandboxStoreErrorKind::Missing,
            ResidentSandboxCatalogErrorKind::Conflict => ResidentSandboxStoreErrorKind::Conflict,
            ResidentSandboxCatalogErrorKind::InvalidSuccessor => {
                ResidentSandboxStoreErrorKind::InvalidSuccessor
            }
            ResidentSandboxCatalogErrorKind::LimitExceeded => {
                ResidentSandboxStoreErrorKind::LimitExceeded
            }
            ResidentSandboxCatalogErrorKind::UnsupportedVersion => {
                ResidentSandboxStoreErrorKind::UnsupportedVersion
            }
            ResidentSandboxCatalogErrorKind::NonCanonical => {
                ResidentSandboxStoreErrorKind::NonCanonical
            }
            ResidentSandboxCatalogErrorKind::InvalidInput
            | ResidentSandboxCatalogErrorKind::Duplicate
            | ResidentSandboxCatalogErrorKind::CorruptState => {
                ResidentSandboxStoreErrorKind::CorruptState
            }
        };
        Self {
            kind,
            message: "resident-sandbox catalog document is invalid or cannot be persisted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentSandboxCatalogWriteDisposition {
    Created,
    Replaced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResidentSandboxCatalogWriteReceipt {
    disposition: ResidentSandboxCatalogWriteDisposition,
    revision: ResidentSandboxCatalogRevision,
}

impl ResidentSandboxCatalogWriteReceipt {
    #[must_use]
    pub const fn disposition(self) -> ResidentSandboxCatalogWriteDisposition {
        self.disposition
    }

    #[must_use]
    pub const fn revision(self) -> ResidentSandboxCatalogRevision {
        self.revision
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentSandboxRecoveryDisposition {
    Clean,
    PublishedStaged,
    RemovedStaleStaged,
    DiscardedStartedStaged,
}

/// Persistence contract for a resident-sandbox catalog.
pub trait ResidentSandboxCatalogStore {
    fn load(&self) -> Result<Option<ResidentSandboxCatalog>, ResidentSandboxStoreError>;

    fn create(
        &mut self,
        catalog: &ResidentSandboxCatalog,
    ) -> Result<ResidentSandboxCatalogWriteReceipt, ResidentSandboxStoreError>;

    fn replace_if_revision(
        &mut self,
        expected_revision: ResidentSandboxCatalogRevision,
        catalog: &ResidentSandboxCatalog,
    ) -> Result<ResidentSandboxCatalogWriteReceipt, ResidentSandboxStoreError>;

    fn recover(&mut self) -> Result<ResidentSandboxRecoveryDisposition, ResidentSandboxStoreError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FilesystemIdentity {
    device: fs::Dev,
    inode: u64,
    uid: u32,
    gid: u32,
}

impl FilesystemIdentity {
    fn from_stat(stat: &rustix::fs::Stat) -> Self {
        Self {
            device: stat.st_dev,
            inode: stat.st_ino,
            uid: stat.st_uid,
            gid: stat.st_gid,
        }
    }

    fn matches(self, stat: &rustix::fs::Stat) -> bool {
        self == Self::from_stat(stat)
    }
}

/// Durable resident-sandbox catalog owner bound to an installation directory descriptor.
///
/// The caller supplies the installation-local parent directory. The store creates or opens the
/// private `resident-sandbox` child and retains both descriptors for the lifetime of the store.
/// All mutations take the nonblocking exclusive resident-sandbox writer lock. A project-disk lock
/// is intentionally never acquired here; paired transactions must use the order
/// resident-sandbox -> project-disk.
#[derive(Debug)]
pub struct UnixResidentSandboxCatalogStore {
    root: OwnedFd,
    directory: OwnedFd,
    owner: (u32, u32),
    root_identity: FilesystemIdentity,
    directory_identity: FilesystemIdentity,
}

/// Short compatibility name for callers that refer to the durable owner without the catalog
/// qualifier. It carries exactly the same authority boundary.
pub type UnixResidentSandboxStore = UnixResidentSandboxCatalogStore;

impl UnixResidentSandboxCatalogStore {
    /// Open or create the private resident-sandbox store and recover only exact safe stages.
    pub fn open_or_create(
        root_path: impl AsRef<Path>,
    ) -> Result<(Self, ResidentSandboxRecoveryDisposition), ResidentSandboxStoreError> {
        let root = fs::open(root_path.as_ref(), DIRECTORY_FLAGS, Mode::empty())
            .map_err(map_root_open_error)?;
        let root_stat = inspect_directory(&root, "installation root", None, None)?;
        let owner = (root_stat.st_uid, root_stat.st_gid);
        let directory = ensure_store_directory(&root, owner)?;
        let directory_stat = inspect_directory(
            &directory,
            "resident-sandbox store directory",
            Some(owner),
            Some(PRIVATE_DIRECTORY_MODE),
        )?;
        ensure_lock_file(&directory, owner)?;
        let store = Self {
            root,
            directory,
            owner,
            root_identity: FilesystemIdentity::from_stat(&root_stat),
            directory_identity: FilesystemIdentity::from_stat(&directory_stat),
        };
        let lock = store.acquire_writer_lock()?;
        store.prepare_locked(&lock)?;
        let recovery = store.recover_locked(&lock)?;
        lock.validate()?;
        drop(lock);
        Ok((store, recovery))
    }

    /// Open an existing store without creating its managed directory or lock file.
    pub fn open_existing(root_path: impl AsRef<Path>) -> Result<Self, ResidentSandboxStoreError> {
        let root = fs::open(root_path.as_ref(), DIRECTORY_FLAGS, Mode::empty())
            .map_err(map_root_open_error)?;
        let root_stat = inspect_directory(&root, "installation root", None, None)?;
        let owner = (root_stat.st_uid, root_stat.st_gid);
        let directory = fs::openat(
            &root,
            RESIDENT_SANDBOX_STORE_DIRECTORY,
            DIRECTORY_FLAGS,
            Mode::empty(),
        )
        .map_err(map_directory_open_error)?;
        let directory_stat = inspect_directory(
            &directory,
            "resident-sandbox store directory",
            Some(owner),
            Some(PRIVATE_DIRECTORY_MODE),
        )?;
        let lock = fs::openat(
            &directory,
            RESIDENT_SANDBOX_STORE_LOCK_FILE,
            EXISTING_LOCK_FLAGS,
            Mode::empty(),
        )
        .map_err(map_lock_open_error)?;
        inspect_private_file(&lock, owner, "resident-sandbox store lock", Some(0))?;
        Ok(Self {
            root,
            directory,
            owner,
            root_identity: FilesystemIdentity::from_stat(&root_stat),
            directory_identity: FilesystemIdentity::from_stat(&directory_stat),
        })
    }

    /// Return the canonical document, refusing to interpret an unsettled stage.
    pub fn load(&self) -> Result<Option<ResidentSandboxCatalog>, ResidentSandboxStoreError> {
        let lock = self.acquire_writer_lock()?;
        self.prepare_locked(&lock)?;
        if self
            .open_named_document(RESIDENT_SANDBOX_STAGED_DOCUMENT)?
            .is_some()
        {
            return Err(store_error(
                ResidentSandboxStoreErrorKind::RecoveryRequired,
                "resident-sandbox catalog has an unsettled staged document",
            ));
        }
        let current = self
            .open_named_document(RESIDENT_SANDBOX_CURRENT_DOCUMENT)?
            .map(|document| document.catalog);
        lock.validate()?;
        Ok(current)
    }

    /// Run phase-aware staged recovery under the resident-sandbox writer lock.
    pub fn recover(
        &mut self,
    ) -> Result<ResidentSandboxRecoveryDisposition, ResidentSandboxStoreError> {
        let lock = self.acquire_writer_lock()?;
        self.prepare_locked(&lock)?;
        let recovery = self.recover_locked(&lock)?;
        lock.validate()?;
        Ok(recovery)
    }

    pub fn create(
        &mut self,
        catalog: &ResidentSandboxCatalog,
    ) -> Result<ResidentSandboxCatalogWriteReceipt, ResidentSandboxStoreError> {
        if catalog.revision().get() != 1 || !catalog.entries().is_empty() {
            return Err(store_error(
                ResidentSandboxStoreErrorKind::Conflict,
                "initial resident-sandbox catalog must be empty revision one",
            ));
        }
        let lock = self.acquire_writer_lock()?;
        self.prepare_locked(&lock)?;
        self.recover_locked(&lock)?;
        if self
            .open_named_document(RESIDENT_SANDBOX_CURRENT_DOCUMENT)?
            .is_some()
        {
            return Err(store_error(
                ResidentSandboxStoreErrorKind::AlreadyExists,
                "resident-sandbox catalog already exists",
            ));
        }
        let mut staged = self.stage(catalog)?;
        self.publish(&lock, &mut staged, catalog, None, true, false)?;
        Ok(ResidentSandboxCatalogWriteReceipt {
            disposition: ResidentSandboxCatalogWriteDisposition::Created,
            revision: catalog.revision(),
        })
    }

    pub fn replace_if_revision(
        &mut self,
        expected_revision: ResidentSandboxCatalogRevision,
        catalog: &ResidentSandboxCatalog,
    ) -> Result<ResidentSandboxCatalogWriteReceipt, ResidentSandboxStoreError> {
        let lock = self.acquire_writer_lock()?;
        self.prepare_locked(&lock)?;
        self.recover_locked(&lock)?;
        let current = self
            .open_named_document(RESIDENT_SANDBOX_CURRENT_DOCUMENT)?
            .ok_or_else(|| {
                store_error(
                    ResidentSandboxStoreErrorKind::Missing,
                    "resident-sandbox catalog does not exist",
                )
            })?;
        if current.catalog.revision() != expected_revision {
            return Err(store_error(
                ResidentSandboxStoreErrorKind::Conflict,
                "resident-sandbox catalog revision changed before publication",
            ));
        }
        catalog
            .validate_successor_of(&current.catalog)
            .map_err(ResidentSandboxStoreError::from)?;
        let mut staged = self.stage(catalog)?;
        self.publish(&lock, &mut staged, catalog, Some(&current), false, false)?;
        Ok(ResidentSandboxCatalogWriteReceipt {
            disposition: ResidentSandboxCatalogWriteDisposition::Replaced,
            revision: catalog.revision(),
        })
    }

    fn acquire_writer_lock(
        &self,
    ) -> Result<ResidentSandboxWriterLock<'_>, ResidentSandboxStoreError> {
        let marker = fs::openat(
            &self.directory,
            RESIDENT_SANDBOX_STORE_LOCK_FILE,
            EXISTING_LOCK_FLAGS,
            Mode::empty(),
        )
        .map_err(map_lock_open_error)?;
        let stat =
            inspect_private_file(&marker, self.owner, "resident-sandbox store lock", Some(0))?;
        let identity = FilesystemIdentity::from_stat(&stat);
        let directory = fs::openat(
            &self.root,
            RESIDENT_SANDBOX_STORE_DIRECTORY,
            DIRECTORY_FLAGS,
            Mode::empty(),
        )
        .map_err(map_directory_open_error)?;
        let directory_stat = inspect_directory(
            &directory,
            "fresh locked resident-sandbox directory",
            Some(self.owner),
            Some(PRIVATE_DIRECTORY_MODE),
        )?;
        if !self.directory_identity.matches(&directory_stat) {
            return Err(store_error(
                ResidentSandboxStoreErrorKind::UnsafeFilesystem,
                "fresh resident-sandbox writer directory does not match the retained directory",
            ));
        }
        match fs::flock(&directory, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => match fs::flock(&marker, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => {
                    let guard = ResidentSandboxWriterLock {
                        lock: marker,
                        directory,
                        root: self.root.as_fd(),
                        marker_directory: self.directory.as_fd(),
                        owner: self.owner,
                        identity,
                    };
                    guard.validate()?;
                    Ok(guard)
                }
                Err(Errno::AGAIN) => Err(store_error(
                    ResidentSandboxStoreErrorKind::Busy,
                    "another resident-sandbox catalog mutation holds the persistent writer lock",
                )),
                Err(_) => Err(store_error(
                    ResidentSandboxStoreErrorKind::Io,
                    "could not acquire the persistent resident-sandbox writer lock",
                )),
            },
            Err(Errno::AGAIN) => Err(store_error(
                ResidentSandboxStoreErrorKind::Busy,
                "another resident-sandbox catalog mutation holds the writer lock",
            )),
            Err(_) => Err(store_error(
                ResidentSandboxStoreErrorKind::Io,
                "could not acquire the resident-sandbox catalog writer lock",
            )),
        }
    }

    fn prepare_locked(
        &self,
        lock: &ResidentSandboxWriterLock<'_>,
    ) -> Result<(), ResidentSandboxStoreError> {
        self.check_rebind()?;
        lock.validate()?;
        // A prior mkdir/rename can have completed before its parent fsync failed. Synchronize the
        // retained installation parent on every retry before interpreting or publishing state.
        synchronize_installation_root(&self.root)?;
        synchronize_directory(&lock.directory)?;
        self.check_rebind()?;
        lock.validate()
    }

    fn check_rebind(&self) -> Result<(), ResidentSandboxStoreError> {
        let root = fs::fstat(&self.root).map_err(|_| {
            store_error(
                ResidentSandboxStoreErrorKind::Io,
                "could not inspect the held installation root descriptor",
            )
        })?;
        let directory = fs::fstat(&self.directory).map_err(|_| {
            store_error(
                ResidentSandboxStoreErrorKind::Io,
                "could not inspect the held resident-sandbox directory descriptor",
            )
        })?;
        let named_directory = fs::openat(
            &self.root,
            RESIDENT_SANDBOX_STORE_DIRECTORY,
            DIRECTORY_FLAGS,
            Mode::empty(),
        )
        .map_err(map_directory_open_error)?;
        let named_directory = inspect_directory(
            &named_directory,
            "named resident-sandbox store directory",
            Some(self.owner),
            Some(PRIVATE_DIRECTORY_MODE),
        )?;
        if !self.root_identity.matches(&root)
            || !self.directory_identity.matches(&directory)
            || !self.directory_identity.matches(&named_directory)
            || root.st_uid != self.owner.0
            || root.st_gid != self.owner.1
            || directory.st_uid != self.owner.0
            || directory.st_gid != self.owner.1
            || directory.st_mode & 0o7777 != 0o700
        {
            return Err(store_error(
                ResidentSandboxStoreErrorKind::UnsafeFilesystem,
                "resident-sandbox store descriptors no longer match their opening identities",
            ));
        }
        Ok(())
    }

    fn open_named_document(
        &self,
        name: &str,
    ) -> Result<Option<NamedCatalogDocument>, ResidentSandboxStoreError> {
        let file = match fs::openat(
            &self.directory,
            name,
            EXISTING_DOCUMENT_FLAGS,
            Mode::empty(),
        ) {
            Ok(file) => file,
            Err(Errno::NOENT) => return Ok(None),
            Err(error) => return Err(map_document_open_error(error)),
        };
        let before =
            inspect_private_file(&file, self.owner, "resident-sandbox catalog document", None)?;
        let identity = FilesystemIdentity::from_stat(&before);
        let mut bytes = Vec::new();
        let mut file = File::from(file);
        std::io::Read::by_ref(&mut file)
            .take(
                (crate::resident_sandbox_catalog::MAX_RESIDENT_SANDBOX_CATALOG_DOCUMENT_BYTES + 1)
                    as u64,
            )
            .read_to_end(&mut bytes)
            .map_err(|_| {
                store_error(
                    ResidentSandboxStoreErrorKind::Io,
                    "could not read resident-sandbox catalog document",
                )
            })?;
        if bytes.is_empty() && name == RESIDENT_SANDBOX_STAGED_DOCUMENT {
            let after = inspect_private_file(
                file.as_fd(),
                self.owner,
                "resident-sandbox empty staged slot",
                Some(0),
            )?;
            if !identity.matches(&after) {
                return Err(store_error(
                    ResidentSandboxStoreErrorKind::UnsafeFilesystem,
                    "resident-sandbox empty staged slot changed during observation",
                ));
            }
            let named = fs::openat(
                &self.directory,
                name,
                EXISTING_DOCUMENT_FLAGS,
                Mode::empty(),
            )
            .map_err(map_document_open_error)?;
            let named = inspect_private_file(
                &named,
                self.owner,
                "named resident-sandbox empty staged slot",
                None,
            )?;
            if !identity.matches(&named) {
                return Err(store_error(
                    ResidentSandboxStoreErrorKind::UnsafeFilesystem,
                    "resident-sandbox empty staged slot directory entry was rebound",
                ));
            }
            if named.st_size != 0 {
                return Err(store_error(
                    ResidentSandboxStoreErrorKind::CorruptState,
                    "resident-sandbox empty staged slot name is not empty",
                ));
            }
            return Ok(None);
        }
        if bytes.len()
            > crate::resident_sandbox_catalog::MAX_RESIDENT_SANDBOX_CATALOG_DOCUMENT_BYTES
        {
            return Err(store_error(
                ResidentSandboxStoreErrorKind::CorruptState,
                "resident-sandbox catalog document exceeds its bounded size",
            ));
        }
        let catalog =
            decode_resident_sandbox_catalog(&bytes).map_err(ResidentSandboxStoreError::from)?;
        let after = inspect_private_file(
            file.as_fd(),
            self.owner,
            "resident-sandbox catalog document",
            Some(bytes.len()),
        )?;
        if !identity.matches(&after) {
            return Err(store_error(
                ResidentSandboxStoreErrorKind::UnsafeFilesystem,
                "resident-sandbox catalog document changed during observation",
            ));
        }
        let document = NamedCatalogDocument {
            file: file.into(),
            identity,
            catalog,
        };
        self.validate_named_document(name, &document)?;
        Ok(Some(document))
    }

    #[cfg(test)]
    fn load_current(&self) -> Result<Option<ResidentSandboxCatalog>, ResidentSandboxStoreError> {
        self.open_named_document(RESIDENT_SANDBOX_CURRENT_DOCUMENT)
            .map(|document| document.map(|document| document.catalog))
    }

    fn validate_named_document(
        &self,
        name: &str,
        expected: &NamedCatalogDocument,
    ) -> Result<(), ResidentSandboxStoreError> {
        let file = fs::openat(
            &self.directory,
            name,
            EXISTING_DOCUMENT_FLAGS,
            Mode::empty(),
        )
        .map_err(map_document_open_error)?;
        let stat =
            inspect_private_file(&file, self.owner, "resident-sandbox catalog document", None)?;
        if !expected.identity.matches(&stat) {
            return Err(store_error(
                ResidentSandboxStoreErrorKind::UnsafeFilesystem,
                "resident-sandbox catalog directory entry was rebound",
            ));
        }
        let mut file = File::from(file);
        let mut bytes = Vec::new();
        std::io::Read::by_ref(&mut file)
            .take(
                (crate::resident_sandbox_catalog::MAX_RESIDENT_SANDBOX_CATALOG_DOCUMENT_BYTES + 1)
                    as u64,
            )
            .read_to_end(&mut bytes)
            .map_err(|_| {
                store_error(
                    ResidentSandboxStoreErrorKind::Io,
                    "could not revalidate resident-sandbox catalog document",
                )
            })?;
        if bytes.len()
            > crate::resident_sandbox_catalog::MAX_RESIDENT_SANDBOX_CATALOG_DOCUMENT_BYTES
            || decode_resident_sandbox_catalog(&bytes).as_ref() != Ok(&expected.catalog)
        {
            return Err(store_error(
                ResidentSandboxStoreErrorKind::CorruptState,
                "resident-sandbox catalog document changed before publication",
            ));
        }
        let after = inspect_private_file(
            file.as_fd(),
            self.owner,
            "resident-sandbox catalog document",
            Some(bytes.len()),
        )?;
        if !expected.identity.matches(&after) {
            return Err(store_error(
                ResidentSandboxStoreErrorKind::UnsafeFilesystem,
                "resident-sandbox catalog document changed during revalidation",
            ));
        }
        Ok(())
    }

    fn validate_named_identity_and_catalog(
        &self,
        name: &str,
        identity: FilesystemIdentity,
        expected: &ResidentSandboxCatalog,
    ) -> Result<(), ResidentSandboxStoreError> {
        let observed = self.open_named_document(name)?.ok_or_else(|| {
            store_error(
                ResidentSandboxStoreErrorKind::Missing,
                "resident-sandbox catalog document disappeared during publication",
            )
        })?;
        if observed.identity != identity || observed.catalog != *expected {
            return Err(store_error(
                ResidentSandboxStoreErrorKind::UnsafeFilesystem,
                "resident-sandbox catalog publication inode or bytes changed",
            ));
        }
        Ok(())
    }

    fn stage(
        &self,
        catalog: &ResidentSandboxCatalog,
    ) -> Result<StagedDocument<'_>, ResidentSandboxStoreError> {
        let encoded =
            encode_resident_sandbox_catalog(catalog).map_err(ResidentSandboxStoreError::from)?;
        maybe_fault(FaultPoint::StageCreate)?;
        let file = match fs::openat(
            &self.directory,
            RESIDENT_SANDBOX_STAGED_DOCUMENT,
            NEW_FILE_FLAGS,
            PRIVATE_FILE_MODE,
        ) {
            Ok(file) => file,
            Err(Errno::EXIST) => {
                let file = fs::openat(
                    &self.directory,
                    RESIDENT_SANDBOX_STAGED_DOCUMENT,
                    EXISTING_DOCUMENT_FLAGS,
                    Mode::empty(),
                )
                .map_err(map_document_open_error)?;
                inspect_private_file(
                    &file,
                    self.owner,
                    "resident-sandbox reusable staged catalog slot",
                    Some(0),
                )?;
                file
            }
            Err(error) => return Err(map_stage_create_error(error)),
        };
        let mut staged = StagedDocument {
            directory: self.directory.as_fd(),
            file: Some(File::from(file)),
            identity: None,
            owner: self.owner,
            armed: true,
        };
        let opened = staged.file.as_ref().expect("staged file is present");
        fs::fchmod(opened, PRIVATE_FILE_MODE).map_err(|_| {
            store_error(
                ResidentSandboxStoreErrorKind::Io,
                "could not set resident-sandbox staged-file permissions",
            )
        })?;
        let empty_stat = inspect_private_file(
            opened,
            self.owner,
            "resident-sandbox staged catalog",
            Some(0),
        )?;
        staged.identity = Some(FilesystemIdentity::from_stat(&empty_stat));
        maybe_fault(FaultPoint::StageOpen)?;
        let file = staged.file.as_mut().expect("staged file is present");
        maybe_fault(FaultPoint::StageWrite)?;
        if take_fault(FaultPoint::StagePartialWrite) {
            let partial = encoded.len().saturating_div(2).max(1).min(encoded.len());
            file.write_all(&encoded[..partial]).map_err(|_| {
                store_error(
                    ResidentSandboxStoreErrorKind::Io,
                    "could not write partial resident-sandbox staged catalog",
                )
            })?;
            return Err(store_error(
                ResidentSandboxStoreErrorKind::Io,
                "injected partial resident-sandbox staged catalog write failure",
            ));
        }
        file.write_all(&encoded).map_err(|_| {
            store_error(
                ResidentSandboxStoreErrorKind::Io,
                "could not write resident-sandbox staged catalog",
            )
        })?;
        maybe_fault(FaultPoint::StageFileSync)?;
        file.sync_all().map_err(|_| {
            store_error(
                ResidentSandboxStoreErrorKind::Io,
                "could not synchronize resident-sandbox staged catalog",
            )
        })?;
        let written_stat = inspect_private_file(
            file.as_fd(),
            self.owner,
            "resident-sandbox staged catalog",
            Some(encoded.len()),
        )?;
        if !staged
            .identity
            .is_some_and(|identity| identity.matches(&written_stat))
        {
            return Err(store_error(
                ResidentSandboxStoreErrorKind::UnsafeFilesystem,
                "resident-sandbox staged catalog changed during publication preparation",
            ));
        }
        Ok(staged)
    }

    fn validate_existing_stage(
        &self,
        staged: &StagedDocument<'_>,
        expected: &ResidentSandboxCatalog,
    ) -> Result<(), ResidentSandboxStoreError> {
        maybe_fault(FaultPoint::StageReopenValidation)?;
        let observed = self
            .open_named_document(RESIDENT_SANDBOX_STAGED_DOCUMENT)?
            .ok_or_else(|| {
                store_error(
                    ResidentSandboxStoreErrorKind::Missing,
                    "resident-sandbox staged catalog disappeared before publication",
                )
            })?;
        if staged.identity != Some(observed.identity) || observed.catalog != *expected {
            return Err(store_error(
                ResidentSandboxStoreErrorKind::UnsafeFilesystem,
                "resident-sandbox staged catalog directory entry was rebound",
            ));
        }
        let held = staged.file.as_ref().ok_or_else(|| {
            store_error(
                ResidentSandboxStoreErrorKind::CorruptState,
                "resident-sandbox staged catalog descriptor is missing",
            )
        })?;
        let held_stat =
            inspect_private_file(held, self.owner, "resident-sandbox staged catalog", None)?;
        if !observed.identity.matches(&held_stat) {
            return Err(store_error(
                ResidentSandboxStoreErrorKind::UnsafeFilesystem,
                "resident-sandbox staged catalog no longer matches its retained descriptor",
            ));
        }
        fs::fsync(held).map_err(|_| {
            store_error(
                ResidentSandboxStoreErrorKind::Io,
                "could not synchronize retained resident-sandbox staged catalog",
            )
        })?;
        Ok(())
    }

    fn publish(
        &self,
        lock: &ResidentSandboxWriterLock<'_>,
        staged: &mut StagedDocument<'_>,
        expected: &ResidentSandboxCatalog,
        predecessor: Option<&NamedCatalogDocument>,
        no_replace: bool,
        recovery: bool,
    ) -> Result<(), ResidentSandboxStoreError> {
        self.validate_existing_stage(staged, expected)?;
        match predecessor {
            Some(current) => {
                self.validate_named_document(RESIDENT_SANDBOX_CURRENT_DOCUMENT, current)?
            }
            None => {
                if self
                    .open_named_document(RESIDENT_SANDBOX_CURRENT_DOCUMENT)?
                    .is_some()
                {
                    return Err(store_error(
                        ResidentSandboxStoreErrorKind::AlreadyExists,
                        "resident-sandbox canonical catalog appeared before publication",
                    ));
                }
            }
        }
        if recovery {
            maybe_fault(FaultPoint::RecoveryPublication)?;
        }
        maybe_fault(FaultPoint::BeforeRename)?;
        self.check_rebind()?;
        lock.validate()?;
        self.validate_existing_stage(staged, expected)?;
        if let Some(current) = predecessor {
            self.validate_named_document(RESIDENT_SANDBOX_CURRENT_DOCUMENT, current)?;
        }
        run_before_rename_hook();
        let flags = if no_replace {
            RenameFlags::NOREPLACE
        } else {
            RenameFlags::EXCHANGE
        };
        fs::renameat_with(
            &self.directory,
            RESIDENT_SANDBOX_STAGED_DOCUMENT,
            &self.directory,
            RESIDENT_SANDBOX_CURRENT_DOCUMENT,
            flags,
        )
        .map_err(|error| map_publish_error(error, no_replace))?;
        staged.disarm();
        let staged_identity = staged.identity.ok_or_else(|| {
            store_error(
                ResidentSandboxStoreErrorKind::CorruptState,
                "resident-sandbox staged identity disappeared after publication",
            )
        })?;
        self.validate_named_identity_and_catalog(
            RESIDENT_SANDBOX_CURRENT_DOCUMENT,
            staged_identity,
            expected,
        )?;
        if let Some(current) = predecessor {
            self.validate_named_document(RESIDENT_SANDBOX_STAGED_DOCUMENT, current)?;
        }
        maybe_fault(FaultPoint::AfterRenameBeforeParentSync)?;
        maybe_fault(FaultPoint::ParentSync)?;
        // Exchange has made both names truthful, but the predecessor is still durable state until
        // the directory entry is synchronized. Retire only its retained writable descriptor.
        synchronize_directory(&lock.directory)?;
        self.validate_named_identity_and_catalog(
            RESIDENT_SANDBOX_CURRENT_DOCUMENT,
            staged_identity,
            expected,
        )?;
        if let Some(current) = predecessor {
            retire_exact_document(
                &self.directory,
                RESIDENT_SANDBOX_STAGED_DOCUMENT,
                &current.file,
                current.identity,
                self.owner,
            )?;
        }
        synchronize_directory(&lock.directory)?;
        self.validate_named_identity_and_catalog(
            RESIDENT_SANDBOX_CURRENT_DOCUMENT,
            staged_identity,
            expected,
        )?;
        self.check_rebind()?;
        lock.validate()?;
        Ok(())
    }

    fn remove_stage(
        &self,
        lock: &ResidentSandboxWriterLock<'_>,
        staged: &NamedCatalogDocument,
    ) -> Result<(), ResidentSandboxStoreError> {
        maybe_fault(FaultPoint::StaleStageRemoval)?;
        self.check_rebind()?;
        lock.validate()?;
        self.validate_named_document(RESIDENT_SANDBOX_STAGED_DOCUMENT, staged)?;
        synchronize_directory(&lock.directory)?;
        retire_exact_document(
            &self.directory,
            RESIDENT_SANDBOX_STAGED_DOCUMENT,
            &staged.file,
            staged.identity,
            self.owner,
        )?;
        synchronize_directory(&lock.directory)?;
        lock.validate()
    }

    fn recover_locked(
        &self,
        lock: &ResidentSandboxWriterLock<'_>,
    ) -> Result<ResidentSandboxRecoveryDisposition, ResidentSandboxStoreError> {
        let Some(staged_document) = self.open_named_document(RESIDENT_SANDBOX_STAGED_DOCUMENT)?
        else {
            return Ok(ResidentSandboxRecoveryDisposition::Clean);
        };
        let staged = &staged_document.catalog;
        let current_document = self.open_named_document(RESIDENT_SANDBOX_CURRENT_DOCUMENT)?;
        let Some(current_document) = current_document else {
            if *staged == ResidentSandboxCatalog::empty() {
                let mut staged_file = StagedDocument::existing(
                    self.directory.as_fd(),
                    staged_document.file,
                    staged_document.identity,
                    self.owner,
                );
                self.publish(lock, &mut staged_file, staged, None, true, true)?;
                return Ok(ResidentSandboxRecoveryDisposition::PublishedStaged);
            }
            return Err(store_error(
                ResidentSandboxStoreErrorKind::CorruptState,
                "resident-sandbox staged catalog has no canonical predecessor",
            ));
        };
        let current = &current_document.catalog;
        if staged == current {
            self.remove_stage(lock, &staged_document)?;
            return Ok(ResidentSandboxRecoveryDisposition::RemovedStaleStaged);
        }
        // A replacement uses EXCHANGE. If the process stopped after the exchange but before the
        // predecessor descriptor was retired, current is the exact successor and staged is the
        // exact predecessor. Clearing that predecessor is safe cleanup; it cannot authorize a
        // callback or manufacture a new binding.
        if staged.revision().get().checked_add(1) == Some(current.revision().get())
            && current.validate_successor_of(staged).is_ok()
        {
            self.remove_stage(lock, &staged_document)?;
            return Ok(ResidentSandboxRecoveryDisposition::RemovedStaleStaged);
        }
        if current.revision().get().checked_add(1) != Some(staged.revision().get()) {
            return Err(store_error(
                ResidentSandboxStoreErrorKind::CorruptState,
                "resident-sandbox staged catalog revision is not the exact successor",
            ));
        }
        staged
            .validate_successor_of(current)
            .map_err(ResidentSandboxStoreError::from)?;
        match classify_successor_recovery(current, staged) {
            SuccessorRecoveryClass::DiscardUnpublishedStarted => {
                self.remove_stage(lock, &staged_document)?;
                return Ok(ResidentSandboxRecoveryDisposition::DiscardedStartedStaged);
            }
            SuccessorRecoveryClass::ExplicitRecovery => {
                return Err(store_error(
                    ResidentSandboxStoreErrorKind::RecoveryRequired,
                    "resident-sandbox staged catalog requires explicit recovery",
                ));
            }
            SuccessorRecoveryClass::SafePublish => {}
        }
        let mut staged_file = StagedDocument::existing(
            self.directory.as_fd(),
            staged_document.file,
            staged_document.identity,
            self.owner,
        );
        self.publish(
            lock,
            &mut staged_file,
            staged,
            Some(&current_document),
            false,
            true,
        )?;
        Ok(ResidentSandboxRecoveryDisposition::PublishedStaged)
    }
}

impl ResidentSandboxCatalogStore for UnixResidentSandboxCatalogStore {
    fn load(&self) -> Result<Option<ResidentSandboxCatalog>, ResidentSandboxStoreError> {
        Self::load(self)
    }

    fn create(
        &mut self,
        catalog: &ResidentSandboxCatalog,
    ) -> Result<ResidentSandboxCatalogWriteReceipt, ResidentSandboxStoreError> {
        Self::create(self, catalog)
    }

    fn replace_if_revision(
        &mut self,
        expected_revision: ResidentSandboxCatalogRevision,
        catalog: &ResidentSandboxCatalog,
    ) -> Result<ResidentSandboxCatalogWriteReceipt, ResidentSandboxStoreError> {
        Self::replace_if_revision(self, expected_revision, catalog)
    }

    fn recover(&mut self) -> Result<ResidentSandboxRecoveryDisposition, ResidentSandboxStoreError> {
        Self::recover(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SuccessorRecoveryClass {
    SafePublish,
    DiscardUnpublishedStarted,
    ExplicitRecovery,
}

fn classify_successor_recovery(
    current: &ResidentSandboxCatalog,
    staged: &ResidentSandboxCatalog,
) -> SuccessorRecoveryClass {
    // Acceptance is pure allocation; an Authorized checkpoint is the durable pre-callback
    // boundary; a prestart failure clears the operation and is also callback-free. A
    // RecoveryRequired successor only makes already-canonical Started debt more restrictive.
    if staged.entries().len() == current.entries().len() + 1 {
        return SuccessorRecoveryClass::SafePublish;
    }
    let Some((prior, next)) = current
        .entries()
        .iter()
        .zip(staged.entries())
        .find(|(prior, next)| prior != next)
    else {
        return SuccessorRecoveryClass::ExplicitRecovery;
    };
    match (prior.active_operation(), next.active_operation()) {
        (
            ResidentSandboxActiveOperation::None,
            ResidentSandboxActiveOperation::Materialize {
                phase: ResidentSandboxOperationPhase::Authorized,
                ..
            }
            | ResidentSandboxActiveOperation::Start {
                phase: ResidentSandboxOperationPhase::Authorized,
                ..
            }
            | ResidentSandboxActiveOperation::Stop {
                phase: ResidentSandboxOperationPhase::Authorized,
                ..
            },
        )
        | (
            ResidentSandboxActiveOperation::Materialize {
                phase: ResidentSandboxOperationPhase::Authorized,
                ..
            }
            | ResidentSandboxActiveOperation::Start {
                phase: ResidentSandboxOperationPhase::Authorized,
                ..
            }
            | ResidentSandboxActiveOperation::Stop {
                phase: ResidentSandboxOperationPhase::Authorized,
                ..
            },
            ResidentSandboxActiveOperation::None,
        ) => SuccessorRecoveryClass::SafePublish,
        (
            ResidentSandboxActiveOperation::Materialize {
                generation: prior_generation,
                policy_identity: prior_policy,
                phase: ResidentSandboxOperationPhase::Authorized,
            },
            ResidentSandboxActiveOperation::Materialize {
                generation: next_generation,
                policy_identity: next_policy,
                phase: ResidentSandboxOperationPhase::Started,
            },
        )
        | (
            ResidentSandboxActiveOperation::Start {
                generation: prior_generation,
                policy_identity: prior_policy,
                phase: ResidentSandboxOperationPhase::Authorized,
            },
            ResidentSandboxActiveOperation::Start {
                generation: next_generation,
                policy_identity: next_policy,
                phase: ResidentSandboxOperationPhase::Started,
            },
        )
        | (
            ResidentSandboxActiveOperation::Stop {
                generation: prior_generation,
                policy_identity: prior_policy,
                phase: ResidentSandboxOperationPhase::Authorized,
            },
            ResidentSandboxActiveOperation::Stop {
                generation: next_generation,
                policy_identity: next_policy,
                phase: ResidentSandboxOperationPhase::Started,
            },
        ) if prior_generation == next_generation && prior_policy == next_policy => {
            SuccessorRecoveryClass::DiscardUnpublishedStarted
        }
        (
            ResidentSandboxActiveOperation::Materialize {
                generation: prior_generation,
                policy_identity: prior_policy,
                phase: ResidentSandboxOperationPhase::Started,
            },
            ResidentSandboxActiveOperation::Materialize {
                generation: next_generation,
                policy_identity: next_policy,
                phase: ResidentSandboxOperationPhase::RecoveryRequired,
            },
        )
        | (
            ResidentSandboxActiveOperation::Start {
                generation: prior_generation,
                policy_identity: prior_policy,
                phase: ResidentSandboxOperationPhase::Started,
            },
            ResidentSandboxActiveOperation::Start {
                generation: next_generation,
                policy_identity: next_policy,
                phase: ResidentSandboxOperationPhase::RecoveryRequired,
            },
        )
        | (
            ResidentSandboxActiveOperation::Stop {
                generation: prior_generation,
                policy_identity: prior_policy,
                phase: ResidentSandboxOperationPhase::Started,
            },
            ResidentSandboxActiveOperation::Stop {
                generation: next_generation,
                policy_identity: next_policy,
                phase: ResidentSandboxOperationPhase::RecoveryRequired,
            },
        ) if prior_generation == next_generation && prior_policy == next_policy => {
            SuccessorRecoveryClass::SafePublish
        }
        _ => SuccessorRecoveryClass::ExplicitRecovery,
    }
}

fn ensure_store_directory(
    root: &OwnedFd,
    owner: (u32, u32),
) -> Result<OwnedFd, ResidentSandboxStoreError> {
    match fs::openat(
        root,
        RESIDENT_SANDBOX_STORE_DIRECTORY,
        DIRECTORY_FLAGS,
        Mode::empty(),
    ) {
        Ok(directory) => {
            inspect_directory(
                &directory,
                "resident-sandbox store directory",
                Some(owner),
                Some(PRIVATE_DIRECTORY_MODE),
            )?;
            Ok(directory)
        }
        Err(Errno::NOENT) => {
            let created = match fs::mkdirat(
                root,
                RESIDENT_SANDBOX_STORE_DIRECTORY,
                PRIVATE_DIRECTORY_MODE,
            ) {
                Ok(()) => true,
                Err(Errno::EXIST) => false,
                Err(_) => {
                    return Err(store_error(
                        ResidentSandboxStoreErrorKind::Io,
                        "could not create resident-sandbox store directory",
                    ));
                }
            };
            let directory = fs::openat(
                root,
                RESIDENT_SANDBOX_STORE_DIRECTORY,
                DIRECTORY_FLAGS,
                Mode::empty(),
            )
            .map_err(map_directory_open_error)?;
            if created {
                fs::fchmod(&directory, PRIVATE_DIRECTORY_MODE).map_err(|_| {
                    store_error(
                        ResidentSandboxStoreErrorKind::Io,
                        "could not set resident-sandbox store directory permissions",
                    )
                })?;
            }
            inspect_directory(
                &directory,
                "resident-sandbox store directory",
                Some(owner),
                Some(PRIVATE_DIRECTORY_MODE),
            )?;
            if created {
                synchronize_installation_root(root)?;
            }
            Ok(directory)
        }
        Err(error) => Err(map_directory_open_error(error)),
    }
}

fn ensure_lock_file(
    directory: &OwnedFd,
    owner: (u32, u32),
) -> Result<(), ResidentSandboxStoreError> {
    match fs::openat(
        directory,
        RESIDENT_SANDBOX_STORE_LOCK_FILE,
        NEW_LOCK_FLAGS,
        PRIVATE_FILE_MODE,
    ) {
        Ok(lock) => {
            fs::fchmod(&lock, PRIVATE_FILE_MODE).map_err(|_| {
                store_error(
                    ResidentSandboxStoreErrorKind::Io,
                    "could not set resident-sandbox lock permissions",
                )
            })?;
            inspect_private_file(&lock, owner, "resident-sandbox store lock", Some(0))?;
            fs::fsync(&lock).map_err(|_| {
                store_error(
                    ResidentSandboxStoreErrorKind::Io,
                    "could not synchronize resident-sandbox store lock",
                )
            })?;
            synchronize_directory(directory)
        }
        Err(Errno::EXIST) => {
            let lock = fs::openat(
                directory,
                RESIDENT_SANDBOX_STORE_LOCK_FILE,
                EXISTING_LOCK_FLAGS,
                Mode::empty(),
            )
            .map_err(map_lock_open_error)?;
            inspect_private_file(&lock, owner, "resident-sandbox store lock", Some(0)).map(|_| ())
        }
        Err(error) => Err(map_lock_open_error(error)),
    }
}

fn inspect_directory(
    directory: impl AsFd,
    _subject: &str,
    expected_owner: Option<(u32, u32)>,
    expected_mode: Option<Mode>,
) -> Result<rustix::fs::Stat, ResidentSandboxStoreError> {
    let stat = fs::fstat(directory.as_fd()).map_err(|_| {
        store_error(
            ResidentSandboxStoreErrorKind::Io,
            "could not inspect resident-sandbox store directory",
        )
    })?;
    if !FileType::from_raw_mode(stat.st_mode).is_dir() {
        return Err(store_error(
            ResidentSandboxStoreErrorKind::UnsafeFilesystem,
            "resident-sandbox store path is not a directory",
        ));
    }
    if expected_mode.is_some_and(|mode| Mode::from_raw_mode(stat.st_mode) != mode) {
        return Err(store_error(
            ResidentSandboxStoreErrorKind::UnsafeFilesystem,
            "resident-sandbox store directory has an unexpected mode",
        ));
    }
    if expected_owner.is_some_and(|owner| owner != (stat.st_uid, stat.st_gid)) {
        return Err(store_error(
            ResidentSandboxStoreErrorKind::UnsafeFilesystem,
            "resident-sandbox store directory has an unexpected owner",
        ));
    }
    Ok(stat)
}

fn inspect_private_file(
    file: impl AsFd,
    owner: (u32, u32),
    _subject: &str,
    expected_size: Option<usize>,
) -> Result<rustix::fs::Stat, ResidentSandboxStoreError> {
    let stat = fs::fstat(file.as_fd()).map_err(|_| {
        store_error(
            ResidentSandboxStoreErrorKind::Io,
            "could not inspect resident-sandbox store file",
        )
    })?;
    if !FileType::from_raw_mode(stat.st_mode).is_file() || stat.st_nlink != 1 {
        return Err(store_error(
            ResidentSandboxStoreErrorKind::UnsafeFilesystem,
            "resident-sandbox store object is not a single-link regular file",
        ));
    }
    if stat.st_mode & 0o7777 != 0o600 {
        return Err(store_error(
            ResidentSandboxStoreErrorKind::UnsafeFilesystem,
            "resident-sandbox store file has an unexpected mode",
        ));
    }
    if owner != (stat.st_uid, stat.st_gid) {
        return Err(store_error(
            ResidentSandboxStoreErrorKind::UnsafeFilesystem,
            "resident-sandbox store file has an unexpected owner",
        ));
    }
    if expected_size.is_some_and(|size| stat.st_size < 0 || stat.st_size as u64 != size as u64) {
        return Err(store_error(
            ResidentSandboxStoreErrorKind::CorruptState,
            "resident-sandbox store file has an unexpected size",
        ));
    }
    Ok(stat)
}

fn synchronize_directory(directory: impl AsFd) -> Result<(), ResidentSandboxStoreError> {
    fs::fsync(directory.as_fd()).map_err(|_| {
        store_error(
            ResidentSandboxStoreErrorKind::Io,
            "could not synchronize resident-sandbox store directory",
        )
    })
}

fn synchronize_installation_root(directory: impl AsFd) -> Result<(), ResidentSandboxStoreError> {
    maybe_fault(FaultPoint::InstallationRootSync)?;
    synchronize_directory(directory)
}

fn retire_exact_document(
    directory: impl AsFd,
    name: &str,
    file: impl AsFd,
    identity: FilesystemIdentity,
    owner: (u32, u32),
) -> Result<(), ResidentSandboxStoreError> {
    let named = fs::openat(&directory, name, EXISTING_DOCUMENT_FLAGS, Mode::empty())
        .map_err(map_document_open_error)?;
    let named_stat = inspect_private_file(
        &named,
        owner,
        "resident-sandbox exact retirement candidate",
        None,
    )?;
    if !identity.matches(&named_stat) {
        return Err(store_error(
            ResidentSandboxStoreErrorKind::UnsafeFilesystem,
            "resident-sandbox retirement name was rebound",
        ));
    }
    let held_stat = inspect_private_file(
        &file,
        owner,
        "resident-sandbox retained retirement descriptor",
        None,
    )?;
    if !identity.matches(&held_stat) {
        return Err(store_error(
            ResidentSandboxStoreErrorKind::UnsafeFilesystem,
            "resident-sandbox retained retirement descriptor changed",
        ));
    }
    run_before_retire_hook();
    fs::ftruncate(&file, 0).map_err(|_| {
        store_error(
            ResidentSandboxStoreErrorKind::Io,
            "could not truncate retired resident-sandbox predecessor",
        )
    })?;
    fs::fsync(&file).map_err(|_| {
        store_error(
            ResidentSandboxStoreErrorKind::Io,
            "could not synchronize retired resident-sandbox predecessor",
        )
    })?;
    let after = inspect_private_file(
        &file,
        owner,
        "resident-sandbox retired predecessor",
        Some(0),
    )?;
    if !identity.matches(&after) {
        return Err(store_error(
            ResidentSandboxStoreErrorKind::UnsafeFilesystem,
            "resident-sandbox retired predecessor identity changed",
        ));
    }
    let named_after = fs::openat(&directory, name, EXISTING_DOCUMENT_FLAGS, Mode::empty())
        .map_err(map_document_open_error)?;
    let named_after = inspect_private_file(
        &named_after,
        owner,
        "resident-sandbox retired predecessor name",
        None,
    )?;
    if !identity.matches(&named_after) {
        return Err(store_error(
            ResidentSandboxStoreErrorKind::UnsafeFilesystem,
            "resident-sandbox retired predecessor name was rebound",
        ));
    }
    if named_after.st_size != 0 {
        return Err(store_error(
            ResidentSandboxStoreErrorKind::CorruptState,
            "resident-sandbox retired predecessor name is not empty",
        ));
    }
    Ok(())
}

fn store_error(
    kind: ResidentSandboxStoreErrorKind,
    message: &'static str,
) -> ResidentSandboxStoreError {
    ResidentSandboxStoreError { kind, message }
}

fn map_root_open_error(error: Errno) -> ResidentSandboxStoreError {
    match error {
        Errno::LOOP | Errno::NOTDIR => store_error(
            ResidentSandboxStoreErrorKind::UnsafeFilesystem,
            "installation root is symlinked or is not a directory",
        ),
        Errno::NOENT => store_error(
            ResidentSandboxStoreErrorKind::Missing,
            "installation root does not exist",
        ),
        _ => store_error(
            ResidentSandboxStoreErrorKind::Io,
            "could not open installation root",
        ),
    }
}

fn map_directory_open_error(error: Errno) -> ResidentSandboxStoreError {
    match error {
        Errno::LOOP | Errno::NOTDIR | Errno::ISDIR => store_error(
            ResidentSandboxStoreErrorKind::UnsafeFilesystem,
            "resident-sandbox store directory is symlinked or invalid",
        ),
        Errno::NOENT => store_error(
            ResidentSandboxStoreErrorKind::Missing,
            "resident-sandbox store directory does not exist",
        ),
        _ => store_error(
            ResidentSandboxStoreErrorKind::Io,
            "could not open resident-sandbox store directory",
        ),
    }
}

fn map_lock_open_error(error: Errno) -> ResidentSandboxStoreError {
    match error {
        Errno::LOOP | Errno::NOTDIR | Errno::ISDIR => store_error(
            ResidentSandboxStoreErrorKind::UnsafeFilesystem,
            "resident-sandbox store lock is symlinked or invalid",
        ),
        _ => store_error(
            ResidentSandboxStoreErrorKind::Io,
            "could not open resident-sandbox store lock",
        ),
    }
}

fn map_document_open_error(error: Errno) -> ResidentSandboxStoreError {
    match error {
        Errno::LOOP | Errno::NOTDIR | Errno::ISDIR => store_error(
            ResidentSandboxStoreErrorKind::UnsafeFilesystem,
            "resident-sandbox catalog document is symlinked or invalid",
        ),
        _ => store_error(
            ResidentSandboxStoreErrorKind::Io,
            "could not open resident-sandbox catalog document",
        ),
    }
}

fn map_stage_create_error(error: Errno) -> ResidentSandboxStoreError {
    match error {
        Errno::EXIST => store_error(
            ResidentSandboxStoreErrorKind::RecoveryRequired,
            "resident-sandbox staged catalog already exists",
        ),
        Errno::LOOP | Errno::NOTDIR | Errno::ISDIR => store_error(
            ResidentSandboxStoreErrorKind::UnsafeFilesystem,
            "resident-sandbox staged catalog path is unsafe",
        ),
        _ => store_error(
            ResidentSandboxStoreErrorKind::Io,
            "could not create resident-sandbox staged catalog",
        ),
    }
}

fn map_publish_error(error: Errno, no_replace: bool) -> ResidentSandboxStoreError {
    if no_replace && error == Errno::EXIST {
        store_error(
            ResidentSandboxStoreErrorKind::AlreadyExists,
            "resident-sandbox canonical catalog already exists",
        )
    } else {
        store_error(
            ResidentSandboxStoreErrorKind::Io,
            "could not atomically publish resident-sandbox catalog",
        )
    }
}

struct NamedCatalogDocument {
    file: OwnedFd,
    identity: FilesystemIdentity,
    catalog: ResidentSandboxCatalog,
}

#[derive(Debug)]
struct ResidentSandboxWriterLock<'a> {
    lock: OwnedFd,
    directory: OwnedFd,
    root: BorrowedFd<'a>,
    marker_directory: BorrowedFd<'a>,
    owner: (u32, u32),
    identity: FilesystemIdentity,
}

impl ResidentSandboxWriterLock<'_> {
    fn validate(&self) -> Result<(), ResidentSandboxStoreError> {
        let held = inspect_private_file(
            &self.lock,
            self.owner,
            "retained resident-sandbox store lock",
            Some(0),
        )?;
        let named = fs::openat(
            self.marker_directory,
            RESIDENT_SANDBOX_STORE_LOCK_FILE,
            EXISTING_LOCK_FLAGS,
            Mode::empty(),
        )
        .map_err(map_lock_open_error)?;
        let named = inspect_private_file(
            &named,
            self.owner,
            "named resident-sandbox store lock",
            Some(0),
        )?;
        if !self.identity.matches(&held) || !self.identity.matches(&named) {
            return Err(store_error(
                ResidentSandboxStoreErrorKind::UnsafeFilesystem,
                "resident-sandbox store lock directory entry was rebound",
            ));
        }
        let held_directory = inspect_directory(
            &self.directory,
            "retained locked resident-sandbox directory",
            Some(self.owner),
            Some(PRIVATE_DIRECTORY_MODE),
        )?;
        let named_directory = fs::openat(
            self.root,
            RESIDENT_SANDBOX_STORE_DIRECTORY,
            DIRECTORY_FLAGS,
            Mode::empty(),
        )
        .map_err(map_directory_open_error)?;
        let named_directory = inspect_directory(
            &named_directory,
            "named resident-sandbox directory",
            Some(self.owner),
            Some(PRIVATE_DIRECTORY_MODE),
        )?;
        if held_directory.st_dev != named_directory.st_dev
            || held_directory.st_ino != named_directory.st_ino
        {
            return Err(store_error(
                ResidentSandboxStoreErrorKind::UnsafeFilesystem,
                "resident-sandbox writer directory was rebound",
            ));
        }
        Ok(())
    }
}

impl Drop for ResidentSandboxWriterLock<'_> {
    fn drop(&mut self) {
        let _ = fs::flock(&self.lock, FlockOperation::Unlock);
        let _ = fs::flock(&self.directory, FlockOperation::Unlock);
    }
}

struct StagedDocument<'a> {
    directory: BorrowedFd<'a>,
    file: Option<File>,
    identity: Option<FilesystemIdentity>,
    owner: (u32, u32),
    armed: bool,
}

impl<'a> StagedDocument<'a> {
    fn existing(
        directory: BorrowedFd<'a>,
        file: OwnedFd,
        identity: FilesystemIdentity,
        owner: (u32, u32),
    ) -> Self {
        Self {
            directory,
            file: Some(File::from(file)),
            identity: Some(identity),
            owner,
            armed: false,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StagedDocument<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Some(identity) = self.identity else {
            return;
        };
        let Some(file) = self.file.as_ref() else {
            return;
        };
        let _ = retire_exact_document(
            self.directory,
            RESIDENT_SANDBOX_STAGED_DOCUMENT,
            file,
            identity,
            self.owner,
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FaultPoint {
    InstallationRootSync,
    StageCreate,
    StageOpen,
    StageWrite,
    StagePartialWrite,
    StageFileSync,
    StageReopenValidation,
    BeforeRename,
    AfterRenameBeforeParentSync,
    ParentSync,
    StaleStageRemoval,
    RecoveryPublication,
}

#[cfg(test)]
thread_local! {
    static FAULT: std::cell::Cell<Option<FaultPoint>> = const { std::cell::Cell::new(None) };
    static BEFORE_RENAME_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_RETIRE_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
struct FaultGuard;

#[cfg(test)]
fn inject_fault(point: FaultPoint) -> FaultGuard {
    FAULT.with(|fault| assert!(fault.replace(Some(point)).is_none()));
    FaultGuard
}

#[cfg(test)]
impl Drop for FaultGuard {
    fn drop(&mut self) {
        FAULT.with(|fault| fault.set(None));
    }
}

#[cfg(test)]
fn take_fault(point: FaultPoint) -> bool {
    FAULT.with(|fault| {
        if fault.get() == Some(point) {
            fault.set(None);
            true
        } else {
            false
        }
    })
}

#[cfg(not(test))]
fn take_fault(_point: FaultPoint) -> bool {
    false
}

#[cfg(test)]
fn maybe_fault(point: FaultPoint) -> Result<(), ResidentSandboxStoreError> {
    if take_fault(point) {
        Err(store_error(
            ResidentSandboxStoreErrorKind::Io,
            "injected resident-sandbox durable publication failure",
        ))
    } else {
        Ok(())
    }
}

#[cfg(not(test))]
fn maybe_fault(_point: FaultPoint) -> Result<(), ResidentSandboxStoreError> {
    Ok(())
}

#[cfg(test)]
struct TestHookGuard {
    rename: bool,
}

#[cfg(test)]
impl Drop for TestHookGuard {
    fn drop(&mut self) {
        if self.rename {
            BEFORE_RENAME_HOOK.with(|hook| {
                hook.borrow_mut().take();
            });
        } else {
            BEFORE_RETIRE_HOOK.with(|hook| {
                hook.borrow_mut().take();
            });
        }
    }
}

#[cfg(test)]
fn inject_before_rename_hook(hook: impl FnOnce() + 'static) -> TestHookGuard {
    BEFORE_RENAME_HOOK.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
    TestHookGuard { rename: true }
}

#[cfg(test)]
fn inject_before_retire_hook(hook: impl FnOnce() + 'static) -> TestHookGuard {
    BEFORE_RETIRE_HOOK.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
    TestHookGuard { rename: false }
}

#[cfg(test)]
fn run_before_rename_hook() {
    BEFORE_RENAME_HOOK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_rename_hook() {}

#[cfg(test)]
fn run_before_retire_hook() {
    BEFORE_RETIRE_HOOK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_retire_hook() {}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::project_catalog::ProjectIdentity;
    use crate::project_disk_host_observation::ProjectDiskLimaSourceIdentity;
    use crate::project_disk_lease::ResidentSandboxId;
    use crate::resident_sandbox_catalog::{
        ResidentGuestControlPolicyGeneration, ResidentGuestPrivilegePolicy,
        ResidentGuestPrivilegePolicyGeneration, ResidentLimaLayoutGeneration,
        ResidentLocatorPolicyGeneration, ResidentNetworkPolicyGeneration,
        ResidentPreparedTemplateGeneration, ResidentProjectIntegrationPolicyGeneration,
        ResidentResourceDeclaration, ResidentResourceGeneration, ResidentSandboxAcceptanceRequest,
        ResidentSandboxAcceptanceRequestId, ResidentSandboxCheckpoint, ResidentSandboxConfig,
        ResidentSandboxConfigGeneration, ResidentSandboxKey, ResidentSandboxRecordRevision,
    };

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
                "smolrunner-resident-catalog-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create temporary root");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o750))
                .expect("set temporary root mode");
            let metadata = fs::symlink_metadata(&path).expect("inspect temporary root");
            Self {
                path,
                device: metadata.dev(),
                inode: metadata.ino(),
            }
        }

        fn path(&self) -> &Path {
            &self.path
        }

        fn directory(&self) -> PathBuf {
            self.path.join(RESIDENT_SANDBOX_STORE_DIRECTORY)
        }

        fn current(&self) -> PathBuf {
            self.directory().join(RESIDENT_SANDBOX_CURRENT_DOCUMENT)
        }

        fn staged(&self) -> PathBuf {
            self.directory().join(RESIDENT_SANDBOX_STAGED_DOCUMENT)
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

    fn source() -> ProjectDiskLimaSourceIdentity {
        ProjectDiskLimaSourceIdentity::parse(&format!("sha256:{}", "a".repeat(64)))
            .expect("source identity")
    }

    fn config() -> ResidentSandboxConfig {
        ResidentSandboxConfig::reviewed(
            ResidentPreparedTemplateGeneration::new(1).expect("template generation"),
            ResidentSandboxConfigGeneration::new(1).expect("config generation"),
            ResidentLimaLayoutGeneration::new(1).expect("layout generation"),
            ResidentResourceDeclaration::new(
                ResidentResourceGeneration::new(1).expect("resource generation"),
                2_000,
                2 * 1024 * 1024 * 1024,
                20 * 1024 * 1024 * 1024,
            )
            .expect("resources"),
            ResidentNetworkPolicyGeneration::new(1).expect("network generation"),
            crate::resident_sandbox_catalog::ResidentCredentialPolicyGeneration::new(1)
                .expect("credential generation"),
            ResidentGuestControlPolicyGeneration::new(1).expect("guest control generation"),
            ResidentGuestPrivilegePolicy::reviewed(
                ResidentGuestPrivilegePolicyGeneration::new(1).expect("privilege generation"),
            ),
            ResidentProjectIntegrationPolicyGeneration::new(1).expect("integration generation"),
            None,
        )
        .expect("reviewed config")
    }

    fn request(id: &str) -> ResidentSandboxAcceptanceRequest {
        ResidentSandboxAcceptanceRequest::new(
            ResidentSandboxAcceptanceRequestId::parse(id).expect("request ID"),
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").expect("project"),
            ResidentSandboxId::parse("resident-a").expect("sandbox ID"),
            source(),
            ResidentLocatorPolicyGeneration::new(1).expect("locator policy generation"),
            config(),
        )
    }

    fn accepted() -> (ResidentSandboxCatalog, ResidentSandboxKey) {
        let (catalog, receipt) = ResidentSandboxCatalog::empty()
            .accept(request("request-a"))
            .expect("acceptance");
        (catalog, receipt.key().clone())
    }

    fn authorized(
        catalog: &ResidentSandboxCatalog,
        key: &ResidentSandboxKey,
    ) -> ResidentSandboxCatalog {
        catalog
            .checkpoint(
                key,
                ResidentSandboxRecordRevision::new(1).expect("record revision"),
                ResidentSandboxCheckpoint::MaterializeAuthorized,
            )
            .expect("authorized checkpoint")
    }

    fn started(
        catalog: &ResidentSandboxCatalog,
        key: &ResidentSandboxKey,
    ) -> ResidentSandboxCatalog {
        let entry = catalog.find(key).expect("accepted entry");
        catalog
            .checkpoint(
                key,
                entry.revision(),
                ResidentSandboxCheckpoint::MaterializeStarted,
            )
            .expect("started checkpoint")
    }

    fn checkpoint(
        catalog: &ResidentSandboxCatalog,
        key: &ResidentSandboxKey,
        checkpoint: ResidentSandboxCheckpoint,
    ) -> ResidentSandboxCatalog {
        let entry = catalog.find(key).expect("accepted entry");
        catalog
            .checkpoint(key, entry.revision(), checkpoint)
            .expect("checkpoint")
    }

    #[derive(Debug, Clone, Copy)]
    enum TestOperationKind {
        Materialize,
        Start,
        Stop,
    }

    #[derive(Debug, Clone, Copy)]
    enum TestCheckpointKind {
        Authorized,
        PrestartFailed,
        Started,
        RecoveryRequired,
    }

    fn operation_fixture(kind: TestOperationKind) -> (ResidentSandboxCatalog, ResidentSandboxKey) {
        let (catalog, key) = accepted();
        let catalog = match kind {
            TestOperationKind::Materialize => catalog,
            TestOperationKind::Start => catalog
                .test_with_bound_physical_state(&key, false)
                .expect("stopped-bound test fixture"),
            TestOperationKind::Stop => catalog
                .test_with_bound_physical_state(&key, true)
                .expect("running-bound test fixture"),
        };
        (catalog, key)
    }

    fn authorize_operation(
        catalog: &ResidentSandboxCatalog,
        key: &ResidentSandboxKey,
        kind: TestOperationKind,
    ) -> ResidentSandboxCatalog {
        checkpoint(
            catalog,
            key,
            match kind {
                TestOperationKind::Materialize => ResidentSandboxCheckpoint::MaterializeAuthorized,
                TestOperationKind::Start => ResidentSandboxCheckpoint::StartAuthorized,
                TestOperationKind::Stop => ResidentSandboxCheckpoint::StopAuthorized,
            },
        )
    }

    fn start_operation(
        catalog: &ResidentSandboxCatalog,
        key: &ResidentSandboxKey,
        kind: TestOperationKind,
    ) -> ResidentSandboxCatalog {
        checkpoint(
            catalog,
            key,
            match kind {
                TestOperationKind::Materialize => ResidentSandboxCheckpoint::MaterializeStarted,
                TestOperationKind::Start => ResidentSandboxCheckpoint::StartStarted,
                TestOperationKind::Stop => ResidentSandboxCheckpoint::StopStarted,
            },
        )
    }

    fn fail_prestart_operation(
        catalog: &ResidentSandboxCatalog,
        key: &ResidentSandboxKey,
        kind: TestOperationKind,
    ) -> ResidentSandboxCatalog {
        checkpoint(
            catalog,
            key,
            match kind {
                TestOperationKind::Materialize => {
                    ResidentSandboxCheckpoint::MaterializePrestartFailed
                }
                TestOperationKind::Start => ResidentSandboxCheckpoint::StartPrestartFailed,
                TestOperationKind::Stop => ResidentSandboxCheckpoint::StopPrestartFailed,
            },
        )
    }

    fn recovery_required_operation(
        catalog: &ResidentSandboxCatalog,
        key: &ResidentSandboxKey,
        kind: TestOperationKind,
    ) -> ResidentSandboxCatalog {
        checkpoint(
            catalog,
            key,
            match kind {
                TestOperationKind::Materialize => {
                    ResidentSandboxCheckpoint::MaterializeRecoveryRequired
                }
                TestOperationKind::Start => ResidentSandboxCheckpoint::StartRecoveryRequired,
                TestOperationKind::Stop => ResidentSandboxCheckpoint::StopRecoveryRequired,
            },
        )
    }

    fn operation_checkpoint_fixture(
        operation: TestOperationKind,
        checkpoint_kind: TestCheckpointKind,
    ) -> (
        ResidentSandboxCatalog,
        ResidentSandboxCatalog,
        ResidentSandboxKey,
    ) {
        let (initial, key) = operation_fixture(operation);
        let authorized = authorize_operation(&initial, &key, operation);
        match checkpoint_kind {
            TestCheckpointKind::Authorized => (initial, authorized, key),
            TestCheckpointKind::PrestartFailed => {
                let failed = fail_prestart_operation(&authorized, &key, operation);
                (authorized, failed, key)
            }
            TestCheckpointKind::Started => {
                let started = start_operation(&authorized, &key, operation);
                (authorized, started, key)
            }
            TestCheckpointKind::RecoveryRequired => {
                let started = start_operation(&authorized, &key, operation);
                let recovery = recovery_required_operation(&started, &key, operation);
                (started, recovery, key)
            }
        }
    }

    fn install_test_catalog(
        store: &UnixResidentSandboxCatalogStore,
        catalog: &ResidentSandboxCatalog,
    ) {
        let lock = store.acquire_writer_lock().expect("acquire writer lock");
        store.prepare_locked(&lock).expect("prepare locked store");
        assert!(
            store
                .open_named_document(RESIDENT_SANDBOX_CURRENT_DOCUMENT)
                .unwrap()
                .is_none()
        );
        let mut staged = store.stage(catalog).expect("stage initial test catalog");
        store
            .publish(&lock, &mut staged, catalog, None, true, false)
            .expect("publish initial test catalog");
    }

    fn assert_operation_identity(
        observed: &ResidentSandboxCatalog,
        expected: &ResidentSandboxCatalog,
        key: &ResidentSandboxKey,
    ) {
        assert_eq!(observed.revision(), expected.revision());
        let observed = observed.find(key).expect("observed operation record");
        let expected = expected.find(key).expect("expected operation record");
        assert_eq!(observed.revision(), expected.revision());
        assert_eq!(
            observed.last_operation_generation(),
            expected.last_operation_generation()
        );
        assert_eq!(observed.active_operation(), expected.active_operation());
        assert_eq!(observed.config_digest(), expected.config_digest());
        assert_eq!(observed.locator(), expected.locator());
        assert_eq!(observed.physical(), expected.physical());
    }

    fn assert_no_external_callbacks(count: usize) {
        assert_eq!(count, 0, "the persistence slice must execute no callback");
    }

    fn stage_without_cleanup(
        store: &UnixResidentSandboxCatalogStore,
        catalog: &ResidentSandboxCatalog,
    ) {
        let mut stage = store.stage(catalog).expect("stage catalog");
        stage.disarm();
    }

    fn assert_stage_absent(root: &TempRoot) {
        if root.staged().exists() {
            assert_eq!(
                fs::metadata(root.staged()).expect("staged metadata").len(),
                0,
                "a retained staged slot must be an exact private zero-length slot"
            );
        }
    }

    fn open_store(root: &TempRoot) -> UnixResidentSandboxCatalogStore {
        let (store, recovery) = UnixResidentSandboxCatalogStore::open_or_create(root.path())
            .expect("open resident-sandbox store");
        assert_eq!(recovery, ResidentSandboxRecoveryDisposition::Clean);
        store
    }

    #[test]
    fn opens_private_store_and_reopens_catalog() {
        let root = TempRoot::new("reopen");
        let mut store = open_store(&root);
        assert_eq!(
            store
                .create(&ResidentSandboxCatalog::empty())
                .expect("create catalog")
                .disposition(),
            ResidentSandboxCatalogWriteDisposition::Created
        );
        let (next, _) = accepted();
        store
            .replace_if_revision(ResidentSandboxCatalogRevision::new(1).unwrap(), &next)
            .expect("publish accepted catalog");
        drop(store);
        let reopened = open_store(&root);
        assert_eq!(reopened.load().expect("load catalog"), Some(next));
        assert_eq!(
            fs::metadata(root.directory()).unwrap().permissions().mode() & 0o7777,
            0o700
        );
        for file in [
            root.directory().join(RESIDENT_SANDBOX_STORE_LOCK_FILE),
            root.current(),
        ] {
            assert_eq!(
                fs::metadata(file).unwrap().permissions().mode() & 0o7777,
                0o600
            );
        }
    }

    #[test]
    fn initialization_parent_sync_failure_is_closed_by_the_next_locked_open() {
        let root = TempRoot::new("parent-sync-retry");
        let fault = inject_fault(FaultPoint::InstallationRootSync);
        assert_eq!(
            UnixResidentSandboxCatalogStore::open_or_create(root.path())
                .unwrap_err()
                .kind(),
            ResidentSandboxStoreErrorKind::Io
        );
        drop(fault);
        assert!(root.directory().exists());
        let mut store = open_store(&root);
        store.create(&ResidentSandboxCatalog::empty()).unwrap();
        drop(store);
        let reopened = open_store(&root);
        assert_eq!(
            reopened.load().unwrap(),
            Some(ResidentSandboxCatalog::empty())
        );
    }

    #[test]
    fn lock_rebind_is_detected_and_guard_drop_unlocks_an_inherited_duplicate() {
        let root = TempRoot::new("lock-rebind");
        let store = open_store(&root);
        let guard = store.acquire_writer_lock().expect("acquire writer lock");
        assert!(
            rustix::io::fcntl_getfd(&guard.directory)
                .expect("inspect directory lock descriptor flags")
                .contains(rustix::io::FdFlags::CLOEXEC)
        );
        let retained =
            rustix::io::dup(&guard.directory).expect("duplicate directory lock description");
        let retained_marker =
            rustix::io::dup(&guard.lock).expect("duplicate persistent lock description");
        drop(guard);
        let competing = fs::OpenOptions::new()
            .read(true)
            .open(root.directory())
            .expect("open competing lock");
        rustix::fs::flock(&competing, FlockOperation::NonBlockingLockExclusive)
            .expect("explicit guard drop unlocks inherited description");
        rustix::fs::flock(&competing, FlockOperation::Unlock).expect("release competing lock");
        let competing_marker = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.directory().join(RESIDENT_SANDBOX_STORE_LOCK_FILE))
            .expect("open competing persistent lock");
        rustix::fs::flock(&competing_marker, FlockOperation::NonBlockingLockExclusive)
            .expect("explicit guard drop unlocks inherited persistent description");
        rustix::fs::flock(&competing_marker, FlockOperation::Unlock)
            .expect("release competing persistent lock");
        drop(retained);
        drop(retained_marker);

        let guard = store.acquire_writer_lock().expect("reacquire writer lock");
        let displaced = root.directory().join("store.lock.displaced");
        fs::rename(
            root.directory().join(RESIDENT_SANDBOX_STORE_LOCK_FILE),
            &displaced,
        )
        .expect("displace canonical lock");
        let replacement = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(root.directory().join(RESIDENT_SANDBOX_STORE_LOCK_FILE))
            .expect("create replacement lock");
        fs::set_permissions(
            root.directory().join(RESIDENT_SANDBOX_STORE_LOCK_FILE),
            fs::Permissions::from_mode(0o600),
        )
        .expect("set replacement lock mode");
        rustix::fs::flock(&replacement, FlockOperation::NonBlockingLockExclusive)
            .expect("replacement inode has split lock authority");
        let competing_store = UnixResidentSandboxCatalogStore::open_existing(root.path())
            .expect("open competing store view");
        assert_eq!(
            competing_store.acquire_writer_lock().unwrap_err().kind(),
            ResidentSandboxStoreErrorKind::Busy,
            "replacing the marker cannot create a second writer over the held directory lock"
        );
        assert_eq!(
            guard.validate().unwrap_err().kind(),
            ResidentSandboxStoreErrorKind::UnsafeFilesystem
        );
        rustix::fs::flock(&replacement, FlockOperation::Unlock).expect("release replacement lock");
    }

    #[test]
    fn exact_started_stage_is_discarded_without_external_callback() {
        let root = TempRoot::new("started-discard");
        let mut store = open_store(&root);
        store.create(&ResidentSandboxCatalog::empty()).unwrap();
        let (accepted_catalog, key) = accepted();
        store
            .replace_if_revision(
                ResidentSandboxCatalogRevision::new(1).unwrap(),
                &accepted_catalog,
            )
            .unwrap();
        let authorized = authorized(&accepted_catalog, &key);
        store
            .replace_if_revision(ResidentSandboxCatalogRevision::new(2).unwrap(), &authorized)
            .unwrap();
        let started_catalog = started(&authorized, &key);
        stage_without_cleanup(&store, &started_catalog);
        assert_eq!(
            store.recover().unwrap(),
            ResidentSandboxRecoveryDisposition::DiscardedStartedStaged
        );
        assert_eq!(store.load().unwrap(), Some(authorized));
        assert_stage_absent(&root);
    }

    #[test]
    fn opener_reports_the_exact_recovery_action_it_performed() {
        let root = TempRoot::new("open-recovery-receipt");
        let mut store = open_store(&root);
        store.create(&ResidentSandboxCatalog::empty()).unwrap();
        let (accepted_catalog, key) = accepted();
        store
            .replace_if_revision(
                ResidentSandboxCatalogRevision::new(1).unwrap(),
                &accepted_catalog,
            )
            .unwrap();
        let authorized = authorized(&accepted_catalog, &key);
        store
            .replace_if_revision(accepted_catalog.revision(), &authorized)
            .unwrap();
        let started_catalog = started(&authorized, &key);
        stage_without_cleanup(&store, &started_catalog);
        drop(store);

        let (reopened, recovery) =
            UnixResidentSandboxCatalogStore::open_or_create(root.path()).unwrap();
        assert_eq!(
            recovery,
            ResidentSandboxRecoveryDisposition::DiscardedStartedStaged
        );
        assert_eq!(reopened.load().unwrap(), Some(authorized));
        assert_stage_absent(&root);
    }

    #[test]
    fn safe_operation_stages_recover_without_rewriting_operation_identity() {
        let root = TempRoot::new("safe-operation-recovery");
        let mut store = open_store(&root);
        store.create(&ResidentSandboxCatalog::empty()).unwrap();
        let (accepted_catalog, key) = accepted();
        store
            .replace_if_revision(
                ResidentSandboxCatalogRevision::new(1).unwrap(),
                &accepted_catalog,
            )
            .unwrap();

        let first_authorized = authorized(&accepted_catalog, &key);
        stage_without_cleanup(&store, &first_authorized);
        assert_eq!(
            store.recover().unwrap(),
            ResidentSandboxRecoveryDisposition::PublishedStaged
        );
        let first_operation = first_authorized
            .find(&key)
            .unwrap()
            .last_operation_generation()
            .unwrap();
        assert_eq!(first_operation.get(), 1);

        let prestart_failed = checkpoint(
            &first_authorized,
            &key,
            ResidentSandboxCheckpoint::MaterializePrestartFailed,
        );
        stage_without_cleanup(&store, &prestart_failed);
        assert_eq!(
            store.recover().unwrap(),
            ResidentSandboxRecoveryDisposition::PublishedStaged
        );
        assert!(matches!(
            prestart_failed.find(&key).unwrap().active_operation(),
            ResidentSandboxActiveOperation::None
        ));
        assert_eq!(
            prestart_failed
                .find(&key)
                .unwrap()
                .last_operation_generation(),
            Some(first_operation)
        );

        let second_authorized = checkpoint(
            &prestart_failed,
            &key,
            ResidentSandboxCheckpoint::MaterializeAuthorized,
        );
        store
            .replace_if_revision(prestart_failed.revision(), &second_authorized)
            .unwrap();
        let second_operation = second_authorized
            .find(&key)
            .unwrap()
            .last_operation_generation()
            .unwrap();
        assert_eq!(second_operation.get(), 2);
        let second_started = started(&second_authorized, &key);
        store
            .replace_if_revision(second_authorized.revision(), &second_started)
            .unwrap();
        let recovery_required = checkpoint(
            &second_started,
            &key,
            ResidentSandboxCheckpoint::MaterializeRecoveryRequired,
        );
        stage_without_cleanup(&store, &recovery_required);
        assert_eq!(
            store.recover().unwrap(),
            ResidentSandboxRecoveryDisposition::PublishedStaged
        );
        let recovered = store.load().unwrap().unwrap();
        assert_eq!(recovered, recovery_required);
        assert_eq!(
            recovered.find(&key).unwrap().last_operation_generation(),
            Some(second_operation)
        );
        assert!(matches!(
            recovered.find(&key).unwrap().active_operation(),
            ResidentSandboxActiveOperation::Materialize {
                generation,
                phase: ResidentSandboxOperationPhase::RecoveryRequired,
                ..
            } if *generation == second_operation
        ));
        assert_eq!(
            recovered.find(&key).unwrap().locator(),
            accepted_catalog.find(&key).unwrap().locator()
        );
        assert_eq!(
            recovered.find(&key).unwrap().config_digest(),
            accepted_catalog.find(&key).unwrap().config_digest()
        );
    }

    #[test]
    fn publication_refuses_an_identical_rebound_stage_and_preserves_the_foreign_entry() {
        let root = TempRoot::new("stage-replacement");
        let mut store = open_store(&root);
        let current = ResidentSandboxCatalog::empty();
        store.create(&current).unwrap();
        let (expected, _) = accepted();
        let lock = store.acquire_writer_lock().expect("acquire writer lock");
        store.prepare_locked(&lock).expect("prepare locked store");
        let predecessor = store
            .open_named_document(RESIDENT_SANDBOX_CURRENT_DOCUMENT)
            .unwrap()
            .unwrap();
        let mut stage = store.stage(&expected).expect("stage expected candidate");
        fs::remove_file(root.staged()).expect("remove named stage");
        fs::write(
            root.staged(),
            encode_resident_sandbox_catalog(&expected).expect("encode rebound stage"),
        )
        .expect("write replacement stage");
        fs::set_permissions(root.staged(), fs::Permissions::from_mode(0o600))
            .expect("set replacement mode");
        assert_eq!(
            store
                .publish(
                    &lock,
                    &mut stage,
                    &expected,
                    Some(&predecessor),
                    false,
                    false,
                )
                .unwrap_err()
                .kind(),
            ResidentSandboxStoreErrorKind::UnsafeFilesystem
        );
        drop(stage);
        assert_eq!(
            decode_resident_sandbox_catalog(&fs::read(root.staged()).unwrap()).unwrap(),
            expected
        );
        drop(lock);
        assert_eq!(store.load_current().unwrap(), Some(current));
    }

    #[test]
    fn publication_refuses_an_identical_rebound_current_predecessor() {
        let root = TempRoot::new("current-rebind");
        let mut store = open_store(&root);
        let current = ResidentSandboxCatalog::empty();
        store.create(&current).unwrap();
        let (next, _) = accepted();
        let lock = store.acquire_writer_lock().expect("acquire writer lock");
        store.prepare_locked(&lock).expect("prepare locked store");
        let predecessor = store
            .open_named_document(RESIDENT_SANDBOX_CURRENT_DOCUMENT)
            .unwrap()
            .unwrap();
        let mut stage = store.stage(&next).expect("stage successor");
        let displaced = root.directory().join("catalog.displaced.json");
        fs::rename(root.current(), &displaced).expect("displace canonical predecessor");
        fs::write(
            root.current(),
            encode_resident_sandbox_catalog(&current).expect("encode rebound predecessor"),
        )
        .expect("write rebound predecessor");
        fs::set_permissions(root.current(), fs::Permissions::from_mode(0o600))
            .expect("set rebound predecessor mode");
        assert_eq!(
            store
                .publish(&lock, &mut stage, &next, Some(&predecessor), false, false,)
                .unwrap_err()
                .kind(),
            ResidentSandboxStoreErrorKind::UnsafeFilesystem
        );
        drop(stage);
        assert_stage_absent(&root);
        assert_eq!(
            decode_resident_sandbox_catalog(&fs::read(root.current()).unwrap()).unwrap(),
            current
        );
    }

    #[test]
    fn exchange_detects_a_stage_rebind_in_the_final_window_without_destroying_truth() {
        let root = TempRoot::new("stage-final-window-rebind");
        let mut store = open_store(&root);
        let current = ResidentSandboxCatalog::empty();
        store.create(&current).unwrap();
        let (next, _) = accepted();
        let lock = store.acquire_writer_lock().expect("acquire writer lock");
        store.prepare_locked(&lock).expect("prepare locked store");
        let predecessor = store
            .open_named_document(RESIDENT_SANDBOX_CURRENT_DOCUMENT)
            .unwrap()
            .unwrap();
        let mut stage = store.stage(&next).expect("stage successor");
        let original_stage_inode = fs::metadata(root.staged()).unwrap().ino();
        let predecessor_inode = fs::metadata(root.current()).unwrap().ino();
        let displaced_stage = root.directory().join("stage.retained.json");
        let staged_path = root.staged();
        let next_bytes = encode_resident_sandbox_catalog(&next).unwrap();
        let hook = inject_before_rename_hook(move || {
            fs::rename(&staged_path, &displaced_stage).expect("retain exact staged inode");
            fs::write(&staged_path, next_bytes).expect("write identical foreign stage");
            fs::set_permissions(&staged_path, fs::Permissions::from_mode(0o600))
                .expect("set foreign stage mode");
        });

        assert_eq!(
            store
                .publish(&lock, &mut stage, &next, Some(&predecessor), false, false,)
                .unwrap_err()
                .kind(),
            ResidentSandboxStoreErrorKind::UnsafeFilesystem
        );
        drop(hook);
        drop(stage);
        assert_eq!(
            fs::metadata(root.directory().join("stage.retained.json"))
                .unwrap()
                .ino(),
            original_stage_inode
        );
        assert_eq!(
            fs::metadata(root.staged()).unwrap().ino(),
            predecessor_inode
        );
        assert_eq!(
            decode_resident_sandbox_catalog(&fs::read(root.current()).unwrap()).unwrap(),
            next
        );
        assert_eq!(
            decode_resident_sandbox_catalog(&fs::read(root.staged()).unwrap()).unwrap(),
            current
        );
    }

    #[test]
    fn exchange_detects_a_predecessor_rebind_in_the_final_window_without_overwrite() {
        let root = TempRoot::new("current-final-window-rebind");
        let mut store = open_store(&root);
        let current = ResidentSandboxCatalog::empty();
        store.create(&current).unwrap();
        let (next, _) = accepted();
        let lock = store.acquire_writer_lock().expect("acquire writer lock");
        store.prepare_locked(&lock).expect("prepare locked store");
        let predecessor = store
            .open_named_document(RESIDENT_SANDBOX_CURRENT_DOCUMENT)
            .unwrap()
            .unwrap();
        let mut stage = store.stage(&next).expect("stage successor");
        let original_predecessor_inode = fs::metadata(root.current()).unwrap().ino();
        let original_stage_inode = fs::metadata(root.staged()).unwrap().ino();
        let displaced_current = root.directory().join("current.retained.json");
        let current_path = root.current();
        let current_bytes = encode_resident_sandbox_catalog(&current).unwrap();
        let hook = inject_before_rename_hook(move || {
            fs::rename(&current_path, &displaced_current).expect("retain exact predecessor");
            fs::write(&current_path, current_bytes).expect("write identical foreign predecessor");
            fs::set_permissions(&current_path, fs::Permissions::from_mode(0o600))
                .expect("set foreign predecessor mode");
        });

        assert_eq!(
            store
                .publish(&lock, &mut stage, &next, Some(&predecessor), false, false,)
                .unwrap_err()
                .kind(),
            ResidentSandboxStoreErrorKind::UnsafeFilesystem
        );
        drop(hook);
        drop(stage);
        assert_eq!(
            fs::metadata(root.directory().join("current.retained.json"))
                .unwrap()
                .ino(),
            original_predecessor_inode
        );
        assert_eq!(
            fs::metadata(root.current()).unwrap().ino(),
            original_stage_inode
        );
        assert_eq!(
            decode_resident_sandbox_catalog(&fs::read(root.current()).unwrap()).unwrap(),
            next
        );
        assert_eq!(
            decode_resident_sandbox_catalog(&fs::read(root.staged()).unwrap()).unwrap(),
            current
        );
    }

    #[test]
    fn exact_descriptor_retirement_preserves_a_final_window_foreign_stage() {
        let root = TempRoot::new("retire-final-window-rebind");
        let mut store = open_store(&root);
        let current = ResidentSandboxCatalog::empty();
        store.create(&current).unwrap();
        stage_without_cleanup(&store, &current);
        let original_stage_inode = fs::metadata(root.staged()).unwrap().ino();
        let displaced_stage = root.directory().join("stage.discarded.json");
        let staged_path = root.staged();
        let current_bytes = encode_resident_sandbox_catalog(&current).unwrap();
        let hook = inject_before_retire_hook(move || {
            fs::rename(&staged_path, &displaced_stage).expect("retain discard candidate");
            fs::write(&staged_path, current_bytes).expect("write identical foreign stage");
            fs::set_permissions(&staged_path, fs::Permissions::from_mode(0o600))
                .expect("set foreign stage mode");
        });

        assert_eq!(
            store.recover().unwrap_err().kind(),
            ResidentSandboxStoreErrorKind::UnsafeFilesystem
        );
        drop(hook);
        assert_eq!(
            fs::metadata(root.directory().join("stage.discarded.json"))
                .unwrap()
                .ino(),
            original_stage_inode
        );
        assert_eq!(
            fs::metadata(root.directory().join("stage.discarded.json"))
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            decode_resident_sandbox_catalog(&fs::read(root.staged()).unwrap()).unwrap(),
            current
        );
        assert_eq!(store.load_current().unwrap(), Some(current));
    }

    #[test]
    fn held_directory_rebind_blocks_further_store_authority() {
        let root = TempRoot::new("directory-rebind");
        let mut store = open_store(&root);
        store.create(&ResidentSandboxCatalog::empty()).unwrap();
        let displaced = root.path().join("resident-sandbox-displaced");
        fs::rename(root.directory(), &displaced).expect("displace store directory");
        fs::create_dir(root.directory()).expect("create replacement directory");
        fs::set_permissions(root.directory(), fs::Permissions::from_mode(0o700))
            .expect("set replacement directory mode");
        assert_eq!(
            store.load().unwrap_err().kind(),
            ResidentSandboxStoreErrorKind::UnsafeFilesystem
        );
    }

    #[test]
    fn busy_lock_and_unsafe_canonical_metadata_fail_closed() {
        let root = TempRoot::new("lock-and-metadata");
        let mut store = open_store(&root);
        store.create(&ResidentSandboxCatalog::empty()).unwrap();
        let lock = fs::OpenOptions::new()
            .read(true)
            .open(root.directory())
            .expect("open writer lock");
        rustix::fs::flock(&lock, FlockOperation::NonBlockingLockExclusive)
            .expect("hold writer lock");
        assert_eq!(
            store.load().unwrap_err().kind(),
            ResidentSandboxStoreErrorKind::Busy
        );
        rustix::fs::flock(&lock, FlockOperation::Unlock).expect("release writer lock");
        let legacy_lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.directory().join(RESIDENT_SANDBOX_STORE_LOCK_FILE))
            .expect("open persistent writer lock");
        rustix::fs::flock(&legacy_lock, FlockOperation::NonBlockingLockExclusive)
            .expect("hold persistent writer lock");
        assert_eq!(
            store.load().unwrap_err().kind(),
            ResidentSandboxStoreErrorKind::Busy,
            "directory locking must remain compatible with a marker-only incumbent"
        );
        rustix::fs::flock(&legacy_lock, FlockOperation::Unlock)
            .expect("release persistent writer lock");
        fs::set_permissions(root.current(), fs::Permissions::from_mode(0o644))
            .expect("weaken canonical mode");
        assert_eq!(
            store.load().unwrap_err().kind(),
            ResidentSandboxStoreErrorKind::UnsafeFilesystem
        );
    }

    #[test]
    fn malformed_and_oversized_stages_remain_fenced_for_explicit_recovery() {
        let root = TempRoot::new("bad-stages");
        let mut store = open_store(&root);
        let current = ResidentSandboxCatalog::empty();
        store.create(&current).unwrap();

        fs::write(root.staged(), b"{}\n").expect("write malformed stage");
        fs::set_permissions(root.staged(), fs::Permissions::from_mode(0o600))
            .expect("set malformed stage mode");
        assert!(matches!(
            store.recover().unwrap_err().kind(),
            ResidentSandboxStoreErrorKind::CorruptState
                | ResidentSandboxStoreErrorKind::NonCanonical
        ));
        assert!(root.staged().exists());
        assert_eq!(store.load_current().unwrap(), Some(current.clone()));

        fs::remove_file(root.staged()).expect("remove malformed stage fixture");
        fs::write(
            root.staged(),
            vec![
                b'x';
                crate::resident_sandbox_catalog::MAX_RESIDENT_SANDBOX_CATALOG_DOCUMENT_BYTES + 1
            ],
        )
        .expect("write oversized stage");
        fs::set_permissions(root.staged(), fs::Permissions::from_mode(0o600))
            .expect("set oversized stage mode");
        assert_eq!(
            store.recover().unwrap_err().kind(),
            ResidentSandboxStoreErrorKind::CorruptState
        );
        assert!(root.staged().exists());
        assert_eq!(store.load_current().unwrap(), Some(current));
    }

    #[test]
    fn stale_stage_is_removed_and_fault_is_retryable() {
        let root = TempRoot::new("stale");
        let mut store = open_store(&root);
        let catalog = ResidentSandboxCatalog::empty();
        store.create(&catalog).unwrap();
        stage_without_cleanup(&store, &catalog);
        let _fault = inject_fault(FaultPoint::StaleStageRemoval);
        assert_eq!(
            store.recover().unwrap_err().kind(),
            ResidentSandboxStoreErrorKind::Io
        );
        assert!(root.staged().exists());
        assert_eq!(
            store.recover().unwrap(),
            ResidentSandboxRecoveryDisposition::RemovedStaleStaged
        );
        assert_stage_absent(&root);
    }

    #[test]
    fn publication_faults_preserve_exact_pre_or_post_rename_truth() {
        for point in [
            FaultPoint::StageCreate,
            FaultPoint::StageOpen,
            FaultPoint::StageWrite,
            FaultPoint::StagePartialWrite,
            FaultPoint::StageFileSync,
            FaultPoint::StageReopenValidation,
            FaultPoint::BeforeRename,
            FaultPoint::AfterRenameBeforeParentSync,
            FaultPoint::ParentSync,
        ] {
            let root = TempRoot::new("fault");
            let mut store = open_store(&root);
            store.create(&ResidentSandboxCatalog::empty()).unwrap();
            let (next, _) = accepted();
            let fault = inject_fault(point);
            let result =
                store.replace_if_revision(ResidentSandboxCatalogRevision::new(1).unwrap(), &next);
            assert!(result.is_err(), "fault {point:?} should fail");
            drop(fault);
            let recovery = store
                .recover()
                .unwrap_or_else(|error| panic!("fault {point:?} was not recoverable: {error:?}"));
            assert_eq!(
                recovery,
                if matches!(
                    point,
                    FaultPoint::AfterRenameBeforeParentSync | FaultPoint::ParentSync
                ) {
                    ResidentSandboxRecoveryDisposition::RemovedStaleStaged
                } else {
                    ResidentSandboxRecoveryDisposition::Clean
                }
            );
            let observed = store
                .load()
                .unwrap_or_else(|error| panic!("fault {point:?} left unreadable state: {error:?}"));
            if matches!(
                point,
                FaultPoint::AfterRenameBeforeParentSync | FaultPoint::ParentSync
            ) {
                assert_eq!(observed, Some(next));
            } else {
                assert_eq!(observed, Some(ResidentSandboxCatalog::empty()));
            }
            assert_stage_absent(&root);
        }
    }

    #[test]
    fn recovery_publication_fault_preserves_stage_and_then_publishes() {
        let root = TempRoot::new("recovery-publication");
        let mut store = open_store(&root);
        store.create(&ResidentSandboxCatalog::empty()).unwrap();
        let (next, _) = accepted();
        stage_without_cleanup(&store, &next);
        let _fault = inject_fault(FaultPoint::RecoveryPublication);
        assert_eq!(
            store.recover().unwrap_err().kind(),
            ResidentSandboxStoreErrorKind::Io
        );
        assert!(root.staged().exists());
        assert_eq!(
            store.recover().unwrap(),
            ResidentSandboxRecoveryDisposition::PublishedStaged
        );
        assert_eq!(store.load().unwrap(), Some(next));
    }

    #[test]
    fn every_operation_checkpoint_publication_fault_preserves_exact_truth() {
        for operation in [
            TestOperationKind::Materialize,
            TestOperationKind::Start,
            TestOperationKind::Stop,
        ] {
            for checkpoint_kind in [
                TestCheckpointKind::Authorized,
                TestCheckpointKind::PrestartFailed,
                TestCheckpointKind::Started,
                TestCheckpointKind::RecoveryRequired,
            ] {
                for point in [
                    FaultPoint::StageCreate,
                    FaultPoint::StageOpen,
                    FaultPoint::StageWrite,
                    FaultPoint::StagePartialWrite,
                    FaultPoint::StageFileSync,
                    FaultPoint::StageReopenValidation,
                    FaultPoint::BeforeRename,
                    FaultPoint::AfterRenameBeforeParentSync,
                    FaultPoint::ParentSync,
                ] {
                    let root = TempRoot::new("operation-checkpoint-publication-matrix");
                    let mut store = open_store(&root);
                    let (predecessor, successor, key) =
                        operation_checkpoint_fixture(operation, checkpoint_kind);
                    install_test_catalog(&store, &predecessor);

                    let fault = inject_fault(point);
                    let result = store.replace_if_revision(predecessor.revision(), &successor);
                    assert!(
                        result.is_err(),
                        "{operation:?} {checkpoint_kind:?} fault {point:?} should fail"
                    );
                    drop(fault);

                    let recovery = store.recover().unwrap_or_else(|error| {
                        panic!(
                            "{operation:?} {checkpoint_kind:?} fault {point:?} was not recoverable: {error:?}"
                        )
                    });
                    let after_exchange = matches!(
                        point,
                        FaultPoint::AfterRenameBeforeParentSync | FaultPoint::ParentSync
                    );
                    assert_eq!(
                        recovery,
                        if after_exchange {
                            ResidentSandboxRecoveryDisposition::RemovedStaleStaged
                        } else {
                            ResidentSandboxRecoveryDisposition::Clean
                        }
                    );
                    let observed = store.load().expect("load exact post-fault truth").unwrap();
                    let expected = if after_exchange {
                        &successor
                    } else {
                        &predecessor
                    };
                    assert_eq!(&observed, expected);
                    assert_operation_identity(&observed, expected, &key);
                    assert_stage_absent(&root);
                    assert_no_external_callbacks(0);
                }
            }
        }
    }

    #[test]
    fn recovery_required_successors_recover_for_each_operation_family() {
        for kind in [
            TestOperationKind::Materialize,
            TestOperationKind::Start,
            TestOperationKind::Stop,
        ] {
            for point in [
                FaultPoint::RecoveryPublication,
                FaultPoint::StageReopenValidation,
                FaultPoint::BeforeRename,
                FaultPoint::AfterRenameBeforeParentSync,
                FaultPoint::ParentSync,
            ] {
                let root = TempRoot::new("recovery-required-operation-matrix");
                let mut store = open_store(&root);
                let (initial, key) = operation_fixture(kind);
                install_test_catalog(&store, &initial);
                let authorized = authorize_operation(&initial, &key, kind);
                store
                    .replace_if_revision(initial.revision(), &authorized)
                    .expect("publish authorized checkpoint");
                let started = start_operation(&authorized, &key, kind);
                store
                    .replace_if_revision(authorized.revision(), &started)
                    .expect("publish started checkpoint");
                let recovery_required = checkpoint(
                    &started,
                    &key,
                    match kind {
                        TestOperationKind::Materialize => {
                            ResidentSandboxCheckpoint::MaterializeRecoveryRequired
                        }
                        TestOperationKind::Start => {
                            ResidentSandboxCheckpoint::StartRecoveryRequired
                        }
                        TestOperationKind::Stop => ResidentSandboxCheckpoint::StopRecoveryRequired,
                    },
                );
                stage_without_cleanup(&store, &recovery_required);

                let fault = inject_fault(point);
                assert!(
                    store.recover().is_err(),
                    "{kind:?} fault {point:?} should fail"
                );
                drop(fault);

                let recovery = store.recover().expect("retry recovery-required stage");
                let observed = store
                    .load()
                    .expect("load recovery-required catalog")
                    .unwrap();
                let expected_disposition = if matches!(
                    point,
                    FaultPoint::AfterRenameBeforeParentSync | FaultPoint::ParentSync
                ) {
                    ResidentSandboxRecoveryDisposition::RemovedStaleStaged
                } else {
                    ResidentSandboxRecoveryDisposition::PublishedStaged
                };
                assert_eq!(recovery, expected_disposition);
                assert_operation_identity(&observed, &recovery_required, &key);
                assert_eq!(
                    observed.find(&key).unwrap().physical(),
                    recovery_required.find(&key).unwrap().physical()
                );
                assert_stage_absent(&root);
                assert_no_external_callbacks(0);
            }
        }
    }

    #[test]
    fn prestart_failed_successors_recover_for_each_operation_family() {
        for kind in [
            TestOperationKind::Materialize,
            TestOperationKind::Start,
            TestOperationKind::Stop,
        ] {
            for point in [
                FaultPoint::RecoveryPublication,
                FaultPoint::StageReopenValidation,
                FaultPoint::BeforeRename,
                FaultPoint::AfterRenameBeforeParentSync,
                FaultPoint::ParentSync,
            ] {
                let root = TempRoot::new("prestart-failed-operation-matrix");
                let mut store = open_store(&root);
                let (initial, key) = operation_fixture(kind);
                install_test_catalog(&store, &initial);
                let authorized = authorize_operation(&initial, &key, kind);
                store
                    .replace_if_revision(initial.revision(), &authorized)
                    .expect("publish authorized checkpoint");
                let prestart_failed = fail_prestart_operation(&authorized, &key, kind);
                stage_without_cleanup(&store, &prestart_failed);

                let fault = inject_fault(point);
                assert!(
                    store.recover().is_err(),
                    "{kind:?} fault {point:?} should fail"
                );
                drop(fault);

                let recovery = store.recover().expect("retry prestart-failed stage");
                assert_eq!(
                    recovery,
                    if matches!(
                        point,
                        FaultPoint::AfterRenameBeforeParentSync | FaultPoint::ParentSync
                    ) {
                        ResidentSandboxRecoveryDisposition::RemovedStaleStaged
                    } else {
                        ResidentSandboxRecoveryDisposition::PublishedStaged
                    }
                );
                let observed = store.load().expect("load prestart-failed catalog").unwrap();
                assert_eq!(observed, prestart_failed);
                assert_operation_identity(&observed, &prestart_failed, &key);
                assert!(matches!(
                    observed.find(&key).unwrap().active_operation(),
                    ResidentSandboxActiveOperation::None
                ));
                assert_stage_absent(&root);
                assert_no_external_callbacks(0);
            }
        }
    }

    #[test]
    fn recovery_publication_fault_matrix_preserves_exact_safe_successor() {
        for kind in [
            TestOperationKind::Materialize,
            TestOperationKind::Start,
            TestOperationKind::Stop,
        ] {
            for point in [
                FaultPoint::RecoveryPublication,
                FaultPoint::StageReopenValidation,
                FaultPoint::BeforeRename,
                FaultPoint::AfterRenameBeforeParentSync,
                FaultPoint::ParentSync,
            ] {
                let root = TempRoot::new("recovery-publication-matrix");
                let mut store = open_store(&root);
                let (initial, key) = operation_fixture(kind);
                install_test_catalog(&store, &initial);
                let authorized = authorize_operation(&initial, &key, kind);
                stage_without_cleanup(&store, &authorized);

                let fault = inject_fault(point);
                let result = store.recover();
                assert!(result.is_err(), "{kind:?} fault {point:?} should fail");
                drop(fault);

                if matches!(
                    point,
                    FaultPoint::AfterRenameBeforeParentSync | FaultPoint::ParentSync
                ) {
                    assert!(root.staged().exists());
                    assert!(fs::metadata(root.staged()).unwrap().len() > 0);
                    let observed = store
                        .load_current()
                        .expect("load current after recovery rename")
                        .unwrap();
                    assert_eq!(observed, authorized);
                    assert_operation_identity(&observed, &authorized, &key);
                    assert_eq!(
                        store.recover().expect("confirm recovered publication"),
                        ResidentSandboxRecoveryDisposition::RemovedStaleStaged
                    );
                } else {
                    assert!(root.staged().exists(), "safe staged successor must remain");
                    let observed = store
                        .load_current()
                        .expect("load predecessor before recovery retry")
                        .unwrap();
                    assert_eq!(observed, initial);
                    assert_operation_identity(&observed, &initial, &key);
                    assert_eq!(
                        store.recover().expect("retry safe recovery publication"),
                        ResidentSandboxRecoveryDisposition::PublishedStaged
                    );
                    assert_stage_absent(&root);
                }

                let observed = store.load().expect("load recovered successor").unwrap();
                assert_eq!(observed, authorized);
                assert_operation_identity(&observed, &authorized, &key);
                assert_no_external_callbacks(0);
            }
        }
    }

    #[test]
    fn started_stage_discard_fault_matrix_preserves_authorized_no_replay_boundary() {
        for kind in [
            TestOperationKind::Materialize,
            TestOperationKind::Start,
            TestOperationKind::Stop,
        ] {
            let root = TempRoot::new("started-discard-matrix");
            let mut store = open_store(&root);
            let (initial, key) = operation_fixture(kind);
            install_test_catalog(&store, &initial);
            let authorized = authorize_operation(&initial, &key, kind);
            store
                .replace_if_revision(initial.revision(), &authorized)
                .expect("publish callback-free authorization");
            let started = start_operation(&authorized, &key, kind);
            stage_without_cleanup(&store, &started);

            let fault = inject_fault(FaultPoint::StaleStageRemoval);
            let result = store.recover();
            assert!(result.is_err(), "{kind:?} stage removal fault should fail");
            drop(fault);
            assert!(
                root.staged().exists(),
                "started evidence must remain for retry"
            );
            let observed = store
                .load_current()
                .expect("load authorized predecessor")
                .unwrap();
            assert_eq!(observed, authorized);
            assert_operation_identity(&observed, &authorized, &key);

            assert_eq!(
                store
                    .recover()
                    .expect("retry discard of unpublished Started"),
                ResidentSandboxRecoveryDisposition::DiscardedStartedStaged
            );
            assert_stage_absent(&root);
            let observed = store.load().expect("load no-replay boundary").unwrap();
            assert_eq!(observed, authorized);
            assert_operation_identity(&observed, &authorized, &key);
            assert_no_external_callbacks(0);
        }
    }
}
