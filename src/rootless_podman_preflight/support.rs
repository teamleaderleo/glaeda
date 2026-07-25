use std::path::{Component, Path, PathBuf};

use rustix::fs::{self as rustix_fs, FileType, Mode, OFlags};
use rustix::io::Errno;

use crate::lane_executable::{ExecutableVerificationErrorKind, verify_executable};

use super::RootlessPodmanPreflightState;

pub(super) fn verify_reviewed_executable(path: &Path) -> ExecutableProbe {
    match verify_executable(path) {
        Ok(verified) => ExecutableProbe {
            state: RootlessPodmanPreflightState::Matching,
            evidence: vec![format!(
                "reviewed executable {} is a protected root-owned file with mode {:04o}",
                verified.path().display(),
                verified.mode()
            )],
        },
        Err(error) => {
            let state = match error.kind() {
                ExecutableVerificationErrorKind::Missing => RootlessPodmanPreflightState::Absent,
                ExecutableVerificationErrorKind::Metadata => RootlessPodmanPreflightState::Unknown,
                ExecutableVerificationErrorKind::Symlink
                | ExecutableVerificationErrorKind::NonRegularFile
                | ExecutableVerificationErrorKind::WrongOwner
                | ExecutableVerificationErrorKind::WritableByNonOwner
                | ExecutableVerificationErrorKind::NotExecutable => {
                    RootlessPodmanPreflightState::Conflicting
                }
            };
            ExecutableProbe {
                state,
                evidence: vec![error.message().to_owned()],
            }
        }
    }
}

pub(super) fn canonical_non_root_path(path: PathBuf) -> Result<PathBuf, String> {
    let Some(value) = path.to_str() else {
        return Err("runtime root must be valid UTF-8".to_owned());
    };
    if value.is_empty()
        || value == "/"
        || value.len() > 4_096
        || value.ends_with('/')
        || value.chars().any(char::is_control)
        || !path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err("runtime root must be a canonical non-root absolute path".to_owned());
    }
    Ok(path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RuntimeIdentity {
    pub(super) uid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExecutableProbe {
    pub(super) state: RootlessPodmanPreflightState,
    pub(super) evidence: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RuntimePathKind {
    Directory,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RuntimePathMetadata {
    pub(super) kind: RuntimePathKind,
    pub(super) uid: u32,
    pub(super) mode: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RuntimePathObservation {
    Missing,
    Present(RuntimePathMetadata),
    Unknown,
}

pub(super) trait RuntimeFilesystem {
    fn inspect(&self, path: &Path) -> RuntimePathObservation;
}

pub(super) struct LinuxRuntimeFilesystem;

impl RuntimeFilesystem for LinuxRuntimeFilesystem {
    fn inspect(&self, path: &Path) -> RuntimePathObservation {
        let descriptor = match open_traversed(path) {
            Ok(descriptor) => descriptor,
            Err(Errno::NOENT) => return RuntimePathObservation::Missing,
            Err(_) => return RuntimePathObservation::Unknown,
        };
        let stat = match rustix_fs::fstat(&descriptor) {
            Ok(stat) => stat,
            Err(_) => return RuntimePathObservation::Unknown,
        };
        let kind = match FileType::from_raw_mode(stat.st_mode) {
            FileType::Directory => RuntimePathKind::Directory,
            _ => RuntimePathKind::Other,
        };
        RuntimePathObservation::Present(RuntimePathMetadata {
            kind,
            uid: stat.st_uid,
            mode: stat.st_mode & 0o7777,
        })
    }
}

fn open_traversed(path: &Path) -> Result<std::os::fd::OwnedFd, Errno> {
    let mut components = path.components();
    if components.next() != Some(Component::RootDir) {
        return Err(Errno::INVAL);
    }
    let mut current = rustix_fs::open(
        "/",
        OFlags::PATH.union(OFlags::DIRECTORY).union(OFlags::CLOEXEC),
        Mode::empty(),
    )?;
    let mut remaining = components.peekable();
    while let Some(component) = remaining.next() {
        let Component::Normal(name) = component else {
            return Err(Errno::INVAL);
        };
        let flags = if remaining.peek().is_some() {
            OFlags::PATH
                .union(OFlags::DIRECTORY)
                .union(OFlags::NOFOLLOW)
                .union(OFlags::CLOEXEC)
        } else {
            OFlags::PATH.union(OFlags::NOFOLLOW).union(OFlags::CLOEXEC)
        };
        current = rustix_fs::openat(&current, name, flags, Mode::empty())?;
    }
    Ok(current)
}
