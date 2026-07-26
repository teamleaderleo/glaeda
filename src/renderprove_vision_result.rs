use std::fmt;

use serde::Serialize;

use crate::artifact::RepositoryRef;
use crate::execution_admission::ExecutionAdmissionIdentity;
use crate::renderprove_vision_profile::{
    RenderproveVisionPacketProfile, RenderproveVisionPreviewEvidence, RenderproveVisionToolIdentity,
};
use crate::verification_profile::{
    RepositoryCommandIdentity, TestedSourceIdentity, VerificationProfileId,
};

pub const RENDERPROVE_VISION_RESULT_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderproveVisionFailureCode {
    InputRejected,
    ProcessFailed,
    TimedOut,
    OutputLimitExceeded,
    MalformedPreview,
    IdentityDrift,
    CleanupFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RenderproveVisionProcessOutcome {
    Succeeded,
    Failed { code: RenderproveVisionFailureCode },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderproveVisionCleanupOutcome {
    Completed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RenderproveVisionDisposition {
    Succeeded,
    Failed { code: RenderproveVisionFailureCode },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderproveVisionEvidenceDisclosure {
    Included,
    Omitted,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RenderproveVisionEvidenceCoverage {
    pub public_preview: RenderproveVisionEvidenceDisclosure,
    pub screenshot_pixels: RenderproveVisionEvidenceDisclosure,
    pub brief_contents: RenderproveVisionEvidenceDisclosure,
    pub receipt_contents: RenderproveVisionEvidenceDisclosure,
    pub process_output: RenderproveVisionEvidenceDisclosure,
    pub environment: RenderproveVisionEvidenceDisclosure,
    pub credentials: RenderproveVisionEvidenceDisclosure,
}

impl RenderproveVisionEvidenceCoverage {
    const fn for_preview(preview_present: bool) -> Self {
        Self {
            public_preview: if preview_present {
                RenderproveVisionEvidenceDisclosure::Included
            } else {
                RenderproveVisionEvidenceDisclosure::Unavailable
            },
            screenshot_pixels: RenderproveVisionEvidenceDisclosure::Omitted,
            brief_contents: RenderproveVisionEvidenceDisclosure::Omitted,
            receipt_contents: RenderproveVisionEvidenceDisclosure::Omitted,
            process_output: RenderproveVisionEvidenceDisclosure::Omitted,
            environment: RenderproveVisionEvidenceDisclosure::Omitted,
            credentials: RenderproveVisionEvidenceDisclosure::Omitted,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderproveVisionTerminalEvidenceDefinition {
    pub admission: ExecutionAdmissionIdentity,
    pub tested_source: TestedSourceIdentity,
    pub project_command: RepositoryCommandIdentity,
    pub tool: RenderproveVisionToolIdentity,
    pub process: RenderproveVisionProcessOutcome,
    pub cleanup: RenderproveVisionCleanupOutcome,
    pub preview: Option<RenderproveVisionPreviewEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderproveVisionTerminalEvidence {
    admission: ExecutionAdmissionIdentity,
    tested_source: TestedSourceIdentity,
    project_command: RepositoryCommandIdentity,
    tool: RenderproveVisionToolIdentity,
    process: RenderproveVisionProcessOutcome,
    cleanup: RenderproveVisionCleanupOutcome,
    preview: Option<RenderproveVisionPreviewEvidence>,
}

impl RenderproveVisionTerminalEvidence {
    /// Retain one typed terminal observation without raw process output or private input paths.
    ///
    /// # Errors
    ///
    /// Returns an error when a failed process claims a valid public preview or when a process failure
    /// uses the cleanup-only failure code.
    pub fn new(
        definition: RenderproveVisionTerminalEvidenceDefinition,
    ) -> Result<Self, RenderproveVisionResultError> {
        if matches!(
            definition.process,
            RenderproveVisionProcessOutcome::Failed { .. }
        ) && definition.preview.is_some()
        {
            return Err(RenderproveVisionResultError::new(
                "evidence.preview",
                "preview_after_process_failure",
                "a failed packet process must not claim a validated public preview",
            ));
        }
        if matches!(
            definition.process,
            RenderproveVisionProcessOutcome::Failed {
                code: RenderproveVisionFailureCode::CleanupFailed
            }
        ) {
            return Err(RenderproveVisionResultError::new(
                "evidence.process.code",
                "invalid_process_failure_code",
                "cleanup failure belongs to the cleanup outcome",
            ));
        }
        Ok(Self {
            admission: definition.admission,
            tested_source: definition.tested_source,
            project_command: definition.project_command,
            tool: definition.tool,
            process: definition.process,
            cleanup: definition.cleanup,
            preview: definition.preview,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderproveVisionResult {
    schema_version: u8,
    profile_id: VerificationProfileId,
    admission: ExecutionAdmissionIdentity,
    project_repository: RepositoryRef,
    tested_source: TestedSourceIdentity,
    project_command: RepositoryCommandIdentity,
    tool: RenderproveVisionToolIdentity,
    process: RenderproveVisionProcessOutcome,
    cleanup: RenderproveVisionCleanupOutcome,
    disposition: RenderproveVisionDisposition,
    preview: Option<RenderproveVisionPreviewEvidence>,
    coverage: RenderproveVisionEvidenceCoverage,
}

impl RenderproveVisionResult {
    #[must_use]
    pub const fn disposition(&self) -> RenderproveVisionDisposition {
        self.disposition
    }

    #[must_use]
    pub const fn preview(&self) -> Option<&RenderproveVisionPreviewEvidence> {
        self.preview.as_ref()
    }

    #[must_use]
    pub const fn coverage(&self) -> RenderproveVisionEvidenceCoverage {
        self.coverage
    }
}

/// Bind terminal packet evidence to the exact admitted profile and derive one bounded public result.
///
/// # Errors
///
/// Returns an error for profile/admission/source/command/tool drift, an impossible success without a
/// validated preview, or contradictory process and cleanup outcomes.
pub fn finalize_renderprove_vision_result(
    profile: &RenderproveVisionPacketProfile,
    evidence: RenderproveVisionTerminalEvidence,
) -> Result<RenderproveVisionResult, RenderproveVisionResultError> {
    if evidence.admission.verification_profile_id != *profile.profile_id() {
        return Err(RenderproveVisionResultError::new(
            "result.admission.verification_profile_id",
            "admission_profile_mismatch",
            "must match the exact Renderprove vision packet profile",
        ));
    }
    if evidence.tested_source != *profile.tested_source()
        || evidence.project_command != *profile.project_command()
        || evidence.tool != *profile.tool()
    {
        return Err(RenderproveVisionResultError::new(
            "result.identity",
            "terminal_identity_drift",
            "must preserve the exact tested source, project command, and Renderprove tool identities",
        ));
    }

    let disposition = match (
        evidence.process,
        evidence.cleanup,
        evidence.preview.as_ref(),
    ) {
        (
            RenderproveVisionProcessOutcome::Succeeded,
            RenderproveVisionCleanupOutcome::Completed,
            Some(_),
        ) => RenderproveVisionDisposition::Succeeded,
        (
            RenderproveVisionProcessOutcome::Succeeded,
            RenderproveVisionCleanupOutcome::Failed,
            _,
        ) => RenderproveVisionDisposition::Failed {
            code: RenderproveVisionFailureCode::CleanupFailed,
        },
        (RenderproveVisionProcessOutcome::Succeeded, _, None) => {
            RenderproveVisionDisposition::Failed {
                code: RenderproveVisionFailureCode::MalformedPreview,
            }
        }
        (RenderproveVisionProcessOutcome::Failed { code }, _, None) => {
            RenderproveVisionDisposition::Failed { code }
        }
        (RenderproveVisionProcessOutcome::Failed { .. }, _, Some(_)) => {
            return Err(RenderproveVisionResultError::new(
                "result.preview",
                "preview_after_process_failure",
                "a failed packet process must not claim a validated public preview",
            ));
        }
    };
    let coverage = RenderproveVisionEvidenceCoverage::for_preview(evidence.preview.is_some());

    Ok(RenderproveVisionResult {
        schema_version: RENDERPROVE_VISION_RESULT_SCHEMA_VERSION,
        profile_id: profile.profile_id().clone(),
        admission: evidence.admission,
        project_repository: profile.project_repository().clone(),
        tested_source: evidence.tested_source,
        project_command: evidence.project_command,
        tool: evidence.tool,
        process: evidence.process,
        cleanup: evidence.cleanup,
        disposition,
        preview: evidence.preview,
        coverage,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderproveVisionResultError {
    pub field: String,
    pub code: String,
    pub problem: String,
}

impl RenderproveVisionResultError {
    fn new(field: impl Into<String>, code: impl Into<String>, problem: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            code: code.into(),
            problem: problem.into(),
        }
    }
}

impl fmt::Display for RenderproveVisionResultError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}: {}", self.field, self.code, self.problem)
    }
}

impl std::error::Error for RenderproveVisionResultError {}
