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
    filesystem_generation: ProjectDiskFilesystemGeneration,
    format_profile_generation: ProjectDiskFilesystemFormatProfileGeneration,
    filesystem_kind: ProjectDiskFilesystemKind,
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
    pub const fn correlation_generation(&self) -> TrustedProjectFilesystemCorrelationGeneration {
        self.correlation_generation
    }

    #[must_use]
    pub const fn filesystem_device_bound(&self) -> bool {
        self.filesystem_device_bound
    }
}

/// Opaque evidence that one currently observed Linux filesystem device is the exact filesystem for
/// one accepted project-disk attachment and filesystem generation.
///
/// Production deliberately has no constructor. Until #565 P3/P4, #628, and the guest correlation
/// path can prove the host disk, current attachment, resident sandbox, exact filesystem generation,
/// and guest device together, normal SmolRunner code cannot mint this proof and the privileged
/// OverlayFS executor remains unusable.
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

    /// Confirm the sealed proof against the exact source anchor and freshly held role filesystem
    /// device used by #589.
    ///
    /// The anchor now carries the project-disk revision and attachment generation, so this check
    /// rejects an old proof after a detach/reattach even when the disk and resident sandbox
    /// generations remain unchanged.
    pub(crate) fn confirm_overlay_anchor(
        &self,
        anchor: &OverlaySourceAnchorBinding,
        observed_filesystem_device: u64,
    ) -> Result<(), TrustedProjectFilesystemCorrelationError> {
        if &self.summary.project != anchor.project()
            || &self.summary.disk_id != anchor.disk_id()
            || self.summary.disk_generation != anchor.disk_generation()
            || self.summary.disk_revision != anchor.disk_revision()
            || self.summary.attachment_generation != anchor.attachment_generation()
            || &self.summary.sandbox_id != anchor.resident_sandbox_id()
            || self.summary.sandbox_generation != anchor.resident_sandbox_generation()
            || !self.summary.filesystem_device_bound
            || self.filesystem_device != observed_filesystem_device
        {
            return Err(correlation_mismatch());
        }
        Ok(())
    }

    /// Reconfirm every durable project-disk/filesystem parent needed by the eventual production
    /// composition in addition to the #589 anchor and held role device.
    ///
    /// This method remains crate-private and currently has no production caller. The later guest
    /// transaction will invoke it at both existing #589 confirmation points after freshly reading
    /// the current attached lease and accepted filesystem generation.
    #[allow(dead_code)]
    pub(crate) fn confirm_current_attachment_and_filesystem(
        &self,
        anchor: &OverlaySourceAnchorBinding,
        current: &ProjectDiskLeaseRecord,
        filesystem: &ProjectDiskFilesystemBinding,
        observed_filesystem_device: u64,
    ) -> Result<(), TrustedProjectFilesystemCorrelationError> {
        self.confirm_overlay_anchor(anchor, observed_filesystem_device)?;
        if current.project() != &self.summary.project
            || current.disk_id() != &self.summary.disk_id
            || current.disk_generation() != self.summary.disk_generation
            || current.revision() != self.summary.disk_revision
            || !filesystem.matches_project_disk(current)
            || filesystem.filesystem_generation() != self.summary.filesystem_generation
            || filesystem.format_profile_generation() != self.summary.format_profile_generation
            || filesystem.kind() != self.summary.filesystem_kind
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

    /// Legacy anchor-only constructor retained solely for the linux-gated `#589` executor tests.
    ///
    /// Their module is not compiled on other hosts, so the function is intentionally unreachable
    /// there and the portability-scoped allowance keeps non-Linux lint runs honest.
    #[cfg(test)]
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn for_test(
        anchor: &OverlaySourceAnchorBinding,
        _legacy_attachment_generation: ProjectDiskAttachmentGeneration,
        correlation_generation: TrustedProjectFilesystemCorrelationGeneration,
        filesystem_device: u64,
    ) -> Self {
        Self {
            summary: TrustedProjectFilesystemCorrelationSummary {
                schema_version: TRUSTED_PROJECT_FILESYSTEM_CORRELATION_SCHEMA_VERSION,
                project: anchor.project().clone(),
                disk_id: anchor.disk_id().clone(),
                disk_generation: anchor.disk_generation(),
                disk_revision: anchor.disk_revision(),
                attachment_generation: anchor.attachment_generation(),
                sandbox_id: anchor.resident_sandbox_id().clone(),
                sandbox_generation: anchor.resident_sandbox_generation(),
                filesystem_generation: ProjectDiskFilesystemGeneration::new(1)
                    .expect("test filesystem generation is positive"),
                format_profile_generation: ProjectDiskFilesystemFormatProfileGeneration::new(1)
                    .expect("test format-profile generation is positive"),
                filesystem_kind: ProjectDiskFilesystemKind::Ext4,
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
        filesystem: &ProjectDiskFilesystemBinding,
        correlation_generation: TrustedProjectFilesystemCorrelationGeneration,
        filesystem_device: u64,
    ) -> Result<Self, TrustedProjectFilesystemCorrelationError> {
        let ProjectDiskLeaseState::Attached { attachment } = current.state() else {
            return Err(correlation_mismatch());
        };
        if current.project() != anchor.project()
            || current.disk_id() != anchor.disk_id()
            || current.disk_generation() != anchor.disk_generation()
            || current.revision() != anchor.disk_revision()
            || attachment.generation() != anchor.attachment_generation()
            || attachment.sandbox_id() != anchor.resident_sandbox_id()
            || attachment.sandbox_generation() != anchor.resident_sandbox_generation()
            || !filesystem.matches_project_disk(current)
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
                filesystem_generation: filesystem.filesystem_generation(),
                format_profile_generation: filesystem.format_profile_generation(),
                filesystem_kind: filesystem.kind(),
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
    use crate::project_disk_filesystem::{
        ProjectDiskFilesystemBinding, ProjectDiskFilesystemFormatProfileGeneration,
        ProjectDiskFilesystemGeneration, ProjectDiskFilesystemKind,
    };
    use crate::project_disk_lease::{
        ProjectDiskGeneration, ProjectDiskId, ProjectDiskLeaseRecord, ProjectDiskLeaseState,
        ProjectDiskLockObservation, ProjectDiskObservation, ProjectDiskPhysicalObservation,
        ProjectDiskRecoverability, ProjectDiskUseObservation, ResidentSandboxGeneration,
        ResidentSandboxId,
    };
    use crate::trusted_overlay_task_view::{
        OverlaySourceAnchorBinding, OverlaySourceAnchorGeneration, OverlaySourceAnchorId,
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
                detached_exact(),
            )
            .unwrap();
        detached
            .record_attach_success(&plan, attached_exact())
            .unwrap()
    }

    fn anchor_for(record: &ProjectDiskLeaseRecord) -> OverlaySourceAnchorBinding {
        let ProjectDiskLeaseState::Attached { attachment } = record.state() else {
            panic!("test requires attached record");
        };
        OverlaySourceAnchorBinding::new(
            record.project().clone(),
            record.disk_id().clone(),
            record.disk_generation(),
            record.revision(),
            attachment.sandbox_id().clone(),
            attachment.sandbox_generation(),
            attachment.generation(),
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

    fn filesystem(
        record: &ProjectDiskLeaseRecord,
        generation: u64,
        profile: u64,
        kind: ProjectDiskFilesystemKind,
    ) -> ProjectDiskFilesystemBinding {
        ProjectDiskFilesystemBinding::new(
            record,
            ProjectDiskFilesystemGeneration::new(generation).unwrap(),
            ProjectDiskFilesystemFormatProfileGeneration::new(profile).unwrap(),
            kind,
        )
    }

    fn detach_reattach(current: &ProjectDiskLeaseRecord) -> ProjectDiskLeaseRecord {
        let detach = current.plan_detach(attached_exact()).unwrap();
        let detached = current
            .record_detach_success(&detach, detached_exact())
            .unwrap();
        let reattach = detached
            .plan_attach(
                ResidentSandboxId::parse("sandbox-a").unwrap(),
                ResidentSandboxGeneration::new(11).unwrap(),
                detached_exact(),
            )
            .unwrap();
        detached
            .record_attach_success(&reattach, attached_exact())
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
    fn proof_confirms_exact_anchor_revision_attachment_and_device() {
        let current = attached_record();
        let anchor = anchor_for(&current);
        let proof = TrustedProjectFilesystemCorrelationProof::for_test_current(
            &anchor,
            &current,
            &filesystem(&current, 7, 2, ProjectDiskFilesystemKind::Xfs),
            TrustedProjectFilesystemCorrelationGeneration::new(13).unwrap(),
            0xfeed_beef,
        )
        .unwrap();
        proof.confirm_overlay_anchor(&anchor, 0xfeed_beef).unwrap();
        assert!(proof.confirm_overlay_anchor(&anchor, 0xfeed_beee).is_err());
        assert_eq!(proof.summary().disk_revision(), current.revision());
        assert_eq!(
            proof.summary().attachment_generation(),
            anchor.attachment_generation()
        );
    }

    #[test]
    fn detach_reattach_same_sandbox_expires_old_proof() {
        let current = attached_record();
        let anchor = anchor_for(&current);
        let fs = filesystem(&current, 7, 2, ProjectDiskFilesystemKind::Xfs);
        let proof = TrustedProjectFilesystemCorrelationProof::for_test_current(
            &anchor,
            &current,
            &fs,
            TrustedProjectFilesystemCorrelationGeneration::new(13).unwrap(),
            0xfeed_beef,
        )
        .unwrap();
        let reattached = detach_reattach(&current);
        let new_anchor = anchor_for(&reattached);
        assert_ne!(current.revision(), reattached.revision());
        assert_ne!(
            anchor.attachment_generation(),
            new_anchor.attachment_generation()
        );
        assert!(
            proof
                .confirm_overlay_anchor(&new_anchor, 0xfeed_beef)
                .is_err()
        );
    }

    #[test]
    fn current_confirmation_rejects_revalidation_state() {
        let current = attached_record();
        let anchor = anchor_for(&current);
        let fs = filesystem(&current, 7, 2, ProjectDiskFilesystemKind::Xfs);
        let proof = TrustedProjectFilesystemCorrelationProof::for_test_current(
            &anchor,
            &current,
            &fs,
            TrustedProjectFilesystemCorrelationGeneration::new(13).unwrap(),
            0xfeed_beef,
        )
        .unwrap();
        let revalidate = current.require_revalidation().unwrap();
        assert!(
            proof
                .confirm_current_attachment_and_filesystem(&anchor, &revalidate, &fs, 0xfeed_beef,)
                .is_err()
        );
    }

    #[test]
    fn current_confirmation_state_guard_rejects_same_revision_non_attached() {
        let current = attached_record();
        let anchor = anchor_for(&current);
        let fs = filesystem(&current, 7, 2, ProjectDiskFilesystemKind::Xfs);
        let proof = TrustedProjectFilesystemCorrelationProof::for_test_current(
            &anchor,
            &current,
            &fs,
            TrustedProjectFilesystemCorrelationGeneration::new(13).unwrap(),
            0xfeed_beef,
        )
        .unwrap();
        let sibling_detached_start = ProjectDiskLeaseRecord::new_detached(
            current.project().clone(),
            current.disk_id().clone(),
            current.disk_generation(),
        );
        let revalidate_required = sibling_detached_start.require_revalidation().unwrap();
        assert_eq!(revalidate_required.revision(), current.revision());
        assert!(
            proof
                .confirm_current_attachment_and_filesystem(
                    &anchor,
                    &revalidate_required,
                    &fs,
                    0xfeed_beef,
                )
                .is_err()
        );
    }

    #[test]
    fn current_confirmation_rejects_filesystem_generation_profile_and_kind_drift() {
        let current = attached_record();
        let anchor = anchor_for(&current);
        let fs = filesystem(&current, 7, 2, ProjectDiskFilesystemKind::Xfs);
        let proof = TrustedProjectFilesystemCorrelationProof::for_test_current(
            &anchor,
            &current,
            &fs,
            TrustedProjectFilesystemCorrelationGeneration::new(13).unwrap(),
            0xfeed_beef,
        )
        .unwrap();
        for changed in [
            filesystem(&current, 8, 2, ProjectDiskFilesystemKind::Xfs),
            filesystem(&current, 7, 3, ProjectDiskFilesystemKind::Xfs),
            filesystem(&current, 7, 2, ProjectDiskFilesystemKind::Ext4),
        ] {
            assert!(
                proof
                    .confirm_current_attachment_and_filesystem(
                        &anchor,
                        &current,
                        &changed,
                        0xfeed_beef,
                    )
                    .is_err()
            );
        }
    }

    #[test]
    fn current_confirmation_accepts_exact_full_tuple() {
        let current = attached_record();
        let anchor = anchor_for(&current);
        let fs = filesystem(&current, 7, 2, ProjectDiskFilesystemKind::Xfs);
        let proof = TrustedProjectFilesystemCorrelationProof::for_test_current(
            &anchor,
            &current,
            &fs,
            TrustedProjectFilesystemCorrelationGeneration::new(13).unwrap(),
            0xfeed_beef,
        )
        .unwrap();
        proof
            .confirm_current_attachment_and_filesystem(&anchor, &current, &fs, 0xfeed_beef)
            .unwrap();
        assert_eq!(proof.summary().filesystem_generation().get(), 7);
        assert_eq!(proof.summary().format_profile_generation().get(), 2);
        assert_eq!(
            proof.summary().filesystem_kind(),
            ProjectDiskFilesystemKind::Xfs
        );
    }

    #[test]
    fn debug_redacts_raw_filesystem_device() {
        let current = attached_record();
        let anchor = anchor_for(&current);
        let proof = TrustedProjectFilesystemCorrelationProof::for_test_current(
            &anchor,
            &current,
            &filesystem(&current, 7, 2, ProjectDiskFilesystemKind::Xfs),
            TrustedProjectFilesystemCorrelationGeneration::new(13).unwrap(),
            123_456_789,
        )
        .unwrap();
        let debug = format!("{proof:?}");
        assert!(!debug.contains("123456789"));
        assert!(debug.contains("<private exact filesystem device>"));
    }
}
