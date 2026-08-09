//! Read-only Linux discovery of one protected personal-worker runtime manifest.
//!
//! Discovery holds the pre-existing installation-catalog lock shared, resolves the project from
//! strict protected state, and reads the fixed manifest descriptor-relatively. It never creates
//! state and returns only the manifest's recorded-not-observed declaration.

use std::ffi::CStr;
use std::fmt;
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom, Take};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::Path;

use rustix::fs::{self, AtFlags, Dir, FileType, FlockOperation, Mode, OFlags};
use rustix::io::Errno;
use serde::Serialize;

use crate::ownership::ProjectIdentity;
use crate::personal_worker_runtime_manifest::{
    MAX_PERSONAL_WORKER_RUNTIME_MANIFEST_BYTES, PersonalWorkerRuntimeManifest,
    PersonalWorkerRuntimeManifestErrorKind, decode_personal_worker_runtime_manifest,
};
use crate::state::{InstallationId, STATE_ROOT};
use crate::state_document::{ProjectStateDocument, StateDocument, decode_state_document};
use crate::state_store::MAX_STATE_DOCUMENT_BYTES;

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NONBLOCK);
const MANAGED_DIRECTORY_MODE: u32 = 0o750;
const PRIVATE_FILE_MODE: u32 = 0o600;
const INSTALLATIONS_DIRECTORY: &str = "installations";
const CATALOG_LOCK_FILE: &str = "catalog.lock";
const PROJECT_FILE: &str = "project.json";
const RUNTIME_MANIFEST_FILE: &str = "runtime-manifest.json";
const MAX_INSTALLATIONS: usize = 1_024;

#[derive(Debug, PartialEq, Eq)]
pub enum PersonalWorkerRuntimeManifestDiscovery {
    Missing,
    Found(PersonalWorkerRuntimeManifest),
}

impl PersonalWorkerRuntimeManifestDiscovery {
    #[must_use]
    pub const fn manifest(&self) -> Option<&PersonalWorkerRuntimeManifest> {
        match self {
            Self::Missing => None,
            Self::Found(manifest) => Some(manifest),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerRuntimeManifestDiscoveryErrorKind {
    Busy,
    RecoveryRequired,
    VersionIncompatible,
    CorruptState,
    UnsafeFilesystem,
    ChangedDuringRead,
    Io,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerRuntimeManifestDiscoveryError {
    pub kind: PersonalWorkerRuntimeManifestDiscoveryErrorKind,
    pub code: &'static str,
    pub message: &'static str,
}

impl PersonalWorkerRuntimeManifestDiscoveryError {
    const fn new(
        kind: PersonalWorkerRuntimeManifestDiscoveryErrorKind,
        code: &'static str,
        message: &'static str,
    ) -> Self {
        Self {
            kind,
            code,
            message,
        }
    }
}

impl fmt::Debug for PersonalWorkerRuntimeManifestDiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersonalWorkerRuntimeManifestDiscoveryError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for PersonalWorkerRuntimeManifestDiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for PersonalWorkerRuntimeManifestDiscoveryError {}

/// Discover one recorded runtime manifest beneath the fixed root-owned state catalog.
///
/// The result is declaration evidence only. It cannot seal runtime readiness or authorize a
/// subprocess, mount, container, cache mutation, or repository execution.
///
/// # Errors
///
/// Returns bounded path-free errors for a missing required catalog lock, contention, unsafe or
/// corrupt state, incompatible versions, drift during the read, and ordinary I/O failure.
pub fn discover_personal_worker_runtime_manifest(
    project: &ProjectIdentity,
) -> Result<PersonalWorkerRuntimeManifestDiscovery, PersonalWorkerRuntimeManifestDiscoveryError> {
    discover_at(Path::new(STATE_ROOT), 0, project)
}

fn discover_at(
    root_path: &Path,
    expected_root_uid: u32,
    project: &ProjectIdentity,
) -> Result<PersonalWorkerRuntimeManifestDiscovery, PersonalWorkerRuntimeManifestDiscoveryError> {
    let Some(guard) = open_catalog_read_guard(root_path, expected_root_uid)? else {
        verify_root_absent(root_path)?;
        return Ok(PersonalWorkerRuntimeManifestDiscovery::Missing);
    };
    let Some(installations) =
        open_optional_directory(guard.root(), INSTALLATIONS_DIRECTORY, guard.owner())?
    else {
        verify_entry_absent(guard.root(), INSTALLATIONS_DIRECTORY)?;
        finish_catalog_read(&guard, root_path)?;
        return Ok(PersonalWorkerRuntimeManifestDiscovery::Missing);
    };

    let matched = find_project_installation(&installations, guard.owner(), project)?;
    let Some(matched) = matched else {
        verify_directory_entry(
            guard.root(),
            INSTALLATIONS_DIRECTORY,
            installations.as_fd(),
            guard.owner(),
        )?;
        finish_catalog_read(&guard, root_path)?;
        return Ok(PersonalWorkerRuntimeManifestDiscovery::Missing);
    };

    let Some(manifest_file) = open_optional_private_file(
        matched.directory.as_fd(),
        RUNTIME_MANIFEST_FILE,
        guard.owner(),
        MAX_PERSONAL_WORKER_RUNTIME_MANIFEST_BYTES,
    )?
    else {
        verify_entry_absent(matched.directory.as_fd(), RUNTIME_MANIFEST_FILE)?;
        verify_matched_installation(&installations, &matched, guard.owner())?;
        verify_directory_entry(
            guard.root(),
            INSTALLATIONS_DIRECTORY,
            installations.as_fd(),
            guard.owner(),
        )?;
        finish_catalog_read(&guard, root_path)?;
        return Ok(PersonalWorkerRuntimeManifestDiscovery::Missing);
    };
    let manifest = decode_personal_worker_runtime_manifest(&manifest_file.bytes).map_err(
        |error| match error.kind {
            PersonalWorkerRuntimeManifestErrorKind::VersionIncompatible => version_incompatible(),
            PersonalWorkerRuntimeManifestErrorKind::InvalidDocument
            | PersonalWorkerRuntimeManifestErrorKind::CorruptDocument => corrupt_error(),
        },
    )?;
    if manifest.installation_id() != &matched.installation_id {
        return Err(corrupt_error());
    }

    verify_file_entry(
        matched.directory.as_fd(),
        RUNTIME_MANIFEST_FILE,
        manifest_file.file.as_fd(),
        guard.owner(),
        MAX_PERSONAL_WORKER_RUNTIME_MANIFEST_BYTES,
    )?;
    verify_matched_installation(&installations, &matched, guard.owner())?;
    verify_directory_entry(
        guard.root(),
        INSTALLATIONS_DIRECTORY,
        installations.as_fd(),
        guard.owner(),
    )?;
    finish_catalog_read(&guard, root_path)?;
    Ok(PersonalWorkerRuntimeManifestDiscovery::Found(manifest))
}

struct CatalogReadGuard {
    root: OwnedFd,
    owner: (u32, u32),
    lock: OwnedFd,
}

impl CatalogReadGuard {
    fn root(&self) -> BorrowedFd<'_> {
        self.root.as_fd()
    }

    const fn owner(&self) -> (u32, u32) {
        self.owner
    }
}

impl Drop for CatalogReadGuard {
    fn drop(&mut self) {
        // Release the shared open-file description even if fork temporarily retained a duplicate.
        let _ = fs::flock(&self.lock, FlockOperation::Unlock);
    }
}

fn open_catalog_read_guard(
    root_path: &Path,
    expected_root_uid: u32,
) -> Result<Option<CatalogReadGuard>, PersonalWorkerRuntimeManifestDiscoveryError> {
    let root = match fs::open(root_path, DIRECTORY_FLAGS, Mode::empty()) {
        Ok(root) => root,
        Err(Errno::NOENT) => return Ok(None),
        Err(Errno::LOOP | Errno::NOTDIR) => return Err(unsafe_error()),
        Err(_) => return Err(io_error()),
    };
    let root_stat = inspect_directory(root.as_fd(), None)?;
    if root_stat.st_uid != expected_root_uid {
        return Err(unsafe_error());
    }
    let owner = (root_stat.st_uid, root_stat.st_gid);
    let lock = match fs::openat(&root, CATALOG_LOCK_FILE, FILE_FLAGS, Mode::empty()) {
        Ok(lock) => lock,
        Err(Errno::NOENT) => return Err(recovery_required()),
        Err(Errno::LOOP | Errno::NOTDIR | Errno::ISDIR) => return Err(unsafe_error()),
        Err(_) => return Err(io_error()),
    };
    inspect_private_file(lock.as_fd(), owner, 0)?;
    match fs::flock(&lock, FlockOperation::NonBlockingLockShared) {
        Ok(()) => Ok(Some(CatalogReadGuard { root, owner, lock })),
        Err(Errno::AGAIN) => Err(busy_error()),
        Err(_) => Err(io_error()),
    }
}

struct MatchedInstallation {
    installation_id: InstallationId,
    directory: OwnedFd,
    project_file: File,
}

fn find_project_installation(
    installations: &OwnedFd,
    owner: (u32, u32),
    project: &ProjectIdentity,
) -> Result<Option<MatchedInstallation>, PersonalWorkerRuntimeManifestDiscoveryError> {
    let mut entries = Dir::read_from(installations).map_err(|_| io_error())?;
    let mut count = 0_usize;
    let mut matched = None;
    for entry in &mut entries {
        let entry = entry.map_err(|_| io_error())?;
        let name = entry.file_name();
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        count = count.checked_add(1).ok_or_else(corrupt_error)?;
        if count > MAX_INSTALLATIONS {
            return Err(corrupt_error());
        }
        let installation_id = parse_installation_id(name)?;
        let directory = fs::openat(
            installations,
            installation_id.as_str(),
            DIRECTORY_FLAGS,
            Mode::empty(),
        )
        .map_err(map_required_directory_open_error)?;
        inspect_directory(directory.as_fd(), Some(owner))?;
        let project_file = open_required_private_file(
            directory.as_fd(),
            PROJECT_FILE,
            owner,
            MAX_STATE_DOCUMENT_BYTES,
        )?;
        let document = decode_project_document(&project_file.bytes)?;
        if document.installation_id() != &installation_id {
            return Err(corrupt_error());
        }
        verify_file_entry(
            directory.as_fd(),
            PROJECT_FILE,
            project_file.file.as_fd(),
            owner,
            MAX_STATE_DOCUMENT_BYTES,
        )?;
        verify_directory_entry(
            installations.as_fd(),
            installation_id.as_str(),
            directory.as_fd(),
            owner,
        )?;
        if document.project() == project {
            if matched.is_some() {
                return Err(corrupt_error());
            }
            matched = Some(MatchedInstallation {
                installation_id,
                directory,
                project_file: project_file.file,
            });
        }
    }
    Ok(matched)
}

struct OpenedFile {
    file: File,
    bytes: Vec<u8>,
}

fn open_required_private_file(
    parent: BorrowedFd<'_>,
    name: &str,
    owner: (u32, u32),
    max_bytes: usize,
) -> Result<OpenedFile, PersonalWorkerRuntimeManifestDiscoveryError> {
    let file =
        fs::openat(parent, name, FILE_FLAGS, Mode::empty()).map_err(|error| match error {
            Errno::LOOP | Errno::NOTDIR | Errno::ISDIR => unsafe_error(),
            Errno::NOENT => corrupt_error(),
            _ => io_error(),
        })?;
    read_stable_private_file(file, owner, max_bytes)
}

fn open_optional_private_file(
    parent: BorrowedFd<'_>,
    name: &str,
    owner: (u32, u32),
    max_bytes: usize,
) -> Result<Option<OpenedFile>, PersonalWorkerRuntimeManifestDiscoveryError> {
    let file = match fs::openat(parent, name, FILE_FLAGS, Mode::empty()) {
        Ok(file) => file,
        Err(Errno::NOENT) => return Ok(None),
        Err(Errno::LOOP | Errno::NOTDIR | Errno::ISDIR) => return Err(unsafe_error()),
        Err(_) => return Err(io_error()),
    };
    read_stable_private_file(file, owner, max_bytes).map(Some)
}

fn read_stable_private_file(
    file: OwnedFd,
    owner: (u32, u32),
    max_bytes: usize,
) -> Result<OpenedFile, PersonalWorkerRuntimeManifestDiscoveryError> {
    let before = fs::fstat(&file).map_err(|_| io_error())?;
    inspect_private_file_stat(&before, owner, max_bytes)?;
    let mut file = File::from(file);
    let first = read_bounded(&mut file, max_bytes)?;
    file.seek(SeekFrom::Start(0)).map_err(|_| io_error())?;
    let second = read_bounded(&mut file, max_bytes)?;
    let after = fs::fstat(&file).map_err(|_| io_error())?;
    inspect_private_file_stat(&after, owner, max_bytes)?;
    if first != second || !same_file_snapshot(&before, &after) {
        return Err(changed_error());
    }
    Ok(OpenedFile { file, bytes: first })
}

fn read_bounded(
    file: &mut File,
    max_bytes: usize,
) -> Result<Vec<u8>, PersonalWorkerRuntimeManifestDiscoveryError> {
    let mut reader: Take<&mut File> = file.take((max_bytes + 1) as u64);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).map_err(|_| io_error())?;
    if bytes.len() > max_bytes {
        return Err(corrupt_error());
    }
    Ok(bytes)
}

fn decode_project_document(
    bytes: &[u8],
) -> Result<ProjectStateDocument, PersonalWorkerRuntimeManifestDiscoveryError> {
    let input = std::str::from_utf8(bytes).map_err(|_| corrupt_error())?;
    let document = decode_state_document(input).map_err(|_| corrupt_error())?;
    match document {
        StateDocument::Project(project) => Ok(project),
        StateDocument::Resource(_) => Err(corrupt_error()),
    }
}

fn parse_installation_id(
    name: &CStr,
) -> Result<InstallationId, PersonalWorkerRuntimeManifestDiscoveryError> {
    let name = std::str::from_utf8(name.to_bytes()).map_err(|_| corrupt_error())?;
    InstallationId::parse(name).map_err(|_| corrupt_error())
}

fn open_optional_directory(
    parent: BorrowedFd<'_>,
    name: &str,
    owner: (u32, u32),
) -> Result<Option<OwnedFd>, PersonalWorkerRuntimeManifestDiscoveryError> {
    let directory = match fs::openat(parent, name, DIRECTORY_FLAGS, Mode::empty()) {
        Ok(directory) => directory,
        Err(Errno::NOENT) => return Ok(None),
        Err(Errno::LOOP | Errno::NOTDIR) => return Err(unsafe_error()),
        Err(_) => return Err(io_error()),
    };
    inspect_directory(directory.as_fd(), Some(owner))?;
    Ok(Some(directory))
}

fn inspect_directory(
    directory: BorrowedFd<'_>,
    expected_owner: Option<(u32, u32)>,
) -> Result<rustix::fs::Stat, PersonalWorkerRuntimeManifestDiscoveryError> {
    let stat = fs::fstat(directory).map_err(|_| io_error())?;
    if !FileType::from_raw_mode(stat.st_mode).is_dir()
        || stat.st_mode & 0o7777 != MANAGED_DIRECTORY_MODE
        || expected_owner.is_some_and(|owner| owner != (stat.st_uid, stat.st_gid))
    {
        return Err(unsafe_error());
    }
    Ok(stat)
}

fn inspect_private_file(
    file: BorrowedFd<'_>,
    owner: (u32, u32),
    expected_size: usize,
) -> Result<(), PersonalWorkerRuntimeManifestDiscoveryError> {
    let stat = fs::fstat(file).map_err(|_| io_error())?;
    inspect_private_file_stat(&stat, owner, expected_size)?;
    if stat.st_size as u64 != expected_size as u64 {
        return Err(corrupt_error());
    }
    Ok(())
}

fn inspect_private_file_stat(
    stat: &rustix::fs::Stat,
    owner: (u32, u32),
    max_bytes: usize,
) -> Result<(), PersonalWorkerRuntimeManifestDiscoveryError> {
    if !FileType::from_raw_mode(stat.st_mode).is_file()
        || stat.st_nlink != 1
        || stat.st_mode & 0o7777 != PRIVATE_FILE_MODE
        || owner != (stat.st_uid, stat.st_gid)
    {
        return Err(unsafe_error());
    }
    if stat.st_size < 0 || stat.st_size as u64 > max_bytes as u64 {
        return Err(corrupt_error());
    }
    Ok(())
}

fn verify_matched_installation(
    installations: &OwnedFd,
    matched: &MatchedInstallation,
    owner: (u32, u32),
) -> Result<(), PersonalWorkerRuntimeManifestDiscoveryError> {
    verify_file_entry(
        matched.directory.as_fd(),
        PROJECT_FILE,
        matched.project_file.as_fd(),
        owner,
        MAX_STATE_DOCUMENT_BYTES,
    )?;
    verify_directory_entry(
        installations.as_fd(),
        matched.installation_id.as_str(),
        matched.directory.as_fd(),
        owner,
    )
}

fn finish_catalog_read(
    guard: &CatalogReadGuard,
    root_path: &Path,
) -> Result<(), PersonalWorkerRuntimeManifestDiscoveryError> {
    verify_file_entry(
        guard.root(),
        CATALOG_LOCK_FILE,
        guard.lock.as_fd(),
        guard.owner(),
        0,
    )?;
    let rebound =
        fs::open(root_path, DIRECTORY_FLAGS, Mode::empty()).map_err(|_| changed_error())?;
    let held = inspect_directory(guard.root(), Some(guard.owner()))?;
    let rebound = inspect_directory(rebound.as_fd(), Some(guard.owner()))?;
    if !same_object(&held, &rebound) {
        return Err(changed_error());
    }
    Ok(())
}

fn verify_root_absent(root_path: &Path) -> Result<(), PersonalWorkerRuntimeManifestDiscoveryError> {
    match fs::open(root_path, DIRECTORY_FLAGS, Mode::empty()) {
        Err(Errno::NOENT) => Ok(()),
        _ => Err(changed_error()),
    }
}

fn verify_entry_absent(
    parent: BorrowedFd<'_>,
    name: &str,
) -> Result<(), PersonalWorkerRuntimeManifestDiscoveryError> {
    match fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Err(Errno::NOENT) => Ok(()),
        _ => Err(changed_error()),
    }
}

fn verify_directory_entry(
    parent: BorrowedFd<'_>,
    name: &str,
    directory: BorrowedFd<'_>,
    owner: (u32, u32),
) -> Result<(), PersonalWorkerRuntimeManifestDiscoveryError> {
    let held = inspect_directory(directory, Some(owner))?;
    let path = fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| changed_error())?;
    if !same_object(&held, &path) {
        return Err(changed_error());
    }
    Ok(())
}

fn verify_file_entry(
    parent: BorrowedFd<'_>,
    name: &str,
    file: BorrowedFd<'_>,
    owner: (u32, u32),
    max_bytes: usize,
) -> Result<(), PersonalWorkerRuntimeManifestDiscoveryError> {
    let held = fs::fstat(file).map_err(|_| io_error())?;
    inspect_private_file_stat(&held, owner, max_bytes)?;
    let path = fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| changed_error())?;
    if !same_object(&held, &path) {
        return Err(changed_error());
    }
    Ok(())
}

fn same_object(left: &rustix::fs::Stat, right: &rustix::fs::Stat) -> bool {
    left.st_dev == right.st_dev
        && left.st_ino == right.st_ino
        && left.st_uid == right.st_uid
        && left.st_gid == right.st_gid
        && left.st_mode == right.st_mode
        && left.st_nlink == right.st_nlink
}

fn same_file_snapshot(left: &rustix::fs::Stat, right: &rustix::fs::Stat) -> bool {
    same_object(left, right) && left.st_size == right.st_size
}

fn map_required_directory_open_error(error: Errno) -> PersonalWorkerRuntimeManifestDiscoveryError {
    match error {
        Errno::LOOP | Errno::NOTDIR => unsafe_error(),
        Errno::NOENT => changed_error(),
        _ => io_error(),
    }
}

const fn busy_error() -> PersonalWorkerRuntimeManifestDiscoveryError {
    PersonalWorkerRuntimeManifestDiscoveryError::new(
        PersonalWorkerRuntimeManifestDiscoveryErrorKind::Busy,
        "runtime_manifest_busy",
        "personal worker runtime manifest discovery is busy",
    )
}

const fn recovery_required() -> PersonalWorkerRuntimeManifestDiscoveryError {
    PersonalWorkerRuntimeManifestDiscoveryError::new(
        PersonalWorkerRuntimeManifestDiscoveryErrorKind::RecoveryRequired,
        "runtime_manifest_recovery_required",
        "personal worker runtime state requires recovery before discovery",
    )
}

const fn version_incompatible() -> PersonalWorkerRuntimeManifestDiscoveryError {
    PersonalWorkerRuntimeManifestDiscoveryError::new(
        PersonalWorkerRuntimeManifestDiscoveryErrorKind::VersionIncompatible,
        "runtime_manifest_version_incompatible",
        "personal worker runtime manifest schema is incompatible",
    )
}

const fn corrupt_error() -> PersonalWorkerRuntimeManifestDiscoveryError {
    PersonalWorkerRuntimeManifestDiscoveryError::new(
        PersonalWorkerRuntimeManifestDiscoveryErrorKind::CorruptState,
        "runtime_manifest_corrupt",
        "personal worker runtime state is corrupt or noncanonical",
    )
}

const fn unsafe_error() -> PersonalWorkerRuntimeManifestDiscoveryError {
    PersonalWorkerRuntimeManifestDiscoveryError::new(
        PersonalWorkerRuntimeManifestDiscoveryErrorKind::UnsafeFilesystem,
        "runtime_manifest_unsafe_filesystem",
        "personal worker runtime state has unsafe filesystem evidence",
    )
}

const fn changed_error() -> PersonalWorkerRuntimeManifestDiscoveryError {
    PersonalWorkerRuntimeManifestDiscoveryError::new(
        PersonalWorkerRuntimeManifestDiscoveryErrorKind::ChangedDuringRead,
        "runtime_manifest_changed",
        "personal worker runtime state changed during discovery",
    )
}

const fn io_error() -> PersonalWorkerRuntimeManifestDiscoveryError {
    PersonalWorkerRuntimeManifestDiscoveryError::new(
        PersonalWorkerRuntimeManifestDiscoveryErrorKind::Io,
        "runtime_manifest_unavailable",
        "personal worker runtime state could not be read",
    )
}

#[cfg(test)]
mod tests {
    use std::fs as std_fs;
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use rustix::io::dup;
    use rustix::process::geteuid;

    use crate::manifest::RunnerScope;
    use crate::state_document::{ProjectStateDocument, StateDocument, encode_state_document};

    use super::*;

    const INSTALLATION_ID: &str = "runtime-discovery-0001";
    const OTHER_INSTALLATION_ID: &str = "runtime-discovery-0002";
    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(label: &str) -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "smolrunner-runtime-discovery-{label}-{}-{sequence}",
                std::process::id()
            ));
            std_fs::create_dir(&path).expect("create root");
            set_mode(&path, MANAGED_DIRECTORY_MODE);
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std_fs::remove_dir_all(&self.0);
        }
    }

    fn project() -> ProjectIdentity {
        ProjectIdentity {
            repository: "example/runtime".to_owned(),
            runner_scope: RunnerScope::Repository,
            runner_user: "runtime-runner".to_owned(),
        }
    }

    fn set_mode(path: &Path, mode: u32) {
        std_fs::set_permissions(path, std_fs::Permissions::from_mode(mode)).expect("set mode");
    }

    fn write_private(path: &Path, bytes: &[u8]) {
        std_fs::write(path, bytes).expect("write fixture");
        set_mode(path, PRIVATE_FILE_MODE);
    }

    fn manifest_bytes(installation_id: &str) -> Vec<u8> {
        format!(
            concat!(
                "{{\n",
                "  \"document_type\": \"smolrunner_personal_worker_runtime_manifest\",\n",
                "  \"schema_version\": 1,\n",
                "  \"runtime_contract_schema_version\": 1,\n",
                "  \"installation_id\": \"{}\",\n",
                "  \"runtime_generation\": 7,\n",
                "  \"image_store_generation\": 11,\n",
                "  \"platform\": \"ubuntu2404\",\n",
                "  \"architecture\": \"aarch64\",\n",
                "  \"runtime_identity_digest\": ",
                "\"sha256:1111111111111111111111111111111111111111111111111111111111111111\"\n",
                "}}\n"
            ),
            installation_id
        )
        .into_bytes()
    }

    fn prepare_root(label: &str) -> TempRoot {
        let root = TempRoot::new(label);
        write_private(&root.path().join(CATALOG_LOCK_FILE), b"");
        std_fs::create_dir(root.path().join(INSTALLATIONS_DIRECTORY)).expect("installations");
        set_mode(
            &root.path().join(INSTALLATIONS_DIRECTORY),
            MANAGED_DIRECTORY_MODE,
        );
        root
    }

    fn add_installation(
        root: &TempRoot,
        installation_id: &str,
        project: ProjectIdentity,
        include_manifest: bool,
    ) -> PathBuf {
        let id = InstallationId::parse(installation_id).expect("installation ID");
        let directory = root
            .path()
            .join(INSTALLATIONS_DIRECTORY)
            .join(installation_id);
        std_fs::create_dir(&directory).expect("installation directory");
        set_mode(&directory, MANAGED_DIRECTORY_MODE);
        let document = ProjectStateDocument::new(id, project).expect("project document");
        let encoded = encode_state_document(&StateDocument::Project(document))
            .expect("encode project document");
        write_private(&directory.join(PROJECT_FILE), encoded.as_bytes());
        if include_manifest {
            write_private(
                &directory.join(RUNTIME_MANIFEST_FILE),
                &manifest_bytes(installation_id),
            );
        }
        directory
    }

    fn discover(
        root: &Path,
    ) -> Result<PersonalWorkerRuntimeManifestDiscovery, PersonalWorkerRuntimeManifestDiscoveryError>
    {
        discover_at(root, geteuid().as_raw(), &project())
    }

    #[test]
    fn missing_root_project_and_manifest_are_read_only_missing() {
        let missing = std::env::temp_dir().join(format!(
            "smolrunner-runtime-missing-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        assert!(!missing.exists());
        assert_eq!(
            discover(&missing).expect("missing root"),
            PersonalWorkerRuntimeManifestDiscovery::Missing
        );
        assert!(!missing.exists());

        let root = prepare_root("missing-project");
        assert_eq!(
            discover(root.path()).expect("missing project"),
            PersonalWorkerRuntimeManifestDiscovery::Missing
        );
        add_installation(&root, INSTALLATION_ID, project(), false);
        assert_eq!(
            discover(root.path()).expect("missing manifest"),
            PersonalWorkerRuntimeManifestDiscovery::Missing
        );
    }

    #[test]
    fn exact_protected_project_manifest_is_recorded_not_observed() {
        let root = prepare_root("exact");
        add_installation(&root, INSTALLATION_ID, project(), true);
        let discovery = discover(root.path()).expect("discover manifest");
        let manifest = discovery.manifest().expect("recorded manifest");
        assert_eq!(
            manifest.summary().disposition(),
            crate::personal_worker_runtime_manifest::PersonalWorkerRuntimeManifestDisposition::RecordedNotObserved
        );
        let debug = format!("{discovery:?}");
        assert!(!debug.contains(INSTALLATION_ID));
        assert!(!debug.contains("sha256:"));
    }

    #[test]
    fn missing_lock_is_recovery_and_exclusive_lock_is_busy() {
        let root = TempRoot::new("missing-lock");
        let error = discover(root.path()).expect_err("missing catalog lock");
        assert_eq!(
            error.kind,
            PersonalWorkerRuntimeManifestDiscoveryErrorKind::RecoveryRequired
        );

        let root = prepare_root("busy");
        let lock = fs::open(
            root.path().join(CATALOG_LOCK_FILE),
            FILE_FLAGS,
            Mode::empty(),
        )
        .expect("open lock");
        fs::flock(&lock, FlockOperation::LockExclusive).expect("hold exclusive lock");
        let error = discover(root.path()).expect_err("busy catalog");
        assert_eq!(
            error.kind,
            PersonalWorkerRuntimeManifestDiscoveryErrorKind::Busy
        );
    }

    #[test]
    fn manifest_identity_version_and_canonical_bytes_fail_closed() {
        let root = prepare_root("manifest-errors");
        let directory = add_installation(&root, INSTALLATION_ID, project(), true);
        let manifest = directory.join(RUNTIME_MANIFEST_FILE);

        write_private(&manifest, &manifest_bytes(OTHER_INSTALLATION_ID));
        assert_eq!(
            discover(root.path()).expect_err("foreign manifest").kind,
            PersonalWorkerRuntimeManifestDiscoveryErrorKind::CorruptState
        );

        let future = String::from_utf8(manifest_bytes(INSTALLATION_ID))
            .expect("UTF-8 manifest")
            .replace("\"schema_version\": 1", "\"schema_version\": 2");
        write_private(&manifest, future.as_bytes());
        assert_eq!(
            discover(root.path()).expect_err("future manifest").kind,
            PersonalWorkerRuntimeManifestDiscoveryErrorKind::VersionIncompatible
        );

        let compact: serde_json::Value =
            serde_json::from_slice(&manifest_bytes(INSTALLATION_ID)).expect("manifest JSON");
        write_private(
            &manifest,
            &serde_json::to_vec(&compact).expect("compact manifest"),
        );
        assert_eq!(
            discover(root.path())
                .expect_err("noncanonical manifest")
                .kind,
            PersonalWorkerRuntimeManifestDiscoveryErrorKind::CorruptState
        );
    }

    #[test]
    fn unsafe_files_and_ambiguous_project_state_fail_closed() {
        let root = prepare_root("unsafe");
        let directory = add_installation(&root, INSTALLATION_ID, project(), true);
        let manifest = directory.join(RUNTIME_MANIFEST_FILE);
        set_mode(&manifest, 0o644);
        assert_eq!(
            discover(root.path()).expect_err("public manifest").kind,
            PersonalWorkerRuntimeManifestDiscoveryErrorKind::UnsafeFilesystem
        );

        std_fs::remove_file(&manifest).expect("remove manifest fixture");
        symlink(PROJECT_FILE, &manifest).expect("symlink manifest");
        assert_eq!(
            discover(root.path()).expect_err("symlink manifest").kind,
            PersonalWorkerRuntimeManifestDiscoveryErrorKind::UnsafeFilesystem
        );

        let root = prepare_root("ambiguous");
        add_installation(&root, INSTALLATION_ID, project(), true);
        add_installation(&root, OTHER_INSTALLATION_ID, project(), true);
        assert_eq!(
            discover(root.path()).expect_err("duplicate project").kind,
            PersonalWorkerRuntimeManifestDiscoveryErrorKind::CorruptState
        );
    }

    #[test]
    fn shared_guard_drop_unlocks_an_inherited_open_file_description() {
        let root = prepare_root("inherited-lock");
        let guard = open_catalog_read_guard(root.path(), geteuid().as_raw())
            .expect("open read guard")
            .expect("existing root");
        let retained = dup(&guard.lock).expect("duplicate lock descriptor");
        drop(guard);
        fs::flock(&retained, FlockOperation::NonBlockingLockExclusive)
            .expect("shared lock must be explicitly released");
    }

    #[test]
    fn final_revalidation_rejects_in_place_safety_drift() {
        let root = prepare_root("file-mode-drift");
        let installation_path = add_installation(&root, INSTALLATION_ID, project(), true);
        let guard = open_catalog_read_guard(root.path(), geteuid().as_raw())
            .expect("open read guard")
            .expect("existing root");
        let directory = fs::open(&installation_path, DIRECTORY_FLAGS, Mode::empty())
            .expect("open installation");
        let manifest = fs::openat(&directory, RUNTIME_MANIFEST_FILE, FILE_FLAGS, Mode::empty())
            .expect("open manifest");
        set_mode(&installation_path.join(RUNTIME_MANIFEST_FILE), 0o644);
        let error = verify_file_entry(
            directory.as_fd(),
            RUNTIME_MANIFEST_FILE,
            manifest.as_fd(),
            guard.owner(),
            MAX_PERSONAL_WORKER_RUNTIME_MANIFEST_BYTES,
        )
        .expect_err("unsafe final file mode");
        assert_eq!(
            error.kind,
            PersonalWorkerRuntimeManifestDiscoveryErrorKind::UnsafeFilesystem
        );

        let root = prepare_root("lock-size-drift");
        let guard = open_catalog_read_guard(root.path(), geteuid().as_raw())
            .expect("open read guard")
            .expect("existing root");
        write_private(&root.path().join(CATALOG_LOCK_FILE), b"x");
        let error = finish_catalog_read(&guard, root.path()).expect_err("nonempty catalog lock");
        assert_eq!(
            error.kind,
            PersonalWorkerRuntimeManifestDiscoveryErrorKind::CorruptState
        );
    }

    #[test]
    fn production_module_has_no_creation_persistence_process_or_readiness_authority() {
        let source = include_str!("linux_personal_worker_runtime_manifest.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("production source");
        for forbidden in [
            ["OFlags", "::CREATE"].concat(),
            ["std", "::process"].concat(),
            ["std", "::fs", "::write"].concat(),
            ["fs", "::mkdir"].concat(),
            ["fs", "::rename"].concat(),
            ["fs", "::unlink"].concat(),
            ["fs", "::fsync"].concat(),
            ["Command", "Executor"].concat(),
            ["PersonalWorkerRuntime", "EvidenceBundle"].concat(),
            ["seal_personal_worker_", "runtime_readiness"].concat(),
        ] {
            assert!(
                !source.contains(&forbidden),
                "forbidden authority: {forbidden}"
            );
        }
    }
}
