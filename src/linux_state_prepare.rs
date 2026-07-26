use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::Path;

use rustix::fs::{self, AtFlags, FileType, Mode, OFlags};
use rustix::io::Errno;
use serde::Serialize;

use crate::state::{InstallationId, STATE_ROOT};
use crate::state_store::{StateStoreError, StateStoreErrorKind};

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const MANAGED_DIRECTORY_MODE: Mode = Mode::from_raw_mode(0o750);

/// Result of preparing one installation's durable state directories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct StatePreparationReceipt {
    created_directories: usize,
}

impl StatePreparationReceipt {
    #[must_use]
    pub const fn created_directories(&self) -> usize {
        self.created_directories
    }
}

/// Prepare one installation beneath the canonical system state root.
///
/// # Errors
///
/// Returns a bounded store error when the root or an existing directory is symlinked, has a
/// different owner or group, has a mode other than `0750`, or cannot be created and synchronized.
pub fn prepare_default_installation(
    installation_id: &InstallationId,
) -> Result<StatePreparationReceipt, StateStoreError> {
    prepare_installation(STATE_ROOT, installation_id)
}

/// Prepare one installation beneath an existing trusted state root.
///
/// The root itself must already exist with mode `0750`. This function creates only the fixed
/// `installations/ID/resources`, `installations/ID/journals`, and
/// `installations/ID/receipts` hierarchy. Existing compatible
/// directories are retained, while incompatible state is protected from mutation.
///
/// # Errors
///
/// Returns a bounded store error when the root or an existing directory is symlinked, has a
/// different owner or group, has a mode other than `0750`, or cannot be created and synchronized.
pub fn prepare_installation(
    root_path: impl AsRef<Path>,
    installation_id: &InstallationId,
) -> Result<StatePreparationReceipt, StateStoreError> {
    let root = fs::open(root_path.as_ref(), DIRECTORY_FLAGS, Mode::empty())
        .map_err(map_root_open_error)?;
    let root_stat = inspect_managed_directory(&root, "state root", None)?;
    let owner = (root_stat.st_uid, root_stat.st_gid);

    let (installations, installations_created) = ensure_directory(&root, "installations", owner)?;
    let (installation, installation_created) =
        ensure_directory(&installations, installation_id.as_str(), owner)?;
    let (_, resources_created) = ensure_directory(&installation, "resources", owner)?;
    let (_, journals_created) = ensure_directory(&installation, "journals", owner)?;
    let (_, receipts_created) = ensure_directory(&installation, "receipts", owner)?;

    Ok(StatePreparationReceipt {
        created_directories: [
            installations_created,
            installation_created,
            resources_created,
            journals_created,
            receipts_created,
        ]
        .into_iter()
        .filter(|created| *created)
        .count(),
    })
}

fn ensure_directory(
    parent: &OwnedFd,
    name: &str,
    owner: (u32, u32),
) -> Result<(OwnedFd, bool), StateStoreError> {
    match fs::openat(parent, name, DIRECTORY_FLAGS, Mode::empty()) {
        Ok(directory) => {
            inspect_managed_directory(&directory, "existing state directory", Some(owner))?;
            Ok((directory, false))
        }
        Err(Errno::NOENT) => create_directory(parent, name, owner),
        Err(error) => Err(map_directory_open_error(error)),
    }
}

fn create_directory(
    parent: &OwnedFd,
    name: &str,
    owner: (u32, u32),
) -> Result<(OwnedFd, bool), StateStoreError> {
    match fs::mkdirat(parent, name, MANAGED_DIRECTORY_MODE) {
        Ok(()) => {
            let mut guard = CreatedDirectory::new(parent.as_fd(), name.to_owned());
            let directory = fs::openat(parent, name, DIRECTORY_FLAGS, Mode::empty())
                .map_err(map_directory_open_error)?;
            fs::fchmod(&directory, MANAGED_DIRECTORY_MODE).map_err(|_| {
                StateStoreError::public(
                    StateStoreErrorKind::Io,
                    "could not set managed state-directory permissions",
                )
            })?;
            inspect_managed_directory(&directory, "new state directory", Some(owner))?;
            fs::fsync(parent).map_err(|_| {
                StateStoreError::public(
                    StateStoreErrorKind::Io,
                    "could not synchronize a new state-directory parent",
                )
            })?;
            guard.disarm();
            Ok((directory, true))
        }
        Err(Errno::EXIST) => {
            let directory = fs::openat(parent, name, DIRECTORY_FLAGS, Mode::empty())
                .map_err(map_directory_open_error)?;
            inspect_managed_directory(&directory, "existing state directory", Some(owner))?;
            Ok((directory, false))
        }
        Err(error) => Err(map_directory_create_error(error)),
    }
}

fn inspect_managed_directory(
    directory: &OwnedFd,
    subject: &str,
    expected_owner: Option<(u32, u32)>,
) -> Result<rustix::fs::Stat, StateStoreError> {
    let stat = fs::fstat(directory).map_err(|_| {
        StateStoreError::public(
            StateStoreErrorKind::Io,
            format!("could not inspect {subject}"),
        )
    })?;
    if !FileType::from_raw_mode(stat.st_mode).is_dir() {
        return Err(StateStoreError::public(
            StateStoreErrorKind::UnsafeFilesystem,
            format!("{subject} is not a directory"),
        ));
    }
    if stat.st_mode & 0o7777 != 0o750 {
        return Err(StateStoreError::public(
            StateStoreErrorKind::UnsafeFilesystem,
            format!("{subject} does not have mode 0750"),
        ));
    }
    if expected_owner.is_some_and(|(uid, gid)| stat.st_uid != uid || stat.st_gid != gid) {
        return Err(StateStoreError::public(
            StateStoreErrorKind::UnsafeFilesystem,
            format!("{subject} has an unexpected owner or group"),
        ));
    }
    Ok(stat)
}

struct CreatedDirectory<'a> {
    parent: BorrowedFd<'a>,
    name: String,
    armed: bool,
}

impl<'a> CreatedDirectory<'a> {
    fn new(parent: BorrowedFd<'a>, name: String) -> Self {
        Self {
            parent,
            name,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CreatedDirectory<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::unlinkat(self.parent, &self.name, AtFlags::REMOVEDIR);
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

fn map_directory_open_error(error: Errno) -> StateStoreError {
    match error {
        Errno::LOOP | Errno::NOTDIR => StateStoreError::public(
            StateStoreErrorKind::UnsafeFilesystem,
            "state directory is symlinked or not a directory",
        ),
        _ => StateStoreError::public(StateStoreErrorKind::Io, "could not open state directory"),
    }
}

fn map_directory_create_error(error: Errno) -> StateStoreError {
    match error {
        Errno::LOOP | Errno::NOTDIR => StateStoreError::public(
            StateStoreErrorKind::UnsafeFilesystem,
            "state-directory parent is symlinked or invalid",
        ),
        _ => StateStoreError::public(StateStoreErrorKind::Io, "could not create state directory"),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::state::InstallationId;
    use crate::state_store::StateStoreErrorKind;

    use super::prepare_installation;

    static NEXT_TEMP_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TempRoot {
        path: PathBuf,
    }

    impl TempRoot {
        fn new(label: &str) -> Self {
            let sequence = NEXT_TEMP_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "smolrunner-prepare-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create temporary state root");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o750))
                .expect("set state-root mode");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn installation_id() -> InstallationId {
        InstallationId::parse("0123456789abcdef").expect("installation ID")
    }

    fn assert_managed_directory(path: &Path, expected_owner: (u32, u32)) {
        let metadata = fs::metadata(path).expect("managed directory metadata");
        assert!(metadata.is_dir());
        assert_eq!(metadata.mode() & 0o7777, 0o750);
        assert_eq!((metadata.uid(), metadata.gid()), expected_owner);
    }

    #[test]
    fn creates_the_fixed_installation_tree_with_restrictive_modes() {
        let root = TempRoot::new("create");
        let root_metadata = fs::metadata(root.path()).expect("root metadata");
        let root_owner = (root_metadata.uid(), root_metadata.gid());
        let receipt = prepare_installation(root.path(), &installation_id())
            .expect("prepare installation state");
        assert_eq!(receipt.created_directories(), 5);

        let installations = root.path().join("installations");
        let installation = installations.join(installation_id().as_str());
        assert_managed_directory(&installations, root_owner);
        assert_managed_directory(&installation, root_owner);
        assert_managed_directory(&installation.join("resources"), root_owner);
        assert_managed_directory(&installation.join("journals"), root_owner);
        assert_managed_directory(&installation.join("receipts"), root_owner);

        let receipt = prepare_installation(root.path(), &installation_id())
            .expect("repeat installation preparation");
        assert_eq!(receipt.created_directories(), 0);
    }

    #[test]
    fn completes_a_compatible_partial_tree() {
        let root = TempRoot::new("partial");
        let installations = root.path().join("installations");
        fs::create_dir(&installations).expect("create installations directory");
        fs::set_permissions(&installations, fs::Permissions::from_mode(0o750))
            .expect("set installations mode");

        let receipt =
            prepare_installation(root.path(), &installation_id()).expect("complete partial tree");
        assert_eq!(receipt.created_directories(), 4);
    }

    #[test]
    fn symlinked_or_broad_existing_directories_are_protected() {
        let root = TempRoot::new("protected");
        let outside = TempRoot::new("outside");
        symlink(outside.path(), root.path().join("installations"))
            .expect("create installations symlink");
        let error = prepare_installation(root.path(), &installation_id())
            .expect_err("symlinked state directory must fail");
        assert_eq!(error.kind(), StateStoreErrorKind::UnsafeFilesystem);
        fs::remove_file(root.path().join("installations")).expect("remove symlink");

        let installations = root.path().join("installations");
        fs::create_dir(&installations).expect("create broad directory");
        fs::set_permissions(&installations, fs::Permissions::from_mode(0o755))
            .expect("set broad mode");
        let error = prepare_installation(root.path(), &installation_id())
            .expect_err("broad directory mode must fail");
        assert_eq!(error.kind(), StateStoreErrorKind::UnsafeFilesystem);
        assert_eq!(
            fs::metadata(installations)
                .expect("broad directory metadata")
                .mode()
                & 0o7777,
            0o755
        );
    }

    #[test]
    fn broad_state_root_mode_is_rejected_without_mutation() {
        let root = TempRoot::new("broad-root");
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755))
            .expect("set broad root mode");
        let error = prepare_installation(root.path(), &installation_id())
            .expect_err("broad root mode must fail");
        assert_eq!(error.kind(), StateStoreErrorKind::UnsafeFilesystem);
        assert!(!root.path().join("installations").exists());
    }
}
