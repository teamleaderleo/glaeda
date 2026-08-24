//! Pure sealed identity for one same-writer-lock project-filesystem correlation window.
//!
//! The future durable P3/P4 coordinator will mint this value only while holding the exclusive
//! project-disk mutation/reconciliation guard across fresh host observation, guest correlation,
//! proof minting, and immediate #589 consumption. This slice implements identity/freshness checks
//! only; it acquires no OS lock, performs no persistence, and grants no mutation authority.

use std::fmt;

use serde::Serialize;

use crate::project_catalog::ProjectIdentity;
use crate::project_disk_filesystem::{
    ProjectDiskFilesystemBinding, ProjectDiskFilesystemFormatProfileGeneration,
    ProjectDiskFilesystemGeneration, ProjectDiskFilesystemKind,
};
use crate::project_disk_lease::{
    ProjectDiskAttachmentGeneration, ProjectDiskGeneration, ProjectDiskId, ProjectDiskLeaseRecord,
    ProjectDiskLeaseState, ProjectDiskRevision, ResidentSandboxGeneration, ResidentSandboxId,
};

pub const PROJECT_DISK_CORRELATION_WINDOW_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ProjectDiskCorrelationWindowGeneration(u64);

impl ProjectDiskCorrelationWindowGeneration {
    /// Construct one positive correlation-window generation.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal for zero.
    pub fn new(value: u64) -> Result<Self, ProjectDiskCorrelationWindowError> {
        if value == 0 {
            return Err(invalid_generation());
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiskCorrelationWindowSummary {
    schema_version: u8,
    generation: ProjectDiskCorrelationWindowGeneration,
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    disk_revision: ProjectDiskRevision,
    attachment_generation: ProjectDiskAttachmentGeneration,
    sandbox_id: ResidentSandboxId,
    sandbox_generation: ResidentSandboxGeneration,
    filesystem_generation: ProjectDiskFilesystemGeneration,
    format_profile_generation: ProjectDiskFilesystemFormatProfileGeneration,
    filesystem_kind: ProjectDiskFilesystemKind,
    exclusive_mutation_guard_held: bool,
}

impl ProjectDiskCorrelationWindowSummary {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn generation(&self) -> ProjectDiskCorrelationWindowGeneration {
        self.generation
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
    pub const fn disk_revision(&self) -> ProjectDiskRevision {
        self.disk_revision
    }

    #[must_use]
    pub const fn attachment_generation(&self) -> ProjectDiskAttachmentGeneration {
        self.attachment_generation
    }

    #[must_use]
    pub const fn sandbox_id(&self) -> &ResidentSandboxId {
        &self.sandbox_id
    }

    #[must_use]
    pub const fn sandbox_generation(&self) -> ResidentSandboxGeneration {
        self.sandbox_generation
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
    pub const fn filesystem_kind(&self) -> ProjectDiskFilesystemKind {
        self.filesystem_kind
    }

    #[must_use]
    pub const fn exclusive_mutation_guard_held(&self) -> bool {
        self.exclusive_mutation_guard_held
    }
}

/// Short-lived capability proving the final correlation is executing under one exclusive mutation
/// window for the exact current attachment/filesystem generation.
///
/// This value is deliberately non-serializable and non-cloneable. Production construction remains
/// absent until the durable same-writer-lock coordinator lands.
pub struct ProjectDiskCorrelationWindow {
    summary: ProjectDiskCorrelationWindowSummary,
}

impl ProjectDiskCorrelationWindow {
    #[must_use]
    pub const fn summary(&self) -> &ProjectDiskCorrelationWindowSummary {
        &self.summary
    }

    /// Reconfirm that current durable lease/filesystem authority is unchanged inside the window.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal for any revision, attachment, sandbox, filesystem generation,
    /// profile, kind, or state drift.
    pub fn confirm(
        &self,
        current: &ProjectDiskLeaseRecord,
        filesystem: &ProjectDiskFilesystemBinding,
    ) -> Result<(), ProjectDiskCorrelationWindowError> {
        let ProjectDiskLeaseState::Attached { attachment } = current.state() else {
            return Err(authority_changed());
        };
        if current.project() != &self.summary.project
            || current.disk_id() != &self.summary.disk_id
            || current.disk_generation() != self.summary.disk_generation
            || current.revision() != self.summary.disk_revision
            || attachment.generation() != self.summary.attachment_generation
            || attachment.sandbox_id() != &self.summary.sandbox_id
            || attachment.sandbox_generation() != self.summary.sandbox_generation
            || !filesystem.matches_project_disk(current)
            || filesystem.filesystem_generation() != self.summary.filesystem_generation
            || filesystem.format_profile_generation() != self.summary.format_profile_generation
            || filesystem.kind() != self.summary.filesystem_kind
            || !self.summary.exclusive_mutation_guard_held
        {
            return Err(authority_changed());
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        generation: ProjectDiskCorrelationWindowGeneration,
        current: &ProjectDiskLeaseRecord,
        filesystem: &ProjectDiskFilesystemBinding,
    ) -> Result<Self, ProjectDiskCorrelationWindowError> {
        let ProjectDiskLeaseState::Attached { attachment } = current.state() else {
            return Err(authority_changed());
        };
        if !filesystem.matches_project_disk(current) {
            return Err(authority_changed());
        }
        Ok(Self {
            summary: ProjectDiskCorrelationWindowSummary {
                schema_version: PROJECT_DISK_CORRELATION_WINDOW_SCHEMA_VERSION,
                generation,
                project: current.project().clone(),
                disk_id: current.disk_id().clone(),
                disk_generation: current.disk_generation(),
                disk_revision: current.revision(),
                attachment_generation: attachment.generation(),
                sandbox_id: attachment.sandbox_id().clone(),
                sandbox_generation: attachment.sandbox_generation(),
                filesystem_generation: filesystem.filesystem_generation(),
                format_profile_generation: filesystem.format_profile_generation(),
                filesystem_kind: filesystem.kind(),
                exclusive_mutation_guard_held: true,
            },
        })
    }
}

impl fmt::Debug for ProjectDiskCorrelationWindow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectDiskCorrelationWindow")
            .field("summary", &self.summary)
            .field("lock_identity", &"<private-exclusive-project-disk-guard>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDiskCorrelationWindowErrorKind {
    InvalidGeneration,
    AuthorityChanged,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProjectDiskCorrelationWindowError {
    kind: ProjectDiskCorrelationWindowErrorKind,
    code: &'static str,
    message: &'static str,
}

impl ProjectDiskCorrelationWindowError {
    #[must_use]
    pub const fn kind(self) -> ProjectDiskCorrelationWindowErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for ProjectDiskCorrelationWindowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectDiskCorrelationWindowError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for ProjectDiskCorrelationWindowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ProjectDiskCorrelationWindowError {}

const fn invalid_generation() -> ProjectDiskCorrelationWindowError {
    ProjectDiskCorrelationWindowError {
        kind: ProjectDiskCorrelationWindowErrorKind::InvalidGeneration,
        code: "project_disk_correlation_window_generation_invalid",
        message: "project disk correlation-window generation is invalid",
    }
}

const fn authority_changed() -> ProjectDiskCorrelationWindowError {
    ProjectDiskCorrelationWindowError {
        kind: ProjectDiskCorrelationWindowErrorKind::AuthorityChanged,
        code: "project_disk_correlation_window_authority_changed",
        message: "project disk correlation-window authority changed",
    }
}

#[cfg(test)]
mod tests {
    use super::{ProjectDiskCorrelationWindow, ProjectDiskCorrelationWindowGeneration};
    use crate::project_catalog::ProjectIdentity;
    use crate::project_disk_filesystem::{
        ProjectDiskFilesystemBinding, ProjectDiskFilesystemFormatProfileGeneration,
        ProjectDiskFilesystemGeneration, ProjectDiskFilesystemKind,
    };
    use crate::project_disk_lease::{
        ProjectDiskGeneration, ProjectDiskId, ProjectDiskLeaseRecord, ProjectDiskLockObservation,
        ProjectDiskObservation, ProjectDiskPhysicalObservation, ProjectDiskRecoverability,
        ProjectDiskUseObservation, ResidentSandboxGeneration, ResidentSandboxId,
    };

    const fn detached_exact() -> ProjectDiskObservation {
        ProjectDiskObservation::new(
            ProjectDiskPhysicalObservation::Exact,
            ProjectDiskUseObservation::Unused,
            ProjectDiskLockObservation::Unlocked,
            ProjectDiskRecoverability::Unknown,
        )
    }

    const fn attached_exact() -> ProjectDiskObservation {
        ProjectDiskObservation::new(
            ProjectDiskPhysicalObservation::Exact,
            ProjectDiskUseObservation::CurrentAttachment,
            ProjectDiskLockObservation::CurrentAttachment,
            ProjectDiskRecoverability::Unknown,
        )
    }

    fn attached() -> ProjectDiskLeaseRecord {
        let detached = ProjectDiskLeaseRecord::new_detached(
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            ProjectDiskId::parse("disk-a").unwrap(),
            ProjectDiskGeneration::new(3).unwrap(),
        );
        let plan = detached
            .plan_attach(
                ResidentSandboxId::parse("sandbox-a").unwrap(),
                ResidentSandboxGeneration::new(11).unwrap(),
                detached_exact(),
            )
            .unwrap();
        detached.record_attach_success(&plan, attached_exact()).unwrap()
    }

    fn filesystem(
        current: &ProjectDiskLeaseRecord,
        generation: u64,
    ) -> ProjectDiskFilesystemBinding {
        ProjectDiskFilesystemBinding::new(
            current,
            ProjectDiskFilesystemGeneration::new(generation).unwrap(),
            ProjectDiskFilesystemFormatProfileGeneration::new(2).unwrap(),
            ProjectDiskFilesystemKind::Ext4,
        )
    }

    #[test]
    fn exact_current_authority_remains_valid_inside_window() {
        let current = attached();
        let fs = filesystem(&current, 7);
        let window = ProjectDiskCorrelationWindow::for_test(
            ProjectDiskCorrelationWindowGeneration::new(4).unwrap(),
            &current,
            &fs,
        )
        .unwrap();
        window.confirm(&current, &fs).unwrap();
        assert!(window.summary().exclusive_mutation_guard_held());
        assert_eq!(window.summary().disk_revision(), current.revision());
        assert_eq!(window.summary().filesystem_generation().get(), 7);
    }

    #[test]
    fn lease_revision_or_attachment_state_drift_expires_window() {
        let current = attached();
        let fs = filesystem(&current, 7);
        let window = ProjectDiskCorrelationWindow::for_test(
            ProjectDiskCorrelationWindowGeneration::new(4).unwrap(),
            &current,
            &fs,
        )
        .unwrap();
        let revalidate = current.require_revalidation().unwrap();
        assert!(window.confirm(&revalidate, &fs).is_err());
    }

    #[test]
    fn filesystem_generation_drift_expires_window() {
        let current = attached();
        let fs = filesystem(&current, 7);
        let window = ProjectDiskCorrelationWindow::for_test(
            ProjectDiskCorrelationWindowGeneration::new(4).unwrap(),
            &current,
            &fs,
        )
        .unwrap();
        assert!(window.confirm(&current, &filesystem(&current, 8)).is_err());
    }
}
