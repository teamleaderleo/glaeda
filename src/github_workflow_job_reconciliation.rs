use std::collections::BTreeSet;
use std::fmt;

use serde::Serialize;

use crate::artifact::{CommitId, GitTreeId, RepositoryRef};
use crate::execution_admission::EpochMillis;
use crate::github_workflow_job_mapper::{
    GitHubWorkflowJobAction, GitHubWorkflowJobConclusion, GitHubWorkflowJobEvent,
    GitHubWorkflowJobEventDefinition, GitHubWorkflowJobEvidenceOrigin,
};

pub const GITHUB_WORKFLOW_JOB_RECONCILIATION_SCHEMA_VERSION: u8 = 1;
pub const MAX_GITHUB_RECONCILIATION_PAGES: usize = 16;
pub const MAX_GITHUB_RECONCILIATION_JOBS_PER_PAGE: usize = 100;
pub const MAX_GITHUB_RECONCILIATION_JOBS: usize = 512;
pub const MAX_GITHUB_RECONCILIATION_SNAPSHOT_AGE_MILLIS: u64 = 300_000;
pub const MAX_GITHUB_RECONCILIATION_QUERY_DURATION_MILLIS: u64 = 60_000;

const MAX_GITHUB_RECONCILIATION_GENERATION: u64 = 1_000_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct GitHubWorkflowJobReconciliationPolicy {
    schema_version: u8,
    max_snapshot_age_millis: u64,
    max_query_duration_millis: u64,
}

impl GitHubWorkflowJobReconciliationPolicy {
    /// Define the bounded freshness and query-duration contract for complete reconciliation snapshots.
    ///
    /// # Errors
    ///
    /// Returns an error when either bound is zero or exceeds the reviewed maximum.
    pub fn new(
        max_snapshot_age_millis: u64,
        max_query_duration_millis: u64,
    ) -> Result<Self, GitHubWorkflowJobReconciliationError> {
        if !(1..=MAX_GITHUB_RECONCILIATION_SNAPSHOT_AGE_MILLIS).contains(&max_snapshot_age_millis) {
            return Err(GitHubWorkflowJobReconciliationError::new(
                "policy.max_snapshot_age_millis",
                "invalid_snapshot_age",
                "GitHub reconciliation snapshot age must be within the reviewed positive range",
            ));
        }
        if !(1..=MAX_GITHUB_RECONCILIATION_QUERY_DURATION_MILLIS)
            .contains(&max_query_duration_millis)
        {
            return Err(GitHubWorkflowJobReconciliationError::new(
                "policy.max_query_duration_millis",
                "invalid_query_duration",
                "GitHub reconciliation query duration must be within the reviewed positive range",
            ));
        }
        Ok(Self {
            schema_version: GITHUB_WORKFLOW_JOB_RECONCILIATION_SCHEMA_VERSION,
            max_snapshot_age_millis,
            max_query_duration_millis,
        })
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    /// Normalize one already-authenticated complete paginated query result into mapper events.
    ///
    /// The caller owns GitHub authentication, credential use, HTTP, pagination retrieval, retries,
    /// and clock sampling. This function performs no I/O and returns no event until the complete
    /// snapshot has passed all timing, pagination, count, identity, and mapper-shape checks.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for stale or future evidence, incomplete or contradictory
    /// pagination, count drift, duplicate job identity, or malformed workflow-job evidence.
    pub fn normalize(
        &self,
        snapshot: GitHubWorkflowJobReconciliationSnapshotDefinition,
        decision_at: EpochMillis,
    ) -> Result<GitHubWorkflowJobReconciliationBatch, GitHubWorkflowJobReconciliationError> {
        validate_snapshot_timing(self, &snapshot, decision_at)?;
        validate_generation(snapshot.snapshot_generation)?;
        validate_page_count(snapshot.pages.len())?;

        let page_count = bounded_u16(snapshot.pages.len())?;
        let mut seen = BTreeSet::new();
        let mut jobs = Vec::new();

        for (index, page) in snapshot.pages.into_iter().enumerate() {
            let expected_page = bounded_u16(index + 1)?;
            if page.page_number != expected_page {
                return Err(GitHubWorkflowJobReconciliationError::new(
                    "pages.page_number",
                    "page_number_drift",
                    "GitHub reconciliation pages must be consecutive and one-indexed",
                ));
            }
            if page.jobs.len() > MAX_GITHUB_RECONCILIATION_JOBS_PER_PAGE {
                return Err(GitHubWorkflowJobReconciliationError::new(
                    "pages.jobs",
                    "page_job_limit_exceeded",
                    "GitHub reconciliation page exceeds the reviewed job bound",
                ));
            }
            let is_final = index + 1 == usize::from(page_count);
            if page.has_next_page == is_final {
                return Err(GitHubWorkflowJobReconciliationError::new(
                    "pages.has_next_page",
                    "incomplete_pagination",
                    "GitHub reconciliation pagination does not prove one complete final page",
                ));
            }

            for job in page.jobs {
                let identity = GitHubWorkflowJobReconciliationEventIdentity {
                    workflow_job_id: job.workflow_job_id,
                    run_id: job.run_id,
                    run_attempt: job.run_attempt,
                };
                if !seen.insert(identity) {
                    return Err(GitHubWorkflowJobReconciliationError::new(
                        "jobs.identity",
                        "duplicate_job_identity",
                        "GitHub reconciliation snapshot contains duplicate workflow-job identity",
                    ));
                }
                if jobs.len() >= MAX_GITHUB_RECONCILIATION_JOBS {
                    return Err(GitHubWorkflowJobReconciliationError::new(
                        "jobs",
                        "snapshot_job_limit_exceeded",
                        "GitHub reconciliation snapshot exceeds the reviewed job bound",
                    ));
                }
                let event = GitHubWorkflowJobEvent::new(GitHubWorkflowJobEventDefinition {
                    origin: GitHubWorkflowJobEvidenceOrigin::Reconciliation {
                        snapshot_generation: snapshot.snapshot_generation,
                    },
                    workflow_job_id: job.workflow_job_id,
                    run_id: job.run_id,
                    run_attempt: job.run_attempt,
                    action: job.action,
                    repository: snapshot.repository.clone(),
                    workflow_name: job.workflow_name,
                    job_name: job.job_name,
                    head_commit: job.head_commit,
                    head_tree: job.head_tree,
                    created_at: job.created_at,
                    started_at: job.started_at,
                    completed_at: job.completed_at,
                    conclusion: job.conclusion,
                    observed_at: snapshot.observed_at,
                })
                .map_err(|_| {
                    GitHubWorkflowJobReconciliationError::new(
                        "jobs.evidence",
                        "invalid_job_evidence",
                        "GitHub reconciliation job cannot produce reviewed mapper evidence",
                    )
                })?;
                jobs.push(GitHubWorkflowJobReconciledEvent { identity, event });
            }
        }

        if jobs.len()
            != usize::try_from(snapshot.reported_total_jobs).map_err(|_| {
                GitHubWorkflowJobReconciliationError::new(
                    "reported_total_jobs",
                    "invalid_reported_total",
                    "GitHub reconciliation reported total cannot be represented safely",
                )
            })?
        {
            return Err(GitHubWorkflowJobReconciliationError::new(
                "reported_total_jobs",
                "reported_total_drift",
                "GitHub reconciliation reported total does not match the complete page set",
            ));
        }

        jobs.sort_by_key(|job| job.identity);
        let job_count = bounded_u32(jobs.len())?;
        Ok(GitHubWorkflowJobReconciliationBatch {
            receipt: GitHubWorkflowJobReconciliationReceipt {
                schema_version: GITHUB_WORKFLOW_JOB_RECONCILIATION_SCHEMA_VERSION,
                snapshot_generation: snapshot.snapshot_generation,
                repository: snapshot.repository,
                query_started_at: snapshot.query_started_at,
                observed_at: snapshot.observed_at,
                normalized_at: decision_at,
                page_count,
                job_count,
            },
            jobs,
        })
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct GitHubWorkflowJobReconciliationJobDefinition {
    pub workflow_job_id: u64,
    pub run_id: u64,
    pub run_attempt: u32,
    pub action: GitHubWorkflowJobAction,
    pub workflow_name: String,
    pub job_name: String,
    pub head_commit: CommitId,
    pub head_tree: GitTreeId,
    pub created_at: EpochMillis,
    pub started_at: Option<EpochMillis>,
    pub completed_at: Option<EpochMillis>,
    pub conclusion: Option<GitHubWorkflowJobConclusion>,
}

impl fmt::Debug for GitHubWorkflowJobReconciliationJobDefinition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GitHubWorkflowJobReconciliationJobDefinition")
            .field("workflow_job_id", &self.workflow_job_id)
            .field("run_id", &self.run_id)
            .field("run_attempt", &self.run_attempt)
            .field("action", &self.action)
            .field("workflow_name", &"<reviewed-github-name>")
            .field("job_name", &"<reviewed-github-name>")
            .field("head_commit", &self.head_commit)
            .field("head_tree", &self.head_tree)
            .field("created_at", &self.created_at)
            .field("started_at", &self.started_at)
            .field("completed_at", &self.completed_at)
            .field("conclusion", &self.conclusion)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct GitHubWorkflowJobReconciliationPageDefinition {
    pub page_number: u16,
    pub has_next_page: bool,
    pub jobs: Vec<GitHubWorkflowJobReconciliationJobDefinition>,
}

impl fmt::Debug for GitHubWorkflowJobReconciliationPageDefinition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GitHubWorkflowJobReconciliationPageDefinition")
            .field("page_number", &self.page_number)
            .field("has_next_page", &self.has_next_page)
            .field("job_count", &self.jobs.len())
            .field("jobs", &"<private-reviewed-job-evidence>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct GitHubWorkflowJobReconciliationSnapshotDefinition {
    pub snapshot_generation: u64,
    pub repository: RepositoryRef,
    pub query_started_at: EpochMillis,
    pub observed_at: EpochMillis,
    pub reported_total_jobs: u32,
    pub pages: Vec<GitHubWorkflowJobReconciliationPageDefinition>,
}

impl fmt::Debug for GitHubWorkflowJobReconciliationSnapshotDefinition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GitHubWorkflowJobReconciliationSnapshotDefinition")
            .field("snapshot_generation", &self.snapshot_generation)
            .field("repository", &self.repository)
            .field("query_started_at", &self.query_started_at)
            .field("observed_at", &self.observed_at)
            .field("reported_total_jobs", &self.reported_total_jobs)
            .field("page_count", &self.pages.len())
            .field("pages", &"<private-reviewed-page-evidence>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct GitHubWorkflowJobReconciliationEventIdentity {
    workflow_job_id: u64,
    run_id: u64,
    run_attempt: u32,
}

impl GitHubWorkflowJobReconciliationEventIdentity {
    #[must_use]
    pub const fn workflow_job_id(&self) -> u64 {
        self.workflow_job_id
    }

    #[must_use]
    pub const fn run_id(&self) -> u64 {
        self.run_id
    }

    #[must_use]
    pub const fn run_attempt(&self) -> u32 {
        self.run_attempt
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct GitHubWorkflowJobReconciledEvent {
    identity: GitHubWorkflowJobReconciliationEventIdentity,
    event: GitHubWorkflowJobEvent,
}

impl GitHubWorkflowJobReconciledEvent {
    #[must_use]
    pub const fn identity(&self) -> GitHubWorkflowJobReconciliationEventIdentity {
        self.identity
    }

    #[must_use]
    pub const fn event(&self) -> &GitHubWorkflowJobEvent {
        &self.event
    }
}

impl fmt::Debug for GitHubWorkflowJobReconciledEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GitHubWorkflowJobReconciledEvent")
            .field("identity", &self.identity)
            .field("event", &"<private-reviewed-mapper-event>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitHubWorkflowJobReconciliationReceipt {
    schema_version: u8,
    snapshot_generation: u64,
    repository: RepositoryRef,
    query_started_at: EpochMillis,
    observed_at: EpochMillis,
    normalized_at: EpochMillis,
    page_count: u16,
    job_count: u32,
}

impl GitHubWorkflowJobReconciliationReceipt {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn snapshot_generation(&self) -> u64 {
        self.snapshot_generation
    }

    #[must_use]
    pub const fn repository(&self) -> &RepositoryRef {
        &self.repository
    }

    #[must_use]
    pub const fn page_count(&self) -> u16 {
        self.page_count
    }

    #[must_use]
    pub const fn job_count(&self) -> u32 {
        self.job_count
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct GitHubWorkflowJobReconciliationBatch {
    receipt: GitHubWorkflowJobReconciliationReceipt,
    jobs: Vec<GitHubWorkflowJobReconciledEvent>,
}

impl GitHubWorkflowJobReconciliationBatch {
    #[must_use]
    pub const fn receipt(&self) -> &GitHubWorkflowJobReconciliationReceipt {
        &self.receipt
    }

    #[must_use]
    pub fn jobs(&self) -> &[GitHubWorkflowJobReconciledEvent] {
        &self.jobs
    }
}

impl fmt::Debug for GitHubWorkflowJobReconciliationBatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GitHubWorkflowJobReconciliationBatch")
            .field("receipt", &self.receipt)
            .field("job_count", &self.jobs.len())
            .field("jobs", &"<private-reviewed-mapper-events>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct GitHubWorkflowJobReconciliationError {
    pub field: &'static str,
    pub code: &'static str,
    pub message: &'static str,
}

impl GitHubWorkflowJobReconciliationError {
    const fn new(field: &'static str, code: &'static str, message: &'static str) -> Self {
        Self {
            field,
            code,
            message,
        }
    }
}

impl fmt::Display for GitHubWorkflowJobReconciliationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for GitHubWorkflowJobReconciliationError {}

fn validate_snapshot_timing(
    policy: &GitHubWorkflowJobReconciliationPolicy,
    snapshot: &GitHubWorkflowJobReconciliationSnapshotDefinition,
    decision_at: EpochMillis,
) -> Result<(), GitHubWorkflowJobReconciliationError> {
    let duration = snapshot
        .observed_at
        .get()
        .checked_sub(snapshot.query_started_at.get())
        .ok_or_else(|| {
            GitHubWorkflowJobReconciliationError::new(
                "query_started_at",
                "query_time_reversal",
                "GitHub reconciliation query completion cannot precede its start",
            )
        })?;
    if duration > policy.max_query_duration_millis {
        return Err(GitHubWorkflowJobReconciliationError::new(
            "observed_at",
            "query_duration_exceeded",
            "GitHub reconciliation query exceeded the reviewed duration bound",
        ));
    }
    let age = decision_at
        .get()
        .checked_sub(snapshot.observed_at.get())
        .ok_or_else(|| {
            GitHubWorkflowJobReconciliationError::new(
                "observed_at",
                "future_snapshot",
                "GitHub reconciliation snapshot cannot be newer than normalization",
            )
        })?;
    if age > policy.max_snapshot_age_millis {
        return Err(GitHubWorkflowJobReconciliationError::new(
            "observed_at",
            "stale_snapshot",
            "GitHub reconciliation snapshot is older than the reviewed freshness window",
        ));
    }
    Ok(())
}

fn validate_generation(value: u64) -> Result<(), GitHubWorkflowJobReconciliationError> {
    if !(1..=MAX_GITHUB_RECONCILIATION_GENERATION).contains(&value) {
        return Err(GitHubWorkflowJobReconciliationError::new(
            "snapshot_generation",
            "invalid_snapshot_generation",
            "GitHub reconciliation snapshot generation must be within the bounded positive range",
        ));
    }
    Ok(())
}

fn validate_page_count(value: usize) -> Result<(), GitHubWorkflowJobReconciliationError> {
    if value == 0 || value > MAX_GITHUB_RECONCILIATION_PAGES {
        return Err(GitHubWorkflowJobReconciliationError::new(
            "pages",
            "invalid_page_count",
            "GitHub reconciliation snapshot must contain a bounded nonempty page set",
        ));
    }
    Ok(())
}

fn bounded_u16(value: usize) -> Result<u16, GitHubWorkflowJobReconciliationError> {
    u16::try_from(value).map_err(|_| {
        GitHubWorkflowJobReconciliationError::new(
            "count",
            "count_out_of_range",
            "GitHub reconciliation count cannot be represented safely",
        )
    })
}

fn bounded_u32(value: usize) -> Result<u32, GitHubWorkflowJobReconciliationError> {
    u32::try_from(value).map_err(|_| {
        GitHubWorkflowJobReconciliationError::new(
            "count",
            "count_out_of_range",
            "GitHub reconciliation count cannot be represented safely",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRIVATE_WORKFLOW: &str = "Private Workflow / secret-looking";
    const PRIVATE_JOB: &str = "private job token-looking";

    fn time(value: u64) -> EpochMillis {
        EpochMillis::new(value).expect("time")
    }

    fn repository() -> RepositoryRef {
        RepositoryRef::parse("teamleaderleo/smolrunner").expect("repository")
    }

    fn policy() -> GitHubWorkflowJobReconciliationPolicy {
        GitHubWorkflowJobReconciliationPolicy::new(30_000, 10_000).expect("policy")
    }

    fn job(
        id: u64,
        action: GitHubWorkflowJobAction,
    ) -> GitHubWorkflowJobReconciliationJobDefinition {
        let (started_at, completed_at, conclusion) = match action {
            GitHubWorkflowJobAction::Queued => (None, None, None),
            GitHubWorkflowJobAction::InProgress => (Some(time(1_100)), None, None),
            GitHubWorkflowJobAction::Completed => (
                Some(time(1_100)),
                Some(time(1_200)),
                Some(GitHubWorkflowJobConclusion::Success),
            ),
        };
        GitHubWorkflowJobReconciliationJobDefinition {
            workflow_job_id: id,
            run_id: id + 1_000,
            run_attempt: 1,
            action,
            workflow_name: "Verify".to_owned(),
            job_name: "verify".to_owned(),
            head_commit: CommitId::parse(&"a".repeat(40)).expect("commit"),
            head_tree: GitTreeId::parse(&"b".repeat(40)).expect("tree"),
            created_at: time(1_000),
            started_at,
            completed_at,
            conclusion,
        }
    }

    fn snapshot(
        pages: Vec<GitHubWorkflowJobReconciliationPageDefinition>,
        total: u32,
    ) -> GitHubWorkflowJobReconciliationSnapshotDefinition {
        GitHubWorkflowJobReconciliationSnapshotDefinition {
            snapshot_generation: 7,
            repository: repository(),
            query_started_at: time(1_250),
            observed_at: time(1_300),
            reported_total_jobs: total,
            pages,
        }
    }

    #[test]
    fn complete_pages_normalize_into_deterministic_identity_order() {
        let batch = policy()
            .normalize(
                snapshot(
                    vec![
                        GitHubWorkflowJobReconciliationPageDefinition {
                            page_number: 1,
                            has_next_page: true,
                            jobs: vec![job(30, GitHubWorkflowJobAction::Completed)],
                        },
                        GitHubWorkflowJobReconciliationPageDefinition {
                            page_number: 2,
                            has_next_page: false,
                            jobs: vec![
                                job(10, GitHubWorkflowJobAction::Queued),
                                job(20, GitHubWorkflowJobAction::InProgress),
                            ],
                        },
                    ],
                    3,
                ),
                time(1_300),
            )
            .expect("normalization");

        assert_eq!(batch.receipt().schema_version(), 1);
        assert_eq!(batch.receipt().snapshot_generation(), 7);
        assert_eq!(batch.receipt().page_count(), 2);
        assert_eq!(batch.receipt().job_count(), 3);
        assert_eq!(
            batch
                .jobs()
                .iter()
                .map(|item| item.identity().workflow_job_id())
                .collect::<Vec<_>>(),
            vec![10, 20, 30]
        );
    }

    #[test]
    fn empty_complete_snapshot_is_represented_exactly() {
        let batch = policy()
            .normalize(
                snapshot(
                    vec![GitHubWorkflowJobReconciliationPageDefinition {
                        page_number: 1,
                        has_next_page: false,
                        jobs: vec![],
                    }],
                    0,
                ),
                time(1_300),
            )
            .expect("empty normalization");
        assert!(batch.jobs().is_empty());
        assert_eq!(batch.receipt().job_count(), 0);
    }

    #[test]
    fn incomplete_or_drifting_pagination_fails_closed() {
        for pages in [
            vec![GitHubWorkflowJobReconciliationPageDefinition {
                page_number: 2,
                has_next_page: false,
                jobs: vec![],
            }],
            vec![GitHubWorkflowJobReconciliationPageDefinition {
                page_number: 1,
                has_next_page: true,
                jobs: vec![],
            }],
        ] {
            assert!(policy().normalize(snapshot(pages, 0), time(1_300)).is_err());
        }
    }

    #[test]
    fn duplicate_identity_and_reported_total_drift_are_rejected() {
        let duplicate = job(10, GitHubWorkflowJobAction::Queued);
        let error = policy()
            .normalize(
                snapshot(
                    vec![GitHubWorkflowJobReconciliationPageDefinition {
                        page_number: 1,
                        has_next_page: false,
                        jobs: vec![duplicate.clone(), duplicate],
                    }],
                    2,
                ),
                time(1_300),
            )
            .expect_err("duplicate");
        assert_eq!(error.code, "duplicate_job_identity");

        let error = policy()
            .normalize(
                snapshot(
                    vec![GitHubWorkflowJobReconciliationPageDefinition {
                        page_number: 1,
                        has_next_page: false,
                        jobs: vec![job(10, GitHubWorkflowJobAction::Queued)],
                    }],
                    2,
                ),
                time(1_300),
            )
            .expect_err("reported total");
        assert_eq!(error.code, "reported_total_drift");
    }

    #[test]
    fn stale_future_and_excessive_query_evidence_is_rejected() {
        let pages = || {
            vec![GitHubWorkflowJobReconciliationPageDefinition {
                page_number: 1,
                has_next_page: false,
                jobs: vec![],
            }]
        };

        let stale = snapshot(pages(), 0);
        assert_eq!(
            policy()
                .normalize(stale, time(40_000))
                .expect_err("stale")
                .code,
            "stale_snapshot"
        );

        let future = snapshot(pages(), 0);
        assert_eq!(
            policy()
                .normalize(future, time(1_299))
                .expect_err("future")
                .code,
            "future_snapshot"
        );

        let mut slow = snapshot(pages(), 0);
        slow.query_started_at = time(1);
        let short_query_policy =
            GitHubWorkflowJobReconciliationPolicy::new(30_000, 100).expect("short policy");
        assert_eq!(
            short_query_policy
                .normalize(slow, time(1_300))
                .expect_err("slow")
                .code,
            "query_duration_exceeded"
        );
    }

    #[test]
    fn malformed_mapper_evidence_is_rejected_before_batch_publication() {
        let mut malformed = job(10, GitHubWorkflowJobAction::Queued);
        malformed.started_at = Some(time(1_100));
        let error = policy()
            .normalize(
                snapshot(
                    vec![GitHubWorkflowJobReconciliationPageDefinition {
                        page_number: 1,
                        has_next_page: false,
                        jobs: vec![malformed],
                    }],
                    1,
                ),
                time(1_300),
            )
            .expect_err("malformed");
        assert_eq!(error.code, "invalid_job_evidence");
    }

    #[test]
    fn public_receipt_and_debug_surfaces_omit_private_job_names() {
        let mut private_job = job(10, GitHubWorkflowJobAction::Queued);
        private_job.workflow_name = PRIVATE_WORKFLOW.to_owned();
        private_job.job_name = PRIVATE_JOB.to_owned();
        let definition = snapshot(
            vec![GitHubWorkflowJobReconciliationPageDefinition {
                page_number: 1,
                has_next_page: false,
                jobs: vec![private_job],
            }],
            1,
        );
        let definition_debug = format!("{definition:?}");
        let batch = policy()
            .normalize(definition, time(1_300))
            .expect("normalization");
        let receipt_json = serde_json::to_string(batch.receipt()).expect("receipt JSON");
        let debug = format!("{batch:?} {:?}", batch.jobs()[0]);

        for output in [definition_debug, receipt_json, debug] {
            assert!(!output.contains(PRIVATE_WORKFLOW));
            assert!(!output.contains(PRIVATE_JOB));
            assert!(!output.contains("credential"));
            assert!(!output.contains("authorization"));
            assert!(!output.contains("stdout"));
            assert!(!output.contains("stderr"));
            assert!(!output.contains("/Users/"));
        }
    }
}
