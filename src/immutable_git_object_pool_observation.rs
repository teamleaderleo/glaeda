#![cfg(target_os = "linux")]

use std::fmt;
use std::fs::{self, File};
use std::os::fd::{AsFd as _, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{FileExt as _, MetadataExt as _};
use std::path::{Component, Path, PathBuf};

use rustix::fs::{self as rustix_fs, FileType, Mode, OFlags};
use rustix::io::Errno;
use rustix::process::geteuid;
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::immutable_git_object_pool::{GitObjectFormat, GitObjectPoolBinding};
use crate::immutable_git_object_pool_marker::{
    IMMUTABLE_GIT_OBJECT_POOL_MARKER_BYTES, ImmutableGitObjectPoolMarker,
    git_object_pool_binding_digest,
};

pub const IMMUTABLE_GIT_OBJECT_POOL_OBSERVATION_SCHEMA_VERSION: u8 = 1;
const MAX_PRIVATE_PATH_BYTES: usize = 1_024;
const MAX_INVENTORY_ENTRIES: u64 = 2_000_000;
const MAX_INVENTORY_DEPTH: usize = 64;
const MARKER_NAME: &str = ".smolrunner-object-pool-generation";
const OBJECTS_NAME: &str = "objects";
const INFO_NAME: &str = "info";
const ALTERNATES_NAME: &str = "alternates";
const DIRECTORY_MODE: u32 = 0o555;
const FILE_MODE: u32 = 0o444;
const DIRECTORY_FLAGS: OFlags = OFlags::PATH
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NONBLOCK);
const INVENTORY_DOMAIN: &[u8] = b"smolrunner-immutable-git-object-pool-inventory-v1\0";
const SHA256_PREFIX: &str = "sha256:";
const HEX: &[u8; 16] = b"0123456789abcdef";

#[derive(Clone, PartialEq, Eq)]
pub struct ImmutableGitObjectPoolPath(PathBuf);

impl ImmutableGitObjectPoolPath {
    /// Parse one private absolute pool-generation path.
    ///
    /// # Errors
    ///
    /// Returns a bounded path-private error unless the value is normalized absolute UTF-8, bounded,
    /// non-root, and contains only ordinary components beneath `/`.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, ImmutableGitObjectPoolObservationError> {
        let path = path.into();
        validate_absolute_path(&path)?;
        Ok(Self(path))
    }
}

impl fmt::Debug for ImmutableGitObjectPoolPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<private immutable Git object-pool path>")
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
    entry_count: u64,
    regular_file_count: u64,
    logical_bytes: u64,
    allocated_bytes: u64,
    inventory_digest: Sha256Digest,
    marker_matched: bool,
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
    pub const fn entry_count(&self) -> u64 {
        self.entry_count
    }

    #[must_use]
    pub const fn regular_file_count(&self) -> u64 {
        self.regular_file_count
    }

    #[must_use]
    pub const fn logical_bytes(&self) -> u64 {
        self.logical_bytes
    }

    #[must_use]
    pub const fn allocated_bytes(&self) -> u64 {
        self.allocated_bytes
    }

    #[must_use]
    pub const fn inventory_digest(&self) -> &Sha256Digest {
        &self.inventory_digest
    }
}

pub struct ImmutableGitObjectPoolObservation {
    summary: ImmutableGitObjectPoolObservationSummary,
    binding_digest: Sha256Digest,
    pool_path: PathBuf,
    root: OwnedFd,
    objects: OwnedFd,
    root_snapshot: DirectorySnapshot,
    objects_snapshot: DirectorySnapshot,
}

impl ImmutableGitObjectPoolObservation {
    #[must_use]
    pub const fn summary(&self) -> &ImmutableGitObjectPoolObservationSummary {
        &self.summary
    }

    /// Reconfirm this exact frozen generation against the logical binding and current filesystem.
    ///
    /// This remains read-only. The inventory is intentionally rescanned because confirmation is a
    /// generation boundary, not a per-command hot-path operation.
    ///
    /// # Errors
    ///
    /// Returns a bounded path-private error for binding/marker mismatch, path or descriptor drift,
    /// writable/aliased inventory, nested alternates, or changed storage accounting/fingerprint.
    pub fn confirm(
        &self,
        binding: &GitObjectPoolBinding,
    ) -> Result<(), ImmutableGitObjectPoolObservationError> {
        if git_object_pool_binding_digest(binding).map_err(|_| marker_error())?
            != self.binding_digest
        {
            return Err(binding_mismatch());
        }
        require_directory_fd(&self.root, 0, DIRECTORY_MODE, &self.root_snapshot)?;
        require_directory_fd(
            &self.objects,
            0,
            DIRECTORY_MODE,
            &self.objects_snapshot,
        )?;
        let reopened = open_absolute_pool(&self.pool_path, 0)?;
        let reopened_snapshot = snapshot_directory_fd(&reopened)?;
        if reopened_snapshot != self.root_snapshot {
            return Err(changed());
        }
        verify_marker(&reopened, binding, 0)?;
        require_no_nested_alternates(&reopened, 0)?;
        let inventory = scan_stable_inventory(&self.pool_path, 0)?;
        if inventory.entry_count != self.summary.entry_count
            || inventory.regular_file_count != self.summary.regular_file_count
            || inventory.logical_bytes != self.summary.logical_bytes
            || inventory.allocated_bytes != self.summary.allocated_bytes
            || inventory.digest != self.summary.inventory_digest
        {
            return Err(changed());
        }
        Ok(())
    }
}

impl fmt::Debug for ImmutableGitObjectPoolObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let _ = (&self.pool_path, &self.root, &self.objects);
        formatter
            .debug_struct("ImmutableGitObjectPoolObservation")
            .field("summary", &self.summary)
            .field("binding_digest", &self.binding_digest)
            .field("private_filesystem", &"<private exact pool descriptors>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImmutableGitObjectPoolObservationErrorKind {
    InvalidPath,
    PermissionDenied,
    UnsafeFilesystem,
    MarkerInvalid,
    BindingMismatch,
    NestedAlternate,
    InventoryLimit,
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

/// Observe one exact immutable Git object-pool generation without mutation or a child process.
///
/// Public observation requires root because the accepted active generation is root-owned and the
/// later one-shot guest-control transaction is the trusted Linux-side observer. Unit tests use a
/// relocated same-UID root while exercising the same frozen inventory rules.
///
/// # Errors
///
/// Returns a bounded path-private error unless the pool marker matches the exact #583 binding, the
/// pool and every inventory entry are frozen under trusted ownership, regular files are single-link,
/// no symlink/special entry or nested object alternate exists, and two complete scans agree.
pub fn observe_immutable_git_object_pool(
    binding: &GitObjectPoolBinding,
    path: &ImmutableGitObjectPoolPath,
) -> Result<ImmutableGitObjectPoolObservation, ImmutableGitObjectPoolObservationError> {
    if geteuid().as_raw() != 0 {
        return Err(permission_denied());
    }
    observe_absolute(binding, &path.0, 0)
}

fn observe_absolute(
    binding: &GitObjectPoolBinding,
    path: &Path,
    expected_owner_uid: u32,
) -> Result<ImmutableGitObjectPoolObservation, ImmutableGitObjectPoolObservationError> {
    let root = open_absolute_pool(path, expected_owner_uid)?;
    observe_opened(binding, path, expected_owner_uid, root)
}

fn observe_relative_for_test(
    binding: &GitObjectPoolBinding,
    trusted_base: &Path,
    relative_pool: &Path,
    expected_owner_uid: u32,
) -> Result<ImmutableGitObjectPoolObservation, ImmutableGitObjectPoolObservationError> {
    let base = rustix_fs::open(trusted_base, DIRECTORY_FLAGS, Mode::empty()).map_err(|_| io())?;
    let base_snapshot = snapshot_directory_fd(&base)?;
    if base_snapshot.uid != expected_owner_uid || base_snapshot.mode & 0o022 != 0 {
        return Err(unsafe_filesystem());
    }
    let root = open_relative_pool(&base, relative_pool, expected_owner_uid)?;
    let absolute = trusted_base.join(relative_pool);
    observe_opened(binding, &absolute, expected_owner_uid, root)
}

fn observe_opened(
    binding: &GitObjectPoolBinding,
    path: &Path,
    expected_owner_uid: u32,
    root: OwnedFd,
) -> Result<ImmutableGitObjectPoolObservation, ImmutableGitObjectPoolObservationError> {
    let root_snapshot = snapshot_directory_fd(&root)?;
    require_frozen_directory(&root_snapshot, expected_owner_uid, DIRECTORY_MODE)?;
    verify_marker(&root, binding, expected_owner_uid)?;

    let objects = rustix_fs::openat(root.as_fd(), OBJECTS_NAME, DIRECTORY_FLAGS, Mode::empty())
        .map_err(map_open)?;
    let objects_snapshot = snapshot_directory_fd(&objects)?;
    require_frozen_directory(&objects_snapshot, expected_owner_uid, DIRECTORY_MODE)?;
    require_no_nested_alternates(&root, expected_owner_uid)?;

    let inventory = scan_stable_inventory(path, expected_owner_uid)?;
    let binding_digest = git_object_pool_binding_digest(binding).map_err(|_| marker_error())?;
    Ok(ImmutableGitObjectPoolObservation {
        summary: ImmutableGitObjectPoolObservationSummary {
            schema_version: IMMUTABLE_GIT_OBJECT_POOL_OBSERVATION_SCHEMA_VERSION,
            disposition: ImmutableGitObjectPoolObservationDisposition::RootOwnedFrozenGeneration,
            pool_generation: binding.generation().get(),
            object_format: binding.object_format(),
            entry_count: inventory.entry_count,
            regular_file_count: inventory.regular_file_count,
            logical_bytes: inventory.logical_bytes,
            allocated_bytes: inventory.allocated_bytes,
            inventory_digest: inventory.digest,
            marker_matched: true,
        },
        binding_digest,
        pool_path: path.to_path_buf(),
        root,
        objects,
        root_snapshot,
        objects_snapshot,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct InventoryObservation {
    entry_count: u64,
    regular_file_count: u64,
    logical_bytes: u64,
    allocated_bytes: u64,
    digest: Sha256Digest,
}

fn open_absolute_pool(
    path: &Path,
    expected_owner_uid: u32,
) -> Result<OwnedFd, ImmutableGitObjectPoolObservationError> {
    validate_absolute_path(path)?;
    let mut current = rustix_fs::open(Path::new("/"), DIRECTORY_FLAGS, Mode::empty())
        .map_err(|_| io())?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                current = rustix_fs::openat(current.as_fd(), name, DIRECTORY_FLAGS, Mode::empty())
                    .map_err(map_open)?;
                let snapshot = snapshot_directory_fd(&current)?;
                if snapshot.uid != expected_owner_uid || snapshot.mode & 0o022 != 0 {
                    return Err(unsafe_filesystem());
                }
            }
            _ => return Err(invalid_path()),
        }
    }
    Ok(current)
}

fn open_relative_pool(
    base: &OwnedFd,
    path: &Path,
    expected_owner_uid: u32,
) -> Result<OwnedFd, ImmutableGitObjectPoolObservationError> {
    let mut current = rustix_fs::fcntl_dupfd_cloexec(base, 0).map_err(|_| io())?;
    for component in path.components() {
        let Component::Normal(name) = component else {
            return Err(invalid_path());
        };
        current = rustix_fs::openat(current.as_fd(), name, DIRECTORY_FLAGS, Mode::empty())
            .map_err(map_open)?;
        let snapshot = snapshot_directory_fd(&current)?;
        if snapshot.uid != expected_owner_uid || snapshot.mode & 0o022 != 0 {
            return Err(unsafe_filesystem());
        }
    }
    Ok(current)
}

fn snapshot_directory_fd(
    fd: &OwnedFd,
) -> Result<DirectorySnapshot, ImmutableGitObjectPoolObservationError> {
    let stat = rustix_fs::fstat(fd).map_err(|_| io())?;
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

fn require_directory_fd(
    fd: &OwnedFd,
    expected_owner_uid: u32,
    expected_mode: u32,
    expected: &DirectorySnapshot,
) -> Result<(), ImmutableGitObjectPoolObservationError> {
    let current = snapshot_directory_fd(fd)?;
    require_frozen_directory(&current, expected_owner_uid, expected_mode)?;
    if &current != expected {
        return Err(changed());
    }
    Ok(())
}

fn require_frozen_directory(
    snapshot: &DirectorySnapshot,
    expected_owner_uid: u32,
    expected_mode: u32,
) -> Result<(), ImmutableGitObjectPoolObservationError> {
    if snapshot.uid != expected_owner_uid || snapshot.mode & 0o7777 != expected_mode {
        return Err(unsafe_filesystem());
    }
    Ok(())
}

fn verify_marker(
    root: &OwnedFd,
    binding: &GitObjectPoolBinding,
    expected_owner_uid: u32,
) -> Result<(), ImmutableGitObjectPoolObservationError> {
    let fd = rustix_fs::openat(root.as_fd(), MARKER_NAME, FILE_FLAGS, Mode::empty()).map_err(map_open)?;
    let file = File::from(fd);
    let before = file.metadata().map_err(|_| io())?;
    if !before.file_type().is_file()
        || before.uid() != expected_owner_uid
        || before.mode() & 0o7777 != FILE_MODE
        || before.nlink() != 1
        || before.len() != IMMUTABLE_GIT_OBJECT_POOL_MARKER_BYTES as u64
    {
        return Err(marker_error());
    }
    let mut bytes = [0_u8; IMMUTABLE_GIT_OBJECT_POOL_MARKER_BYTES];
    file.read_exact_at(&mut bytes, 0).map_err(|_| io())?;
    ImmutableGitObjectPoolMarker::decode_and_verify(&bytes, binding).map_err(|_| marker_error())?;
    let after = file.metadata().map_err(|_| io())?;
    if metadata_identity(&before) != metadata_identity(&after) {
        return Err(changed());
    }
    Ok(())
}

fn require_no_nested_alternates(
    root: &OwnedFd,
    expected_owner_uid: u32,
) -> Result<(), ImmutableGitObjectPoolObservationError> {
    let objects = rustix_fs::openat(root.as_fd(), OBJECTS_NAME, DIRECTORY_FLAGS, Mode::empty())
        .map_err(map_open)?;
    let info = match rustix_fs::openat(objects.as_fd(), INFO_NAME, DIRECTORY_FLAGS, Mode::empty()) {
        Ok(info) => info,
        Err(Errno::NOENT) => return Ok(()),
        Err(error) => return Err(map_open(error)),
    };
    let snapshot = snapshot_directory_fd(&info)?;
    require_frozen_directory(&snapshot, expected_owner_uid, DIRECTORY_MODE)?;
    match rustix_fs::openat(info.as_fd(), ALTERNATES_NAME, FILE_FLAGS, Mode::empty()) {
        Ok(_) => Err(nested_alternate()),
        Err(Errno::NOENT) => Ok(()),
        Err(error) => Err(map_open(error)),
    }
}

fn scan_stable_inventory(
    pool: &Path,
    expected_owner_uid: u32,
) -> Result<InventoryObservation, ImmutableGitObjectPoolObservationError> {
    let first = scan_inventory(pool, expected_owner_uid)?;
    let second = scan_inventory(pool, expected_owner_uid)?;
    if first != second {
        return Err(changed());
    }
    Ok(first)
}

fn scan_inventory(
    pool: &Path,
    expected_owner_uid: u32,
) -> Result<InventoryObservation, ImmutableGitObjectPoolObservationError> {
    let mut state = InventoryState {
        hasher: Sha256::new(),
        entry_count: 0,
        regular_file_count: 0,
        logical_bytes: 0,
        allocated_bytes: 0,
    };
    state.hasher.update(INVENTORY_DOMAIN);
    scan_entry(pool, pool, expected_owner_uid, 0, &mut state)?;
    Ok(InventoryObservation {
        entry_count: state.entry_count,
        regular_file_count: state.regular_file_count,
        logical_bytes: state.logical_bytes,
        allocated_bytes: state.allocated_bytes,
        digest: digest_to_sha256(state.hasher.finalize().as_slice())?,
    })
}

struct InventoryState {
    hasher: Sha256,
    entry_count: u64,
    regular_file_count: u64,
    logical_bytes: u64,
    allocated_bytes: u64,
}

fn scan_entry(
    pool: &Path,
    path: &Path,
    expected_owner_uid: u32,
    depth: usize,
    state: &mut InventoryState,
) -> Result<(), ImmutableGitObjectPoolObservationError> {
    if depth > MAX_INVENTORY_DEPTH || state.entry_count >= MAX_INVENTORY_ENTRIES {
        return Err(inventory_limit());
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| io())?;
    if metadata.uid() != expected_owner_uid {
        return Err(unsafe_filesystem());
    }
    let file_type = metadata.file_type();
    let kind = if file_type.is_dir() {
        if metadata.mode() & 0o7777 != DIRECTORY_MODE {
            return Err(unsafe_filesystem());
        }
        1_u8
    } else if file_type.is_file() {
        if metadata.mode() & 0o7777 != FILE_MODE || metadata.nlink() != 1 {
            return Err(unsafe_filesystem());
        }
        state.regular_file_count = state
            .regular_file_count
            .checked_add(1)
            .ok_or_else(inventory_limit)?;
        state.logical_bytes = state
            .logical_bytes
            .checked_add(metadata.len())
            .ok_or_else(inventory_limit)?;
        2_u8
    } else {
        return Err(unsafe_filesystem());
    };

    state.entry_count = state
        .entry_count
        .checked_add(1)
        .ok_or_else(inventory_limit)?;
    state.allocated_bytes = state
        .allocated_bytes
        .checked_add(metadata.blocks().checked_mul(512).ok_or_else(inventory_limit)?)
        .ok_or_else(inventory_limit)?;

    let relative = path.strip_prefix(pool).map_err(|_| invalid_path())?;
    let relative_bytes = relative.as_os_str().as_bytes();
    let relative_len = u32::try_from(relative_bytes.len()).map_err(|_| inventory_limit())?;
    state.hasher.update(relative_len.to_be_bytes());
    state.hasher.update(relative_bytes);
    state.hasher.update([kind]);
    state.hasher.update(metadata.dev().to_be_bytes());
    state.hasher.update(metadata.ino().to_be_bytes());
    state.hasher.update(metadata.uid().to_be_bytes());
    state.hasher.update(metadata.gid().to_be_bytes());
    state.hasher.update(metadata.mode().to_be_bytes());
    state.hasher.update(metadata.nlink().to_be_bytes());
    state.hasher.update(metadata.len().to_be_bytes());
    state.hasher.update(metadata.blocks().to_be_bytes());
    state.hasher.update(metadata.mtime().to_be_bytes());
    state.hasher.update(metadata.mtime_nsec().to_be_bytes());
    state.hasher.update(metadata.ctime().to_be_bytes());
    state.hasher.update(metadata.ctime_nsec().to_be_bytes());

    if file_type.is_dir() {
        let mut children = fs::read_dir(path)
            .map_err(|_| io())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| io())?;
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            scan_entry(pool, &child.path(), expected_owner_uid, depth + 1, state)?;
        }
    }
    Ok(())
}

fn validate_absolute_path(path: &Path) -> Result<(), ImmutableGitObjectPoolObservationError> {
    if !path.is_absolute()
        || path == Path::new("/")
        || path.as_os_str().as_bytes().len() > MAX_PRIVATE_PATH_BYTES
        || path.to_str().is_none()
    {
        return Err(invalid_path());
    }
    let mut normal = 0_usize;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(_) => normal += 1,
            _ => return Err(invalid_path()),
        }
    }
    if normal == 0 {
        return Err(invalid_path());
    }
    Ok(())
}

fn metadata_identity(metadata: &fs::Metadata) -> (u64, u64, u32, u32, u32, u64, i64, i64, i64, i64) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.uid(),
        metadata.gid(),
        metadata.mode(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

fn digest_to_sha256(bytes: &[u8]) -> Result<Sha256Digest, ImmutableGitObjectPoolObservationError> {
    let mut value = String::with_capacity(SHA256_PREFIX.len() + bytes.len() * 2);
    value.push_str(SHA256_PREFIX);
    for byte in bytes {
        value.push(char::from(HEX[usize::from(byte >> 4)]));
        value.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Sha256Digest::parse(&value).map_err(|_| io())
}

fn map_open(error: Errno) -> ImmutableGitObjectPoolObservationError {
    match error {
        Errno::LOOP | Errno::NOTDIR => unsafe_filesystem(),
        Errno::ACCESS | Errno::PERM => permission_denied(),
        _ => io(),
    }
}

const fn error(
    kind: ImmutableGitObjectPoolObservationErrorKind,
    code: &'static str,
    message: &'static str,
) -> ImmutableGitObjectPoolObservationError {
    ImmutableGitObjectPoolObservationError { kind, code, message }
}

const fn invalid_path() -> ImmutableGitObjectPoolObservationError {
    error(
        ImmutableGitObjectPoolObservationErrorKind::InvalidPath,
        "git_object_pool_path_invalid",
        "immutable Git object-pool path is invalid",
    )
}

const fn permission_denied() -> ImmutableGitObjectPoolObservationError {
    error(
        ImmutableGitObjectPoolObservationErrorKind::PermissionDenied,
        "git_object_pool_observation_permission_denied",
        "immutable Git object-pool observation requires trusted authority",
    )
}

const fn unsafe_filesystem() -> ImmutableGitObjectPoolObservationError {
    error(
        ImmutableGitObjectPoolObservationErrorKind::UnsafeFilesystem,
        "git_object_pool_filesystem_unsafe",
        "immutable Git object-pool filesystem evidence is unsafe",
    )
}

const fn marker_error() -> ImmutableGitObjectPoolObservationError {
    error(
        ImmutableGitObjectPoolObservationErrorKind::MarkerInvalid,
        "git_object_pool_marker_unproven",
        "immutable Git object-pool generation marker is unproven",
    )
}

const fn binding_mismatch() -> ImmutableGitObjectPoolObservationError {
    error(
        ImmutableGitObjectPoolObservationErrorKind::BindingMismatch,
        "git_object_pool_binding_mismatch",
        "immutable Git object-pool binding does not match the observation",
    )
}

const fn nested_alternate() -> ImmutableGitObjectPoolObservationError {
    error(
        ImmutableGitObjectPoolObservationErrorKind::NestedAlternate,
        "git_object_pool_nested_alternate",
        "immutable Git object-pool generation cannot depend on another object alternate",
    )
}

const fn inventory_limit() -> ImmutableGitObjectPoolObservationError {
    error(
        ImmutableGitObjectPoolObservationErrorKind::InventoryLimit,
        "git_object_pool_inventory_limit",
        "immutable Git object-pool inventory exceeds its bounded observation envelope",
    )
}

const fn changed() -> ImmutableGitObjectPoolObservationError {
    error(
        ImmutableGitObjectPoolObservationErrorKind::ChangedDuringObservation,
        "git_object_pool_changed_during_observation",
        "immutable Git object-pool generation changed during observation",
    )
}

const fn io() -> ImmutableGitObjectPoolObservationError {
    error(
        ImmutableGitObjectPoolObservationErrorKind::Io,
        "git_object_pool_observation_io",
        "immutable Git object-pool evidence could not be read safely",
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use rustix::process::geteuid;

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
        FILE_MODE, ImmutableGitObjectPoolObservationErrorKind, MARKER_NAME, OBJECTS_NAME,
        observe_relative_for_test,
    };

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
        pool: PathBuf,
        owner: u32,
    }

    impl Fixture {
        fn new() -> Self {
            let owner = geteuid().as_raw();
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let base = std::env::temp_dir().join(format!(
                "smolrunner-git-pool-observation-{}-{unique}",
                std::process::id()
            ));
            fs::create_dir(&base).unwrap();
            fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
            let pool = base.join("pool");
            fs::create_dir(&pool).unwrap();
            fs::create_dir(pool.join(OBJECTS_NAME)).unwrap();
            fs::create_dir(pool.join(OBJECTS_NAME).join("info")).unwrap();
            fs::create_dir(pool.join("refs")).unwrap();
            fs::write(pool.join("HEAD"), b"ref: refs/heads/main\n").unwrap();
            fs::write(pool.join(OBJECTS_NAME).join("pack-a"), b"object-bytes").unwrap();
            let marker = ImmutableGitObjectPoolMarker::new(
                &binding(1),
                GitObjectPoolMarkerNonce::new([7; 16]).unwrap(),
            )
            .unwrap()
            .encode()
            .unwrap();
            fs::write(pool.join(MARKER_NAME), marker).unwrap();
            freeze_tree(&pool);
            Self { base, pool, owner }
        }

        fn observe(
            &self,
            expected: &GitObjectPoolBinding,
        ) -> Result<super::ImmutableGitObjectPoolObservation, super::ImmutableGitObjectPoolObservationError>
        {
            observe_relative_for_test(expected, &self.base, Path::new("pool"), self.owner)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            thaw_tree(&self.pool);
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    fn freeze_tree(path: &Path) {
        if path.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                freeze_tree(&entry.unwrap().path());
            }
            fs::set_permissions(path, fs::Permissions::from_mode(0o555)).unwrap();
        } else if path.is_file() {
            fs::set_permissions(path, fs::Permissions::from_mode(FILE_MODE)).unwrap();
        }
    }

    fn thaw_tree(path: &Path) {
        let Ok(metadata) = fs::symlink_metadata(path) else {
            return;
        };
        if metadata.file_type().is_dir() {
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
            if let Ok(entries) = fs::read_dir(path) {
                for entry in entries.flatten() {
                    thaw_tree(&entry.path());
                }
            }
        } else if metadata.file_type().is_file() {
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
        }
    }

    #[test]
    fn observes_and_reconfirms_one_frozen_marker_bound_generation() {
        let fixture = Fixture::new();
        let expected = binding(1);
        let observation = fixture.observe(&expected).unwrap();
        assert_eq!(observation.summary().pool_generation(), 1);
        assert!(observation.summary().marker_matched);
        assert!(observation.summary().entry_count() >= 7);
        assert!(observation.summary().regular_file_count() >= 3);
        assert!(observation.summary().logical_bytes() > 0);
        observation.confirm(&expected).unwrap();
        let debug = format!("{observation:?}");
        assert!(!debug.contains(fixture.base.to_str().unwrap()));
    }

    #[test]
    fn marker_for_another_generation_is_refused() {
        let fixture = Fixture::new();
        assert_eq!(
            fixture.observe(&binding(2)).unwrap_err().kind(),
            ImmutableGitObjectPoolObservationErrorKind::MarkerInvalid
        );
    }

    #[test]
    fn writable_inventory_entry_is_refused() {
        let fixture = Fixture::new();
        let object = fixture.pool.join(OBJECTS_NAME).join("pack-a");
        fs::set_permissions(&object, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            fixture.observe(&binding(1)).unwrap_err().kind(),
            ImmutableGitObjectPoolObservationErrorKind::UnsafeFilesystem
        );
    }

    #[test]
    fn hardlinked_regular_file_is_refused() {
        let fixture = Fixture::new();
        let source = fixture.pool.join(OBJECTS_NAME).join("pack-a");
        let alias = fixture.pool.join(OBJECTS_NAME).join("pack-b");
        fs::hard_link(&source, &alias).unwrap();
        fs::set_permissions(&alias, fs::Permissions::from_mode(FILE_MODE)).unwrap();
        assert!(fs::metadata(&source).unwrap().nlink() > 1);
        assert_eq!(
            fixture.observe(&binding(1)).unwrap_err().kind(),
            ImmutableGitObjectPoolObservationErrorKind::UnsafeFilesystem
        );
    }

    #[test]
    fn symlink_and_nested_alternate_are_refused() {
        let fixture = Fixture::new();
        let symlink_path = fixture.pool.join("symlink");
        symlink("HEAD", &symlink_path).unwrap();
        assert_eq!(
            fixture.observe(&binding(1)).unwrap_err().kind(),
            ImmutableGitObjectPoolObservationErrorKind::UnsafeFilesystem
        );
        fs::remove_file(&symlink_path).unwrap();

        let alternates = fixture.pool.join(OBJECTS_NAME).join("info").join("alternates");
        fs::write(&alternates, b"/other/objects\n").unwrap();
        fs::set_permissions(&alternates, fs::Permissions::from_mode(FILE_MODE)).unwrap();
        assert_eq!(
            fixture.observe(&binding(1)).unwrap_err().kind(),
            ImmutableGitObjectPoolObservationErrorKind::NestedAlternate
        );
    }

    #[test]
    fn confirmation_detects_post_observation_mode_drift() {
        let fixture = Fixture::new();
        let expected = binding(1);
        let observation = fixture.observe(&expected).unwrap();
        fs::set_permissions(&fixture.pool, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            observation.confirm(&expected).unwrap_err().kind(),
            ImmutableGitObjectPoolObservationErrorKind::UnsafeFilesystem
        );
    }
}
