use std::fmt::Write as _;
use std::fs::File;
use std::io::{Read as _, Write as _};
use std::os::fd::{AsFd, OwnedFd};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use rustix::fs::{self, AtFlags, FileType, FlockOperation, Mode, OFlags, RenameFlags};
use rustix::io::Errno;

use crate::lease::{LEASE_SCHEMA_VERSION, LeaseRecord};
use crate::lease_catalog::{
    LeaseSelector, LeaseStore, LeaseStoreError, LeaseStoreErrorKind, LeaseWriteDisposition,
    LeaseWriteReceipt,
};
use crate::lease_document::{
    MAX_LEASE_DOCUMENT_BYTES, decode_lease_document, encode_lease_document,
};
use crate::state::InstallationId;

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
const LEASES_DIRECTORY: &str = "leases";
const LEASES_LOCK_FILE: &str = "leases.lock";
const LEASE_FILE_SUFFIX: &str = ".json";

static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(1);

/// Crash-safe file-backed lease store bound to one installation directory.
///
/// Authoritative lease documents live beneath `leases/LEASE_ID.json`. Mutations serialize through a
/// persistent `leases.lock`, write and synchronize a private temporary file in the same directory,
/// then publish it with one atomic rename. The store retains opened directory descriptors so later
/// operations cannot be redirected by replacing the installation path.
#[derive(Debug)]
pub struct LinuxLeaseStore {
    installation_id: InstallationId,
    installation: OwnedFd,
    leases: OwnedFd,
    owner: (u32, u32),
}

impl LinuxLeaseStore {
    /// Open one trusted installation directory and prepare its durable lease-store boundary.
    ///
    /// The installation directory must already exist as a real `0750` directory. This function
    /// creates or validates the `leases` directory and persistent empty `0600` lock file, synchronizes
    /// newly created metadata, and binds the returned store to the supplied installation identity.
    ///
    /// # Errors
    ///
    /// Returns `UnsafeFilesystem` for symlinks, hard links, incompatible object types, ownership, or
    /// permissions; `CorruptState` for nonempty lock state; and `Io` for bounded filesystem failures.
    pub fn open_or_create(
        installation_path: impl AsRef<Path>,
        installation_id: InstallationId,
    ) -> Result<Self, LeaseStoreError> {
        let installation = fs::open(installation_path.as_ref(), DIRECTORY_FLAGS, Mode::empty())
            .map_err(map_installation_open_error)?;
        let installation_stat = inspect_directory(&installation, "installation directory", None)?;
        let owner = (installation_stat.st_uid, installation_stat.st_gid);
        let leases = ensure_leases_directory(&installation, owner)?;
        ensure_lock_file(&installation, owner)?;

        Ok(Self {
            installation_id,
            installation,
            leases,
            owner,
        })
    }

    fn validate_selector(&self, selector: &LeaseSelector) -> Result<(), LeaseStoreError> {
        if selector.installation_id != self.installation_id {
            return Err(store_error(
                LeaseStoreErrorKind::CorruptState,
                "lease selector does not belong to this installation store",
            ));
        }
        Ok(())
    }

    fn validate_record(&self, record: &LeaseRecord) -> Result<LeaseSelector, LeaseStoreError> {
        if record.schema_version != LEASE_SCHEMA_VERSION {
            return Err(store_error(
                LeaseStoreErrorKind::CorruptState,
                "lease record uses an unsupported schema version",
            ));
        }
        let selector = LeaseSelector::from_identity(&record.identity);
        self.validate_selector(&selector)?;
        Ok(selector)
    }

    fn acquire_mutation_lock(&self) -> Result<LeaseMutationLock, LeaseStoreError> {
        let lock = fs::openat(
            &self.installation,
            LEASES_LOCK_FILE,
            EXISTING_LOCK_FLAGS,
            Mode::empty(),
        )
        .map_err(map_lock_open_error)?;
        inspect_private_file(&lock, self.owner, "lease-store lock", Some(0))?;
        match fs::flock(&lock, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(LeaseMutationLock { _lock: lock }),
            Err(Errno::AGAIN) => Err(store_error(
                LeaseStoreErrorKind::Busy,
                "another lease-store mutation holds the installation lock",
            )),
            Err(_) => Err(store_error(
                LeaseStoreErrorKind::Io,
                "could not acquire the lease-store mutation lock",
            )),
        }
    }

    fn load_locked(
        &self,
        selector: &LeaseSelector,
    ) -> Result<Option<LeaseRecord>, LeaseStoreError> {
        self.validate_selector(selector)?;
        let name = lease_file_name(selector);
        let file = match fs::openat(&self.leases, &name, EXISTING_FILE_FLAGS, Mode::empty()) {
            Ok(file) => file,
            Err(Errno::NOENT) => return Ok(None),
            Err(error) => return Err(map_lease_open_error(error)),
        };
        inspect_private_file(&file, self.owner, "lease document", None)?;

        let mut bytes = Vec::new();
        File::from(file)
            .take((MAX_LEASE_DOCUMENT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| {
                store_error(LeaseStoreErrorKind::Io, "could not read the lease document")
            })?;
        if bytes.len() > MAX_LEASE_DOCUMENT_BYTES {
            return Err(store_error(
                LeaseStoreErrorKind::CorruptState,
                "lease document exceeds the bounded document limit",
            ));
        }
        let record = decode_lease_document(&bytes).map_err(|_| {
            store_error(
                LeaseStoreErrorKind::CorruptState,
                "lease document is malformed or semantically invalid",
            )
        })?;
        if LeaseSelector::from_identity(&record.identity) != *selector {
            return Err(store_error(
                LeaseStoreErrorKind::CorruptState,
                "persisted lease identity does not match its file selector",
            ));
        }
        Ok(Some(record))
    }

    fn stage_document(
        &self,
        selector: &LeaseSelector,
        encoded: &[u8],
    ) -> Result<StagedLeaseDocument<'_>, LeaseStoreError> {
        let name = temporary_file_name(selector);
        let file = fs::openat(&self.leases, &name, NEW_FILE_FLAGS, PRIVATE_FILE_MODE)
            .map_err(map_temp_create_error)?;
        let mut staged = StagedLeaseDocument {
            parent: self.leases.as_fd(),
            file: Some(file),
            name,
            armed: true,
        };
        let opened = staged.file.as_ref().expect("staged file is present");
        fs::fchmod(opened, PRIVATE_FILE_MODE).map_err(|_| {
            store_error(
                LeaseStoreErrorKind::Io,
                "could not set private lease-document permissions",
            )
        })?;
        inspect_private_file(opened, self.owner, "staged lease document", Some(0))?;

        let mut file = File::from(staged.file.take().expect("staged file is present"));
        file.write_all(encoded).map_err(|_| {
            store_error(
                LeaseStoreErrorKind::Io,
                "could not write the staged lease document",
            )
        })?;
        file.sync_all().map_err(|_| {
            store_error(
                LeaseStoreErrorKind::Io,
                "could not synchronize the staged lease document",
            )
        })?;
        inspect_private_file(
            file.as_fd(),
            self.owner,
            "staged lease document",
            Some(encoded.len()),
        )?;
        staged.file = Some(file.into());
        Ok(staged)
    }
}

impl LeaseStore for LinuxLeaseStore {
    fn load(&self, selector: &LeaseSelector) -> Result<Option<LeaseRecord>, LeaseStoreError> {
        self.load_locked(selector)
    }

    fn create(&mut self, record: &LeaseRecord) -> Result<LeaseWriteReceipt, LeaseStoreError> {
        let selector = self.validate_record(record)?;
        let encoded = encode_lease_document(record).map_err(|_| {
            store_error(
                LeaseStoreErrorKind::CorruptState,
                "lease record could not be encoded for durable publication",
            )
        })?;
        let _lock = self.acquire_mutation_lock()?;
        if self.load_locked(&selector)?.is_some() {
            return Err(store_error(
                LeaseStoreErrorKind::AlreadyExists,
                "lease already exists",
            ));
        }

        let mut staged = self.stage_document(&selector, encoded.as_bytes())?;
        let final_name = lease_file_name(&selector);
        match fs::renameat_with(
            &self.leases,
            staged.name(),
            &self.leases,
            &final_name,
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => staged.disarm(),
            Err(Errno::EXIST) => {
                return Err(store_error(
                    LeaseStoreErrorKind::AlreadyExists,
                    "lease already exists",
                ));
            }
            Err(_) => {
                return Err(store_error(
                    LeaseStoreErrorKind::Io,
                    "could not atomically publish the new lease document",
                ));
            }
        }
        synchronize_directory(&self.leases, "lease directory")?;
        Ok(LeaseWriteReceipt::new(
            LeaseWriteDisposition::Created,
            record.revision,
        ))
    }

    fn replace_if_revision(
        &mut self,
        expected_revision: u64,
        record: &LeaseRecord,
    ) -> Result<LeaseWriteReceipt, LeaseStoreError> {
        let selector = self.validate_record(record)?;
        let next_revision = expected_revision.checked_add(1).ok_or_else(|| {
            store_error(
                LeaseStoreErrorKind::Conflict,
                "lease revision counter is exhausted",
            )
        })?;
        if record.revision != next_revision {
            return Err(store_error(
                LeaseStoreErrorKind::Conflict,
                "replacement revision must advance exactly once",
            ));
        }
        let encoded = encode_lease_document(record).map_err(|_| {
            store_error(
                LeaseStoreErrorKind::CorruptState,
                "lease record could not be encoded for durable publication",
            )
        })?;

        let _lock = self.acquire_mutation_lock()?;
        let current = self
            .load_locked(&selector)?
            .ok_or_else(|| store_error(LeaseStoreErrorKind::Missing, "lease does not exist"))?;
        if current.identity != record.identity {
            return Err(store_error(
                LeaseStoreErrorKind::CorruptState,
                "replacement lease identity differs from persisted identity",
            ));
        }
        if current.revision != expected_revision {
            return Err(store_error(
                LeaseStoreErrorKind::Conflict,
                format!(
                    "stale lease revision: expected {expected_revision}, current revision is {}",
                    current.revision
                ),
            ));
        }

        let mut staged = self.stage_document(&selector, encoded.as_bytes())?;
        let final_name = lease_file_name(&selector);
        fs::renameat_with(
            &self.leases,
            staged.name(),
            &self.leases,
            &final_name,
            RenameFlags::empty(),
        )
        .map_err(|_| {
            store_error(
                LeaseStoreErrorKind::Io,
                "could not atomically replace the lease document",
            )
        })?;
        staged.disarm();
        synchronize_directory(&self.leases, "lease directory")?;
        Ok(LeaseWriteReceipt::new(
            LeaseWriteDisposition::Replaced,
            record.revision,
        ))
    }
}

#[derive(Debug)]
struct LeaseMutationLock {
    _lock: OwnedFd,
}

struct StagedLeaseDocument<'a> {
    parent: std::os::fd::BorrowedFd<'a>,
    file: Option<OwnedFd>,
    name: String,
    armed: bool,
}

impl StagedLeaseDocument<'_> {
    fn name(&self) -> &str {
        &self.name
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StagedLeaseDocument<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::unlinkat(self.parent, &self.name, AtFlags::empty());
        }
    }
}

fn ensure_leases_directory(
    installation: &OwnedFd,
    owner: (u32, u32),
) -> Result<OwnedFd, LeaseStoreError> {
    match fs::openat(
        installation,
        LEASES_DIRECTORY,
        DIRECTORY_FLAGS,
        Mode::empty(),
    ) {
        Ok(directory) => {
            inspect_directory(&directory, "lease directory", Some(owner))?;
            Ok(directory)
        }
        Err(Errno::NOENT) => {
            let created = match fs::mkdirat(installation, LEASES_DIRECTORY, MANAGED_DIRECTORY_MODE)
            {
                Ok(()) => true,
                Err(Errno::EXIST) => false,
                Err(_) => {
                    return Err(store_error(
                        LeaseStoreErrorKind::Io,
                        "could not create the lease directory",
                    ));
                }
            };
            let directory = fs::openat(
                installation,
                LEASES_DIRECTORY,
                DIRECTORY_FLAGS,
                Mode::empty(),
            )
            .map_err(map_leases_open_error)?;
            if created {
                fs::fchmod(&directory, MANAGED_DIRECTORY_MODE).map_err(|_| {
                    store_error(
                        LeaseStoreErrorKind::Io,
                        "could not set lease-directory permissions",
                    )
                })?;
            }
            inspect_directory(&directory, "lease directory", Some(owner))?;
            if created {
                synchronize_directory(installation, "installation directory")?;
            }
            Ok(directory)
        }
        Err(error) => Err(map_leases_open_error(error)),
    }
}

fn ensure_lock_file(installation: &OwnedFd, owner: (u32, u32)) -> Result<(), LeaseStoreError> {
    match fs::openat(
        installation,
        LEASES_LOCK_FILE,
        NEW_LOCK_FLAGS,
        PRIVATE_FILE_MODE,
    ) {
        Ok(lock) => {
            let mut guard = CreatedLeaseLock {
                installation: installation.as_fd(),
                armed: true,
            };
            fs::fchmod(&lock, PRIVATE_FILE_MODE).map_err(|_| {
                store_error(
                    LeaseStoreErrorKind::Io,
                    "could not set lease-store lock permissions",
                )
            })?;
            inspect_private_file(&lock, owner, "lease-store lock", Some(0))?;
            fs::fsync(&lock).map_err(|_| {
                store_error(
                    LeaseStoreErrorKind::Io,
                    "could not synchronize the lease-store lock",
                )
            })?;
            synchronize_directory(installation, "installation directory")?;
            guard.armed = false;
            Ok(())
        }
        Err(Errno::EXIST) => {
            let lock = fs::openat(
                installation,
                LEASES_LOCK_FILE,
                EXISTING_LOCK_FLAGS,
                Mode::empty(),
            )
            .map_err(map_lock_open_error)?;
            inspect_private_file(&lock, owner, "lease-store lock", Some(0))
        }
        Err(error) => Err(map_lock_open_error(error)),
    }
}

struct CreatedLeaseLock<'a> {
    installation: std::os::fd::BorrowedFd<'a>,
    armed: bool,
}

impl Drop for CreatedLeaseLock<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::unlinkat(self.installation, LEASES_LOCK_FILE, AtFlags::empty());
        }
    }
}

fn inspect_directory(
    directory: &OwnedFd,
    subject: &str,
    expected_owner: Option<(u32, u32)>,
) -> Result<rustix::fs::Stat, LeaseStoreError> {
    let stat = fs::fstat(directory).map_err(|_| {
        store_error(
            LeaseStoreErrorKind::Io,
            format!("could not inspect {subject}"),
        )
    })?;
    if !FileType::from_raw_mode(stat.st_mode).is_dir() {
        return Err(store_error(
            LeaseStoreErrorKind::UnsafeFilesystem,
            format!("{subject} is not a directory"),
        ));
    }
    if stat.st_mode & 0o7777 != 0o750 {
        return Err(store_error(
            LeaseStoreErrorKind::UnsafeFilesystem,
            format!("{subject} does not have mode 0750"),
        ));
    }
    if expected_owner.is_some_and(|owner| owner != (stat.st_uid, stat.st_gid)) {
        return Err(store_error(
            LeaseStoreErrorKind::UnsafeFilesystem,
            format!("{subject} has an unexpected owner or group"),
        ));
    }
    Ok(stat)
}

fn inspect_private_file(
    file: impl AsFd,
    owner: (u32, u32),
    subject: &str,
    expected_size: Option<usize>,
) -> Result<(), LeaseStoreError> {
    let stat = fs::fstat(file.as_fd()).map_err(|_| {
        store_error(
            LeaseStoreErrorKind::Io,
            format!("could not inspect {subject}"),
        )
    })?;
    if !FileType::from_raw_mode(stat.st_mode).is_file() {
        return Err(store_error(
            LeaseStoreErrorKind::UnsafeFilesystem,
            format!("{subject} is not a regular file"),
        ));
    }
    if stat.st_nlink != 1 {
        return Err(store_error(
            LeaseStoreErrorKind::UnsafeFilesystem,
            format!("{subject} has multiple hard links"),
        ));
    }
    if stat.st_mode & 0o7777 != 0o600 {
        return Err(store_error(
            LeaseStoreErrorKind::UnsafeFilesystem,
            format!("{subject} does not have mode 0600"),
        ));
    }
    if owner != (stat.st_uid, stat.st_gid) {
        return Err(store_error(
            LeaseStoreErrorKind::UnsafeFilesystem,
            format!("{subject} has an unexpected owner or group"),
        ));
    }
    if expected_size
        .is_some_and(|expected| stat.st_size < 0 || stat.st_size as u64 != expected as u64)
    {
        return Err(store_error(
            LeaseStoreErrorKind::CorruptState,
            format!("{subject} has an unexpected size"),
        ));
    }
    Ok(())
}

fn synchronize_directory(directory: impl AsFd, subject: &str) -> Result<(), LeaseStoreError> {
    fs::fsync(directory.as_fd()).map_err(|_| {
        store_error(
            LeaseStoreErrorKind::Io,
            format!("could not synchronize {subject}"),
        )
    })
}

fn lease_file_name(selector: &LeaseSelector) -> String {
    format!("{}{LEASE_FILE_SUFFIX}", selector.lease_id.as_str())
}

fn temporary_file_name(selector: &LeaseSelector) -> String {
    let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
    let mut name = String::new();
    write!(
        &mut name,
        ".{}-{}-{sequence}.tmp",
        selector.lease_id.as_str(),
        std::process::id()
    )
    .expect("writing to String cannot fail");
    name
}

fn store_error(kind: LeaseStoreErrorKind, message: impl Into<String>) -> LeaseStoreError {
    LeaseStoreError::public(kind, message)
}

fn map_installation_open_error(error: Errno) -> LeaseStoreError {
    match error {
        Errno::LOOP | Errno::NOTDIR => store_error(
            LeaseStoreErrorKind::UnsafeFilesystem,
            "installation path is symlinked or is not a directory",
        ),
        Errno::NOENT => store_error(
            LeaseStoreErrorKind::Missing,
            "installation directory does not exist",
        ),
        _ => store_error(
            LeaseStoreErrorKind::Io,
            "could not open the installation directory",
        ),
    }
}

fn map_leases_open_error(error: Errno) -> LeaseStoreError {
    match error {
        Errno::LOOP | Errno::NOTDIR => store_error(
            LeaseStoreErrorKind::UnsafeFilesystem,
            "lease directory is symlinked or invalid",
        ),
        _ => store_error(
            LeaseStoreErrorKind::Io,
            "could not open the lease directory",
        ),
    }
}

fn map_lock_open_error(error: Errno) -> LeaseStoreError {
    match error {
        Errno::LOOP | Errno::NOTDIR | Errno::ISDIR => store_error(
            LeaseStoreErrorKind::UnsafeFilesystem,
            "lease-store lock is symlinked or invalid",
        ),
        _ => store_error(
            LeaseStoreErrorKind::Io,
            "could not open the lease-store lock",
        ),
    }
}

fn map_lease_open_error(error: Errno) -> LeaseStoreError {
    match error {
        Errno::LOOP | Errno::NOTDIR | Errno::ISDIR => store_error(
            LeaseStoreErrorKind::UnsafeFilesystem,
            "lease document is symlinked or invalid",
        ),
        _ => store_error(LeaseStoreErrorKind::Io, "could not open the lease document"),
    }
}

fn map_temp_create_error(error: Errno) -> LeaseStoreError {
    match error {
        Errno::EXIST => store_error(
            LeaseStoreErrorKind::Conflict,
            "temporary lease-document name collided",
        ),
        Errno::LOOP | Errno::NOTDIR | Errno::ISDIR => store_error(
            LeaseStoreErrorKind::UnsafeFilesystem,
            "temporary lease-document path is unsafe",
        ),
        _ => store_error(
            LeaseStoreErrorKind::Io,
            "could not create the staged lease document",
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::lease::{LeaseAction, LeaseId, LeaseIdentity, LeaseKind, LeaseRecord, LeaseState};
    use crate::lease_catalog::{
        LeaseCatalog, LeaseSelector, LeaseStore, LeaseStoreErrorKind, LeaseWriteDisposition,
    };
    use crate::state::InstallationId;

    use super::{LEASES_DIRECTORY, LEASES_LOCK_FILE, LinuxLeaseStore};

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TempInstallation(PathBuf);

    impl TempInstallation {
        fn new(label: &str) -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "smolrunner-linux-lease-store-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create temporary installation");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o750))
                .expect("set installation mode");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempInstallation {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn installation_id() -> InstallationId {
        InstallationId::parse("installation-001").expect("installation ID")
    }

    fn identity() -> LeaseIdentity {
        LeaseIdentity::new(
            LeaseId::parse("preview-pr-42").expect("lease ID"),
            installation_id(),
            LeaseKind::Preview,
        )
    }

    #[test]
    fn durable_store_reopens_and_preserves_revisioned_state() {
        let root = TempInstallation::new("reopen");
        let store = LinuxLeaseStore::open_or_create(root.path(), installation_id())
            .expect("open durable store");
        let mut catalog = LeaseCatalog::new(store);
        let (pending, created) = catalog.create(identity()).expect("create lease");
        assert_eq!(created.disposition, LeaseWriteDisposition::Created);
        let selector = LeaseSelector::from_identity(&pending.identity);
        let (active, replaced) = catalog
            .transition(&selector, 0, LeaseAction::Activate)
            .expect("activate lease");
        assert_eq!(active.state, LeaseState::Active);
        assert_eq!(replaced.revision, 1);
        drop(catalog);

        let reopened = LinuxLeaseStore::open_or_create(root.path(), installation_id())
            .expect("reopen durable store");
        assert_eq!(reopened.load(&selector).expect("load lease"), Some(active));
        assert!(root.path().join(LEASES_DIRECTORY).is_dir());
        assert!(root.path().join(LEASES_LOCK_FILE).is_file());
    }

    #[test]
    fn duplicate_create_and_stale_revision_fail_without_replacement() {
        let root = TempInstallation::new("conflict");
        let store = LinuxLeaseStore::open_or_create(root.path(), installation_id())
            .expect("open durable store");
        let mut catalog = LeaseCatalog::new(store);
        let (pending, _) = catalog.create(identity()).expect("create lease");
        let selector = LeaseSelector::from_identity(&pending.identity);
        let duplicate = catalog.create(identity()).expect_err("duplicate must fail");
        assert_eq!(duplicate.kind, LeaseStoreErrorKind::AlreadyExists);
        let (active, _) = catalog
            .transition(&selector, 0, LeaseAction::Activate)
            .expect("activate lease");
        let stale = catalog
            .transition(&selector, 0, LeaseAction::Expire)
            .expect_err("stale transition must fail");
        assert_eq!(stale.kind, LeaseStoreErrorKind::Conflict);
        assert_eq!(catalog.load(&selector).expect("load lease"), Some(active));
    }

    #[test]
    fn unsafe_lease_objects_fail_closed() {
        let root = TempInstallation::new("unsafe");
        let store = LinuxLeaseStore::open_or_create(root.path(), installation_id())
            .expect("open durable store");
        let selector = LeaseSelector::from_identity(&identity());
        let lease_path = root
            .path()
            .join(LEASES_DIRECTORY)
            .join("preview-pr-42.json");
        symlink(root.path(), &lease_path).expect("create lease symlink");
        let error = store.load(&selector).expect_err("symlink must fail");
        assert_eq!(error.kind, LeaseStoreErrorKind::UnsafeFilesystem);
    }

    #[test]
    fn concurrent_mutation_lock_reports_busy() {
        let root = TempInstallation::new("busy");
        let first = LinuxLeaseStore::open_or_create(root.path(), installation_id())
            .expect("open first durable store");
        let second = LinuxLeaseStore::open_or_create(root.path(), installation_id())
            .expect("open second durable store");
        let _guard = first
            .acquire_mutation_lock()
            .expect("acquire first mutation lock");
        let error = second
            .acquire_mutation_lock()
            .expect_err("second mutation lock must be busy");
        assert_eq!(error.kind, LeaseStoreErrorKind::Busy);
    }

    #[test]
    fn mismatched_installation_selector_is_rejected() {
        let root = TempInstallation::new("identity");
        let mut store = LinuxLeaseStore::open_or_create(root.path(), installation_id())
            .expect("open durable store");
        let foreign = LeaseRecord::pending(LeaseIdentity::new(
            LeaseId::parse("preview-pr-42").expect("lease ID"),
            InstallationId::parse("installation-002").expect("foreign installation ID"),
            LeaseKind::Preview,
        ));
        let error = store
            .create(&foreign)
            .expect_err("foreign identity must fail");
        assert_eq!(error.kind, LeaseStoreErrorKind::CorruptState);
    }
}
