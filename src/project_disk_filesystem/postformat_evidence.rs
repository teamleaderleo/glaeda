//! Sealed guest-side evidence that one completed formatter transaction produced exactly the
//! controller-requested project filesystem generation on the intended whole block device.
//!
//! This module performs no block-device I/O, formatter execution, mount, Lima operation, cleanup,
//! or proof minting. Production construction remains absent until the reviewed guest observer can
//! establish the exact filesystem metadata from held device evidence after `mkfs`.

use std::fmt;

use serde::Serialize;

use super::format_plan::{ProjectDiskFilesystemFormatPlan, ProjectDiskFilesystemUuid};
use super::{
    ProjectDiskFilesystemFormatProfileGeneration, ProjectDiskFilesystemGeneration,
    ProjectDiskFilesystemKind,
};
use crate::artifact::Sha256Digest;
use crate::project_catalog::ProjectIdentity;
use crate::project_disk_lease::{ProjectDiskGeneration, ProjectDiskId};

pub const PROJECT_DISK_POSTFORMAT_EVIDENCE_SCHEMA_VERSION: u8 = 1;
const ZERO_DIGEST: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000000";
const REDACTED_BLOCK_DEVICE: &str = "<private-postformat-block-device>";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiskPostFormatEvidenceSummary {
    schema_version: u8,
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    filesystem_generation: ProjectDiskFilesystemGeneration,
    format_profile_generation: ProjectDiskFilesystemFormatProfileGeneration,
    filesystem_kind: ProjectDiskFilesystemKind,
    filesystem_uuid: ProjectDiskFilesystemUuid,
    feature_policy_digest: Sha256Digest,
    logical_bytes: u64,
    whole_block_device: bool,
    partition_table_absent: bool,
    filesystem_signature_exact: bool,
    mounted_use_absent: bool,
    swap_use_absent: bool,
    format_plan_bound: bool,
}

impl ProjectDiskPostFormatEvidenceSummary {
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
    pub const fn filesystem_kind(&self) -> ProjectDiskFilesystemKind {
        self.filesystem_kind
    }

    #[must_use]
    pub const fn filesystem_uuid(&self) -> &ProjectDiskFilesystemUuid {
        &self.filesystem_uuid
    }

    #[must_use]
    pub const fn feature_policy_digest(&self) -> &Sha256Digest {
        &self.feature_policy_digest
    }

    #[must_use]
    pub const fn logical_bytes(&self) -> u64 {
        self.logical_bytes
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
    pub const fn filesystem_signature_exact(&self) -> bool {
        self.filesystem_signature_exact
    }

    #[must_use]
    pub const fn mounted_use_absent(&self) -> bool {
        self.mounted_use_absent
    }

    #[must_use]
    pub const fn swap_use_absent(&self) -> bool {
        self.swap_use_absent
    }
}

/// Short-lived exact observation of the formatted whole block device.
///
/// Raw block `st_rdev`/inode plus the observation digest stay private. The value is deliberately
/// non-serializable and non-cloneable so later P4 state must consume fresh evidence instead of
/// importing a historical public receipt.
pub struct ProjectDiskPostFormatEvidence {
    summary: ProjectDiskPostFormatEvidenceSummary,
    block_device_rdev: u64,
    block_device_inode: u64,
    format_plan_digest: Sha256Digest,
    observation_digest: Sha256Digest,
}

impl ProjectDiskPostFormatEvidence {
    #[must_use]
    pub const fn summary(&self) -> &ProjectDiskPostFormatEvidenceSummary {
        &self.summary
    }

    /// Reconfirm this post-format observation against the immutable formatter plan.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal for any project/disk/filesystem generation, UUID, feature policy,
    /// logical-size, or format-plan mismatch, or when the observed device is not the exact whole,
    /// partitionless, unused filesystem requested by the plan.
    pub fn confirm_plan(
        &self,
        plan: &ProjectDiskFilesystemFormatPlan,
    ) -> Result<(), ProjectDiskPostFormatEvidenceError> {
        let filesystem = plan.filesystem();
        if filesystem.project() != &self.summary.project
            || filesystem.disk_id() != &self.summary.disk_id
            || filesystem.disk_generation() != self.summary.disk_generation
            || filesystem.filesystem_generation() != self.summary.filesystem_generation
            || filesystem.format_profile_generation() != self.summary.format_profile_generation
            || filesystem.kind() != self.summary.filesystem_kind
            || plan.filesystem_uuid() != &self.summary.filesystem_uuid
            || plan.feature_policy_digest() != &self.summary.feature_policy_digest
            || plan.expected_logical_bytes() != self.summary.logical_bytes
            || plan.plan_digest() != &self.format_plan_digest
            || !self.summary.whole_block_device
            || !self.summary.partition_table_absent
            || !self.summary.filesystem_signature_exact
            || !self.summary.mounted_use_absent
            || !self.summary.swap_use_absent
            || !self.summary.format_plan_bound
            || self.block_device_inode == 0
            || self.observation_digest.as_str() == ZERO_DIGEST
        {
            return Err(evidence_mismatch());
        }
        Ok(())
    }

    /// Revalidate the exact whole block-node identity captured by the post-format observer.
    #[must_use]
    pub const fn matches_block_device_identity(
        &self,
        observed_rdev: u64,
        observed_inode: u64,
    ) -> bool {
        observed_inode != 0
            && self.block_device_rdev == observed_rdev
            && self.block_device_inode == observed_inode
    }

    #[must_use]
    pub(crate) const fn observation_digest(&self) -> &Sha256Digest {
        &self.observation_digest
    }

    #[cfg(test)]
    fn for_test(
        plan: &ProjectDiskFilesystemFormatPlan,
        block_device_rdev: u64,
        block_device_inode: u64,
        observation_digest: Sha256Digest,
    ) -> Result<Self, ProjectDiskPostFormatEvidenceError> {
        if block_device_inode == 0 || observation_digest.as_str() == ZERO_DIGEST {
            return Err(evidence_mismatch());
        }
        Ok(Self {
            summary: ProjectDiskPostFormatEvidenceSummary {
                schema_version: PROJECT_DISK_POSTFORMAT_EVIDENCE_SCHEMA_VERSION,
                project: plan.filesystem().project().clone(),
                disk_id: plan.filesystem().disk_id().clone(),
                disk_generation: plan.filesystem().disk_generation(),
                filesystem_generation: plan.filesystem().filesystem_generation(),
                format_profile_generation: plan.filesystem().format_profile_generation(),
                filesystem_kind: plan.filesystem().kind(),
                filesystem_uuid: plan.filesystem_uuid().clone(),
                feature_policy_digest: plan.feature_policy_digest().clone(),
                logical_bytes: plan.expected_logical_bytes(),
                whole_block_device: true,
                partition_table_absent: true,
                filesystem_signature_exact: true,
                mounted_use_absent: true,
                swap_use_absent: true,
                format_plan_bound: true,
            },
            block_device_rdev,
            block_device_inode,
            format_plan_digest: plan.plan_digest().clone(),
            observation_digest,
        })
    }
}

impl fmt::Debug for ProjectDiskPostFormatEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectDiskPostFormatEvidence")
            .field("summary", &self.summary)
            .field("block_device", &REDACTED_BLOCK_DEVICE)
            .field("format_plan_digest", &"<redacted>")
            .field("observation_digest", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDiskPostFormatEvidenceErrorKind {
    EvidenceMismatch,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProjectDiskPostFormatEvidenceError {
    kind: ProjectDiskPostFormatEvidenceErrorKind,
    code: &'static str,
    message: &'static str,
}

impl ProjectDiskPostFormatEvidenceError {
    #[must_use]
    pub const fn kind(self) -> ProjectDiskPostFormatEvidenceErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for ProjectDiskPostFormatEvidenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectDiskPostFormatEvidenceError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for ProjectDiskPostFormatEvidenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ProjectDiskPostFormatEvidenceError {}

const fn evidence_mismatch() -> ProjectDiskPostFormatEvidenceError {
    ProjectDiskPostFormatEvidenceError {
        kind: ProjectDiskPostFormatEvidenceErrorKind::EvidenceMismatch,
        code: "project_disk_postformat_evidence_mismatch",
        message: "post-format filesystem evidence does not match the exact formatter plan",
    }
}

#[cfg(test)]
mod tests {
    use super::ProjectDiskPostFormatEvidence;
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
    fn exact_postformat_observation_confirms_requested_filesystem() {
        let plan = plan(7);
        let evidence = ProjectDiskPostFormatEvidence::for_test(&plan, 2049, 77, digest('f')).unwrap();
        evidence.confirm_plan(&plan).unwrap();
        assert_eq!(evidence.summary().filesystem_generation().get(), 7);
        assert_eq!(
            evidence.summary().filesystem_uuid().as_str(),
            "01234567-89ab-cdef-0123-456789abcdef"
        );
        assert_eq!(evidence.summary().feature_policy_digest(), &digest('c'));
        assert_eq!(evidence.summary().logical_bytes(), 1_073_741_824);
        assert!(evidence.matches_block_device_identity(2049, 77));
    }

    #[test]
    fn another_generation_or_plan_is_rejected() {
        let first = plan(7);
        let second = plan(8);
        let evidence = ProjectDiskPostFormatEvidence::for_test(&first, 2049, 77, digest('f')).unwrap();
        assert!(evidence.confirm_plan(&second).is_err());
    }

    #[test]
    fn block_node_replacement_is_detected() {
        let plan = plan(7);
        let evidence = ProjectDiskPostFormatEvidence::for_test(&plan, 2049, 77, digest('f')).unwrap();
        assert!(!evidence.matches_block_device_identity(2049, 78));
        assert!(!evidence.matches_block_device_identity(2050, 77));
    }

    #[test]
    fn debug_keeps_block_and_digests_private() {
        let plan = plan(7);
        let evidence = ProjectDiskPostFormatEvidence::for_test(&plan, 2049, 77, digest('f')).unwrap();
        let debug = format!("{evidence:?}");
        assert!(debug.contains(REDACTED_BLOCK_DEVICE));
        assert!(!debug.contains("2049"));
        assert!(!debug.contains("77"));
        assert!(!debug.contains(digest('f').as_str()));
    }
}
