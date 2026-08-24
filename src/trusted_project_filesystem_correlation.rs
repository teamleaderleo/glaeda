use std::fmt;

use serde::Serialize;

use crate::project_catalog::ProjectIdentity;
use crate::project_disk_lease::{
    ProjectDiskAttachmentGeneration, ProjectDiskGeneration, ProjectDiskId, ResidentSandboxGeneration,
    ResidentSandboxId,
};

pub const TRUSTED_PROJECT_FILESYSTEM_CORRELATION_SCHEMA_VERSION: u8 = 1;

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
            return Err(TrustedProjectFilesystemCorrelationError);
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

/// Opaque proof that one currently observed Linux filesystem device is the exact filesystem owned by
/// one accepted project-disk attachment generation.
///
/// This type deliberately has no public or crate-visible production constructor in P1. Until #565
/// read-only physical project-disk observation lands, normal SmolRunner code cannot mint one and the
/// future privileged OverlayFS executor therefore remains unusable from production composition.
///
/// The raw filesystem device number is intentionally absent from public summaries, Debug, Clone, and
/// serialization. It is kernel-local evidence, not a durable product identity.
pub struct TrustedProjectFilesystemCorrelationProof {
    summary: TrustedProjectFilesystemCorrelationSummary,
    filesystem_device: u64,
}

impl TrustedProjectFilesystemCorrelationProof {
    #[must_use]
    pub const fn summary(&self) -> &TrustedProjectFilesystemCorrelationSummary {
        &self.summary
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
pub struct TrustedProjectFilesystemCorrelationError;

impl fmt::Display for TrustedProjectFilesystemCorrelationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("trusted project-filesystem correlation generation is invalid")
    }
}

impl std::error::Error for TrustedProjectFilesystemCorrelationError {}

#[cfg(test)]
mod tests {
    use super::{
        TRUSTED_PROJECT_FILESYSTEM_CORRELATION_SCHEMA_VERSION,
        TrustedProjectFilesystemCorrelationGeneration, TrustedProjectFilesystemCorrelationProof,
        TrustedProjectFilesystemCorrelationSummary,
    };
    use crate::project_catalog::ProjectIdentity;
    use crate::project_disk_lease::{
        ProjectDiskAttachmentGeneration, ProjectDiskGeneration, ProjectDiskId,
        ResidentSandboxGeneration, ResidentSandboxId,
    };

    fn proof(device: u64) -> TrustedProjectFilesystemCorrelationProof {
        TrustedProjectFilesystemCorrelationProof {
            summary: TrustedProjectFilesystemCorrelationSummary {
                schema_version: TRUSTED_PROJECT_FILESYSTEM_CORRELATION_SCHEMA_VERSION,
                project: ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
                disk_id: ProjectDiskId::parse("disk-a").unwrap(),
                disk_generation: ProjectDiskGeneration::new(3).unwrap(),
                attachment_generation: ProjectDiskAttachmentGeneration::new(7).unwrap(),
                sandbox_id: ResidentSandboxId::parse("sandbox-a").unwrap(),
                sandbox_generation: ResidentSandboxGeneration::new(11).unwrap(),
                correlation_generation: TrustedProjectFilesystemCorrelationGeneration::new(13)
                    .unwrap(),
                filesystem_device_bound: true,
            },
            filesystem_device: device,
        }
    }

    #[test]
    fn correlation_generation_must_be_positive() {
        assert!(TrustedProjectFilesystemCorrelationGeneration::new(0).is_err());
        assert_eq!(
            TrustedProjectFilesystemCorrelationGeneration::new(1)
                .unwrap()
                .get(),
            1
        );
    }

    #[test]
    fn proof_summary_binds_exact_logical_attachment_dimensions() {
        let proof = proof(0xfeed_beef);
        let summary = proof.summary();
        assert_eq!(summary.schema_version(), 1);
        assert_eq!(summary.project().as_str(), "github.com/teamleaderleo/smolrunner");
        assert_eq!(summary.disk_id().as_str(), "disk-a");
        assert_eq!(summary.disk_generation().get(), 3);
        assert_eq!(summary.attachment_generation().get(), 7);
        assert_eq!(summary.sandbox_id().as_str(), "sandbox-a");
        assert_eq!(summary.sandbox_generation().get(), 11);
        assert_eq!(summary.correlation_generation().get(), 13);
        assert!(summary.filesystem_device_bound());
    }

    #[test]
    fn debug_redacts_raw_filesystem_device() {
        let proof = proof(123_456_789);
        let debug = format!("{proof:?}");
        assert!(!debug.contains("123456789"));
        assert!(debug.contains("<private exact filesystem device>"));
    }
}
