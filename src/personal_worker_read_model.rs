use std::fmt;

use serde::Serialize;

use crate::artifact::Sha256Digest;
use crate::execution_admission::{
    DrainAcknowledgement, EpochMillis, ExecutionAdmissionIdentity, ExecutionAdmissionState,
    ExecutionRequestId, ExecutionResourceLimits, FallbackProfileEligibility, ReservationGeneration,
    ReservationId, UnavailableReason,
};
use crate::personal_worker_queue::{
    PersonalWorkerActivityEvidence, PersonalWorkerCacheAccessMode, PersonalWorkerCacheNamespace,
    PersonalWorkerCancellationState, PersonalWorkerPriority, PersonalWorkerProfile,
    PersonalWorkerProfileObservation, PersonalWorkerQueueGeneration, PersonalWorkerQueueVisibility,
    PersonalWorkerSourceIdentity, evaluate_personal_worker_queue,
};
use crate::personal_worker_store::{
    PersonalWorkerStoreDocument, PersonalWorkerStoreRevision, PersonalWorkerTerminalMutationClass,
};

pub const PERSONAL_WORKER_READ_MODEL_SCHEMA_VERSION: u8 = 2;
pub const MAX_PERSONAL_WORKER_QUEUE_PAGE_SIZE: u16 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerProfileIntentView {
    target: PersonalWorkerProfile,
    requested_at: EpochMillis,
}

impl PersonalWorkerProfileIntentView {
    #[must_use]
    pub const fn target(&self) -> PersonalWorkerProfile {
        self.target
    }

    #[must_use]
    pub const fn requested_at(&self) -> EpochMillis {
        self.requested_at
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerStatusView {
    schema_version: u8,
    store_revision: PersonalWorkerStoreRevision,
    queue_generation: PersonalWorkerQueueGeneration,
    observed_at: EpochMillis,
    profile_observation: PersonalWorkerProfileObservation,
    activity_evidence: PersonalWorkerActivityEvidence,
    desired_profile: PersonalWorkerProfile,
    #[serde(skip_serializing_if = "Option::is_none")]
    pending_profile_change: Option<PersonalWorkerProfileIntentView>,
    queued_entry_count: u32,
    eligible_queue_count: u32,
    cancelled_queue_count: u32,
    selected_count: u32,
    active_count: u32,
    draining_count: u32,
    cache_lease_count: u32,
    terminal_tombstone_count: u32,
}

impl PersonalWorkerStatusView {
    #[must_use]
    pub const fn store_revision(&self) -> PersonalWorkerStoreRevision {
        self.store_revision
    }

    #[must_use]
    pub const fn queue_generation(&self) -> PersonalWorkerQueueGeneration {
        self.queue_generation
    }

    #[must_use]
    pub const fn profile_observation(&self) -> PersonalWorkerProfileObservation {
        self.profile_observation
    }

    #[must_use]
    pub const fn current_profile(&self) -> Option<PersonalWorkerProfile> {
        self.profile_observation.profile()
    }

    #[must_use]
    pub const fn activity_evidence(&self) -> PersonalWorkerActivityEvidence {
        self.activity_evidence
    }

    #[must_use]
    pub const fn last_activity_at(&self) -> Option<EpochMillis> {
        self.activity_evidence.last_activity_at()
    }

    #[must_use]
    pub const fn desired_profile(&self) -> PersonalWorkerProfile {
        self.desired_profile
    }

    #[must_use]
    pub const fn pending_profile_change(&self) -> Option<PersonalWorkerProfileIntentView> {
        self.pending_profile_change
    }

    #[must_use]
    pub const fn queued_entry_count(&self) -> u32 {
        self.queued_entry_count
    }

    #[must_use]
    pub const fn eligible_queue_count(&self) -> u32 {
        self.eligible_queue_count
    }

    #[must_use]
    pub const fn cancelled_queue_count(&self) -> u32 {
        self.cancelled_queue_count
    }

    #[must_use]
    pub const fn selected_count(&self) -> u32 {
        self.selected_count
    }

    #[must_use]
    pub const fn active_count(&self) -> u32 {
        self.active_count
    }

    #[must_use]
    pub const fn draining_count(&self) -> u32 {
        self.draining_count
    }

    #[must_use]
    pub const fn cache_lease_count(&self) -> u32 {
        self.cache_lease_count
    }

    #[must_use]
    pub const fn terminal_tombstone_count(&self) -> u32 {
        self.terminal_tombstone_count
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersonalWorkerQueuePageRequest {
    expected_revision: PersonalWorkerStoreRevision,
    expected_queue_generation: PersonalWorkerQueueGeneration,
    offset: u32,
    limit: u16,
}

impl PersonalWorkerQueuePageRequest {
    pub fn new(
        expected_revision: PersonalWorkerStoreRevision,
        expected_queue_generation: PersonalWorkerQueueGeneration,
        offset: u32,
        limit: u16,
    ) -> Result<Self, PersonalWorkerReadError> {
        if !(1..=MAX_PERSONAL_WORKER_QUEUE_PAGE_SIZE).contains(&limit) {
            return Err(PersonalWorkerReadError::new(
                PersonalWorkerReadErrorKind::InvalidPage,
                "personal worker queue page limit is outside the bounded positive range",
            ));
        }
        Ok(Self {
            expected_revision,
            expected_queue_generation,
            offset,
            limit,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerQueuePage {
    schema_version: u8,
    store_revision: PersonalWorkerStoreRevision,
    queue_generation: PersonalWorkerQueueGeneration,
    observed_at: EpochMillis,
    offset: u32,
    limit: u16,
    total: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_offset: Option<u32>,
    items: Vec<PersonalWorkerQueueVisibility>,
}

impl PersonalWorkerQueuePage {
    #[must_use]
    pub const fn store_revision(&self) -> PersonalWorkerStoreRevision {
        self.store_revision
    }

    #[must_use]
    pub const fn queue_generation(&self) -> PersonalWorkerQueueGeneration {
        self.queue_generation
    }

    #[must_use]
    pub const fn total(&self) -> u32 {
        self.total
    }

    #[must_use]
    pub const fn next_offset(&self) -> Option<u32> {
        self.next_offset
    }

    #[must_use]
    pub fn items(&self) -> &[PersonalWorkerQueueVisibility] {
        &self.items
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonalWorkerJobReadRequest {
    expected_revision: PersonalWorkerStoreRevision,
    expected_queue_generation: PersonalWorkerQueueGeneration,
    request_id: ExecutionRequestId,
}

impl PersonalWorkerJobReadRequest {
    #[must_use]
    pub const fn new(
        expected_revision: PersonalWorkerStoreRevision,
        expected_queue_generation: PersonalWorkerQueueGeneration,
        request_id: ExecutionRequestId,
    ) -> Self {
        Self {
            expected_revision,
            expected_queue_generation,
            request_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerReservationView {
    id: ReservationId,
    generation: ReservationGeneration,
    reserved_at: EpochMillis,
    expires_at: EpochMillis,
}

impl PersonalWorkerReservationView {
    #[must_use]
    pub const fn id(&self) -> &ReservationId {
        &self.id
    }

    #[must_use]
    pub const fn generation(&self) -> ReservationGeneration {
        self.generation
    }

    #[must_use]
    pub const fn reserved_at(&self) -> EpochMillis {
        self.reserved_at
    }

    #[must_use]
    pub const fn expires_at(&self) -> EpochMillis {
        self.expires_at
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerAdmissionView {
    state: ExecutionAdmissionState,
    observed_at: EpochMillis,
    reservation: PersonalWorkerReservationView,
}

impl PersonalWorkerAdmissionView {
    #[must_use]
    pub const fn state(&self) -> ExecutionAdmissionState {
        self.state
    }

    #[must_use]
    pub const fn observed_at(&self) -> EpochMillis {
        self.observed_at
    }

    #[must_use]
    pub const fn reservation(&self) -> &PersonalWorkerReservationView {
        &self.reservation
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerDurableCacheLeaseView {
    reservation_id: ReservationId,
    reservation_generation: ReservationGeneration,
    acquired_at: EpochMillis,
}

impl PersonalWorkerDurableCacheLeaseView {
    #[must_use]
    pub const fn reservation_id(&self) -> &ReservationId {
        &self.reservation_id
    }

    #[must_use]
    pub const fn reservation_generation(&self) -> ReservationGeneration {
        self.reservation_generation
    }

    #[must_use]
    pub const fn acquired_at(&self) -> EpochMillis {
        self.acquired_at
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerTerminalRequestView {
    identity: ExecutionAdmissionIdentity,
    source: PersonalWorkerSourceIdentity,
    priority: PersonalWorkerPriority,
    requested_limits: ExecutionResourceLimits,
    cache_namespace: PersonalWorkerCacheNamespace,
    cache_access: PersonalWorkerCacheAccessMode,
    fallback_eligibility: FallbackProfileEligibility,
}

impl PersonalWorkerTerminalRequestView {
    #[must_use]
    pub const fn identity(&self) -> &ExecutionAdmissionIdentity {
        &self.identity
    }

    #[must_use]
    pub const fn source(&self) -> &PersonalWorkerSourceIdentity {
        &self.source
    }

    #[must_use]
    pub const fn priority(&self) -> PersonalWorkerPriority {
        self.priority
    }

    #[must_use]
    pub const fn requested_limits(&self) -> ExecutionResourceLimits {
        self.requested_limits
    }

    #[must_use]
    pub const fn cache_namespace(&self) -> &PersonalWorkerCacheNamespace {
        &self.cache_namespace
    }

    #[must_use]
    pub const fn cache_access(&self) -> PersonalWorkerCacheAccessMode {
        self.cache_access
    }

    #[must_use]
    pub const fn fallback_eligibility(&self) -> &FallbackProfileEligibility {
        &self.fallback_eligibility
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerTerminalJobView {
    mutation_class: PersonalWorkerTerminalMutationClass,
    request: PersonalWorkerTerminalRequestView,
    admission_state: ExecutionAdmissionState,
    completed_at: EpochMillis,
    #[serde(skip_serializing_if = "Option::is_none")]
    started_at: Option<EpochMillis>,
    unavailable_reason: UnavailableReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    acknowledgement: Option<DrainAcknowledgement>,
    reservation: PersonalWorkerReservationView,
    durable_cache_lease: PersonalWorkerDurableCacheLeaseView,
    evidence_digest: Sha256Digest,
}

impl PersonalWorkerTerminalJobView {
    #[must_use]
    pub const fn mutation_class(&self) -> PersonalWorkerTerminalMutationClass {
        self.mutation_class
    }

    #[must_use]
    pub const fn request(&self) -> &PersonalWorkerTerminalRequestView {
        &self.request
    }

    #[must_use]
    pub const fn admission_state(&self) -> ExecutionAdmissionState {
        self.admission_state
    }

    #[must_use]
    pub const fn completed_at(&self) -> EpochMillis {
        self.completed_at
    }

    #[must_use]
    pub const fn started_at(&self) -> Option<EpochMillis> {
        self.started_at
    }

    #[must_use]
    pub const fn unavailable_reason(&self) -> UnavailableReason {
        self.unavailable_reason
    }

    #[must_use]
    pub const fn acknowledgement(&self) -> Option<DrainAcknowledgement> {
        self.acknowledgement
    }

    #[must_use]
    pub const fn reservation(&self) -> &PersonalWorkerReservationView {
        &self.reservation
    }

    #[must_use]
    pub const fn durable_cache_lease(&self) -> &PersonalWorkerDurableCacheLeaseView {
        &self.durable_cache_lease
    }

    #[must_use]
    pub const fn evidence_digest(&self) -> &Sha256Digest {
        &self.evidence_digest
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PersonalWorkerJobStateView {
    Queued {
        entry: PersonalWorkerQueueVisibility,
    },
    Active {
        entry: PersonalWorkerQueueVisibility,
        admission: PersonalWorkerAdmissionView,
        durable_cache_lease: PersonalWorkerDurableCacheLeaseView,
    },
    Terminal {
        terminal: Box<PersonalWorkerTerminalJobView>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerJobView {
    schema_version: u8,
    store_revision: PersonalWorkerStoreRevision,
    queue_generation: PersonalWorkerQueueGeneration,
    observed_at: EpochMillis,
    submitted_at: EpochMillis,
    #[serde(skip_serializing_if = "Option::is_none")]
    operator_deadline: Option<EpochMillis>,
    cancellation: PersonalWorkerCancellationState,
    job: PersonalWorkerJobStateView,
}

impl PersonalWorkerJobView {
    #[must_use]
    pub const fn store_revision(&self) -> PersonalWorkerStoreRevision {
        self.store_revision
    }

    #[must_use]
    pub const fn queue_generation(&self) -> PersonalWorkerQueueGeneration {
        self.queue_generation
    }

    #[must_use]
    pub const fn submitted_at(&self) -> EpochMillis {
        self.submitted_at
    }

    #[must_use]
    pub const fn cancellation(&self) -> PersonalWorkerCancellationState {
        self.cancellation
    }

    #[must_use]
    pub const fn state(&self) -> &PersonalWorkerJobStateView {
        &self.job
    }

    #[must_use]
    pub fn entry(&self) -> Option<&PersonalWorkerQueueVisibility> {
        match &self.job {
            PersonalWorkerJobStateView::Queued { entry }
            | PersonalWorkerJobStateView::Active { entry, .. } => Some(entry),
            PersonalWorkerJobStateView::Terminal { .. } => None,
        }
    }

    #[must_use]
    pub fn admission(&self) -> Option<&PersonalWorkerAdmissionView> {
        match &self.job {
            PersonalWorkerJobStateView::Active { admission, .. } => Some(admission),
            PersonalWorkerJobStateView::Queued { .. }
            | PersonalWorkerJobStateView::Terminal { .. } => None,
        }
    }

    #[must_use]
    pub fn durable_cache_lease(&self) -> Option<&PersonalWorkerDurableCacheLeaseView> {
        match &self.job {
            PersonalWorkerJobStateView::Active {
                durable_cache_lease,
                ..
            } => Some(durable_cache_lease),
            PersonalWorkerJobStateView::Queued { .. }
            | PersonalWorkerJobStateView::Terminal { .. } => None,
        }
    }

    #[must_use]
    pub fn terminal(&self) -> Option<&PersonalWorkerTerminalJobView> {
        match &self.job {
            PersonalWorkerJobStateView::Terminal { terminal } => Some(terminal),
            PersonalWorkerJobStateView::Queued { .. }
            | PersonalWorkerJobStateView::Active { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerReadErrorKind {
    StaleRevision,
    StaleQueueGeneration,
    InvalidPage,
    OffsetOutOfBounds,
    NotFound,
    InvalidDocument,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerReadError {
    kind: PersonalWorkerReadErrorKind,
    public_message: &'static str,
}

impl PersonalWorkerReadError {
    #[must_use]
    pub const fn kind(&self) -> PersonalWorkerReadErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn message(&self) -> &'static str {
        self.public_message
    }

    const fn new(kind: PersonalWorkerReadErrorKind, public_message: &'static str) -> Self {
        Self {
            kind,
            public_message,
        }
    }

    const fn invalid_document() -> Self {
        Self::new(
            PersonalWorkerReadErrorKind::InvalidDocument,
            "durable personal worker state cannot produce a valid read model",
        )
    }
}

impl fmt::Display for PersonalWorkerReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.public_message)
    }
}

impl std::error::Error for PersonalWorkerReadError {}

/// Derive one bounded worker-status view from an already loaded durable document.
///
/// # Errors
///
/// Returns a fixed error if the durable queue state cannot be evaluated or represented safely.
pub fn personal_worker_status(
    document: &PersonalWorkerStoreDocument,
) -> Result<PersonalWorkerStatusView, PersonalWorkerReadError> {
    let decision = evaluate_personal_worker_queue(document.queue(), None)
        .map_err(|_| PersonalWorkerReadError::invalid_document())?;
    let queue = document.queue();
    let eligible_queue_count = bounded_count(
        queue
            .queued
            .iter()
            .filter(|request| !request.cancellation.is_cancelled())
            .count(),
    )?;
    let cancelled_queue_count = bounded_count(
        queue
            .queued
            .iter()
            .filter(|request| request.cancellation.is_cancelled())
            .count(),
    )?;
    let draining_count = bounded_count(
        queue
            .active
            .iter()
            .filter(|active| active.admission.state() == ExecutionAdmissionState::Draining)
            .count(),
    )?;
    Ok(PersonalWorkerStatusView {
        schema_version: PERSONAL_WORKER_READ_MODEL_SCHEMA_VERSION,
        store_revision: document.revision(),
        queue_generation: queue.generation,
        observed_at: queue.observed_at,
        profile_observation: queue.profile_observation,
        activity_evidence: queue.activity_evidence,
        desired_profile: decision.desired_profile,
        pending_profile_change: queue.pending_profile_change.map(|pending| {
            PersonalWorkerProfileIntentView {
                target: pending.target,
                requested_at: pending.requested_at,
            }
        }),
        queued_entry_count: bounded_count(queue.queued.len())?,
        eligible_queue_count,
        cancelled_queue_count,
        selected_count: bounded_count(decision.selected.len())?,
        active_count: bounded_count(queue.active.len())?,
        draining_count,
        cache_lease_count: bounded_count(document.cache_leases().len())?,
        terminal_tombstone_count: bounded_count(document.terminal_tombstones().len())?,
    })
}

/// Derive one exact, bounded queue page from an already loaded durable document.
///
/// # Errors
///
/// Returns a fixed error for stale snapshot expectations, invalid pagination, or invalid durable
/// queue state.
pub fn personal_worker_queue_page(
    document: &PersonalWorkerStoreDocument,
    request: PersonalWorkerQueuePageRequest,
) -> Result<PersonalWorkerQueuePage, PersonalWorkerReadError> {
    validate_snapshot(
        document,
        request.expected_revision,
        request.expected_queue_generation,
    )?;
    let decision = evaluate_personal_worker_queue(document.queue(), None)
        .map_err(|_| PersonalWorkerReadError::invalid_document())?;
    let total = bounded_count(decision.visibility.len())?;
    if request.offset > total {
        return Err(PersonalWorkerReadError::new(
            PersonalWorkerReadErrorKind::OffsetOutOfBounds,
            "personal worker queue page offset exceeds the exact snapshot length",
        ));
    }
    let start =
        usize::try_from(request.offset).map_err(|_| PersonalWorkerReadError::invalid_document())?;
    let end = start
        .saturating_add(usize::from(request.limit))
        .min(decision.visibility.len());
    let next_offset = if end < decision.visibility.len() {
        Some(bounded_count(end)?)
    } else {
        None
    };
    Ok(PersonalWorkerQueuePage {
        schema_version: PERSONAL_WORKER_READ_MODEL_SCHEMA_VERSION,
        store_revision: document.revision(),
        queue_generation: document.queue().generation,
        observed_at: document.queue().observed_at,
        offset: request.offset,
        limit: request.limit,
        total,
        next_offset,
        items: decision.visibility[start..end].to_vec(),
    })
}

/// Derive one exact job view from queued, active, or retained terminal durable state.
///
/// Queue pagination remains live-state-only. Terminal views are projected only from the exact
/// bounded durable tombstone ledger and never fabricate queue visibility.
///
/// # Errors
///
/// Returns a fixed error for stale snapshot expectations, an unprovable or evicted request,
/// or invalid durable reservation, cache-lease, or terminal evidence.
pub fn personal_worker_job_view(
    document: &PersonalWorkerStoreDocument,
    request: PersonalWorkerJobReadRequest,
) -> Result<PersonalWorkerJobView, PersonalWorkerReadError> {
    validate_snapshot(
        document,
        request.expected_revision,
        request.expected_queue_generation,
    )?;
    let decision = evaluate_personal_worker_queue(document.queue(), None)
        .map_err(|_| PersonalWorkerReadError::invalid_document())?;

    if let Some(queued) = document
        .queue()
        .queued
        .iter()
        .find(|queued| queued.identity.request_id == request.request_id)
    {
        let entry = decision
            .visibility
            .iter()
            .find(|entry| entry.request_id == request.request_id)
            .cloned()
            .ok_or_else(PersonalWorkerReadError::invalid_document)?;
        return Ok(PersonalWorkerJobView {
            schema_version: PERSONAL_WORKER_READ_MODEL_SCHEMA_VERSION,
            store_revision: document.revision(),
            queue_generation: document.queue().generation,
            observed_at: document.queue().observed_at,
            submitted_at: queued.submitted_at,
            operator_deadline: queued.operator_deadline,
            cancellation: queued.cancellation,
            job: PersonalWorkerJobStateView::Queued { entry },
        });
    }

    if let Some(active) = document
        .queue()
        .active
        .iter()
        .find(|active| active.request.identity.request_id == request.request_id)
    {
        let entry = decision
            .visibility
            .iter()
            .find(|entry| entry.request_id == request.request_id)
            .cloned()
            .ok_or_else(PersonalWorkerReadError::invalid_document)?;
        let reservation = active
            .admission
            .reservation()
            .ok_or_else(PersonalWorkerReadError::invalid_document)?;
        let lease = document
            .cache_leases()
            .iter()
            .find(|lease| lease.request_id() == &request.request_id)
            .ok_or_else(PersonalWorkerReadError::invalid_document)?;
        return Ok(PersonalWorkerJobView {
            schema_version: PERSONAL_WORKER_READ_MODEL_SCHEMA_VERSION,
            store_revision: document.revision(),
            queue_generation: document.queue().generation,
            observed_at: document.queue().observed_at,
            submitted_at: active.request.submitted_at,
            operator_deadline: active.request.operator_deadline,
            cancellation: active.request.cancellation,
            job: PersonalWorkerJobStateView::Active {
                entry,
                admission: PersonalWorkerAdmissionView {
                    state: active.admission.state(),
                    observed_at: active.admission.observed_at(),
                    reservation: PersonalWorkerReservationView {
                        id: reservation.id.clone(),
                        generation: reservation.generation,
                        reserved_at: reservation.reserved_at,
                        expires_at: reservation.expires_at,
                    },
                },
                durable_cache_lease: PersonalWorkerDurableCacheLeaseView {
                    reservation_id: lease.reservation_id().clone(),
                    reservation_generation: lease.reservation_generation(),
                    acquired_at: lease.acquired_at(),
                },
            },
        });
    }

    if let Some(tombstone) = document
        .terminal_tombstones()
        .iter()
        .find(|terminal| terminal.request().identity.request_id == request.request_id)
    {
        let terminal_request = tombstone.request();
        let terminal_admission = tombstone.terminal_admission();
        let reservation = terminal_admission
            .reservation()
            .ok_or_else(PersonalWorkerReadError::invalid_document)?;
        let unavailable_reason = terminal_admission
            .unavailable_reason()
            .ok_or_else(PersonalWorkerReadError::invalid_document)?;
        let lease = tombstone.cache_lease();
        return Ok(PersonalWorkerJobView {
            schema_version: PERSONAL_WORKER_READ_MODEL_SCHEMA_VERSION,
            store_revision: document.revision(),
            queue_generation: document.queue().generation,
            observed_at: document.queue().observed_at,
            submitted_at: terminal_request.submitted_at,
            operator_deadline: terminal_request.operator_deadline,
            cancellation: terminal_request.cancellation,
            job: PersonalWorkerJobStateView::Terminal {
                terminal: Box::new(PersonalWorkerTerminalJobView {
                    mutation_class: tombstone.mutation_class(),
                    request: PersonalWorkerTerminalRequestView {
                        identity: terminal_request.identity.clone(),
                        source: terminal_request.source.clone(),
                        priority: terminal_request.priority,
                        requested_limits: terminal_request.requested_limits,
                        cache_namespace: terminal_request.cache_namespace.clone(),
                        cache_access: terminal_request.cache_access,
                        fallback_eligibility: terminal_request.fallback_eligibility.clone(),
                    },
                    admission_state: terminal_admission.state(),
                    completed_at: tombstone.completed_at(),
                    started_at: tombstone.started_at(),
                    unavailable_reason,
                    acknowledgement: terminal_admission.acknowledgement(),
                    reservation: PersonalWorkerReservationView {
                        id: reservation.id.clone(),
                        generation: reservation.generation,
                        reserved_at: reservation.reserved_at,
                        expires_at: reservation.expires_at,
                    },
                    durable_cache_lease: PersonalWorkerDurableCacheLeaseView {
                        reservation_id: lease.reservation_id().clone(),
                        reservation_generation: lease.reservation_generation(),
                        acquired_at: lease.acquired_at(),
                    },
                    evidence_digest: tombstone.evidence_digest().clone(),
                }),
            },
        });
    }

    Err(PersonalWorkerReadError::new(
        PersonalWorkerReadErrorKind::NotFound,
        "personal worker job is not provable in the exact durable snapshot",
    ))
}

fn validate_snapshot(
    document: &PersonalWorkerStoreDocument,
    expected_revision: PersonalWorkerStoreRevision,
    expected_queue_generation: PersonalWorkerQueueGeneration,
) -> Result<(), PersonalWorkerReadError> {
    if document.revision() != expected_revision {
        return Err(PersonalWorkerReadError::new(
            PersonalWorkerReadErrorKind::StaleRevision,
            "personal worker read revision does not match the exact durable snapshot",
        ));
    }
    if document.queue().generation != expected_queue_generation {
        return Err(PersonalWorkerReadError::new(
            PersonalWorkerReadErrorKind::StaleQueueGeneration,
            "personal worker read generation does not match the exact durable snapshot",
        ));
    }
    Ok(())
}

fn bounded_count(value: usize) -> Result<u32, PersonalWorkerReadError> {
    u32::try_from(value).map_err(|_| PersonalWorkerReadError::invalid_document())
}
