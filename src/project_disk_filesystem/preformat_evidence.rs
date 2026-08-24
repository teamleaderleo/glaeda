//! Sealed guest-side precondition for the destructive project-disk formatter boundary.
//!
//! This module defines the exact predicates that must be freshly proven before `mkfs` may be
//! issued. It performs no device I/O, signature probing, mount inspection, process execution, Lima
//! operation, format, cleanup, or proof minting. Production construction remains absent until the
//! reviewed guest observer can establish every predicate from held descriptors and bounded kernel
//! evidence.

use std::fmt;

use serde::Serialize;

use super::format_plan::ProjectDiskFilesystemFormatPlan;
use crate::artifact::Sha256Digest;
use crate::project_catalog::ProjectIdentity;
use crate::project_disk_lease::{ProjectDiskGeneration, ProjectDiskId};

pub const PROJECT_DISK_PREFORMAT_EVIDENCE_SCHEMA_VERSION: u8 = 1;
const ZERO_DIGEST: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiskPreFormatEvidenceSummary {
    schema_version: u8,
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    expected_logical_bytes: u64,
    whole_block_device: bool,
    p3_physical_identity_bound: bool,
    backing_identity_bound: bool,
    partition_table_absent: bool,
    filesystem_signature_absent: bool,
    mounted_use_absent: bool,
    swap_use_absent: bool,
    holder_use_absent: bool,
    format_plan_bound: bool,
}

impl ProjectDiskPreFormatEvidenceSummary {
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
    pub const fn expected_logical_bytes(&self) -> u64 {
        self.expected_logical_bytes
    }

    #[must_use]
    pub const fn whole_block_device(&self) -> bool {
        self.whole_block_device
    }

    #[must_use]
    pub const fn partition_table_absent(&self) -> bool {
        self.partition_table_absent
    }

    #[must_use]
    pub const fn filesystem_signature_absent(&self) -> bool {
        self.filesystem_signature_absent
    }

    #[must_use]
    pub const fn mounted_use_absent(&self) -> bool {
        self.mounted_use_absent
    }

    #[must_use]
    pub const fn swap_use_absent(&self) -> bool {
        self.swap_use_absent
    }

    #[must_use]
    pub const fn holder_use_absent(&self) -> bool {
        self.holder_use_absent
    }

    #[must_use]
    pub const fn format_plan_bound(&self) -> bool {
        self.format_plan_bound
    }
}

/// Short-lived proof that one exact P3-owned raw disk is safe to enter the formatter transaction.
///
/// The physical/backing digests and format-plan digest stay private. This value is non-serializable
/// and non-cloneable so it cannot become durable ownership by survival.
pub struct ProjectDiskPreFormatEvidence {
    summary: ProjectDiskPreFormatEvidenceSummary,
    physical_identity_digest: Sha256Digest,
    backing_identity_digest: Sha256Digest,
    format_plan_digest: Sha256Digest,
}

impl ProjectDiskPreFormatEvidence {
    #[must_use]
    pub const fn summary(&self) -> &ProjectDiskPreFormatEvidenceSummary {
        &self.summary
    }

    /// Reconfirm the evidence against the immutable format plan before a later coordinator crosses
    /// the formatter boundary.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal if project/disk identity, exact logical size, or plan digest has
    /// changed, or if any required blank/unused predicate is false.
    pub fn confirm_plan(
        &self,
        plan: &ProjectDiskFilesystemFormatPlan,
    ) -> Result<(), ProjectDiskPreFormatEvidenceError> {
        if plan.filesystem().project() != &self.summary.project
            || plan.filesystem().disk_id() != &self.summary.disk_id
            || plan.filesystem().disk_generation() != self.summary.disk_generation
            || plan.expected_logical_bytes() != self.summary.expected_logical_bytes
            || plan.plan_digest() != &self.format_plan_digest
            || !self.summary.whole_block_device
            || !self.summary.p3_physical_identity_bound
            || !self.summary.backing_identity_bound
            || !self.summary.partition_table_absent
            || !self.summary.filesystem_signature_absent
            || !self.summary.mounted_use_absent
            || !self.summary.swap_use_absent
            || !self.summary.holder_use_absent
            || !self.summary.format_plan_bound
            || self.physical_identity_digest.as_str() == ZERO_DIGEST
            || self.backing_identity_digest.as_str() == ZERO_DIGEST
        {
            return Err(evidence_mismatch());
        }
        Ok(())
    }

    /// Private identity comparison for the future P3→P4 adapter.
    #[must_use]
    pub(crate) fn matches_physical_identity(
        &self,
        physical_identity_digest: &Sha256Digest,
        backing_identity_digest: &Sha256Digest,
    ) -> bool {
        self.physical_identity_digest == *physical_identity_digest
            && self.backing_identity_digest == *backing_identity_digest
    }

    #[cfg(test)]
    fn for_test(
        plan: &ProjectDiskFilesystemFormatPlan,
        physical_identity_digest: Sha256Digest,
        backing_identity_digest: Sha256Digest,
    ) -> Self {
        Self {
            summary: ProjectDiskPreFormatEvidenceSummary {
                schema_version: PROJECT_DISK_PREFORMAT_EVIDENCE_SCHEMA_VERSION,
                project: plan.filesystem().project().clone(),
                disk_id: plan.filesystem().disk_id().clone(),
                disk_generation: plan.filesystem().disk_generation(),
                expected_logical_bytes: plan.expected_logical_bytes(),
                whole_block_device: true,
                p3_physical_identity_bound: true,
                backing_identity_bound: true,
                partition_table_absent: true,
                filesystem_signature_absent: true,
                mounted_use_absent: true,
                swap_use_absent: true,
                holder_use_absent: true,
                format_plan_bound: true,
            },
            physical_identity_digest,
            backing_identity_digest,
            format_plan_digest: plan.plan_digest().clone(),
        }
    }
}

impl fmt::Debug for ProjectDiskPreFormatEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectDiskPreFormatEvidence")
            .field("summary", &self.summary)
            .field("physical_identity", &"<redacted>")
            .field("backing_identity", &"<redacted>")
            .field("format_plan_digest", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDiskPreFormatEvidenceErrorKind {
    EvidenceMismatch,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProjectDiskPreFormatEvidenceError {
    kind: ProjectDiskPreFormatEvidenceErrorKind,
    code: &'static str,
    message: &'static str,
}

impl ProjectDiskPreFormatEvidenceError {
    #[must_use]
    pub const fn kind(self) -> ProjectDiskPreFormatEvidenceErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for ProjectDiskPreFormatEvidenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectDiskPreFormatEvidenceError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for ProjectDiskPreFormatEvidenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ProjectDiskPreFormatEvidenceError {}

const fn evidence_mismatch() -> ProjectDiskPreFormatEvidenceError {
    ProjectDiskPreFormatEvidenceError {
        kind: ProjectDiskPreFormatEvidenceErrorKind::EvidenceMismatch,
        code: "project_disk_preformat_evidence_mismatch",
        message: "project disk pre-format evidence does not match the exact blank device plan",
    }
}

#[cfg(test)]
mod tests {
    use super::ProjectDiskPreFormatEvidence;
    use crate::artifact::Sha256Digest;
    use crate::project_catalog::ProjectIdentity;
    use crate::project_disk_filesystem::format_plan::{
        ProjectDiskFilesystemFormatPlan, ProjectDiskFilesystemUuid,
        ProjectDiskFormatDurabilityPolicyGeneration, ProjectDiskFormatterArchitecture,
        ProjectDiskFormatterBinaryBinding, ProjectDiskFormatterBinaryGeneration,
        ProjectDiskFormatterGuestGeneration, ProjectDiskFormatterVersion,
    };
    use crate::project_disk_filesystem::{
        ProjectDiskFilesystemBinding, ProjectDiskFilesystemFormatProfileGeneration,
        ProjectDiskFilesystemGeneration, ProjectDiskFilesystemKind,
    };
    use crate::project_disk_lease::{ProjectDiskGeneration, ProjectDiskId};

    fn digest(byte: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    fn plan(generation: u64) -> ProjectDiskFilesystemFormatPlan {
        ProjectDiskFilesystemFormatPlan::new(
            ProjectDiskFilesystemBinding::new_for_project_disk(
                ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
                ProjectDiskId::parse("disk-a").unwrap(),
                ProjectDiskGeneration::new(3).unwrap(),
                ProjectDiskFilesystemGeneration::new(generation).unwrap(),
                ProjectDiskFilesystemFormatProfileGeneration::new(2).unwrap(),
                ProjectDiskFilesystemKind::Ext4,
            ),
            ProjectDiskFilesystemUuid::parse("01234567-89ab-cdef-0123-456789abcdef").unwrap(),
            1_073_741_824,
            digest('c'),
            ProjectDiskFormatterBinaryBinding::new(
                ProjectDiskFormatterBinaryGeneration::new(4).unwrap(),
                digest('d'),
                ProjectDiskFormatterVersion::parse("1.47.2").unwrap(),
                ProjectDiskFormatterArchitecture::LinuxAarch64,
            ),
            digest('e'),
            ProjectDiskFormatterGuestGeneration::new(5).unwrap(),
            ProjectDiskFormatDurabilityPolicyGeneration::new(3).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn exact_blank_unused_device_confirms_format_plan() {
        let plan = plan(7);
        let evidence = ProjectDiskPreFormatEvidence::for_test(&plan, digest('a'), digest('b'));
        evidence.confirm_plan(&plan).unwrap();
        assert!(evidence.summary().whole_block_device());
        assert!(evidence.summary().partition_table_absent());
        assert!(evidence.summary().filesystem_signature_absent());
        assert!(evidence.summary().mounted_use_absent());
        assert!(evidence.summary().swap_use_absent());
        assert!(evidence.summary().holder_use_absent());
        assert!(evidence.matches_physical_identity(&digest('a'), &digest('b')));
    }

    #[test]
    fn another_filesystem_generation_or_plan_is_rejected() {
        let first = plan(7);
        let second = plan(8);
        let evidence = ProjectDiskPreFormatEvidence::for_test(&first, digest('a'), digest('b'));
        assert!(evidence.confirm_plan(&second).is_err());
    }

    #[test]
    fn debug_keeps_private_identity_digests_redacted() {
        let plan = plan(7);
        let evidence = ProjectDiskPreFormatEvidence::for_test(&plan, digest('a'), digest('b'));
        let debug = format!("{evidence:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains(digest('a').as_str()));
        assert!(!debug.contains(digest('b').as_str()));
    }
}
