//! Sealed admission of one accepted P3/P4 formatted-detached project disk into the P1 lease model.
//!
//! `ProjectDiskLeaseRecord::new_detached` remains useful as a pure model constructor, but product
//! composition should not call it directly. The eventual P4 format adapter must first mint the
//! non-serializable proof below from exact P3 physical provenance + accepted format receipt + fresh
//! detached/unused observation; only then may this module publish the initial P1 detached lease.

use std::fmt;

use serde::Serialize;

use crate::artifact::Sha256Digest;
use crate::project_catalog::ProjectIdentity;
use crate::project_disk_filesystem::{
    ProjectDiskFilesystemBinding, ProjectDiskFilesystemFormatProfileGeneration,
    ProjectDiskFilesystemGeneration, ProjectDiskFilesystemKind,
};
use crate::project_disk_lease::{
    ProjectDiskGeneration, ProjectDiskId, ProjectDiskLeaseRecord, ProjectDiskLeaseState,
};

pub const PROJECT_DISK_FORMATTED_LEASE_ADMISSION_SCHEMA_VERSION: u8 = 1;
const ZERO_DIGEST: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiskFormattedLeaseAdmissionSummary {
    schema_version: u8,
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    filesystem_generation: ProjectDiskFilesystemGeneration,
    format_profile_generation: ProjectDiskFilesystemFormatProfileGeneration,
    filesystem_kind: ProjectDiskFilesystemKind,
    p3_physical_identity_bound: bool,
    p3_backing_identity_bound: bool,
    format_outcome_bound: bool,
    exact_detached_unused: bool,
}

impl ProjectDiskFormattedLeaseAdmissionSummary {
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
}

/// Short-lived proof that one exact project-disk generation has completed P3 create + P4 format and
/// is freshly observed detached/unused.
///
/// Physical/backing/format digests remain private. Production construction is intentionally absent
/// until the P3/P4/P2 adapters exist.
pub struct ProjectDiskFormattedLeaseAdmissionProof {
    summary: ProjectDiskFormattedLeaseAdmissionSummary,
    filesystem: ProjectDiskFilesystemBinding,
    physical_identity_digest: Sha256Digest,
    backing_identity_digest: Sha256Digest,
    format_outcome_digest: Sha256Digest,
}

impl ProjectDiskFormattedLeaseAdmissionProof {
    #[must_use]
    pub const fn summary(&self) -> &ProjectDiskFormattedLeaseAdmissionSummary {
        &self.summary
    }

    #[cfg(test)]
    fn for_test(
        filesystem: ProjectDiskFilesystemBinding,
        physical_identity_digest: Sha256Digest,
        backing_identity_digest: Sha256Digest,
        format_outcome_digest: Sha256Digest,
    ) -> Result<Self, ProjectDiskFormattedLeaseAdmissionError> {
        if is_zero_digest(&physical_identity_digest)
            || is_zero_digest(&backing_identity_digest)
            || is_zero_digest(&format_outcome_digest)
        {
            return Err(invalid_evidence());
        }
        Ok(Self {
            summary: ProjectDiskFormattedLeaseAdmissionSummary {
                schema_version: PROJECT_DISK_FORMATTED_LEASE_ADMISSION_SCHEMA_VERSION,
                project: filesystem.project().clone(),
                disk_id: filesystem.disk_id().clone(),
                disk_generation: filesystem.disk_generation(),
                filesystem_generation: filesystem.filesystem_generation(),
                format_profile_generation: filesystem.format_profile_generation(),
                filesystem_kind: filesystem.kind(),
                p3_physical_identity_bound: true,
                p3_backing_identity_bound: true,
                format_outcome_bound: true,
                exact_detached_unused: true,
            },
            filesystem,
            physical_identity_digest,
            backing_identity_digest,
            format_outcome_digest,
        })
    }
}

impl fmt::Debug for ProjectDiskFormattedLeaseAdmissionProof {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectDiskFormattedLeaseAdmissionProof")
            .field("summary", &self.summary)
            .field("physical_identity", &"<redacted>")
            .field("backing_identity", &"<redacted>")
            .field("format_outcome", &"<redacted>")
            .finish()
    }
}

/// Initial P1 lease plus the exact accepted filesystem generation admitted with it.
pub struct ProjectDiskFormattedLeaseAdmission {
    lease: ProjectDiskLeaseRecord,
    filesystem: ProjectDiskFilesystemBinding,
}

impl ProjectDiskFormattedLeaseAdmission {
    #[must_use]
    pub const fn lease(&self) -> &ProjectDiskLeaseRecord {
        &self.lease
    }

    #[must_use]
    pub const fn filesystem(&self) -> &ProjectDiskFilesystemBinding {
        &self.filesystem
    }

    #[must_use]
    pub fn into_parts(self) -> (ProjectDiskLeaseRecord, ProjectDiskFilesystemBinding) {
        (self.lease, self.filesystem)
    }
}

impl fmt::Debug for ProjectDiskFormattedLeaseAdmission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectDiskFormattedLeaseAdmission")
            .field("lease", &self.lease)
            .field("filesystem", &self.filesystem)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDiskFormattedLeaseAdmissionErrorKind {
    InvalidEvidence,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProjectDiskFormattedLeaseAdmissionError {
    kind: ProjectDiskFormattedLeaseAdmissionErrorKind,
    code: &'static str,
    message: &'static str,
}

impl ProjectDiskFormattedLeaseAdmissionError {
    #[must_use]
    pub const fn kind(self) -> ProjectDiskFormattedLeaseAdmissionErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for ProjectDiskFormattedLeaseAdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectDiskFormattedLeaseAdmissionError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for ProjectDiskFormattedLeaseAdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ProjectDiskFormattedLeaseAdmissionError {}

/// Consume one sealed formatted-detached proof and publish the initial P1 detached lease.
///
/// Product code cannot call this successfully until a real P3/P4/P2 adapter can construct the
/// proof. No physical mutation happens here.
pub fn admit_formatted_project_disk(
    proof: ProjectDiskFormattedLeaseAdmissionProof,
) -> Result<ProjectDiskFormattedLeaseAdmission, ProjectDiskFormattedLeaseAdmissionError> {
    if !proof.summary.p3_physical_identity_bound
        || !proof.summary.p3_backing_identity_bound
        || !proof.summary.format_outcome_bound
        || !proof.summary.exact_detached_unused
        || is_zero_digest(&proof.physical_identity_digest)
        || is_zero_digest(&proof.backing_identity_digest)
        || is_zero_digest(&proof.format_outcome_digest)
        || proof.filesystem.project() != &proof.summary.project
        || proof.filesystem.disk_id() != &proof.summary.disk_id
        || proof.filesystem.disk_generation() != proof.summary.disk_generation
        || proof.filesystem.filesystem_generation() != proof.summary.filesystem_generation
        || proof.filesystem.format_profile_generation() != proof.summary.format_profile_generation
        || proof.filesystem.kind() != proof.summary.filesystem_kind
    {
        return Err(invalid_evidence());
    }
    let lease = ProjectDiskLeaseRecord::new_detached(
        proof.summary.project,
        proof.summary.disk_id,
        proof.summary.disk_generation,
    );
    if !matches!(lease.state(), ProjectDiskLeaseState::Detached) {
        return Err(invalid_evidence());
    }
    Ok(ProjectDiskFormattedLeaseAdmission {
        lease,
        filesystem: proof.filesystem,
    })
}

fn is_zero_digest(digest: &Sha256Digest) -> bool {
    digest.as_str() == ZERO_DIGEST
}

const fn invalid_evidence() -> ProjectDiskFormattedLeaseAdmissionError {
    ProjectDiskFormattedLeaseAdmissionError {
        kind: ProjectDiskFormattedLeaseAdmissionErrorKind::InvalidEvidence,
        code: "project_disk_formatted_lease_admission_invalid",
        message: "formatted project-disk lease admission evidence is invalid",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ProjectDiskFormattedLeaseAdmissionProof, ZERO_DIGEST, admit_formatted_project_disk,
    };
    use crate::artifact::Sha256Digest;
    use crate::project_catalog::ProjectIdentity;
    use crate::project_disk_filesystem::{
        ProjectDiskFilesystemBinding, ProjectDiskFilesystemFormatProfileGeneration,
        ProjectDiskFilesystemGeneration, ProjectDiskFilesystemKind,
    };
    use crate::project_disk_lease::{ProjectDiskGeneration, ProjectDiskId, ProjectDiskLeaseState};

    fn digest(byte: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    fn filesystem() -> ProjectDiskFilesystemBinding {
        ProjectDiskFilesystemBinding::new_for_project_disk(
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            ProjectDiskId::parse("disk-a").unwrap(),
            ProjectDiskGeneration::new(3).unwrap(),
            ProjectDiskFilesystemGeneration::new(7).unwrap(),
            ProjectDiskFilesystemFormatProfileGeneration::new(2).unwrap(),
            ProjectDiskFilesystemKind::Ext4,
        )
    }

    #[test]
    fn sealed_formatted_detached_proof_admits_initial_p1_lease() {
        let proof = ProjectDiskFormattedLeaseAdmissionProof::for_test(
            filesystem(),
            digest('a'),
            digest('b'),
            digest('c'),
        )
        .unwrap();
        let admission = admit_formatted_project_disk(proof).unwrap();
        assert!(matches!(
            admission.lease().state(),
            ProjectDiskLeaseState::Detached
        ));
        assert_eq!(admission.lease().revision().get(), 1);
        assert_eq!(admission.filesystem().filesystem_generation().get(), 7);
        assert!(
            admission
                .filesystem()
                .matches_project_disk(admission.lease())
        );
    }

    #[test]
    fn raw_zero_identity_cannot_be_admitted() {
        let zero = Sha256Digest::parse(ZERO_DIGEST).unwrap();
        assert!(
            ProjectDiskFormattedLeaseAdmissionProof::for_test(
                filesystem(),
                zero,
                digest('b'),
                digest('c'),
            )
            .is_err()
        );
    }

    #[test]
    fn debug_keeps_private_provenance_redacted() {
        let proof = ProjectDiskFormattedLeaseAdmissionProof::for_test(
            filesystem(),
            digest('a'),
            digest('b'),
            digest('c'),
        )
        .unwrap();
        let debug = format!("{proof:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains(digest('a').as_str()));
        assert!(!debug.contains(digest('b').as_str()));
    }
}
