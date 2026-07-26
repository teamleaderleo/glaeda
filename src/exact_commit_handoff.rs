use std::collections::BTreeSet;
use std::fmt;

use serde::Serialize;

use crate::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use crate::verification_profile::{
    ExactVerificationScope, ImmutableVerificationIdentity, RepositoryCommandIdentity,
    VerificationResultDisposition,
};

pub const EXACT_COMMIT_HANDOFF_SCHEMA_VERSION: u8 = 1;
const MAX_IDENTITY_LENGTH: usize = 128;
const MAX_REF_LENGTH: usize = 512;
const MAX_CHANGED_PATH_LENGTH: usize = 1_024;
const MAX_CHANGED_PATHS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerIdentityClass {
    LimaGuest,
    SelfHostedRunner,
    LeasedHost,
}

impl RunnerIdentityClass {
    const fn as_str(self) -> &'static str {
        match self {
            Self::LimaGuest => "lima_guest",
            Self::SelfHostedRunner => "self_hosted_runner",
            Self::LeasedHost => "leased_host",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct RunnerIdentity {
    class: RunnerIdentityClass,
    id: String,
}

impl RunnerIdentity {
    pub fn new(
        class: RunnerIdentityClass,
        id: impl Into<String>,
    ) -> Result<Self, ExactCommitHandoffError> {
        let id = id.into();
        validate_identifier("runner.id", &id)?;
        Ok(Self { class, id })
    }

    #[must_use]
    pub const fn class(&self) -> RunnerIdentityClass {
        self.class
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct WorkspaceIdentity {
    runner_id: String,
    id: String,
}

impl WorkspaceIdentity {
    pub fn new(
        runner_id: impl Into<String>,
        id: impl Into<String>,
    ) -> Result<Self, ExactCommitHandoffError> {
        let runner_id = runner_id.into();
        let id = id.into();
        validate_identifier("workspace.runner_id", &runner_id)?;
        validate_identifier("workspace.id", &id)?;
        Ok(Self { runner_id, id })
    }

    #[must_use]
    pub fn runner_id(&self) -> &str {
        &self.runner_id
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PublisherIdentityClass {
    CredentialedOperatorHost,
    CredentialedAutomation,
}

impl PublisherIdentityClass {
    const fn as_str(self) -> &'static str {
        match self {
            Self::CredentialedOperatorHost => "credentialed_operator_host",
            Self::CredentialedAutomation => "credentialed_automation",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct RepositoryPath(String);

impl RepositoryPath {
    pub fn parse(value: &str) -> Result<Self, ExactCommitHandoffError> {
        if !valid_repository_path(value) {
            return Err(ExactCommitHandoffError::invalid_input(
                HandoffPhase::Planning,
                "changed_paths",
                "must contain bounded repository-relative paths without traversal, empty components, backslashes, or control characters",
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
pub struct TargetRef {
    repository: RepositoryRef,
    name: String,
}

impl TargetRef {
    pub fn new(
        repository: RepositoryRef,
        name: impl Into<String>,
    ) -> Result<Self, ExactCommitHandoffError> {
        let name = name.into();
        if !valid_target_ref(&name) {
            return Err(ExactCommitHandoffError::invalid_input(
                HandoffPhase::Planning,
                "target_ref",
                "must be one bounded canonical refs/heads/* Git reference",
            ));
        }
        Ok(Self { repository, name })
    }

    #[must_use]
    pub const fn repository(&self) -> &RepositoryRef {
        &self.repository
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CandidateAncestryRequirement {
    ExactParent { parent: CommitId },
    DescendantOf { ancestor: CommitId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CandidateAncestryEvidence {
    direct_parent: CommitId,
    ancestors: Vec<CommitId>,
}

impl CandidateAncestryEvidence {
    pub fn new(
        direct_parent: CommitId,
        ancestors: Vec<CommitId>,
    ) -> Result<Self, ExactCommitHandoffError> {
        let mut ancestors = canonical_commit_ids(ancestors);
        if !ancestors.contains(&direct_parent) {
            ancestors.push(direct_parent.clone());
            ancestors.sort();
        }
        if ancestors.len() > MAX_CHANGED_PATHS {
            return Err(ExactCommitHandoffError::invalid_input(
                HandoffPhase::Planning,
                "candidate.ancestry",
                "contains more ancestry identities than the bounded contract permits",
            ));
        }
        Ok(Self {
            direct_parent,
            ancestors,
        })
    }

    #[must_use]
    pub const fn direct_parent(&self) -> &CommitId {
        &self.direct_parent
    }

    #[must_use]
    pub fn ancestors(&self) -> &[CommitId] {
        &self.ancestors
    }

    fn proves(&self, requirement: &CandidateAncestryRequirement) -> bool {
        match requirement {
            CandidateAncestryRequirement::ExactParent { parent } => &self.direct_parent == parent,
            CandidateAncestryRequirement::DescendantOf { ancestor } => {
                self.ancestors.contains(ancestor)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeState {
    Clean,
    Dirty,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorktreeObservation {
    state: WorktreeState,
    evidence_digest: Sha256Digest,
}

impl WorktreeObservation {
    #[must_use]
    pub const fn new(state: WorktreeState, evidence_digest: Sha256Digest) -> Self {
        Self {
            state,
            evidence_digest,
        }
    }

    #[must_use]
    pub const fn state(&self) -> WorktreeState {
        self.state
    }

    #[must_use]
    pub const fn evidence_digest(&self) -> &Sha256Digest {
        &self.evidence_digest
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CandidateCommitObservation {
    commit: CommitId,
    tree: GitTreeId,
    ancestry: CandidateAncestryEvidence,
    changed_paths: Vec<RepositoryPath>,
    worktree: WorktreeObservation,
    verification_identity: Option<ImmutableVerificationIdentity>,
}

impl CandidateCommitObservation {
    pub fn new(
        commit: CommitId,
        tree: GitTreeId,
        ancestry: CandidateAncestryEvidence,
        changed_paths: Vec<RepositoryPath>,
        worktree: WorktreeObservation,
        verification_identity: Option<ImmutableVerificationIdentity>,
    ) -> Result<Self, ExactCommitHandoffError> {
        let changed_paths = canonical_paths(changed_paths, HandoffPhase::Planning)?;
        Ok(Self {
            commit,
            tree,
            ancestry,
            changed_paths,
            worktree,
            verification_identity,
        })
    }

    #[must_use]
    pub const fn commit(&self) -> &CommitId {
        &self.commit
    }

    #[must_use]
    pub const fn tree(&self) -> &GitTreeId {
        &self.tree
    }

    #[must_use]
    pub fn changed_paths(&self) -> &[RepositoryPath] {
        &self.changed_paths
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExactCommitHandoffRequest {
    schema_version: u8,
    runner: RunnerIdentity,
    workspace: WorkspaceIdentity,
    target: TargetRef,
    ancestry: CandidateAncestryRequirement,
    allowed_changed_paths: Vec<RepositoryPath>,
    expected_remote_parent: CommitId,
    expected_verification_schema_version: u8,
    expected_verification_digest: Sha256Digest,
    expected_verification_command: RepositoryCommandIdentity,
    expected_verification_target_scope: ExactVerificationScope,
}

impl ExactCommitHandoffRequest {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        runner: RunnerIdentity,
        workspace: WorkspaceIdentity,
        target: TargetRef,
        ancestry: CandidateAncestryRequirement,
        allowed_changed_paths: Vec<RepositoryPath>,
        expected_remote_parent: CommitId,
        expected_verification_schema_version: u8,
        expected_verification_digest: Sha256Digest,
        expected_verification_command: RepositoryCommandIdentity,
        expected_verification_target_scope: ExactVerificationScope,
    ) -> Result<Self, ExactCommitHandoffError> {
        if workspace.runner_id != runner.id {
            return Err(ExactCommitHandoffError::invalid_input(
                HandoffPhase::Planning,
                "workspace.runner_id",
                "must match the selected runner identity",
            ));
        }
        let allowed_changed_paths = canonical_paths(allowed_changed_paths, HandoffPhase::Planning)?;
        if expected_verification_schema_version == 0 {
            return Err(ExactCommitHandoffError::invalid_input(
                HandoffPhase::Planning,
                "verification.schema_version",
                "must identify one nonzero immutable verification schema version",
            ));
        }
        if allowed_changed_paths.is_empty() {
            return Err(ExactCommitHandoffError::invalid_input(
                HandoffPhase::Planning,
                "allowed_changed_paths",
                "must contain at least one reviewed repository path",
            ));
        }
        Ok(Self {
            schema_version: EXACT_COMMIT_HANDOFF_SCHEMA_VERSION,
            runner,
            workspace,
            target,
            ancestry,
            allowed_changed_paths,
            expected_remote_parent,
            expected_verification_schema_version,
            expected_verification_digest,
            expected_verification_command,
            expected_verification_target_scope,
        })
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HandoffImmutableIdentity {
    target: TargetRef,
    expected_remote_parent: CommitId,
    candidate_commit: CommitId,
    candidate_parent: CommitId,
    tree: GitTreeId,
    changed_paths: Vec<RepositoryPath>,
    verification: ImmutableVerificationIdentity,
}

impl HandoffImmutableIdentity {
    #[must_use]
    pub const fn target(&self) -> &TargetRef {
        &self.target
    }

    #[must_use]
    pub const fn expected_remote_parent(&self) -> &CommitId {
        &self.expected_remote_parent
    }

    #[must_use]
    pub const fn candidate_commit(&self) -> &CommitId {
        &self.candidate_commit
    }

    #[must_use]
    pub const fn candidate_parent(&self) -> &CommitId {
        &self.candidate_parent
    }

    #[must_use]
    pub const fn tree(&self) -> &GitTreeId {
        &self.tree
    }

    #[must_use]
    pub fn changed_paths(&self) -> &[RepositoryPath] {
        &self.changed_paths
    }

    #[must_use]
    pub const fn verification(&self) -> &ImmutableVerificationIdentity {
        &self.verification
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExactCommitHandoffPlan {
    schema_version: u8,
    runner: RunnerIdentity,
    workspace: WorkspaceIdentity,
    ancestry: CandidateAncestryRequirement,
    allowed_changed_paths: Vec<RepositoryPath>,
    worktree_evidence_digest: Sha256Digest,
    identity: HandoffImmutableIdentity,
}

impl ExactCommitHandoffPlan {
    pub fn new(
        request: ExactCommitHandoffRequest,
        mut candidates: Vec<CandidateCommitObservation>,
    ) -> Result<Self, ExactCommitHandoffError> {
        if candidates.len() != 1 {
            return Err(ExactCommitHandoffError::fixed(
                HandoffRefusalCode::AmbiguousCandidate,
                HandoffPhase::Planning,
                "candidate",
                "candidate resolution must produce exactly one immutable commit",
            ));
        }
        let candidate = candidates.pop().expect("length checked");
        match candidate.worktree.state {
            WorktreeState::Clean => {}
            WorktreeState::Dirty => {
                return Err(ExactCommitHandoffError::fixed(
                    HandoffRefusalCode::DirtyWorkspace,
                    HandoffPhase::Planning,
                    "worktree",
                    "the runner workspace contains uncommitted changes",
                ));
            }
            WorktreeState::Unknown => {
                return Err(ExactCommitHandoffError::fixed(
                    HandoffRefusalCode::UnknownWorkspaceState,
                    HandoffPhase::Planning,
                    "worktree",
                    "the runner workspace cleanliness is unknown",
                ));
            }
        }
        if !candidate.ancestry.proves(&request.ancestry) {
            return Err(ExactCommitHandoffError::fixed(
                HandoffRefusalCode::AncestryMismatch,
                HandoffPhase::Planning,
                "candidate.ancestry",
                "the candidate commit does not satisfy the reviewed ancestry requirement",
            ));
        }
        if !candidate
            .ancestry
            .ancestors
            .contains(&request.expected_remote_parent)
        {
            return Err(ExactCommitHandoffError::fixed(
                HandoffRefusalCode::AncestryMismatch,
                HandoffPhase::Planning,
                "expected_remote_parent",
                "the expected remote parent is not proven in the candidate ancestry",
            ));
        }
        if candidate.changed_paths.is_empty() {
            return Err(ExactCommitHandoffError::fixed(
                HandoffRefusalCode::ChangedPathOutsideAllowlist,
                HandoffPhase::Planning,
                "candidate.changed_paths",
                "the candidate must contain at least one reviewed changed path",
            ));
        }
        let allowed = request
            .allowed_changed_paths
            .iter()
            .collect::<BTreeSet<_>>();
        if candidate
            .changed_paths
            .iter()
            .any(|path| !allowed.contains(path))
        {
            return Err(ExactCommitHandoffError::fixed(
                HandoffRefusalCode::ChangedPathOutsideAllowlist,
                HandoffPhase::Planning,
                "candidate.changed_paths",
                "the candidate changes a path outside the reviewed allowlist",
            ));
        }
        let Some(verification) = candidate.verification_identity else {
            return Err(ExactCommitHandoffError::fixed(
                HandoffRefusalCode::MissingVerificationIdentity,
                HandoffPhase::Planning,
                "verification",
                "an immutable passing verification identity is required",
            ));
        };
        match verification.disposition() {
            VerificationResultDisposition::Passed => {}
            VerificationResultDisposition::Failed => {
                return Err(ExactCommitHandoffError::fixed(
                    HandoffRefusalCode::VerificationFailed,
                    HandoffPhase::Planning,
                    "verification.disposition",
                    "the immutable verification result failed",
                ));
            }
            VerificationResultDisposition::CleanupIncomplete => {
                return Err(ExactCommitHandoffError::fixed(
                    HandoffRefusalCode::VerificationCleanupIncomplete,
                    HandoffPhase::Planning,
                    "verification.disposition",
                    "the immutable verification result has incomplete cleanup",
                ));
            }
        }
        let Some(tested_commit) = verification.tested_commit() else {
            return Err(ExactCommitHandoffError::fixed(
                HandoffRefusalCode::VerificationSourceNotCommit,
                HandoffPhase::Planning,
                "verification.tested_source",
                "commit publication requires a commit-backed verification identity",
            ));
        };
        if verification.schema_version() != request.expected_verification_schema_version {
            return Err(ExactCommitHandoffError::fixed(
                HandoffRefusalCode::VerificationSchemaMismatch,
                HandoffPhase::Planning,
                "verification.schema_version",
                "the verification schema differs from the reviewed expected schema",
            ));
        }
        if verification.receipt_digest() != &request.expected_verification_digest {
            return Err(ExactCommitHandoffError::fixed(
                HandoffRefusalCode::VerificationDigestMismatch,
                HandoffPhase::Planning,
                "verification.receipt_digest",
                "the verification receipt digest differs from the reviewed expected digest",
            ));
        }
        if verification.repository() != request.target.repository() {
            return Err(ExactCommitHandoffError::fixed(
                HandoffRefusalCode::VerificationRepositoryMismatch,
                HandoffPhase::Planning,
                "verification.repository",
                "the verification repository differs from the publication target repository",
            ));
        }
        if tested_commit != &candidate.commit {
            return Err(ExactCommitHandoffError::fixed(
                HandoffRefusalCode::VerificationCommitMismatch,
                HandoffPhase::Planning,
                "verification.tested_source.commit",
                "the verified commit differs from the candidate commit",
            ));
        }
        if verification.tested_tree() != &candidate.tree {
            return Err(ExactCommitHandoffError::fixed(
                HandoffRefusalCode::VerificationTreeMismatch,
                HandoffPhase::Planning,
                "verification.tested_source.tree",
                "the verified tree differs from the candidate tree",
            ));
        }
        if verification.command() != &request.expected_verification_command {
            return Err(ExactCommitHandoffError::fixed(
                HandoffRefusalCode::VerificationCommandMismatch,
                HandoffPhase::Planning,
                "verification.command",
                "the verification command differs from the reviewed expected command",
            ));
        }
        if verification.target_scope() != &request.expected_verification_target_scope {
            return Err(ExactCommitHandoffError::fixed(
                HandoffRefusalCode::VerificationTargetScopeMismatch,
                HandoffPhase::Planning,
                "verification.target_scope",
                "the verification target scope differs from the reviewed expected scope",
            ));
        }
        let identity = HandoffImmutableIdentity {
            target: request.target,
            expected_remote_parent: request.expected_remote_parent,
            candidate_commit: candidate.commit,
            candidate_parent: candidate.ancestry.direct_parent,
            tree: candidate.tree,
            changed_paths: candidate.changed_paths,
            verification,
        };
        Ok(Self {
            schema_version: EXACT_COMMIT_HANDOFF_SCHEMA_VERSION,
            runner: request.runner,
            workspace: request.workspace,
            ancestry: request.ancestry,
            allowed_changed_paths: request.allowed_changed_paths,
            worktree_evidence_digest: candidate.worktree.evidence_digest,
            identity,
        })
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn runner(&self) -> &RunnerIdentity {
        &self.runner
    }

    #[must_use]
    pub const fn workspace(&self) -> &WorkspaceIdentity {
        &self.workspace
    }

    #[must_use]
    pub const fn identity(&self) -> &HandoffImmutableIdentity {
        &self.identity
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HandoffExportReceipt {
    schema_version: u8,
    identity: HandoffImmutableIdentity,
    package_digest: Sha256Digest,
}

impl HandoffExportReceipt {
    pub fn new(
        plan: &ExactCommitHandoffPlan,
        package_digest: Sha256Digest,
        exported_commit: CommitId,
        exported_tree: GitTreeId,
    ) -> Result<Self, ExactCommitHandoffError> {
        if exported_commit != plan.identity.candidate_commit || exported_tree != plan.identity.tree
        {
            return Err(ExactCommitHandoffError::fixed(
                HandoffRefusalCode::ExportIdentityMismatch,
                HandoffPhase::Export,
                "exported_identity",
                "the export package does not contain the exact planned commit and tree",
            ));
        }
        Ok(Self {
            schema_version: EXACT_COMMIT_HANDOFF_SCHEMA_VERSION,
            identity: plan.identity.clone(),
            package_digest,
        })
    }

    #[must_use]
    pub const fn identity(&self) -> &HandoffImmutableIdentity {
        &self.identity
    }

    #[must_use]
    pub const fn package_digest(&self) -> &Sha256Digest {
        &self.package_digest
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferDisposition {
    Complete,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HandoffTransferObservation {
    disposition: TransferDisposition,
    package_digest: Sha256Digest,
    commit: CommitId,
    tree: GitTreeId,
}

impl HandoffTransferObservation {
    #[must_use]
    pub const fn new(
        disposition: TransferDisposition,
        package_digest: Sha256Digest,
        commit: CommitId,
        tree: GitTreeId,
    ) -> Self {
        Self {
            disposition,
            package_digest,
            commit,
            tree,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HandoffTransferReceipt {
    schema_version: u8,
    identity: HandoffImmutableIdentity,
    package_digest: Sha256Digest,
}

impl HandoffTransferReceipt {
    pub fn new(
        export: &HandoffExportReceipt,
        observation: HandoffTransferObservation,
    ) -> Result<Self, ExactCommitHandoffError> {
        if observation.disposition == TransferDisposition::Failed {
            return Err(ExactCommitHandoffError::fixed(
                HandoffRefusalCode::TransferFailed,
                HandoffPhase::Transfer,
                "transfer",
                "the content-addressed transfer did not complete",
            ));
        }
        if observation.package_digest != export.package_digest
            || observation.commit != export.identity.candidate_commit
            || observation.tree != export.identity.tree
        {
            return Err(ExactCommitHandoffError::fixed(
                HandoffRefusalCode::AlteredTransferPackage,
                HandoffPhase::Transfer,
                "transfer_package",
                "the received transfer package differs from the exact exported package identity",
            ));
        }
        Ok(Self {
            schema_version: EXACT_COMMIT_HANDOFF_SCHEMA_VERSION,
            identity: export.identity.clone(),
            package_digest: export.package_digest.clone(),
        })
    }

    #[must_use]
    pub const fn identity(&self) -> &HandoffImmutableIdentity {
        &self.identity
    }

    #[must_use]
    pub const fn package_digest(&self) -> &Sha256Digest {
        &self.package_digest
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportDisposition {
    Complete,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HandoffImportObservation {
    disposition: ImportDisposition,
    commit: CommitId,
    direct_parent: CommitId,
    tree: GitTreeId,
    changed_paths: Vec<RepositoryPath>,
    verification: ImmutableVerificationIdentity,
}

impl HandoffImportObservation {
    pub fn new(
        disposition: ImportDisposition,
        commit: CommitId,
        direct_parent: CommitId,
        tree: GitTreeId,
        changed_paths: Vec<RepositoryPath>,
        verification: ImmutableVerificationIdentity,
    ) -> Result<Self, ExactCommitHandoffError> {
        Ok(Self {
            disposition,
            commit,
            direct_parent,
            tree,
            changed_paths: canonical_paths(changed_paths, HandoffPhase::Import)?,
            verification,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HandoffImportReceipt {
    schema_version: u8,
    identity: HandoffImmutableIdentity,
    package_digest: Sha256Digest,
}

impl HandoffImportReceipt {
    pub fn new(
        transfer: &HandoffTransferReceipt,
        observation: HandoffImportObservation,
    ) -> Result<Self, ExactCommitHandoffError> {
        if observation.disposition == ImportDisposition::Failed {
            return Err(ExactCommitHandoffError::fixed(
                HandoffRefusalCode::ImportFailed,
                HandoffPhase::Import,
                "import",
                "the publisher could not import the content-addressed transfer package",
            ));
        }
        if observation.commit != transfer.identity.candidate_commit
            || observation.direct_parent != transfer.identity.candidate_parent
            || observation.tree != transfer.identity.tree
            || observation.changed_paths != transfer.identity.changed_paths
            || observation.verification != transfer.identity.verification
        {
            return Err(ExactCommitHandoffError::fixed(
                HandoffRefusalCode::ImportedIdentityMismatch,
                HandoffPhase::Import,
                "imported_identity",
                "the imported commit, parent, tree, changed paths, or verification identity differ from the plan",
            ));
        }
        Ok(Self {
            schema_version: EXACT_COMMIT_HANDOFF_SCHEMA_VERSION,
            identity: transfer.identity.clone(),
            package_digest: transfer.package_digest.clone(),
        })
    }

    #[must_use]
    pub const fn identity(&self) -> &HandoffImmutableIdentity {
        &self.identity
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HandoffPublishAuthorization {
    schema_version: u8,
    identity: HandoffImmutableIdentity,
    publisher: PublisherIdentityClass,
    observed_remote_parent: CommitId,
    fast_forward_only: bool,
}

impl HandoffPublishAuthorization {
    pub fn new(
        import: &HandoffImportReceipt,
        publisher: PublisherIdentityClass,
        observed_remote_parent: CommitId,
    ) -> Result<Self, ExactCommitHandoffError> {
        if observed_remote_parent != import.identity.expected_remote_parent {
            return Err(ExactCommitHandoffError::fixed(
                HandoffRefusalCode::MovedRemoteRef,
                HandoffPhase::PublishAuthorization,
                "remote_parent",
                "the target ref moved away from the reviewed expected remote parent",
            ));
        }
        Ok(Self {
            schema_version: EXACT_COMMIT_HANDOFF_SCHEMA_VERSION,
            identity: import.identity.clone(),
            publisher,
            observed_remote_parent,
            fast_forward_only: true,
        })
    }

    #[must_use]
    pub const fn identity(&self) -> &HandoffImmutableIdentity {
        &self.identity
    }

    #[must_use]
    pub const fn publisher(&self) -> PublisherIdentityClass {
        self.publisher
    }

    #[must_use]
    pub const fn fast_forward_only(&self) -> bool {
        self.fast_forward_only
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationDisposition {
    Published,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HandoffPublicationObservation {
    disposition: PublicationDisposition,
    final_remote_commit: CommitId,
    final_remote_tree: GitTreeId,
}

impl HandoffPublicationObservation {
    #[must_use]
    pub const fn new(
        disposition: PublicationDisposition,
        final_remote_commit: CommitId,
        final_remote_tree: GitTreeId,
    ) -> Self {
        Self {
            disposition,
            final_remote_commit,
            final_remote_tree,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HandoffPublicationResult {
    schema_version: u8,
    identity: HandoffImmutableIdentity,
    publisher: PublisherIdentityClass,
    final_remote_commit: CommitId,
    final_remote_tree: GitTreeId,
}

impl HandoffPublicationResult {
    pub fn new(
        authorization: &HandoffPublishAuthorization,
        observation: HandoffPublicationObservation,
    ) -> Result<Self, ExactCommitHandoffError> {
        if observation.disposition == PublicationDisposition::Failed {
            return Err(ExactCommitHandoffError::fixed(
                HandoffRefusalCode::PublicationFailed,
                HandoffPhase::Publication,
                "publication",
                "the publisher did not complete the authorized fast-forward-only update",
            ));
        }
        if observation.final_remote_commit != authorization.identity.candidate_commit
            || observation.final_remote_tree != authorization.identity.tree
        {
            return Err(ExactCommitHandoffError::fixed(
                HandoffRefusalCode::PublishedIdentityMismatch,
                HandoffPhase::Publication,
                "final_remote_identity",
                "the final remote commit or tree differs from the exact authorized candidate",
            ));
        }
        Ok(Self {
            schema_version: EXACT_COMMIT_HANDOFF_SCHEMA_VERSION,
            identity: authorization.identity.clone(),
            publisher: authorization.publisher,
            final_remote_commit: observation.final_remote_commit,
            final_remote_tree: observation.final_remote_tree,
        })
    }

    #[must_use]
    pub const fn identity(&self) -> &HandoffImmutableIdentity {
        &self.identity
    }

    #[must_use]
    pub const fn final_remote_commit(&self) -> &CommitId {
        &self.final_remote_commit
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupResource {
    RunnerTemporaryRef,
    ExportPackage,
    PublisherTemporaryRef,
    ImportedPackage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HandoffCleanupObservation {
    remaining_resources: Vec<CleanupResource>,
    runner_worktree: WorktreeState,
}

impl HandoffCleanupObservation {
    #[must_use]
    pub fn new(
        mut remaining_resources: Vec<CleanupResource>,
        runner_worktree: WorktreeState,
    ) -> Self {
        remaining_resources.sort();
        remaining_resources.dedup();
        Self {
            remaining_resources,
            runner_worktree,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupDisposition {
    Complete,
    Incomplete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HandoffCleanupResult {
    schema_version: u8,
    identity: HandoffImmutableIdentity,
    disposition: CleanupDisposition,
    remaining_resources: Vec<CleanupResource>,
    runner_worktree: WorktreeState,
}

impl HandoffCleanupResult {
    #[must_use]
    pub fn new(plan: &ExactCommitHandoffPlan, observation: HandoffCleanupObservation) -> Self {
        let disposition = if observation.remaining_resources.is_empty()
            && observation.runner_worktree == WorktreeState::Clean
        {
            CleanupDisposition::Complete
        } else {
            CleanupDisposition::Incomplete
        };
        Self {
            schema_version: EXACT_COMMIT_HANDOFF_SCHEMA_VERSION,
            identity: plan.identity.clone(),
            disposition,
            remaining_resources: observation.remaining_resources,
            runner_worktree: observation.runner_worktree,
        }
    }

    #[must_use]
    pub const fn disposition(&self) -> CleanupDisposition {
        self.disposition
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffReportDisposition {
    Published,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExactCommitHandoffReport {
    schema_version: u8,
    disposition: HandoffReportDisposition,
    runner: RunnerIdentity,
    workspace: WorkspaceIdentity,
    publisher: PublisherIdentityClass,
    target: TargetRef,
    expected_remote_parent: CommitId,
    candidate_commit: CommitId,
    final_remote_commit: CommitId,
    tree: GitTreeId,
    changed_paths: Vec<RepositoryPath>,
    verification: ImmutableVerificationIdentity,
    cleanup: CleanupDisposition,
}

impl ExactCommitHandoffReport {
    pub fn new(
        plan: &ExactCommitHandoffPlan,
        export: &HandoffExportReceipt,
        transfer: &HandoffTransferReceipt,
        import: &HandoffImportReceipt,
        authorization: &HandoffPublishAuthorization,
        publication: &HandoffPublicationResult,
        cleanup: &HandoffCleanupResult,
    ) -> Result<Self, ExactCommitHandoffError> {
        let identity = plan.identity();
        if export.identity() != identity
            || transfer.identity() != identity
            || import.identity() != identity
            || authorization.identity() != identity
            || publication.identity() != identity
            || &cleanup.identity != identity
        {
            return Err(ExactCommitHandoffError::fixed(
                HandoffRefusalCode::PhaseIdentityMismatch,
                HandoffPhase::Reporting,
                "phase_identity",
                "one or more phase receipts do not preserve the exact planned identity",
            ));
        }
        if cleanup.disposition != CleanupDisposition::Complete {
            return Err(ExactCommitHandoffError::fixed(
                HandoffRefusalCode::CleanupIncomplete,
                HandoffPhase::Cleanup,
                "cleanup",
                "temporary handoff resources remain or the runner worktree is not clean",
            ));
        }
        Ok(Self {
            schema_version: EXACT_COMMIT_HANDOFF_SCHEMA_VERSION,
            disposition: HandoffReportDisposition::Published,
            runner: plan.runner.clone(),
            workspace: plan.workspace.clone(),
            publisher: publication.publisher,
            target: identity.target.clone(),
            expected_remote_parent: identity.expected_remote_parent.clone(),
            candidate_commit: identity.candidate_commit.clone(),
            final_remote_commit: publication.final_remote_commit.clone(),
            tree: identity.tree.clone(),
            changed_paths: identity.changed_paths.clone(),
            verification: identity.verification.clone(),
            cleanup: cleanup.disposition,
        })
    }

    #[must_use]
    pub const fn candidate_commit(&self) -> &CommitId {
        &self.candidate_commit
    }

    #[must_use]
    pub const fn final_remote_commit(&self) -> &CommitId {
        &self.final_remote_commit
    }
}

#[must_use]
pub fn render_exact_commit_handoff_human(report: &ExactCommitHandoffReport) -> String {
    let changed_paths = report
        .changed_paths
        .iter()
        .map(RepositoryPath::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Exact-commit handoff: published\nRunner: {}/{}\nWorkspace: {}\nPublisher: {}\nTarget: {}:{}\nExpected remote parent: {}\nCandidate commit: {}\nFinal remote commit: {}\nTree: {}\nChanged paths: {}\nVerification: passed schema={} {}\nCleanup: complete\n",
        report.runner.class.as_str(),
        report.runner.id,
        report.workspace.id,
        report.publisher.as_str(),
        report.target.repository.as_str(),
        report.target.name,
        report.expected_remote_parent.as_str(),
        report.candidate_commit.as_str(),
        report.final_remote_commit.as_str(),
        report.tree.as_str(),
        changed_paths,
        report.verification.schema_version(),
        report.verification.receipt_digest().as_str(),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffPhase {
    Planning,
    Export,
    Transfer,
    Import,
    PublishAuthorization,
    Publication,
    Cleanup,
    Reporting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffRefusalCode {
    InvalidInput,
    AmbiguousCandidate,
    DirtyWorkspace,
    UnknownWorkspaceState,
    ChangedPathOutsideAllowlist,
    AncestryMismatch,
    MissingVerificationIdentity,
    VerificationFailed,
    VerificationCleanupIncomplete,
    VerificationSourceNotCommit,
    VerificationSchemaMismatch,
    VerificationDigestMismatch,
    VerificationRepositoryMismatch,
    VerificationCommitMismatch,
    VerificationTreeMismatch,
    VerificationCommandMismatch,
    VerificationTargetScopeMismatch,
    ExportIdentityMismatch,
    AlteredTransferPackage,
    TransferFailed,
    ImportFailed,
    ImportedIdentityMismatch,
    MovedRemoteRef,
    PublicationFailed,
    PublishedIdentityMismatch,
    CleanupIncomplete,
    PhaseIdentityMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExactCommitHandoffError {
    pub code: HandoffRefusalCode,
    pub phase: HandoffPhase,
    pub field: String,
    pub public_message: String,
}

impl ExactCommitHandoffError {
    fn invalid_input(phase: HandoffPhase, field: &str, public_message: &str) -> Self {
        Self::fixed(
            HandoffRefusalCode::InvalidInput,
            phase,
            field,
            public_message,
        )
    }

    fn fixed(
        code: HandoffRefusalCode,
        phase: HandoffPhase,
        field: &str,
        public_message: &str,
    ) -> Self {
        Self {
            code,
            phase,
            field: field.to_owned(),
            public_message: public_message.to_owned(),
        }
    }
}

impl fmt::Display for ExactCommitHandoffError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.public_message)
    }
}

impl std::error::Error for ExactCommitHandoffError {}

fn validate_identifier(field: &str, value: &str) -> Result<(), ExactCommitHandoffError> {
    if value.is_empty()
        || value.len() > MAX_IDENTITY_LENGTH
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(ExactCommitHandoffError::invalid_input(
            HandoffPhase::Planning,
            field,
            "must use one bounded ASCII identifier containing only letters, digits, '.', '_', or '-'",
        ));
    }
    Ok(())
}

fn valid_repository_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_CHANGED_PATH_LENGTH
        && !value.starts_with('/')
        && !value.ends_with('/')
        && !value.contains('\\')
        && !value.chars().any(char::is_control)
        && value
            .split('/')
            .all(|component| !component.is_empty() && !matches!(component, "." | ".."))
}

fn valid_target_ref(value: &str) -> bool {
    let Some(suffix) = value.strip_prefix("refs/heads/") else {
        return false;
    };
    !suffix.is_empty()
        && value.len() <= MAX_REF_LENGTH
        && !value.ends_with('/')
        && !value.ends_with('.')
        && !value.contains("..")
        && !value.contains("@{")
        && !value.chars().any(|character| {
            character.is_control()
                || character.is_whitespace()
                || matches!(character, '~' | '^' | ':' | '?' | '*' | '[' | '\\')
        })
        && suffix.split('/').all(|component| {
            !component.is_empty()
                && component != "."
                && component != ".."
                && !component.ends_with(".lock")
        })
}

fn canonical_paths(
    mut paths: Vec<RepositoryPath>,
    phase: HandoffPhase,
) -> Result<Vec<RepositoryPath>, ExactCommitHandoffError> {
    if paths.len() > MAX_CHANGED_PATHS {
        return Err(ExactCommitHandoffError::invalid_input(
            phase,
            "changed_paths",
            "contains more repository paths than the bounded contract permits",
        ));
    }
    paths.sort();
    let original_len = paths.len();
    paths.dedup();
    if paths.len() != original_len {
        return Err(ExactCommitHandoffError::invalid_input(
            phase,
            "changed_paths",
            "must not contain duplicate repository paths",
        ));
    }
    Ok(paths)
}

fn canonical_commit_ids(mut commits: Vec<CommitId>) -> Vec<CommitId> {
    commits.sort();
    commits.dedup();
    commits
}

#[cfg(test)]
mod tests;
