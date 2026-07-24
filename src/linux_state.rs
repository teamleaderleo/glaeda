use std::fs::File;
use std::io::{Read, Take, Write as _};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::Path;

use rustix::fs::{self, AtFlags, FileType, FlockOperation, Mode, OFlags};
use rustix::io::Errno;
use rustix::rand::{GetRandomFlags, getrandom};

use crate::state::{STATE_ROOT, StateComponent, StatePath};
use crate::state_store::{
    MAX_STATE_DOCUMENT_BYTES, StateRead, StateRecord, StateStore, StateStoreError,
    StateStoreErrorKind, StateWriteDisposition, StateWriteReceipt,
};

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const EXISTING_LOCK_FLAGS: OFlags = OFlags::RDWR.union(OFlags::NOFOLLOW).union(OFlags::CLOEXEC);
const NEW_LOCK_FLAGS: OFlags = EXISTING_LOCK_FLAGS
    .union(OFlags::CREATE)
    .union(OFlags::EXCL);
const TEMP_FILE_FLAGS: OFlags = OFlags::WRONLY
    .union(OFlags::CREATE)
    .union(OFlags::EXCL)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const PRIVATE_FILE_MODE: Mode = Mode::RUSR.union(Mode::WUSR);
const LOCK_FILE_NAME: &str = "write.lock";
const TEMP_FILE_ATTEMPTS: usize = 8;
const TEMP_FILE_PREFIX: &str = ".smolrunner-tmp-";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WritePhase {
    BeforeWrite,
    BeforeFileSync,
    BeforeRename,
    BeforeParentSync,
}

/// Descriptor-relative access to one trusted SmolRunner state root.
#[derive(Debug)]
pub struct LinuxStateRoot {
    root: OwnedFd,
}

impl LinuxStateRoot {
    /// Open the canonical system state root without following a final symlink.
    ///
    /// # Errors
    ///
    /// Returns a bounded store error when the root is missing, inaccessible, symlinked, or not a
    /// directory.
    pub fn open_default() -> Result<Self, StateStoreError> {
        Self::open(STATE_ROOT)
    }

    /// Open one trusted state root for descriptor-relative access.
    ///
    /// This constructor is public so integration tests and explicitly configured hosts can use a
    /// temporary or relocated root. Callers remain responsible for choosing the trusted root path.
    ///
    /// # Errors
    ///
    /// Returns a bounded store error when the root is missing, inaccessible, symlinked, or not a
    /// directory.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StateStoreError> {
        let root =
            fs::open(path.as_ref(), DIRECTORY_FLAGS, Mode::empty()).map_err(map_root_open_error)?;
        verify_directory(&root, "state root")?;
        Ok(Self { root })
    }

    /// Read one canonical state path without following symlinks in any component.
    ///
    /// # Errors
    ///
    /// Returns `UnsafeFilesystem` for symlinks, non-directory parents, or non-regular final files;
    /// `CorruptState` for oversized files; and `Io` for other bounded read failures.
    pub fn read(&self, path: &StatePath) -> Result<StateRead, StateStoreError> {
        let Some((file_name, parents)) = path.components().split_last() else {
            return Err(empty_state_path_error());
        };

        let mut current = None::<OwnedFd>;
        for component in parents {
            let dirfd = active_directory(&self.root, current.as_ref());
            current = match fs::openat(dirfd, component.as_str(), DIRECTORY_FLAGS, Mode::empty()) {
                Ok(directory) => {
                    verify_directory(&directory, "state path parent")?;
                    Some(directory)
                }
                Err(Errno::NOENT) => return Ok(StateRead::Missing),
                Err(error) => return Err(map_component_open_error(error)),
            };
        }

        let dirfd = active_directory(&self.root, current.as_ref());
        let file = match fs::openat(dirfd, file_name.as_str(), FILE_FLAGS, Mode::empty()) {
            Ok(file) => file,
            Err(Errno::NOENT) => return Ok(StateRead::Missing),
            Err(error) => return Err(map_component_open_error(error)),
        };
        verify_regular_file(&file, "state file", true)?;
        read_bounded(file)
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
    pub fn write_atomic(
        &mut self,
        record: &StateRecord,
    ) -> Result<StateWriteReceipt, StateStoreError> {
        let mut hook = |_phase| Ok::<(), StateStoreError>(());
        self.write_atomic_with_hook(record, &mut hook)
    }

    fn write_atomic_with_hook<F>(
        &mut self,
        record: &StateRecord,
        hook: &mut F,
    ) -> Result<StateWriteReceipt, StateStoreError>
    where
        F: FnMut(WritePhase) -> Result<(), StateStoreError>,
    {
        let _lock = self.acquire_installation_lock(record.path())?;
        let (parent, file_name) = self.open_required_parent(record.path())?;
        let disposition = inspect_destination(&parent, file_name)?;
        let (temporary, temporary_name) = create_temporary_file(&parent)?;
        let mut temporary_path = TemporaryPath::new(parent.as_fd(), temporary_name);

        fs::fchmod(&temporary, PRIVATE_FILE_MODE).map_err(|_| {
            StateStoreError::public(
                StateStoreErrorKind::Io,
                "could not set private state-file permissions",
            )
        })?;
        write_and_sync(temporary, record.bytes(), hook)?;

        hook(WritePhase::BeforeRename)?;
        fs::renameat(&parent, temporary_path.name(), &parent, file_name.as_str())
            .map_err(map_rename_error)?;
        temporary_path.disarm();

        hook(WritePhase::BeforeParentSync)?;
        fs::fsync(&parent).map_err(|_| {
            StateStoreError::public(
                StateStoreErrorKind::Io,
                "state file was published but its parent directory could not be synchronized",
            )
        })?;

        Ok(StateWriteReceipt::new(disposition, record.bytes().len()))
    }

    fn open_required_parent<'a>(
        &self,
        path: &'a StatePath,
    ) -> Result<(OwnedFd, &'a StateComponent), StateStoreError> {
        let Some((file_name, parents)) = path.components().split_last() else {
            return Err(empty_state_path_error());
        };
        let mut current = self.root.try_clone().map_err(|_| {
            StateStoreError::public(StateStoreErrorKind::Io, "could not duplicate state root")
        })?;
        for component in parents {
            current = fs::openat(&current, component.as_str(), DIRECTORY_FLAGS, Mode::empty())
                .map_err(map_required_parent_error)?;
            verify_directory(&current, "state path parent")?;
        }
        Ok((current, file_name))
    }

    fn acquire_installation_lock(&self, path: &StatePath) -> Result<OwnedFd, StateStoreError> {
        let components = path.components();
        if components.len() < 3 || components[0].as_str() != "installations" {
            return Err(StateStoreError::public(
                StateStoreErrorKind::CorruptState,
                "state path does not identify one installation",
            ));
        }

        let installations = fs::openat(
            &self.root,
            components[0].as_str(),
            DIRECTORY_FLAGS,
            Mode::empty(),
        )
        .map_err(map_required_parent_error)?;
        verify_directory(&installations, "installations directory")?;
        let installation = fs::openat(
            &installations,
            components[1].as_str(),
            DIRECTORY_FLAGS,
            Mode::empty(),
        )
        .map_err(map_required_parent_error)?;
        verify_directory(&installation, "installation directory")?;

        let lock = open_installation_lock(&installation)?;
        match fs::flock(&lock, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(lock),
            Err(Errno::AGAIN) => Err(StateStoreError::public(
                StateStoreErrorKind::Busy,
                "another state writer holds the installation lock",
            )),
            Err(_) => Err(StateStoreError::public(
                StateStoreErrorKind::Io,
                "could not acquire the installation lock",
            )),
        }
    }
}

impl StateStore for LinuxStateRoot {
    fn read(&self, path: &StatePath) -> Result<StateRead, StateStoreError> {
        Self::read(self, path)
    }

    fn write_atomic(&mut self, record: &StateRecord) -> Result<StateWriteReceipt, StateStoreError> {
        Self::write_atomic(self, record)
    }
}

struct TemporaryPath<'a> {
    parent: BorrowedFd<'a>,
    name: String,
    armed: bool,
}

impl<'a> TemporaryPath<'a> {
    fn new(parent: BorrowedFd<'a>, name: String) -> Self {
        Self {
            parent,
            name,
            armed: true,
        }
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TemporaryPath<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::unlinkat(self.parent, &self.name, AtFlags::empty());
        }
    }
}

fn active_directory<'a>(root: &'a OwnedFd, current: Option<&'a OwnedFd>) -> BorrowedFd<'a> {
    match current {
        Some(directory) => directory.as_fd(),
        None => root.as_fd(),
    }
}

fn open_installation_lock(installation: &OwnedFd) -> Result<OwnedFd, StateStoreError> {
    match fs::openat(
        installation,
        LOCK_FILE_NAME,
        NEW_LOCK_FLAGS,
        PRIVATE_FILE_MODE,
    ) {
        Ok(lock) => {
            fs::fchmod(&lock, PRIVATE_FILE_MODE).map_err(|_| {
                StateStoreError::public(
                    StateStoreErrorKind::Io,
                    "could not set installation-lock permissions",
                )
            })?;
            Ok(lock)
        }
        Err(Errno::EXIST) => {
            let lock = fs::openat(
                installation,
                LOCK_FILE_NAME,
                EXISTING_LOCK_FLAGS,
                Mode::empty(),
            )
            .map_err(map_lock_open_error)?;
            verify_regular_file(&lock, "installation lock", false)?;
            verify_private_mode(&lock, "installation lock")?;
            Ok(lock)
        }
        Err(error) => Err(map_lock_open_error(error)),
    }
}

fn inspect_destination(
    parent: &OwnedFd,
    file_name: &StateComponent,
) -> Result<StateWriteDisposition, StateStoreError> {
    match fs::openat(parent, file_name.as_str(), FILE_FLAGS, Mode::empty()) {
        Ok(file) => {
            verify_regular_file(&file, "existing state file", true)?;
            verify_private_mode(&file, "existing state file")?;
            Ok(StateWriteDisposition::Replaced)
        }
        Err(Errno::NOENT) => Ok(StateWriteDisposition::Created),
        Err(error) => Err(map_component_open_error(error)),
    }
}

fn create_temporary_file(parent: &OwnedFd) -> Result<(OwnedFd, String), StateStoreError> {
    for _ in 0..TEMP_FILE_ATTEMPTS {
        let name = random_temporary_name()?;
        match fs::openat(parent, &name, TEMP_FILE_FLAGS, PRIVATE_FILE_MODE) {
            Ok(file) => return Ok((file, name)),
            Err(Errno::EXIST) => {}
            Err(Errno::LOOP | Errno::NOTDIR) => {
                return Err(StateStoreError::public(
                    StateStoreErrorKind::UnsafeFilesystem,
                    "temporary state path is symlinked or invalid",
                ));
            }
            Err(_) => {
                return Err(StateStoreError::public(
                    StateStoreErrorKind::Io,
                    "could not create temporary state file",
                ));
            }
        }
    }
    Err(StateStoreError::public(
        StateStoreErrorKind::Io,
        "could not allocate a unique temporary state file",
    ))
}

fn random_temporary_name() -> Result<String, StateStoreError> {
    let mut random = [0_u8; 16];
    let filled = getrandom(&mut random[..], GetRandomFlags::empty()).map_err(|_| {
        StateStoreError::public(
            StateStoreErrorKind::Io,
            "could not obtain operating-system randomness for a temporary state file",
        )
    })?;
    if filled != random.len() {
        return Err(StateStoreError::public(
            StateStoreErrorKind::Io,
            "operating-system randomness returned an incomplete temporary-file name",
        ));
    }

    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut name = String::with_capacity(TEMP_FILE_PREFIX.len() + random.len() * 2);
    name.push_str(TEMP_FILE_PREFIX);
    for byte in random {
        name.push(HEX[(byte >> 4) as usize] as char);
        name.push(HEX[(byte & 0x0f) as usize] as char);
    }
    Ok(name)
}

fn write_and_sync<F>(fd: OwnedFd, bytes: &[u8], hook: &mut F) -> Result<(), StateStoreError>
where
    F: FnMut(WritePhase) -> Result<(), StateStoreError>,
{
    let mut file = File::from(fd);
    hook(WritePhase::BeforeWrite)?;
    file.write_all(bytes).map_err(|_| {
        StateStoreError::public(
            StateStoreErrorKind::Io,
            "could not write temporary state file",
        )
    })?;
    hook(WritePhase::BeforeFileSync)?;
    fs::fsync(&file).map_err(|_| {
        StateStoreError::public(
            StateStoreErrorKind::Io,
            "could not synchronize temporary state file",
        )
    })
}

fn verify_directory(fd: &OwnedFd, subject: &str) -> Result<(), StateStoreError> {
    let stat = fs::fstat(fd).map_err(|_| {
        StateStoreError::public(
            StateStoreErrorKind::Io,
            format!("could not inspect {subject}"),
        )
    })?;
    if FileType::from_raw_mode(stat.st_mode).is_dir() {
        Ok(())
    } else {
        Err(StateStoreError::public(
            StateStoreErrorKind::UnsafeFilesystem,
            format!("{subject} is not a directory"),
        ))
    }
}

fn verify_regular_file(
    fd: &OwnedFd,
    subject: &str,
    enforce_size_limit: bool,
) -> Result<(), StateStoreError> {
    let stat = fs::fstat(fd).map_err(|_| {
        StateStoreError::public(
            StateStoreErrorKind::Io,
            format!("could not inspect {subject}"),
        )
    })?;
    if !FileType::from_raw_mode(stat.st_mode).is_file() {
        return Err(StateStoreError::public(
            StateStoreErrorKind::UnsafeFilesystem,
            format!("{subject} is not a regular file"),
        ));
    }
    if enforce_size_limit
        && (stat.st_size < 0 || stat.st_size as u64 > MAX_STATE_DOCUMENT_BYTES as u64)
    {
        return Err(StateStoreError::public(
            StateStoreErrorKind::CorruptState,
            format!("{subject} exceeds the configured size limit"),
        ));
    }
    Ok(())
}

fn verify_private_mode(fd: &OwnedFd, subject: &str) -> Result<(), StateStoreError> {
    let stat = fs::fstat(fd).map_err(|_| {
        StateStoreError::public(
            StateStoreErrorKind::Io,
            format!("could not inspect {subject} permissions"),
        )
    })?;
    if stat.st_mode & 0o7777 == 0o600 {
        Ok(())
    } else {
        Err(StateStoreError::public(
            StateStoreErrorKind::UnsafeFilesystem,
            format!("{subject} does not have mode 0600"),
        ))
    }
}

fn read_bounded(fd: OwnedFd) -> Result<StateRead, StateStoreError> {
    let file = File::from(fd);
    let mut reader: Take<File> = file.take((MAX_STATE_DOCUMENT_BYTES + 1) as u64);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).map_err(|_| {
        StateStoreError::public(StateStoreErrorKind::Io, "could not read state file")
    })?;
    if bytes.len() > MAX_STATE_DOCUMENT_BYTES {
        return Err(StateStoreError::public(
            StateStoreErrorKind::CorruptState,
            "state file exceeds the configured size limit",
        ));
    }
    Ok(StateRead::Present(bytes))
}

fn empty_state_path_error() -> StateStoreError {
    StateStoreError::public(
        StateStoreErrorKind::CorruptState,
        "state path contains no file component",
    )
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

fn map_component_open_error(error: Errno) -> StateStoreError {
    match error {
        Errno::LOOP | Errno::NOTDIR => StateStoreError::public(
            StateStoreErrorKind::UnsafeFilesystem,
            "state path contains a symlink or non-directory parent",
        ),
        _ => StateStoreError::public(StateStoreErrorKind::Io, "could not open state path"),
    }
}

fn map_required_parent_error(error: Errno) -> StateStoreError {
    match error {
        Errno::LOOP | Errno::NOTDIR => StateStoreError::public(
            StateStoreErrorKind::UnsafeFilesystem,
            "state path contains a symlink or non-directory parent",
        ),
        Errno::NOENT => StateStoreError::public(
            StateStoreErrorKind::Io,
            "state path parent has not been prepared",
        ),
        _ => StateStoreError::public(StateStoreErrorKind::Io, "could not open state path parent"),
    }
}

fn map_lock_open_error(error: Errno) -> StateStoreError {
    match error {
        Errno::LOOP | Errno::NOTDIR => StateStoreError::public(
            StateStoreErrorKind::UnsafeFilesystem,
            "installation lock is symlinked or invalid",
        ),
        _ => StateStoreError::public(StateStoreErrorKind::Io, "could not open installation lock"),
    }
}

fn map_rename_error(error: Errno) -> StateStoreError {
    match error {
        Errno::ISDIR | Errno::NOTDIR | Errno::LOOP => StateStoreError::public(
            StateStoreErrorKind::UnsafeFilesystem,
            "state destination changed to an incompatible filesystem object",
        ),
        _ => StateStoreError::public(
            StateStoreErrorKind::Io,
            "could not publish temporary state file",
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, OpenOptions};
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use rustix::fs::{self as rustix_fs, FlockOperation};

    use crate::manifest::RunnerScope;
    use crate::ownership::ProjectIdentity;
    use crate::state::{InstallationId, StateLayout};
    use crate::state_document::ProjectStateDocument;
    use crate::state_store::{
        MAX_STATE_DOCUMENT_BYTES, StateRead, StateRecord, StateStoreError, StateStoreErrorKind,
        StateWriteDisposition,
    };

    use super::{LOCK_FILE_NAME, LinuxStateRoot, TEMP_FILE_PREFIX, WritePhase};

    static NEXT_TEMP_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TempTree {
        path: PathBuf,
    }

    impl TempTree {
        fn new(label: &str) -> Self {
            let sequence = NEXT_TEMP_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "smolrunner-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create isolated temporary root");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn installation_id() -> InstallationId {
        InstallationId::parse("0123456789abcdef").expect("installation ID")
    }

    fn create_project_parent(root: &Path) -> PathBuf {
        let installation = root.join("installations").join(installation_id().as_str());
        fs::create_dir_all(&installation).expect("create project parent");
        installation
    }

    fn project_record(repository: &str) -> StateRecord {
        StateRecord::project(
            ProjectStateDocument::new(
                installation_id(),
                ProjectIdentity {
                    repository: repository.to_owned(),
                    runner_scope: RunnerScope::Repository,
                    runner_user: "project-runner".to_owned(),
                },
            )
            .expect("project document"),
        )
        .expect("project record")
    }

    fn fail_at(target: WritePhase) -> impl FnMut(WritePhase) -> Result<(), StateStoreError> {
        move |phase| {
            if phase != target {
                return Ok(());
            }
            let message = match phase {
                WritePhase::BeforeWrite => {
                    "injected state-write failure before temporary-file write"
                }
                WritePhase::BeforeFileSync => {
                    "injected state-write failure before temporary-file synchronization"
                }
                WritePhase::BeforeRename => {
                    "injected state-write failure before publication rename"
                }
                WritePhase::BeforeParentSync => {
                    "state file was published before an injected parent-sync failure"
                }
            };
            Err(StateStoreError::public(StateStoreErrorKind::Io, message))
        }
    }

    fn assert_no_temporary_files(parent: &Path) {
        assert!(
            fs::read_dir(parent)
                .expect("list state directory")
                .all(|entry| !entry
                    .expect("directory entry")
                    .file_name()
                    .to_string_lossy()
                    .starts_with(TEMP_FILE_PREFIX))
        );
    }

    #[test]
    fn reads_regular_file_through_canonical_components() {
        let root = TempTree::new("regular-read");
        let parent = create_project_parent(root.path());
        fs::write(parent.join("project.json"), b"{\"schema_version\":1}\n")
            .expect("write project state");

        let reader = LinuxStateRoot::open(root.path()).expect("open state root");
        assert_eq!(
            reader
                .read(&StateLayout::project_document(&installation_id()))
                .expect("read state"),
            StateRead::Present(b"{\"schema_version\":1}\n".to_vec())
        );
    }

    #[test]
    fn missing_parent_or_file_is_reported_as_missing() {
        let root = TempTree::new("missing");
        let reader = LinuxStateRoot::open(root.path()).expect("open state root");
        assert_eq!(
            reader
                .read(&StateLayout::project_document(&installation_id()))
                .expect("read missing state"),
            StateRead::Missing
        );

        create_project_parent(root.path());
        assert_eq!(
            reader
                .read(&StateLayout::project_document(&installation_id()))
                .expect("read missing file"),
            StateRead::Missing
        );
    }

    #[test]
    fn symlinked_parent_is_rejected() {
        let root = TempTree::new("parent-link");
        let outside = TempTree::new("parent-link-outside");
        symlink(outside.path(), root.path().join("installations")).expect("create parent symlink");

        let reader = LinuxStateRoot::open(root.path()).expect("open state root");
        let error = reader
            .read(&StateLayout::project_document(&installation_id()))
            .expect_err("parent symlink must fail");
        assert_eq!(error.kind(), StateStoreErrorKind::UnsafeFilesystem);
    }

    #[test]
    fn symlinked_final_file_is_rejected() {
        let root = TempTree::new("file-link");
        let outside = TempTree::new("file-link-outside");
        let outside_file = outside.path().join("project.json");
        fs::write(&outside_file, b"foreign").expect("write foreign file");
        let parent = create_project_parent(root.path());
        symlink(&outside_file, parent.join("project.json")).expect("create file symlink");

        let reader = LinuxStateRoot::open(root.path()).expect("open state root");
        let error = reader
            .read(&StateLayout::project_document(&installation_id()))
            .expect_err("file symlink must fail");
        assert_eq!(error.kind(), StateStoreErrorKind::UnsafeFilesystem);
    }

    #[test]
    fn symlinked_root_is_rejected() {
        let actual = TempTree::new("actual-root");
        let link_parent = TempTree::new("root-link-parent");
        let link = link_parent.path().join("state-link");
        symlink(actual.path(), &link).expect("create root symlink");

        let error = LinuxStateRoot::open(&link).expect_err("root symlink must fail");
        assert_eq!(error.kind(), StateStoreErrorKind::UnsafeFilesystem);
    }

    #[test]
    fn non_regular_and_oversized_files_are_rejected() {
        let root = TempTree::new("invalid-files");
        let parent = create_project_parent(root.path());
        fs::create_dir(parent.join("project.json")).expect("create directory at file path");
        let reader = LinuxStateRoot::open(root.path()).expect("open state root");
        let error = reader
            .read(&StateLayout::project_document(&installation_id()))
            .expect_err("directory at file path must fail");
        assert_eq!(error.kind(), StateStoreErrorKind::UnsafeFilesystem);

        fs::remove_dir(parent.join("project.json")).expect("remove directory");
        fs::write(
            parent.join("project.json"),
            vec![0_u8; MAX_STATE_DOCUMENT_BYTES + 1],
        )
        .expect("write oversized state");
        let error = reader
            .read(&StateLayout::project_document(&installation_id()))
            .expect_err("oversized state must fail");
        assert_eq!(error.kind(), StateStoreErrorKind::CorruptState);
    }

    #[test]
    fn atomic_write_creates_and_replaces_private_state() {
        let root = TempTree::new("atomic-write");
        let parent = create_project_parent(root.path());
        let mut store = LinuxStateRoot::open(root.path()).expect("open state root");
        let first = project_record("example/project");
        let receipt = store.write_atomic(&first).expect("create state");
        assert_eq!(receipt.disposition(), StateWriteDisposition::Created);
        assert_eq!(receipt.bytes_written(), first.bytes().len());
        assert_eq!(
            fs::metadata(parent.join("project.json"))
                .expect("state metadata")
                .mode()
                & 0o7777,
            0o600
        );
        assert_eq!(
            store.read(first.path()).expect("read created state"),
            StateRead::Present(first.bytes().to_vec())
        );

        let second = project_record("example/renamed");
        let receipt = store.write_atomic(&second).expect("replace state");
        assert_eq!(receipt.disposition(), StateWriteDisposition::Replaced);
        assert_eq!(
            store.read(second.path()).expect("read replaced state"),
            StateRead::Present(second.bytes().to_vec())
        );
        assert!(
            fs::read_dir(parent)
                .expect("list state directory")
                .all(|entry| !entry
                    .expect("directory entry")
                    .file_name()
                    .to_string_lossy()
                    .starts_with(TEMP_FILE_PREFIX))
        );
    }

    #[test]
    fn prepublication_failures_preserve_existing_state_and_remove_temporary_files() {
        for point in [
            WritePhase::BeforeWrite,
            WritePhase::BeforeFileSync,
            WritePhase::BeforeRename,
        ] {
            let root = TempTree::new(&format!("prepublication-{point:?}"));
            let parent = create_project_parent(root.path());
            let mut store = LinuxStateRoot::open(root.path()).expect("open state root");
            let original = project_record("example/original");
            store.write_atomic(&original).expect("write original state");

            let replacement = project_record("example/replacement");
            let mut hook = fail_at(point);
            let error = store
                .write_atomic_with_hook(&replacement, &mut hook)
                .expect_err("injected prepublication failure must fail");
            assert_eq!(error.kind(), StateStoreErrorKind::Io);
            assert_eq!(
                store.read(original.path()).expect("read preserved state"),
                StateRead::Present(original.bytes().to_vec())
            );
            assert_no_temporary_files(&parent);
        }
    }

    #[test]
    fn parent_sync_failure_reports_published_state_and_removes_temporary_file() {
        let root = TempTree::new("parent-sync-failure");
        let parent = create_project_parent(root.path());
        let mut store = LinuxStateRoot::open(root.path()).expect("open state root");
        let original = project_record("example/original");
        store.write_atomic(&original).expect("write original state");

        let replacement = project_record("example/replacement");
        let mut hook = fail_at(WritePhase::BeforeParentSync);
        let error = store
            .write_atomic_with_hook(&replacement, &mut hook)
            .expect_err("injected parent-sync failure must fail");
        assert_eq!(error.kind(), StateStoreErrorKind::Io);
        assert!(error.message().contains("published"));
        assert_eq!(
            store
                .read(replacement.path())
                .expect("read published state"),
            StateRead::Present(replacement.bytes().to_vec())
        );
        assert_no_temporary_files(&parent);
    }

    #[test]
    fn write_requires_prepared_parents_and_rejects_destination_symlink() {
        let root = TempTree::new("write-boundaries");
        let mut store = LinuxStateRoot::open(root.path()).expect("open state root");
        let record = project_record("example/project");
        let error = store
            .write_atomic(&record)
            .expect_err("missing parent must fail");
        assert_eq!(error.kind(), StateStoreErrorKind::Io);

        let outside = TempTree::new("write-boundaries-outside");
        let outside_file = outside.path().join("foreign.json");
        fs::write(&outside_file, b"foreign").expect("write foreign file");
        let parent = create_project_parent(root.path());
        symlink(&outside_file, parent.join("project.json")).expect("create destination symlink");
        let error = store
            .write_atomic(&record)
            .expect_err("destination symlink must fail");
        assert_eq!(error.kind(), StateStoreErrorKind::UnsafeFilesystem);
        assert_eq!(
            fs::read(&outside_file).expect("read foreign file"),
            b"foreign"
        );
    }

    #[test]
    fn held_installation_lock_returns_busy() {
        let root = TempTree::new("write-lock");
        let parent = create_project_parent(root.path());
        let lock_path = parent.join(LOCK_FILE_NAME);
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&lock_path)
            .expect("create lock file");
        fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600)).expect("set lock mode");
        rustix_fs::flock(&lock, FlockOperation::LockExclusive).expect("hold lock");

        let mut store = LinuxStateRoot::open(root.path()).expect("open state root");
        let error = store
            .write_atomic(&project_record("example/project"))
            .expect_err("concurrent writer must be busy");
        assert_eq!(error.kind(), StateStoreErrorKind::Busy);
    }

    #[test]
    fn incompatible_existing_lock_is_rejected_without_chmod() {
        let root = TempTree::new("unsafe-lock");
        let parent = create_project_parent(root.path());
        let lock_path = parent.join(LOCK_FILE_NAME);
        fs::write(&lock_path, b"foreign lock").expect("write foreign lock");
        fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o644))
            .expect("set foreign lock mode");

        let mut store = LinuxStateRoot::open(root.path()).expect("open state root");
        let error = store
            .write_atomic(&project_record("example/project"))
            .expect_err("broad lock mode must fail");
        assert_eq!(error.kind(), StateStoreErrorKind::UnsafeFilesystem);
        assert_eq!(
            fs::metadata(lock_path).expect("lock metadata").mode() & 0o7777,
            0o644
        );
    }
}
