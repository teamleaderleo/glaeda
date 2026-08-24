//! Sealed composition of durable project-disk authority, host physical attachment evidence, and the
//! exact guest filesystem/whole-device correlation.
//!
//! `VerifiedProjectFilesystemCorrelation` is the sole value intended to sit immediately before the
//! eventual production `TrustedProjectFilesystemCorrelationProof` constructor. Production evidence
//! constructors remain absent in this slice, so the value is test-constructible only until P3/P4,
//! repaired P2, and #588 supply their reviewed adapters.

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
use crate::trusted_overlay_task_view::OverlaySourceAnchorBinding;
use crate::trusted_project_filesystem_full_guest_correlation::TrustedProjectFilesystemFullGuestCorrelation;

pub const VERIFIED_PROJECT_FILESYSTEM_CORRELATION_SCHEMA_VERSION: u8 = 1;
const ZERO_DIGEST: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000000";
const REDACTED_DEVICE: &str = "<private-exact-filesystem-device>";

/// Sealed P3 ownership evidence for the exact physical/backing identity created for one logical
/// project-disk generation.
///
/// Production construction intentionally remains absent. The future P3 adapter must consume the
/// accepted `CreatedUnformatted` provenance rather than raw caller-supplied digests.
pub struct TrustedProjectDiskCreateProvenanceEvidence {
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    physical_identity_digest: Sha256Digest,
    backing_identity_digest: Sha256Digest,
}

impl fmt::Debug for TrustedProjectDiskCreateProvenanceEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedProjectDiskCreateProvenanceEvidence")
            .field("project", &self.project)
            .field("disk_id", &self.disk_id)
            .field("disk_generation", &self.disk_generation)
            .field("physical_identity", &"<redacted>")
            .field("backing_identity", &"<redacted>")
            .finish()
    }
}

/// Sealed fresh host observation that the exact P3 physical/backing identity is currently attached
/// under the exact P1 attachment/sandbox generation and descriptor-bound resident host identity.
pub struct TrustedProjectDiskHostAttachmentEvidence {
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    disk_revision: ProjectDiskRevision,
    attachment_generation: ProjectDiskAttachmentGeneration,
    sandbox_id: ResidentSandboxId,
    sandbox_generation: ResidentSandboxGeneration,
    physical_identity_digest: Sha256Digest,
    backing_identity_digest: Sha256Digest,
    resident_host_identity_bound: bool,
}

impl fmt::Debug for TrustedProjectDiskHostAttachmentEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedProjectDiskHostAttachmentEvidence")
            .field("project", &self.project)
            .field("disk_id", &self.disk_id)
            .field("disk_generation", &self.disk_generation)
            .field("disk_revision", &self.disk_revision)
            .field("attachment_generation", &self.attachment_generation)
            .field("sandbox_id", &self.sandbox_id)
            .field("sandbox_generation", &self.sandbox_generation)
            .field("private_host_identity", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerifiedProjectFilesystemCorrelationSummary {
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
    p3_physical_provenance_bound: bool,
    resident_host_identity_bound: bool,
    guest_mount_root_bound: bool,
    guest_mountinfo_device_bound: bool,
    guest_whole_block_device_bound: bool,
    role_filesystem_device_bound: bool,
}

impl VerifiedProjectFilesystemCorrelationSummary {
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
    pub const fn role_filesystem_device_bound(&self) -> bool {
        self.role_filesystem_device_bound
    }
}

/// Fully verified short-lived correlation immediately preceding the sealed #589 proof.
///
/// Raw Linux device identity remains private. The value is non-serializable and non-cloneable so it
/// cannot become durable authority or an importable receipt.
pub struct VerifiedProjectFilesystemCorrelation {
    summary: VerifiedProjectFilesystemCorrelationSummary,
    filesystem_device: u64,
}

impl VerifiedProjectFilesystemCorrelation {
    #[must_use]
    pub const fn summary(&self) -> &VerifiedProjectFilesystemCorrelationSummary {
        &self.summary
    }

    /// Private device access for the eventual crate-private proof conversion only.
    #[must_use]
    pub(crate) const fn filesystem_device(&self) -> u64 {
        self.filesystem_device
    }
}

impl fmt::Debug for VerifiedProjectFilesystemCorrelation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedProjectFilesystemCorrelation")
            .field("summary", &self.summary)
            .field("filesystem_device", &REDACTED_DEVICE)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifiedProjectFilesystemCorrelationErrorKind {
    DurableAuthorityMismatch,
    PhysicalProvenanceMismatch,
    ResidentHostMismatch,
    FilesystemGenerationMismatch,
    GuestCorrelationMismatch,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct VerifiedProjectFilesystemCorrelationError {
    kind: VerifiedProjectFilesystemCorrelationErrorKind,
    code: &'static str,
    message: &'static str,
}

impl VerifiedProjectFilesystemCorrelationError {
    #[must_use]
    pub const fn kind(self) -> VerifiedProjectFilesystemCorrelationErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for VerifiedProjectFilesystemCorrelationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedProjectFilesystemCorrelationError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for VerifiedProjectFilesystemCorrelationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for VerifiedProjectFilesystemCorrelationError {}

/// Compose the exact durable, host, and guest evidence for one role filesystem device.
///
/// # Errors
///
/// Returns a bounded refusal unless every identity/generation agrees and the supplied role
/// descriptor device is the same whole device already proven by the guest correlation.
pub fn verify_project_filesystem_correlation(
    current: &ProjectDiskLeaseRecord,
    filesystem: &ProjectDiskFilesystemBinding,
    anchor: &OverlaySourceAnchorBinding,
    create: &TrustedProjectDiskCreateProvenanceEvidence,
    host: &TrustedProjectDiskHostAttachmentEvidence,
    guest: &TrustedProjectFilesystemFullGuestCorrelation,
    observed_role_filesystem_device: u64,
) -> Result<VerifiedProjectFilesystemCorrelation, VerifiedProjectFilesystemCorrelationError> {
    let ProjectDiskLeaseState::Attached { attachment } = current.state() else {
        return Err(durable_authority_mismatch());
    };
    if current.project() != anchor.project()
        || current.disk_id() != anchor.disk_id()
        || current.disk_generation() != anchor.disk_generation()
        || current.revision() != anchor.disk_revision()
        || attachment.generation() != anchor.attachment_generation()
        || attachment.sandbox_id() != anchor.resident_sandbox_id()
        || attachment.sandbox_generation() != anchor.resident_sandbox_generation()
    {
        return Err(durable_authority_mismatch());
    }
    if create.project != *current.project()
        || create.disk_id != *current.disk_id()
        || create.disk_generation != current.disk_generation()
        || host.project != *current.project()
        || host.disk_id != *current.disk_id()
        || host.disk_generation != current.disk_generation()
    {
        return Err(physical_provenance_mismatch());
    }
    if create.physical_identity_digest != host.physical_identity_digest
        || create.backing_identity_digest != host.backing_identity_digest
        || create.physical_identity_digest.as_str() == ZERO_DIGEST
        || create.backing_identity_digest.as_str() == ZERO_DIGEST
    {
        return Err(physical_provenance_mismatch());
    }
    if host.disk_revision != current.revision()
        || host.attachment_generation != attachment.generation()
        || host.sandbox_id != *attachment.sandbox_id()
        || host.sandbox_generation != attachment.sandbox_generation()
        || !host.resident_host_identity_bound
    {
        return Err(resident_host_mismatch());
    }
    if !filesystem.matches_project_disk(current) {
        return Err(filesystem_generation_mismatch());
    }
    let guest_summary = guest.summary();
    if guest_summary.filesystem_generation() != filesystem.filesystem_generation()
        || guest_summary.format_profile_generation() != filesystem.format_profile_generation()
        || guest_summary.filesystem_kind() != filesystem.kind()
        || !guest_summary.mount_root_identity_bound()
        || !guest_summary.mountinfo_device_bound()
        || !guest_summary.whole_block_device_bound()
    {
        return Err(guest_correlation_mismatch());
    }
    if !guest.matches_role_filesystem_device(observed_role_filesystem_device) {
        return Err(guest_correlation_mismatch());
    }
    Ok(VerifiedProjectFilesystemCorrelation {
        summary: VerifiedProjectFilesystemCorrelationSummary {
            schema_version: VERIFIED_PROJECT_FILESYSTEM_CORRELATION_SCHEMA_VERSION,
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
            p3_physical_provenance_bound: true,
            resident_host_identity_bound: true,
            guest_mount_root_bound: true,
            guest_mountinfo_device_bound: true,
            guest_whole_block_device_bound: true,
            role_filesystem_device_bound: true,
        },
        filesystem_device: observed_role_filesystem_device,
    })
}

const fn error(
    kind: VerifiedProjectFilesystemCorrelationErrorKind,
    code: &'static str,
    message: &'static str,
) -> VerifiedProjectFilesystemCorrelationError {
    VerifiedProjectFilesystemCorrelationError {
        kind,
        code,
        message,
    }
}

const fn durable_authority_mismatch() -> VerifiedProjectFilesystemCorrelationError {
    error(
        VerifiedProjectFilesystemCorrelationErrorKind::DurableAuthorityMismatch,
        "verified_project_filesystem_durable_authority_mismatch",
        "current project-disk authority does not match the correlation request",
    )
}

const fn physical_provenance_mismatch() -> VerifiedProjectFilesystemCorrelationError {
    error(
        VerifiedProjectFilesystemCorrelationErrorKind::PhysicalProvenanceMismatch,
        "verified_project_filesystem_physical_provenance_mismatch",
        "project-disk physical provenance does not match current host evidence",
    )
}

const fn resident_host_mismatch() -> VerifiedProjectFilesystemCorrelationError {
    error(
        VerifiedProjectFilesystemCorrelationErrorKind::ResidentHostMismatch,
        "verified_project_filesystem_resident_host_mismatch",
        "current resident host attachment evidence does not match durable authority",
    )
}

const fn filesystem_generation_mismatch() -> VerifiedProjectFilesystemCorrelationError {
    error(
        VerifiedProjectFilesystemCorrelationErrorKind::FilesystemGenerationMismatch,
        "verified_project_filesystem_generation_mismatch",
        "accepted filesystem generation does not match the current project disk",
    )
}

const fn guest_correlation_mismatch() -> VerifiedProjectFilesystemCorrelationError {
    error(
        VerifiedProjectFilesystemCorrelationErrorKind::GuestCorrelationMismatch,
        "verified_project_filesystem_guest_correlation_mismatch",
        "guest project-filesystem correlation does not match the accepted filesystem",
    )
}

#[cfg(test)]
mod tests {
    use super::{
        TrustedProjectDiskCreateProvenanceEvidence, TrustedProjectDiskHostAttachmentEvidence,
        VerifiedProjectFilesystemCorrelationErrorKind, verify_project_filesystem_correlation,
    };
    use crate::artifact::{CommitId, GitTreeId, Sha256Digest};
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
    use crate::trusted_overlay_task_view::{
        OverlaySourceAnchorBinding, OverlaySourceAnchorGeneration, OverlaySourceAnchorId,
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

    fn anchor(current: &ProjectDiskLeaseRecord) -> OverlaySourceAnchorBinding {
        let ProjectDiskLeaseState::Attached { attachment } = current.state() else {
            panic!("expected attached record");
        };
        OverlaySourceAnchorBinding::new(
            current.project().clone(),
            current.disk_id().clone(),
            current.disk_generation(),
            current.revision(),
            attachment.sandbox_id().clone(),
            attachment.sandbox_generation(),
            attachment.generation(),
            OverlaySourceAnchorId::parse("anchor-a").unwrap(),
            OverlaySourceAnchorGeneration::new(5).unwrap(),
            CommitId::parse("0123456789abcdef0123456789abcdef01234567").unwrap(),
            GitTreeId::parse("89abcdef0123456789abcdef0123456789abcdef").unwrap(),
            digest('a'),
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

    fn create(current: &ProjectDiskLeaseRecord) -> TrustedProjectDiskCreateProvenanceEvidence {
        TrustedProjectDiskCreateProvenanceEvidence {
            project: current.project().clone(),
            disk_id: current.disk_id().clone(),
            disk_generation: current.disk_generation(),
            physical_identity_digest: digest('b'),
            backing_identity_digest: digest('c'),
        }
    }

    fn host(current: &ProjectDiskLeaseRecord) -> TrustedProjectDiskHostAttachmentEvidence {
        let ProjectDiskLeaseState::Attached { attachment } = current.state() else {
            panic!("expected attached record");
        };
        TrustedProjectDiskHostAttachmentEvidence {
            project: current.project().clone(),
            disk_id: current.disk_id().clone(),
            disk_generation: current.disk_generation(),
            disk_revision: current.revision(),
            attachment_generation: attachment.generation(),
            sandbox_id: attachment.sandbox_id().clone(),
            sandbox_generation: attachment.sandbox_generation(),
            physical_identity_digest: digest('b'),
            backing_identity_digest: digest('c'),
            resident_host_identity_bound: true,
        }
    }

    #[test]
    fn exact_full_chain_produces_one_verified_value() {
        let current = current();
        let fs = filesystem(&current, 7);
        let verified = verify_project_filesystem_correlation(
            &current,
            &fs,
            &anchor(&current),
            &create(&current),
            &host(&current),
            &guest(&fs),
            2049,
        )
        .unwrap();
        assert_eq!(verified.summary().disk_revision(), current.revision());
        assert_eq!(verified.summary().filesystem_generation().get(), 7);
        assert!(verified.summary().role_filesystem_device_bound());
        assert_eq!(verified.filesystem_device(), 2049);
        let debug = format!("{verified:?}");
        assert!(debug.contains(REDACTED_DEVICE));
        assert!(!debug.contains("2049"));
    }

    #[test]
    fn stale_durable_revision_or_attachment_is_rejected() {
        let current = current();
        let fs = filesystem(&current, 7);
        let revalidate = current.require_revalidation().unwrap();
        let error = verify_project_filesystem_correlation(
            &revalidate,
            &fs,
            &anchor(&current),
            &create(&current),
            &host(&current),
            &guest(&fs),
            2049,
        )
        .unwrap_err();
        assert_eq!(
            error.kind(),
            VerifiedProjectFilesystemCorrelationErrorKind::DurableAuthorityMismatch
        );
    }

    #[test]
    fn physical_identity_mismatch_blocks_same_name_adoption() {
        let current = current();
        let fs = filesystem(&current, 7);
        let mut wrong = host(&current);
        wrong.physical_identity_digest = digest('d');
        let error = verify_project_filesystem_correlation(
            &current,
            &fs,
            &anchor(&current),
            &create(&current),
            &wrong,
            &guest(&fs),
            2049,
        )
        .unwrap_err();
        assert_eq!(
            error.kind(),
            VerifiedProjectFilesystemCorrelationErrorKind::PhysicalProvenanceMismatch
        );
    }

    #[test]
    fn wrong_filesystem_generation_is_rejected() {
        let current = current();
        let accepted = filesystem(&current, 7);
        let wrong_guest_fs = filesystem(&current, 8);
        let error = verify_project_filesystem_correlation(
            &current,
            &accepted,
            &anchor(&current),
            &create(&current),
            &host(&current),
            &guest(&wrong_guest_fs),
            2049,
        )
        .unwrap_err();
        assert_eq!(
            error.kind(),
            VerifiedProjectFilesystemCorrelationErrorKind::GuestCorrelationMismatch
        );
    }

    #[test]
    fn wrong_role_device_is_rejected() {
        let current = current();
        let fs = filesystem(&current, 7);
        let error = verify_project_filesystem_correlation(
            &current,
            &fs,
            &anchor(&current),
            &create(&current),
            &host(&current),
            &guest(&fs),
            2050,
        )
        .unwrap_err();
        assert_eq!(
            error.kind(),
            VerifiedProjectFilesystemCorrelationErrorKind::GuestCorrelationMismatch
        );
    }
}
