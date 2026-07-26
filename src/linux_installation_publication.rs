use std::fmt;
use std::fs::File;
use std::io::Write as _;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use rustix::fs::{self, AtFlags, FileType, Mode, OFlags, RenameFlags};
use rustix::io::Errno;
use serde::Serialize;

use crate::linux_installation_catalog_lock::InstallationCatalogLock;
use crate::ownership::ProjectIdentity;
use crate::state::InstallationId;
use crate::state_document::{ProjectStateDocument, StateDocument, encode_state_document};

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const PROJECT_FILE_FLAGS: OFlags = OFlags::WRONLY
    .union(OFlags::CREATE)
    .union(OFlags::EXCL)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const MANAGED_DIRECTORY_MODE: Mode = Mode::from_raw_mode(0o750);
const PRIVATE_FILE_MODE: Mode = Mode::RUSR.union(Mode::WUSR);
const INSTALLATIONS_DIRECTORY: &str = "installations";
const STAGING_DIRECTORY: &str = "staging";
const RESOURCES_DIRECTORY: &str = "resources";
const JOURNALS_DIRECTORY: &str = "journals";
const RECEIPTS_DIRECTORY: &str = "receipts";
const PROJECT_FILE: &str = "project.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallationPublicationErrorKind {
    IdCollision,
    InvalidProject,
    Io,
    UnsafeFilesystem,
    CorruptState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstallationPublicationError {
    kind: InstallationPublicationErrorKind,
    public_message: String,
}

impl InstallationPublicationError {
    fn new(kind: InstallationPublicationErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            public_message: message.into(),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> InstallationPublicationErrorKind {
        self.kind
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.public_message
    }
}

impl fmt::Display for InstallationPublicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.public_message)
    }
}

impl std::error::Error for InstallationPublicationError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstallationPublicationReceipt {
    installation_id: InstallationId,
    bytes_written: usize,
}

impl InstallationPublicationReceipt {
    #[must_use]
    pub fn installation_id(&self) -> &InstallationId {
        &self.installation_id
    }

    #[must_use]
    pub const fn bytes_written(&self) -> usize {
        self.bytes_written
    }
}

/// Publish one complete new installation beneath the exact locked state root.
///
/// The function validates and encodes project state before touching the filesystem. It creates a
/// complete tree under `staging/INSTALLATION_ID`, synchronizes every new file and directory, then
/// atomically renames the staged directory to `installations/INSTALLATION_ID` with no replacement.
/// A failed pre-publication attempt removes only the newly staged tree. A failure after rename keeps
/// the published installation and returns a bounded durability error for later recovery.
///
/// # Errors
///
/// Returns `IdCollision` when either the staging or final installation ID already exists,
/// `InvalidProject` for malformed project identity, `UnsafeFilesystem` for symlinks or incompatible
/// state objects, `CorruptState` for unexpected staged state, and `Io` for bounded filesystem
/// failures.
pub fn publish_new_installation(
    catalog_lock: &InstallationCatalogLock,
    installation_id: InstallationId,
    project: ProjectIdentity,
) -> Result<InstallationPublicationReceipt, InstallationPublicationError> {
    let document = ProjectStateDocument::new(installation_id.clone(), project).map_err(|_| {
        invalid_project_error("project identity is invalid for installation publication")
    })?;
    let encoded = encode_state_document(&StateDocument::Project(document)).map_err(|_| {
        invalid_project_error("project state could not be encoded for installation publication")
    })?;

    inspect_directory(catalog_lock.root(), "state root", catalog_lock.owner())?;
    let installations = ensure_fixed_directory(
        catalog_lock.root(),
        INSTALLATIONS_DIRECTORY,
        catalog_lock.owner(),
    )?;
    let staging =
        ensure_fixed_directory(catalog_lock.root(), STAGING_DIRECTORY, catalog_lock.owner())?;
    let mut staged =
        create_staged_installation(staging.as_fd(), &installation_id, catalog_lock.owner())?;

    let resources = create_empty_directory(
        staged.directory(),
        RESOURCES_DIRECTORY,
        catalog_lock.owner(),
    )?;
    let journals =
        create_empty_directory(staged.directory(), JOURNALS_DIRECTORY, catalog_lock.owner())?;
    let receipts =
        create_empty_directory(staged.directory(), RECEIPTS_DIRECTORY, catalog_lock.owner())?;
    write_project_document(staged.directory(), encoded.as_bytes(), catalog_lock.owner())?;

    synchronize_directory(&resources, "resource state directory")?;
    synchronize_directory(&journals, "journal state directory")?;
    synchronize_directory(&receipts, "execution receipt directory")?;
    synchronize_directory(staged.directory(), "staged installation directory")?;

    fs::renameat_with(
        &staging,
        staged.name(),
        &installations,
        installation_id.as_str(),
        RenameFlags::NOREPLACE,
    )
    .map_err(map_publication_rename_error)?;
    staged.disarm();

    synchronize_directory(&installations, "installation catalog directory")?;
    synchronize_directory(&staging, "installation staging directory")?;

    Ok(InstallationPublicationReceipt {
        installation_id,
        bytes_written: encoded.len(),
    })
}

fn ensure_fixed_directory(
    parent: BorrowedFd<'_>,
    name: &str,
    owner: (u32, u32),
) -> Result<OwnedFd, InstallationPublicationError> {
    match fs::openat(parent, name, DIRECTORY_FLAGS, Mode::empty()) {
        Ok(directory) => {
            inspect_directory(directory.as_fd(), "existing catalog directory", owner)?;
            Ok(directory)
        }
        Err(Errno::NOENT) => create_fixed_directory(parent, name, owner),
        Err(error) => Err(map_directory_open_error(error)),
    }
}

fn create_fixed_directory(
    parent: BorrowedFd<'_>,
    name: &str,
    owner: (u32, u32),
) -> Result<OwnedFd, InstallationPublicationError> {
    match fs::mkdirat(parent, name, MANAGED_DIRECTORY_MODE) {
        Ok(()) => {
            let mut guard = CreatedDirectory::new(parent, name.to_owned());
            let directory = fs::openat(parent, name, DIRECTORY_FLAGS, Mode::empty())
                .map_err(map_directory_open_error)?;
            set_directory_mode(&directory)?;
            inspect_directory(directory.as_fd(), "new catalog directory", owner)?;
            synchronize_borrowed_directory(parent, "state root")?;
            guard.disarm();
            Ok(directory)
        }
        Err(Errno::EXIST) => {
            let directory = fs::openat(parent, name, DIRECTORY_FLAGS, Mode::empty())
                .map_err(map_directory_open_error)?;
            inspect_directory(directory.as_fd(), "existing catalog directory", owner)?;
            Ok(directory)
        }
        Err(error) => Err(map_directory_create_error(error)),
    }
}

fn create_staged_installation<'a>(
    staging: BorrowedFd<'a>,
    installation_id: &InstallationId,
    owner: (u32, u32),
) -> Result<StagedInstallation<'a>, InstallationPublicationError> {
    match fs::mkdirat(staging, installation_id.as_str(), MANAGED_DIRECTORY_MODE) {
        Ok(()) => {
            let name = installation_id.as_str().to_owned();
            let mut guard = CreatedDirectory::new(staging, name.clone());
            let directory = fs::openat(
                staging,
                installation_id.as_str(),
                DIRECTORY_FLAGS,
                Mode::empty(),
            )
            .map_err(map_directory_open_error)?;
            set_directory_mode(&directory)?;
            inspect_directory(directory.as_fd(), "staged installation directory", owner)?;
            synchronize_borrowed_directory(staging, "installation staging directory")?;
            guard.disarm();
            Ok(StagedInstallation {
                parent: staging,
                directory,
                name,
                armed: true,
            })
        }
        Err(Errno::EXIST) => Err(id_collision_error(
            "installation ID already exists in the staging area",
        )),
        Err(error) => Err(map_directory_create_error(error)),
    }
}

fn create_empty_directory(
    parent: &OwnedFd,
    name: &str,
    owner: (u32, u32),
) -> Result<OwnedFd, InstallationPublicationError> {
    match fs::mkdirat(parent, name, MANAGED_DIRECTORY_MODE) {
        Ok(()) => {
            let directory = fs::openat(parent, name, DIRECTORY_FLAGS, Mode::empty())
                .map_err(map_directory_open_error)?;
            set_directory_mode(&directory)?;
            inspect_directory(directory.as_fd(), "new installation directory", owner)?;
            Ok(directory)
        }
        Err(Errno::EXIST) => Err(corrupt_error(
            "new staged installation contains an unexpected existing directory",
        )),
        Err(error) => Err(map_directory_create_error(error)),
    }
}

fn write_project_document(
    installation: &OwnedFd,
    bytes: &[u8],
    owner: (u32, u32),
) -> Result<(), InstallationPublicationError> {
    let project = fs::openat(
        installation,
        PROJECT_FILE,
        PROJECT_FILE_FLAGS,
        PRIVATE_FILE_MODE,
    )
    .map_err(map_project_create_error)?;
    fs::fchmod(&project, PRIVATE_FILE_MODE)
        .map_err(|_| io_error("could not set private project-state permissions"))?;
    inspect_project_file(project.as_fd(), owner, 0)?;

    let mut project = File::from(project);
    project
        .write_all(bytes)
        .map_err(|_| io_error("could not write the staged project-state document"))?;
    fs::fsync(&project)
        .map_err(|_| io_error("could not synchronize the staged project-state document"))?;
    inspect_project_file(project.as_fd(), owner, bytes.len())
}

fn inspect_directory(
    directory: BorrowedFd<'_>,
    subject: &str,
    owner: (u32, u32),
) -> Result<(), InstallationPublicationError> {
    let stat =
        fs::fstat(directory).map_err(|_| io_error(format!("could not inspect {subject}")))?;
    if !FileType::from_raw_mode(stat.st_mode).is_dir() {
        return Err(unsafe_error(format!("{subject} is not a directory")));
    }
    if stat.st_mode & 0o7777 != 0o750 {
        return Err(unsafe_error(format!("{subject} does not have mode 0750")));
    }
    if owner != (stat.st_uid, stat.st_gid) {
        return Err(unsafe_error(format!(
            "{subject} has an unexpected owner or group"
        )));
    }
    Ok(())
}

fn inspect_project_file(
    project: BorrowedFd<'_>,
    owner: (u32, u32),
    expected_size: usize,
) -> Result<(), InstallationPublicationError> {
    let stat = fs::fstat(project)
        .map_err(|_| io_error("could not inspect the staged project-state document"))?;
    if !FileType::from_raw_mode(stat.st_mode).is_file() {
        return Err(unsafe_error(
            "staged project-state document is not a regular file",
        ));
    }
    if stat.st_nlink != 1 {
        return Err(unsafe_error(
            "staged project-state document has multiple hard links",
        ));
    }
    if stat.st_mode & 0o7777 != 0o600 {
        return Err(unsafe_error(
            "staged project-state document does not have mode 0600",
        ));
    }
    if owner != (stat.st_uid, stat.st_gid) {
        return Err(unsafe_error(
            "staged project-state document has an unexpected owner or group",
        ));
    }
    if stat.st_size < 0 || stat.st_size as u64 != expected_size as u64 {
        return Err(corrupt_error(
            "staged project-state document has an unexpected size",
        ));
    }
    Ok(())
}

fn set_directory_mode(directory: &OwnedFd) -> Result<(), InstallationPublicationError> {
    fs::fchmod(directory, MANAGED_DIRECTORY_MODE)
        .map_err(|_| io_error("could not set managed installation-directory permissions"))
}

fn synchronize_directory(
    directory: &OwnedFd,
    subject: &str,
) -> Result<(), InstallationPublicationError> {
    fs::fsync(directory).map_err(|_| io_error(format!("could not synchronize {subject}")))
}

fn synchronize_borrowed_directory(
    directory: BorrowedFd<'_>,
    subject: &str,
) -> Result<(), InstallationPublicationError> {
    fs::fsync(directory).map_err(|_| io_error(format!("could not synchronize {subject}")))
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

struct StagedInstallation<'a> {
    parent: BorrowedFd<'a>,
    directory: OwnedFd,
    name: String,
    armed: bool,
}

impl StagedInstallation<'_> {
    fn directory(&self) -> &OwnedFd {
        &self.directory
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StagedInstallation<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::unlinkat(&self.directory, PROJECT_FILE, AtFlags::empty());
            let _ = fs::unlinkat(&self.directory, RESOURCES_DIRECTORY, AtFlags::REMOVEDIR);
            let _ = fs::unlinkat(&self.directory, JOURNALS_DIRECTORY, AtFlags::REMOVEDIR);
            let _ = fs::unlinkat(&self.directory, RECEIPTS_DIRECTORY, AtFlags::REMOVEDIR);
            let _ = fs::unlinkat(self.parent, &self.name, AtFlags::REMOVEDIR);
            let _ = fs::fsync(self.parent);
        }
    }
}

fn map_directory_open_error(error: Errno) -> InstallationPublicationError {
    match error {
        Errno::LOOP | Errno::NOTDIR => {
            unsafe_error("installation state contains a symlink or incompatible directory")
        }
        _ => io_error("could not open an installation state directory"),
    }
}

fn map_directory_create_error(error: Errno) -> InstallationPublicationError {
    match error {
        Errno::LOOP | Errno::NOTDIR => {
            unsafe_error("installation state has a symlinked or invalid parent")
        }
        _ => io_error("could not create an installation state directory"),
    }
}

fn map_project_create_error(error: Errno) -> InstallationPublicationError {
    match error {
        Errno::EXIST => corrupt_error("new staged installation already contains project state"),
        Errno::ISDIR | Errno::LOOP | Errno::NOTDIR => {
            unsafe_error("staged project-state path is symlinked or incompatible")
        }
        _ => io_error("could not create the staged project-state document"),
    }
}

fn map_publication_rename_error(error: Errno) -> InstallationPublicationError {
    match error {
        Errno::EXIST => id_collision_error("installation ID already exists in the catalog"),
        Errno::ISDIR | Errno::LOOP | Errno::NOTDIR => {
            unsafe_error("installation publication encountered an incompatible filesystem object")
        }
        _ => io_error("could not publish the staged installation"),
    }
}

fn id_collision_error(message: impl Into<String>) -> InstallationPublicationError {
    InstallationPublicationError::new(InstallationPublicationErrorKind::IdCollision, message)
}

fn invalid_project_error(message: impl Into<String>) -> InstallationPublicationError {
    InstallationPublicationError::new(InstallationPublicationErrorKind::InvalidProject, message)
}

fn io_error(message: impl Into<String>) -> InstallationPublicationError {
    InstallationPublicationError::new(InstallationPublicationErrorKind::Io, message)
}

fn unsafe_error(message: impl Into<String>) -> InstallationPublicationError {
    InstallationPublicationError::new(InstallationPublicationErrorKind::UnsafeFilesystem, message)
}

fn corrupt_error(message: impl Into<String>) -> InstallationPublicationError {
    InstallationPublicationError::new(InstallationPublicationErrorKind::CorruptState, message)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::linux_installation_catalog::{InstallationLookup, find_installation};
    use crate::linux_installation_catalog_lock::lock_installation_catalog;
    use crate::manifest::RunnerScope;
    use crate::ownership::ProjectIdentity;
    use crate::state::InstallationId;
    use crate::state_document::{StateDocument, decode_state_document};

    use super::{
        INSTALLATIONS_DIRECTORY, InstallationPublicationErrorKind, JOURNALS_DIRECTORY,
        PROJECT_FILE, RECEIPTS_DIRECTORY, RESOURCES_DIRECTORY, STAGING_DIRECTORY,
        publish_new_installation,
    };

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(label: &str) -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "smolrunner-installation-publication-{label}-{}-{sequence}",
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

    fn installation_id(value: &str) -> InstallationId {
        InstallationId::parse(value).expect("installation ID")
    }

    fn project(repository: &str) -> ProjectIdentity {
        ProjectIdentity {
            repository: repository.to_owned(),
            runner_scope: RunnerScope::Repository,
            runner_user: "project-runner".to_owned(),
        }
    }

    fn assert_directory(path: &Path, owner: (u32, u32)) {
        let metadata = fs::metadata(path).expect("directory metadata");
        assert!(metadata.is_dir());
        assert_eq!(metadata.mode() & 0o7777, 0o750);
        assert_eq!((metadata.uid(), metadata.gid()), owner);
    }

    #[test]
    fn publishes_a_complete_installation_and_catalog_lookup_finds_it() {
        let root = TempRoot::new("complete");
        let root_metadata = fs::metadata(root.path()).expect("root metadata");
        let owner = (root_metadata.uid(), root_metadata.gid());
        let id = installation_id("1111111111111111");
        let expected_project = project("example/project");
        let lock = lock_installation_catalog(root.path()).expect("lock catalog");

        let receipt = publish_new_installation(&lock, id.clone(), expected_project.clone())
            .expect("publish installation");
        assert_eq!(receipt.installation_id(), &id);
        assert!(receipt.bytes_written() > 0);

        let installations = root.path().join(INSTALLATIONS_DIRECTORY);
        let staging = root.path().join(STAGING_DIRECTORY);
        let installation = installations.join(id.as_str());
        assert_directory(&installations, owner);
        assert_directory(&staging, owner);
        assert_directory(&installation, owner);
        assert_directory(&installation.join(RESOURCES_DIRECTORY), owner);
        assert_directory(&installation.join(JOURNALS_DIRECTORY), owner);
        assert_directory(&installation.join(RECEIPTS_DIRECTORY), owner);
        assert!(!staging.join(id.as_str()).exists());

        let project_path = installation.join(PROJECT_FILE);
        let metadata = fs::metadata(&project_path).expect("project metadata");
        assert!(metadata.is_file());
        assert_eq!(metadata.mode() & 0o7777, 0o600);
        assert_eq!(metadata.nlink(), 1);
        assert_eq!((metadata.uid(), metadata.gid()), owner);
        let encoded = fs::read_to_string(project_path).expect("read project document");
        match decode_state_document(&encoded).expect("decode project document") {
            StateDocument::Project(document) => {
                assert_eq!(document.installation_id(), &id);
                assert_eq!(document.project(), &expected_project);
            }
            StateDocument::Resource(_) => panic!("expected project document"),
        }

        assert_eq!(
            find_installation(root.path(), &expected_project).expect("catalog lookup"),
            InstallationLookup::Found(id)
        );
    }

    #[test]
    fn existing_final_installation_is_never_replaced() {
        let root = TempRoot::new("collision");
        let id = installation_id("2222222222222222");
        let first_project = project("example/first");
        let lock = lock_installation_catalog(root.path()).expect("lock catalog");
        publish_new_installation(&lock, id.clone(), first_project.clone())
            .expect("publish first installation");

        let error = publish_new_installation(&lock, id.clone(), project("example/second"))
            .expect_err("duplicate installation ID must fail");
        assert_eq!(error.kind(), InstallationPublicationErrorKind::IdCollision);
        assert_eq!(
            find_installation(root.path(), &first_project).expect("first project lookup"),
            InstallationLookup::Found(id)
        );
    }

    #[test]
    fn invalid_project_is_rejected_before_catalog_directories_exist() {
        let root = TempRoot::new("invalid-project");
        let lock = lock_installation_catalog(root.path()).expect("lock catalog");
        let invalid = ProjectIdentity {
            repository: "invalid".to_owned(),
            runner_scope: RunnerScope::Repository,
            runner_user: "project-runner".to_owned(),
        };
        let error = publish_new_installation(&lock, installation_id("3333333333333333"), invalid)
            .expect_err("invalid project must fail");
        assert_eq!(
            error.kind(),
            InstallationPublicationErrorKind::InvalidProject
        );
        assert!(!root.path().join(INSTALLATIONS_DIRECTORY).exists());
        assert!(!root.path().join(STAGING_DIRECTORY).exists());
    }

    #[test]
    fn stale_staging_id_returns_collision_without_catalog_mutation() {
        let root = TempRoot::new("staging-collision");
        let id = installation_id("4444444444444444");
        let staging = root.path().join(STAGING_DIRECTORY);
        fs::create_dir(&staging).expect("create staging directory");
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o750)).expect("set staging mode");
        let stale = staging.join(id.as_str());
        fs::create_dir(&stale).expect("create stale staging entry");
        fs::set_permissions(&stale, fs::Permissions::from_mode(0o750))
            .expect("set stale staging mode");
        let lock = lock_installation_catalog(root.path()).expect("lock catalog");

        let error = publish_new_installation(&lock, id, project("example/project"))
            .expect_err("stale staging ID must fail");
        assert_eq!(error.kind(), InstallationPublicationErrorKind::IdCollision);
        assert!(
            !root
                .path()
                .join(INSTALLATIONS_DIRECTORY)
                .join("4444444444444444")
                .exists()
        );
    }

    #[test]
    fn symlinked_catalog_directory_is_rejected() {
        let root = TempRoot::new("symlink");
        let outside = TempRoot::new("outside");
        symlink(outside.path(), root.path().join(INSTALLATIONS_DIRECTORY))
            .expect("create installations symlink");
        let lock = lock_installation_catalog(root.path()).expect("lock catalog");

        let error = publish_new_installation(
            &lock,
            installation_id("5555555555555555"),
            project("example/project"),
        )
        .expect_err("symlinked catalog directory must fail");
        assert_eq!(
            error.kind(),
            InstallationPublicationErrorKind::UnsafeFilesystem
        );
    }
}
