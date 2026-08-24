//! Sealed binding between one exact #588 project-filesystem observation transaction and the
//! resulting full guest filesystem/whole-block-device correlation.
//!
//! A decoded protocol request or receipt is declaration data. Production construction of this value
//! remains absent until the Mac invocation adapter has freshly re-confirmed the current durable
//! attachment, exact resident invocation target, reviewed limactl, and reviewed guest-control binary
//! around the one-shot execution and has accepted the typed operation result.

use std::fmt;

use serde::Serialize;

use crate::artifact::Sha256Digest;
use crate::project_catalog::ProjectIdentity;
use crate::project_disk_filesystem::{
    ProjectDiskFilesystemBinding, ProjectDiskFilesystemFormatProfileGeneration,
    ProjectDiskFilesystemGeneration, ProjectDiskFilesystemKind,
};
use crate::project_disk_lease::{
    ProjectDiskAttachmentGeneration, ProjectDiskGeneration, ProjectDiskId, ProjectDiskLeaseRecord,
    ProjectDiskLeaseState, ProjectDiskRevision, ResidentSandboxGeneration, ResidentSandboxId,
};
use crate::trusted_project_filesystem_full_guest_correlation::TrustedProjectFilesystemFullGuestCorrelation;

pub const TRUSTED_PROJECT_FILESYSTEM_GUEST_TRANSACTION_SCHEMA_VERSION: u8 = 1;
const ZERO_DIGEST: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrustedProjectFilesystemGuestTransactionSummary {
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
    limactl_generation: u64,
    guest_binary_generation: u64,
    exact_invocation_target_bound: bool,
    request_digest_bound: bool,
    result_digest_bound: bool,
}

impl TrustedProjectFilesystemGuestTransactionSummary {
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
    pub const fn limactl_generation(&self) -> u64 {
        self.limactl_generation
    }

    #[must_use]
    pub const fn guest_binary_generation(&self) -> u64 {
        self.guest_binary_generation
    }
}

/// Short-lived evidence that the full guest correlation came from the exact accepted one-shot
/// invocation for the current attachment.
///
/// Request/result, limactl, and guest-binary digests stay private. The value is non-serializable and
/// non-cloneable.
pub struct TrustedProjectFilesystemGuestTransaction {
    summary: TrustedProjectFilesystemGuestTransactionSummary,
    request_digest: Sha256Digest,
    result_digest: Sha256Digest,
    limactl_digest: Sha256Digest,
    guest_binary_digest: Sha256Digest,
}

impl TrustedProjectFilesystemGuestTransaction {
    #[must_use]
    pub const fn summary(&self) -> &TrustedProjectFilesystemGuestTransactionSummary {
        &self.summary
    }

    /// Reconfirm the transaction against current durable authority, the accepted filesystem
    /// generation, and the full guest device correlation.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal for any current revision/attachment/sandbox/filesystem drift or for
    /// incomplete invocation/request/result/binary evidence.
    pub fn confirm(
        &self,
        current: &ProjectDiskLeaseRecord,
        filesystem: &ProjectDiskFilesystemBinding,
        guest: &TrustedProjectFilesystemFullGuestCorrelation,
    ) -> Result<(), TrustedProjectFilesystemGuestTransactionError> {
        let ProjectDiskLeaseState::Attached { attachment } = current.state() else {
            return Err(authority_mismatch());
        };
        let guest_summary = guest.summary();
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
            || guest_summary.filesystem_generation() != self.summary.filesystem_generation
            || guest_summary.format_profile_generation() != self.summary.format_profile_generation
            || guest_summary.filesystem_kind() != self.summary.filesystem_kind
            || !self.summary.exact_invocation_target_bound
            || !self.summary.request_digest_bound
            || !self.summary.result_digest_bound
            || self.summary.limactl_generation == 0
            || self.summary.guest_binary_generation == 0
            || is_zero_digest(&self.request_digest)
            || is_zero_digest(&self.result_digest)
            || is_zero_digest(&self.limactl_digest)
            || is_zero_digest(&self.guest_binary_digest)
        {
            return Err(authority_mismatch());
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        current: &ProjectDiskLeaseRecord,
        filesystem: &ProjectDiskFilesystemBinding,
        limactl_generation: u64,
        limactl_digest: Sha256Digest,
        guest_binary_generation: u64,
        guest_binary_digest: Sha256Digest,
        request_digest: Sha256Digest,
        result_digest: Sha256Digest,
    ) -> Result<Self, TrustedProjectFilesystemGuestTransactionError> {
        let ProjectDiskLeaseState::Attached { attachment } = current.state() else {
            return Err(authority_mismatch());
        };
        if !filesystem.matches_project_disk(current)
            || limactl_generation == 0
            || guest_binary_generation == 0
            || is_zero_digest(&limactl_digest)
            || is_zero_digest(&guest_binary_digest)
            || is_zero_digest(&request_digest)
            || is_zero_digest(&result_digest)
        {
            return Err(authority_mismatch());
        }
        Ok(Self {
            summary: TrustedProjectFilesystemGuestTransactionSummary {
                schema_version: TRUSTED_PROJECT_FILESYSTEM_GUEST_TRANSACTION_SCHEMA_VERSION,
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
                limactl_generation,
                guest_binary_generation,
                exact_invocation_target_bound: true,
                request_digest_bound: true,
                result_digest_bound: true,
            },
            request_digest,
            result_digest,
            limactl_digest,
            guest_binary_digest,
        })
    }
}

impl fmt::Debug for TrustedProjectFilesystemGuestTransaction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedProjectFilesystemGuestTransaction")
            .field("summary", &self.summary)
            .field("request_digest", &"<redacted>")
            .field("result_digest", &"<redacted>")
            .field("limactl_digest", &"<redacted>")
            .field("guest_binary_digest", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustedProjectFilesystemGuestTransactionErrorKind {
    AuthorityMismatch,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TrustedProjectFilesystemGuestTransactionError {
    kind: TrustedProjectFilesystemGuestTransactionErrorKind,
    code: &'static str,
    message: &'static str,
}

impl TrustedProjectFilesystemGuestTransactionError {
    #[must_use]
    pub const fn kind(self) -> TrustedProjectFilesystemGuestTransactionErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for TrustedProjectFilesystemGuestTransactionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedProjectFilesystemGuestTransactionError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for TrustedProjectFilesystemGuestTransactionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for TrustedProjectFilesystemGuestTransactionError {}

fn is_zero_digest(digest: &Sha256Digest) -> bool {
    digest.as_str() == ZERO_DIGEST
}

const fn authority_mismatch() -> TrustedProjectFilesystemGuestTransactionError {
    TrustedProjectFilesystemGuestTransactionError {
        kind: TrustedProjectFilesystemGuestTransactionErrorKind::AuthorityMismatch,
        code: "project_filesystem_guest_transaction_authority_mismatch",
        message: "guest project-filesystem transaction does not match current authority",
    }
}

#[cfg(test)]
mod tests {
    use super::TrustedProjectFilesystemGuestTransaction;
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

    fn filesystem(current: &ProjectDiskLeaseRecord, generation: u64) -> ProjectDiskFilesystemBinding {
        ProjectDiskFilesystemBinding::new(
            current,
            ProjectDiskFilesystemGeneration::new(generation).unwrap(),
            ProjectDiskFilesystemFormatProfileGeneration::new(2).unwrap(),
            ProjectDiskFilesystemKind::Ext4,
        )
    }

    fn guest(
        filesystem: &ProjectDiskFilesystemBinding,
    ) -> crate::trusted_project_filesystem_full_guest_correlation::TrustedProjectFilesystemFullGuestCorrelation {
        let observed = observe_trusted_project_filesystem_guest(
            filesystem,
            2049,
            99,
            b"/srv/project",
            b"123 45 8:1 / /srv/project rw - ext4 /dev/vda rw\n",
        )
        .unwrap();
        let block = correlate_trusted_project_block_device(&observed, 2049, 77, true, true).unwrap();
        correlate_trusted_project_filesystem_guest(observed, block).unwrap()
    }

    fn transaction(
        current: &ProjectDiskLeaseRecord,
        filesystem: &ProjectDiskFilesystemBinding,
    ) -> TrustedProjectFilesystemGuestTransaction {
        TrustedProjectFilesystemGuestTransaction::for_test(
            current,
            filesystem,
            4,
            digest('a'),
            5,
            digest('b'),
            digest('c'),
            digest('d'),
        )
        .unwrap()
    }

    #[test]
    fn exact_guest_transaction_binds_current_attachment_and_filesystem() {
        let current = current();
        let fs = filesystem(&current, 7);
        let tx = transaction(&current, &fs);
        tx.confirm(&current, &fs, &guest(&fs)).unwrap();
        assert_eq!(tx.summary().disk_revision(), current.revision());
        assert_eq!(tx.summary().filesystem_generation().get(), 7);
        assert_eq!(tx.summary().limactl_generation(), 4);
        assert_eq!(tx.summary().guest_binary_generation(), 5);
    }

    #[test]
    fn durable_revision_or_filesystem_generation_drift_expires_transaction() {
        let current = current();
        let fs = filesystem(&current, 7);
        let tx = transaction(&current, &fs);
        assert!(tx.confirm(&current.require_revalidation().unwrap(), &fs, &guest(&fs)).is_err());
        let changed_fs = filesystem(&current, 8);
        assert!(tx.confirm(&current, &changed_fs, &guest(&changed_fs)).is_err());
    }

    #[test]
    fn debug_redacts_invocation_and_result_digests() {
        let current = current();
        let fs = filesystem(&current, 7);
        let tx = transaction(&current, &fs);
        let debug = format!("{tx:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains(digest('a').as_str()));
        assert!(!debug.contains(digest('b').as_str()));
        assert!(!debug.contains(digest('c').as_str()));
        assert!(!debug.contains(digest('d').as_str()));
    }
}
