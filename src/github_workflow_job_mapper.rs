use std::collections::BTreeSet;
use std::fmt;

use serde::Serialize;

use crate::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use crate::execution_admission::{
    EpochMillis, ExecutionAdmissionIdentity, ExecutionRequestId, ExecutionResourceLimits,
    FallbackProfileEligibility, RunnerProfileId,
};
use crate::personal_worker_queue::{
    PERSONAL_WORKER_SCHEDULABLE_CPU_MILLIS, PERSONAL_WORKER_SCHEDULABLE_MEMORY_BYTES,
    PersonalWorkerCacheAccessMode, PersonalWorkerCacheNamespace, PersonalWorkerCancellationState,
    PersonalWorkerJobRequest, PersonalWorkerPriority, PersonalWorkerSourceIdentity,
};
use crate::verification_profile::{CacheId, VerificationProfileId};

pub const GITHUB_WORKFLOW_JOB_MAPPER_SCHEMA_VERSION: u8 = 1;
pub const MAX_GITHUB_WORKFLOW_JOB_ROUTES: usize = 32;
pub const MAX_GITHUB_WORKFLOW_JOB_EVIDENCE_AGE_MILLIS: u64 = 300_000;

const MAX_GITHUB_IDENTIFIER_LEN: usize = 96;
const MAX_GITHUB_REVIEWED_NAME_LEN: usize = 128;
const MAX_GITHUB_ROUTE_REVISION: u64 = 1_000_000_000_000;
const MAX_GITHUB_QUEUE_SNAPSHOT_GENERATION: u64 = 1_000_000_000_000;

macro_rules! identifier_type {
    ($name:ident, $field:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Parse one bounded public mapper identifier.
            ///
            /// # Errors
            ///
            /// Returns an error for an empty, oversized, non-ASCII, or path-shaped value.
            pub fn parse(value: &str) -> Result<Self, GitHubWorkflowJobMapperError> {
                validate_identifier($field, value)?;
                Ok(Self(value.to_owned()))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

identifier_type!(GitHubWorkflowJobRouteId, "route.id");

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GitHubDeliveryId(String);

impl GitHubDeliveryId {
    /// Parse one bounded private webhook delivery identifier.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty, oversized, non-ASCII, or path-shaped value.
    pub fn parse(value: &str) -> Result<Self, GitHubWorkflowJobMapperError> {
        validate_identifier("origin.delivery_id", value)?;
        Ok(Self(value.to_owned()))
    }
}

impl fmt::Debug for GitHubDeliveryId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<private-github-delivery-id>")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct GitHubWorkflowJobRouteRevision(u64);

impl GitHubWorkflowJobRouteRevision {
    /// Construct one positive bounded route revision.
    ///
    /// # Errors
    ///
    /// Returns an error for zero or an implementation-exceeding revision.
    pub fn new(value: u64) -> Result<Self, GitHubWorkflowJobMapperError> {
        if !(1..=MAX_GITHUB_ROUTE_REVISION).contains(&value) {
            return Err(GitHubWorkflowJobMapperError::new(
                "route.revision",
                "invalid_route_revision",
                "GitHub workflow-job route revision must be within the bounded positive range",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ReviewedGitHubName(String);

impl ReviewedGitHubName {
    fn parse(field: &'static str, value: &str) -> Result<Self, GitHubWorkflowJobMapperError> {
        let valid = !value.is_empty()
            && value.len() <= MAX_GITHUB_REVIEWED_NAME_LEN
            && value.trim() == value
            && value
                .bytes()
                .all(|byte| byte.is_ascii_graphic() || byte == b' ')
            && !value
                .bytes()
                .any(|byte| matches!(byte, b'\\' | b'\r' | b'\n' | b'\0'));
        if !valid {
            return Err(GitHubWorkflowJobMapperError::new(
                field,
                "invalid_reviewed_name",
                "reviewed GitHub name must be bounded printable ASCII without path aliases or control data",
            ));
        }
        Ok(Self(value.to_owned()))
    }
}

impl fmt::Debug for ReviewedGitHubName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<reviewed-github-name>")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitHubWorkflowJobRouteContract {
    pub route_id: GitHubWorkflowJobRouteId,
    pub revision: GitHubWorkflowJobRouteRevision,
    pub repository: RepositoryRef,
    pub verification_profile_id: VerificationProfileId,
    pub runner_profile_id: RunnerProfileId,
    pub priority: PersonalWorkerPriority,
    pub requested_limits: ExecutionResourceLimits,
    pub cache_id: CacheId,
    pub cache_namespace_digest: Sha256Digest,
    pub cache_access: PersonalWorkerCacheAccessMode,
}

#[derive(Clone, PartialEq, Eq)]
pub struct GitHubWorkflowJobRoute {
    contract: GitHubWorkflowJobRouteContract,
    workflow_name: ReviewedGitHubName,
    job_name: ReviewedGitHubName,
}

impl GitHubWorkflowJobRoute {
    /// Define one exact reviewed repository/workflow/job mapping.
    ///
    /// Workflow and job names remain private route evidence; public output carries only the stable
    /// route ID and typed repository/profile/resource contract.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed names or a route that exceeds the personal-worker envelope.
    pub fn new(
        contract: GitHubWorkflowJobRouteContract,
        workflow_name: &str,
        job_name: &str,
    ) -> Result<Self, GitHubWorkflowJobMapperError> {
        if contract.requested_limits.cpu_millis > PERSONAL_WORKER_SCHEDULABLE_CPU_MILLIS
            || contract.requested_limits.memory_bytes > PERSONAL_WORKER_SCHEDULABLE_MEMORY_BYTES
        {
            return Err(GitHubWorkflowJobMapperError::new(
                "route.requested_limits",
                "route_resource_overcommit",
                "GitHub workflow-job route must preserve the reviewed personal-worker reserve",
            ));
        }
        Ok(Self {
            contract,
            workflow_name: ReviewedGitHubName::parse("route.workflow_name", workflow_name)?,
            job_name: ReviewedGitHubName::parse("route.job_name", job_name)?,
        })
    }

    #[must_use]
    pub const fn contract(&self) -> &GitHubWorkflowJobRouteContract {
        &self.contract
    }
}

impl fmt::Debug for GitHubWorkflowJobRoute {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GitHubWorkflowJobRoute")
            .field("contract", &self.contract)
            .field("workflow_name", &"<reviewed-github-name>")
            .field("job_name", &"<reviewed-github-name>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHubWorkflowJobAction {
    Queued,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHubWorkflowJobConclusion {
    Success,
    Failure,
    Cancelled,
    TimedOut,
    Skipped,
    Neutral,
    ActionRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHubWorkflowJobCompletionOutcome {
    Success,
    Failure,
    TimedOut,
    Neutral,
    ActionRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHubWorkflowJobCancellationReason {
    Cancelled,
    Skipped,
}

#[derive(Clone, PartialEq, Eq)]
pub enum GitHubWorkflowJobEvidenceOrigin {
    VerifiedWebhook { delivery_id: GitHubDeliveryId },
    Reconciliation { snapshot_generation: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHubWorkflowJobEvidenceOriginKind {
    VerifiedWebhook,
    Reconciliation,
}

impl GitHubWorkflowJobEvidenceOrigin {
    fn kind(&self) -> GitHubWorkflowJobEvidenceOriginKind {
        match self {
            Self::VerifiedWebhook { .. } => GitHubWorkflowJobEvidenceOriginKind::VerifiedWebhook,
            Self::Reconciliation { .. } => GitHubWorkflowJobEvidenceOriginKind::Reconciliation,
        }
    }

    fn validate(&self) -> Result<(), GitHubWorkflowJobMapperError> {
        if let Self::Reconciliation {
            snapshot_generation,
        } = self
            && !(1..=MAX_GITHUB_QUEUE_SNAPSHOT_GENERATION).contains(snapshot_generation)
        {
            return Err(GitHubWorkflowJobMapperError::new(
                "origin.snapshot_generation",
                "invalid_snapshot_generation",
                "GitHub queue snapshot generation must be within the bounded positive range",
            ));
        }
        Ok(())
    }
}

impl fmt::Debug for GitHubWorkflowJobEvidenceOrigin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GitHubWorkflowJobEvidenceOrigin")
            .field("kind", &self.kind())
            .field("private_evidence", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct GitHubWorkflowJobEventDefinition {
    pub origin: GitHubWorkflowJobEvidenceOrigin,
    pub workflow_job_id: u64,
    pub run_id: u64,
    pub run_attempt: u32,
    pub action: GitHubWorkflowJobAction,
    pub repository: RepositoryRef,
    pub workflow_name: String,
    pub job_name: String,
    pub head_commit: CommitId,
    pub head_tree: GitTreeId,
    pub created_at: EpochMillis,
    pub started_at: Option<EpochMillis>,
    pub completed_at: Option<EpochMillis>,
    pub conclusion: Option<GitHubWorkflowJobConclusion>,
    pub observed_at: EpochMillis,
}

impl fmt::Debug for GitHubWorkflowJobEventDefinition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GitHubWorkflowJobEventDefinition")
            .field("origin", &self.origin.kind())
            .field("workflow_job_id", &self.workflow_job_id)
            .field("run_id", &self.run_id)
            .field("run_attempt", &self.run_attempt)
            .field("action", &self.action)
            .field("repository", &self.repository)
            .field("workflow_name", &"<reviewed-github-name>")
            .field("job_name", &"<reviewed-github-name>")
            .field("head_commit", &self.head_commit)
            .field("head_tree", &self.head_tree)
            .field("created_at", &self.created_at)
            .field("started_at", &self.started_at)
            .field("completed_at", &self.completed_at)
            .field("conclusion", &self.conclusion)
            .field("observed_at", &self.observed_at)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct GitHubWorkflowJobEvent {
    origin: GitHubWorkflowJobEvidenceOrigin,
    workflow_job_id: u64,
    run_id: u64,
    run_attempt: u32,
    action: GitHubWorkflowJobAction,
    repository: RepositoryRef,
    workflow_name: ReviewedGitHubName,
    job_name: ReviewedGitHubName,
    head_commit: CommitId,
    head_tree: GitTreeId,
    created_at: EpochMillis,
    started_at: Option<EpochMillis>,
    completed_at: Option<EpochMillis>,
    conclusion: Option<GitHubWorkflowJobConclusion>,
    observed_at: EpochMillis,
}

impl GitHubWorkflowJobEvent {
    /// Validate one already-authenticated webhook event or bounded reconciliation observation.
    ///
    /// Signature verification and GitHub queue retrieval remain outside this pure mapper. The
    /// caller must use `VerifiedWebhook` only after exact signature verification.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for invalid identity, time ordering, action evidence, or origin.
    pub fn new(
        definition: GitHubWorkflowJobEventDefinition,
    ) -> Result<Self, GitHubWorkflowJobMapperError> {
        definition.origin.validate()?;
        if definition.workflow_job_id == 0 || definition.run_id == 0 || definition.run_attempt == 0
        {
            return Err(GitHubWorkflowJobMapperError::new(
                "identity",
                "invalid_github_job_identity",
                "GitHub workflow-job, run, and attempt identities must be positive",
            ));
        }
        validate_event_times(&definition)?;
        Ok(Self {
            origin: definition.origin,
            workflow_job_id: definition.workflow_job_id,
            run_id: definition.run_id,
            run_attempt: definition.run_attempt,
            action: definition.action,
            repository: definition.repository,
            workflow_name: ReviewedGitHubName::parse(
                "event.workflow_name",
                &definition.workflow_name,
            )?,
            job_name: ReviewedGitHubName::parse("event.job_name", &definition.job_name)?,
            head_commit: definition.head_commit,
            head_tree: definition.head_tree,
            created_at: definition.created_at,
            started_at: definition.started_at,
            completed_at: definition.completed_at,
            conclusion: definition.conclusion,
            observed_at: definition.observed_at,
        })
    }
}

impl fmt::Debug for GitHubWorkflowJobEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GitHubWorkflowJobEvent")
            .field("origin", &self.origin.kind())
            .field("workflow_job_id", &self.workflow_job_id)
            .field("run_id", &self.run_id)
            .field("run_attempt", &self.run_attempt)
            .field("action", &self.action)
            .field("repository", &self.repository)
            .field("workflow_name", &"<reviewed-github-name>")
            .field("job_name", &"<reviewed-github-name>")
            .field("head_commit", &self.head_commit)
            .field("head_tree", &self.head_tree)
            .field("created_at", &self.created_at)
            .field("started_at", &self.started_at)
            .field("completed_at", &self.completed_at)
            .field("conclusion", &self.conclusion)
            .field("observed_at", &self.observed_at)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitHubWorkflowJobSubmission {
    pub identity: ExecutionAdmissionIdentity,
    pub source: PersonalWorkerSourceIdentity,
    pub priority: PersonalWorkerPriority,
    pub requested_limits: ExecutionResourceLimits,
    pub cache_namespace: PersonalWorkerCacheNamespace,
    pub cache_access: PersonalWorkerCacheAccessMode,
    pub submitted_at: EpochMillis,
    pub fallback_eligibility: FallbackProfileEligibility,
}

impl GitHubWorkflowJobSubmission {
    #[must_use]
    pub fn to_job_request(&self) -> PersonalWorkerJobRequest {
        PersonalWorkerJobRequest {
            identity: self.identity.clone(),
            source: self.source.clone(),
            priority: self.priority,
            requested_limits: self.requested_limits,
            cache_namespace: self.cache_namespace.clone(),
            cache_access: self.cache_access,
            submitted_at: self.submitted_at,
            operator_deadline: None,
            cancellation: PersonalWorkerCancellationState::Active,
            fallback_eligibility: self.fallback_eligibility.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum GitHubWorkflowJobPhase {
    Queued,
    InProgress {
        started_at: EpochMillis,
    },
    Completed {
        completed_at: EpochMillis,
        conclusion: GitHubWorkflowJobConclusion,
    },
}

impl GitHubWorkflowJobPhase {
    const fn rank(&self) -> u8 {
        match self {
            Self::Queued => 0,
            Self::InProgress { .. } => 1,
            Self::Completed { .. } => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitHubWorkflowJobRecord {
    schema_version: u8,
    route: GitHubWorkflowJobRouteContract,
    workflow_job_id: u64,
    run_id: u64,
    run_attempt: u32,
    request_id: ExecutionRequestId,
    source: PersonalWorkerSourceIdentity,
    created_at: EpochMillis,
    phase: GitHubWorkflowJobPhase,
    last_observed_at: EpochMillis,
}

impl GitHubWorkflowJobRecord {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn route(&self) -> &GitHubWorkflowJobRouteContract {
        &self.route
    }

    #[must_use]
    pub const fn request_id(&self) -> &ExecutionRequestId {
        &self.request_id
    }

    #[must_use]
    pub const fn phase(&self) -> &GitHubWorkflowJobPhase {
        &self.phase
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHubWorkflowJobNoOpReason {
    ExactReplay,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "intent", rename_all = "snake_case")]
pub enum GitHubWorkflowJobIntent {
    Submit {
        submission: Box<GitHubWorkflowJobSubmission>,
    },
    MarkRunning {
        request_id: ExecutionRequestId,
        started_at: EpochMillis,
    },
    Complete {
        request_id: ExecutionRequestId,
        completed_at: EpochMillis,
        outcome: GitHubWorkflowJobCompletionOutcome,
    },
    Cancel {
        request_id: ExecutionRequestId,
        cancelled_at: EpochMillis,
        reason: GitHubWorkflowJobCancellationReason,
    },
    Reconcile {
        submission: Box<GitHubWorkflowJobSubmission>,
        observed_phase: GitHubWorkflowJobPhase,
    },
    NoOp {
        reason: GitHubWorkflowJobNoOpReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitHubWorkflowJobMapping {
    schema_version: u8,
    mapped_at: EpochMillis,
    origin: GitHubWorkflowJobEvidenceOriginKind,
    intent: GitHubWorkflowJobIntent,
    resulting_record: GitHubWorkflowJobRecord,
}

impl GitHubWorkflowJobMapping {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn intent(&self) -> &GitHubWorkflowJobIntent {
        &self.intent
    }

    #[must_use]
    pub const fn resulting_record(&self) -> &GitHubWorkflowJobRecord {
        &self.resulting_record
    }
}

pub struct GitHubWorkflowJobMapper {
    schema_version: u8,
    max_evidence_age_millis: u64,
    routes: Vec<GitHubWorkflowJobRoute>,
}

impl GitHubWorkflowJobMapper {
    /// Construct one pure mapper from a closed reviewed route registry.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty or oversized registry, duplicate route identities, ambiguous
    /// repository/workflow/job matches, or an invalid freshness window.
    pub fn new(
        max_evidence_age_millis: u64,
        routes: Vec<GitHubWorkflowJobRoute>,
    ) -> Result<Self, GitHubWorkflowJobMapperError> {
        if !(1..=MAX_GITHUB_WORKFLOW_JOB_EVIDENCE_AGE_MILLIS).contains(&max_evidence_age_millis) {
            return Err(GitHubWorkflowJobMapperError::new(
                "policy.max_evidence_age_millis",
                "invalid_freshness_window",
                "GitHub workflow-job freshness window must be within the reviewed positive range",
            ));
        }
        if routes.is_empty() || routes.len() > MAX_GITHUB_WORKFLOW_JOB_ROUTES {
            return Err(GitHubWorkflowJobMapperError::new(
                "routes",
                "invalid_route_count",
                "GitHub workflow-job route registry must be nonempty and within the reviewed bound",
            ));
        }

        let mut route_ids = BTreeSet::new();
        let mut match_keys = BTreeSet::new();
        for route in &routes {
            if !route_ids.insert(route.contract.route_id.clone()) {
                return Err(GitHubWorkflowJobMapperError::new(
                    "route.id",
                    "duplicate_route_id",
                    "GitHub workflow-job route IDs must be unique",
                ));
            }
            let key = (
                route.contract.repository.clone(),
                route.workflow_name.clone(),
                route.job_name.clone(),
            );
            if !match_keys.insert(key) {
                return Err(GitHubWorkflowJobMapperError::new(
                    "routes",
                    "ambiguous_route_match",
                    "repository, workflow, and job must resolve to exactly one reviewed route",
                ));
            }
        }

        Ok(Self {
            schema_version: GITHUB_WORKFLOW_JOB_MAPPER_SCHEMA_VERSION,
            max_evidence_age_millis,
            routes,
        })
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    /// Map one exact event into one typed durable mutation intent without performing the mutation.
    ///
    /// This method reads no clock, payload, filesystem, network, credential, GitHub API, queue
    /// store, runner state, or Lima state. It grants no admission or reservation.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for stale evidence, unknown routes, identity/source drift,
    /// contradictory replay, or reversing workflow-job transitions.
    pub fn map(
        &self,
        event: &GitHubWorkflowJobEvent,
        previous: Option<&GitHubWorkflowJobRecord>,
        decision_at: EpochMillis,
    ) -> Result<GitHubWorkflowJobMapping, GitHubWorkflowJobMapperError> {
        validate_freshness(event.observed_at, decision_at, self.max_evidence_age_millis)?;
        let route = self.resolve_route(event)?;
        let request_id = request_id(event)?;
        let submission = submission(route, event, request_id.clone());
        let next_record = record(route, event, request_id.clone())?;

        let (intent, resulting_record) = match previous {
            None => {
                let intent = match &next_record.phase {
                    GitHubWorkflowJobPhase::Queued => GitHubWorkflowJobIntent::Submit {
                        submission: Box::new(submission),
                    },
                    phase => GitHubWorkflowJobIntent::Reconcile {
                        submission: Box::new(submission),
                        observed_phase: phase.clone(),
                    },
                };
                (intent, next_record)
            }
            Some(previous) => {
                validate_previous(previous, route, event, &request_id)?;
                if same_semantic_event(previous, &next_record) {
                    (
                        GitHubWorkflowJobIntent::NoOp {
                            reason: GitHubWorkflowJobNoOpReason::ExactReplay,
                        },
                        previous.clone(),
                    )
                } else {
                    validate_transition(previous, &next_record)?;
                    let intent = transition_intent(&next_record)?;
                    (intent, next_record)
                }
            }
        };

        Ok(GitHubWorkflowJobMapping {
            schema_version: GITHUB_WORKFLOW_JOB_MAPPER_SCHEMA_VERSION,
            mapped_at: decision_at,
            origin: event.origin.kind(),
            intent,
            resulting_record,
        })
    }

    fn resolve_route<'a>(
        &'a self,
        event: &GitHubWorkflowJobEvent,
    ) -> Result<&'a GitHubWorkflowJobRoute, GitHubWorkflowJobMapperError> {
        self.routes
            .iter()
            .find(|route| {
                route.contract.repository == event.repository
                    && route.workflow_name == event.workflow_name
                    && route.job_name == event.job_name
            })
            .ok_or_else(|| {
                GitHubWorkflowJobMapperError::new(
                    "route",
                    "unreviewed_workflow_job",
                    "GitHub workflow-job event does not match a reviewed route",
                )
            })
    }
}

impl fmt::Debug for GitHubWorkflowJobMapper {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GitHubWorkflowJobMapper")
            .field("schema_version", &self.schema_version)
            .field("max_evidence_age_millis", &self.max_evidence_age_millis)
            .field("route_count", &self.routes.len())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct GitHubWorkflowJobMapperError {
    pub field: &'static str,
    pub code: &'static str,
    pub message: &'static str,
}

impl GitHubWorkflowJobMapperError {
    const fn new(field: &'static str, code: &'static str, message: &'static str) -> Self {
        Self {
            field,
            code,
            message,
        }
    }
}

impl fmt::Display for GitHubWorkflowJobMapperError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for GitHubWorkflowJobMapperError {}

fn validate_identifier(
    field: &'static str,
    value: &str,
) -> Result<(), GitHubWorkflowJobMapperError> {
    let mut bytes = value.bytes();
    let first_is_safe = bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    let remaining_are_safe = bytes.all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
    });
    if value.len() > MAX_GITHUB_IDENTIFIER_LEN || !first_is_safe || !remaining_are_safe {
        return Err(GitHubWorkflowJobMapperError::new(
            field,
            "invalid_identifier",
            "identifier must be bounded lowercase ASCII without path or log content",
        ));
    }
    Ok(())
}

fn validate_event_times(
    definition: &GitHubWorkflowJobEventDefinition,
) -> Result<(), GitHubWorkflowJobMapperError> {
    if definition.created_at > definition.observed_at {
        return Err(GitHubWorkflowJobMapperError::new(
            "created_at",
            "future_created_at",
            "GitHub workflow job cannot be created after its observation",
        ));
    }
    if definition.started_at.is_some_and(|started_at| {
        started_at < definition.created_at || started_at > definition.observed_at
    }) {
        return Err(GitHubWorkflowJobMapperError::new(
            "started_at",
            "invalid_started_at",
            "GitHub workflow-job start time must be between creation and observation",
        ));
    }
    if definition.completed_at.is_some_and(|completed_at| {
        completed_at < definition.created_at
            || completed_at > definition.observed_at
            || definition
                .started_at
                .is_some_and(|started_at| completed_at < started_at)
    }) {
        return Err(GitHubWorkflowJobMapperError::new(
            "completed_at",
            "invalid_completed_at",
            "GitHub workflow-job completion time must follow creation and any start time",
        ));
    }

    let shape_is_valid = match definition.action {
        GitHubWorkflowJobAction::Queued => {
            definition.started_at.is_none()
                && definition.completed_at.is_none()
                && definition.conclusion.is_none()
        }
        GitHubWorkflowJobAction::InProgress => {
            definition.started_at.is_some()
                && definition.completed_at.is_none()
                && definition.conclusion.is_none()
        }
        GitHubWorkflowJobAction::Completed => {
            definition.completed_at.is_some()
                && definition.conclusion.is_some()
                && (definition.started_at.is_some()
                    || matches!(
                        definition.conclusion,
                        Some(
                            GitHubWorkflowJobConclusion::Cancelled
                                | GitHubWorkflowJobConclusion::Skipped
                        )
                    ))
        }
    };
    if !shape_is_valid {
        return Err(GitHubWorkflowJobMapperError::new(
            "action",
            "invalid_action_evidence",
            "GitHub workflow-job action requires its exact reviewed timestamp and conclusion shape",
        ));
    }
    Ok(())
}

fn validate_freshness(
    observed_at: EpochMillis,
    decision_at: EpochMillis,
    max_age_millis: u64,
) -> Result<(), GitHubWorkflowJobMapperError> {
    let age = decision_at
        .get()
        .checked_sub(observed_at.get())
        .ok_or_else(|| {
            GitHubWorkflowJobMapperError::new(
                "observed_at",
                "future_event_observation",
                "GitHub workflow-job observation cannot be newer than the mapping decision",
            )
        })?;
    if age > max_age_millis {
        return Err(GitHubWorkflowJobMapperError::new(
            "observed_at",
            "stale_event_observation",
            "GitHub workflow-job observation is older than the reviewed freshness window",
        ));
    }
    Ok(())
}

fn request_id(
    event: &GitHubWorkflowJobEvent,
) -> Result<ExecutionRequestId, GitHubWorkflowJobMapperError> {
    ExecutionRequestId::parse(&format!(
        "gh-job-{}-{}-{}",
        event.workflow_job_id, event.run_id, event.run_attempt
    ))
    .map_err(|_| {
        GitHubWorkflowJobMapperError::new(
            "identity.request_id",
            "invalid_derived_request_id",
            "GitHub workflow-job identity cannot produce a bounded execution request ID",
        )
    })
}

fn submission(
    route: &GitHubWorkflowJobRoute,
    event: &GitHubWorkflowJobEvent,
    request_id: ExecutionRequestId,
) -> GitHubWorkflowJobSubmission {
    GitHubWorkflowJobSubmission {
        identity: ExecutionAdmissionIdentity::new(
            request_id,
            route.contract.verification_profile_id.clone(),
            route.contract.runner_profile_id.clone(),
        ),
        source: PersonalWorkerSourceIdentity::new(
            event.repository.clone(),
            event.head_commit.clone(),
            event.head_tree.clone(),
        ),
        priority: route.contract.priority,
        requested_limits: route.contract.requested_limits,
        cache_namespace: PersonalWorkerCacheNamespace::RepositoryBuild {
            cache_id: route.contract.cache_id.clone(),
            repository: event.repository.clone(),
            namespace_digest: route.contract.cache_namespace_digest.clone(),
        },
        cache_access: route.contract.cache_access,
        submitted_at: event.created_at,
        fallback_eligibility: FallbackProfileEligibility::ineligible(),
    }
}

fn phase(
    event: &GitHubWorkflowJobEvent,
) -> Result<GitHubWorkflowJobPhase, GitHubWorkflowJobMapperError> {
    match event.action {
        GitHubWorkflowJobAction::Queued => Ok(GitHubWorkflowJobPhase::Queued),
        GitHubWorkflowJobAction::InProgress => Ok(GitHubWorkflowJobPhase::InProgress {
            started_at: event.started_at.ok_or_else(|| {
                GitHubWorkflowJobMapperError::new(
                    "started_at",
                    "missing_started_at",
                    "in-progress GitHub workflow job requires an exact start time",
                )
            })?,
        }),
        GitHubWorkflowJobAction::Completed => Ok(GitHubWorkflowJobPhase::Completed {
            completed_at: event.completed_at.ok_or_else(|| {
                GitHubWorkflowJobMapperError::new(
                    "completed_at",
                    "missing_completed_at",
                    "completed GitHub workflow job requires an exact completion time",
                )
            })?,
            conclusion: event.conclusion.ok_or_else(|| {
                GitHubWorkflowJobMapperError::new(
                    "conclusion",
                    "missing_conclusion",
                    "completed GitHub workflow job requires a reviewed conclusion",
                )
            })?,
        }),
    }
}

fn record(
    route: &GitHubWorkflowJobRoute,
    event: &GitHubWorkflowJobEvent,
    request_id: ExecutionRequestId,
) -> Result<GitHubWorkflowJobRecord, GitHubWorkflowJobMapperError> {
    Ok(GitHubWorkflowJobRecord {
        schema_version: GITHUB_WORKFLOW_JOB_MAPPER_SCHEMA_VERSION,
        route: route.contract.clone(),
        workflow_job_id: event.workflow_job_id,
        run_id: event.run_id,
        run_attempt: event.run_attempt,
        request_id,
        source: PersonalWorkerSourceIdentity::new(
            event.repository.clone(),
            event.head_commit.clone(),
            event.head_tree.clone(),
        ),
        created_at: event.created_at,
        phase: phase(event)?,
        last_observed_at: event.observed_at,
    })
}

fn validate_previous(
    previous: &GitHubWorkflowJobRecord,
    route: &GitHubWorkflowJobRoute,
    event: &GitHubWorkflowJobEvent,
    request_id: &ExecutionRequestId,
) -> Result<(), GitHubWorkflowJobMapperError> {
    if previous.schema_version != GITHUB_WORKFLOW_JOB_MAPPER_SCHEMA_VERSION {
        return Err(GitHubWorkflowJobMapperError::new(
            "previous.schema_version",
            "unsupported_previous_schema",
            "previous GitHub workflow-job record schema is not supported",
        ));
    }
    if previous.route != route.contract {
        return Err(GitHubWorkflowJobMapperError::new(
            "previous.route",
            "route_contract_drift",
            "GitHub workflow-job route contract must remain exact across one job",
        ));
    }
    if previous.workflow_job_id != event.workflow_job_id
        || previous.run_id != event.run_id
        || previous.run_attempt != event.run_attempt
        || previous.request_id != *request_id
    {
        return Err(GitHubWorkflowJobMapperError::new(
            "identity",
            "workflow_job_identity_drift",
            "GitHub workflow-job identity must remain exact across mapped events",
        ));
    }
    let source = PersonalWorkerSourceIdentity::new(
        event.repository.clone(),
        event.head_commit.clone(),
        event.head_tree.clone(),
    );
    if previous.source != source {
        return Err(GitHubWorkflowJobMapperError::new(
            "source",
            "immutable_source_drift",
            "GitHub workflow-job immutable source identity must remain exact",
        ));
    }
    if previous.created_at != event.created_at {
        return Err(GitHubWorkflowJobMapperError::new(
            "created_at",
            "creation_time_drift",
            "GitHub workflow-job creation time must remain exact",
        ));
    }
    if event.observed_at < previous.last_observed_at {
        return Err(GitHubWorkflowJobMapperError::new(
            "observed_at",
            "event_observation_reversal",
            "GitHub workflow-job observation time cannot move backwards",
        ));
    }
    Ok(())
}

fn same_semantic_event(previous: &GitHubWorkflowJobRecord, next: &GitHubWorkflowJobRecord) -> bool {
    previous.route == next.route
        && previous.workflow_job_id == next.workflow_job_id
        && previous.run_id == next.run_id
        && previous.run_attempt == next.run_attempt
        && previous.request_id == next.request_id
        && previous.source == next.source
        && previous.created_at == next.created_at
        && previous.phase == next.phase
}

fn validate_transition(
    previous: &GitHubWorkflowJobRecord,
    next: &GitHubWorkflowJobRecord,
) -> Result<(), GitHubWorkflowJobMapperError> {
    if next.phase.rank() < previous.phase.rank() {
        return Err(GitHubWorkflowJobMapperError::new(
            "phase",
            "workflow_job_phase_reversal",
            "GitHub workflow-job phase cannot move backwards",
        ));
    }
    if next.phase.rank() == previous.phase.rank() {
        return Err(GitHubWorkflowJobMapperError::new(
            "phase",
            "contradictory_phase_replay",
            "same GitHub workflow-job phase cannot change its retained evidence",
        ));
    }
    if matches!(previous.phase, GitHubWorkflowJobPhase::Completed { .. }) {
        return Err(GitHubWorkflowJobMapperError::new(
            "phase",
            "terminal_phase_reversal",
            "completed GitHub workflow job cannot transition again",
        ));
    }
    Ok(())
}

fn transition_intent(
    next: &GitHubWorkflowJobRecord,
) -> Result<GitHubWorkflowJobIntent, GitHubWorkflowJobMapperError> {
    match next.phase {
        GitHubWorkflowJobPhase::Queued => Err(GitHubWorkflowJobMapperError::new(
            "phase",
            "invalid_queued_transition",
            "queued GitHub workflow job cannot follow an existing mapped phase",
        )),
        GitHubWorkflowJobPhase::InProgress { started_at } => {
            Ok(GitHubWorkflowJobIntent::MarkRunning {
                request_id: next.request_id.clone(),
                started_at,
            })
        }
        GitHubWorkflowJobPhase::Completed {
            completed_at,
            conclusion,
        } => match conclusion {
            GitHubWorkflowJobConclusion::Cancelled => Ok(GitHubWorkflowJobIntent::Cancel {
                request_id: next.request_id.clone(),
                cancelled_at: completed_at,
                reason: GitHubWorkflowJobCancellationReason::Cancelled,
            }),
            GitHubWorkflowJobConclusion::Skipped => Ok(GitHubWorkflowJobIntent::Cancel {
                request_id: next.request_id.clone(),
                cancelled_at: completed_at,
                reason: GitHubWorkflowJobCancellationReason::Skipped,
            }),
            GitHubWorkflowJobConclusion::Success => Ok(GitHubWorkflowJobIntent::Complete {
                request_id: next.request_id.clone(),
                completed_at,
                outcome: GitHubWorkflowJobCompletionOutcome::Success,
            }),
            GitHubWorkflowJobConclusion::Failure => Ok(GitHubWorkflowJobIntent::Complete {
                request_id: next.request_id.clone(),
                completed_at,
                outcome: GitHubWorkflowJobCompletionOutcome::Failure,
            }),
            GitHubWorkflowJobConclusion::TimedOut => Ok(GitHubWorkflowJobIntent::Complete {
                request_id: next.request_id.clone(),
                completed_at,
                outcome: GitHubWorkflowJobCompletionOutcome::TimedOut,
            }),
            GitHubWorkflowJobConclusion::Neutral => Ok(GitHubWorkflowJobIntent::Complete {
                request_id: next.request_id.clone(),
                completed_at,
                outcome: GitHubWorkflowJobCompletionOutcome::Neutral,
            }),
            GitHubWorkflowJobConclusion::ActionRequired => Ok(GitHubWorkflowJobIntent::Complete {
                request_id: next.request_id.clone(),
                completed_at,
                outcome: GitHubWorkflowJobCompletionOutcome::ActionRequired,
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1_024 * 1_024 * 1_024;
    const PRIVATE_WORKFLOW: &str = "Private Workflow / token-bearing-name";
    const PRIVATE_JOB: &str = "private job (secret-looking)";
    const PRIVATE_DELIVERY: &str = "delivery-private-123";

    fn time(value: u64) -> EpochMillis {
        EpochMillis::new(value).expect("time")
    }

    fn repository() -> RepositoryRef {
        RepositoryRef::parse("teamleaderleo/smolrunner").expect("repository")
    }

    fn route_with_names(workflow: &str, job: &str) -> GitHubWorkflowJobRoute {
        GitHubWorkflowJobRoute::new(
            GitHubWorkflowJobRouteContract {
                route_id: GitHubWorkflowJobRouteId::parse("smolrunner-verify").expect("route ID"),
                revision: GitHubWorkflowJobRouteRevision::new(1).expect("revision"),
                repository: repository(),
                verification_profile_id: VerificationProfileId::parse("smolrunner.required")
                    .expect("verification profile"),
                runner_profile_id: RunnerProfileId::parse("personal-lima-work")
                    .expect("runner profile"),
                priority: PersonalWorkerPriority::Normal,
                requested_limits: ExecutionResourceLimits::new(3_000, 3 * GIB, 2_048)
                    .expect("limits"),
                cache_id: CacheId::parse("cargo-target").expect("cache ID"),
                cache_namespace_digest: Sha256Digest::parse(&format!("sha256:{}", "ab".repeat(32)))
                    .expect("digest"),
                cache_access: PersonalWorkerCacheAccessMode::Write,
            },
            workflow,
            job,
        )
        .expect("route")
    }

    fn mapper_with_names(workflow: &str, job: &str) -> GitHubWorkflowJobMapper {
        GitHubWorkflowJobMapper::new(30_000, vec![route_with_names(workflow, job)]).expect("mapper")
    }

    fn definition(
        action: GitHubWorkflowJobAction,
        observed_at: u64,
    ) -> GitHubWorkflowJobEventDefinition {
        let (started_at, completed_at, conclusion) = match action {
            GitHubWorkflowJobAction::Queued => (None, None, None),
            GitHubWorkflowJobAction::InProgress => (Some(time(1_100)), None, None),
            GitHubWorkflowJobAction::Completed => (
                Some(time(1_100)),
                Some(time(1_200)),
                Some(GitHubWorkflowJobConclusion::Success),
            ),
        };
        GitHubWorkflowJobEventDefinition {
            origin: GitHubWorkflowJobEvidenceOrigin::VerifiedWebhook {
                delivery_id: GitHubDeliveryId::parse("delivery-123").expect("delivery"),
            },
            workflow_job_id: 42,
            run_id: 84,
            run_attempt: 1,
            action,
            repository: repository(),
            workflow_name: "Verify".to_owned(),
            job_name: "verify".to_owned(),
            head_commit: CommitId::parse(&"a".repeat(40)).expect("commit"),
            head_tree: GitTreeId::parse(&"b".repeat(40)).expect("tree"),
            created_at: time(1_000),
            started_at,
            completed_at,
            conclusion,
            observed_at: time(observed_at),
        }
    }

    fn event(action: GitHubWorkflowJobAction, observed_at: u64) -> GitHubWorkflowJobEvent {
        GitHubWorkflowJobEvent::new(definition(action, observed_at)).expect("event")
    }

    #[test]
    fn reviewed_route_maps_queued_event_to_typed_submission() {
        let mapper = mapper_with_names("Verify", "verify");
        let mapping = mapper
            .map(
                &event(GitHubWorkflowJobAction::Queued, 1_050),
                None,
                time(1_050),
            )
            .expect("mapping");

        let GitHubWorkflowJobIntent::Submit { submission } = mapping.intent() else {
            panic!("expected submit");
        };
        assert_eq!(submission.identity.request_id.as_str(), "gh-job-42-84-1");
        assert_eq!(
            submission.identity.verification_profile_id.as_str(),
            "smolrunner.required"
        );
        assert_eq!(submission.source.repository, repository());
        assert_eq!(submission.requested_limits.cpu_millis, 3_000);
        assert_eq!(submission.to_job_request().operator_deadline, None);
    }

    #[test]
    fn queued_running_and_completed_events_form_monotonic_intents() {
        let mapper = mapper_with_names("Verify", "verify");
        let queued = mapper
            .map(
                &event(GitHubWorkflowJobAction::Queued, 1_050),
                None,
                time(1_050),
            )
            .expect("queued");
        let running = mapper
            .map(
                &event(GitHubWorkflowJobAction::InProgress, 1_100),
                Some(queued.resulting_record()),
                time(1_100),
            )
            .expect("running");
        assert!(matches!(
            running.intent(),
            GitHubWorkflowJobIntent::MarkRunning { .. }
        ));

        let completed = mapper
            .map(
                &event(GitHubWorkflowJobAction::Completed, 1_200),
                Some(running.resulting_record()),
                time(1_200),
            )
            .expect("completed");
        assert!(matches!(
            completed.intent(),
            GitHubWorkflowJobIntent::Complete {
                outcome: GitHubWorkflowJobCompletionOutcome::Success,
                ..
            }
        ));
    }

    #[test]
    fn exact_duplicate_delivery_is_a_no_op() {
        let mapper = mapper_with_names("Verify", "verify");
        let first = mapper
            .map(
                &event(GitHubWorkflowJobAction::Queued, 1_050),
                None,
                time(1_050),
            )
            .expect("first");
        let mut duplicate = definition(GitHubWorkflowJobAction::Queued, 1_060);
        duplicate.origin = GitHubWorkflowJobEvidenceOrigin::VerifiedWebhook {
            delivery_id: GitHubDeliveryId::parse("delivery-456").expect("delivery"),
        };
        let duplicate = GitHubWorkflowJobEvent::new(duplicate).expect("duplicate event");
        let replay = mapper
            .map(&duplicate, Some(first.resulting_record()), time(1_060))
            .expect("replay");
        assert!(matches!(
            replay.intent(),
            GitHubWorkflowJobIntent::NoOp {
                reason: GitHubWorkflowJobNoOpReason::ExactReplay
            }
        ));
        assert_eq!(replay.resulting_record(), first.resulting_record());
    }

    #[test]
    fn missing_webhook_history_becomes_one_reconciliation_intent() {
        let mapper = mapper_with_names("Verify", "verify");
        let mut definition = definition(GitHubWorkflowJobAction::InProgress, 1_100);
        definition.origin = GitHubWorkflowJobEvidenceOrigin::Reconciliation {
            snapshot_generation: 7,
        };
        let event = GitHubWorkflowJobEvent::new(definition).expect("reconciled event");
        let mapping = mapper.map(&event, None, time(1_100)).expect("mapping");
        assert!(matches!(
            mapping.intent(),
            GitHubWorkflowJobIntent::Reconcile {
                observed_phase: GitHubWorkflowJobPhase::InProgress { .. },
                ..
            }
        ));
    }

    #[test]
    fn cancellation_and_skipped_completion_map_to_cancel_intents() {
        let mapper = mapper_with_names("Verify", "verify");
        let queued = mapper
            .map(
                &event(GitHubWorkflowJobAction::Queued, 1_050),
                None,
                time(1_050),
            )
            .expect("queued");
        for (conclusion, reason) in [
            (
                GitHubWorkflowJobConclusion::Cancelled,
                GitHubWorkflowJobCancellationReason::Cancelled,
            ),
            (
                GitHubWorkflowJobConclusion::Skipped,
                GitHubWorkflowJobCancellationReason::Skipped,
            ),
        ] {
            let mut definition = definition(GitHubWorkflowJobAction::Completed, 1_200);
            definition.conclusion = Some(conclusion);
            if conclusion == GitHubWorkflowJobConclusion::Skipped {
                definition.started_at = None;
            }
            let completed = GitHubWorkflowJobEvent::new(definition).expect("completed event");
            let mapping = mapper
                .map(&completed, Some(queued.resulting_record()), time(1_200))
                .expect("mapping");
            assert!(matches!(
                mapping.intent(),
                GitHubWorkflowJobIntent::Cancel {
                    reason: actual,
                    ..
                } if *actual == reason
            ));
        }
    }

    #[test]
    fn route_registry_rejects_duplicate_or_ambiguous_routes() {
        let route = route_with_names("Verify", "verify");
        assert_eq!(
            GitHubWorkflowJobMapper::new(30_000, vec![route.clone(), route])
                .expect_err("duplicate")
                .code,
            "duplicate_route_id"
        );

        let first = route_with_names("Verify", "verify");
        let mut second = route_with_names("Verify", "verify");
        second.contract.route_id =
            GitHubWorkflowJobRouteId::parse("smolrunner-verify-two").expect("route ID");
        assert_eq!(
            GitHubWorkflowJobMapper::new(30_000, vec![first, second])
                .expect_err("ambiguous")
                .code,
            "ambiguous_route_match"
        );
    }

    #[test]
    fn unreviewed_route_source_drift_and_phase_reversal_fail_closed() {
        let mapper = mapper_with_names("Verify", "verify");
        let mut unreviewed = definition(GitHubWorkflowJobAction::Queued, 1_050);
        unreviewed.workflow_name = "Other".to_owned();
        let unreviewed = GitHubWorkflowJobEvent::new(unreviewed).expect("event");
        assert_eq!(
            mapper
                .map(&unreviewed, None, time(1_050))
                .expect_err("unreviewed")
                .code,
            "unreviewed_workflow_job"
        );

        let running = mapper
            .map(
                &event(GitHubWorkflowJobAction::InProgress, 1_100),
                None,
                time(1_100),
            )
            .expect("reconciled running");
        let queued = event(GitHubWorkflowJobAction::Queued, 1_150);
        assert_eq!(
            mapper
                .map(&queued, Some(running.resulting_record()), time(1_150))
                .expect_err("reversal")
                .code,
            "workflow_job_phase_reversal"
        );

        let mut drift = definition(GitHubWorkflowJobAction::Completed, 1_200);
        drift.head_commit = CommitId::parse(&"c".repeat(40)).expect("commit");
        let drift = GitHubWorkflowJobEvent::new(drift).expect("drift event");
        assert_eq!(
            mapper
                .map(&drift, Some(running.resulting_record()), time(1_200))
                .expect_err("source drift")
                .code,
            "immutable_source_drift"
        );
    }

    #[test]
    fn stale_future_and_malformed_evidence_is_rejected() {
        let mapper = mapper_with_names("Verify", "verify");
        assert_eq!(
            mapper
                .map(
                    &event(GitHubWorkflowJobAction::Queued, 1_050),
                    None,
                    time(40_000),
                )
                .expect_err("stale")
                .code,
            "stale_event_observation"
        );

        let malformed = GitHubWorkflowJobEvent::new(GitHubWorkflowJobEventDefinition {
            started_at: Some(time(1_010)),
            ..definition(GitHubWorkflowJobAction::Queued, 1_050)
        })
        .expect_err("malformed");
        assert_eq!(malformed.code, "invalid_action_evidence");

        assert_eq!(
            GitHubWorkflowJobEvent::new(GitHubWorkflowJobEventDefinition {
                origin: GitHubWorkflowJobEvidenceOrigin::Reconciliation {
                    snapshot_generation: 0,
                },
                ..definition(GitHubWorkflowJobAction::Queued, 1_050)
            })
            .expect_err("generation")
            .code,
            "invalid_snapshot_generation"
        );
    }

    #[test]
    fn public_output_omits_raw_route_and_delivery_evidence() {
        let mapper = mapper_with_names(PRIVATE_WORKFLOW, PRIVATE_JOB);
        let mut definition = definition(GitHubWorkflowJobAction::Queued, 1_050);
        definition.workflow_name = PRIVATE_WORKFLOW.to_owned();
        definition.job_name = PRIVATE_JOB.to_owned();
        definition.origin = GitHubWorkflowJobEvidenceOrigin::VerifiedWebhook {
            delivery_id: GitHubDeliveryId::parse(PRIVATE_DELIVERY).expect("delivery"),
        };
        let definition_debug = format!("{definition:?}");
        let origin_debug = format!("{:?}", definition.origin);
        let delivery_debug = format!(
            "{:?}",
            GitHubDeliveryId::parse(PRIVATE_DELIVERY).expect("delivery")
        );
        let event = GitHubWorkflowJobEvent::new(definition).expect("event");
        let mapping = mapper.map(&event, None, time(1_050)).expect("mapping");
        let json = serde_json::to_string(&mapping).expect("JSON");
        let debug = format!("{event:?} {mapper:?}");

        for output in [json, debug, definition_debug, origin_debug, delivery_debug] {
            assert!(!output.contains(PRIVATE_WORKFLOW));
            assert!(!output.contains(PRIVATE_JOB));
            assert!(!output.contains(PRIVATE_DELIVERY));
            assert!(!output.contains("credential"));
            assert!(!output.contains("authorization"));
            assert!(!output.contains("stdout"));
            assert!(!output.contains("stderr"));
            assert!(!output.contains("/Users/"));
        }
    }
}
