//! Read-only byte proof for required objects in one persistent Blender content store.
//!
//! The observer never enumerates the store. It derives canonical object keys only from one exact
//! Blender snapshot, opens those paths beneath a held store-root descriptor without following
//! symlinks, verifies size and SHA-256 from held regular files, and rebinds the complete observed
//! path before returning. It grants zero publication, repair, deletion, provider, network, lease,
//! or execution authority.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::Read as _;
use std::os::fd::{AsFd as _, OwnedFd};
use std::path::Path;

use rustix::fs::{self as rustix_fs, FileType, Mode, OFlags, Stat};
use rustix::io::Errno;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::compute_execution_request::blender_content_store::BlenderContentObjectKey;
use crate::compute_execution_request::blender_snapshot::{
    BlenderContentObject, BlenderProjectSnapshot, BlenderRemoteInventory,
};

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const SHA256_PREFIX: &str = "sha256:";
const READ_BUFFER_BYTES: usize = 1024 * 1024;

/// Observe only the content objects required by `snapshot`.
///
/// Missing canonical object paths are reported as absent. A path that exists but cannot prove the
/// exact expected regular-file bytes refuses the complete observation instead of being advertised
/// as present.
///
/// # Errors
///
/// Returns a bounded, path-private error for an unsafe root, unsafe filesystem node, size/content
/// mismatch, filesystem drift, hardlink ambiguity, unreadable object, or invalid typed inventory
/// construction.
pub fn observe_blender_content_store(
    root: &Path,
    snapshot: &BlenderProjectSnapshot,
) -> Result<BlenderRemoteInventory, BlenderContentStoreObservationError> {
    observe_with_hook(root, snapshot, |_| {})
}

fn observe_with_hook<F>(
    root_path: &Path,
    snapshot: &BlenderProjectSnapshot,
    mut before_object_revalidation: F,
) -> Result<BlenderRemoteInventory, BlenderContentStoreObservationError>
where
    F: FnMut(&Sha256Digest),
{
    let root = BoundStoreRoot::open(root_path)?;
    let Some(objects_directory) = root.directory.open_optional_child("objects")? else {
        root.confirm_child_absent("objects")?;
        return empty_inventory();
    };
    let Some(version_directory) = objects_directory.open_optional_child("v1")? else {
        root.revalidate()?;
        objects_directory.confirm_child_absent("v1")?;
        return empty_inventory();
    };
    let Some(digest_directory) = version_directory.open_optional_child("sha256")? else {
        root.revalidate()?;
        version_directory.confirm_child_absent("sha256")?;
        return empty_inventory();
    };

    let required = required_objects(snapshot)?;
    let mut observed_buckets = BTreeMap::<String, ObservedBucket>::new();
    let mut present = Vec::new();
    for (digest, expected_bytes) in required {
        let key = BlenderContentObjectKey::from_digest(&digest);
        let (bucket_name, object_name) = object_key_components(&key)?;
        let Some(bucket_directory) = digest_directory.open_optional_child(bucket_name)? else {
            continue;
        };
        match observed_buckets.get(bucket_name) {
            Some(existing) if existing.snapshot != bucket_directory.snapshot => {
                return Err(changed());
            }
            _ => {}
        }
        let Some(mut object) = BoundObject::open_optional(&bucket_directory, object_name)? else {
            continue;
        };
        if object.snapshot.links != 1 {
            return Err(hardlink_ambiguous());
        }
        if object.snapshot.bytes != expected_bytes {
            return Err(size_mismatch());
        }
        let observed_digest = hash_held_file(&mut object.file, expected_bytes)?;
        if observed_digest != digest {
            return Err(content_mismatch());
        }

        before_object_revalidation(&digest);
        object.revalidate(&bucket_directory, object_name)?;
        observed_buckets
            .entry(bucket_name.to_owned())
            .or_insert_with(|| ObservedBucket {
                snapshot: bucket_directory.snapshot,
                objects: Vec::new(),
            })
            .objects
            .push(ObservedObject {
                name: object_name.to_owned(),
                snapshot: object.snapshot,
            });
        present.push(
            BlenderContentObject::new(digest, expected_bytes).map_err(|_| invalid_inventory())?,
        );
    }

    root.revalidate_complete_path(
        &objects_directory,
        &version_directory,
        &digest_directory,
        &observed_buckets,
    )?;
    BlenderRemoteInventory::new(present).map_err(|_| invalid_inventory())
}

fn required_objects(
    snapshot: &BlenderProjectSnapshot,
) -> Result<BTreeMap<Sha256Digest, u64>, BlenderContentStoreObservationError> {
    let mut required = BTreeMap::new();
    for file in snapshot.files() {
        match required.insert(file.digest().clone(), file.bytes()) {
            Some(previous) if previous != file.bytes() => return Err(invalid_inventory()),
            _ => {}
        }
    }
    Ok(required)
}

fn empty_inventory() -> Result<BlenderRemoteInventory, BlenderContentStoreObservationError> {
    BlenderRemoteInventory::new(Vec::new()).map_err(|_| invalid_inventory())
}

fn object_key_components(
    key: &BlenderContentObjectKey,
) -> Result<(&str, &str), BlenderContentStoreObservationError> {
    let suffix = key
        .as_str()
        .strip_prefix("objects/v1/sha256/")
        .ok_or_else(invalid_inventory)?;
    let (bucket, object_name) = suffix.split_once('/').ok_or_else(invalid_inventory)?;
    if bucket.len() != 2 || object_name.len() != 62 || object_name.contains('/') {
        return Err(invalid_inventory());
    }
    Ok((bucket, object_name))
}

#[derive(Debug)]
struct BoundStoreRoot<'a> {
    path: &'a Path,
    directory: BoundDirectory,
}

impl<'a> BoundStoreRoot<'a> {
    fn open(path: &'a Path) -> Result<Self, BlenderContentStoreObservationError> {
        if !path.is_absolute() || std::fs::canonicalize(path).map_err(|_| unsafe_root())? != path {
            return Err(unsafe_root());
        }
        let fd =
            rustix_fs::open(path, DIRECTORY_FLAGS, Mode::empty()).map_err(|_| unsafe_root())?;
        let directory = BoundDirectory::from_fd(fd).map_err(|_| unsafe_root())?;
        Ok(Self { path, directory })
    }

    fn revalidate(&self) -> Result<(), BlenderContentStoreObservationError> {
        self.directory.revalidate_held()?;
        let fd =
            rustix_fs::open(self.path, DIRECTORY_FLAGS, Mode::empty()).map_err(|_| changed())?;
        let rebound = BoundDirectory::from_fd(fd).map_err(|_| changed())?;
        if rebound.snapshot != self.directory.snapshot {
            return Err(changed());
        }
        Ok(())
    }

    fn confirm_child_absent(&self, name: &str) -> Result<(), BlenderContentStoreObservationError> {
        self.revalidate()?;
        self.directory.confirm_child_absent(name)
    }

    fn revalidate_complete_path(
        &self,
        objects: &BoundDirectory,
        version: &BoundDirectory,
        digest_directory: &BoundDirectory,
        observed_buckets: &BTreeMap<String, ObservedBucket>,
    ) -> Result<(), BlenderContentStoreObservationError> {
        self.revalidate()?;
        let current_objects = self
            .directory
            .reopen_bound_child("objects", objects.snapshot)?;
        let current_version = current_objects.reopen_bound_child("v1", version.snapshot)?;
        let current_digest =
            current_version.reopen_bound_child("sha256", digest_directory.snapshot)?;

        for (bucket_name, observed_bucket) in observed_buckets {
            let current_bucket =
                current_digest.reopen_bound_child(bucket_name, observed_bucket.snapshot)?;
            for observed_object in &observed_bucket.objects {
                BoundObject::revalidate_current(
                    &current_bucket,
                    &observed_object.name,
                    observed_object.snapshot,
                )?;
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
struct BoundDirectory {
    fd: OwnedFd,
    snapshot: DirectorySnapshot,
}

impl BoundDirectory {
    fn from_fd(fd: OwnedFd) -> Result<Self, BlenderContentStoreObservationError> {
        let snapshot = directory_snapshot(fd.as_fd())?;
        Ok(Self { fd, snapshot })
    }

    fn open_optional_child(
        &self,
        name: &str,
    ) -> Result<Option<Self>, BlenderContentStoreObservationError> {
        match rustix_fs::openat(self.fd.as_fd(), name, DIRECTORY_FLAGS, Mode::empty()) {
            Ok(fd) => Self::from_fd(fd).map(Some),
            Err(Errno::NOENT) => Ok(None),
            Err(_) => Err(unsafe_node()),
        }
    }

    fn confirm_child_absent(&self, name: &str) -> Result<(), BlenderContentStoreObservationError> {
        self.revalidate_held()?;
        match rustix_fs::openat(self.fd.as_fd(), name, DIRECTORY_FLAGS, Mode::empty()) {
            Err(Errno::NOENT) => Ok(()),
            _ => Err(changed()),
        }
    }

    fn reopen_bound_child(
        &self,
        name: &str,
        expected: DirectorySnapshot,
    ) -> Result<Self, BlenderContentStoreObservationError> {
        self.revalidate_held()?;
        let fd = rustix_fs::openat(self.fd.as_fd(), name, DIRECTORY_FLAGS, Mode::empty())
            .map_err(|_| changed())?;
        let current = Self::from_fd(fd).map_err(|_| changed())?;
        if current.snapshot != expected {
            return Err(changed());
        }
        Ok(current)
    }

    fn revalidate_held(&self) -> Result<(), BlenderContentStoreObservationError> {
        if directory_snapshot(self.fd.as_fd())? != self.snapshot {
            return Err(changed());
        }
        Ok(())
    }
}

#[derive(Debug)]
struct BoundObject {
    file: File,
    snapshot: FileSnapshot,
}

impl BoundObject {
    fn open_optional(
        parent: &BoundDirectory,
        name: &str,
    ) -> Result<Option<Self>, BlenderContentStoreObservationError> {
        match rustix_fs::openat(parent.fd.as_fd(), name, FILE_FLAGS, Mode::empty()) {
            Ok(fd) => {
                let snapshot = file_snapshot(fd.as_fd())?;
                Ok(Some(Self {
                    file: File::from(fd),
                    snapshot,
                }))
            }
            Err(Errno::NOENT) => Ok(None),
            Err(_) => Err(unsafe_node()),
        }
    }

    fn revalidate(
        &self,
        parent: &BoundDirectory,
        name: &str,
    ) -> Result<(), BlenderContentStoreObservationError> {
        let held = file_snapshot(self.file.as_fd())?;
        if held != self.snapshot {
            return Err(changed());
        }
        Self::revalidate_current(parent, name, self.snapshot)
    }

    fn revalidate_current(
        parent: &BoundDirectory,
        name: &str,
        expected: FileSnapshot,
    ) -> Result<(), BlenderContentStoreObservationError> {
        parent.revalidate_held()?;
        let fd = rustix_fs::openat(parent.fd.as_fd(), name, FILE_FLAGS, Mode::empty())
            .map_err(|_| changed())?;
        let current = file_snapshot(fd.as_fd()).map_err(|_| changed())?;
        if current != expected || current.links != 1 {
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
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}

fn directory_snapshot(
    descriptor: impl std::os::fd::AsFd,
) -> Result<DirectorySnapshot, BlenderContentStoreObservationError> {
    let stat = rustix_fs::fstat(descriptor).map_err(|_| unreadable())?;
    if !FileType::from_raw_mode(stat.st_mode).is_dir() {
        return Err(unsafe_node());
    }
    Ok(DirectorySnapshot {
        device: stat.st_dev,
        inode: stat.st_ino,
        mode: stat.st_mode,
        mtime: stat.st_mtime,
        mtime_nsec: i64::try_from(stat.st_mtime_nsec).map_err(|_| unsafe_node())?,
        ctime: stat.st_ctime,
        ctime_nsec: i64::try_from(stat.st_ctime_nsec).map_err(|_| unsafe_node())?,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileSnapshot {
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    bytes: u64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}

fn file_snapshot(
    descriptor: impl std::os::fd::AsFd,
) -> Result<FileSnapshot, BlenderContentStoreObservationError> {
    let stat = rustix_fs::fstat(descriptor).map_err(|_| unreadable())?;
    if !FileType::from_raw_mode(stat.st_mode).is_file() {
        return Err(unsafe_node());
    }
    stat_snapshot(&stat)
}

fn stat_snapshot(stat: &Stat) -> Result<FileSnapshot, BlenderContentStoreObservationError> {
    Ok(FileSnapshot {
        device: stat.st_dev,
        inode: stat.st_ino,
        mode: stat.st_mode,
        links: u64::from(stat.st_nlink),
        bytes: u64::try_from(stat.st_size).map_err(|_| unsafe_node())?,
        mtime: stat.st_mtime,
        mtime_nsec: i64::try_from(stat.st_mtime_nsec).map_err(|_| unsafe_node())?,
        ctime: stat.st_ctime,
        ctime_nsec: i64::try_from(stat.st_ctime_nsec).map_err(|_| unsafe_node())?,
    })
}

#[derive(Debug)]
struct ObservedBucket {
    snapshot: DirectorySnapshot,
    objects: Vec<ObservedObject>,
}

#[derive(Debug)]
struct ObservedObject {
    name: String,
    snapshot: FileSnapshot,
}

fn hash_held_file(
    file: &mut File,
    expected_bytes: u64,
) -> Result<Sha256Digest, BlenderContentStoreObservationError> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; READ_BUFFER_BYTES];
    let mut observed_bytes = 0_u64;
    loop {
        let read = file.read(&mut buffer).map_err(|_| unreadable())?;
        if read == 0 {
            break;
        }
        observed_bytes = observed_bytes
            .checked_add(u64::try_from(read).map_err(|_| unreadable())?)
            .ok_or_else(unreadable)?;
        if observed_bytes > expected_bytes {
            return Err(size_mismatch());
        }
        hasher.update(&buffer[..read]);
    }
    if observed_bytes != expected_bytes {
        return Err(size_mismatch());
    }
    let encoded = format!("{SHA256_PREFIX}{:x}", hasher.finalize());
    Sha256Digest::parse(&encoded).map_err(|_| invalid_inventory())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlenderContentStoreObservationError {
    code: &'static str,
    message: &'static str,
}

impl BlenderContentStoreObservationError {
    const fn new(code: &'static str, message: &'static str) -> Self {
        Self { code, message }
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for BlenderContentStoreObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for BlenderContentStoreObservationError {}

const fn unsafe_root() -> BlenderContentStoreObservationError {
    BlenderContentStoreObservationError::new(
        "blender_content_store_root_unsafe",
        "Blender content-store root is unavailable or unsafe",
    )
}

const fn unsafe_node() -> BlenderContentStoreObservationError {
    BlenderContentStoreObservationError::new(
        "blender_content_store_object_unsafe",
        "Blender content-store object path is unavailable or unsafe",
    )
}

const fn unreadable() -> BlenderContentStoreObservationError {
    BlenderContentStoreObservationError::new(
        "blender_content_store_object_unreadable",
        "Blender content-store object could not be read completely",
    )
}

const fn hardlink_ambiguous() -> BlenderContentStoreObservationError {
    BlenderContentStoreObservationError::new(
        "blender_content_store_hardlink_ambiguous",
        "Blender content-store object has ambiguous hardlink aliases",
    )
}

const fn size_mismatch() -> BlenderContentStoreObservationError {
    BlenderContentStoreObservationError::new(
        "blender_content_store_size_mismatch",
        "Blender content-store object byte length does not match its expected identity",
    )
}

const fn content_mismatch() -> BlenderContentStoreObservationError {
    BlenderContentStoreObservationError::new(
        "blender_content_store_digest_mismatch",
        "Blender content-store object bytes do not match their expected digest",
    )
}

const fn changed() -> BlenderContentStoreObservationError {
    BlenderContentStoreObservationError::new(
        "blender_content_store_changed",
        "Blender content-store observation changed while it was being verified",
    )
}

const fn invalid_inventory() -> BlenderContentStoreObservationError {
    BlenderContentStoreObservationError::new(
        "blender_content_store_inventory_invalid",
        "Blender content-store observation could not form a valid provider-neutral inventory",
    )
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::compute_execution_request::blender_snapshot::{
        BlenderProjectFile, BlenderProjectFileClass, BlenderRelativePath, BlenderRuntimeId,
    };

    struct Fixture {
        root: std::path::PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let parent = std::fs::canonicalize(std::env::temp_dir()).unwrap();
            let root = parent.join(format!(
                "glaeda-blender-content-store-{}-{nonce}",
                std::process::id()
            ));
            std::fs::create_dir(&root).unwrap();
            Self { root }
        }

        fn publish(&self, bytes: &[u8]) -> Sha256Digest {
            let digest = digest(bytes);
            let path = self.object_path(&digest);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
            digest
        }

        fn object_path(&self, digest: &Sha256Digest) -> std::path::PathBuf {
            self.root
                .join(BlenderContentObjectKey::from_digest(digest).as_str())
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn digest(bytes: &[u8]) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{:x}", Sha256::digest(bytes))).unwrap()
    }

    fn snapshot(files: Vec<(&str, BlenderProjectFileClass, &[u8])>) -> BlenderProjectSnapshot {
        BlenderProjectSnapshot::new(
            BlenderRuntimeId::parse("blender-5.2.0").unwrap(),
            BlenderRelativePath::parse("scenes/main.blend").unwrap(),
            files
                .into_iter()
                .map(|(path, class, bytes)| {
                    BlenderProjectFile::new(
                        BlenderRelativePath::parse(path).unwrap(),
                        class,
                        digest(bytes),
                        u64::try_from(bytes.len()).unwrap(),
                    )
                    .unwrap()
                })
                .collect(),
        )
        .unwrap()
    }

    #[test]
    fn proves_only_required_matching_objects() {
        let fixture = Fixture::new();
        let scene = b"scene bytes";
        let asset = b"asset bytes";
        fixture.publish(scene);
        fixture.publish(b"unrelated object that is never enumerated");
        let project = snapshot(vec![
            ("scenes/main.blend", BlenderProjectFileClass::Scene, scene),
            ("assets/wood.exr", BlenderProjectFileClass::Asset, asset),
        ]);

        let inventory = observe_blender_content_store(&fixture.root, &project).unwrap();
        assert_eq!(inventory.objects().len(), 1);
        assert_eq!(inventory.objects()[0].digest(), &digest(scene));
        assert_eq!(inventory.objects()[0].bytes(), scene.len() as u64);
    }

    #[test]
    fn duplicate_snapshot_content_is_observed_once() {
        let fixture = Fixture::new();
        let shared = b"same bytes";
        fixture.publish(shared);
        let project = snapshot(vec![
            ("scenes/main.blend", BlenderProjectFileClass::Scene, shared),
            ("assets/a.bin", BlenderProjectFileClass::Asset, shared),
            ("assets/b.bin", BlenderProjectFileClass::Asset, shared),
        ]);

        let inventory = observe_blender_content_store(&fixture.root, &project).unwrap();
        assert_eq!(inventory.objects().len(), 1);
    }

    #[test]
    fn matching_key_with_wrong_content_refuses() {
        let fixture = Fixture::new();
        let expected = b"expected";
        let project = snapshot(vec![(
            "scenes/main.blend",
            BlenderProjectFileClass::Scene,
            expected,
        )]);
        let path = fixture.object_path(&digest(expected));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"badbytes").unwrap();

        let error = observe_blender_content_store(&fixture.root, &project).unwrap_err();
        assert_eq!(error.code(), "blender_content_store_digest_mismatch");
    }

    #[test]
    fn wrong_length_refuses_before_advertising_presence() {
        let fixture = Fixture::new();
        let expected = b"expected";
        let project = snapshot(vec![(
            "scenes/main.blend",
            BlenderProjectFileClass::Scene,
            expected,
        )]);
        let path = fixture.object_path(&digest(expected));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"short").unwrap();

        let error = observe_blender_content_store(&fixture.root, &project).unwrap_err();
        assert_eq!(error.code(), "blender_content_store_size_mismatch");
    }

    #[test]
    fn symlinked_required_object_refuses() {
        let fixture = Fixture::new();
        let expected = b"expected";
        let project = snapshot(vec![(
            "scenes/main.blend",
            BlenderProjectFileClass::Scene,
            expected,
        )]);
        let real = fixture.root.join("real-object");
        std::fs::write(&real, expected).unwrap();
        let path = fixture.object_path(&digest(expected));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        symlink(real, path).unwrap();

        let error = observe_blender_content_store(&fixture.root, &project).unwrap_err();
        assert_eq!(error.code(), "blender_content_store_object_unsafe");
    }

    #[test]
    fn hardlinked_required_object_refuses() {
        let fixture = Fixture::new();
        let expected = b"expected";
        let project = snapshot(vec![(
            "scenes/main.blend",
            BlenderProjectFileClass::Scene,
            expected,
        )]);
        let source = fixture.root.join("hardlink-source");
        std::fs::write(&source, expected).unwrap();
        let path = fixture.object_path(&digest(expected));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::hard_link(source, path).unwrap();

        let error = observe_blender_content_store(&fixture.root, &project).unwrap_err();
        assert_eq!(error.code(), "blender_content_store_hardlink_ambiguous");
    }

    #[test]
    fn object_replacement_after_hash_refuses() {
        let fixture = Fixture::new();
        let expected = b"expected";
        let expected_digest = fixture.publish(expected);
        let object_path = fixture.object_path(&expected_digest);
        let replaced = fixture.root.join("replaced-object");
        let project = snapshot(vec![(
            "scenes/main.blend",
            BlenderProjectFileClass::Scene,
            expected,
        )]);
        let mut replaced_once = false;
        let error = observe_with_hook(&fixture.root, &project, |_| {
            if replaced_once {
                return;
            }
            replaced_once = true;
            std::fs::rename(&object_path, &replaced).unwrap();
            std::fs::write(&object_path, expected).unwrap();
        })
        .unwrap_err();
        assert_eq!(error.code(), "blender_content_store_changed");
    }

    #[test]
    fn bucket_replacement_after_hash_refuses() {
        let fixture = Fixture::new();
        let expected = b"expected";
        let expected_digest = fixture.publish(expected);
        let object_path = fixture.object_path(&expected_digest);
        let bucket = object_path.parent().unwrap().to_path_buf();
        let displaced = bucket.with_extension("displaced");
        let project = snapshot(vec![(
            "scenes/main.blend",
            BlenderProjectFileClass::Scene,
            expected,
        )]);
        let mut replaced_once = false;
        let error = observe_with_hook(&fixture.root, &project, |_| {
            if replaced_once {
                return;
            }
            replaced_once = true;
            std::fs::rename(&bucket, &displaced).unwrap();
            std::fs::create_dir(&bucket).unwrap();
        })
        .unwrap_err();
        assert_eq!(error.code(), "blender_content_store_changed");
    }

    #[test]
    fn noncanonical_root_refuses_without_leaking_path() {
        let fixture = Fixture::new();
        let alias = fixture.root.with_extension("alias");
        symlink(&fixture.root, &alias).unwrap();
        let project = snapshot(vec![(
            "scenes/main.blend",
            BlenderProjectFileClass::Scene,
            b"scene",
        )]);
        let error = observe_blender_content_store(&alias, &project).unwrap_err();
        let _ = std::fs::remove_file(alias);
        assert_eq!(error.code(), "blender_content_store_root_unsafe");
        assert!(!error.to_string().contains(fixture.root.to_str().unwrap()));
    }
}
