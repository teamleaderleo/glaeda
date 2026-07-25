use std::collections::BTreeSet;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::artifact::{ArtifactIdentity, ArtifactKind, CommitId, RepositoryRef, Sha256Digest};

pub const RENDERPROVE_VERIFICATION_SCHEMA_VERSION: u8 = 1;
pub const MAX_EVIDENCE_BUDGET_BYTES: u64 = 1_073_741_824;
pub const MAX_EVIDENCE_ARTIFACTS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderproveSourceIdentity {
    pub repository: RepositoryRef,
    pub commit: CommitId,
}

impl RenderproveSourceIdentity {
    #[must_use]
    pub const fn new(repository: RepositoryRef, commit: CommitId) -> Self {
        Self { repository, commit }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderproveWorkerImageIdentity {
    reference: String,
    digest: Sha256Digest,
}

impl RenderproveWorkerImageIdentity {
    /// Build one immutable worker-image identity.
    ///
    /// # Errors
    ///
    /// Returns an error when the human-readable image reference is empty, too long, or contains
    /// whitespace or control characters. The digest is canonical because it uses
    /// [`Sha256Digest`].
    pub fn new(
        reference: impl Into<String>,
        digest: Sha256Digest,
    ) -> Result<Self, RenderproveVerificationError> {
        let reference = reference.into();
        if reference.is_empty()
            || reference.len() > 512
            || reference
                .chars()
                .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(RenderproveVerificationError::new(
                "worker_image.reference",
                "must be a non-empty image reference of at most 512 characters without whitespace or control characters",
            ));
        }
        Ok(Self { reference, digest })
    }

    #[must_use]
    pub fn reference(&self) -> &str {
        &self.reference
    }

    #[must_use]
    pub const fn digest(&self) -> &Sha256Digest {
        &self.digest
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderproveEvidencePolicy {
    directory: PathBuf,
    byte_budget: u64,
}

impl RenderproveEvidencePolicy {
    /// Define the sole project-relative writable evidence directory and its byte budget.
    ///
    /// # Errors
    ///
    /// Returns an error for an absolute, empty, current-directory, or parent-traversing path, a
    /// zero budget, or a budget above [`MAX_EVIDENCE_BUDGET_BYTES`].
    pub fn new(
        directory: impl Into<PathBuf>,
        byte_budget: u64,
    ) -> Result<Self, RenderproveVerificationError> {
        let directory = directory.into();
        if !valid_relative_path(&directory) {
            return Err(RenderproveVerificationError::new(
                "evidence.directory",
                "must be a non-empty relative project subdirectory without parent traversal",
            ));
        }
        if byte_budget == 0 || byte_budget > MAX_EVIDENCE_BUDGET_BYTES {
            return Err(RenderproveVerificationError::new(
                "evidence.byte_budget",
                format!("must be greater than zero and at most {MAX_EVIDENCE_BUDGET_BYTES} bytes"),
            ));
        }
        Ok(Self {
            directory,
            byte_budget,
        })
    }

    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    #[must_use]
    pub const fn byte_budget(&self) -> u64 {
        self.byte_budget
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct RenderproveDeployedOrigin(String);

impl RenderproveDeployedOrigin {
    /// Validate one explicit deployed HTTPS origin.
    ///
    /// # Errors
    ///
    /// Returns an error unless the value is a bounded ASCII `https://` origin containing one
    /// DNS-style authority and an optional nonzero numeric port, with no path, query, fragment,
    /// credentials, or whitespace.
    pub fn parse(value: &str) -> Result<Self, RenderproveVerificationError> {
        let Some(authority) = value.strip_prefix("https://") else {
            return Err(RenderproveVerificationError::new(
                "network.deployed_origin",
                "must use an explicit https:// origin",
            ));
        };
        if value.len() > 2_048 || !valid_deployed_authority(authority) {
            return Err(RenderproveVerificationError::new(
                "network.deployed_origin",
                "must contain one bounded DNS-style authority with an optional nonzero numeric port",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum RenderproveReviewNetworkPolicy {
    LoopbackOnly,
    DeployedOrigin { origin: RenderproveDeployedOrigin },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderproveVerificationRequest {
    schema_version: u8,
    source: RenderproveSourceIdentity,
    project_image: ArtifactIdentity,
    worker_image: RenderproveWorkerImageIdentity,
    manifest_digest: Sha256Digest,
    evidence: RenderproveEvidencePolicy,
    network: RenderproveReviewNetworkPolicy,
}

impl RenderproveVerificationRequest {
    /// Bind a Renderprove review to exact source, project-image, worker-image, and manifest
    /// identities before any execution occurs.
    ///
    /// # Errors
    ///
    /// Returns an error unless the project image is an OCI image for the exact source repository
    /// and revision.
    pub fn new(
        source: RenderproveSourceIdentity,
        project_image: ArtifactIdentity,
        worker_image: RenderproveWorkerImageIdentity,
        manifest_digest: Sha256Digest,
        evidence: RenderproveEvidencePolicy,
        network: RenderproveReviewNetworkPolicy,
    ) -> Result<Self, RenderproveVerificationError> {
        if project_image.kind != ArtifactKind::OciImage {
            return Err(RenderproveVerificationError::new(
                "project_image.kind",
                "must be an OCI image identity",
            ));
        }
        if project_image.repository != source.repository {
            return Err(RenderproveVerificationError::new(
                "project_image.repository",
                "must match the exact source repository",
            ));
        }
        if project_image.commit != source.commit {
            return Err(RenderproveVerificationError::new(
                "project_image.commit",
                "must match the exact source revision",
            ));
        }
        Ok(Self {
            schema_version: RENDERPROVE_VERIFICATION_SCHEMA_VERSION,
            source,
            project_image,
            worker_image,
            manifest_digest,
            evidence,
            network,
        })
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
    pub const fn evidence(&self) -> &RenderproveEvidencePolicy {
        &self.evidence
    }

    #[must_use]
    pub const fn network(&self) -> &RenderproveReviewNetworkPolicy {
        &self.network
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderproveEvidenceKind {
    SanitizedReceipt,
    ApprovedScreenshot,
    PrivateWorkerIdentity,
    PrivateDiagnostics,
    FailureTrace,
    ApprovedVisualDiff,
}

impl RenderproveEvidenceKind {
    #[must_use]
    pub const fn visibility(self) -> RenderproveEvidenceVisibility {
        match self {
            Self::SanitizedReceipt | Self::ApprovedScreenshot | Self::ApprovedVisualDiff => {
                RenderproveEvidenceVisibility::Public
            }
            Self::PrivateWorkerIdentity | Self::PrivateDiagnostics | Self::FailureTrace => {
                RenderproveEvidenceVisibility::Private
            }
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::SanitizedReceipt => "sanitized_receipt",
            Self::ApprovedScreenshot => "approved_screenshot",
            Self::PrivateWorkerIdentity => "private_worker_identity",
            Self::PrivateDiagnostics => "private_diagnostics",
            Self::FailureTrace => "failure_trace",
            Self::ApprovedVisualDiff => "approved_visual_diff",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderproveEvidenceVisibility {
    Public,
    Private,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderproveEvidenceArtifact {
    kind: RenderproveEvidenceKind,
    digest: Sha256Digest,
    bytes: u64,
}

impl RenderproveEvidenceArtifact {
    /// Record one content-addressed evidence artifact.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty artifact.
    pub fn new(
        kind: RenderproveEvidenceKind,
        digest: Sha256Digest,
        bytes: u64,
    ) -> Result<Self, RenderproveVerificationError> {
        if bytes == 0 {
            return Err(RenderproveVerificationError::new(
                "artifact.bytes",
                "must be greater than zero",
            ));
        }
        Ok(Self {
            kind,
            digest,
            bytes,
        })
    }

    #[must_use]
    pub const fn kind(&self) -> RenderproveEvidenceKind {
        self.kind
    }

    #[must_use]
    pub const fn digest(&self) -> &Sha256Digest {
        &self.digest
    }

    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderproveProcessFailure {
    Command,
    AppReadiness,
    Browser,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RenderproveProcessOutcome {
    Succeeded,
    Failed {
        exit_code: u8,
        reason: RenderproveProcessFailure,
    },
    Cancelled,
    OutputLimitExceeded,
}

impl RenderproveProcessOutcome {
    const fn as_str(&self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed { .. } => "failed",
            Self::Cancelled => "cancelled",
            Self::OutputLimitExceeded => "output_limit_exceeded",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderproveCleanupFailure {
    Workspace,
    Container,
    EvidenceExport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RenderproveCleanupOutcome {
    Complete,
    Failed { reason: RenderproveCleanupFailure },
}

impl RenderproveCleanupOutcome {
    const fn as_str(&self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Failed { .. } => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RenderproveReceiptOutcome {
    Passing { digest: Sha256Digest },
    Failing { digest: Sha256Digest },
    Missing,
    Invalid,
}

impl RenderproveReceiptOutcome {
    fn human_summary(&self) -> String {
        match self {
            Self::Passing { digest } => format!("passing {}", digest.as_str()),
            Self::Failing { digest } => format!("failing {}", digest.as_str()),
            Self::Missing => "missing".to_owned(),
            Self::Invalid => "invalid".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderproveVerificationFailure {
    ProcessFailed,
    AppReadinessFailed,
    BrowserFailed,
    Cancelled,
    OutputLimitExceeded,
    ReceiptMissing,
    ReceiptInvalid,
    ReceiptFailed,
    CleanupFailed,
    EvidenceBudgetExceeded,
    ArtifactCountExceeded,
}

impl RenderproveVerificationFailure {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ProcessFailed => "process_failed",
            Self::AppReadinessFailed => "app_readiness_failed",
            Self::BrowserFailed => "browser_failed",
            Self::Cancelled => "cancelled",
            Self::OutputLimitExceeded => "output_limit_exceeded",
            Self::ReceiptMissing => "receipt_missing",
            Self::ReceiptInvalid => "receipt_invalid",
            Self::ReceiptFailed => "receipt_failed",
            Self::CleanupFailed => "cleanup_failed",
            Self::EvidenceBudgetExceeded => "evidence_budget_exceeded",
            Self::ArtifactCountExceeded => "artifact_count_exceeded",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderproveVerificationDisposition {
    Passed,
    Failed,
    Cancelled,
    CleanupFailed,
}

impl RenderproveVerificationDisposition {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::CleanupFailed => "cleanup_failed",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct RenderproveVerificationReport {
    schema_version: u8,
    process: RenderproveProcessOutcome,
    cleanup: RenderproveCleanupOutcome,
    receipt: RenderproveReceiptOutcome,
    disposition: RenderproveVerificationDisposition,
    failures: Vec<RenderproveVerificationFailure>,
    public_artifacts: Vec<RenderproveEvidenceArtifact>,
    #[serde(skip)]
    request: RenderproveVerificationRequest,
    #[serde(skip)]
    private_artifacts: Vec<RenderproveEvidenceArtifact>,
}

impl RenderproveVerificationReport {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn request(&self) -> &RenderproveVerificationRequest {
        &self.request
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
    pub const fn receipt(&self) -> &RenderproveReceiptOutcome {
        &self.receipt
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
    pub fn private_artifacts(&self) -> &[RenderproveEvidenceArtifact] {
        &self.private_artifacts
    }
}

impl fmt::Debug for RenderproveVerificationReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderproveVerificationReport")
            .field("schema_version", &self.schema_version)
            .field("process", &self.process)
            .field("cleanup", &self.cleanup)
            .field("receipt", &self.receipt)
            .field("disposition", &self.disposition)
            .field("failures", &self.failures)
            .field("public_artifacts", &self.public_artifacts)
            .field("request", &"<retained exact private request>")
            .field(
                "private_artifacts",
                &format_args!(
                    "<{} retained private artifacts>",
                    self.private_artifacts.len()
                ),
            )
            .finish()
    }
}

/// Finalize one review only after both the process and cleanup outcomes are known.
///
/// Artifact digests are split by fixed visibility policy. A successful process can pass only when
/// exactly one exported sanitized receipt matches a passing receipt assessment. Budget or count
/// exhaustion suppresses artifact export while preserving an explicit failed outcome.
///
/// # Errors
///
/// Returns an error for an impossible zero exit code or duplicate artifact identities. Runtime
/// failures, cancellation, receipt failures, cleanup failures, and evidence exhaustion are
/// represented in the returned report.
pub fn finalize_renderprove_verification(
    request: RenderproveVerificationRequest,
    process: RenderproveProcessOutcome,
    cleanup: RenderproveCleanupOutcome,
    receipt: RenderproveReceiptOutcome,
    mut artifacts: Vec<RenderproveEvidenceArtifact>,
) -> Result<RenderproveVerificationReport, RenderproveVerificationError> {
    if matches!(
        &process,
        RenderproveProcessOutcome::Failed { exit_code: 0, .. }
    ) {
        return Err(RenderproveVerificationError::new(
            "process.exit_code",
            "must be nonzero for a failed process outcome",
        ));
    }

    artifacts.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.digest.cmp(&right.digest))
    });

    let mut identities = BTreeSet::new();
    for artifact in &artifacts {
        if !identities.insert((artifact.kind, artifact.digest.clone())) {
            return Err(RenderproveVerificationError::new(
                "artifacts",
                "must not contain duplicate kind and digest identities",
            ));
        }
    }

    let mut failures = BTreeSet::new();
    match &process {
        RenderproveProcessOutcome::Succeeded => {}
        RenderproveProcessOutcome::Failed { reason, .. } => {
            failures.insert(RenderproveVerificationFailure::ProcessFailed);
            match reason {
                RenderproveProcessFailure::Command => {}
                RenderproveProcessFailure::AppReadiness => {
                    failures.insert(RenderproveVerificationFailure::AppReadinessFailed);
                }
                RenderproveProcessFailure::Browser => {
                    failures.insert(RenderproveVerificationFailure::BrowserFailed);
                }
            }
        }
        RenderproveProcessOutcome::Cancelled => {
            failures.insert(RenderproveVerificationFailure::Cancelled);
        }
        RenderproveProcessOutcome::OutputLimitExceeded => {
            failures.insert(RenderproveVerificationFailure::OutputLimitExceeded);
        }
    }
    if matches!(&cleanup, RenderproveCleanupOutcome::Failed { .. }) {
        failures.insert(RenderproveVerificationFailure::CleanupFailed);
    }

    let receipt_artifacts = artifacts
        .iter()
        .filter(|artifact| artifact.kind == RenderproveEvidenceKind::SanitizedReceipt)
        .collect::<Vec<_>>();
    match &receipt {
        RenderproveReceiptOutcome::Passing { digest }
        | RenderproveReceiptOutcome::Failing { digest } => {
            let receipt_matches = receipt_artifacts.len() == 1
                && receipt_artifacts
                    .first()
                    .is_some_and(|artifact| &artifact.digest == digest);
            if !receipt_matches {
                failures.insert(RenderproveVerificationFailure::ReceiptInvalid);
            } else if matches!(&receipt, RenderproveReceiptOutcome::Failing { .. }) {
                failures.insert(RenderproveVerificationFailure::ReceiptFailed);
            }
        }
        RenderproveReceiptOutcome::Missing => {
            failures.insert(RenderproveVerificationFailure::ReceiptMissing);
        }
        RenderproveReceiptOutcome::Invalid => {
            failures.insert(RenderproveVerificationFailure::ReceiptInvalid);
        }
    }

    let total_bytes = artifacts
        .iter()
        .try_fold(0_u64, |total, artifact| total.checked_add(artifact.bytes));
    let evidence_exhausted = match total_bytes {
        Some(total) => total > request.evidence.byte_budget,
        None => true,
    };
    let artifact_count_exhausted = artifacts.len() > MAX_EVIDENCE_ARTIFACTS;
    if evidence_exhausted {
        failures.insert(RenderproveVerificationFailure::EvidenceBudgetExceeded);
    }
    if artifact_count_exhausted {
        failures.insert(RenderproveVerificationFailure::ArtifactCountExceeded);
    }

    let suppress_artifacts = evidence_exhausted || artifact_count_exhausted;
    let (public_artifacts, private_artifacts): (Vec<_>, Vec<_>) = if suppress_artifacts {
        (Vec::new(), Vec::new())
    } else {
        artifacts.into_iter().partition(|artifact| {
            artifact.kind.visibility() == RenderproveEvidenceVisibility::Public
        })
    };

    let disposition = if failures.contains(&RenderproveVerificationFailure::CleanupFailed) {
        RenderproveVerificationDisposition::CleanupFailed
    } else if failures.contains(&RenderproveVerificationFailure::Cancelled) {
        RenderproveVerificationDisposition::Cancelled
    } else if failures.is_empty() {
        RenderproveVerificationDisposition::Passed
    } else {
        RenderproveVerificationDisposition::Failed
    };

    Ok(RenderproveVerificationReport {
        schema_version: RENDERPROVE_VERIFICATION_SCHEMA_VERSION,
        process,
        cleanup,
        receipt,
        disposition,
        failures: failures.into_iter().collect(),
        public_artifacts,
        request,
        private_artifacts,
    })
}

#[must_use]
pub fn render_renderprove_verification_human(report: &RenderproveVerificationReport) -> String {
    let failures = if report.failures.is_empty() {
        "none".to_owned()
    } else {
        report
            .failures
            .iter()
            .map(|failure| failure.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut output = format!(
        "Renderprove verification: {}\nProcess: {}\nCleanup: {}\nReceipt: {}\nFailures: {}\n",
        report.disposition.as_str(),
        report.process.as_str(),
        report.cleanup.as_str(),
        report.receipt.human_summary(),
        failures,
    );
    if report.public_artifacts.is_empty() {
        output.push_str("Public artifacts: none\n");
    } else {
        output.push_str("Public artifacts:\n");
        for artifact in &report.public_artifacts {
            output.push_str(&format!(
                "- {} {} ({} bytes)\n",
                artifact.kind.as_str(),
                artifact.digest.as_str(),
                artifact.bytes,
            ));
        }
    }
    output
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderproveVerificationError {
    pub field: String,
    pub problem: String,
}

impl RenderproveVerificationError {
    fn new(field: impl Into<String>, problem: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            problem: problem.into(),
        }
    }
}

impl fmt::Display for RenderproveVerificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", self.field, self.problem)
    }
}

impl std::error::Error for RenderproveVerificationError {}

fn valid_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path != Path::new(".")
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn valid_deployed_authority(authority: &str) -> bool {
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => (host, Some(port)),
        Some(_) => return false,
        None => (authority, None),
    };
    let port_is_valid = port.is_none_or(|value| {
        value
            .parse::<u16>()
            .is_ok_and(|parsed_port| parsed_port > 0)
    });
    !host.is_empty()
        && host.len() <= 253
        && port_is_valid
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

#[cfg(test)]
mod tests {
    use crate::artifact::{ArtifactIdentity, ArtifactKind, CommitId, RepositoryRef, Sha256Digest};

    use super::{
        MAX_EVIDENCE_ARTIFACTS, RenderproveCleanupFailure, RenderproveCleanupOutcome,
        RenderproveDeployedOrigin, RenderproveEvidenceArtifact, RenderproveEvidenceKind,
        RenderproveEvidencePolicy, RenderproveProcessFailure, RenderproveProcessOutcome,
        RenderproveReceiptOutcome, RenderproveReviewNetworkPolicy, RenderproveSourceIdentity,
        RenderproveVerificationDisposition, RenderproveVerificationFailure,
        RenderproveVerificationRequest, RenderproveWorkerImageIdentity,
        finalize_renderprove_verification, render_renderprove_verification_human,
    };

    fn digest(character: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", character.to_string().repeat(64)))
            .expect("digest")
    }

    fn indexed_digest(index: usize) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{index:064x}")).expect("digest")
    }

    fn request(byte_budget: u64) -> RenderproveVerificationRequest {
        let repository = RepositoryRef::parse("example/project").expect("repository");
        let commit = CommitId::parse(&"1a".repeat(20)).expect("commit");
        let source = RenderproveSourceIdentity::new(repository.clone(), commit.clone());
        let project_image =
            ArtifactIdentity::new(repository, commit, ArtifactKind::OciImage, digest('a'));
        RenderproveVerificationRequest::new(
            source,
            project_image,
            RenderproveWorkerImageIdentity::new("registry.example/renderprove@probe", digest('b'))
                .expect("worker"),
            digest('c'),
            RenderproveEvidencePolicy::new(".renderprove-evidence", byte_budget).expect("evidence"),
            RenderproveReviewNetworkPolicy::LoopbackOnly,
        )
        .expect("request")
    }

    fn artifact(
        kind: RenderproveEvidenceKind,
        digest_character: char,
        bytes: u64,
    ) -> RenderproveEvidenceArtifact {
        RenderproveEvidenceArtifact::new(kind, digest(digest_character), bytes).expect("artifact")
    }

    #[test]
    fn passing_report_keeps_private_identity_out_of_public_output() {
        let report = finalize_renderprove_verification(
            request(1_024),
            RenderproveProcessOutcome::Succeeded,
            RenderproveCleanupOutcome::Complete,
            RenderproveReceiptOutcome::Passing {
                digest: digest('d'),
            },
            vec![
                artifact(RenderproveEvidenceKind::PrivateWorkerIdentity, 'f', 100),
                artifact(RenderproveEvidenceKind::ApprovedScreenshot, 'e', 200),
                artifact(RenderproveEvidenceKind::SanitizedReceipt, 'd', 100),
            ],
        )
        .expect("report");

        assert_eq!(
            report.disposition(),
            RenderproveVerificationDisposition::Passed
        );
        assert!(report.failures().is_empty());
        assert_eq!(report.public_artifacts().len(), 2);
        assert_eq!(report.private_artifacts().len(), 1);
        assert_ne!(
            &report.request().project_image().digest,
            report.request().worker_image().digest()
        );
        assert_ne!(
            report.request().worker_image().digest(),
            report.request().manifest_digest()
        );

        let json = serde_json::to_string(&report).expect("json");
        assert!(json.contains(digest('d').as_str()));
        assert!(json.contains(digest('e').as_str()));
        assert!(!json.contains("registry.example"));
        assert!(!json.contains(digest('a').as_str()));
        assert!(!json.contains(digest('b').as_str()));
        assert!(!json.contains(digest('c').as_str()));
        assert!(!json.contains(digest('f').as_str()));
        assert!(!json.contains("renderprove-evidence"));

        let human = render_renderprove_verification_human(&report);
        assert!(human.contains(digest('d').as_str()));
        assert!(human.contains(digest('e').as_str()));
        assert!(!human.contains(digest('b').as_str()));
        assert!(!human.contains(digest('f').as_str()));
    }

    #[test]
    fn project_image_must_match_source_and_be_oci() {
        let repository = RepositoryRef::parse("example/project").expect("repository");
        let commit = CommitId::parse(&"1a".repeat(20)).expect("commit");
        let source = RenderproveSourceIdentity::new(repository.clone(), commit.clone());
        let archive =
            ArtifactIdentity::new(repository, commit, ArtifactKind::StaticArchive, digest('a'));
        let error = RenderproveVerificationRequest::new(
            source,
            archive,
            RenderproveWorkerImageIdentity::new("worker", digest('b')).expect("worker"),
            digest('c'),
            RenderproveEvidencePolicy::new("evidence", 1_024).expect("evidence"),
            RenderproveReviewNetworkPolicy::LoopbackOnly,
        )
        .expect_err("non-image project artifact");
        assert_eq!(error.field, "project_image.kind");
    }

    #[test]
    fn evidence_policy_requires_a_bounded_project_subdirectory() {
        assert!(RenderproveEvidencePolicy::new("evidence", 1_024).is_ok());
        assert!(RenderproveEvidencePolicy::new(".", 1_024).is_err());
        assert!(RenderproveEvidencePolicy::new("../evidence", 1_024).is_err());
        assert!(RenderproveEvidencePolicy::new("/tmp/evidence", 1_024).is_err());
        assert!(RenderproveEvidencePolicy::new("evidence", 0).is_err());
    }

    #[test]
    fn successful_process_without_passing_receipt_fails_verification() {
        let report = finalize_renderprove_verification(
            request(1_024),
            RenderproveProcessOutcome::Succeeded,
            RenderproveCleanupOutcome::Complete,
            RenderproveReceiptOutcome::Missing,
            vec![artifact(
                RenderproveEvidenceKind::ApprovedScreenshot,
                'e',
                200,
            )],
        )
        .expect("report");

        assert_eq!(
            report.disposition(),
            RenderproveVerificationDisposition::Failed
        );
        assert_eq!(
            report.failures(),
            [RenderproveVerificationFailure::ReceiptMissing]
        );
    }

    #[test]
    fn passing_receipt_requires_one_matching_sanitized_receipt_artifact() {
        let report = finalize_renderprove_verification(
            request(1_024),
            RenderproveProcessOutcome::Succeeded,
            RenderproveCleanupOutcome::Complete,
            RenderproveReceiptOutcome::Passing {
                digest: digest('d'),
            },
            vec![artifact(
                RenderproveEvidenceKind::SanitizedReceipt,
                'e',
                100,
            )],
        )
        .expect("report");

        assert_eq!(
            report.disposition(),
            RenderproveVerificationDisposition::Failed
        );
        assert_eq!(
            report.failures(),
            [RenderproveVerificationFailure::ReceiptInvalid]
        );
    }

    #[test]
    fn evidence_budget_exhaustion_suppresses_artifact_export() {
        let report = finalize_renderprove_verification(
            request(250),
            RenderproveProcessOutcome::Succeeded,
            RenderproveCleanupOutcome::Complete,
            RenderproveReceiptOutcome::Passing {
                digest: digest('d'),
            },
            vec![
                artifact(RenderproveEvidenceKind::SanitizedReceipt, 'd', 100),
                artifact(RenderproveEvidenceKind::ApprovedScreenshot, 'e', 200),
            ],
        )
        .expect("report");

        assert_eq!(
            report.disposition(),
            RenderproveVerificationDisposition::Failed
        );
        assert!(
            report
                .failures()
                .contains(&RenderproveVerificationFailure::EvidenceBudgetExceeded)
        );
        assert!(report.public_artifacts().is_empty());
        assert!(report.private_artifacts().is_empty());
    }

    #[test]
    fn artifact_count_exhaustion_suppresses_artifact_export() {
        let artifacts = (0..=MAX_EVIDENCE_ARTIFACTS)
            .map(|index| {
                RenderproveEvidenceArtifact::new(
                    RenderproveEvidenceKind::ApprovedScreenshot,
                    indexed_digest(index),
                    1,
                )
                .expect("artifact")
            })
            .collect();
        let report = finalize_renderprove_verification(
            request(1_024),
            RenderproveProcessOutcome::Succeeded,
            RenderproveCleanupOutcome::Complete,
            RenderproveReceiptOutcome::Missing,
            artifacts,
        )
        .expect("report");

        assert!(
            report
                .failures()
                .contains(&RenderproveVerificationFailure::ArtifactCountExceeded)
        );
        assert!(report.public_artifacts().is_empty());
        assert!(report.private_artifacts().is_empty());
    }

    #[test]
    fn app_readiness_and_browser_failures_are_explicit() {
        for (reason, expected_failure) in [
            (
                RenderproveProcessFailure::AppReadiness,
                RenderproveVerificationFailure::AppReadinessFailed,
            ),
            (
                RenderproveProcessFailure::Browser,
                RenderproveVerificationFailure::BrowserFailed,
            ),
        ] {
            let report = finalize_renderprove_verification(
                request(1_024),
                RenderproveProcessOutcome::Failed {
                    exit_code: 1,
                    reason,
                },
                RenderproveCleanupOutcome::Complete,
                RenderproveReceiptOutcome::Missing,
                Vec::new(),
            )
            .expect("report");
            assert!(
                report
                    .failures()
                    .contains(&RenderproveVerificationFailure::ProcessFailed)
            );
            assert!(report.failures().contains(&expected_failure));
        }
    }

    #[test]
    fn cancellation_and_cleanup_failure_remain_explicit() {
        let report = finalize_renderprove_verification(
            request(1_024),
            RenderproveProcessOutcome::Cancelled,
            RenderproveCleanupOutcome::Failed {
                reason: RenderproveCleanupFailure::Container,
            },
            RenderproveReceiptOutcome::Missing,
            Vec::new(),
        )
        .expect("report");

        assert_eq!(
            report.disposition(),
            RenderproveVerificationDisposition::CleanupFailed
        );
        assert!(
            report
                .failures()
                .contains(&RenderproveVerificationFailure::Cancelled)
        );
        assert!(
            report
                .failures()
                .contains(&RenderproveVerificationFailure::CleanupFailed)
        );
    }

    #[test]
    fn deployed_review_requires_one_https_origin() {
        assert!(RenderproveDeployedOrigin::parse("https://example.com:443").is_ok());
        assert!(RenderproveDeployedOrigin::parse("https://sub.example.com").is_ok());
        assert!(RenderproveDeployedOrigin::parse("http://example.com").is_err());
        assert!(RenderproveDeployedOrigin::parse("https://user@example.com").is_err());
        assert!(RenderproveDeployedOrigin::parse("https://example.com/path").is_err());
        assert!(RenderproveDeployedOrigin::parse("https://example.com:0").is_err());
        assert!(RenderproveDeployedOrigin::parse("https://-example.com").is_err());
    }

    #[test]
    fn duplicate_evidence_identity_is_rejected() {
        let duplicate = artifact(RenderproveEvidenceKind::ApprovedScreenshot, 'e', 100);
        let error = finalize_renderprove_verification(
            request(1_024),
            RenderproveProcessOutcome::Failed {
                exit_code: 1,
                reason: RenderproveProcessFailure::Command,
            },
            RenderproveCleanupOutcome::Complete,
            RenderproveReceiptOutcome::Invalid,
            vec![duplicate.clone(), duplicate],
        )
        .expect_err("duplicates");
        assert_eq!(error.field, "artifacts");
    }

    #[test]
    fn failed_process_requires_a_nonzero_exit_code() {
        let error = finalize_renderprove_verification(
            request(1_024),
            RenderproveProcessOutcome::Failed {
                exit_code: 0,
                reason: RenderproveProcessFailure::Command,
            },
            RenderproveCleanupOutcome::Complete,
            RenderproveReceiptOutcome::Missing,
            Vec::new(),
        )
        .expect_err("zero exit code");
        assert_eq!(error.field, "process.exit_code");
    }
}
