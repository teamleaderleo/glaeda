use std::fmt;

use serde::Serialize;

use crate::project_catalog::ProjectIdentity;
use crate::project_disk_lease::{
    ProjectDiskAttachmentGeneration, ProjectDiskGeneration, ProjectDiskId, ProjectDiskLeaseRecord,
    ProjectDiskLeaseState, ProjectDiskRevision, ResidentSandboxGeneration, ResidentSandboxId,
};
use crate::trusted_overlay_task_view::OverlaySourceAnchorBinding;

pub const TRUSTED_PROJECT_FILESYSTEM_CORRELATION_SCHEMA_VERSION: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct TrustedProjectFilesystemCorrelationGeneration(u64);

impl TrustedProjectFilesystemCorrelationGeneration {
    /// Construct one positive observation/correlation generation.
    ///
    /// # Errors
    ///
    /// Returns a bounded error when the value is zero.
    pub fn new(value: u64) -> Result<Self, TrustedProjectFilesystemCorrelationError> {
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
pub struct TrustedProjectFilesystemCorrelationSummary {
    schema_version: u8,
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    disk_revision: ProjectDiskRevision,
    attachment_generation: ProjectDiskAttachmentGeneration,
    sandbox_id: ResidentSandboxId,
    sandbox_generation: ResidentSandboxGeneration,
    correlation_generation: TrustedProjectFilesystemCorrelationGeneration,
    filesystem_device_bound: bool,
}

impl TrustedProjectFilesystemCorrelationSummary {
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
    pub const fn correlation_generation(&self) -> TrustedProjectFilesystemCorrelationGeneration {
        self.correlation_generation
    }

    #[must_use]
    pub const fn filesystem_device_bound(&self) -> bool {
        self.filesystem_device_bound
    }
}

/// Opaque evidence that one currently observed Linux filesystem device is the exact filesystem for
/// one accepted project-disk attachment generation.
///
/// P1 deliberately has no production constructor. Until #565's physical project-disk observer can
/// prove the host disk, current attachment, resident sandbox, and guest filesystem together, normal
/// SmolRunner code cannot mint this proof and the privileged OverlayFS executor remains unusable.
///
/// The raw device number is kernel-local evidence. It is not serialized, cloneable, or exposed by
/// public Debug/output.
pub struct TrustedProjectFilesystemCorrelationProof {
    summary: TrustedProjectFilesystemCorrelationSummary,
    filesystem_device: u64,
}

impl TrustedProjectFilesystemCorrelationProof {
    #[must_use]
    pub const fn summary(&self) -> &TrustedProjectFilesystemCorrelationSummary {
        &self.summary
    }

    pub(crate) fn confirm_overlay_anchor(
        &self,
        anchor: &OverlaySourceAnchorBinding,
        observed_filesystem_device: u64,
    ) -> Result<(), TrustedProjectFilesystemCorrelationError> {
        if &self.summary.project != anchor.project()
            || &self.summary.disk_id != anchor.disk_id()
            || self.summary.disk_generation != anchor.disk_generation()
            || &self.summary.sandbox_id != anchor.resident_sandbox_id()
            || self.summary.sandbox_generation != anchor.resident_sandbox_generation()
            || !self.summary.filesystem_device_bound
            || self.filesystem_device != observed_filesystem_device
        {
            return Err(correlation_mismatch());
        }
        Ok(())
    }

    /// Reconfirm the proof against the exact current attached project-disk lease as well as the
    /// source anchor and observed filesystem device.
    ///
    /// This is the production-facing confirmation seam for #640. The current #589 executor still
    /// calls the older anchor-only confirmation until the P3/P4 integration can supply a freshly
    /// re-read attached lease at both pre-mutation confirmation points. Keeping this method sealed
    /// now lets that later composition reject stale lease revisions and detach/reattach generation
    /// reuse without exposing any proof constructor.
    pub(crate) fn confirm_current_attachment(
        &self,
        anchor: &OverlaySourceAnchorBinding,
        current: &ProjectDiskLeaseRecord,
        observed_filesystem_device: u64,
    ) -> Result<(), TrustedProjectFilesystemCorrelationError> {
        self.confirm_overlay_anchor(anchor, observed_filesystem_device)?;
        if current.project() != &self.summary.project
            || current.disk_id() != &self.summary.disk_id
            || current.disk_generation() != self.summary.disk_generation
            || current.revision() != self.summary.disk_revision
        {
            return Err(correlation_mismatch());
        }
        let ProjectDiskLeaseState::Attached { attachment } = current.state() else {
            return Err(correlation_mismatch());
        };
        if attachment.generation() != self.summary.attachment_generation
            || attachment.sandbox_id() != &self.summary.sandbox_id
            || attachment.sandbox_generation() != self.summary.sandbox_generation
        {
            return Err(correlation_mismatch());
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        anchor: &OverlaySourceAnchorBinding,
        attachment_generation: ProjectDiskAttachmentGeneration,
        correlation_generation: TrustedProjectFilesystemCorrelationGeneration,
        filesystem_device: u64,
    ) -> Self {
        Self {
            summary: TrustedProjectFilesystemCorrelationSummary {
                schema_version: TRUSTED_PROJECT_FILESYSTEM_CORRELATION_SCHEMA_VERSION,
                project: anchor.project().clone(),
                disk_id: anchor.disk_id().clone(),
                disk_generation: anchor.disk_generation(),
                disk_revision: ProjectDiskRevision::new(1).expect("test revision is positive"),
                attachment_generation,
                sandbox_id: anchor.resident_sandbox_id().clone(),
                sandbox_generation: anchor.resident_sandbox_generation(),
                correlation_generation,
                filesystem_device_bound: true,
            },
            filesystem_device,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test_current(
        anchor: &OverlaySourceAnchorBinding,
        current: &ProjectDiskLeaseRecord,
        correlation_generation: TrustedProjectFilesystemCorrelationGeneration,
        filesystem_device: u64,
    ) -> Result<Self, TrustedProjectFilesystemCorrelationError> {
        let ProjectDiskLeaseState::Attached { attachment } = current.state() else {
            return Err(correlation_mismatch());
        };
        if current.project() != anchor.project()
            || current.disk_id() != anchor.disk_id()
            || current.disk_generation() != anchor.disk_generation()
            || attachment.sandbox_id() != anchor.resident_sandbox_id()
            || attachment.sandbox_generation() != anchor.resident_sandbox_generation()
        {
            return Err(correlation_mismatch());
        }
        Ok(Self {
            summary: TrustedProjectFilesystemCorrelationSummary {
                schema_version: TRUSTED_PROJECT_FILESYSTEM_CORRELATION_SCHEMA_VERSION,
                project: current.project().clone(),
                disk_id: current.disk_id().clone(),
                disk_generation: current.disk_generation(),
                disk_revision: current.revision(),
                attachment_generation: attachment.generation(),
                sandbox_id: attachment.sandbox_id().clone(),
                sandbox_generation: attachment.sandbox_generation(),
                correlation_generation,
                filesystem_device_bound: true,
            },
            filesystem_device,
        })
    }
}

impl fmt::Debug for TrustedProjectFilesystemCorrelationProof {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let _ = self.filesystem_device;
        formatter
            .debug_struct("TrustedProjectFilesystemCorrelationProof")
            .field("summary", &self.summary)
            .field("filesystem_device", &"<private exact filesystem device>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustedProjectFilesystemCorrelationErrorKind {
    InvalidGeneration,
    Mismatch,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TrustedProjectFilesystemCorrelationError {
    kind: TrustedProjectFilesystemCorrelationErrorKind,
    code: &'static str,
    message: &'static str,
}

impl TrustedProjectFilesystemCorrelationError {
    #[must_use]
    pub const fn kind(self) -> TrustedProjectFilesystemCorrelationErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for TrustedProjectFilesystemCorrelationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedProjectFilesystemCorrelationError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for TrustedProjectFilesystemCorrelationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for TrustedProjectFilesystemCorrelationError {}

const fn invalid_generation() -> TrustedProjectFilesystemCorrelationError {
    TrustedProjectFilesystemCorrelationError {
        kind: TrustedProjectFilesystemCorrelationErrorKind::InvalidGeneration,
        code: "project_filesystem_correlation_generation_invalid",
        message: "trusted project-filesystem correlation generation is invalid",
    }
}

const fn correlation_mismatch() -> TrustedProjectFilesystemCorrelationError {
    TrustedProjectFilesystemCorrelationError {
        kind: TrustedProjectFilesystemCorrelationErrorKind::Mismatch,
        code: "project_filesystem_correlation_mismatch",
        message: "trusted project-filesystem correlation evidence does not match the current task",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        TrustedProjectFilesystemCorrelationGeneration, TrustedProjectFilesystemCorrelationProof,
    };
    use crate::artifact::{CommitId, GitTreeId, Sha256Digest};
    use crate::project_catalog::ProjectIdentity;
    use crate::project_disk_lease::{
        ProjectDiskAttachmentGeneration, ProjectDiskGeneration, ProjectDiskId,
        ProjectDiskLeaseRecord, ProjectDiskLockObservation, ProjectDiskObservation,
        ProjectDiskPhysicalObservation, ProjectDiskRecoverability, ProjectDiskUseObservation,
        ResidentSandboxGeneration, ResidentSandboxId,
    };
    use crate::trusted_overlay_task_view::{
        OverlaySourceAnchorBinding, OverlaySourceAnchorGeneration, OverlaySourceAnchorId,
    };

    fn anchor() -> OverlaySourceAnchorBinding {
        OverlaySourceAnchorBinding::new(
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            ProjectDiskId::parse("disk-a").unwrap(),
            ProjectDiskGeneration::new(3).unwrap(),
            ResidentSandboxId::parse("sandbox-a").unwrap(),
            ResidentSandboxGeneration::new(11).unwrap(),
            OverlaySourceAnchorId::parse("anchor-a").unwrap(),
            OverlaySourceAnchorGeneration::new(5).unwrap(),
            CommitId::parse("0123456789abcdef0123456789abcdef01234567").unwrap(),
            GitTreeId::parse("89abcdef0123456789abcdef0123456789abcdef").unwrap(),
            Sha256Digest::parse(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .unwrap(),
        )
    }

    fn attached_record() -> ProjectDiskLeaseRecord {
        let detached = ProjectDiskLeaseRecord::new_detached(
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            ProjectDiskId::parse("disk-a").unwrap(),
            ProjectDiskGeneration::new(3).unwrap(),
        );
        let plan = detached
            .plan_attach(
                ResidentSandboxId::parse("sandbox-a").unwrap(),
                ResidentSandboxGeneration::new(11).unwrap(),
                ProjectDiskObservation::new(
                    ProjectDiskPhysicalObservation::Exact,
                    ProjectDiskUseObservation::Unused,
                    ProjectDiskLockObservation::Unlocked,
                    ProjectDiskRecoverability::Unknown,
                ),
            )
            .unwrap();
        detached
            .record_attach_success(
                &plan,
                ProjectDiskObservation::new(
                    ProjectDiskPhysicalObservation::Exact,
                    ProjectDiskUseObservation::CurrentAttachment,
                    ProjectDiskLockObservation::CurrentAttachment,
                    ProjectDiskRecoverability::Unknown,
                ),
            )
            .unwrap()
    }

    #[test]
    fn generation_must_be_positive() {
        assert!(TrustedProjectFilesystemCorrelationGeneration::new(0).is_err());
        assert_eq!(
            TrustedProjectFilesystemCorrelationGeneration::new(1)
                .unwrap()
                .get(),
            1
        );
    }

    #[test]
    fn proof_confirms_exact_anchor_and_device_only() {
        let anchor = anchor();
        let proof = TrustedProjectFilesystemCorrelationProof::for_test(
            &anchor,
            ProjectDiskAttachmentGeneration::new(7).unwrap(),
            TrustedProjectFilesystemCorrelationGeneration::new(13).unwrap(),
            0xfeed_beef,
        );
        proof.confirm_overlay_anchor(&anchor, 0xfeed_beef).unwrap();
        assert!(proof.confirm_overlay_anchor(&anchor, 0xfeed_beee).is_err());
        assert_eq!(proof.summary().attachment_generation().get(), 7);
        assert_eq!(proof.summary().disk_revision().get(), 1);
    }

    #[test]
    fn current_attachment_confirmation_rejects_stale_detach_reattach_generation() {
        let anchor = anchor();
        let current = attached_record();
        let proof = TrustedProjectFilesystemCorrelationProof::for_test_current(
            &anchor,
            &current,
            TrustedProjectFilesystemCorrelationGeneration::new(13).unwrap(),
            0xfeed_beef,
        )
        .unwrap();
        proof
            .confirm_current_attachment(&anchor, &current, 0xfeed_beef)
            .unwrap();

        let detach = current
            .plan_detach(ProjectDiskObservation::new(
                ProjectDiskPhysicalObservation::Exact,
                ProjectDiskUseObservation::CurrentAttachment,
                ProjectDiskLockObservation::CurrentAttachment,
                ProjectDiskRecoverability::Unknown,
            ))
            .unwrap();
        let detached = current
            .record_detach_success(
                &detach,
                ProjectDiskObservation::new(
                    ProjectDiskPhysicalObservation::Exact,
                    ProjectDiskUseObservation::Unused,
                    ProjectDiskLockObservation::Unlocked,
                    ProjectDiskRecoverability::Unknown,
                ),
            )
            .unwrap();
        let reattach = detached
            .plan_attach(
                ResidentSandboxId::parse("sandbox-a").unwrap(),
                ResidentSandboxGeneration::new(11).unwrap(),
                ProjectDiskObservation::new(
                    ProjectDiskPhysicalObservation::Exact,
                    ProjectDiskUseObservation::Unused,
                    ProjectDiskLockObservation::Unlocked,
                    ProjectDiskRecoverability::Unknown,
                ),
            )
            .unwrap();
        let reattached = detached
            .record_attach_success(
                &reattach,
                ProjectDiskObservation::new(
                    ProjectDiskPhysicalObservation::Exact,
                    ProjectDiskUseObservation::CurrentAttachment,
                    ProjectDiskLockObservation::CurrentAttachment,
                    ProjectDiskRecoverability::Unknown,
                ),
            )
            .unwrap();

        assert_eq!(proof.summary().attachment_generation().get(), 1);
        assert_eq!(reattached.last_attachment_generation().unwrap().get(), 2);
        assert!(
            proof
                .confirm_current_attachment(&anchor, &reattached, 0xfeed_beef)
                .is_err()
        );
    }

    #[test]
    fn current_attachment_confirmation_rejects_non_attached_state() {
        let anchor = anchor();
        let current = attached_record();
        let proof = TrustedProjectFilesystemCorrelationProof::for_test_current(
            &anchor,
            &current,
            TrustedProjectFilesystemCorrelationGeneration::new(13).unwrap(),
            0xfeed_beef,
        )
        .unwrap();
        let revalidate = current.require_revalidation().unwrap();
        assert!(
            proof
                .confirm_current_attachment(&anchor, &revalidate, 0xfeed_beef)
                .is_err()
        );
    }

    #[test]
    fn debug_redacts_raw_filesystem_device() {
        let anchor = anchor();
        let proof = TrustedProjectFilesystemCorrelationProof::for_test(
            &anchor,
            ProjectDiskAttachmentGeneration::new(7).unwrap(),
            TrustedProjectFilesystemCorrelationGeneration::new(13).unwrap(),
            123_456_789,
        );
        let debug = format!("{proof:?}");
        assert!(!debug.contains("123456789"));
        assert!(debug.contains("<private exact filesystem device>"));
    }
}
