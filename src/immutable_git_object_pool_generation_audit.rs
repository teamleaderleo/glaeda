//! Descriptor-relative publication audit for immutable Git object-pool candidates.
//!
//! This is the intentionally O(N) companion to the hot #585 ownership observer. It is meant to
//! run once while promoting a generation, not on each task admission. It proves safe entry types,
//! single-link candidate files, source/candidate object-inode disjointness, bounded logical bytes,
//! and absence of nested Git alternates. It deliberately does not sum `st_blocks` or claim unique
//! physical allocation on reflink-capable filesystems.
//! The caller must keep the producer process group quiescent and keep the candidate outside
//! producer/task traversal authority for the entire audit and every later privileged publication step.

use std::collections::BTreeSet;
use std::fmt;
use std::os::fd::{AsFd as _, OwnedFd};
use std::path::{Component, Path};

use rustix::fs::{self as rustix_fs, AtFlags, Dir, FileType, Mode, OFlags};
use rustix::io::Errno;
use serde::Serialize;

pub const IMMUTABLE_GIT_OBJECT_POOL_GENERATION_AUDIT_SCHEMA_VERSION: u8 = 1;
pub const MAX_IMMUTABLE_GIT_OBJECT_POOL_AUDIT_ENTRIES: u64 = 2_000_000;
pub const MAX_IMMUTABLE_GIT_OBJECT_POOL_AUDIT_DEPTH: u16 = 64;

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NONBLOCK);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImmutableGitObjectPoolGenerationAuditDisposition {
    InodeIndependentCandidate,
}

/// Bounded publication evidence for one candidate generation.
///
/// Counts are logical inventory observations only. This receipt intentionally carries no
/// `st_blocks` sum or unique-allocation estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ImmutableGitObjectPoolGenerationAuditReceipt {
    schema_version: u8,
    disposition: ImmutableGitObjectPoolGenerationAuditDisposition,
    source_object_regular_files: u64,
    candidate_directories: u64,
    candidate_regular_files: u64,
    candidate_object_regular_files: u64,
    candidate_logical_bytes: u64,
    inode_independent: bool,
    candidate_single_link_regular_files: bool,
    safe_entry_types: bool,
    nested_alternates_absent: bool,
}

impl ImmutableGitObjectPoolGenerationAuditReceipt {
    #[must_use]
    pub const fn schema_version(self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn disposition(self) -> ImmutableGitObjectPoolGenerationAuditDisposition {
        self.disposition
    }

    #[must_use]
    pub const fn source_object_regular_files(self) -> u64 {
        self.source_object_regular_files
    }

    #[must_use]
    pub const fn candidate_directories(self) -> u64 {
        self.candidate_directories
    }

    #[must_use]
    pub const fn candidate_regular_files(self) -> u64 {
        self.candidate_regular_files
    }

    #[must_use]
    pub const fn candidate_object_regular_files(self) -> u64 {
        self.candidate_object_regular_files
    }

    #[must_use]
    pub const fn candidate_logical_bytes(self) -> u64 {
        self.candidate_logical_bytes
    }

    #[must_use]
    pub const fn inode_independent(self) -> bool {
        self.inode_independent
    }

    #[must_use]
    pub const fn candidate_single_link_regular_files(self) -> bool {
        self.candidate_single_link_regular_files
    }

    #[must_use]
    pub const fn safe_entry_types(self) -> bool {
        self.safe_entry_types
    }

    #[must_use]
    pub const fn nested_alternates_absent(self) -> bool {
        self.nested_alternates_absent
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImmutableGitObjectPoolGenerationAuditErrorKind {
    InvalidPath,
    Missing,
    UnsafeEntry,
    TooLarge,
    SharedObjectInode,
    CandidateHardlink,
    NestedAlternates,
    ChangedDuringAudit,
    Io,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct ImmutableGitObjectPoolGenerationAuditError {
    kind: ImmutableGitObjectPoolGenerationAuditErrorKind,
    code: &'static str,
    message: &'static str,
}

impl ImmutableGitObjectPoolGenerationAuditError {
    #[must_use]
    pub const fn kind(&self) -> ImmutableGitObjectPoolGenerationAuditErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for ImmutableGitObjectPoolGenerationAuditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImmutableGitObjectPoolGenerationAuditError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for ImmutableGitObjectPoolGenerationAuditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ImmutableGitObjectPoolGenerationAuditError {}

/// Audit a staged bare Git generation against the exact producer object directory before root
/// ownership/freeze/publication.
///
/// Both paths are locators only. The audit opens every traversed directory and file with no-follow
/// semantics, rejects symlinks and special entries, and revalidates directory/file identities while
/// walking. The candidate must have an `objects/` directory, every candidate regular file must have
/// one link, and no candidate object inode may equal a producer object inode.
/// The caller must already have ended the producer process group and revoked producer/task path
/// traversal to the candidate; that quiescence must remain in force through root ownership/freeze.
///
/// # Errors
///
/// Fails closed on unsafe entries, hardlinks, shared object inodes, nested alternates, excessive
/// inventory/depth, concurrent drift, missing required directories, or I/O failure.
pub fn audit_immutable_git_object_pool_generation_candidate(
    source_objects_path: &Path,
    candidate_generation_path: &Path,
) -> Result<ImmutableGitObjectPoolGenerationAuditReceipt, ImmutableGitObjectPoolGenerationAuditError>
{
    let source_objects = BoundDirectory::open_absolute(source_objects_path)?;
    let candidate_root = BoundDirectory::open_absolute(candidate_generation_path)?;
    let candidate_objects = BoundDirectory::open_child(&candidate_root.fd, "objects")?;

    let source = audit_tree(&source_objects.fd, false, true)?;
    source_objects.revalidate()?;

    let candidate = audit_tree(&candidate_root.fd, true, false)?;
    candidate_root.revalidate()?;

    let candidate_objects_audit = audit_tree(&candidate_objects.fd, true, true)?;
    require_nested_alternates_absent(&candidate_objects)?;
    candidate_objects.revalidate()?;

    let rebound_objects = BoundDirectory::open_child(&candidate_root.fd, "objects")?;
    if rebound_objects.snapshot != candidate_objects.snapshot {
        return Err(changed());
    }
    candidate_root.revalidate()?;

    if source.regular_file_inodes.iter().any(|identity| {
        candidate_objects_audit
            .regular_file_inodes
            .contains(identity)
    }) {
        return Err(shared_inode());
    }
    if candidate.has_multiple_links || candidate_objects_audit.has_multiple_links {
        return Err(candidate_hardlink());
    }

    Ok(ImmutableGitObjectPoolGenerationAuditReceipt {
        schema_version: IMMUTABLE_GIT_OBJECT_POOL_GENERATION_AUDIT_SCHEMA_VERSION,
        disposition: ImmutableGitObjectPoolGenerationAuditDisposition::InodeIndependentCandidate,
        source_object_regular_files: source.regular_files,
        candidate_directories: candidate.directories,
        candidate_regular_files: candidate.regular_files,
        candidate_object_regular_files: candidate_objects_audit.regular_files,
        candidate_logical_bytes: candidate.logical_bytes,
        inode_independent: true,
        candidate_single_link_regular_files: true,
        safe_entry_types: true,
        nested_alternates_absent: true,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct PhysicalFileIdentity {
    device: u64,
    inode: u64,
}

#[derive(Default)]
struct TreeAudit {
    directories: u64,
    regular_files: u64,
    logical_bytes: u64,
    has_multiple_links: bool,
    regular_file_inodes: BTreeSet<PhysicalFileIdentity>,
}

fn audit_tree(
    root: &OwnedFd,
    require_single_links: bool,
    collect_inodes: bool,
) -> Result<TreeAudit, ImmutableGitObjectPoolGenerationAuditError> {
    let mut audit = TreeAudit {
        directories: 1,
        ..TreeAudit::default()
    };
    let mut entries = 0_u64;
    audit_directory(
        root,
        0,
        require_single_links,
        collect_inodes,
        &mut entries,
        &mut audit,
    )?;
    Ok(audit)
}

fn audit_directory(
    directory: &OwnedFd,
    depth: u16,
    require_single_links: bool,
    collect_inodes: bool,
    entries_seen: &mut u64,
    audit: &mut TreeAudit,
) -> Result<(), ImmutableGitObjectPoolGenerationAuditError> {
    if depth > MAX_IMMUTABLE_GIT_OBJECT_POOL_AUDIT_DEPTH {
        return Err(too_large());
    }
    let before = snapshot_directory(directory)?;
    let mut entries = Dir::read_from(directory).map_err(|_| io_error())?;
    for entry in &mut entries {
        let entry = entry.map_err(|_| io_error())?;
        let name = entry.file_name();
        let bytes = name.to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        *entries_seen = entries_seen.checked_add(1).ok_or_else(too_large)?;
        if *entries_seen > MAX_IMMUTABLE_GIT_OBJECT_POOL_AUDIT_ENTRIES {
            return Err(too_large());
        }

        let observed = rustix_fs::statat(directory.as_fd(), name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(map_stat)?;
        let file_type = FileType::from_raw_mode(observed.st_mode);
        if file_type.is_dir() {
            let child = BoundDirectory::open_child(directory, name)?;
            if !same_directory_identity(&observed, &child.snapshot) {
                return Err(changed());
            }
            audit.directories = audit.directories.checked_add(1).ok_or_else(too_large)?;
            audit_directory(
                &child.fd,
                depth.checked_add(1).ok_or_else(too_large)?,
                require_single_links,
                collect_inodes,
                entries_seen,
                audit,
            )?;
            child.revalidate()?;
            let rebound = BoundDirectory::open_child(directory, name)?;
            if rebound.snapshot != child.snapshot {
                return Err(changed());
            }
            continue;
        }
        if !file_type.is_file() {
            return Err(unsafe_entry());
        }

        let file = rustix_fs::openat(directory.as_fd(), name, FILE_FLAGS, Mode::empty())
            .map_err(map_open)?;
        let file_before = snapshot_file(&file)?;
        let path_after = rustix_fs::statat(directory.as_fd(), name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(map_stat)?;
        let path_after = snapshot_file_stat(&path_after)?;
        let file_after = snapshot_file(&file)?;
        if file_before != file_after || file_before != path_after {
            return Err(changed());
        }

        audit.regular_files = audit.regular_files.checked_add(1).ok_or_else(too_large)?;
        audit.logical_bytes = audit
            .logical_bytes
            .checked_add(file_before.size)
            .ok_or_else(too_large)?;
        if require_single_links && file_before.nlink != 1 {
            audit.has_multiple_links = true;
        }
        if collect_inodes {
            audit.regular_file_inodes.insert(PhysicalFileIdentity {
                device: file_before.device,
                inode: file_before.inode,
            });
        }
    }

    let after = snapshot_directory(directory)?;
    if before != after {
        return Err(changed());
    }
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
    fn open_absolute(path: &Path) -> Result<Self, ImmutableGitObjectPoolGenerationAuditError> {
        let fd = open_absolute_directory(path)?;
        let snapshot = snapshot_directory(&fd)?;
        Ok(Self { fd, snapshot })
    }

    fn open_child(
        parent: &OwnedFd,
        name: impl rustix::path::Arg,
    ) -> Result<Self, ImmutableGitObjectPoolGenerationAuditError> {
        let fd = rustix_fs::openat(parent.as_fd(), name, DIRECTORY_FLAGS, Mode::empty())
            .map_err(map_open)?;
        let snapshot = snapshot_directory(&fd)?;
        Ok(Self { fd, snapshot })
    }

    fn revalidate(&self) -> Result<(), ImmutableGitObjectPoolGenerationAuditError> {
        if snapshot_directory(&self.fd).map_err(|_| changed())? != self.snapshot {
            return Err(changed());
        }
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
    size: u64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}

fn snapshot_directory(
    descriptor: &OwnedFd,
) -> Result<DirectorySnapshot, ImmutableGitObjectPoolGenerationAuditError> {
    let stat = rustix_fs::fstat(descriptor).map_err(|_| io_error())?;
    if !FileType::from_raw_mode(stat.st_mode).is_dir() {
        return Err(unsafe_entry());
    }
    Ok(DirectorySnapshot {
        device: stat.st_dev,
        inode: stat.st_ino,
        uid: stat.st_uid,
        gid: stat.st_gid,
        mode: stat.st_mode,
        mtime: stat.st_mtime,
        mtime_nsec: i64::try_from(stat.st_mtime_nsec).map_err(|_| unsafe_entry())?,
        ctime: stat.st_ctime,
        ctime_nsec: i64::try_from(stat.st_ctime_nsec).map_err(|_| unsafe_entry())?,
    })
}

fn same_directory_identity(stat: &rustix_fs::Stat, snapshot: &DirectorySnapshot) -> bool {
    FileType::from_raw_mode(stat.st_mode).is_dir()
        && stat.st_dev == snapshot.device
        && stat.st_ino == snapshot.inode
        && stat.st_uid == snapshot.uid
        && stat.st_gid == snapshot.gid
        && stat.st_mode == snapshot.mode
}

fn snapshot_file(
    descriptor: &OwnedFd,
) -> Result<FileSnapshot, ImmutableGitObjectPoolGenerationAuditError> {
    let stat = rustix_fs::fstat(descriptor).map_err(|_| io_error())?;
    snapshot_file_stat(&stat)
}

fn snapshot_file_stat(
    stat: &rustix_fs::Stat,
) -> Result<FileSnapshot, ImmutableGitObjectPoolGenerationAuditError> {
    if !FileType::from_raw_mode(stat.st_mode).is_file() {
        return Err(unsafe_entry());
    }
    Ok(FileSnapshot {
        device: stat.st_dev,
        inode: stat.st_ino,
        uid: stat.st_uid,
        gid: stat.st_gid,
        mode: stat.st_mode,
        nlink: u128::from(stat.st_nlink),
        size: u64::try_from(stat.st_size).map_err(|_| unsafe_entry())?,
        mtime: stat.st_mtime,
        mtime_nsec: i64::try_from(stat.st_mtime_nsec).map_err(|_| unsafe_entry())?,
        ctime: stat.st_ctime,
        ctime_nsec: i64::try_from(stat.st_ctime_nsec).map_err(|_| unsafe_entry())?,
    })
}

fn require_nested_alternates_absent(
    objects: &BoundDirectory,
) -> Result<(), ImmutableGitObjectPoolGenerationAuditError> {
    let before = snapshot_directory(&objects.fd)?;
    let info = match rustix_fs::openat(objects.fd.as_fd(), "info", DIRECTORY_FLAGS, Mode::empty()) {
        Ok(fd) => Some(BoundDirectory {
            snapshot: snapshot_directory(&fd)?,
            fd,
        }),
        Err(Errno::NOENT) => None,
        Err(error) => return Err(map_open(error)),
    };
    if let Some(info) = info {
        match rustix_fs::statat(info.fd.as_fd(), "alternates", AtFlags::SYMLINK_NOFOLLOW) {
            Err(Errno::NOENT) => {}
            Ok(_) => return Err(nested_alternates()),
            Err(_) => return Err(io_error()),
        }
        info.revalidate()?;
        let rebound = BoundDirectory::open_child(&objects.fd, "info")?;
        if rebound.snapshot != info.snapshot {
            return Err(changed());
        }
    } else {
        match rustix_fs::statat(objects.fd.as_fd(), "info", AtFlags::SYMLINK_NOFOLLOW) {
            Err(Errno::NOENT) => {}
            _ => return Err(changed()),
        }
    }
    if snapshot_directory(&objects.fd)? != before {
        return Err(changed());
    }
    Ok(())
}

fn open_absolute_directory(
    path: &Path,
) -> Result<OwnedFd, ImmutableGitObjectPoolGenerationAuditError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(invalid_path());
    }
    let mut current = rustix_fs::open("/", DIRECTORY_FLAGS, Mode::empty()).map_err(map_open)?;
    for component in path.components() {
        if let Component::Normal(name) = component {
            current = rustix_fs::openat(current.as_fd(), name, DIRECTORY_FLAGS, Mode::empty())
                .map_err(map_open)?;
        }
    }
    Ok(current)
}

fn map_open(error: Errno) -> ImmutableGitObjectPoolGenerationAuditError {
    match error {
        Errno::NOENT => missing(),
        Errno::LOOP | Errno::NOTDIR => unsafe_entry(),
        _ => io_error(),
    }
}

fn map_stat(error: Errno) -> ImmutableGitObjectPoolGenerationAuditError {
    match error {
        Errno::NOENT => changed(),
        Errno::LOOP | Errno::NOTDIR => unsafe_entry(),
        _ => io_error(),
    }
}

const fn error(
    kind: ImmutableGitObjectPoolGenerationAuditErrorKind,
    code: &'static str,
    message: &'static str,
) -> ImmutableGitObjectPoolGenerationAuditError {
    ImmutableGitObjectPoolGenerationAuditError {
        kind,
        code,
        message,
    }
}

const fn invalid_path() -> ImmutableGitObjectPoolGenerationAuditError {
    error(
        ImmutableGitObjectPoolGenerationAuditErrorKind::InvalidPath,
        "immutable_git_object_pool_audit_path_invalid",
        "immutable Git object-pool audit path is invalid",
    )
}

const fn missing() -> ImmutableGitObjectPoolGenerationAuditError {
    error(
        ImmutableGitObjectPoolGenerationAuditErrorKind::Missing,
        "immutable_git_object_pool_audit_missing",
        "immutable Git object-pool audit input is missing",
    )
}

const fn unsafe_entry() -> ImmutableGitObjectPoolGenerationAuditError {
    error(
        ImmutableGitObjectPoolGenerationAuditErrorKind::UnsafeEntry,
        "immutable_git_object_pool_audit_unsafe_entry",
        "immutable Git object-pool audit found an unsafe entry",
    )
}

const fn too_large() -> ImmutableGitObjectPoolGenerationAuditError {
    error(
        ImmutableGitObjectPoolGenerationAuditErrorKind::TooLarge,
        "immutable_git_object_pool_audit_too_large",
        "immutable Git object-pool audit exceeded its bounded inventory",
    )
}

const fn shared_inode() -> ImmutableGitObjectPoolGenerationAuditError {
    error(
        ImmutableGitObjectPoolGenerationAuditErrorKind::SharedObjectInode,
        "immutable_git_object_pool_audit_shared_inode",
        "immutable Git object-pool candidate shares an object inode with its producer",
    )
}

const fn candidate_hardlink() -> ImmutableGitObjectPoolGenerationAuditError {
    error(
        ImmutableGitObjectPoolGenerationAuditErrorKind::CandidateHardlink,
        "immutable_git_object_pool_audit_candidate_hardlink",
        "immutable Git object-pool candidate contains a multiply linked regular file",
    )
}

const fn nested_alternates() -> ImmutableGitObjectPoolGenerationAuditError {
    error(
        ImmutableGitObjectPoolGenerationAuditErrorKind::NestedAlternates,
        "immutable_git_object_pool_audit_nested_alternates",
        "immutable Git object-pool candidate contains a nested alternates entry",
    )
}

const fn changed() -> ImmutableGitObjectPoolGenerationAuditError {
    error(
        ImmutableGitObjectPoolGenerationAuditErrorKind::ChangedDuringAudit,
        "immutable_git_object_pool_audit_changed",
        "immutable Git object-pool audit input changed during inspection",
    )
}

const fn io_error() -> ImmutableGitObjectPoolGenerationAuditError {
    error(
        ImmutableGitObjectPoolGenerationAuditErrorKind::Io,
        "immutable_git_object_pool_audit_io",
        "immutable Git object-pool generation audit failed",
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{
        ImmutableGitObjectPoolGenerationAuditDisposition,
        ImmutableGitObjectPoolGenerationAuditErrorKind,
        audit_immutable_git_object_pool_generation_candidate,
    };

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

    struct Fixture {
        base: PathBuf,
        source_objects: PathBuf,
        candidate: PathBuf,
        source_object: PathBuf,
        candidate_object: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let unique = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let base = std::env::temp_dir().join(format!(
                "smolrunner-git-pool-generation-audit-{}-{unique}",
                std::process::id()
            ));
            let source_objects = base.join("source/objects");
            let candidate = base.join("candidate.git");
            let source_object = source_objects.join("aa/source-object");
            let candidate_object = candidate.join("objects/bb/candidate-object");
            fs::create_dir_all(source_object.parent().unwrap()).unwrap();
            fs::create_dir_all(candidate_object.parent().unwrap()).unwrap();
            fs::create_dir_all(candidate.join("objects/info")).unwrap();
            fs::write(&source_object, b"producer object bytes").unwrap();
            fs::write(&candidate_object, b"independent candidate object bytes").unwrap();
            fs::write(candidate.join("HEAD"), b"ref: refs/heads/main\n").unwrap();
            Self {
                base,
                source_objects,
                candidate,
                source_object,
                candidate_object,
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    #[test]
    fn independent_candidate_is_accepted_without_allocation_claims() {
        let fixture = Fixture::new();
        let receipt = audit_immutable_git_object_pool_generation_candidate(
            &fixture.source_objects,
            &fixture.candidate,
        )
        .unwrap();
        assert_eq!(
            receipt.disposition(),
            ImmutableGitObjectPoolGenerationAuditDisposition::InodeIndependentCandidate
        );
        assert_eq!(receipt.source_object_regular_files(), 1);
        assert_eq!(receipt.candidate_object_regular_files(), 1);
        assert!(receipt.inode_independent());
        assert!(receipt.candidate_single_link_regular_files());
        assert!(receipt.safe_entry_types());
        assert!(receipt.nested_alternates_absent());
        assert!(receipt.candidate_logical_bytes() > 0);
    }

    #[test]
    fn producer_object_hardlink_is_refused_as_shared_inode() {
        let fixture = Fixture::new();
        fs::remove_file(&fixture.candidate_object).unwrap();
        fs::hard_link(&fixture.source_object, &fixture.candidate_object).unwrap();
        assert_eq!(
            audit_immutable_git_object_pool_generation_candidate(
                &fixture.source_objects,
                &fixture.candidate,
            )
            .unwrap_err()
            .kind(),
            ImmutableGitObjectPoolGenerationAuditErrorKind::SharedObjectInode
        );
    }

    #[test]
    fn external_candidate_hardlink_is_refused() {
        let fixture = Fixture::new();
        let outside = fixture.base.join("outside-link");
        fs::hard_link(&fixture.candidate_object, &outside).unwrap();
        assert_eq!(
            audit_immutable_git_object_pool_generation_candidate(
                &fixture.source_objects,
                &fixture.candidate,
            )
            .unwrap_err()
            .kind(),
            ImmutableGitObjectPoolGenerationAuditErrorKind::CandidateHardlink
        );
    }

    #[test]
    fn symlink_entry_is_refused() {
        let fixture = Fixture::new();
        symlink("HEAD", fixture.candidate.join("alias")).unwrap();
        assert_eq!(
            audit_immutable_git_object_pool_generation_candidate(
                &fixture.source_objects,
                &fixture.candidate,
            )
            .unwrap_err()
            .kind(),
            ImmutableGitObjectPoolGenerationAuditErrorKind::UnsafeEntry
        );
    }

    #[test]
    fn nested_alternates_are_refused() {
        let fixture = Fixture::new();
        fs::write(
            fixture.candidate.join("objects/info/alternates"),
            b"/unexpected/objects\n",
        )
        .unwrap();
        assert_eq!(
            audit_immutable_git_object_pool_generation_candidate(
                &fixture.source_objects,
                &fixture.candidate,
            )
            .unwrap_err()
            .kind(),
            ImmutableGitObjectPoolGenerationAuditErrorKind::NestedAlternates
        );
    }
}
