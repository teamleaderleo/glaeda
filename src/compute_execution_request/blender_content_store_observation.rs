//! Read-only byte proof for required objects in one persistent Blender content store.
//!
//! The observer never enumerates the store. It derives canonical object keys only from one exact
//! Blender snapshot, opens those paths beneath a held store-root descriptor without following
//! symlinks, verifies size and SHA-256 from the held regular file, and rebinds the path afterward.
//! It grants zero publication, repair, deletion, provider, network, lease, or execution authority.

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
const FILE_FLAGS: OFlags = OFlags::RDONLY.union(OFlags::NOFOLLOW).union(OFlags::CLOEXEC);
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
/// mismatch, filesystem drift, unreadable object, or invalid typed inventory construction.
pub fn observe_blender_content_store(
    root: &Path,
    snapshot: &BlenderProjectSnapshot,
) -> Result<BlenderRemoteInventory, BlenderContentStoreObservationError> {
    let root = BoundStoreRoot::open(root)?;
    let Some(objects_directory) = open_directory(&root.fd, "objects")? else {
        root.revalidate()?;
        return empty_inventory();
    };
    let Some(version_directory) = open_directory(&objects_directory, "v1")? else {
        root.revalidate()?;
        return empty_inventory();
    };
    let Some(digest_directory) = open_directory(&version_directory, "sha256")? else {
        root.revalidate()?;
        return empty_inventory();
    };

    let required = required_objects(snapshot)?;
    let mut present = Vec::new();
    for (digest, expected_bytes) in required {
        let key = BlenderContentObjectKey::from_digest(&digest);
        let (bucket, object_name) = object_key_components(&key)?;
        let Some(bucket_directory) = open_directory(&digest_directory, bucket)? else {
            continue;
        };
        let Some(mut object) = open_object(&bucket_directory, object_name)? else {
            continue;
        };
        let before = file_snapshot(object.as_fd())?;
        if before.bytes != expected_bytes {
            return Err(size_mismatch());
        }
        let observed_digest = hash_held_file(&mut object, expected_bytes)?;
        if observed_digest != digest {
            return Err(content_mismatch());
        }
        let after = file_snapshot(object.as_fd())?;
        if before != after {
            return Err(changed());
        }
        let rebound = open_object_required(&bucket_directory, object_name)?;
        let rebound_snapshot = file_snapshot(rebound.as_fd())?;
        if rebound_snapshot != before {
            return Err(changed());
        }
        present.push(
            BlenderContentObject::new(digest, expected_bytes).map_err(|_| invalid_inventory())?,
        );
    }
    root.revalidate()?;
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

fn open_directory(
    parent: &OwnedFd,
    name: &str,
) -> Result<Option<OwnedFd>, BlenderContentStoreObservationError> {
    match rustix_fs::openat(parent.as_fd(), name, DIRECTORY_FLAGS, Mode::empty()) {
        Ok(directory) => {
            let stat = rustix_fs::fstat(directory.as_fd()).map_err(|_| unreadable())?;
            if !FileType::from_raw_mode(stat.st_mode).is_dir() {
                return Err(unsafe_node());
            }
            Ok(Some(directory))
        }
        Err(Errno::NOENT) => Ok(None),
        Err(_) => Err(unsafe_node()),
    }
}

fn open_object(
    parent: &OwnedFd,
    name: &str,
) -> Result<Option<File>, BlenderContentStoreObservationError> {
    match rustix_fs::openat(parent.as_fd(), name, FILE_FLAGS, Mode::empty()) {
        Ok(fd) => {
            let stat = rustix_fs::fstat(fd.as_fd()).map_err(|_| unreadable())?;
            if !FileType::from_raw_mode(stat.st_mode).is_file() {
                return Err(unsafe_node());
            }
            Ok(Some(File::from(fd)))
        }
        Err(Errno::NOENT) => Ok(None),
        Err(_) => Err(unsafe_node()),
    }
}

fn open_object_required(
    parent: &OwnedFd,
    name: &str,
) -> Result<File, BlenderContentStoreObservationError> {
    open_object(parent, name)?.ok_or_else(changed)
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
        links: stat.st_nlink,
        bytes: u64::try_from(stat.st_size).map_err(|_| unsafe_node())?,
        mtime: stat.st_mtime,
        mtime_nsec: i64::try_from(stat.st_mtime_nsec).map_err(|_| unsafe_node())?,
        ctime: stat.st_ctime,
        ctime_nsec: i64::try_from(stat.st_ctime_nsec).map_err(|_| unsafe_node())?,
    })
}

struct BoundStoreRoot<'a> {
    path: &'a Path,
    fd: OwnedFd,
    device: u64,
    inode: u64,
}

impl<'a> BoundStoreRoot<'a> {
    fn open(path: &'a Path) -> Result<Self, BlenderContentStoreObservationError> {
        if !path.is_absolute()
            || std::fs::canonicalize(path).map_err(|_| unsafe_root())? != path
        {
            return Err(unsafe_root());
        }
        let fd = rustix_fs::open(path, DIRECTORY_FLAGS, Mode::empty()).map_err(|_| unsafe_root())?;
        let stat = rustix_fs::fstat(fd.as_fd()).map_err(|_| unsafe_root())?;
        if !FileType::from_raw_mode(stat.st_mode).is_dir() {
            return Err(unsafe_root());
        }
        Ok(Self {
            path,
            fd,
            device: stat.st_dev,
            inode: stat.st_ino,
        })
    }

    fn revalidate(&self) -> Result<(), BlenderContentStoreObservationError> {
        let held = rustix_fs::fstat(self.fd.as_fd()).map_err(|_| changed())?;
        let rebound = rustix_fs::open(self.path, DIRECTORY_FLAGS, Mode::empty()).map_err(|_| changed())?;
        let current = rustix_fs::fstat(rebound.as_fd()).map_err(|_| changed())?;
        if !FileType::from_raw_mode(held.st_mode).is_dir()
            || !FileType::from_raw_mode(current.st_mode).is_dir()
            || held.st_dev != self.device
            || held.st_ino != self.inode
            || current.st_dev != self.device
            || current.st_ino != self.inode
        {
            return Err(changed());
        }
        Ok(())
    }
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
            let key = BlenderContentObjectKey::from_digest(&digest);
            let path = self.root.join(key.as_str());
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