use std::fmt;

use serde::Serialize;

use crate::artifact::{ArtifactIdentity, Sha256Digest};
use crate::renderprove_execution::RenderproveExecutionReceipt;
use crate::renderprove_verification::{
    RenderproveCleanupOutcome, RenderproveEvidenceArtifact, RenderproveEvidenceKind,
    RenderproveProcessOutcome, RenderproveReceiptOutcome, RenderproveSourceIdentity,
    RenderproveVerificationDisposition, RenderproveVerificationError,
    RenderproveVerificationFailure, RenderproveVerificationRequest, RenderproveWorkerImageIdentity,
    finalize_renderprove_verification,
};

pub const RENDERPROVE_ARTIFACT_RECEIPT_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderproveSanitizedReceiptVerdict {
    Passing,
    Failing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderproveSanitizedReceiptIdentity {
    schema_version: u8,
    source: RenderproveSourceIdentity,
    project_image: ArtifactIdentity,
    worker_image: RenderproveWorkerImageIdentity,
    manifest_digest: Sha256Digest,
    digest: Sha256Digest,
    verdict: RenderproveSanitizedReceiptVerdict,
}

impl RenderproveSanitizedReceiptIdentity {
    #[must_use]
    pub const fn new(
        source: RenderproveSourceIdentity,
        project_image: ArtifactIdentity,
        worker_image: RenderproveWorkerImageIdentity,
        manifest_digest: Sha256Digest,
        digest: Sha256Digest,
        verdict: RenderproveSanitizedReceiptVerdict,
    ) -> Self {
        Self {
            schema_version: RENDERPROVE_ARTIFACT_RECEIPT_SCHEMA_VERSION,
            source,
            project_image,
            worker_image,
            manifest_digest,
            digest,
            verdict,
        }
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn source(&self) -> &RenderproveSourceIdentity {
        &self.source
    }

    #[must_use]
    pub const fn project_image(&self) -> &ArtifactIdentity {
        &self.project_image
    }

    #[must_use]
    pub const fn worker_image(&self) -> &RenderproveWorkerImageIdentity {
        &self.worker_image
    }

    #[must_use]
    pub const fn manifest_digest(&self) -> &Sha256Digest {
        &self.manifest_digest
    }

    #[must_use]
    pub const fn digest(&self) -> &Sha256Digest {
        &self.digest
    }

    #[must_use]
    pub const fn verdict(&self) -> RenderproveSanitizedReceiptVerdict {
        self.verdict
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RenderproveSanitizedReceiptAssessment {
    Present {
        receipt: Box<RenderproveSanitizedReceiptIdentity>,
    },
    Missing,
    Invalid,
}

impl RenderproveSanitizedReceiptAssessment {
    #[must_use]
    pub fn present(receipt: RenderproveSanitizedReceiptIdentity) -> Self {
        Self::Present {
            receipt: Box::new(receipt),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct RenderproveArtifactReceipt {
    schema_version: u8,
    command_id: String,
    source: RenderproveSourceIdentity,
    project_image: ArtifactIdentity,
    worker_image: RenderproveWorkerImageIdentity,
    manifest_digest: Sha256Digest,
    process: RenderproveProcessOutcome,
    cleanup: RenderproveCleanupOutcome,
    sanitized_receipt: RenderproveSanitizedReceiptAssessment,
    disposition: RenderproveVerificationDisposition,
    failures: Vec<RenderproveVerificationFailure>,
    public_artifacts: Vec<RenderproveEvidenceArtifact>,
    #[serde(skip)]
    execution: RenderproveExecutionReceipt,
    #[serde(skip)]
    private_artifacts: Vec<RenderproveEvidenceArtifact>,
}

impl RenderproveArtifactReceipt {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub fn command_id(&self) -> &str {
        &self.command_id
    }

    #[must_use]
    pub const fn source(&self) -> &RenderproveSourceIdentity {
        &self.source
    }

    #[must_use]
    pub const fn project_image(&self) -> &ArtifactIdentity {
        &self.project_image
    }

    #[must_use]
    pub const fn worker_image(&self) -> &RenderproveWorkerImageIdentity {
        &self.worker_image
    }

    #[must_use]
    pub const fn manifest_digest(&self) -> &Sha256Digest {
        &self.manifest_digest
    }

    #[must_use]
    pub const fn process(&self) -> &RenderproveProcessOutcome {
        &self.process
    }

    #[must_use]
    pub const fn cleanup(&self) -> &RenderproveCleanupOutcome {
        &self.cleanup
    }

    #[must_use]
    pub const fn sanitized_receipt(&self) -> &RenderproveSanitizedReceiptAssessment {
        &self.sanitized_receipt
    }

    #[must_use]
    pub const fn disposition(&self) -> RenderproveVerificationDisposition {
        self.disposition
    }

    #[must_use]
    pub fn failures(&self) -> &[RenderproveVerificationFailure] {
        &self.failures
    }

    #[must_use]
    pub fn public_artifacts(&self) -> &[RenderproveEvidenceArtifact] {
        &self.public_artifacts
    }

    #[must_use]
    pub const fn execution(&self) -> &RenderproveExecutionReceipt {
        &self.execution
    }

    #[must_use]
    pub fn private_artifacts(&self) -> &[RenderproveEvidenceArtifact] {
        &self.private_artifacts
    }
}

impl fmt::Debug for RenderproveArtifactReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderproveArtifactReceipt")
            .field("schema_version", &self.schema_version)
            .field("command_id", &self.command_id)
            .field("source", &self.source)
            .field("project_image", &self.project_image)
            .field("worker_image", &self.worker_image)
            .field("manifest_digest", &self.manifest_digest)
            .field("process", &self.process)
            .field("cleanup", &self.cleanup)
            .field("sanitized_receipt", &self.sanitized_receipt)
            .field("disposition", &self.disposition)
            .field("failures", &self.failures)
            .field("public_artifacts", &self.public_artifacts)
            .field("execution", &"<retained exact private execution receipt>")
            .field(
                "private_artifacts",
                &format_args!(
                    "<{} retained private artifact identities>",
                    self.private_artifacts.len()
                ),
            )
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderproveArtifactBindingError {
    pub field: String,
    pub problem: String,
}

impl RenderproveArtifactBindingError {
    fn new(field: impl Into<String>, problem: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            problem: problem.into(),
        }
    }
}

impl fmt::Display for RenderproveArtifactBindingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", self.field, self.problem)
    }
}

impl std::error::Error for RenderproveArtifactBindingError {}

impl From<RenderproveVerificationError> for RenderproveArtifactBindingError {
    fn from(error: RenderproveVerificationError) -> Self {
        Self {
            field: error.field,
            problem: error.problem,
        }
    }
}

/// Bind final artifact identities to one exact typed Renderprove execution receipt.
///
/// The execution receipt supplies the authoritative reviewed request and process outcome. A present
/// sanitized receipt must repeat the exact source, project image, worker image, and manifest
/// identities from that request, and exactly one sanitized-receipt artifact must carry its digest.
/// Missing or invalid receipt assessments cannot include a sanitized-receipt artifact. Cleanup and
/// process outcomes are required before this function can construct a final receipt.
///
/// This is a pure contract. It performs no filesystem access, hashing, artifact export, cleanup,
/// subprocess execution, browser control, networking, deployment, or publication.
///
/// # Errors
///
/// Returns an error for request-identity drift, contradictory receipt/artifact evidence, duplicate
/// artifact identities, impossible process evidence, or another invalid verification contract.
pub fn bind_renderprove_artifacts(
    execution: RenderproveExecutionReceipt,
    cleanup: RenderproveCleanupOutcome,
    sanitized_receipt: RenderproveSanitizedReceiptAssessment,
    artifacts: Vec<RenderproveEvidenceArtifact>,
) -> Result<RenderproveArtifactReceipt, RenderproveArtifactBindingError> {
    let request = execution.command().request().clone();
    let receipt_outcome = validate_sanitized_receipt(&request, &sanitized_receipt, &artifacts)?;
    let verification = finalize_renderprove_verification(
        request.clone(),
        execution.process().clone(),
        cleanup.clone(),
        receipt_outcome,
        artifacts,
    )?;

    Ok(RenderproveArtifactReceipt {
        schema_version: RENDERPROVE_ARTIFACT_RECEIPT_SCHEMA_VERSION,
        command_id: execution.command_id().to_owned(),
        source: request.source().clone(),
        project_image: request.project_image().clone(),
        worker_image: request.worker_image().clone(),
        manifest_digest: request.manifest_digest().clone(),
        process: verification.process().clone(),
        cleanup: verification.cleanup().clone(),
        sanitized_receipt,
        disposition: verification.disposition(),
        failures: verification.failures().to_vec(),
        public_artifacts: verification.public_artifacts().to_vec(),
        private_artifacts: verification.private_artifacts().to_vec(),
        execution,
    })
}

fn validate_sanitized_receipt(
    request: &RenderproveVerificationRequest,
    assessment: &RenderproveSanitizedReceiptAssessment,
    artifacts: &[RenderproveEvidenceArtifact],
) -> Result<RenderproveReceiptOutcome, RenderproveArtifactBindingError> {
    let receipt_artifacts = artifacts
        .iter()
        .filter(|artifact| artifact.kind() == RenderproveEvidenceKind::SanitizedReceipt)
        .collect::<Vec<_>>();

    match assessment {
        RenderproveSanitizedReceiptAssessment::Present { receipt } => {
            validate_receipt_request_identity(request, receipt)?;
            if receipt_artifacts.len() != 1 {
                return Err(RenderproveArtifactBindingError::new(
                    "artifacts.sanitized_receipt",
                    "must contain exactly one sanitized receipt artifact for a present receipt",
                ));
            }
            if receipt_artifacts[0].digest() != receipt.digest() {
                return Err(RenderproveArtifactBindingError::new(
                    "artifacts.sanitized_receipt.digest",
                    "must match the typed sanitized receipt digest",
                ));
            }
            Ok(match receipt.verdict() {
                RenderproveSanitizedReceiptVerdict::Passing => RenderproveReceiptOutcome::Passing {
                    digest: receipt.digest().clone(),
                },
                RenderproveSanitizedReceiptVerdict::Failing => RenderproveReceiptOutcome::Failing {
                    digest: receipt.digest().clone(),
                },
            })
        }
        RenderproveSanitizedReceiptAssessment::Missing => {
            reject_unassessed_receipt_artifacts(&receipt_artifacts)?;
            Ok(RenderproveReceiptOutcome::Missing)
        }
        RenderproveSanitizedReceiptAssessment::Invalid => {
            reject_unassessed_receipt_artifacts(&receipt_artifacts)?;
            Ok(RenderproveReceiptOutcome::Invalid)
        }
    }
}

fn reject_unassessed_receipt_artifacts(
    receipt_artifacts: &[&RenderproveEvidenceArtifact],
) -> Result<(), RenderproveArtifactBindingError> {
    if receipt_artifacts.is_empty() {
        Ok(())
    } else {
        Err(RenderproveArtifactBindingError::new(
            "artifacts.sanitized_receipt",
            "must be absent when the sanitized receipt is missing or invalid",
        ))
    }
}

fn validate_receipt_request_identity(
    request: &RenderproveVerificationRequest,
    receipt: &RenderproveSanitizedReceiptIdentity,
) -> Result<(), RenderproveArtifactBindingError> {
    if receipt.schema_version() != RENDERPROVE_ARTIFACT_RECEIPT_SCHEMA_VERSION {
        return Err(RenderproveArtifactBindingError::new(
            "sanitized_receipt.schema_version",
            "is unsupported",
        ));
    }
    if receipt.source() != request.source() {
        return Err(RenderproveArtifactBindingError::new(
            "sanitized_receipt.source",
            "does not match the exact reviewed source identity",
        ));
    }
    if receipt.project_image() != request.project_image() {
        return Err(RenderproveArtifactBindingError::new(
            "sanitized_receipt.project_image",
            "does not match the exact reviewed project image identity",
        ));
    }
    if receipt.worker_image() != request.worker_image() {
        return Err(RenderproveArtifactBindingError::new(
            "sanitized_receipt.worker_image",
            "does not match the exact reviewed worker image identity",
        ));
    }
    if receipt.manifest_digest() != request.manifest_digest() {
        return Err(RenderproveArtifactBindingError::new(
            "sanitized_receipt.manifest_digest",
            "does not match the exact reviewed manifest digest",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::artifact::{ArtifactIdentity, ArtifactKind, CommitId, RepositoryRef, Sha256Digest};
    use crate::lane_command::{LinuxAccountName, RunnerUserContext};
    use crate::process::ExecutionRecord;
    use crate::renderprove_execution::{
        RenderproveExecutionContext, RenderproveExecutionObservation, bind_renderprove_execution,
        plan_renderprove_command,
    };
    use crate::renderprove_verification::{
        RenderproveCleanupFailure, RenderproveCleanupOutcome, RenderproveEvidenceArtifact,
        RenderproveEvidenceKind, RenderproveEvidencePolicy, RenderproveReviewNetworkPolicy,
        RenderproveSourceIdentity, RenderproveVerificationDisposition,
        RenderproveVerificationFailure, RenderproveVerificationRequest,
        RenderproveWorkerImageIdentity,
    };

    use super::{
        RenderproveSanitizedReceiptAssessment, RenderproveSanitizedReceiptIdentity,
        RenderproveSanitizedReceiptVerdict, bind_renderprove_artifacts,
    };

    fn digest(character: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", character.to_string().repeat(64)))
            .expect("digest")
    }

    fn request() -> RenderproveVerificationRequest {
        let repository = RepositoryRef::parse("example/project").expect("repository");
        let commit = CommitId::parse(&"1a".repeat(20)).expect("commit");
        RenderproveVerificationRequest::new(
            RenderproveSourceIdentity::new(repository.clone(), commit.clone()),
            ArtifactIdentity::new(repository, commit, ArtifactKind::OciImage, digest('a')),
            RenderproveWorkerImageIdentity::new("registry.example/worker@reviewed", digest('b'))
                .expect("worker"),
            digest('c'),
            RenderproveEvidencePolicy::new(".smolrunner/renderprove", 4_096).expect("evidence"),
            RenderproveReviewNetworkPolicy::LoopbackOnly,
        )
        .expect("request")
    }

    fn execution() -> crate::renderprove_execution::RenderproveExecutionReceipt {
        let request = request();
        let runner = RunnerUserContext::new(
            LinuxAccountName::parse("project-runner").expect("runner"),
            1001,
            1001,
            "/var/lib/project-runner",
        )
        .expect("runner context");
        let context = RenderproveExecutionContext::new(
            "/srv/smolrunner/workspaces/job-1",
            "/opt/renderprove",
            runner,
        )
        .expect("context");
        let command = plan_renderprove_command(request, &context).expect("command");
        let spec = command.spec().clone();
        let record = ExecutionRecord {
            argv: spec.displayed_argv(),
            environment_keys: spec.environment.keys().cloned().collect(),
            status: Some(0),
            success: true,
            stdout: "private stdout".to_owned(),
            stderr: "private stderr".to_owned(),
        };
        let observation = RenderproveExecutionObservation::new(
            record,
            command.working_directory().to_path_buf(),
            spec,
        )
        .expect("observation");
        bind_renderprove_execution(command, observation, None).expect("execution receipt")
    }

    fn receipt_identity(
        execution: &crate::renderprove_execution::RenderproveExecutionReceipt,
        digest: Sha256Digest,
        verdict: RenderproveSanitizedReceiptVerdict,
    ) -> RenderproveSanitizedReceiptIdentity {
        let request = execution.command().request();
        RenderproveSanitizedReceiptIdentity::new(
            request.source().clone(),
            request.project_image().clone(),
            request.worker_image().clone(),
            request.manifest_digest().clone(),
            digest,
            verdict,
        )
    }

    #[test]
    fn passing_receipt_binds_exact_execution_and_separates_private_artifacts() {
        let execution = execution();
        let receipt_digest = digest('d');
        let assessment = RenderproveSanitizedReceiptAssessment::present(receipt_identity(
            &execution,
            receipt_digest.clone(),
            RenderproveSanitizedReceiptVerdict::Passing,
        ));
        let artifacts = vec![
            RenderproveEvidenceArtifact::new(
                RenderproveEvidenceKind::SanitizedReceipt,
                receipt_digest.clone(),
                512,
            )
            .expect("receipt artifact"),
            RenderproveEvidenceArtifact::new(
                RenderproveEvidenceKind::ApprovedScreenshot,
                digest('e'),
                1_024,
            )
            .expect("screenshot"),
            RenderproveEvidenceArtifact::new(
                RenderproveEvidenceKind::PrivateWorkerIdentity,
                digest('f'),
                256,
            )
            .expect("worker identity"),
        ];

        let bound = bind_renderprove_artifacts(
            execution,
            RenderproveCleanupOutcome::Complete,
            assessment,
            artifacts,
        )
        .expect("bound receipt");

        assert_eq!(
            bound.disposition(),
            RenderproveVerificationDisposition::Passed
        );
        assert!(bound.failures().is_empty());
        assert_eq!(bound.public_artifacts().len(), 2);
        assert_eq!(bound.private_artifacts().len(), 1);
        assert_eq!(bound.execution().process(), bound.process());
        assert_eq!(
            bound.source(),
            bound.execution().command().request().source()
        );

        let json = serde_json::to_string(&bound).expect("serialize");
        assert!(json.contains(receipt_digest.as_str()));
        assert!(!json.contains("private stdout"));
        assert!(!json.contains("private stderr"));
        assert!(!json.contains(digest('f').as_str()));
        let debug = format!("{bound:?}");
        assert!(!debug.contains("private stdout"));
        assert!(!debug.contains("private stderr"));
        assert!(!debug.contains(digest('f').as_str()));
    }

    #[test]
    fn receipt_source_drift_fails_before_finalization() {
        let execution = execution();
        let request = execution.command().request();
        let mut receipt = receipt_identity(
            &execution,
            digest('d'),
            RenderproveSanitizedReceiptVerdict::Passing,
        );
        receipt.source = RenderproveSourceIdentity::new(
            RepositoryRef::parse("other/project").expect("repository"),
            request.source().commit.clone(),
        );
        let assessment = RenderproveSanitizedReceiptAssessment::present(receipt);
        let artifacts = vec![
            RenderproveEvidenceArtifact::new(
                RenderproveEvidenceKind::SanitizedReceipt,
                digest('d'),
                512,
            )
            .expect("artifact"),
        ];

        let error = bind_renderprove_artifacts(
            execution,
            RenderproveCleanupOutcome::Complete,
            assessment,
            artifacts,
        )
        .expect_err("source drift");
        assert_eq!(error.field, "sanitized_receipt.source");
    }

    #[test]
    fn receipt_artifact_digest_must_match_typed_receipt() {
        let execution = execution();
        let assessment = RenderproveSanitizedReceiptAssessment::present(receipt_identity(
            &execution,
            digest('d'),
            RenderproveSanitizedReceiptVerdict::Passing,
        ));
        let artifacts = vec![
            RenderproveEvidenceArtifact::new(
                RenderproveEvidenceKind::SanitizedReceipt,
                digest('e'),
                512,
            )
            .expect("artifact"),
        ];

        let error = bind_renderprove_artifacts(
            execution,
            RenderproveCleanupOutcome::Complete,
            assessment,
            artifacts,
        )
        .expect_err("digest drift");
        assert_eq!(error.field, "artifacts.sanitized_receipt.digest");
    }

    #[test]
    fn missing_or_invalid_receipt_cannot_export_a_sanitized_receipt_artifact() {
        for assessment in [
            RenderproveSanitizedReceiptAssessment::Missing,
            RenderproveSanitizedReceiptAssessment::Invalid,
        ] {
            let execution = execution();
            let artifacts = vec![
                RenderproveEvidenceArtifact::new(
                    RenderproveEvidenceKind::SanitizedReceipt,
                    digest('d'),
                    512,
                )
                .expect("artifact"),
            ];
            let error = bind_renderprove_artifacts(
                execution,
                RenderproveCleanupOutcome::Complete,
                assessment,
                artifacts,
            )
            .expect_err("contradictory receipt artifact");
            assert_eq!(error.field, "artifacts.sanitized_receipt");
        }
    }

    #[test]
    fn successful_process_without_passing_receipt_cannot_pass() {
        let execution = execution();
        let bound = bind_renderprove_artifacts(
            execution,
            RenderproveCleanupOutcome::Complete,
            RenderproveSanitizedReceiptAssessment::Missing,
            Vec::new(),
        )
        .expect("failed verification receipt");

        assert_eq!(
            bound.disposition(),
            RenderproveVerificationDisposition::Failed
        );
        assert!(
            bound
                .failures()
                .contains(&RenderproveVerificationFailure::ReceiptMissing)
        );
    }

    #[test]
    fn cleanup_failure_is_preserved_after_a_passing_process_and_receipt() {
        let execution = execution();
        let receipt_digest = digest('d');
        let assessment = RenderproveSanitizedReceiptAssessment::present(receipt_identity(
            &execution,
            receipt_digest.clone(),
            RenderproveSanitizedReceiptVerdict::Passing,
        ));
        let artifacts = vec![
            RenderproveEvidenceArtifact::new(
                RenderproveEvidenceKind::SanitizedReceipt,
                receipt_digest,
                512,
            )
            .expect("artifact"),
        ];
        let bound = bind_renderprove_artifacts(
            execution,
            RenderproveCleanupOutcome::Failed {
                reason: RenderproveCleanupFailure::Workspace,
            },
            assessment,
            artifacts,
        )
        .expect("cleanup-failed receipt");

        assert_eq!(
            bound.disposition(),
            RenderproveVerificationDisposition::CleanupFailed
        );
        assert!(
            bound
                .failures()
                .contains(&RenderproveVerificationFailure::CleanupFailed)
        );
    }

    #[test]
    fn failing_sanitized_receipt_remains_distinct_from_process_success() {
        let execution = execution();
        let receipt_digest = digest('d');
        let assessment = RenderproveSanitizedReceiptAssessment::present(receipt_identity(
            &execution,
            receipt_digest.clone(),
            RenderproveSanitizedReceiptVerdict::Failing,
        ));
        let artifacts = vec![
            RenderproveEvidenceArtifact::new(
                RenderproveEvidenceKind::SanitizedReceipt,
                receipt_digest,
                512,
            )
            .expect("artifact"),
        ];
        let bound = bind_renderprove_artifacts(
            execution,
            RenderproveCleanupOutcome::Complete,
            assessment,
            artifacts,
        )
        .expect("failing receipt");

        assert_eq!(
            bound.disposition(),
            RenderproveVerificationDisposition::Failed
        );
        assert!(
            bound
                .failures()
                .contains(&RenderproveVerificationFailure::ReceiptFailed)
        );
    }
}
