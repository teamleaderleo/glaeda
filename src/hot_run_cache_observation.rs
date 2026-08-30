//! Bounded filesystem-metadata observation of one explicitly supplied hot-run cache root.
//!
//! The observer follows no symlinks, reads no file contents, emits no paths, and performs no
//! mutation. A matching opaque name does not establish ownership: every produced lifecycle fact
//! remains unknown and the downstream classifier therefore cannot authorize reclamation.

use std::collections::BTreeMap;
use std::fmt;
use std::os::fd::{AsFd as _, OwnedFd};
use std::path::Path;

use rustix::fs::{self as rustix_fs, AtFlags, Dir, FileType, Mode, OFlags, Stat};

use crate::cache_inventory::{CacheInventoryDocument, CacheStateId, MAX_CACHE_INVENTORY_STATES};

pub const MAX_HOT_RUN_CACHE_OBSERVATION_ENTRIES: u64 = 2_000_000;
pub const MAX_HOT_RUN_CACHE_OBSERVATION_DEPTH: u16 = 64;

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NOATIME)
    .union(OFlags::CLOEXEC);
const ALLOCATION_BLOCK_BYTES: u64 = 512;

/// Observe state IDs and per-state byte totals below one explicit hot-run cache root.
///
/// # Errors
///
/// Refuses missing or unreadable roots, non-directory top-level entries, non-hex state names,
/// followed or cross-filesystem directories, cross-state hardlinks, drift, limit excess, and
/// arithmetic overflow. Errors never contain the supplied path or child names.
pub fn observe_hot_run_cache(
    root: &Path,
) -> Result<CacheInventoryDocument, HotRunCacheObservationError> {
    let root = BoundDirectory::open_root(root)?;
    let root_before = root.snapshot;
    let mut names = state_names(&root.fd)?;
    names.sort_unstable_by(|left, right| left.as_bytes().cmp(right.as_bytes()));

    let mut global_objects = BTreeMap::new();
    let mut entries_seen = 0_u64;
    let mut states = Vec::with_capacity(names.len());
    for (state_ordinal, name) in names.into_iter().enumerate() {
        let path_stat =
            rustix_fs::statat(root.fd.as_fd(), name.as_c_str(), AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|_| unreadable())?;
        if !FileType::from_raw_mode(path_stat.st_mode).is_dir()
            || path_stat.st_dev != root.snapshot.device
        {
            return Err(unsafe_shape());
        }
        let state = BoundDirectory::open_child(&root.fd, name.as_c_str())?;
        if !same_directory_identity(&path_stat, &state.snapshot) {
            return Err(changed());
        }
        let mut totals = TreeTotals::default();
        observe_directory(
            &state,
            root.snapshot.device,
            0,
            state_ordinal,
            &mut entries_seen,
            &mut totals,
            &mut global_objects,
        )?;
        state.revalidate()?;
        let rebound = BoundDirectory::open_child(&root.fd, name.as_c_str())?;
        if rebound.snapshot != state.snapshot {
            return Err(changed());
        }
        let state_id =
            CacheStateId::parse(std::str::from_utf8(name.as_bytes()).map_err(|_| unsafe_shape())?)
                .map_err(|_| unsafe_shape())?;
        states.push((state_id, totals.logical_bytes, totals.allocated_bytes));
    }

    root.revalidate()?;
    if root.snapshot != root_before {
        return Err(changed());
    }
    CacheInventoryDocument::from_unknown_hot_run_states(states).map_err(|_| unsafe_shape())
}

fn state_names(root: &OwnedFd) -> Result<Vec<std::ffi::CString>, HotRunCacheObservationError> {
    let mut directory = Dir::read_from(root).map_err(|_| unreadable())?;
    let mut names = Vec::new();
    for entry in &mut directory {
        let entry = entry.map_err(|_| unreadable())?;
        let name = entry.file_name();
        let bytes = name.to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        if bytes.len() != 64
            || bytes
                .iter()
                .any(|byte| !matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(unsafe_shape());
        }
        if names.len() >= MAX_CACHE_INVENTORY_STATES {
            return Err(too_large());
        }
        names.push(name.to_owned());
    }
    Ok(names)
}

fn observe_directory(
    directory: &BoundDirectory,
    root_device: u64,
    depth: u16,
    state_ordinal: usize,
    entries_seen: &mut u64,
    totals: &mut TreeTotals,
    global_objects: &mut BTreeMap<PhysicalObjectIdentity, usize>,
) -> Result<(), HotRunCacheObservationError> {
    if depth > MAX_HOT_RUN_CACHE_OBSERVATION_DEPTH {
        return Err(too_large());
    }
    totals.add_directory(directory.snapshot.blocks)?;
    *entries_seen = entries_seen.checked_add(1).ok_or_else(too_large)?;
    if *entries_seen > MAX_HOT_RUN_CACHE_OBSERVATION_ENTRIES {
        return Err(too_large());
    }

    let before = directory.snapshot;
    let mut entries = Dir::read_from(&directory.fd).map_err(|_| unreadable())?;
    for entry in &mut entries {
        let entry = entry.map_err(|_| unreadable())?;
        let name = entry.file_name();
        let bytes = name.to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        let first = rustix_fs::statat(directory.fd.as_fd(), name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| unreadable())?;
        if first.st_dev != root_device {
            return Err(unsafe_shape());
        }
        if FileType::from_raw_mode(first.st_mode).is_dir() {
            let child = BoundDirectory::open_child(&directory.fd, name)?;
            if !same_directory_identity(&first, &child.snapshot) {
                return Err(changed());
            }
            observe_directory(
                &child,
                root_device,
                depth.checked_add(1).ok_or_else(too_large)?,
                state_ordinal,
                entries_seen,
                totals,
                global_objects,
            )?;
            child.revalidate()?;
            let rebound = BoundDirectory::open_child(&directory.fd, name)?;
            if rebound.snapshot != child.snapshot {
                return Err(changed());
            }
            continue;
        }

        let snapshot = ObjectSnapshot::from_stat(&first)?;
        let second = rustix_fs::statat(directory.fd.as_fd(), name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| changed())?;
        if snapshot != ObjectSnapshot::from_stat(&second)? {
            return Err(changed());
        }
        *entries_seen = entries_seen.checked_add(1).ok_or_else(too_large)?;
        if *entries_seen > MAX_HOT_RUN_CACHE_OBSERVATION_ENTRIES {
            return Err(too_large());
        }
        let identity = PhysicalObjectIdentity {
            device: snapshot.device,
            inode: snapshot.inode,
        };
        match global_objects.get(&identity) {
            Some(owner) if *owner == state_ordinal => continue,
            Some(_) => return Err(ambiguous_hardlink()),
            None => {
                global_objects.insert(identity, state_ordinal);
            }
        }
        totals.add_object(snapshot.size, snapshot.blocks)?;
    }
    directory.revalidate_against(before)
}

#[derive(Debug, Default)]
struct TreeTotals {
    logical_bytes: u64,
    allocated_bytes: u64,
}

impl TreeTotals {
    fn add_directory(&mut self, blocks: u64) -> Result<(), HotRunCacheObservationError> {
        self.add_allocated_blocks(blocks)
    }

    fn add_object(&mut self, size: u64, blocks: u64) -> Result<(), HotRunCacheObservationError> {
        self.logical_bytes = self.logical_bytes.checked_add(size).ok_or_else(overflow)?;
        self.add_allocated_blocks(blocks)
    }

    fn add_allocated_blocks(&mut self, blocks: u64) -> Result<(), HotRunCacheObservationError> {
        let allocated = blocks
            .checked_mul(ALLOCATION_BLOCK_BYTES)
            .ok_or_else(overflow)?;
        self.allocated_bytes = self
            .allocated_bytes
            .checked_add(allocated)
            .ok_or_else(overflow)?;
        Ok(())
    }
}

#[derive(Debug)]
struct BoundDirectory {
    fd: OwnedFd,
    snapshot: DirectorySnapshot,
}

impl BoundDirectory {
    fn open_root(path: &Path) -> Result<Self, HotRunCacheObservationError> {
        let fd = rustix_fs::open(path, DIRECTORY_FLAGS, Mode::empty())
            .map_err(|_| root_unavailable())?;
        let snapshot = DirectorySnapshot::from_fd(&fd)?;
        Ok(Self { fd, snapshot })
    }

    fn open_child(
        parent: &OwnedFd,
        name: impl rustix::path::Arg,
    ) -> Result<Self, HotRunCacheObservationError> {
        let fd = rustix_fs::openat(parent.as_fd(), name, DIRECTORY_FLAGS, Mode::empty())
            .map_err(|_| unreadable())?;
        let snapshot = DirectorySnapshot::from_fd(&fd)?;
        Ok(Self { fd, snapshot })
    }

    fn revalidate(&self) -> Result<(), HotRunCacheObservationError> {
        self.revalidate_against(self.snapshot)
    }

    fn revalidate_against(
        &self,
        expected: DirectorySnapshot,
    ) -> Result<(), HotRunCacheObservationError> {
        if DirectorySnapshot::from_fd(&self.fd).map_err(|_| changed())? != expected {
            return Err(changed());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DirectorySnapshot {
    device: u64,
    inode: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    size: u64,
    blocks: u64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}

impl DirectorySnapshot {
    fn from_fd(fd: &OwnedFd) -> Result<Self, HotRunCacheObservationError> {
        Self::from_stat(&rustix_fs::fstat(fd).map_err(|_| unreadable())?)
    }

    fn from_stat(stat: &Stat) -> Result<Self, HotRunCacheObservationError> {
        if !FileType::from_raw_mode(stat.st_mode).is_dir() {
            return Err(unsafe_shape());
        }
        Ok(Self {
            device: stat.st_dev,
            inode: stat.st_ino,
            mode: stat.st_mode,
            uid: stat.st_uid,
            gid: stat.st_gid,
            size: u64::try_from(stat.st_size).map_err(|_| unsafe_shape())?,
            blocks: u64::try_from(stat.st_blocks).map_err(|_| unsafe_shape())?,
            mtime: stat.st_mtime,
            mtime_nsec: i64::try_from(stat.st_mtime_nsec).map_err(|_| unsafe_shape())?,
            ctime: stat.st_ctime,
            ctime_nsec: i64::try_from(stat.st_ctime_nsec).map_err(|_| unsafe_shape())?,
        })
    }
}

fn same_directory_identity(stat: &Stat, snapshot: &DirectorySnapshot) -> bool {
    FileType::from_raw_mode(stat.st_mode).is_dir()
        && stat.st_dev == snapshot.device
        && stat.st_ino == snapshot.inode
        && stat.st_mode == snapshot.mode
        && stat.st_uid == snapshot.uid
        && stat.st_gid == snapshot.gid
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct PhysicalObjectIdentity {
    device: u64,
    inode: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ObjectSnapshot {
    device: u64,
    inode: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    size: u64,
    blocks: u64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}

impl ObjectSnapshot {
    fn from_stat(stat: &Stat) -> Result<Self, HotRunCacheObservationError> {
        Ok(Self {
            device: stat.st_dev,
            inode: stat.st_ino,
            mode: stat.st_mode,
            uid: stat.st_uid,
            gid: stat.st_gid,
            size: u64::try_from(stat.st_size).map_err(|_| unsafe_shape())?,
            blocks: u64::try_from(stat.st_blocks).map_err(|_| unsafe_shape())?,
            mtime: stat.st_mtime,
            mtime_nsec: i64::try_from(stat.st_mtime_nsec).map_err(|_| unsafe_shape())?,
            ctime: stat.st_ctime,
            ctime_nsec: i64::try_from(stat.st_ctime_nsec).map_err(|_| unsafe_shape())?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotRunCacheObservationErrorKind {
    RootUnavailable,
    Unreadable,
    UnsafeShape,
    AmbiguousHardlink,
    Changed,
    TooLarge,
    ArithmeticOverflow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotRunCacheObservationError {
    kind: HotRunCacheObservationErrorKind,
    message: &'static str,
}

impl HotRunCacheObservationError {
    #[must_use]
    pub const fn kind(&self) -> HotRunCacheObservationErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self.kind {
            HotRunCacheObservationErrorKind::RootUnavailable => {
                "hot_run_cache_observation_root_unavailable"
            }
            HotRunCacheObservationErrorKind::Unreadable => "hot_run_cache_observation_unreadable",
            HotRunCacheObservationErrorKind::UnsafeShape => {
                "hot_run_cache_observation_unsafe_shape"
            }
            HotRunCacheObservationErrorKind::AmbiguousHardlink => {
                "hot_run_cache_observation_ambiguous_hardlink"
            }
            HotRunCacheObservationErrorKind::Changed => "hot_run_cache_observation_changed",
            HotRunCacheObservationErrorKind::TooLarge => "hot_run_cache_observation_too_large",
            HotRunCacheObservationErrorKind::ArithmeticOverflow => {
                "hot_run_cache_observation_arithmetic_overflow"
            }
        }
    }
}

impl fmt::Display for HotRunCacheObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for HotRunCacheObservationError {}

const fn error(
    kind: HotRunCacheObservationErrorKind,
    message: &'static str,
) -> HotRunCacheObservationError {
    HotRunCacheObservationError { kind, message }
}

fn root_unavailable() -> HotRunCacheObservationError {
    error(
        HotRunCacheObservationErrorKind::RootUnavailable,
        "hot-run cache observation root is unavailable",
    )
}

fn unreadable() -> HotRunCacheObservationError {
    error(
        HotRunCacheObservationErrorKind::Unreadable,
        "hot-run cache observation could not read the reviewed tree",
    )
}

fn unsafe_shape() -> HotRunCacheObservationError {
    error(
        HotRunCacheObservationErrorKind::UnsafeShape,
        "hot-run cache observation found an unsupported filesystem shape",
    )
}

fn ambiguous_hardlink() -> HotRunCacheObservationError {
    error(
        HotRunCacheObservationErrorKind::AmbiguousHardlink,
        "hot-run cache observation found cross-state physical attribution",
    )
}

fn changed() -> HotRunCacheObservationError {
    error(
        HotRunCacheObservationErrorKind::Changed,
        "hot-run cache observation changed during traversal",
    )
}

fn too_large() -> HotRunCacheObservationError {
    error(
        HotRunCacheObservationErrorKind::TooLarge,
        "hot-run cache observation exceeds the reviewed bound",
    )
}

fn overflow() -> HotRunCacheObservationError {
    error(
        HotRunCacheObservationErrorKind::ArithmeticOverflow,
        "hot-run cache observation byte aggregate overflows",
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{MetadataExt as _, symlink};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::cache_inventory::build_local_hot_run_cache_report;

    use super::{
        HotRunCacheObservationErrorKind, MAX_HOT_RUN_CACHE_OBSERVATION_DEPTH, observe_hot_run_cache,
    };

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new() -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "glaeda-hot-run-cache-observation-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create temporary observation root");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn state(&self, byte: char) -> PathBuf {
            let state = self.0.join(byte.to_string().repeat(64));
            fs::create_dir(&state).expect("create state");
            state
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn bytes(metadata: &fs::Metadata) -> (u64, u64) {
        (metadata.size(), metadata.blocks() * 512)
    }

    #[test]
    fn observes_metadata_once_per_hardlinked_object_and_never_follows_symlinks() {
        let root = TempRoot::new();
        let state = root.state('a');
        let data = state.join("data");
        fs::write(&data, b"abc").expect("write fixture");
        fs::hard_link(&data, state.join("alias")).expect("hardlink fixture");
        let outside_root = TempRoot::new();
        let outside = outside_root.path().join("outside");
        fs::write(&outside, vec![0_u8; 1_000_000]).expect("write outside fixture");
        let link = state.join("link");
        symlink(&outside, &link).expect("symlink fixture");

        let (_, directory_allocated) = bytes(&fs::metadata(&state).unwrap());
        let (data_logical, data_allocated) = bytes(&fs::metadata(&data).unwrap());
        let (link_logical, link_allocated) = bytes(&fs::symlink_metadata(&link).unwrap());
        let document = observe_hot_run_cache(root.path()).expect("observe fixture");
        let report = build_local_hot_run_cache_report(&document).expect("classify fixture");

        assert_eq!(report.summary().state_count(), 1);
        assert_eq!(
            report.summary().logical_bytes(),
            data_logical + link_logical
        );
        assert_eq!(
            report.summary().allocated_bytes(),
            directory_allocated + data_allocated + link_allocated
        );
        assert_eq!(report.states()[0].classification().as_str(), "unknown");
    }

    #[test]
    fn rejects_names_and_top_level_entries_that_cannot_be_state_ids() {
        let root = TempRoot::new();
        fs::write(root.path().join("not-a-state"), b"x").expect("write invalid entry");
        let error = observe_hot_run_cache(root.path()).expect_err("reject invalid entry");
        assert_eq!(error.kind(), HotRunCacheObservationErrorKind::UnsafeShape);
        assert!(
            !error
                .to_string()
                .contains(root.path().to_string_lossy().as_ref())
        );
    }

    #[test]
    fn rejects_physical_objects_shared_between_states() {
        let root = TempRoot::new();
        let first = root.state('a');
        let second = root.state('b');
        fs::write(first.join("data"), b"x").expect("write fixture");
        fs::hard_link(first.join("data"), second.join("data")).expect("cross-state hardlink");
        let error = observe_hot_run_cache(root.path()).expect_err("reject cross-state hardlink");
        assert_eq!(
            error.kind(),
            HotRunCacheObservationErrorKind::AmbiguousHardlink
        );
    }

    #[test]
    fn rejects_depth_beyond_the_reviewed_bound() {
        let root = TempRoot::new();
        let mut directory = root.state('a');
        for index in 0..=MAX_HOT_RUN_CACHE_OBSERVATION_DEPTH {
            directory = directory.join(format!("d{index}"));
            fs::create_dir(&directory).expect("create nested directory");
        }
        let error = observe_hot_run_cache(root.path()).expect_err("reject deep tree");
        assert_eq!(error.kind(), HotRunCacheObservationErrorKind::TooLarge);
    }
}
