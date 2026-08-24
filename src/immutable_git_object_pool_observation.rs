use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::File;
use std::os::fd::{AsFd as _, OwnedFd};
use std::os::unix::fs::{FileExt as _, MetadataExt as _};
use std::path::{Component, Path, PathBuf};

use rustix::fs::{self as rustix_fs, FileType, Mode, OFlags};
use rustix::io::Errno;
use serde::Serialize;

use crate::artifact::Sha256Digest;
use crate::immutable_git_object_pool::{GitObjectFormat, GitObjectPoolBinding};
use crate::immutable_git_object_pool_marker::{
    IMMUTABLE_GIT_OBJECT_POOL_MARKER_BYTES, ImmutableGitObjectPoolMarker,
    git_object_pool_binding_digest,
};

pub const IMMUTABLE_GIT_OBJECT_POOL_OBSERVATION_SCHEMA_VERSION: u8 = 1;
const ROOT_UID: u32 = 0;
const ROOT_GID: u32 = 0;
const PARENT_MODE: u32 = 0o755;
const GENERATION_MODE: u32 = 0o555;
const FILE_MODE: u32 = 0o444;
const MARKER_NAME: &str = ".smolrunner-git-object-pool";
const OBJECTS_NAME: &str = "objects";
const INFO_NAME: &str = "info";
const ALTERNATES_NAME: &str = "alternates";
const DIRECTORY_FLAGS: OFlags = OFlags::PATH
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NONBLOCK);

#[derive(Clone, PartialEq, Eq)]
pub struct ImmutableGitObjectPoolLocation {
    parent: PathBuf,
    generation_name: OsString,
}

impl ImmutableGitObjectPoolLocation {
    /// Bind one private absolute generation path without touching the filesystem.
    ///
    /// # Errors
    ///
    /// Returns a bounded error unless the path is absolute, non-root, lexically normalized, and the
    /// final generation name is one ordinary non-empty component.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, ImmutableGitObjectPoolObservationError> {
        let path = path.into();
        validate_absolute_path(&path)?;
        let parent = path.parent().ok_or_else(invalid_path)?.to_path_buf();
        let generation_name = path.file_name().ok_or_else(invalid_path)?.to_os_string();
        if generation_name.is_empty() {
            return Err(invalid_path());
        }
        Ok(Self {
            parent,
            generation_name,
        })
    }
}

impl fmt::Debug for ImmutableGitObjectPoolLocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let _ = (&self.parent, &self.generation_name);
        formatter.write_str("<private immutable Git object-pool location>")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImmutableGitObjectPoolObservationDisposition {
    RootOwnedFrozenGeneration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImmutableGitObjectPoolObservationSummary {
    schema_version: u8,
    disposition: ImmutableGitObjectPoolObservationDisposition,
    pool_generation: u64,
    object_format: GitObjectFormat,
    binding_digest: Sha256Digest,
    marker_matched: bool,
    objects_directory_bound: bool,
    nested_alternates_absent: bool,
}

impl ImmutableGitObjectPoolObservationSummary {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn disposition(&self) -> ImmutableGitObjectPoolObservationDisposition {
        self.disposition
    }

    #[must_use]
    pub const fn pool_generation(&self) -> u64 {
        self.pool_generation
    }

    #[must_use]
    pub const fn object_format(&self) -> GitObjectFormat {
        self.object_format
    }

    #[must_use]
    pub const fn binding_digest(&self) -> &Sha256Digest {
        &self.binding_digest
    }

    #[must_use]
    pub const fn marker_matched(&self) -> bool {
        self.marker_matched
    }

    #[must_use]
    pub const fn objects_directory_bound(&self) -> bool {
        self.objects_directory_bound
    }

    #[must_use]
    pub const fn nested_alternates_absent(&self) -> bool {
        self.nested_alternates_absent
    }
}

/// Descriptor-bound read-only evidence for one already-published immutable Git pool generation.
///
/// The observation retains the exact root parent, generation root, and `objects/` descriptors. It
/// carries no creation, freeze, Git, GC, task-clone, mount, deletion, or adoption authority.
pub struct ImmutableGitObjectPoolObservation {
    summary: ImmutableGitObjectPoolObservationSummary,
    binding: GitObjectPoolBinding,
    parent: OwnedFd,
    generation_name: OsString,
    root: OwnedFd,
    objects: OwnedFd,
    parent_identity: DirectoryIdentity,
    root_snapshot: DirectorySnapshot,
    objects_snapshot: DirectorySnapshot,
    expected_owner_uid: u32,
    expected_owner_gid: u32,
}

impl ImmutableGitObjectPoolObservation {
    #[must_use]
    pub const fn summary(&self) -> &ImmutableGitObjectPoolObservationSummary {
        &self.summary
    }

    /// Reconfirm the exact held generation without resolving its original full path.
    ///
    /// # Errors
    ///
    /// Returns a bounded path-private error if the parent authority, parent entry, frozen root,
    /// `objects/` directory, generation marker, or no-alternates prerequisite changed.
    pub fn confirm(&self) -> Result<(), ImmutableGitObjectPoolObservationError> {
        require_parent_identity(
            &self.parent,
            self.expected_owner_uid,
            self.expected_owner_gid,
            &self.parent_identity,
        )?;
        require_current_directory_entry(
            &self.parent,
            self.generation_name.as_os_str(),
            self.expected_owner_uid,
            self.expected_owner_gid,
            GENERATION_MODE,
            &self.root_snapshot,
        )?;
        require_directory_snapshot(
            &self.root,
            self.expected_owner_uid,
            self.expected_owner_gid,
            GENERATION_MODE,
            &self.root_snapshot,
        )?;
        require_current_directory_entry(
            &self.root,
            OsStr::new(OBJECTS_NAME),
            self.expected_owner_uid,
            self.expected_owner_gid,
            GENERATION_MODE,
            &self.objects_snapshot,
        )?;
        require_directory_snapshot(
            &self.objects,
            self.expected_owner_uid,
            self.expected_owner_gid,
            GENERATION_MODE,
            &self.objects_snapshot,
        )?;
        verify_marker(
            &self.root,
            &self.binding,
            self.expected_owner_uid,
            self.expected_owner_gid,
        )?;
        require_no_nested_alternates(
            &self.objects,
            &self.objects_snapshot,
            self.expected_owner_uid,
            self.expected_owner_gid,
        )?;
        require_directory_snapshot(
            &self.objects,
            self.expected_owner_uid,
            self.expected_owner_gid,
            GENERATION_MODE,
            &self.objects_snapshot,
        )?;
        require_current_directory_entry(
            &self.root,
            OsStr::new(OBJECTS_NAME),
            self.expected_owner_uid,
            self.expected_owner_gid,
            GENERATION_MODE,
            &self.objects_snapshot,
        )?;
        require_directory_snapshot(
            &self.root,
            self.expected_owner_uid,
            self.expected_owner_gid,
            GENERATION_MODE,
            &self.root_snapshot,
        )?;
        require_current_directory_entry(
            &self.parent,
            self.generation_name.as_os_str(),
            self.expected_owner_uid,
            self.expected_owner_gid,
            GENERATION_MODE,
            &self.root_snapshot,
        )?;
        require_parent_identity(
            &self.parent,
            self.expected_owner_uid,
            self.expected_owner_gid,
            &self.parent_identity,
        )?;
        Ok(())
    }
}

impl fmt::Debug for ImmutableGitObjectPoolObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let _ = (
            &self.parent,
            &self.generation_name,
            &self.root,
            &self.objects,
            &self.parent_identity,
            &self.root_snapshot,
            &self.objects_snapshot,
        );
        formatter
            .debug_struct("ImmutableGitObjectPoolObservation")
            .field("summary", &self.summary)
            .field(
                "private_filesystem_evidence",
                &"<descriptor-bound frozen Git pool>",
            )
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImmutableGitObjectPoolObservationErrorKind {
    InvalidPath,
    Missing,
    UnsafeFilesystem,
    MarkerMismatch,
    NestedAlternate,
    Changed,
    Io,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ImmutableGitObjectPoolObservationError {
    kind: ImmutableGitObjectPoolObservationErrorKind,
    code: &'static str,
    message: &'static str,
}

impl ImmutableGitObjectPoolObservationError {
    #[must_use]
    pub const fn kind(self) -> ImmutableGitObjectPoolObservationErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
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

/// Observe one root-owned frozen immutable Git pool generation without mutation or child processes.
///
/// # Errors
///
/// Fails closed unless the exact parent/generation/objects descriptors, fixed #590 marker, ownership,
/// frozen modes, one-filesystem binding, and no-nested-alternates prerequisite are all proven.
pub fn observe_immutable_git_object_pool(
    binding: &GitObjectPoolBinding,
    location: ImmutableGitObjectPoolLocation,
) -> Result<ImmutableGitObjectPoolObservation, ImmutableGitObjectPoolObservationError> {
    observe_with_owner(binding, location, ROOT_UID, ROOT_GID)
}

fn observe_with_owner(
    binding: &GitObjectPoolBinding,
    location: ImmutableGitObjectPoolLocation,
    expected_owner_uid: u32,
    expected_owner_gid: u32,
) -> Result<ImmutableGitObjectPoolObservation, ImmutableGitObjectPoolObservationError> {
    let parent = open_absolute_directory(&location.parent)?;
    let parent_identity = snapshot_directory_identity(&parent)?;
    require_parent(
        &parent_identity,
        expected_owner_uid,
        expected_owner_gid,
        PARENT_MODE,
    )?;
    let root = rustix_fs::openat(
        parent.as_fd(),
        &location.generation_name,
        DIRECTORY_FLAGS,
        Mode::empty(),
    )
    .map_err(map_open)?;
    let root_snapshot = snapshot_directory(&root)?;
    require_frozen_directory(
        &root_snapshot,
        expected_owner_uid,
        expected_owner_gid,
        GENERATION_MODE,
    )?;
    if root_snapshot.identity.device != parent_identity.device {
        return Err(unsafe_filesystem());
    }
    let objects = rustix_fs::openat(root.as_fd(), OBJECTS_NAME, DIRECTORY_FLAGS, Mode::empty())
        .map_err(map_open)?;
    let objects_snapshot = snapshot_directory(&objects)?;
    require_frozen_directory(
        &objects_snapshot,
        expected_owner_uid,
        expected_owner_gid,
        GENERATION_MODE,
    )?;
    if objects_snapshot.identity.device != root_snapshot.identity.device {
        return Err(unsafe_filesystem());
    }
    let binding_digest = git_object_pool_binding_digest(binding).map_err(|_| marker_mismatch())?;

    let observation = ImmutableGitObjectPoolObservation {
        summary: ImmutableGitObjectPoolObservationSummary {
            schema_version: IMMUTABLE_GIT_OBJECT_POOL_OBSERVATION_SCHEMA_VERSION,
            disposition: ImmutableGitObjectPoolObservationDisposition::RootOwnedFrozenGeneration,
            pool_generation: binding.generation().get(),
            object_format: binding.object_format(),
            binding_digest,
            marker_matched: true,
            objects_directory_bound: true,
            nested_alternates_absent: true,
        },
        binding: binding.clone(),
        parent,
        generation_name: location.generation_name,
        root,
        objects,
        parent_identity,
        root_snapshot,
        objects_snapshot,
        expected_owner_uid,
        expected_owner_gid,
    };
    observation.confirm()?;
    Ok(observation)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
    uid: u32,
    gid: u32,
    mode: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DirectorySnapshot {
    identity: DirectoryIdentity,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}

fn open_absolute_directory(path: &Path) -> Result<OwnedFd, ImmutableGitObjectPoolObservationError> {
    validate_absolute_path(path)?;
    let mut current =
        rustix_fs::open(Path::new("/"), DIRECTORY_FLAGS, Mode::empty()).map_err(|_| io())?;
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

fn snapshot_directory_identity(
    fd: &OwnedFd,
) -> Result<DirectoryIdentity, ImmutableGitObjectPoolObservationError> {
    let stat = rustix_fs::fstat(fd).map_err(|_| io())?;
    if !FileType::from_raw_mode(stat.st_mode).is_dir() {
        return Err(unsafe_filesystem());
    }
    Ok(DirectoryIdentity {
        device: stat.st_dev,
        inode: stat.st_ino,
        uid: stat.st_uid,
        gid: stat.st_gid,
        mode: stat.st_mode,
    })
}

fn snapshot_directory(
    fd: &OwnedFd,
) -> Result<DirectorySnapshot, ImmutableGitObjectPoolObservationError> {
    let stat = rustix_fs::fstat(fd).map_err(|_| io())?;
    if !FileType::from_raw_mode(stat.st_mode).is_dir() {
        return Err(unsafe_filesystem());
    }
    Ok(DirectorySnapshot {
        identity: DirectoryIdentity {
            device: stat.st_dev,
            inode: stat.st_ino,
            uid: stat.st_uid,
            gid: stat.st_gid,
            mode: stat.st_mode,
        },
        mtime: stat.st_mtime,
        mtime_nsec: i64::try_from(stat.st_mtime_nsec).map_err(|_| unsafe_filesystem())?,
        ctime: stat.st_ctime,
        ctime_nsec: i64::try_from(stat.st_ctime_nsec).map_err(|_| unsafe_filesystem())?,
    })
}

fn require_parent(
    identity: &DirectoryIdentity,
    expected_uid: u32,
    expected_gid: u32,
    expected_mode: u32,
) -> Result<(), ImmutableGitObjectPoolObservationError> {
    if identity.uid != expected_uid
        || identity.gid != expected_gid
        || identity.mode & 0o7777 != expected_mode
    {
        return Err(unsafe_filesystem());
    }
    Ok(())
}

fn require_parent_identity(
    fd: &OwnedFd,
    expected_uid: u32,
    expected_gid: u32,
    expected: &DirectoryIdentity,
) -> Result<(), ImmutableGitObjectPoolObservationError> {
    let current = snapshot_directory_identity(fd)?;
    require_parent(&current, expected_uid, expected_gid, PARENT_MODE)?;
    if &current != expected {
        return Err(changed());
    }
    Ok(())
}

fn require_frozen_directory(
    snapshot: &DirectorySnapshot,
    expected_uid: u32,
    expected_gid: u32,
    expected_mode: u32,
) -> Result<(), ImmutableGitObjectPoolObservationError> {
    require_parent(
        &snapshot.identity,
        expected_uid,
        expected_gid,
        expected_mode,
    )
}

fn require_directory_snapshot(
    fd: &OwnedFd,
    expected_uid: u32,
    expected_gid: u32,
    expected_mode: u32,
    expected: &DirectorySnapshot,
) -> Result<(), ImmutableGitObjectPoolObservationError> {
    let current = snapshot_directory(fd)?;
    require_frozen_directory(&current, expected_uid, expected_gid, expected_mode)?;
    if &current != expected {
        return Err(changed());
    }
    Ok(())
}

fn require_current_directory_entry(
    parent: &OwnedFd,
    name: &OsStr,
    expected_uid: u32,
    expected_gid: u32,
    expected_mode: u32,
    expected: &DirectorySnapshot,
) -> Result<(), ImmutableGitObjectPoolObservationError> {
    let current = rustix_fs::openat(parent.as_fd(), name, DIRECTORY_FLAGS, Mode::empty())
        .map_err(map_open)?;
    require_directory_snapshot(
        &current,
        expected_uid,
        expected_gid,
        expected_mode,
        expected,
    )
}

fn verify_marker(
    root: &OwnedFd,
    binding: &GitObjectPoolBinding,
    expected_uid: u32,
    expected_gid: u32,
) -> Result<(), ImmutableGitObjectPoolObservationError> {
    let fd = rustix_fs::openat(root.as_fd(), MARKER_NAME, FILE_FLAGS, Mode::empty())
        .map_err(map_open)?;
    let file = File::from(fd);
    let before = file.metadata().map_err(|_| io())?;
    if !before.file_type().is_file()
        || before.uid() != expected_uid
        || before.gid() != expected_gid
        || before.mode() & 0o7777 != FILE_MODE
        || before.nlink() != 1
        || before.len() != IMMUTABLE_GIT_OBJECT_POOL_MARKER_BYTES as u64
    {
        return Err(marker_mismatch());
    }
    let mut bytes = [0_u8; IMMUTABLE_GIT_OBJECT_POOL_MARKER_BYTES];
    file.read_exact_at(&mut bytes, 0).map_err(|_| io())?;
    ImmutableGitObjectPoolMarker::decode_and_verify(&bytes, binding)
        .map_err(|_| marker_mismatch())?;
    let after = file.metadata().map_err(|_| io())?;
    if file_identity(&before) != file_identity(&after) {
        return Err(changed());
    }
    Ok(())
}

fn require_no_nested_alternates(
    objects: &OwnedFd,
    objects_snapshot: &DirectorySnapshot,
    expected_uid: u32,
    expected_gid: u32,
) -> Result<(), ImmutableGitObjectPoolObservationError> {
    require_directory_snapshot(
        objects,
        expected_uid,
        expected_gid,
        GENERATION_MODE,
        objects_snapshot,
    )?;
    let info = match rustix_fs::openat(objects.as_fd(), INFO_NAME, DIRECTORY_FLAGS, Mode::empty()) {
        Ok(info) => info,
        Err(Errno::NOENT) => {
            require_directory_snapshot(
                objects,
                expected_uid,
                expected_gid,
                GENERATION_MODE,
                objects_snapshot,
            )?;
            return Ok(());
        }
        Err(error) => return Err(map_open(error)),
    };
    let info_snapshot = snapshot_directory(&info)?;
    require_frozen_directory(&info_snapshot, expected_uid, expected_gid, GENERATION_MODE)?;
    match rustix_fs::openat(info.as_fd(), ALTERNATES_NAME, FILE_FLAGS, Mode::empty()) {
        Ok(_) => return Err(nested_alternate()),
        Err(Errno::NOENT) => {}
        Err(error) => return Err(map_open(error)),
    }
    require_directory_snapshot(
        &info,
        expected_uid,
        expected_gid,
        GENERATION_MODE,
        &info_snapshot,
    )?;
    require_current_directory_entry(
        objects,
        OsStr::new(INFO_NAME),
        expected_uid,
        expected_gid,
        GENERATION_MODE,
        &info_snapshot,
    )?;
    require_directory_snapshot(
        objects,
        expected_uid,
        expected_gid,
        GENERATION_MODE,
        objects_snapshot,
    )?;
    Ok(())
}

fn file_identity(
    metadata: &std::fs::Metadata,
) -> (u64, u64, u32, u32, u32, u64, u64, i64, i64, i64, i64) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.uid(),
        metadata.gid(),
        metadata.mode(),
        metadata.nlink(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

fn validate_absolute_path(path: &Path) -> Result<(), ImmutableGitObjectPoolObservationError> {
    if !path.is_absolute() || path == Path::new("/") {
        return Err(invalid_path());
    }
    let mut normal = 0_usize;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) if !name.is_empty() => normal += 1,
            _ => return Err(invalid_path()),
        }
    }
    if normal == 0 {
        return Err(invalid_path());
    }
    Ok(())
}

fn map_open(error: Errno) -> ImmutableGitObjectPoolObservationError {
    match error {
        Errno::NOENT => missing(),
        Errno::LOOP | Errno::NOTDIR => unsafe_filesystem(),
        _ => io(),
    }
}

const fn invalid_path() -> ImmutableGitObjectPoolObservationError {
    error(
        ImmutableGitObjectPoolObservationErrorKind::InvalidPath,
        "immutable_git_pool_path_invalid",
        "immutable Git object-pool location is invalid",
    )
}

const fn missing() -> ImmutableGitObjectPoolObservationError {
    error(
        ImmutableGitObjectPoolObservationErrorKind::Missing,
        "immutable_git_pool_missing",
        "immutable Git object-pool generation is missing",
    )
}

const fn unsafe_filesystem() -> ImmutableGitObjectPoolObservationError {
    error(
        ImmutableGitObjectPoolObservationErrorKind::UnsafeFilesystem,
        "immutable_git_pool_filesystem_unsafe",
        "immutable Git object-pool filesystem evidence is unsafe",
    )
}

const fn marker_mismatch() -> ImmutableGitObjectPoolObservationError {
    error(
        ImmutableGitObjectPoolObservationErrorKind::MarkerMismatch,
        "immutable_git_pool_marker_mismatch",
        "immutable Git object-pool generation marker does not match",
    )
}

const fn nested_alternate() -> ImmutableGitObjectPoolObservationError {
    error(
        ImmutableGitObjectPoolObservationErrorKind::NestedAlternate,
        "immutable_git_pool_nested_alternate",
        "immutable Git object-pool generation contains a nested alternate",
    )
}

const fn changed() -> ImmutableGitObjectPoolObservationError {
    error(
        ImmutableGitObjectPoolObservationErrorKind::Changed,
        "immutable_git_pool_changed",
        "immutable Git object-pool generation changed during observation",
    )
}

const fn io() -> ImmutableGitObjectPoolObservationError {
    error(
        ImmutableGitObjectPoolObservationErrorKind::Io,
        "immutable_git_pool_io",
        "immutable Git object-pool observation failed",
    )
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use rustix::process::{getegid, geteuid};

    use super::{
        ALTERNATES_NAME, FILE_MODE, GENERATION_MODE, INFO_NAME, ImmutableGitObjectPoolLocation,
        ImmutableGitObjectPoolObservationErrorKind, MARKER_NAME, OBJECTS_NAME, PARENT_MODE,
        observe_with_owner,
    };
    use crate::immutable_git_object_pool::{
        GitObjectFormat, GitObjectPoolBinding, GitObjectPoolGeneration, GitObjectPoolId,
        GitObjectPoolProducerGenerationId, GitObjectPoolTrustGenerationId,
    };
    use crate::immutable_git_object_pool_marker::{
        GitObjectPoolMarkerNonce, ImmutableGitObjectPoolMarker,
    };
    use crate::project_catalog::ProjectIdentity;
    use crate::project_disk_lease::{ProjectDiskGeneration, ProjectDiskId};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

    fn binding(generation: u64) -> GitObjectPoolBinding {
        GitObjectPoolBinding::new(
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            GitObjectPoolId::parse("pool-a").unwrap(),
            GitObjectPoolGeneration::new(generation).unwrap(),
            ProjectDiskId::parse("disk-a").unwrap(),
            ProjectDiskGeneration::new(3).unwrap(),
            GitObjectFormat::Sha1,
            GitObjectPoolProducerGenerationId::parse("producer-a").unwrap(),
            GitObjectPoolTrustGenerationId::parse("trust-a").unwrap(),
        )
    }

    struct Fixture {
        parent: PathBuf,
        pool: PathBuf,
        uid: u32,
        gid: u32,
    }

    impl Fixture {
        fn new(pool_binding: &GitObjectPoolBinding) -> Self {
            let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let base = std::env::temp_dir().join(format!(
                "smolrunner-git-pool-observe-{}-{id}",
                std::process::id()
            ));
            let parent = base.join("generations");
            let pool = parent.join("generation-a");
            let objects = pool.join(OBJECTS_NAME);
            let info = objects.join(INFO_NAME);
            fs::create_dir_all(&info).unwrap();
            let uid = geteuid().as_raw();
            let gid = getegid().as_raw();
            fs::set_permissions(&parent, fs::Permissions::from_mode(PARENT_MODE)).unwrap();
            fs::set_permissions(&pool, fs::Permissions::from_mode(GENERATION_MODE)).unwrap();
            fs::set_permissions(&objects, fs::Permissions::from_mode(GENERATION_MODE)).unwrap();
            fs::set_permissions(&info, fs::Permissions::from_mode(GENERATION_MODE)).unwrap();
            let marker = ImmutableGitObjectPoolMarker::new(
                pool_binding,
                GitObjectPoolMarkerNonce::new([7; 16]).unwrap(),
            )
            .unwrap()
            .encode()
            .unwrap();
            let marker_path = pool.join(MARKER_NAME);
            fs::set_permissions(&pool, fs::Permissions::from_mode(0o755)).unwrap();
            fs::write(&marker_path, marker).unwrap();
            fs::set_permissions(&marker_path, fs::Permissions::from_mode(FILE_MODE)).unwrap();
            fs::set_permissions(&pool, fs::Permissions::from_mode(GENERATION_MODE)).unwrap();
            Self {
                parent,
                pool,
                uid,
                gid,
            }
        }

        fn observe(
            &self,
            pool_binding: &GitObjectPoolBinding,
        ) -> Result<
            super::ImmutableGitObjectPoolObservation,
            super::ImmutableGitObjectPoolObservationError,
        > {
            observe_with_owner(
                pool_binding,
                ImmutableGitObjectPoolLocation::new(self.pool.clone()).unwrap(),
                self.uid,
                self.gid,
            )
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let base = self.parent.parent().unwrap();
            let _ = fs::set_permissions(&self.parent, fs::Permissions::from_mode(0o755));
            let _ = fs::set_permissions(&self.pool, fs::Permissions::from_mode(0o755));
            let _ = fs::remove_dir_all(base);
        }
    }

    #[test]
    fn observes_and_reconfirms_exact_frozen_generation() {
        let binding = binding(1);
        let fixture = Fixture::new(&binding);
        let observation = fixture.observe(&binding).unwrap();
        assert_eq!(observation.summary().pool_generation(), 1);
        assert_eq!(observation.summary().object_format(), GitObjectFormat::Sha1);
        assert!(observation.summary().marker_matched());
        assert!(observation.summary().objects_directory_bound());
        assert!(observation.summary().nested_alternates_absent());
        observation.confirm().unwrap();
        let debug = format!("{observation:?}");
        assert!(!debug.contains(fixture.pool.to_str().unwrap()));
    }

    #[test]
    fn marker_for_other_generation_is_refused() {
        let expected = binding(1);
        let fixture = Fixture::new(&binding(2));
        assert_eq!(
            fixture.observe(&expected).unwrap_err().kind(),
            ImmutableGitObjectPoolObservationErrorKind::MarkerMismatch
        );
    }

    #[test]
    fn writable_or_aliased_generation_is_refused() {
        let binding = binding(1);
        let fixture = Fixture::new(&binding);
        fs::set_permissions(&fixture.pool, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            fixture.observe(&binding).unwrap_err().kind(),
            ImmutableGitObjectPoolObservationErrorKind::UnsafeFilesystem
        );

        fs::set_permissions(&fixture.pool, fs::Permissions::from_mode(GENERATION_MODE)).unwrap();
        let moved = fixture.parent.join("real-generation");
        fs::rename(&fixture.pool, &moved).unwrap();
        symlink(&moved, &fixture.pool).unwrap();
        assert_eq!(
            fixture.observe(&binding).unwrap_err().kind(),
            ImmutableGitObjectPoolObservationErrorKind::UnsafeFilesystem
        );
    }

    #[test]
    fn nested_alternate_is_refused() {
        let binding = binding(1);
        let fixture = Fixture::new(&binding);
        let info = fixture.pool.join(OBJECTS_NAME).join(INFO_NAME);
        fs::set_permissions(&info, fs::Permissions::from_mode(0o755)).unwrap();
        let alternate = info.join(ALTERNATES_NAME);
        fs::write(&alternate, b"/foreign/objects\n").unwrap();
        fs::set_permissions(&alternate, fs::Permissions::from_mode(FILE_MODE)).unwrap();
        fs::set_permissions(&info, fs::Permissions::from_mode(GENERATION_MODE)).unwrap();
        assert_eq!(
            fixture.observe(&binding).unwrap_err().kind(),
            ImmutableGitObjectPoolObservationErrorKind::NestedAlternate
        );
    }

    #[test]
    fn same_name_replacement_is_detected_after_observation() {
        let binding = binding(1);
        let fixture = Fixture::new(&binding);
        let observation = fixture.observe(&binding).unwrap();
        let moved = fixture.parent.join("generation-old");
        fs::rename(&fixture.pool, &moved).unwrap();
        fs::create_dir(&fixture.pool).unwrap();
        fs::set_permissions(&fixture.pool, fs::Permissions::from_mode(GENERATION_MODE)).unwrap();
        assert_eq!(
            observation.confirm().unwrap_err().kind(),
            ImmutableGitObjectPoolObservationErrorKind::Changed
        );
    }

    #[test]
    fn absent_objects_info_is_accepted_when_generation_is_already_frozen() {
        let binding = binding(1);
        let fixture = Fixture::new(&binding);
        let objects = fixture.pool.join(OBJECTS_NAME);
        let info = objects.join(INFO_NAME);
        fs::set_permissions(&objects, fs::Permissions::from_mode(0o755)).unwrap();
        fs::remove_dir(&info).unwrap();
        fs::set_permissions(&objects, fs::Permissions::from_mode(GENERATION_MODE)).unwrap();
        fixture.observe(&binding).unwrap().confirm().unwrap();
    }

    #[test]
    fn retained_objects_entry_rebind_detects_same_name_replacement() {
        let binding = binding(1);
        let fixture = Fixture::new(&binding);
        let observation = fixture.observe(&binding).unwrap();
        let objects = fixture.pool.join(OBJECTS_NAME);
        let old_objects = fixture.pool.join("objects-old");
        fs::set_permissions(&fixture.pool, fs::Permissions::from_mode(0o755)).unwrap();
        fs::rename(&objects, &old_objects).unwrap();
        fs::create_dir(&objects).unwrap();
        fs::set_permissions(&objects, fs::Permissions::from_mode(GENERATION_MODE)).unwrap();
        fs::set_permissions(&fixture.pool, fs::Permissions::from_mode(GENERATION_MODE)).unwrap();
        assert_eq!(
            super::require_current_directory_entry(
                &observation.root,
                std::ffi::OsStr::new(OBJECTS_NAME),
                observation.expected_owner_uid,
                observation.expected_owner_gid,
                GENERATION_MODE,
                &observation.objects_snapshot,
            )
            .unwrap_err()
            .kind(),
            ImmutableGitObjectPoolObservationErrorKind::Changed
        );
    }

    #[test]
    fn objects_info_change_after_observation_invalidates_confirmation() {
        let binding = binding(1);
        let fixture = Fixture::new(&binding);
        let observation = fixture.observe(&binding).unwrap();
        let objects = fixture.pool.join(OBJECTS_NAME);
        let info = objects.join(INFO_NAME);
        fs::set_permissions(&objects, fs::Permissions::from_mode(0o755)).unwrap();
        fs::remove_dir(&info).unwrap();
        fs::set_permissions(&objects, fs::Permissions::from_mode(GENERATION_MODE)).unwrap();
        assert_eq!(
            observation.confirm().unwrap_err().kind(),
            ImmutableGitObjectPoolObservationErrorKind::Changed
        );
    }

    #[test]
    fn location_debug_never_exposes_private_path() {
        let binding = binding(1);
        let fixture = Fixture::new(&binding);
        let location = ImmutableGitObjectPoolLocation::new(fixture.pool.clone()).unwrap();
        let debug = format!("{location:?}");
        assert!(!debug.contains(fixture.pool.to_str().unwrap()));
        assert!(debug.contains("private immutable Git object-pool location"));
    }
}
