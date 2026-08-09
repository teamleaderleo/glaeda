use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::Path;

use rustix::fs::{self, AtFlags, FileType, FlockOperation, Mode, OFlags};
use rustix::io::Errno;

use crate::state::STATE_ROOT;
use crate::state_store::{StateStoreError, StateStoreErrorKind};

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const EXISTING_LOCK_FLAGS: OFlags = OFlags::RDWR.union(OFlags::NOFOLLOW).union(OFlags::CLOEXEC);
const NEW_LOCK_FLAGS: OFlags = EXISTING_LOCK_FLAGS
    .union(OFlags::CREATE)
    .union(OFlags::EXCL);
const MANAGED_DIRECTORY_MODE: u32 = 0o750;
const PRIVATE_FILE_MODE: Mode = Mode::RUSR.union(Mode::WUSR);
const LOCK_FILE_NAME: &str = "catalog.lock";

/// Held nonblocking lock for installation-catalog discovery and creation.
#[derive(Debug)]
pub struct InstallationCatalogLock {
    root: OwnedFd,
    owner: (u32, u32),
    _lock: OwnedFd,
}

impl InstallationCatalogLock {
    pub(crate) fn root(&self) -> BorrowedFd<'_> {
        self.root.as_fd()
    }

    pub(crate) const fn owner(&self) -> (u32, u32) {
        self.owner
    }
}

impl Drop for InstallationCatalogLock {
    fn drop(&mut self) {
        // Release the shared open-file description even if a concurrent fork temporarily retains
        // a duplicate until exec applies `CLOEXEC`.
        let _ = fs::flock(&self._lock, FlockOperation::Unlock);
    }
}

/// Acquire the installation-catalog lock beneath the canonical system state root.
///
/// # Errors
///
/// Returns `Busy` when another catalog operation holds the lock, `UnsafeFilesystem` for symlinks,
/// hard links, incompatible ownership or permissions, and `Io` for bounded filesystem failures.
pub fn lock_default_installation_catalog() -> Result<InstallationCatalogLock, StateStoreError> {
    lock_installation_catalog(STATE_ROOT)
}

/// Acquire the installation-catalog lock beneath one existing trusted state root.
///
/// The lock file is persistent, empty, mode `0600`, and owned by the same UID and GID as the
/// restrictive `0750` state root. The returned guard retains the exact opened root descriptor, so
/// follow-on catalog operations cannot be redirected by replacing the root path. Dropping the guard
/// releases the advisory lock without deleting the file.
///
/// # Errors
///
/// Returns `Busy` when another catalog operation holds the lock, `UnsafeFilesystem` for symlinks,
/// hard links, incompatible ownership or permissions, and `Io` for bounded filesystem failures.
pub fn lock_installation_catalog(
    root_path: impl AsRef<Path>,
) -> Result<InstallationCatalogLock, StateStoreError> {
    let root = fs::open(root_path.as_ref(), DIRECTORY_FLAGS, Mode::empty())
        .map_err(map_root_open_error)?;
    let root_stat = inspect_root(&root)?;
    let owner = (root_stat.st_uid, root_stat.st_gid);
    let lock = open_catalog_lock(&root, owner)?;

    match fs::flock(&lock, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(InstallationCatalogLock {
            root,
            owner,
            _lock: lock,
        }),
        Err(Errno::AGAIN) => Err(StateStoreError::public(
            StateStoreErrorKind::Busy,
            "another installation-catalog operation holds the lock",
        )),
        Err(_) => Err(StateStoreError::public(
            StateStoreErrorKind::Io,
            "could not acquire the installation-catalog lock",
        )),
    }
}

fn open_catalog_lock(root: &OwnedFd, owner: (u32, u32)) -> Result<OwnedFd, StateStoreError> {
    match fs::openat(root, LOCK_FILE_NAME, NEW_LOCK_FLAGS, PRIVATE_FILE_MODE) {
        Ok(lock) => finish_new_lock(root, lock, owner),
        Err(Errno::EXIST) => open_existing_lock(root, owner),
        Err(error) => Err(map_lock_open_error(error)),
    }
}

fn finish_new_lock(
    root: &OwnedFd,
    lock: OwnedFd,
    owner: (u32, u32),
) -> Result<OwnedFd, StateStoreError> {
    let mut guard = CreatedLock::new(root.as_fd());
    fs::fchmod(&lock, PRIVATE_FILE_MODE).map_err(|_| {
        StateStoreError::public(
            StateStoreErrorKind::Io,
            "could not set installation-catalog lock permissions",
        )
    })?;
    inspect_lock(&lock, owner)?;
    fs::fsync(&lock).map_err(|_| {
        StateStoreError::public(
            StateStoreErrorKind::Io,
            "could not synchronize the installation-catalog lock",
        )
    })?;
    fs::fsync(root).map_err(|_| {
        StateStoreError::public(
            StateStoreErrorKind::Io,
            "could not synchronize the installation-catalog lock parent",
        )
    })?;
    guard.disarm();
    Ok(lock)
}

fn open_existing_lock(root: &OwnedFd, owner: (u32, u32)) -> Result<OwnedFd, StateStoreError> {
    let lock = fs::openat(root, LOCK_FILE_NAME, EXISTING_LOCK_FLAGS, Mode::empty())
        .map_err(map_lock_open_error)?;
    inspect_lock(&lock, owner)?;
    Ok(lock)
}

fn inspect_root(root: &OwnedFd) -> Result<rustix::fs::Stat, StateStoreError> {
    let stat = fs::fstat(root).map_err(|_| {
        StateStoreError::public(StateStoreErrorKind::Io, "could not inspect the state root")
    })?;
    if !FileType::from_raw_mode(stat.st_mode).is_dir() {
        return Err(StateStoreError::public(
            StateStoreErrorKind::UnsafeFilesystem,
            "state root is not a directory",
        ));
    }
    if stat.st_mode & 0o7777 != MANAGED_DIRECTORY_MODE {
        return Err(StateStoreError::public(
            StateStoreErrorKind::UnsafeFilesystem,
            "state root does not have mode 0750",
        ));
    }
    Ok(stat)
}

fn inspect_lock(lock: &OwnedFd, owner: (u32, u32)) -> Result<(), StateStoreError> {
    let stat = fs::fstat(lock).map_err(|_| {
        StateStoreError::public(
            StateStoreErrorKind::Io,
            "could not inspect the installation-catalog lock",
        )
    })?;
    if !FileType::from_raw_mode(stat.st_mode).is_file() {
        return Err(StateStoreError::public(
            StateStoreErrorKind::UnsafeFilesystem,
            "installation-catalog lock is not a regular file",
        ));
    }
    if stat.st_nlink != 1 {
        return Err(StateStoreError::public(
            StateStoreErrorKind::UnsafeFilesystem,
            "installation-catalog lock has multiple hard links",
        ));
    }
    if stat.st_mode & 0o7777 != 0o600 {
        return Err(StateStoreError::public(
            StateStoreErrorKind::UnsafeFilesystem,
            "installation-catalog lock does not have mode 0600",
        ));
    }
    if owner != (stat.st_uid, stat.st_gid) {
        return Err(StateStoreError::public(
            StateStoreErrorKind::UnsafeFilesystem,
            "installation-catalog lock has an unexpected owner or group",
        ));
    }
    if stat.st_size != 0 {
        return Err(StateStoreError::public(
            StateStoreErrorKind::CorruptState,
            "installation-catalog lock contains unexpected data",
        ));
    }
    Ok(())
}

struct CreatedLock<'a> {
    root: BorrowedFd<'a>,
    armed: bool,
}

impl<'a> CreatedLock<'a> {
    fn new(root: BorrowedFd<'a>) -> Self {
        Self { root, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CreatedLock<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::unlinkat(self.root, LOCK_FILE_NAME, AtFlags::empty());
        }
    }
}

fn map_root_open_error(error: Errno) -> StateStoreError {
    match error {
        Errno::LOOP | Errno::NOTDIR => StateStoreError::public(
            StateStoreErrorKind::UnsafeFilesystem,
            "state root is symlinked or not a directory",
        ),
        Errno::NOENT => {
            StateStoreError::public(StateStoreErrorKind::Io, "state root does not exist")
        }
        _ => StateStoreError::public(StateStoreErrorKind::Io, "could not open state root"),
    }
}

fn map_lock_open_error(error: Errno) -> StateStoreError {
    match error {
        Errno::ISDIR | Errno::LOOP | Errno::NOTDIR => StateStoreError::public(
            StateStoreErrorKind::UnsafeFilesystem,
            "installation-catalog lock is symlinked or invalid",
        ),
        _ => StateStoreError::public(
            StateStoreErrorKind::Io,
            "could not open the installation-catalog lock",
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use rustix::io::dup;

    use crate::state_store::StateStoreErrorKind;

    use super::{LOCK_FILE_NAME, lock_installation_catalog};

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(label: &str) -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "smolrunner-catalog-lock-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create temporary state root");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o750))
                .expect("set state-root mode");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn creates_and_reuses_a_private_persistent_lock() {
        let root = TempRoot::new("persistent");
        let root_metadata = fs::metadata(root.path()).expect("root metadata");
        let expected_owner = (root_metadata.uid(), root_metadata.gid());

        let lock = lock_installation_catalog(root.path()).expect("acquire catalog lock");
        let path = root.path().join(LOCK_FILE_NAME);
        let metadata = fs::metadata(&path).expect("catalog-lock metadata");
        assert!(metadata.is_file());
        assert_eq!(metadata.mode() & 0o7777, 0o600);
        assert_eq!(metadata.nlink(), 1);
        assert_eq!(metadata.len(), 0);
        assert_eq!((metadata.uid(), metadata.gid()), expected_owner);

        drop(lock);
        lock_installation_catalog(root.path()).expect("reacquire persistent catalog lock");
    }

    #[test]
    fn concurrent_catalog_lock_returns_busy() {
        let root = TempRoot::new("busy");
        let _lock = lock_installation_catalog(root.path()).expect("acquire first catalog lock");
        let error = lock_installation_catalog(root.path()).expect_err("second lock must be busy");
        assert_eq!(error.kind(), StateStoreErrorKind::Busy);
    }

    #[test]
    fn catalog_lock_drop_unlocks_an_inherited_open_file_description() {
        let root = TempRoot::new("inherited");
        let guard = lock_installation_catalog(root.path()).expect("acquire catalog lock");
        let inherited = dup(&guard._lock).expect("duplicate inherited lock descriptor");

        drop(guard);
        let reacquired = lock_installation_catalog(root.path())
            .expect("guard drop must explicitly unlock inherited description");

        drop(reacquired);
        drop(inherited);
    }

    #[test]
    fn symlinked_broad_or_directory_lock_is_rejected() {
        let root = TempRoot::new("unsafe");
        let outside = TempRoot::new("outside");
        let lock_path = root.path().join(LOCK_FILE_NAME);
        symlink(outside.path(), &lock_path).expect("create catalog-lock symlink");
        let error = lock_installation_catalog(root.path()).expect_err("symlink must fail");
        assert_eq!(error.kind(), StateStoreErrorKind::UnsafeFilesystem);
        fs::remove_file(&lock_path).expect("remove catalog-lock symlink");

        fs::write(&lock_path, []).expect("create broad catalog lock");
        fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o644))
            .expect("set broad catalog-lock mode");
        let error = lock_installation_catalog(root.path()).expect_err("broad mode must fail");
        assert_eq!(error.kind(), StateStoreErrorKind::UnsafeFilesystem);
        fs::remove_file(&lock_path).expect("remove broad catalog lock");

        fs::create_dir(&lock_path).expect("create catalog-lock directory");
        let error = lock_installation_catalog(root.path()).expect_err("directory must fail");
        assert_eq!(error.kind(), StateStoreErrorKind::UnsafeFilesystem);
    }

    #[test]
    fn hard_linked_or_nonempty_lock_is_rejected() {
        let root = TempRoot::new("identity");
        let lock = lock_installation_catalog(root.path()).expect("create catalog lock");
        drop(lock);
        let lock_path = root.path().join(LOCK_FILE_NAME);
        let alias = root.path().join("catalog-lock-alias");
        fs::hard_link(&lock_path, &alias).expect("create hard link");
        let error = lock_installation_catalog(root.path()).expect_err("hard link must fail");
        assert_eq!(error.kind(), StateStoreErrorKind::UnsafeFilesystem);
        fs::remove_file(alias).expect("remove hard link");

        fs::write(&lock_path, b"unexpected").expect("write unexpected lock data");
        let error = lock_installation_catalog(root.path()).expect_err("nonempty lock must fail");
        assert_eq!(error.kind(), StateStoreErrorKind::CorruptState);
    }

    #[test]
    fn broad_state_root_is_rejected_without_creating_a_lock() {
        let root = TempRoot::new("broad-root");
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755))
            .expect("broaden state-root mode");
        let error = lock_installation_catalog(root.path()).expect_err("broad root must fail");
        assert_eq!(error.kind(), StateStoreErrorKind::UnsafeFilesystem);
        assert!(!root.path().join(LOCK_FILE_NAME).exists());
    }
}
