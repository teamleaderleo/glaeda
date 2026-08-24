use std::ffi::OsStr;
use std::fs::File;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::FileExt as _;
use std::path::{Component, Path};

use rustix::fs::{self, AtFlags, Dir, FileType, Mode, OFlags};

use crate::receipt::{
    MAX_PATH_BYTES, MAX_PROJECT_DISK_RECEIPT_ENTRY_COUNT,
    MAX_PROJECT_DISK_RECEIPT_SMALL_FILE_BYTES, ProjectDiskDirectoryEvidenceDocument,
    ReceiptDirectoryEntryEvidence, ReceiptDirectoryEntryKind, ReceiptFilesystemSnapshot,
    same_entry_binding,
};

use super::{
    ProjectDiskPhysicalCaptureError, changed, encode_hex, filesystem_error, invalid_request,
};

const ALLOCATION_BLOCK_BYTES: u64 = 512;
const MAX_SYMLINK_TARGET_BYTES: usize = MAX_PATH_BYTES;
const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const REGULAR_FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC);

pub(super) struct HeldDiskDirectory {
    directory: OwnedFd,
    before: ReceiptFilesystemSnapshot,
    entries: Vec<HeldEntry>,
}

impl HeldDiskDirectory {
    pub(super) fn finish(
        &mut self,
    ) -> Result<ProjectDiskDirectoryEvidenceDocument, ProjectDiskPhysicalCaptureError> {
        let mut entries = Vec::with_capacity(self.entries.len());
        for held in &mut self.entries {
            entries.push(held.finish(&self.directory)?);
        }

        let after_names = read_directory_entry_names(&self.directory)?;
        if !after_names
            .iter()
            .map(Vec::as_slice)
            .eq(self.entries.iter().map(|entry| entry.name.as_slice()))
        {
            return Err(changed());
        }
        let after = snapshot(&fs::fstat(&self.directory).map_err(|_| filesystem_error())?)?;
        if self.before != after {
            return Err(changed());
        }

        Ok(ProjectDiskDirectoryEvidenceDocument {
            path: String::new(),
            before: self.before,
            entries,
            after_entry_names_hex: after_names.iter().map(|name| encode_hex(name)).collect(),
            after,
        })
    }
}

struct HeldEntry {
    name: Vec<u8>,
    kind: ReceiptDirectoryEntryKind,
    before: ReceiptFilesystemSnapshot,
    symlink_target: Option<Vec<u8>>,
    small_regular_file: Option<Vec<u8>>,
    descriptor: Option<OwnedFd>,
}

impl HeldEntry {
    fn finish(
        &mut self,
        directory: &OwnedFd,
    ) -> Result<ReceiptDirectoryEntryEvidence, ProjectDiskPhysicalCaptureError> {
        let name = OsStr::from_bytes(&self.name);
        let after_stat = fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| filesystem_error())?;
        let after = snapshot(&after_stat)?;
        if !stable_entry_observation(self.kind, &self.before, &after) {
            return Err(changed());
        }
        if let Some(descriptor) = &self.descriptor {
            let held_after = snapshot(&fs::fstat(descriptor).map_err(|_| filesystem_error())?)?;
            if !stable_entry_observation(self.kind, &self.before, &held_after) {
                return Err(changed());
            }
        }
        if self.kind == ReceiptDirectoryEntryKind::Symlink {
            let target = fs::readlinkat(
                directory,
                name,
                Vec::with_capacity(MAX_SYMLINK_TARGET_BYTES),
            )
            .map_err(|_| filesystem_error())?;
            if Some(target.as_bytes()) != self.symlink_target.as_deref() {
                return Err(changed());
            }
        }
        if self.kind == ReceiptDirectoryEntryKind::Regular {
            if let (Some(expected), Some(descriptor)) = (&self.small_regular_file, &self.descriptor)
            {
                let actual = read_small_regular_file(descriptor, self.before.logical_bytes)?;
                if &actual != expected {
                    return Err(changed());
                }
            }
        }

        Ok(ReceiptDirectoryEntryEvidence {
            name_hex: encode_hex(&self.name),
            name_utf8: std::str::from_utf8(&self.name).ok().map(str::to_owned),
            kind: self.kind,
            before: self.before,
            symlink_target_hex: self.symlink_target.as_deref().map(encode_hex),
            small_regular_file_hex: self.small_regular_file.as_deref().map(encode_hex),
            after,
        })
    }
}

pub(super) fn capture_held_disk_directory(
    directory: OwnedFd,
) -> Result<HeldDiskDirectory, ProjectDiskPhysicalCaptureError> {
    let before = snapshot(&fs::fstat(&directory).map_err(|_| filesystem_error())?)?;
    if FileType::from_raw_mode(before.mode.try_into().map_err(|_| filesystem_error())?)
        != FileType::Directory
    {
        return Err(filesystem_error());
    }

    let names = read_directory_entry_names(&directory)?;
    let mut entries = Vec::with_capacity(names.len());
    for name in names {
        entries.push(capture_held_entry(&directory, &name)?);
    }
    Ok(HeldDiskDirectory {
        directory,
        before,
        entries,
    })
}

fn read_directory_entry_names(
    directory: &OwnedFd,
) -> Result<Vec<Vec<u8>>, ProjectDiskPhysicalCaptureError> {
    let mut stream = Dir::read_from(directory).map_err(|_| filesystem_error())?;
    let mut names = Vec::new();
    for entry in &mut stream {
        let entry = entry.map_err(|_| filesystem_error())?;
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        if names.len() >= MAX_PROJECT_DISK_RECEIPT_ENTRY_COUNT
            || name.is_empty()
            || name.len() > 255
            || name.contains(&0)
            || name.contains(&b'/')
        {
            return Err(filesystem_error());
        }
        names.push(name.to_vec());
    }
    if names.is_empty() {
        return Err(filesystem_error());
    }
    names.sort();
    if names.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(filesystem_error());
    }
    Ok(names)
}

fn capture_held_entry(
    directory: &OwnedFd,
    name: &[u8],
) -> Result<HeldEntry, ProjectDiskPhysicalCaptureError> {
    let name_os = OsStr::from_bytes(name);
    let before_stat = fs::statat(directory, name_os, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| filesystem_error())?;
    let before = snapshot(&before_stat)?;
    let file_type =
        FileType::from_raw_mode(before.mode.try_into().map_err(|_| filesystem_error())?);
    match file_type {
        FileType::RegularFile => {
            let descriptor = fs::openat(directory, name_os, REGULAR_FILE_FLAGS, Mode::empty())
                .map_err(|_| filesystem_error())?;
            let held = snapshot(&fs::fstat(&descriptor).map_err(|_| filesystem_error())?)?;
            if !stable_entry_observation(ReceiptDirectoryEntryKind::Regular, &before, &held) {
                return Err(changed());
            }
            let small_regular_file = if before.logical_bytes
                <= u64::try_from(MAX_PROJECT_DISK_RECEIPT_SMALL_FILE_BYTES)
                    .map_err(|_| filesystem_error())?
            {
                Some(read_small_regular_file(&descriptor, before.logical_bytes)?)
            } else {
                None
            };
            Ok(HeldEntry {
                name: name.to_vec(),
                kind: ReceiptDirectoryEntryKind::Regular,
                before,
                symlink_target: None,
                small_regular_file,
                descriptor: Some(descriptor),
            })
        }
        FileType::Directory => {
            let descriptor = fs::openat(directory, name_os, DIRECTORY_FLAGS, Mode::empty())
                .map_err(|_| filesystem_error())?;
            let held = snapshot(&fs::fstat(&descriptor).map_err(|_| filesystem_error())?)?;
            if !stable_entry_observation(ReceiptDirectoryEntryKind::Directory, &before, &held) {
                return Err(changed());
            }
            Ok(HeldEntry {
                name: name.to_vec(),
                kind: ReceiptDirectoryEntryKind::Directory,
                before,
                symlink_target: None,
                small_regular_file: None,
                descriptor: Some(descriptor),
            })
        }
        FileType::Symlink => {
            let target = fs::readlinkat(
                directory,
                name_os,
                Vec::with_capacity(MAX_SYMLINK_TARGET_BYTES),
            )
            .map_err(|_| filesystem_error())?;
            if target.as_bytes().is_empty()
                || target.as_bytes().len() > MAX_SYMLINK_TARGET_BYTES
                || u64::try_from(target.as_bytes().len()).map_err(|_| filesystem_error())?
                    != before.logical_bytes
            {
                return Err(filesystem_error());
            }
            let after_readlink = snapshot(
                &fs::statat(directory, name_os, AtFlags::SYMLINK_NOFOLLOW)
                    .map_err(|_| filesystem_error())?,
            )?;
            if !stable_entry_observation(
                ReceiptDirectoryEntryKind::Symlink,
                &before,
                &after_readlink,
            ) {
                return Err(changed());
            }
            Ok(HeldEntry {
                name: name.to_vec(),
                kind: ReceiptDirectoryEntryKind::Symlink,
                before,
                symlink_target: Some(target.as_bytes().to_vec()),
                small_regular_file: None,
                descriptor: None,
            })
        }
        _ => Err(filesystem_error()),
    }
}

fn stable_entry_observation(
    kind: ReceiptDirectoryEntryKind,
    before: &ReceiptFilesystemSnapshot,
    after: &ReceiptFilesystemSnapshot,
) -> bool {
    match kind {
        ReceiptDirectoryEntryKind::Regular => same_entry_binding(before, after),
        ReceiptDirectoryEntryKind::Directory | ReceiptDirectoryEntryKind::Symlink => {
            before == after
        }
    }
}

fn read_small_regular_file(
    descriptor: &OwnedFd,
    logical_bytes: u64,
) -> Result<Vec<u8>, ProjectDiskPhysicalCaptureError> {
    let length = usize::try_from(logical_bytes).map_err(|_| filesystem_error())?;
    if length > MAX_PROJECT_DISK_RECEIPT_SMALL_FILE_BYTES {
        return Err(filesystem_error());
    }
    let mut bytes = vec![0_u8; length];
    if length == 0 {
        return Ok(bytes);
    }
    let file = File::from(rustix::io::dup(descriptor).map_err(|_| filesystem_error())?);
    file.read_exact_at(&mut bytes, 0)
        .map_err(|_| filesystem_error())?;
    Ok(bytes)
}

pub(super) fn open_absolute_directory(
    path: &Path,
) -> Result<OwnedFd, ProjectDiskPhysicalCaptureError> {
    let mut current =
        fs::open(Path::new("/"), DIRECTORY_FLAGS, Mode::empty()).map_err(|_| filesystem_error())?;
    let mut components = path.components();
    if components.next() != Some(Component::RootDir) {
        return Err(invalid_request());
    }
    for component in components {
        let Component::Normal(name) = component else {
            return Err(invalid_request());
        };
        current = fs::openat(&current, name, DIRECTORY_FLAGS, Mode::empty())
            .map_err(|_| filesystem_error())?;
    }
    Ok(current)
}

pub(super) fn open_relative_directory(
    base: &OwnedFd,
    path: &Path,
) -> Result<OwnedFd, ProjectDiskPhysicalCaptureError> {
    let mut current: Option<OwnedFd> = None;
    for component in path.components() {
        let Component::Normal(name) = component else {
            return Err(invalid_request());
        };
        let opened = match &current {
            Some(parent) => fs::openat(parent, name, DIRECTORY_FLAGS, Mode::empty()),
            None => fs::openat(base, name, DIRECTORY_FLAGS, Mode::empty()),
        }
        .map_err(|_| filesystem_error())?;
        current = Some(opened);
    }
    current.ok_or_else(invalid_request)
}

pub(super) fn snapshot(
    stat: &rustix::fs::Stat,
) -> Result<ReceiptFilesystemSnapshot, ProjectDiskPhysicalCaptureError> {
    let logical_bytes = u64::try_from(stat.st_size).map_err(|_| filesystem_error())?;
    let blocks = u64::try_from(stat.st_blocks).map_err(|_| filesystem_error())?;
    let allocated_bytes = blocks
        .checked_mul(ALLOCATION_BLOCK_BYTES)
        .ok_or_else(filesystem_error)?;
    Ok(ReceiptFilesystemSnapshot {
        device: u64::try_from(stat.st_dev).map_err(|_| filesystem_error())?,
        inode: u64::try_from(stat.st_ino).map_err(|_| filesystem_error())?,
        uid: u32::try_from(stat.st_uid).map_err(|_| filesystem_error())?,
        gid: u32::try_from(stat.st_gid).map_err(|_| filesystem_error())?,
        mode: u32::try_from(stat.st_mode).map_err(|_| filesystem_error())?,
        links: u64::try_from(stat.st_nlink).map_err(|_| filesystem_error())?,
        logical_bytes,
        allocated_bytes,
        mtime_seconds: i64::try_from(stat.st_mtime).map_err(|_| filesystem_error())?,
        mtime_nanoseconds: i64::try_from(stat.st_mtime_nsec).map_err(|_| filesystem_error())?,
        ctime_seconds: i64::try_from(stat.st_ctime).map_err(|_| filesystem_error())?,
        ctime_nanoseconds: i64::try_from(stat.st_ctime_nsec).map_err(|_| filesystem_error())?,
    })
}
