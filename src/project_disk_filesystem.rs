//! Pure identity vocabulary for one formatted project-disk filesystem generation.
//!
//! This module performs no persistence, formatter execution, filesystem probing, mount operation,
//! or proof minting. P4 will use these values to distinguish one accepted filesystem generation
//! from another even when the surrounding project-disk generation remains unchanged.

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

/// Declared identity for one accepted formatted filesystem generation on one project-disk
/// generation.
///
/// This value is declaration data only. P4 must establish it through durable pre-format authority
/// plus fresh post-format guest observation before any later correlation treats it as current.
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
    /// Construct one filesystem identity directly from the pre-format logical project-disk
    /// generation.
    ///
    /// This is the P3 -> P4 constructor. It deliberately does not require a
    /// [`ProjectDiskLeaseRecord`], because P1 models an already-formatted disk and therefore cannot
    /// legitimately exist until P4 has completed format and post-format observation.
    #[must_use]
    pub fn new_for_project_disk(
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

    /// Convenience constructor for an already-formatted disk that already has a valid P1 lease.
    ///
    /// New P4 format flows must use [`Self::new_for_project_disk`] before `mkfs`; this constructor is
    /// retained for compatibility with existing read-only consumers and tests.
    #[must_use]
    pub fn new(
        record: &ProjectDiskLeaseRecord,
        filesystem_generation: ProjectDiskFilesystemGeneration,
        format_profile_generation: ProjectDiskFilesystemFormatProfileGeneration,
        kind: ProjectDiskFilesystemKind,
    ) -> Self {
        Self::new_for_project_disk(
            record.project().clone(),
            record.disk_id().clone(),
            record.disk_generation(),
            filesystem_generation,
            format_profile_generation,
            kind,
        )
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

    /// Return whether this declaration names an exact logical project-disk generation.
    #[must_use]
    pub fn matches_project_disk_identity(
        &self,
        project: &ProjectIdentity,
        disk_id: &ProjectDiskId,
        disk_generation: ProjectDiskGeneration,
    ) -> bool {
        project == &self.project
            && disk_id == &self.disk_id
            && disk_generation == self.disk_generation
    }

    /// Return whether this declaration still names the same logical project-disk generation.
    ///
    /// Lease revision and attachment state are deliberately excluded: those are separately current
    /// authority and can change while the same formatted filesystem generation remains resident.
    #[must_use]
    pub fn matches_project_disk(&self, record: &ProjectDiskLeaseRecord) -> bool {
        self.matches_project_disk_identity(
            record.project(),
            record.disk_id(),
            record.disk_generation(),
        )
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

    fn record(disk: &str, generation: u64) -> ProjectDiskLeaseRecord {
        ProjectDiskLeaseRecord::new_detached(
            project(),
            ProjectDiskId::parse(disk).unwrap(),
            ProjectDiskGeneration::new(generation).unwrap(),
        )
    }

    #[test]
    fn generations_must_be_positive() {
        assert!(ProjectDiskFilesystemGeneration::new(0).is_err());
        assert!(ProjectDiskFilesystemFormatProfileGeneration::new(0).is_err());
        assert_eq!(ProjectDiskFilesystemGeneration::new(2).unwrap().get(), 2);
    }

    #[test]
    fn preformat_binding_needs_no_p1_lease() {
        let disk_id = ProjectDiskId::parse("disk-a").unwrap();
        let disk_generation = ProjectDiskGeneration::new(3).unwrap();
        let binding = ProjectDiskFilesystemBinding::new_for_project_disk(
            project(),
            disk_id.clone(),
            disk_generation,
            ProjectDiskFilesystemGeneration::new(7).unwrap(),
            ProjectDiskFilesystemFormatProfileGeneration::new(2).unwrap(),
            ProjectDiskFilesystemKind::Xfs,
        );
        assert!(binding.matches_project_disk_identity(&project(), &disk_id, disk_generation));
        assert_eq!(binding.filesystem_generation().get(), 7);
        assert_eq!(binding.format_profile_generation().get(), 2);
        assert_eq!(binding.kind(), ProjectDiskFilesystemKind::Xfs);
    }

    #[test]
    fn binding_names_exact_project_disk_and_filesystem_generation() {
        let disk = record("disk-a", 3);
        let binding = ProjectDiskFilesystemBinding::new(
            &disk,
            ProjectDiskFilesystemGeneration::new(7).unwrap(),
            ProjectDiskFilesystemFormatProfileGeneration::new(2).unwrap(),
            ProjectDiskFilesystemKind::Xfs,
        );
        assert!(binding.matches_project_disk(&disk));
        assert_eq!(binding.filesystem_generation().get(), 7);
        assert_eq!(binding.format_profile_generation().get(), 2);
        assert_eq!(binding.kind(), ProjectDiskFilesystemKind::Xfs);
        assert!(!binding.matches_project_disk(&record("disk-b", 3)));
        assert!(!binding.matches_project_disk(&record("disk-a", 4)));
    }

    #[test]
    fn reformat_generation_is_distinct_even_on_same_disk_generation() {
        let disk = record("disk-a", 3);
        let first = ProjectDiskFilesystemBinding::new(
            &disk,
            ProjectDiskFilesystemGeneration::new(1).unwrap(),
            ProjectDiskFilesystemFormatProfileGeneration::new(1).unwrap(),
            ProjectDiskFilesystemKind::Ext4,
        );
        let reformatted = ProjectDiskFilesystemBinding::new(
            &disk,
            ProjectDiskFilesystemGeneration::new(2).unwrap(),
            ProjectDiskFilesystemFormatProfileGeneration::new(1).unwrap(),
            ProjectDiskFilesystemKind::Ext4,
        );
        assert_ne!(first, reformatted);
        assert!(first.matches_project_disk(&disk));
        assert!(reformatted.matches_project_disk(&disk));
    }

    #[test]
    fn format_profile_change_is_part_of_identity() {
        let disk = record("disk-a", 3);
        let first = ProjectDiskFilesystemBinding::new(
            &disk,
            ProjectDiskFilesystemGeneration::new(1).unwrap(),
            ProjectDiskFilesystemFormatProfileGeneration::new(1).unwrap(),
            ProjectDiskFilesystemKind::Xfs,
        );
        let changed_profile = ProjectDiskFilesystemBinding::new(
            &disk,
            ProjectDiskFilesystemGeneration::new(1).unwrap(),
            ProjectDiskFilesystemFormatProfileGeneration::new(2).unwrap(),
            ProjectDiskFilesystemKind::Xfs,
        );
        assert_ne!(first, changed_profile);
    }
}
