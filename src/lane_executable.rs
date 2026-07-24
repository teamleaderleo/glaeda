use std::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::lane_command::LaneCommand;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutableVerificationErrorKind {
    Missing,
    Symlink,
    NonRegularFile,
    WrongOwner,
    WritableByNonOwner,
    NotExecutable,
    Metadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecutableVerificationError {
    kind: ExecutableVerificationErrorKind,
    path: PathBuf,
    public_message: String,
}

impl ExecutableVerificationError {
    #[must_use]
    pub fn kind(&self) -> ExecutableVerificationErrorKind {
        self.kind
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.public_message
    }

    fn new(kind: ExecutableVerificationErrorKind, path: &Path, message: impl Into<String>) -> Self {
        Self {
            kind,
            path: path.to_path_buf(),
            public_message: message.into(),
        }
    }
}

impl fmt::Display for ExecutableVerificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.public_message)
    }
}

impl std::error::Error for ExecutableVerificationError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerifiedExecutable {
    path: PathBuf,
    mode: u32,
}

impl VerifiedExecutable {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn mode(&self) -> u32 {
        self.mode
    }
}

/// Verify every executable required by one typed lane command.
///
/// # Errors
///
/// Returns the first bounded verification error when a required executable is missing, symlinked,
/// non-regular, not root-owned, writable by group or others, or lacks executable permission bits.
pub fn verify_lane_command(
    command: &LaneCommand,
) -> Result<Vec<VerifiedExecutable>, ExecutableVerificationError> {
    command
        .required_programs()
        .into_iter()
        .map(verify_executable)
        .collect()
}

/// Verify one reviewed absolute executable path without following a final symlink.
///
/// # Errors
///
/// Returns a bounded verification error when metadata cannot be read or the executable fails the
/// root-owner, regular-file, write-bit, or execute-bit policy.
pub fn verify_executable(path: &Path) -> Result<VerifiedExecutable, ExecutableVerificationError> {
    if !path.is_absolute() {
        return Err(ExecutableVerificationError::new(
            ExecutableVerificationErrorKind::Metadata,
            path,
            "reviewed executable path is not absolute",
        ));
    }

    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(ExecutableVerificationError::new(
                ExecutableVerificationErrorKind::Missing,
                path,
                format!("reviewed executable does not exist: {}", path.display()),
            ));
        }
        Err(_) => {
            return Err(ExecutableVerificationError::new(
                ExecutableVerificationErrorKind::Metadata,
                path,
                format!("could not inspect reviewed executable: {}", path.display()),
            ));
        }
    };

    let object_kind = if metadata.file_type().is_symlink() {
        ObservedObjectKind::Symlink
    } else if metadata.is_file() {
        ObservedObjectKind::RegularFile
    } else {
        ObservedObjectKind::Other
    };
    verify_observation(path, object_kind, metadata.uid(), metadata.mode() & 0o7777)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObservedObjectKind {
    RegularFile,
    Symlink,
    Other,
}

fn verify_observation(
    path: &Path,
    object_kind: ObservedObjectKind,
    owner_uid: u32,
    mode: u32,
) -> Result<VerifiedExecutable, ExecutableVerificationError> {
    match object_kind {
        ObservedObjectKind::Symlink => {
            return Err(ExecutableVerificationError::new(
                ExecutableVerificationErrorKind::Symlink,
                path,
                format!("reviewed executable is a symlink: {}", path.display()),
            ));
        }
        ObservedObjectKind::Other => {
            return Err(ExecutableVerificationError::new(
                ExecutableVerificationErrorKind::NonRegularFile,
                path,
                format!(
                    "reviewed executable is not a regular file: {}",
                    path.display()
                ),
            ));
        }
        ObservedObjectKind::RegularFile => {}
    }
    if owner_uid != 0 {
        return Err(ExecutableVerificationError::new(
            ExecutableVerificationErrorKind::WrongOwner,
            path,
            format!(
                "reviewed executable is not owned by root: {}",
                path.display()
            ),
        ));
    }
    if mode & 0o022 != 0 {
        return Err(ExecutableVerificationError::new(
            ExecutableVerificationErrorKind::WritableByNonOwner,
            path,
            format!(
                "reviewed executable is writable by group or others: {}",
                path.display()
            ),
        ));
    }
    if mode & 0o111 == 0 {
        return Err(ExecutableVerificationError::new(
            ExecutableVerificationErrorKind::NotExecutable,
            path,
            format!("reviewed executable lacks execute bits: {}", path.display()),
        ));
    }
    Ok(VerifiedExecutable {
        path: path.to_path_buf(),
        mode,
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::journal::{ExecutionLane, PlannedMutation, Preconditions, RollbackClass};
    use crate::lane_command::{LaneCommand, LinuxAccountName, RunnerUserContext};

    use super::{
        ExecutableVerificationErrorKind, ObservedObjectKind, verify_lane_command,
        verify_observation,
    };

    fn action() -> PlannedMutation {
        PlannedMutation::new(
            "inspect-runner-user",
            ExecutionLane::RunnerUser,
            "inspect runner-user tools",
            RollbackClass::Reversible,
            Preconditions::new(["runner user inspected"]),
        )
    }

    #[test]
    fn pure_evidence_accepts_only_root_owned_nonwritable_executables() {
        let verified = verify_observation(
            Path::new("/usr/bin/example"),
            ObservedObjectKind::RegularFile,
            0,
            0o755,
        )
        .expect("valid executable evidence");
        assert_eq!(verified.path(), Path::new("/usr/bin/example"));
        assert_eq!(verified.mode(), 0o755);

        for (kind, uid, mode, expected) in [
            (
                ObservedObjectKind::Symlink,
                0,
                0o755,
                ExecutableVerificationErrorKind::Symlink,
            ),
            (
                ObservedObjectKind::Other,
                0,
                0o755,
                ExecutableVerificationErrorKind::NonRegularFile,
            ),
            (
                ObservedObjectKind::RegularFile,
                1000,
                0o755,
                ExecutableVerificationErrorKind::WrongOwner,
            ),
            (
                ObservedObjectKind::RegularFile,
                0,
                0o775,
                ExecutableVerificationErrorKind::WritableByNonOwner,
            ),
            (
                ObservedObjectKind::RegularFile,
                0,
                0o644,
                ExecutableVerificationErrorKind::NotExecutable,
            ),
        ] {
            let error = verify_observation(Path::new("/usr/bin/example"), kind, uid, mode)
                .expect_err("invalid executable evidence");
            assert_eq!(error.kind(), expected);
        }
    }

    #[test]
    fn runner_git_command_verifies_outer_and_inner_reviewed_programs_when_present() {
        if !Path::new("/usr/sbin/runuser").exists() || !Path::new("/usr/bin/git").exists() {
            return;
        }
        let runner = RunnerUserContext::new(
            LinuxAccountName::parse("project-runner").expect("runner name"),
            1001,
            1001,
            "/srv/runner",
        )
        .expect("runner context");
        let command = LaneCommand::runner_git_version(&action(), &runner).expect("git command");
        let verified = verify_lane_command(&command).expect("verify reviewed programs");
        assert_eq!(verified.len(), 3);
        assert_eq!(verified[0].path(), Path::new("/usr/sbin/runuser"));
        assert_eq!(verified[1].path(), Path::new("/usr/bin/env"));
        assert_eq!(verified[2].path(), Path::new("/usr/bin/git"));
    }
}
