use std::fmt;
use std::path::Path;

use serde::Serialize;

use crate::installation_id::generate_installation_id;
use crate::linux_installation_catalog::{InstallationLookup, find_locked_installation};
use crate::linux_installation_catalog_lock::lock_installation_catalog;
use crate::linux_installation_publication::{
    InstallationPublicationError, InstallationPublicationErrorKind, publish_new_installation,
};
use crate::ownership::ProjectIdentity;
use crate::state::{InstallationId, STATE_ROOT};
use crate::state_store::{StateStoreError, StateStoreErrorKind};

const INSTALLATION_ID_ATTEMPTS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallationEnrollmentDisposition {
    Created,
    Existing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstallationEnrollmentReceipt {
    installation_id: InstallationId,
    disposition: InstallationEnrollmentDisposition,
}

impl InstallationEnrollmentReceipt {
    #[must_use]
    pub fn installation_id(&self) -> &InstallationId {
        &self.installation_id
    }

    #[must_use]
    pub const fn disposition(&self) -> InstallationEnrollmentDisposition {
        self.disposition
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallationEnrollmentErrorKind {
    Busy,
    Conflict,
    InvalidProject,
    Io,
    UnsafeFilesystem,
    CorruptState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstallationEnrollmentError {
    kind: InstallationEnrollmentErrorKind,
    public_message: String,
}

impl InstallationEnrollmentError {
    fn new(kind: InstallationEnrollmentErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            public_message: message.into(),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> InstallationEnrollmentErrorKind {
        self.kind
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.public_message
    }
}

impl fmt::Display for InstallationEnrollmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.public_message)
    }
}

impl std::error::Error for InstallationEnrollmentError {}

/// Create or load one project installation beneath the canonical state root.
///
/// # Errors
///
/// Returns a bounded error for catalog contention, invalid project identity, unsafe or corrupt
/// state, operating-system randomness failure, or filesystem failure.
pub fn create_or_load_default_installation(
    project: ProjectIdentity,
) -> Result<InstallationEnrollmentReceipt, InstallationEnrollmentError> {
    create_or_load_installation(STATE_ROOT, project)
}

/// Create or load one project installation beneath an existing trusted state root.
///
/// The function holds one nonblocking catalog lock across exact project lookup and any new
/// installation publication. An existing exact project returns its stable installation ID. Missing
/// state generates a fresh random ID and publishes a complete staged installation atomically. ID
/// collisions are retried without replacing either staged or published state.
///
/// # Errors
///
/// Returns a bounded error for catalog contention, invalid project identity, unsafe or corrupt
/// state, operating-system randomness failure, or filesystem failure.
pub fn create_or_load_installation(
    root_path: impl AsRef<Path>,
    project: ProjectIdentity,
) -> Result<InstallationEnrollmentReceipt, InstallationEnrollmentError> {
    let catalog_lock = lock_installation_catalog(root_path).map_err(map_store_error)?;
    match find_locked_installation(&catalog_lock, &project).map_err(map_store_error)? {
        InstallationLookup::Found(installation_id) => {
            return Ok(InstallationEnrollmentReceipt {
                installation_id,
                disposition: InstallationEnrollmentDisposition::Existing,
            });
        }
        InstallationLookup::Missing => {}
    }

    for _ in 0..INSTALLATION_ID_ATTEMPTS {
        let installation_id = generate_installation_id().map_err(|_| {
            InstallationEnrollmentError::new(
                InstallationEnrollmentErrorKind::Io,
                "could not generate an installation ID",
            )
        })?;
        match publish_new_installation(&catalog_lock, installation_id.clone(), project.clone()) {
            Ok(_) => {
                return Ok(InstallationEnrollmentReceipt {
                    installation_id,
                    disposition: InstallationEnrollmentDisposition::Created,
                });
            }
            Err(error) if error.kind() == InstallationPublicationErrorKind::IdCollision => {}
            Err(error) => return Err(map_publication_error(error)),
        }
    }

    Err(InstallationEnrollmentError::new(
        InstallationEnrollmentErrorKind::Io,
        "could not allocate a unique installation ID",
    ))
}

fn map_store_error(error: StateStoreError) -> InstallationEnrollmentError {
    let kind = match error.kind() {
        StateStoreErrorKind::Busy => InstallationEnrollmentErrorKind::Busy,
        StateStoreErrorKind::Conflict => InstallationEnrollmentErrorKind::Conflict,
        StateStoreErrorKind::Io => InstallationEnrollmentErrorKind::Io,
        StateStoreErrorKind::UnsafeFilesystem => InstallationEnrollmentErrorKind::UnsafeFilesystem,
        StateStoreErrorKind::CorruptState => InstallationEnrollmentErrorKind::CorruptState,
    };
    InstallationEnrollmentError::new(kind, error.message())
}

fn map_publication_error(error: InstallationPublicationError) -> InstallationEnrollmentError {
    let kind = match error.kind() {
        InstallationPublicationErrorKind::IdCollision => InstallationEnrollmentErrorKind::Io,
        InstallationPublicationErrorKind::InvalidProject => {
            InstallationEnrollmentErrorKind::InvalidProject
        }
        InstallationPublicationErrorKind::Io => InstallationEnrollmentErrorKind::Io,
        InstallationPublicationErrorKind::UnsafeFilesystem => {
            InstallationEnrollmentErrorKind::UnsafeFilesystem
        }
        InstallationPublicationErrorKind::CorruptState => {
            InstallationEnrollmentErrorKind::CorruptState
        }
    };
    InstallationEnrollmentError::new(kind, error.message())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::linux_installation_catalog::{InstallationLookup, find_installation};
    use crate::linux_installation_catalog_lock::lock_installation_catalog;
    use crate::manifest::RunnerScope;
    use crate::ownership::ProjectIdentity;

    use super::{
        InstallationEnrollmentDisposition, InstallationEnrollmentErrorKind,
        create_or_load_installation,
    };

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(label: &str) -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "smolrunner-installation-enrollment-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create temporary state root");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o750))
                .expect("set state-root mode");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn project(repository: &str) -> ProjectIdentity {
        ProjectIdentity {
            repository: repository.to_owned(),
            runner_scope: RunnerScope::Repository,
            runner_user: "project-runner".to_owned(),
        }
    }

    #[test]
    fn first_enrollment_creates_and_second_loads_the_same_installation() {
        let root = TempRoot::new("create-load");
        let project = project("example/project");

        let first = create_or_load_installation(root.path(), project.clone()).expect("enroll project");
        assert_eq!(
            first.disposition(),
            InstallationEnrollmentDisposition::Created
        );
        let second = create_or_load_installation(root.path(), project).expect("load project");
        assert_eq!(
            second.disposition(),
            InstallationEnrollmentDisposition::Existing
        );
        assert_eq!(second.installation_id(), first.installation_id());
        assert_eq!(
            find_installation(root.path(), &project("example/project"))
                .expect("find installation"),
            InstallationLookup::Found(first.installation_id().clone())
        );
    }

    #[test]
    fn held_catalog_lock_maps_to_bounded_busy_error() {
        let root = TempRoot::new("busy");
        let _lock = lock_installation_catalog(root.path()).expect("hold catalog lock");
        let error = create_or_load_installation(root.path(), project("example/project"))
            .expect_err("second lock must fail");
        assert_eq!(error.kind(), InstallationEnrollmentErrorKind::Busy);
        assert_eq!(error.message(), "another installation catalog operation is in progress");
    }
}
