//! Sealed cross-layer evidence that one exact P3-owned Lima physical disk attached to one exact
//! resident sandbox appears as one exact whole block-device node in that guest.
//!
//! This is the missing bridge between host-side descriptor/Lima attachment evidence and the guest
//! `st_rdev`/filesystem device chain. Production construction remains absent until the physical P4/
//! #628 acceptance establishes the reviewed Lima-to-guest block-device observation method.

use std::fmt;

use serde::Serialize;

use crate::artifact::Sha256Digest;
use crate::project_catalog::ProjectIdentity;
use crate::project_disk_lease::{
    ProjectDiskAttachmentGeneration, ProjectDiskGeneration, ProjectDiskId, ProjectDiskLeaseRecord,
    ProjectDiskLeaseState, ProjectDiskRevision, ResidentSandboxGeneration, ResidentSandboxId,
};
use crate::trusted_project_filesystem_full_guest_correlation::TrustedProjectFilesystemFullGuestCorrelation;

pub const TRUSTED_PROJECT_DISK_GUEST_BLOCK_ATTACHMENT_SCHEMA_VERSION: u8 = 1;
const ZERO_DIGEST: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrustedProjectDiskGuestBlockAttachmentSummary {
    schema_version: u8,
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    disk_revision: ProjectDiskRevision,
    attachment_generation: ProjectDiskAttachmentGeneration,
    sandbox_id: ResidentSandboxId,
    sandbox_generation: ResidentSandboxGeneration,
    host_physical_identity_bound: bool,
    host_backing_identity_bound: bool,
    lima_attachment_bound: bool,
    guest_whole_block_device_bound: bool,
}

impl TrustedProjectDiskGuestBlockAttachmentSummary {
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
}

/// Exact host-disk → guest-block-device bridge.
///
/// Host physical/backing digests plus guest block `st_rdev`/inode stay private. The value is
/// non-serializable and non-cloneable.
pub struct TrustedProjectDiskGuestBlockAttachment {
    summary: TrustedProjectDiskGuestBlockAttachmentSummary,
    physical_identity_digest: Sha256Digest,
    backing_identity_digest: Sha256Digest,
    guest_block_rdev: u64,
    guest_block_inode: u64,
}

impl TrustedProjectDiskGuestBlockAttachment {
    #[must_use]
    pub const fn summary(&self) -> &TrustedProjectDiskGuestBlockAttachmentSummary {
        &self.summary
    }

    /// Reconfirm current P1 attachment identity and the exact whole block node proven by the guest
    /// correlation.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal for any current revision/attachment/sandbox drift or block-node
    /// replacement.
    pub fn confirm(
        &self,
        current: &ProjectDiskLeaseRecord,
        guest: &TrustedProjectFilesystemFullGuestCorrelation,
    ) -> Result<(), TrustedProjectDiskGuestBlockAttachmentError> {
        let ProjectDiskLeaseState::Attached { attachment } = current.state() else {
            return Err(attachment_mismatch());
        };
        if current.project() != &self.summary.project
            || current.disk_id() != &self.summary.disk_id
            || current.disk_generation() != self.summary.disk_generation
            || current.revision() != self.summary.disk_revision
            || attachment.generation() != self.summary.attachment_generation
            || attachment.sandbox_id() != &self.summary.sandbox_id
            || attachment.sandbox_generation() != self.summary.sandbox_generation
            || !self.summary.host_physical_identity_bound
            || !self.summary.host_backing_identity_bound
            || !self.summary.lima_attachment_bound
            || !self.summary.guest_whole_block_device_bound
            || is_zero_digest(&self.physical_identity_digest)
            || is_zero_digest(&self.backing_identity_digest)
            || !guest.matches_block_device_identity(self.guest_block_rdev, self.guest_block_inode)
        {
            return Err(attachment_mismatch());
        }
        Ok(())
    }

    #[must_use]
    pub(crate) fn matches_host_physical_identity(
        &self,
        physical_identity_digest: &Sha256Digest,
        backing_identity_digest: &Sha256Digest,
    ) -> bool {
        self.physical_identity_digest == *physical_identity_digest
            && self.backing_identity_digest == *backing_identity_digest
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        current: &ProjectDiskLeaseRecord,
        physical_identity_digest: Sha256Digest,
        backing_identity_digest: Sha256Digest,
        guest_block_rdev: u64,
        guest_block_inode: u64,
    ) -> Result<Self, TrustedProjectDiskGuestBlockAttachmentError> {
        let ProjectDiskLeaseState::Attached { attachment } = current.state() else {
            return Err(attachment_mismatch());
        };
        if guest_block_inode == 0
            || is_zero_digest(&physical_identity_digest)
            || is_zero_digest(&backing_identity_digest)
        {
            return Err(attachment_mismatch());
        }
        Ok(Self {
            summary: TrustedProjectDiskGuestBlockAttachmentSummary {
                schema_version: TRUSTED_PROJECT_DISK_GUEST_BLOCK_ATTACHMENT_SCHEMA_VERSION,
                project: current.project().clone(),
                disk_id: current.disk_id().clone(),
                disk_generation: current.disk_generation(),
                disk_revision: current.revision(),
                attachment_generation: attachment.generation(),
                sandbox_id: attachment.sandbox_id().clone(),
                sandbox_generation: attachment.sandbox_generation(),
                host_physical_identity_bound: true,
                host_backing_identity_bound: true,
                lima_attachment_bound: true,
                guest_whole_block_device_bound: true,
            },
            physical_identity_digest,
            backing_identity_digest,
            guest_block_rdev,
            guest_block_inode,
        })
    }
}

impl fmt::Debug for TrustedProjectDiskGuestBlockAttachment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedProjectDiskGuestBlockAttachment")
            .field("summary", &self.summary)
            .field("physical_identity", &"<redacted>")
            .field("backing_identity", &"<redacted>")
            .field("guest_block_device", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustedProjectDiskGuestBlockAttachmentErrorKind {
    AttachmentMismatch,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TrustedProjectDiskGuestBlockAttachmentError {
    kind: TrustedProjectDiskGuestBlockAttachmentErrorKind,
    code: &'static str,
    message: &'static str,
}

impl TrustedProjectDiskGuestBlockAttachmentError {
    #[must_use]
    pub const fn kind(self) -> TrustedProjectDiskGuestBlockAttachmentErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for TrustedProjectDiskGuestBlockAttachmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedProjectDiskGuestBlockAttachmentError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for TrustedProjectDiskGuestBlockAttachmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for TrustedProjectDiskGuestBlockAttachmentError {}

fn is_zero_digest(digest: &Sha256Digest) -> bool {
    digest.as_str() == ZERO_DIGEST
}

const fn attachment_mismatch() -> TrustedProjectDiskGuestBlockAttachmentError {
    TrustedProjectDiskGuestBlockAttachmentError {
        kind: TrustedProjectDiskGuestBlockAttachmentErrorKind::AttachmentMismatch,
        code: "project_disk_guest_block_attachment_mismatch",
        message: "host project disk does not match the exact guest whole block device",
    }
}

#[cfg(test)]
mod tests {
    use super::TrustedProjectDiskGuestBlockAttachment;
    use crate::artifact::Sha256Digest;
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
    use crate::trusted_project_block_device_correlation::correlate_trusted_project_block_device;
    use crate::trusted_project_filesystem_full_guest_correlation::correlate_trusted_project_filesystem_guest;
    use crate::trusted_project_filesystem_guest_observation::observe_trusted_project_filesystem_guest;

    fn digest(byte: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

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

    fn current() -> ProjectDiskLeaseRecord {
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

    fn guest(
        current: &ProjectDiskLeaseRecord,
        block_rdev: u64,
        block_inode: u64,
    ) -> crate::trusted_project_filesystem_full_guest_correlation::TrustedProjectFilesystemFullGuestCorrelation {
        let fs = ProjectDiskFilesystemBinding::new(
            current,
            ProjectDiskFilesystemGeneration::new(7).unwrap(),
            ProjectDiskFilesystemFormatProfileGeneration::new(2).unwrap(),
            ProjectDiskFilesystemKind::Ext4,
        );
        let observed = observe_trusted_project_filesystem_guest(
            &fs,
            block_rdev,
            99,
            b"/srv/project",
            b"123 45 8:1 / /srv/project rw - ext4 /dev/vda rw\n",
        )
        .unwrap();
        let block = correlate_trusted_project_block_device(
            &observed,
            block_rdev,
            block_inode,
            true,
            true,
        )
        .unwrap();
        correlate_trusted_project_filesystem_guest(observed, block).unwrap()
    }

    #[test]
    fn exact_lima_attachment_to_guest_block_bridge_confirms() {
        let current = current();
        let bridge = TrustedProjectDiskGuestBlockAttachment::for_test(
            &current,
            digest('a'),
            digest('b'),
            2049,
            77,
        )
        .unwrap();
        bridge.confirm(&current, &guest(&current, 2049, 77)).unwrap();
        assert!(bridge.matches_host_physical_identity(&digest('a'), &digest('b')));
    }

    #[test]
    fn wrong_guest_block_or_current_revision_is_rejected() {
        let current = current();
        let bridge = TrustedProjectDiskGuestBlockAttachment::for_test(
            &current,
            digest('a'),
            digest('b'),
            2049,
            77,
        )
        .unwrap();
        assert!(bridge.confirm(&current, &guest(&current, 2049, 78)).is_err());
        assert!(
            bridge
                .confirm(
                    &current.require_revalidation().unwrap(),
                    &guest(&current, 2049, 77),
                )
                .is_err()
        );
    }

    #[test]
    fn debug_redacts_host_and_guest_physical_identity() {
        let current = current();
        let bridge = TrustedProjectDiskGuestBlockAttachment::for_test(
            &current,
            digest('a'),
            digest('b'),
            2049,
            77,
        )
        .unwrap();
        let debug = format!("{bridge:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("2049"));
        assert!(!debug.contains("77"));
        assert!(!debug.contains(digest('a').as_str()));
    }
}
