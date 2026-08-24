//! Descriptor-bound ownership observation for one immutable Git object-pool generation.
//!
//! This P1 observer proves only the authority-critical envelope: a protected control parent, an
//! exact root-owned frozen generation directory, an exact root-owned frozen `objects/` directory
//! on the same filesystem, the exact fixed #590 marker, and absence of a nested
//! `objects/info/alternates`. It retains those descriptors and revalidates through them.
//!
//! Recursive inventory, hardlink/special-entry auditing, byte accounting, Git content validation,
//! publication, and task Git preparation belong to later slices.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::os::fd::{AsFd as _, OwnedFd};
use std::path::{Component, Path};

use rustix::fs::{self as rustix_fs, AtFlags, FileType, Mode, OFlags};
use rustix::io::Errno;
use serde::Serialize;

use crate::immutable_git_object_pool::GitObjectPoolBinding;
use crate::immutable_git_object_pool_marker::{
    IMMUTABLE_GIT_OBJECT_POOL_MARKER_BYTES, IMMUTABLE_GIT_OBJECT_POOL_MARKER_SCHEMA_VERSION,
    ImmutableGitObjectPoolMarker,
};

pub const IMMUTABLE_GIT_OBJECT_POOL_OBSERVATION_SCHEMA_VERSION: u8 = 1;
pub const IMMUTABLE_GIT_OBJECT_POOL_MARKER_FILE_NAME: &str = ".smolrunner-git-object-pool-marker";

const ROOT_OWNER: (u32, u32) = (0, 0);
const DIRECTORY_FLAGS: OFlags = OFlags::PATH
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NONBLOCK);
const REDACTED: &str = "<private-immutable-git-object-pool-descriptors>";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImmutableGitObjectPoolObservationDisposition {
    OwnershipEnvelopeObserved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ImmutableGitObjectPoolObservationSummary {
    schema_version: u8,
    disposition: ImmutableGitObjectPoolObservationDisposition,
    marker_schema_version: u8,
    retained_directory_descriptors: u8,
    retained_marker_descriptor: bool,
    same_filesystem_device: bool,
    nested_alternates_absent: bool,
}

impl ImmutableGitObjectPoolObservationSummary {
    #[must_use]
    pub const fn schema_version(self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn disposition(self) -> ImmutableGitObjectPoolObservationDisposition {
        self.disposition
    }

    #[must_use]
    pub const fn marker_schema_version(self) -> u8 {
        self.marker_schema_version
    }

    #[must_use]
    pub const fn retained_directory_descriptors(self) -> u8 {
        self.retained_directory_descriptors
    }

    #[must_use]
    pub const fn retained_marker_descriptor(self) -> bool {
        self.retained_marker_descriptor
    }

    #[must_use]
    pub const fn same_filesystem_device(self) -> bool {
        self.same_filesystem_device
    }

    #[must_use]
    pub const fn nested_alternates_absent(self) -> bool {
        self.nested_alternates_absent
    }
}

/// Opaque descriptor lease for one accepted immutable Git object-pool ownership envelope.
pub struct ImmutableGitObjectPoolObservation {
    summary: ImmutableGitObjectPoolObservationSummary,
    binding: GitObjectPoolBinding,
    parent: BoundDirectory,
    generation_name: OsString,
    root: BoundDirectory,
    objects: BoundDirectory,
    info: Option<BoundDirectory>,
    marker: BoundMarker,
    authority_owner: (u32, u32),
}

impl ImmutableGitObjectPoolObservation {
    #[must_use]
    pub const fn summary(&self) -> ImmutableGitObjectPoolObservationSummary {
        self.summary
    }

    #[must_use]
    pub const fn binding(&self) -> &GitObjectPoolBinding {
        &self.binding
    }

    /// Reconfirm the exact held ownership envelope against the same logical #583 binding.
    ///
    /// # Errors
    ///
    /// Fails closed when the binding differs, any held object changes, the generation basename
    /// resolves elsewhere, the fixed marker changes, or a nested alternate appears.
    pub fn confirm(
        &mut self,
        binding: &GitObjectPoolBinding,
    ) -> Result<(), ImmutableGitObjectPoolObservationError> {
        if binding != &self.binding {
            return Err(binding_mismatch());
        }
        revalidate_observation(self, binding)
    }
}

impl fmt::Debug for ImmutableGitObjectPoolObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImmutableGitObjectPoolObservation")
            .field("summary", &self.summary)
            .field("descriptors", &REDACTED)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImmutableGitObjectPoolObservationErrorKind {
    InvalidPath,
    Missing,
    UnsafeFilesystem,
    OwnershipMismatch,
    WritableGeneration,
    FilesystemMismatch,
    MarkerMismatch,
    NestedAlternates,
    BindingMismatch,
    ChangedDuringObservation,
    Io,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct ImmutableGitObjectPoolObservationError {
    kind: ImmutableGitObjectPoolObservationErrorKind,
    code: &'static str,
    message: &'static str,
}

impl ImmutableGitObjectPoolObservationError {
    #[must_use]
    pub const fn kind(&self) -> ImmutableGitObjectPoolObservationErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for ImmutableGitObjectPoolObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImmutableGitObjectPoolObservationError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for ImmutableGitObjectPoolObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ImmutableGitObjectPoolObservationError {}

/// Observe one already-published immutable Git object-pool generation without executing Git or
/// mutating the filesystem.
///
/// # Errors
///
/// Fails closed unless the immediate control parent is root-owned and protected from group/other
/// writes; the generation root, `objects/`, optional `objects/info/`, and marker are root-owned and
/// frozen; root and `objects/` share one filesystem device; the marker verifies the supplied
/// binding; nested alternates are absent; and every held descriptor survives final revalidation.
pub fn observe_immutable_git_object_pool_generation(
    generation_path: &Path,
    binding: &GitObjectPoolBinding,
) -> Result<ImmutableGitObjectPoolObservation, ImmutableGitObjectPoolObservationError> {
    observe_with_owner(generation_path, binding, ROOT_OWNER, || {})
}

fn observe_with_owner<F>(
    generation_path: &Path,
    binding: &GitObjectPoolBinding,
    authority_owner: (u32, u32),
    before_revalidation: F,
) -> Result<ImmutableGitObjectPoolObservation, ImmutableGitObjectPoolObservationError>
where
    F: FnOnce(),
{
    let (parent_path, generation_name) = split_generation_path(generation_path)?;
    let parent = BoundDirectory::open_path(parent_path)?;
    inspect_control_parent(&parent.snapshot, authority_owner)?;

    let root = BoundDirectory::open_child(&parent.fd, &generation_name)?;
    inspect_frozen_directory(&root.snapshot, authority_owner)?;
    let objects = BoundDirectory::open_child(&root.fd, OsStr::new("objects"))?;
    inspect_frozen_directory(&objects.snapshot, authority_owner)?;
    if root.snapshot.device != objects.snapshot.device {
        return Err(filesystem_mismatch());
    }

    let info = open_optional_frozen_directory(&objects.fd, OsStr::new("info"), authority_owner)?;
    require_nested_alternates_absent(info.as_ref())?;
    let marker = BoundMarker::open(&root.fd, authority_owner, binding)?;

    let retained_directory_descriptors = if info.is_some() { 4 } else { 3 };
    let mut observation = ImmutableGitObjectPoolObservation {
        summary: ImmutableGitObjectPoolObservationSummary {
            schema_version: IMMUTABLE_GIT_OBJECT_POOL_OBSERVATION_SCHEMA_VERSION,
            disposition: ImmutableGitObjectPoolObservationDisposition::OwnershipEnvelopeObserved,
            marker_schema_version: IMMUTABLE_GIT_OBJECT_POOL_MARKER_SCHEMA_VERSION,
            retained_directory_descriptors,
            retained_marker_descriptor: true,
            same_filesystem_device: true,
            nested_alternates_absent: true,
        },
        binding: binding.clone(),
        parent,
        generation_name,
        root,
        objects,
        info,
        marker,
        authority_owner,
    };

    before_revalidation();
    revalidate_observation(&mut observation, binding)?;
    Ok(observation)
}

fn revalidate_observation(
    observation: &mut ImmutableGitObjectPoolObservation,
    binding: &GitObjectPoolBinding,
) -> Result<(), ImmutableGitObjectPoolObservationError> {
    let authority_owner = observation.authority_owner;
    observation.parent.revalidate_control(authority_owner)?;
    observation.root.revalidate_frozen(authority_owner)?;
    observation.objects.revalidate_frozen(authority_owner)?;

    let reopened_root =
        BoundDirectory::open_child(&observation.parent.fd, &observation.generation_name)
            .map_err(|_| changed())?;
    if reopened_root.snapshot != observation.root.snapshot {
        return Err(changed());
    }
    let reopened_objects = BoundDirectory::open_child(&observation.root.fd, OsStr::new("objects"))
        .map_err(|_| changed())?;
    if reopened_objects.snapshot != observation.objects.snapshot {
        return Err(changed());
    }
    if observation.root.snapshot.device != observation.objects.snapshot.device {
        return Err(changed());
    }

    revalidate_optional_info(
        &observation.objects.fd,
        observation.info.as_ref(),
        authority_owner,
    )?;
    require_nested_alternates_absent(observation.info.as_ref())?;
    observation
        .marker
        .revalidate(&observation.root.fd, authority_owner, binding)?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DirectorySnapshot {
    device: u64,
    inode: u64,
    uid: u32,
    gid: u32,
    mode: u32,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}

struct BoundDirectory {
    fd: OwnedFd,
    snapshot: DirectorySnapshot,
}

impl BoundDirectory {
    fn open_path(path: &Path) -> Result<Self, ImmutableGitObjectPoolObservationError> {
        let fd = open_directory_path(path)?;
        let snapshot = snapshot_directory(&fd)?;
        Ok(Self { fd, snapshot })
    }

    fn open_child(
        parent: &OwnedFd,
        name: &OsStr,
    ) -> Result<Self, ImmutableGitObjectPoolObservationError> {
        let fd = rustix_fs::openat(parent.as_fd(), name, DIRECTORY_FLAGS, Mode::empty())
            .map_err(map_open)?;
        let snapshot = snapshot_directory(&fd)?;
        Ok(Self { fd, snapshot })
    }

    fn revalidate_control(
        &self,
        owner: (u32, u32),
    ) -> Result<(), ImmutableGitObjectPoolObservationError> {
        let current = snapshot_directory(&self.fd).map_err(|_| changed())?;
        if current != self.snapshot {
            return Err(changed());
        }
        inspect_control_parent(&current, owner).map_err(|_| changed())
    }

    fn revalidate_frozen(
        &self,
        owner: (u32, u32),
    ) -> Result<(), ImmutableGitObjectPoolObservationError> {
        let current = snapshot_directory(&self.fd).map_err(|_| changed())?;
        if current != self.snapshot {
            return Err(changed());
        }
        inspect_frozen_directory(&current, owner).map_err(|_| changed())
    }
}

struct BoundMarker {
    file: File,
    snapshot: FileSnapshot,
    bytes: [u8; IMMUTABLE_GIT_OBJECT_POOL_MARKER_BYTES],
}

impl BoundMarker {
    fn open(
        parent: &OwnedFd,
        owner: (u32, u32),
        binding: &GitObjectPoolBinding,
    ) -> Result<Self, ImmutableGitObjectPoolObservationError> {
        let fd = rustix_fs::openat(
            parent.as_fd(),
            IMMUTABLE_GIT_OBJECT_POOL_MARKER_FILE_NAME,
            FILE_FLAGS,
            Mode::empty(),
        )
        .map_err(map_open)?;
        let mut file = File::from(fd);
        let before = snapshot_file(&file)?;
        inspect_marker(&before, owner)?;
        let first = read_marker(&mut file)?;
        let second = read_marker(&mut file)?;
        let after = snapshot_file(&file)?;
        if first != second || before != after {
            return Err(changed());
        }
        ImmutableGitObjectPoolMarker::decode_and_verify(&first, binding)
            .map_err(|_| marker_mismatch())?;
        Ok(Self {
            file,
            snapshot: after,
            bytes: first,
        })
    }

    fn revalidate(
        &mut self,
        parent: &OwnedFd,
        owner: (u32, u32),
        binding: &GitObjectPoolBinding,
    ) -> Result<(), ImmutableGitObjectPoolObservationError> {
        let before = snapshot_file(&self.file).map_err(|_| changed())?;
        if before != self.snapshot {
            return Err(changed());
        }
        inspect_marker(&before, owner).map_err(|_| changed())?;
        let first = read_marker(&mut self.file).map_err(|_| changed())?;
        let second = read_marker(&mut self.file).map_err(|_| changed())?;
        let after = snapshot_file(&self.file).map_err(|_| changed())?;
        let path = rustix_fs::statat(
            parent.as_fd(),
            IMMUTABLE_GIT_OBJECT_POOL_MARKER_FILE_NAME,
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(|_| changed())?;
        let path = snapshot_file_stat(&path).map_err(|_| changed())?;
        if first != self.bytes
            || second != self.bytes
            || after != self.snapshot
            || path != self.snapshot
        {
            return Err(changed());
        }
        ImmutableGitObjectPoolMarker::decode_and_verify(&first, binding).map_err(|_| changed())?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileSnapshot {
    device: u64,
    inode: u64,
    uid: u32,
    gid: u32,
    mode: u32,
    nlink: u128,
    size: i64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}

fn split_generation_path(
    path: &Path,
) -> Result<(&Path, OsString), ImmutableGitObjectPoolObservationError> {
    if !path.is_absolute()
        || path == Path::new("/")
        || path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(invalid_path());
    }
    let parent = path.parent().ok_or_else(invalid_path)?;
    let name = path.file_name().ok_or_else(invalid_path)?.to_os_string();
    Ok((parent, name))
}

fn open_directory_path(path: &Path) -> Result<OwnedFd, ImmutableGitObjectPoolObservationError> {
    let mut current =
        rustix_fs::open(Path::new("/"), DIRECTORY_FLAGS, Mode::empty()).map_err(map_open)?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                current = rustix_fs::openat(current.as_fd(), name, DIRECTORY_FLAGS, Mode::empty())
                    .map_err(map_open)?;
            }
            _ => return Err(invalid_path()),
        }
    }
    Ok(current)
}

fn snapshot_directory(
    descriptor: &OwnedFd,
) -> Result<DirectorySnapshot, ImmutableGitObjectPoolObservationError> {
    let stat = rustix_fs::fstat(descriptor).map_err(|_| io_error())?;
    if !FileType::from_raw_mode(stat.st_mode).is_dir() {
        return Err(unsafe_filesystem());
    }
    Ok(DirectorySnapshot {
        device: stat.st_dev,
        inode: stat.st_ino,
        uid: stat.st_uid,
        gid: stat.st_gid,
        mode: stat.st_mode,
        mtime: stat.st_mtime,
        mtime_nsec: i64::try_from(stat.st_mtime_nsec).map_err(|_| unsafe_filesystem())?,
        ctime: stat.st_ctime,
        ctime_nsec: i64::try_from(stat.st_ctime_nsec).map_err(|_| unsafe_filesystem())?,
    })
}

fn snapshot_file(file: &File) -> Result<FileSnapshot, ImmutableGitObjectPoolObservationError> {
    let stat = rustix_fs::fstat(file).map_err(|_| io_error())?;
    snapshot_file_stat(&stat)
}

fn snapshot_file_stat(
    stat: &rustix_fs::Stat,
) -> Result<FileSnapshot, ImmutableGitObjectPoolObservationError> {
    if !FileType::from_raw_mode(stat.st_mode).is_file() {
        return Err(unsafe_filesystem());
    }
    Ok(FileSnapshot {
        device: stat.st_dev,
        inode: stat.st_ino,
        uid: stat.st_uid,
        gid: stat.st_gid,
        mode: stat.st_mode,
        nlink: u128::from(stat.st_nlink),
        size: stat.st_size,
        mtime: stat.st_mtime,
        mtime_nsec: i64::try_from(stat.st_mtime_nsec).map_err(|_| unsafe_filesystem())?,
        ctime: stat.st_ctime,
        ctime_nsec: i64::try_from(stat.st_ctime_nsec).map_err(|_| unsafe_filesystem())?,
    })
}

fn inspect_control_parent(
    snapshot: &DirectorySnapshot,
    owner: (u32, u32),
) -> Result<(), ImmutableGitObjectPoolObservationError> {
    if (snapshot.uid, snapshot.gid) != owner {
        return Err(ownership_mismatch());
    }
    if snapshot.mode & 0o022 != 0 {
        return Err(unsafe_filesystem());
    }
    Ok(())
}

fn inspect_frozen_directory(
    snapshot: &DirectorySnapshot,
    owner: (u32, u32),
) -> Result<(), ImmutableGitObjectPoolObservationError> {
    if (snapshot.uid, snapshot.gid) != owner {
        return Err(ownership_mismatch());
    }
    if snapshot.mode & 0o222 != 0 {
        return Err(writable_generation());
    }
    Ok(())
}

fn inspect_marker(
    snapshot: &FileSnapshot,
    owner: (u32, u32),
) -> Result<(), ImmutableGitObjectPoolObservationError> {
    if (snapshot.uid, snapshot.gid) != owner {
        return Err(ownership_mismatch());
    }
    if snapshot.mode & 0o222 != 0 {
        return Err(writable_generation());
    }
    if snapshot.nlink != 1
        || snapshot.size
            != i64::try_from(IMMUTABLE_GIT_OBJECT_POOL_MARKER_BYTES)
                .map_err(|_| unsafe_filesystem())?
    {
        return Err(unsafe_filesystem());
    }
    Ok(())
}

fn open_optional_frozen_directory(
    parent: &OwnedFd,
    name: &OsStr,
    owner: (u32, u32),
) -> Result<Option<BoundDirectory>, ImmutableGitObjectPoolObservationError> {
    match rustix_fs::openat(parent.as_fd(), name, DIRECTORY_FLAGS, Mode::empty()) {
        Ok(fd) => {
            let directory = BoundDirectory {
                snapshot: snapshot_directory(&fd)?,
                fd,
            };
            inspect_frozen_directory(&directory.snapshot, owner)?;
            Ok(Some(directory))
        }
        Err(Errno::NOENT) => Ok(None),
        Err(cause) => Err(map_open(cause)),
    }
}

fn revalidate_optional_info(
    objects: &OwnedFd,
    expected: Option<&BoundDirectory>,
    owner: (u32, u32),
) -> Result<(), ImmutableGitObjectPoolObservationError> {
    match expected {
        Some(expected) => {
            expected.revalidate_frozen(owner)?;
            let current =
                BoundDirectory::open_child(objects, OsStr::new("info")).map_err(|_| changed())?;
            if current.snapshot != expected.snapshot {
                return Err(changed());
            }
        }
        None => match rustix_fs::openat(
            objects.as_fd(),
            OsStr::new("info"),
            DIRECTORY_FLAGS,
            Mode::empty(),
        ) {
            Err(Errno::NOENT) => {}
            _ => return Err(changed()),
        },
    }
    Ok(())
}

fn require_nested_alternates_absent(
    info: Option<&BoundDirectory>,
) -> Result<(), ImmutableGitObjectPoolObservationError> {
    let Some(info) = info else {
        return Ok(());
    };
    match rustix_fs::statat(info.fd.as_fd(), "alternates", AtFlags::SYMLINK_NOFOLLOW) {
        Err(Errno::NOENT) => Ok(()),
        Ok(_) => Err(nested_alternates()),
        Err(_) => Err(io_error()),
    }
}

fn read_marker(
    file: &mut File,
) -> Result<[u8; IMMUTABLE_GIT_OBJECT_POOL_MARKER_BYTES], ImmutableGitObjectPoolObservationError> {
    file.seek(SeekFrom::Start(0)).map_err(|_| io_error())?;
    let mut bytes = [0_u8; IMMUTABLE_GIT_OBJECT_POOL_MARKER_BYTES];
    file.read_exact(&mut bytes).map_err(|_| io_error())?;
    let mut extra = [0_u8; 1];
    if file.read(&mut extra).map_err(|_| io_error())? != 0 {
        return Err(unsafe_filesystem());
    }
    Ok(bytes)
}

fn map_open(cause: Errno) -> ImmutableGitObjectPoolObservationError {
    match cause {
        Errno::NOENT => missing(),
        Errno::LOOP | Errno::NOTDIR => unsafe_filesystem(),
        _ => io_error(),
    }
}

const fn error(
    kind: ImmutableGitObjectPoolObservationErrorKind,
    code: &'static str,
    message: &'static str,
) -> ImmutableGitObjectPoolObservationError {
    ImmutableGitObjectPoolObservationError {
        kind,
        code,
        message,
    }
}

const fn invalid_path() -> ImmutableGitObjectPoolObservationError {
    error(
        ImmutableGitObjectPoolObservationErrorKind::InvalidPath,
        "immutable_git_object_pool_path_invalid",
        "immutable Git object-pool generation path is invalid",
    )
}

const fn missing() -> ImmutableGitObjectPoolObservationError {
    error(
        ImmutableGitObjectPoolObservationErrorKind::Missing,
        "immutable_git_object_pool_missing",
        "immutable Git object-pool ownership object is missing",
    )
}

const fn unsafe_filesystem() -> ImmutableGitObjectPoolObservationError {
    error(
        ImmutableGitObjectPoolObservationErrorKind::UnsafeFilesystem,
        "immutable_git_object_pool_filesystem_unsafe",
        "immutable Git object-pool ownership envelope is unsafe",
    )
}

const fn ownership_mismatch() -> ImmutableGitObjectPoolObservationError {
    error(
        ImmutableGitObjectPoolObservationErrorKind::OwnershipMismatch,
        "immutable_git_object_pool_owner_mismatch",
        "immutable Git object-pool ownership differs from control authority",
    )
}

const fn writable_generation() -> ImmutableGitObjectPoolObservationError {
    error(
        ImmutableGitObjectPoolObservationErrorKind::WritableGeneration,
        "immutable_git_object_pool_generation_writable",
        "immutable Git object-pool ownership object remains writable",
    )
}

const fn filesystem_mismatch() -> ImmutableGitObjectPoolObservationError {
    error(
        ImmutableGitObjectPoolObservationErrorKind::FilesystemMismatch,
        "immutable_git_object_pool_filesystem_mismatch",
        "immutable Git object-pool root and objects directory use different filesystems",
    )
}

const fn marker_mismatch() -> ImmutableGitObjectPoolObservationError {
    error(
        ImmutableGitObjectPoolObservationErrorKind::MarkerMismatch,
        "immutable_git_object_pool_marker_mismatch",
        "immutable Git object-pool fixed marker differs from the expected generation",
    )
}

const fn nested_alternates() -> ImmutableGitObjectPoolObservationError {
    error(
        ImmutableGitObjectPoolObservationErrorKind::NestedAlternates,
        "immutable_git_object_pool_nested_alternates",
        "immutable Git object-pool objects directory contains a nested alternates file",
    )
}

const fn binding_mismatch() -> ImmutableGitObjectPoolObservationError {
    error(
        ImmutableGitObjectPoolObservationErrorKind::BindingMismatch,
        "immutable_git_object_pool_binding_mismatch",
        "immutable Git object-pool observation belongs to another logical generation",
    )
}

const fn changed() -> ImmutableGitObjectPoolObservationError {
    error(
        ImmutableGitObjectPoolObservationErrorKind::ChangedDuringObservation,
        "immutable_git_object_pool_changed",
        "immutable Git object-pool ownership envelope changed during observation",
    )
}

const fn io_error() -> ImmutableGitObjectPoolObservationError {
    error(
        ImmutableGitObjectPoolObservationErrorKind::Io,
        "immutable_git_object_pool_io",
        "immutable Git object-pool ownership observation failed",
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::immutable_git_object_pool::{
        GitObjectFormat, GitObjectPoolBinding, GitObjectPoolGeneration, GitObjectPoolId,
        GitObjectPoolProducerGenerationId, GitObjectPoolTrustGenerationId,
    };
    use crate::immutable_git_object_pool_marker::{
        GitObjectPoolMarkerNonce, ImmutableGitObjectPoolMarker,
    };
    use crate::project_catalog::ProjectIdentity;
    use crate::project_disk_lease::{ProjectDiskGeneration, ProjectDiskId};

    use super::{
        IMMUTABLE_GIT_OBJECT_POOL_MARKER_FILE_NAME, ImmutableGitObjectPoolObservationDisposition,
        ImmutableGitObjectPoolObservationErrorKind, observe_with_owner,
    };

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

    fn binding(generation: u64) -> GitObjectPoolBinding {
        GitObjectPoolBinding::new(
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            GitObjectPoolId::parse("pool-a").unwrap(),
            GitObjectPoolGeneration::new(generation).unwrap(),
            ProjectDiskId::parse("disk-a").unwrap(),
            ProjectDiskGeneration::new(1).unwrap(),
            GitObjectFormat::Sha1,
            GitObjectPoolProducerGenerationId::parse("producer-a").unwrap(),
            GitObjectPoolTrustGenerationId::parse("trust-a").unwrap(),
        )
    }

    struct Fixture {
        base: PathBuf,
        parent: PathBuf,
        pool: PathBuf,
        objects: PathBuf,
        info: PathBuf,
        marker: PathBuf,
        owner: (u32, u32),
    }

    impl Fixture {
        fn new(marker_binding: &GitObjectPoolBinding, alternates: bool) -> Self {
            let unique = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let base = std::env::temp_dir().join(format!(
                "smolrunner-git-pool-observation-{}-{unique}",
                std::process::id()
            ));
            let parent = base.join("parent");
            let pool = parent.join("generation");
            let objects = pool.join("objects");
            let info = objects.join("info");
            fs::create_dir_all(&info).unwrap();
            let owner_metadata = fs::metadata(&parent).unwrap();
            let owner = (owner_metadata.uid(), owner_metadata.gid());
            fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();

            let marker = pool.join(IMMUTABLE_GIT_OBJECT_POOL_MARKER_FILE_NAME);
            let marker_bytes = ImmutableGitObjectPoolMarker::new(
                marker_binding,
                GitObjectPoolMarkerNonce::new([7; 16]).unwrap(),
            )
            .unwrap()
            .encode()
            .unwrap();
            fs::write(&marker, marker_bytes).unwrap();
            if alternates {
                fs::write(info.join("alternates"), b"/unexpected/objects\n").unwrap();
            }
            fs::set_permissions(&marker, fs::Permissions::from_mode(0o444)).unwrap();
            fs::set_permissions(&info, fs::Permissions::from_mode(0o555)).unwrap();
            fs::set_permissions(&objects, fs::Permissions::from_mode(0o555)).unwrap();
            fs::set_permissions(&pool, fs::Permissions::from_mode(0o555)).unwrap();
            Self {
                base,
                parent,
                pool,
                objects,
                info,
                marker,
                owner,
            }
        }

        fn thaw_known_paths(&self) {
            for directory in [
                self.pool
                    .with_file_name("generation-old")
                    .join("objects/info"),
                self.pool.with_file_name("generation-old").join("objects"),
                self.pool.with_file_name("generation-old"),
                self.info.clone(),
                self.objects.clone(),
                self.pool.clone(),
                self.parent.clone(),
            ] {
                if directory.is_dir() {
                    let _ = fs::set_permissions(&directory, fs::Permissions::from_mode(0o755));
                }
            }
            for marker in [
                self.pool
                    .with_file_name("generation-old")
                    .join(IMMUTABLE_GIT_OBJECT_POOL_MARKER_FILE_NAME),
                self.marker.clone(),
            ] {
                if marker.is_file() {
                    let _ = fs::set_permissions(&marker, fs::Permissions::from_mode(0o644));
                }
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.thaw_known_paths();
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    #[test]
    fn exact_descriptor_bound_ownership_envelope_is_accepted() {
        let expected = binding(1);
        let fixture = Fixture::new(&expected, false);
        let mut observation =
            observe_with_owner(&fixture.pool, &expected, fixture.owner, || {}).unwrap();
        assert_eq!(
            observation.summary().disposition(),
            ImmutableGitObjectPoolObservationDisposition::OwnershipEnvelopeObserved
        );
        assert_eq!(observation.summary().retained_directory_descriptors(), 4);
        assert!(observation.summary().retained_marker_descriptor());
        assert!(observation.summary().same_filesystem_device());
        assert!(observation.summary().nested_alternates_absent());
        observation.confirm(&expected).unwrap();
    }

    #[test]
    fn wrong_marker_binding_is_refused() {
        let expected = binding(1);
        let fixture = Fixture::new(&binding(2), false);
        assert_eq!(
            observe_with_owner(&fixture.pool, &expected, fixture.owner, || {})
                .unwrap_err()
                .kind(),
            ImmutableGitObjectPoolObservationErrorKind::MarkerMismatch
        );
    }

    #[test]
    fn writable_root_or_objects_are_refused() {
        let expected = binding(1);
        for target in ["root", "objects"] {
            let fixture = Fixture::new(&expected, false);
            let path = if target == "root" {
                &fixture.pool
            } else {
                &fixture.objects
            };
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(
                observe_with_owner(&fixture.pool, &expected, fixture.owner, || {})
                    .unwrap_err()
                    .kind(),
                ImmutableGitObjectPoolObservationErrorKind::WritableGeneration
            );
        }
    }

    #[test]
    fn nested_alternates_are_refused() {
        let expected = binding(1);
        let fixture = Fixture::new(&expected, true);
        assert_eq!(
            observe_with_owner(&fixture.pool, &expected, fixture.owner, || {})
                .unwrap_err()
                .kind(),
            ImmutableGitObjectPoolObservationErrorKind::NestedAlternates
        );
    }

    #[test]
    fn same_name_generation_replacement_is_detected_from_held_parent() {
        let expected = binding(1);
        let fixture = Fixture::new(&expected, false);
        let pool = fixture.pool.clone();
        let old = pool.with_file_name("generation-old");
        let result = observe_with_owner(&pool, &expected, fixture.owner, || {
            fs::rename(&pool, &old).unwrap();
            fs::create_dir(&pool).unwrap();
        });
        assert_eq!(
            result.unwrap_err().kind(),
            ImmutableGitObjectPoolObservationErrorKind::ChangedDuringObservation
        );
    }
}
