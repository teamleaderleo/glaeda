//! Exact-current wrapper for the private #628 full-correlation receipt validator.
//!
//! An internally consistent historical receipt is insufficient for acceptance. This wrapper binds
//! validation to the exact reviewed capture commit, installed Lima version, and current durable
//! project/disk/attachment/sandbox/filesystem tuple selected by the rerun coordinator.

use std::fmt;

use serde::Serialize;

use crate::project_catalog::ProjectIdentity;
use crate::project_disk_filesystem::{
    ProjectDiskFilesystemFormatProfileGeneration, ProjectDiskFilesystemGeneration,
};
use crate::project_disk_lease::{
    ProjectDiskAttachmentGeneration, ProjectDiskGeneration, ProjectDiskId, ProjectDiskRevision,
    ResidentSandboxGeneration, ResidentSandboxId,
};
use crate::project_filesystem_correlation_receipt::{
    ProjectFilesystemCorrelationReceiptError, ProjectFilesystemCorrelationReceiptReport,
    ProjectFilesystemCorrelationVerdict, ProjectFilesystemReceiptKind,
    validate_project_filesystem_correlation_receipt_json,
};

const MAX_COMMIT_BYTES: usize = 40;
const MAX_VERSION_BYTES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectFilesystemCorrelationReceiptExpectation {
    capture_commit: String,
    lima_version: String,
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    disk_revision: ProjectDiskRevision,
    attachment_generation: ProjectDiskAttachmentGeneration,
    sandbox_id: ResidentSandboxId,
    sandbox_generation: ResidentSandboxGeneration,
    filesystem_generation: ProjectDiskFilesystemGeneration,
    format_profile_generation: ProjectDiskFilesystemFormatProfileGeneration,
    filesystem_kind: ProjectFilesystemReceiptKind,
}

impl ProjectFilesystemCorrelationReceiptExpectation {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        capture_commit: impl Into<String>,
        lima_version: impl Into<String>,
        project: ProjectIdentity,
        disk_id: ProjectDiskId,
        disk_generation: ProjectDiskGeneration,
        disk_revision: ProjectDiskRevision,
        attachment_generation: ProjectDiskAttachmentGeneration,
        sandbox_id: ResidentSandboxId,
        sandbox_generation: ResidentSandboxGeneration,
        filesystem_generation: ProjectDiskFilesystemGeneration,
        format_profile_generation: ProjectDiskFilesystemFormatProfileGeneration,
        filesystem_kind: ProjectFilesystemReceiptKind,
    ) -> Result<Self, ProjectFilesystemCorrelationExpectationError> {
        let capture_commit = capture_commit.into();
        let lima_version = lima_version.into();
        validate_commit(&capture_commit)?;
        validate_version(&lima_version)?;
        Ok(Self {
            capture_commit,
            lima_version,
            project,
            disk_id,
            disk_generation,
            disk_revision,
            attachment_generation,
            sandbox_id,
            sandbox_generation,
            filesystem_generation,
            format_profile_generation,
            filesystem_kind,
        })
    }
}

impl fmt::Debug for ProjectFilesystemCorrelationReceiptExpectation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectFilesystemCorrelationReceiptExpectation")
            .field("capture_commit", &self.capture_commit)
            .field("lima_version", &self.lima_version)
            .field("project", &self.project)
            .field("disk_id", &self.disk_id)
            .field("disk_generation", &self.disk_generation)
            .field("disk_revision", &self.disk_revision)
            .field("attachment_generation", &self.attachment_generation)
            .field("sandbox_id", &self.sandbox_id)
            .field("sandbox_generation", &self.sandbox_generation)
            .field("filesystem_generation", &self.filesystem_generation)
            .field("format_profile_generation", &self.format_profile_generation)
            .field("filesystem_kind", &self.filesystem_kind)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExpectedProjectFilesystemCorrelationReceiptReport {
    verdict: ProjectFilesystemCorrelationVerdict,
    capture_context_matches: bool,
    base: ProjectFilesystemCorrelationReceiptReport,
}

impl ExpectedProjectFilesystemCorrelationReceiptReport {
    #[must_use]
    pub const fn verdict(&self) -> ProjectFilesystemCorrelationVerdict {
        self.verdict
    }

    #[must_use]
    pub const fn capture_context_matches(&self) -> bool {
        self.capture_context_matches
    }

    #[must_use]
    pub const fn base(&self) -> &ProjectFilesystemCorrelationReceiptReport {
        &self.base
    }
}

/// Validate one private full-correlation receipt against the exact expected rerun context.
///
/// Any capture commit, Lima version, or durable tuple mismatch is a conclusive `NO`, even when the
/// historical receipt is internally self-consistent. Freshness drift inside the receipt continues
/// to produce `AMBIGUOUS` through the base validator.
///
/// # Errors
///
/// Returns bounded parse/schema/canonical-input errors from the strict base validator or when the
/// top-level capture context cannot be read canonically.
pub fn validate_project_filesystem_correlation_receipt_against(
    bytes: &[u8],
    expected: &ProjectFilesystemCorrelationReceiptExpectation,
) -> Result<ExpectedProjectFilesystemCorrelationReceiptReport, ProjectFilesystemCorrelationExpectationError>
{
    let base = validate_project_filesystem_correlation_receipt_json(bytes)
        .map_err(ProjectFilesystemCorrelationExpectationError::receipt)?;
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| ProjectFilesystemCorrelationExpectationError::context())?;
    let capture_commit = value
        .get("capture_commit")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(ProjectFilesystemCorrelationExpectationError::context)?;
    let lima_version = value
        .get("lima_version")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(ProjectFilesystemCorrelationExpectationError::context)?;

    let current = base.current();
    let capture_context_matches = capture_commit == expected.capture_commit
        && lima_version == expected.lima_version
        && current.project() == &expected.project
        && current.disk_id() == &expected.disk_id
        && current.disk_generation() == expected.disk_generation
        && current.disk_revision() == expected.disk_revision
        && current.attachment_generation() == expected.attachment_generation
        && current.sandbox_id() == &expected.sandbox_id
        && current.sandbox_generation() == expected.sandbox_generation
        && current.filesystem_generation() == expected.filesystem_generation
        && current.format_profile_generation() == expected.format_profile_generation
        && current.filesystem_kind() == expected.filesystem_kind;

    let verdict = if capture_context_matches {
        base.verdict()
    } else {
        ProjectFilesystemCorrelationVerdict::No
    };
    Ok(ExpectedProjectFilesystemCorrelationReceiptReport {
        verdict,
        capture_context_matches,
        base,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectFilesystemCorrelationExpectationErrorKind {
    InvalidExpectation,
    Receipt,
    Context,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProjectFilesystemCorrelationExpectationError {
    kind: ProjectFilesystemCorrelationExpectationErrorKind,
    code: &'static str,
    message: &'static str,
}

impl ProjectFilesystemCorrelationExpectationError {
    #[must_use]
    pub const fn kind(self) -> ProjectFilesystemCorrelationExpectationErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }

    const fn receipt(_error: ProjectFilesystemCorrelationReceiptError) -> Self {
        Self {
            kind: ProjectFilesystemCorrelationExpectationErrorKind::Receipt,
            code: "project_filesystem_correlation_expected_receipt_invalid",
            message: "project filesystem correlation receipt failed strict validation",
        }
    }

    const fn context() -> Self {
        Self {
            kind: ProjectFilesystemCorrelationExpectationErrorKind::Context,
            code: "project_filesystem_correlation_expected_context_invalid",
            message: "project filesystem correlation receipt context is invalid",
        }
    }
}

impl fmt::Debug for ProjectFilesystemCorrelationExpectationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectFilesystemCorrelationExpectationError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for ProjectFilesystemCorrelationExpectationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ProjectFilesystemCorrelationExpectationError {}

fn validate_commit(value: &str) -> Result<(), ProjectFilesystemCorrelationExpectationError> {
    if value.len() != MAX_COMMIT_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid_expectation());
    }
    Ok(())
}

fn validate_version(value: &str) -> Result<(), ProjectFilesystemCorrelationExpectationError> {
    if value.is_empty()
        || value.len() > MAX_VERSION_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+' | b'_'))
    {
        return Err(invalid_expectation());
    }
    Ok(())
}

const fn invalid_expectation() -> ProjectFilesystemCorrelationExpectationError {
    ProjectFilesystemCorrelationExpectationError {
        kind: ProjectFilesystemCorrelationExpectationErrorKind::InvalidExpectation,
        code: "project_filesystem_correlation_expectation_invalid",
        message: "project filesystem correlation expectation is invalid",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ProjectFilesystemCorrelationReceiptExpectation,
        validate_project_filesystem_correlation_receipt_against,
    };
    use crate::project_catalog::ProjectIdentity;
    use crate::project_disk_filesystem::{
        ProjectDiskFilesystemFormatProfileGeneration, ProjectDiskFilesystemGeneration,
    };
    use crate::project_disk_lease::{
        ProjectDiskAttachmentGeneration, ProjectDiskGeneration, ProjectDiskId, ProjectDiskRevision,
        ResidentSandboxGeneration, ResidentSandboxId,
    };
    use crate::project_filesystem_correlation_receipt::{
        PROJECT_FILESYSTEM_CORRELATION_RECEIPT_TYPE, ProjectFilesystemCorrelationVerdict,
        ProjectFilesystemReceiptKind,
    };
    use serde_json::json;

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn tuple() -> serde_json::Value {
        json!({
            "project": "github.com/teamleaderleo/smolrunner",
            "disk_id": "disk-a",
            "disk_generation": 3,
            "disk_revision": 7,
            "attachment_generation": 2,
            "sandbox_id": "sandbox-a",
            "sandbox_generation": 11,
            "filesystem_generation": 5,
            "format_profile_generation": 2,
            "filesystem_kind": "ext4"
        })
    }

    fn receipt() -> Vec<u8> {
        serde_json::to_vec(&json!({
            "schema_version": 1,
            "receipt_type": PROJECT_FILESYSTEM_CORRELATION_RECEIPT_TYPE,
            "capture_commit": "0123456789abcdef0123456789abcdef01234567",
            "lima_version": "2.2.0",
            "before": tuple(),
            "after": tuple(),
            "p3_physical_identity_digest": digest('a'),
            "p3_backing_identity_digest": digest('b'),
            "p2_physical_identity_digest": digest('a'),
            "p2_backing_identity_digest": digest('b'),
            "descriptor_rebind_passed": true,
            "resident_host_identity_bound": true,
            "lima_attachment_agrees": true,
            "host_guest_block_bridge_bound": true,
            "filesystem": {
                "filesystem_uuid": "01234567-89ab-cdef-0123-456789abcdef",
                "feature_policy_digest": digest('c'),
                "logical_bytes": 1073741824_u64
            },
            "guest_transaction": {
                "authority": tuple(),
                "exact_invocation_target_bound": true,
                "limactl_generation": 4,
                "limactl_digest": digest('d'),
                "guest_binary_generation": 5,
                "guest_binary_digest": digest('e'),
                "protocol_generation": 1,
                "request_digest": digest('f'),
                "result_digest": digest('1')
            },
            "guest_filesystem": {
                "filesystem_kind": "ext4",
                "filesystem_uuid": "01234567-89ab-cdef-0123-456789abcdef",
                "feature_policy_digest": digest('c'),
                "logical_bytes": 1073741824_u64,
                "stat_device": 2049_u64,
                "mountinfo_major": 8,
                "mountinfo_minor": 1,
                "mount_root_identity_bound": true,
                "read_write": true
            },
            "guest_block_device": { "rdev": 2049_u64, "inode": 77_u64, "whole_device": true },
            "role_filesystem_device": 2049_u64
        }))
        .unwrap()
    }

    fn expectation() -> ProjectFilesystemCorrelationReceiptExpectation {
        ProjectFilesystemCorrelationReceiptExpectation::new(
            "0123456789abcdef0123456789abcdef01234567",
            "2.2.0",
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            ProjectDiskId::parse("disk-a").unwrap(),
            ProjectDiskGeneration::new(3).unwrap(),
            ProjectDiskRevision::new(7).unwrap(),
            ProjectDiskAttachmentGeneration::new(2).unwrap(),
            ResidentSandboxId::parse("sandbox-a").unwrap(),
            ResidentSandboxGeneration::new(11).unwrap(),
            ProjectDiskFilesystemGeneration::new(5).unwrap(),
            ProjectDiskFilesystemFormatProfileGeneration::new(2).unwrap(),
            ProjectFilesystemReceiptKind::Ext4,
        )
        .unwrap()
    }

    #[test]
    fn exact_current_context_preserves_yes() {
        let report = validate_project_filesystem_correlation_receipt_against(
            &receipt(),
            &expectation(),
        )
        .unwrap();
        assert!(report.capture_context_matches());
        assert_eq!(report.verdict(), ProjectFilesystemCorrelationVerdict::Yes);
    }

    #[test]
    fn old_capture_commit_is_no_even_when_receipt_is_internally_exact() {
        let expected = ProjectFilesystemCorrelationReceiptExpectation::new(
            "1123456789abcdef0123456789abcdef01234567",
            "2.2.0",
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            ProjectDiskId::parse("disk-a").unwrap(),
            ProjectDiskGeneration::new(3).unwrap(),
            ProjectDiskRevision::new(7).unwrap(),
            ProjectDiskAttachmentGeneration::new(2).unwrap(),
            ResidentSandboxId::parse("sandbox-a").unwrap(),
            ResidentSandboxGeneration::new(11).unwrap(),
            ProjectDiskFilesystemGeneration::new(5).unwrap(),
            ProjectDiskFilesystemFormatProfileGeneration::new(2).unwrap(),
            ProjectFilesystemReceiptKind::Ext4,
        )
        .unwrap();
        let report =
            validate_project_filesystem_correlation_receipt_against(&receipt(), &expected).unwrap();
        assert!(!report.capture_context_matches());
        assert_eq!(report.verdict(), ProjectFilesystemCorrelationVerdict::No);
    }

    #[test]
    fn stale_durable_tuple_is_no_even_when_old_receipt_is_self_consistent() {
        let expected = ProjectFilesystemCorrelationReceiptExpectation::new(
            "0123456789abcdef0123456789abcdef01234567",
            "2.2.0",
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            ProjectDiskId::parse("disk-a").unwrap(),
            ProjectDiskGeneration::new(3).unwrap(),
            ProjectDiskRevision::new(8).unwrap(),
            ProjectDiskAttachmentGeneration::new(3).unwrap(),
            ResidentSandboxId::parse("sandbox-a").unwrap(),
            ResidentSandboxGeneration::new(11).unwrap(),
            ProjectDiskFilesystemGeneration::new(5).unwrap(),
            ProjectDiskFilesystemFormatProfileGeneration::new(2).unwrap(),
            ProjectFilesystemReceiptKind::Ext4,
        )
        .unwrap();
        let report =
            validate_project_filesystem_correlation_receipt_against(&receipt(), &expected).unwrap();
        assert_eq!(report.verdict(), ProjectFilesystemCorrelationVerdict::No);
    }
}
