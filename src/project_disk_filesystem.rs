//! Pure identity vocabulary for one formatted project-disk filesystem generation.
//!
//! This module performs no persistence, formatter execution, filesystem probing, mount operation,
//! or proof minting. P4 chooses these values before formatting so one accepted filesystem
//! generation remains distinct from every later reformat on the same project-disk generation.

use serde::Serialize;

use crate::project_catalog::ProjectIdentity;
use crate::project_disk_lease::{ProjectDiskGeneration, ProjectDiskId, ProjectDiskLeaseRecord};

pub const PROJECT_DISK_FILESYSTEM_SCHEMA_VERSION: u8 = 1;

macro_rules! positive_generation_type {
    ($name:ident, $code:literal, $message:literal) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(u64);

        impl $name {
            /// Construct one positive generation.
            ///
            /// # Errors
            ///
            /// Returns a bounded error when `value` is zero.
            pub fn new(value: u64) -> Result<Self, ProjectDiskFilesystemError> {
                if value == 0 {
                    return Err(ProjectDiskFilesystemError {
                        code: $code,
                        message: $message,
                    });
                }
                Ok(Self(value))
            }

            #[must_use]
            pub const fn get(self) -> u64 {
                self.0
            }
        }
    };
}

positive_generation_type!(
    ProjectDiskFilesystemGeneration,
    "project_disk_filesystem_generation_invalid",
    "project disk filesystem generation must be greater than zero"
);

positive_generation_type!(
    ProjectDiskFilesystemFormatProfileGeneration,
    "project_disk_filesystem_format_profile_generation_invalid",
    "project disk filesystem format-profile generation must be greater than zero"
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDiskFilesystemKind {
    Ext4,
    Xfs,
}

/// Declared identity for one accepted filesystem generation on one project-disk generation.
///
/// P4 constructs this declaration from the logical project/disk generation before `mkfs` crosses
/// its destructive boundary. A later successful format plus fresh physical observation may then
/// create the already-formatted P1 detached lease and compare it with this declaration.
///
/// This value is declaration data only. It carries zero formatter, attachment, mount, ownership,
/// or proof-minting authority by itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiskFilesystemBinding {
    schema_version: u8,
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    filesystem_generation: ProjectDiskFilesystemGeneration,
    format_profile_generation: ProjectDiskFilesystemFormatProfileGeneration,
    kind: ProjectDiskFilesystemKind,
}

impl ProjectDiskFilesystemBinding {
    #[must_use]
    pub fn new(
        project: ProjectIdentity,
        disk_id: ProjectDiskId,
        disk_generation: ProjectDiskGeneration,
        filesystem_generation: ProjectDiskFilesystemGeneration,
        format_profile_generation: ProjectDiskFilesystemFormatProfileGeneration,
        kind: ProjectDiskFilesystemKind,
    ) -> Self {
        Self {
            schema_version: PROJECT_DISK_FILESYSTEM_SCHEMA_VERSION,
            project,
            disk_id,
            disk_generation,
            filesystem_generation,
            format_profile_generation,
            kind,
        }
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn project(&self) -> &ProjectIdentity {
        &self.project
    }

    #[must_use]
    pub const fn disk_id(&self) -> &ProjectDiskId {
        &self.disk_id
    }

    #[must_use]
    pub const fn disk_generation(&self) -> ProjectDiskGeneration {
        self.disk_generation
    }

    #[must_use]
    pub const fn filesystem_generation(&self) -> ProjectDiskFilesystemGeneration {
        self.filesystem_generation
    }

    #[must_use]
    pub const fn format_profile_generation(&self) -> ProjectDiskFilesystemFormatProfileGeneration {
        self.format_profile_generation
    }

    #[must_use]
    pub const fn kind(&self) -> ProjectDiskFilesystemKind {
        self.kind
    }

    /// Return whether a later P1 lease names the same logical project-disk generation.
    ///
    /// This check is intentionally post-declaration. Lease revision and attachment state remain
    /// separate current authority and can change while one accepted formatted filesystem
    /// generation remains resident.
    #[must_use]
    pub fn matches_project_disk(&self, record: &ProjectDiskLeaseRecord) -> bool {
        record.project() == &self.project
            && record.disk_id() == &self.disk_id
            && record.disk_generation() == self.disk_generation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProjectDiskFilesystemError {
    code: &'static str,
    message: &'static str,
}

impl ProjectDiskFilesystemError {
    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl std::fmt::Display for ProjectDiskFilesystemError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ProjectDiskFilesystemError {}

#[cfg(test)]
mod tests {
    use super::{
        ProjectDiskFilesystemBinding, ProjectDiskFilesystemFormatProfileGeneration,
        ProjectDiskFilesystemGeneration, ProjectDiskFilesystemKind,
    };
    use crate::project_catalog::ProjectIdentity;
    use crate::project_disk_lease::{ProjectDiskGeneration, ProjectDiskId, ProjectDiskLeaseRecord};

    fn project() -> ProjectIdentity {
        ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap()
    }

    fn disk_id(value: &str) -> ProjectDiskId {
        ProjectDiskId::parse(value).unwrap()
    }

    fn record(disk: &str, generation: u64) -> ProjectDiskLeaseRecord {
        ProjectDiskLeaseRecord::new_detached(
            project(),
            disk_id(disk),
            ProjectDiskGeneration::new(generation).unwrap(),
        )
    }

    fn binding(
        disk: &str,
        disk_generation: u64,
        filesystem_generation: u64,
        profile_generation: u64,
        kind: ProjectDiskFilesystemKind,
    ) -> ProjectDiskFilesystemBinding {
        ProjectDiskFilesystemBinding::new(
            project(),
            disk_id(disk),
            ProjectDiskGeneration::new(disk_generation).unwrap(),
            ProjectDiskFilesystemGeneration::new(filesystem_generation).unwrap(),
            ProjectDiskFilesystemFormatProfileGeneration::new(profile_generation).unwrap(),
            kind,
        )
    }

    #[test]
    fn generations_must_be_positive() {
        assert!(ProjectDiskFilesystemGeneration::new(0).is_err());
        assert!(ProjectDiskFilesystemFormatProfileGeneration::new(0).is_err());
        assert_eq!(ProjectDiskFilesystemGeneration::new(2).unwrap().get(), 2);
    }

    #[test]
    fn binding_is_declarable_before_p1_detached_lease_exists() {
        let binding = binding("disk-a", 3, 7, 2, ProjectDiskFilesystemKind::Xfs);
        assert_eq!(binding.disk_id().as_str(), "disk-a");
        assert_eq!(binding.disk_generation().get(), 3);
        assert_eq!(binding.filesystem_generation().get(), 7);
        assert_eq!(binding.format_profile_generation().get(), 2);
        assert_eq!(binding.kind(), ProjectDiskFilesystemKind::Xfs);

        let later_formatted_lease = record("disk-a", 3);
        assert!(binding.matches_project_disk(&later_formatted_lease));
        assert!(!binding.matches_project_disk(&record("disk-b", 3)));
        assert!(!binding.matches_project_disk(&record("disk-a", 4)));
    }

    #[test]
    fn reformat_generation_is_distinct_even_on_same_disk_generation() {
        let first = binding("disk-a", 3, 1, 1, ProjectDiskFilesystemKind::Ext4);
        let reformatted = binding("disk-a", 3, 2, 1, ProjectDiskFilesystemKind::Ext4);
        assert_ne!(first, reformatted);
        assert!(first.matches_project_disk(&record("disk-a", 3)));
        assert!(reformatted.matches_project_disk(&record("disk-a", 3)));
    }

    #[test]
    fn format_profile_change_is_part_of_identity() {
        let first = binding("disk-a", 3, 1, 1, ProjectDiskFilesystemKind::Xfs);
        let changed_profile = binding("disk-a", 3, 1, 2, ProjectDiskFilesystemKind::Xfs);
        assert_ne!(first, changed_profile);
    }
}
