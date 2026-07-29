use std::fmt;

use serde::Serialize;

use crate::execution_admission::{
    EpochMillis, ExecutionAdmissionRecord, ExecutionAdmissionState, ExecutionRequestId,
};
use crate::personal_worker_queue::{
    PersonalWorkerActiveReservation, PersonalWorkerCancellationState, PersonalWorkerJobRequest,
    PersonalWorkerPendingProfileChange, PersonalWorkerProfile, PersonalWorkerQueueGeneration,
    PersonalWorkerQueueInput,
};
use crate::personal_worker_store::{
    MAX_PERSONAL_WORKER_TERMINAL_TOMBSTONES, PersonalWorkerDurableCacheLease, PersonalWorkerStore,
    PersonalWorkerStoreDocument, PersonalWorkerStoreError, PersonalWorkerStoreErrorKind,
    PersonalWorkerStoreRevision, PersonalWorkerStoreWriteDisposition,
    PersonalWorkerTerminalTombstone,
};

pub const PERSONAL_WORKER_STORE_TRANSACTION_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerStoreMutationClass {
    Submit,
    Cancel,
    RecordReservationAndAcquireCacheLease,
    MarkStarting,
    MarkRunning,
    MarkDraining,
    ReleaseCompletionAndCacheLease,
    SetProfileIntent,
    CancelProfileIntent,
    UpdateLastActivity,
}

#[derive(Clone)]
pub enum PersonalWorkerStoreMutation {
    Submit {
        request: PersonalWorkerJobRequest,
        observed_at: EpochMillis,
    },
    Cancel {
        request_id: ExecutionRequestId,
        cancelled_at: EpochMillis,
        draining_admission: Option<ExecutionAdmissionRecord>,
    },
    RecordReservationAndAcquireCacheLease {
        request_id: ExecutionRequestId,
        admission: ExecutionAdmissionRecord,
        cache_lease: PersonalWorkerDurableCacheLease,
    },
    MarkStarting {
        request_id: ExecutionRequestId,
        admission: ExecutionAdmissionRecord,
        started_at: EpochMillis,
    },
    MarkRunning {
        request_id: ExecutionRequestId,
        admission: ExecutionAdmissionRecord,
    },
    MarkDraining {
        request_id: ExecutionRequestId,
        admission: ExecutionAdmissionRecord,
    },
    ReleaseCompletionAndCacheLease {
        request_id: ExecutionRequestId,
        terminal_admission: ExecutionAdmissionRecord,
    },
    SetProfileIntent {
        target: PersonalWorkerProfile,
        requested_at: EpochMillis,
        observed_at: EpochMillis,
    },
    CancelProfileIntent {
        observed_at: EpochMillis,
    },
    UpdateLastActivity {
        last_activity_at: EpochMillis,
        observed_at: EpochMillis,
    },
}

impl PersonalWorkerStoreMutation {
    #[must_use]
    pub const fn class(&self) -> PersonalWorkerStoreMutationClass {
        match self {
            Self::Submit { .. } => PersonalWorkerStoreMutationClass::Submit,
            Self::Cancel { .. } => PersonalWorkerStoreMutationClass::Cancel,
            Self::RecordReservationAndAcquireCacheLease { .. } => {
                PersonalWorkerStoreMutationClass::RecordReservationAndAcquireCacheLease
            }
            Self::MarkStarting { .. } => PersonalWorkerStoreMutationClass::MarkStarting,
            Self::MarkRunning { .. } => PersonalWorkerStoreMutationClass::MarkRunning,
            Self::MarkDraining { .. } => PersonalWorkerStoreMutationClass::MarkDraining,
            Self::ReleaseCompletionAndCacheLease { .. } => {
                PersonalWorkerStoreMutationClass::ReleaseCompletionAndCacheLease
            }
            Self::SetProfileIntent { .. } => PersonalWorkerStoreMutationClass::SetProfileIntent,
            Self::CancelProfileIntent { .. } => {
                PersonalWorkerStoreMutationClass::CancelProfileIntent
            }
            Self::UpdateLastActivity { .. } => PersonalWorkerStoreMutationClass::UpdateLastActivity,
        }
    }
}

impl fmt::Debug for PersonalWorkerStoreMutation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersonalWorkerStoreMutation")
            .field("class", &self.class())
            .field("private_payload", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerStoreMutationDisposition {
    Applied,
    Duplicate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerStoreMutationReceipt {
    schema_version: u8,
    disposition: PersonalWorkerStoreMutationDisposition,
    mutation: PersonalWorkerStoreMutationClass,
    old_revision: PersonalWorkerStoreRevision,
    new_revision: PersonalWorkerStoreRevision,
    old_queue_generation: PersonalWorkerQueueGeneration,
    new_queue_generation: PersonalWorkerQueueGeneration,
}

impl PersonalWorkerStoreMutationReceipt {
    #[must_use]
    pub const fn disposition(&self) -> PersonalWorkerStoreMutationDisposition {
        self.disposition
    }

    #[must_use]
    pub const fn mutation(&self) -> PersonalWorkerStoreMutationClass {
        self.mutation
    }

    #[must_use]
    pub const fn old_revision(&self) -> PersonalWorkerStoreRevision {
        self.old_revision
    }

    #[must_use]
    pub const fn new_revision(&self) -> PersonalWorkerStoreRevision {
        self.new_revision
    }

    #[must_use]
    pub const fn old_queue_generation(&self) -> PersonalWorkerQueueGeneration {
        self.old_queue_generation
    }

    #[must_use]
    pub const fn new_queue_generation(&self) -> PersonalWorkerQueueGeneration {
        self.new_queue_generation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerStoreMutationErrorKind {
    MissingState,
    StaleRevision,
    StaleQueueGeneration,
    NotFound,
    Conflict,
    InvalidMutation,
    Busy,
    Io,
    UnsafeFilesystem,
    CorruptState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerStoreMutationError {
    kind: PersonalWorkerStoreMutationErrorKind,
    public_message: &'static str,
}

impl PersonalWorkerStoreMutationError {
    #[must_use]
    pub const fn kind(&self) -> PersonalWorkerStoreMutationErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn message(&self) -> &'static str {
        self.public_message
    }

    const fn new(kind: PersonalWorkerStoreMutationErrorKind, message: &'static str) -> Self {
        Self {
            kind,
            public_message: message,
        }
    }

    const fn invalid(message: &'static str) -> Self {
        Self::new(
            PersonalWorkerStoreMutationErrorKind::InvalidMutation,
            message,
        )
    }

    const fn conflict(message: &'static str) -> Self {
        Self::new(PersonalWorkerStoreMutationErrorKind::Conflict, message)
    }

    const fn not_found() -> Self {
        Self::new(
            PersonalWorkerStoreMutationErrorKind::NotFound,
            "personal worker mutation target does not exist",
        )
    }
}

impl fmt::Display for PersonalWorkerStoreMutationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.public_message)
    }
}

impl std::error::Error for PersonalWorkerStoreMutationError {}

/// Recover, validate, and atomically apply one typed durable personal-worker mutation.
///
/// The caller must supply the exact current store revision and queue generation. Exact duplicate
/// intents return a bounded duplicate receipt without advancing durable state. Applied mutations
/// advance both counters exactly once and publish through the store's revision-checked cooperative-writer boundary.
///
/// # Errors
///
/// Returns a bounded error for missing or stale state, identity/state conflicts, invalid transition
/// evidence, lock or filesystem failure, or corrupt durable state.
pub fn apply_personal_worker_store_mutation(
    store: &mut impl PersonalWorkerStore,
    expected_revision: PersonalWorkerStoreRevision,
    expected_queue_generation: PersonalWorkerQueueGeneration,
    mutation: PersonalWorkerStoreMutation,
) -> Result<PersonalWorkerStoreMutationReceipt, PersonalWorkerStoreMutationError> {
    store.recover().map_err(map_store_error)?;
    let current = store.load().map_err(map_store_error)?.ok_or_else(|| {
        PersonalWorkerStoreMutationError::new(
            PersonalWorkerStoreMutationErrorKind::MissingState,
            "durable personal worker state does not exist",
        )
    })?;
    if current.revision() != expected_revision {
        return Err(PersonalWorkerStoreMutationError::new(
            PersonalWorkerStoreMutationErrorKind::StaleRevision,
            "personal worker store revision does not match the exact expected revision",
        ));
    }
    if current.queue().generation != expected_queue_generation {
        return Err(PersonalWorkerStoreMutationError::new(
            PersonalWorkerStoreMutationErrorKind::StaleQueueGeneration,
            "personal worker queue generation does not match the exact expected generation",
        ));
    }

    let class = mutation.class();
    let old_revision = current.revision();
    let old_generation = current.queue().generation;
    let application = apply_to_snapshot(&current, mutation)?;
    let MutationApplication::Applied {
        mut queue,
        leases,
        terminal_tombstones,
        observed_at,
    } = application
    else {
        return Ok(PersonalWorkerStoreMutationReceipt {
            schema_version: PERSONAL_WORKER_STORE_TRANSACTION_SCHEMA_VERSION,
            disposition: PersonalWorkerStoreMutationDisposition::Duplicate,
            mutation: class,
            old_revision,
            new_revision: old_revision,
            old_queue_generation: old_generation,
            new_queue_generation: old_generation,
        });
    };

    if observed_at < queue.observed_at {
        return Err(PersonalWorkerStoreMutationError::invalid(
            "personal worker mutation observation cannot move backwards",
        ));
    }
    queue.observed_at = observed_at;
    queue.generation = queue.generation.next().map_err(|_| {
        PersonalWorkerStoreMutationError::invalid(
            "personal worker queue generation cannot advance for this mutation",
        )
    })?;
    let next = current
        .advance_with_terminal_tombstones(queue, leases, terminal_tombstones)
        .map_err(|_| {
            PersonalWorkerStoreMutationError::invalid(
                "personal worker mutation does not produce a valid durable successor",
            )
        })?;
    let write = store
        .replace_if_revision(expected_revision, &next)
        .map_err(map_store_error)?;
    if write.disposition() != PersonalWorkerStoreWriteDisposition::Replaced
        || write.revision() != next.revision()
    {
        return Err(PersonalWorkerStoreMutationError::new(
            PersonalWorkerStoreMutationErrorKind::CorruptState,
            "personal worker store returned an invalid mutation publication receipt",
        ));
    }

    Ok(PersonalWorkerStoreMutationReceipt {
        schema_version: PERSONAL_WORKER_STORE_TRANSACTION_SCHEMA_VERSION,
        disposition: PersonalWorkerStoreMutationDisposition::Applied,
        mutation: class,
        old_revision,
        new_revision: next.revision(),
        old_queue_generation: old_generation,
        new_queue_generation: next.queue().generation,
    })
}

enum MutationApplication {
    Duplicate,
    Applied {
        queue: PersonalWorkerQueueInput,
        leases: Vec<PersonalWorkerDurableCacheLease>,
        terminal_tombstones: Vec<PersonalWorkerTerminalTombstone>,
        observed_at: EpochMillis,
    },
}

fn apply_to_snapshot(
    current: &PersonalWorkerStoreDocument,
    mutation: PersonalWorkerStoreMutation,
) -> Result<MutationApplication, PersonalWorkerStoreMutationError> {
    let mut queue = current.queue().clone();
    let mut leases = current.cache_leases().to_vec();
    let mut terminal_tombstones = current.terminal_tombstones().to_vec();
    let observed_at = match mutation {
        PersonalWorkerStoreMutation::Submit {
            request,
            observed_at,
        } => {
            if terminal_tombstones.iter().any(|tombstone| {
                tombstone.request().identity.request_id == request.identity.request_id
            }) {
                return Err(PersonalWorkerStoreMutationError::conflict(
                    "personal worker request identity already has durable terminal evidence",
                ));
            }
            if let Some(existing) = queue
                .queued
                .iter()
                .find(|existing| existing.identity.request_id == request.identity.request_id)
                .or_else(|| {
                    queue
                        .active
                        .iter()
                        .map(|active| &active.request)
                        .find(|existing| {
                            existing.identity.request_id == request.identity.request_id
                        })
                })
            {
                return if existing == &request {
                    Ok(MutationApplication::Duplicate)
                } else {
                    Err(PersonalWorkerStoreMutationError::conflict(
                        "personal worker request identity is already bound to different semantics",
                    ))
                };
            }
            queue.queued.push(request);
            observed_at
        }
        PersonalWorkerStoreMutation::Cancel {
            request_id,
            cancelled_at,
            draining_admission,
        } => {
            if let Some(index) = queued_index(&queue, &request_id) {
                if draining_admission.is_some() {
                    return Err(PersonalWorkerStoreMutationError::invalid(
                        "queued cancellation must not include active drain evidence",
                    ));
                }
                match queue.queued[index].cancellation {
                    PersonalWorkerCancellationState::Active => {
                        queue.queued[index].cancellation =
                            PersonalWorkerCancellationState::Cancelled { cancelled_at };
                    }
                    PersonalWorkerCancellationState::Cancelled {
                        cancelled_at: existing,
                    } if existing == cancelled_at => return Ok(MutationApplication::Duplicate),
                    PersonalWorkerCancellationState::Cancelled { .. } => {
                        return Err(PersonalWorkerStoreMutationError::conflict(
                            "personal worker cancellation identity is already bound to different evidence",
                        ));
                    }
                }
                cancelled_at
            } else if let Some(index) = active_index(&queue, &request_id) {
                let next = draining_admission.ok_or_else(|| {
                    PersonalWorkerStoreMutationError::invalid(
                        "active cancellation requires exact draining admission evidence",
                    )
                })?;
                if next.state() != ExecutionAdmissionState::Draining {
                    return Err(PersonalWorkerStoreMutationError::invalid(
                        "active cancellation admission must be draining",
                    ));
                }
                let active = &mut queue.active[index];
                if let PersonalWorkerCancellationState::Cancelled {
                    cancelled_at: existing,
                } = active.request.cancellation
                {
                    if existing == cancelled_at && active.admission == next {
                        return Ok(MutationApplication::Duplicate);
                    }
                    if existing != cancelled_at {
                        return Err(PersonalWorkerStoreMutationError::conflict(
                            "personal worker cancellation identity is already bound to different evidence",
                        ));
                    }
                }
                let transition = active.admission.plan_transition(next).map_err(|_| {
                    PersonalWorkerStoreMutationError::invalid(
                        "active cancellation violates admission transition ordering",
                    )
                })?;
                active.request.cancellation =
                    PersonalWorkerCancellationState::Cancelled { cancelled_at };
                active.admission = transition.resulting_record().clone();
                active.admission.observed_at()
            } else {
                return Err(PersonalWorkerStoreMutationError::not_found());
            }
        }
        PersonalWorkerStoreMutation::RecordReservationAndAcquireCacheLease {
            request_id,
            admission,
            cache_lease,
        } => {
            if admission.state() != ExecutionAdmissionState::Reserved {
                return Err(PersonalWorkerStoreMutationError::invalid(
                    "reservation mutation requires reserved admission evidence",
                ));
            }
            if let Some(index) = active_index(&queue, &request_id) {
                let active = &queue.active[index];
                let exact_lease = leases
                    .iter()
                    .find(|lease| lease.request_id() == &request_id);
                return if active.admission == admission
                    && active.started_at.is_none()
                    && exact_lease == Some(&cache_lease)
                {
                    Ok(MutationApplication::Duplicate)
                } else {
                    Err(PersonalWorkerStoreMutationError::conflict(
                        "personal worker reservation identity is already bound to different evidence",
                    ))
                };
            }
            let index = queued_index(&queue, &request_id)
                .ok_or_else(PersonalWorkerStoreMutationError::not_found)?;
            let request = queue.queued.remove(index);
            if request.cancellation.is_cancelled() {
                return Err(PersonalWorkerStoreMutationError::invalid(
                    "cancelled queued work cannot acquire a reservation",
                ));
            }
            if admission.identity() != &request.identity || cache_lease.request_id() != &request_id
            {
                return Err(PersonalWorkerStoreMutationError::conflict(
                    "reservation mutation identity does not match the queued request",
                ));
            }
            queue.active.push(PersonalWorkerActiveReservation {
                request,
                admission,
                started_at: None,
            });
            leases.push(cache_lease);
            queue
                .active
                .last()
                .expect("active reservation was pushed")
                .admission
                .observed_at()
        }
        PersonalWorkerStoreMutation::MarkStarting {
            request_id,
            admission,
            started_at,
        } => {
            if admission.state() != ExecutionAdmissionState::Starting {
                return Err(PersonalWorkerStoreMutationError::invalid(
                    "starting mutation requires starting admission evidence",
                ));
            }
            let active = active_mut(&mut queue, &request_id)?;
            if active.admission == admission && active.started_at == Some(started_at) {
                return Ok(MutationApplication::Duplicate);
            }
            if active
                .started_at
                .is_some_and(|existing| existing != started_at)
            {
                return Err(PersonalWorkerStoreMutationError::conflict(
                    "personal worker start time is already bound to different evidence",
                ));
            }
            let transition = active.admission.plan_transition(admission).map_err(|_| {
                PersonalWorkerStoreMutationError::invalid(
                    "starting mutation violates admission transition ordering",
                )
            })?;
            active.admission = transition.resulting_record().clone();
            active.started_at = Some(started_at);
            active.admission.observed_at()
        }
        PersonalWorkerStoreMutation::MarkRunning {
            request_id,
            admission,
        } => match apply_active_transition(
            &mut queue,
            &request_id,
            admission,
            ExecutionAdmissionState::Running,
            "running mutation violates admission transition ordering",
        )? {
            Some(observed_at) => observed_at,
            None => return Ok(MutationApplication::Duplicate),
        },
        PersonalWorkerStoreMutation::MarkDraining {
            request_id,
            admission,
        } => match apply_active_transition(
            &mut queue,
            &request_id,
            admission,
            ExecutionAdmissionState::Draining,
            "draining mutation violates admission transition ordering",
        )? {
            Some(observed_at) => observed_at,
            None => return Ok(MutationApplication::Duplicate),
        },
        PersonalWorkerStoreMutation::ReleaseCompletionAndCacheLease {
            request_id,
            terminal_admission,
        } => {
            if terminal_admission.state() != ExecutionAdmissionState::Unavailable {
                return Err(PersonalWorkerStoreMutationError::invalid(
                    "release mutation requires terminal unavailable admission evidence",
                ));
            }
            if terminal_admission.identity().request_id != request_id {
                return Err(PersonalWorkerStoreMutationError::conflict(
                    "release mutation request identity does not match terminal admission evidence",
                ));
            }
            let Some(index) = active_index(&queue, &request_id) else {
                if let Some(existing) = terminal_tombstones
                    .iter()
                    .find(|tombstone| tombstone.request().identity.request_id == request_id)
                {
                    return if existing.terminal_admission() == &terminal_admission {
                        Ok(MutationApplication::Duplicate)
                    } else {
                        Err(PersonalWorkerStoreMutationError::conflict(
                            "personal worker terminal identity is already bound to different evidence",
                        ))
                    };
                }
                return Err(PersonalWorkerStoreMutationError::not_found());
            };
            let active = queue.active[index].clone();
            let transition = active
                .admission
                .plan_transition(terminal_admission)
                .map_err(|_| {
                    PersonalWorkerStoreMutationError::invalid(
                        "release mutation violates admission transition ordering",
                    )
                })?;
            let lease_index = leases
                .iter()
                .position(|lease| lease.request_id() == &request_id)
                .ok_or_else(|| {
                    PersonalWorkerStoreMutationError::new(
                        PersonalWorkerStoreMutationErrorKind::CorruptState,
                        "active personal worker reservation is missing its durable cache lease",
                    )
                })?;
            let lease = leases[lease_index].clone();
            let tombstone = PersonalWorkerTerminalTombstone::new(
                active.request,
                transition.resulting_record().clone(),
                active.started_at,
                lease,
            )
            .map_err(|_| {
                PersonalWorkerStoreMutationError::invalid(
                    "release mutation does not produce valid durable terminal evidence",
                )
            })?;
            let observed_at = tombstone.completed_at();
            queue.active.remove(index);
            leases.remove(lease_index);
            terminal_tombstones.push(tombstone);
            if terminal_tombstones.len() > MAX_PERSONAL_WORKER_TERMINAL_TOMBSTONES {
                terminal_tombstones.remove(0);
            }
            observed_at
        }
        PersonalWorkerStoreMutation::SetProfileIntent {
            target,
            requested_at,
            observed_at,
        } => {
            let next = PersonalWorkerPendingProfileChange {
                target,
                requested_at,
            };
            if queue.pending_profile_change == Some(next) {
                return Ok(MutationApplication::Duplicate);
            }
            if queue
                .pending_profile_change
                .is_some_and(|current| current.requested_at > requested_at)
            {
                return Err(PersonalWorkerStoreMutationError::invalid(
                    "profile intent evidence cannot move backwards",
                ));
            }
            queue.pending_profile_change = Some(next);
            observed_at
        }
        PersonalWorkerStoreMutation::CancelProfileIntent { observed_at } => {
            if queue.pending_profile_change.is_none() {
                return Ok(MutationApplication::Duplicate);
            }
            queue.pending_profile_change = None;
            observed_at
        }
        PersonalWorkerStoreMutation::UpdateLastActivity {
            last_activity_at,
            observed_at,
        } => {
            if last_activity_at < queue.last_activity_at {
                return Err(PersonalWorkerStoreMutationError::invalid(
                    "last-activity evidence cannot move backwards",
                ));
            }
            if last_activity_at == queue.last_activity_at && observed_at == queue.observed_at {
                return Ok(MutationApplication::Duplicate);
            }
            queue.last_activity_at = last_activity_at;
            observed_at
        }
    };

    Ok(MutationApplication::Applied {
        queue,
        leases,
        terminal_tombstones,
        observed_at,
    })
}

fn apply_active_transition(
    queue: &mut PersonalWorkerQueueInput,
    request_id: &ExecutionRequestId,
    admission: ExecutionAdmissionRecord,
    required_state: ExecutionAdmissionState,
    invalid_message: &'static str,
) -> Result<Option<EpochMillis>, PersonalWorkerStoreMutationError> {
    if admission.state() != required_state {
        return Err(PersonalWorkerStoreMutationError::invalid(
            "personal worker mutation carries the wrong admission state",
        ));
    }
    let active = active_mut(queue, request_id)?;
    if active.admission == admission {
        return Ok(None);
    }
    let transition = active
        .admission
        .plan_transition(admission)
        .map_err(|_| PersonalWorkerStoreMutationError::invalid(invalid_message))?;
    active.admission = transition.resulting_record().clone();
    Ok(Some(active.admission.observed_at()))
}

fn queued_index(
    queue: &PersonalWorkerQueueInput,
    request_id: &ExecutionRequestId,
) -> Option<usize> {
    queue
        .queued
        .iter()
        .position(|request| &request.identity.request_id == request_id)
}

fn active_index(
    queue: &PersonalWorkerQueueInput,
    request_id: &ExecutionRequestId,
) -> Option<usize> {
    queue
        .active
        .iter()
        .position(|active| &active.request.identity.request_id == request_id)
}

fn active_mut<'a>(
    queue: &'a mut PersonalWorkerQueueInput,
    request_id: &ExecutionRequestId,
) -> Result<&'a mut PersonalWorkerActiveReservation, PersonalWorkerStoreMutationError> {
    let index =
        active_index(queue, request_id).ok_or_else(PersonalWorkerStoreMutationError::not_found)?;
    Ok(&mut queue.active[index])
}

fn map_store_error(error: PersonalWorkerStoreError) -> PersonalWorkerStoreMutationError {
    match error.kind() {
        PersonalWorkerStoreErrorKind::RevisionConflict => PersonalWorkerStoreMutationError::new(
            PersonalWorkerStoreMutationErrorKind::StaleRevision,
            "durable personal worker state changed before mutation publication",
        ),
        PersonalWorkerStoreErrorKind::Busy => PersonalWorkerStoreMutationError::new(
            PersonalWorkerStoreMutationErrorKind::Busy,
            "durable personal worker state is busy",
        ),
        PersonalWorkerStoreErrorKind::Missing => PersonalWorkerStoreMutationError::new(
            PersonalWorkerStoreMutationErrorKind::MissingState,
            "durable personal worker state does not exist",
        ),
        PersonalWorkerStoreErrorKind::Io => PersonalWorkerStoreMutationError::new(
            PersonalWorkerStoreMutationErrorKind::Io,
            "durable personal worker mutation could not be read or published",
        ),
        PersonalWorkerStoreErrorKind::UnsafeFilesystem => PersonalWorkerStoreMutationError::new(
            PersonalWorkerStoreMutationErrorKind::UnsafeFilesystem,
            "durable personal worker state contains an unsafe filesystem object",
        ),
        PersonalWorkerStoreErrorKind::VersionIncompatible
        | PersonalWorkerStoreErrorKind::CorruptState => PersonalWorkerStoreMutationError::new(
            PersonalWorkerStoreMutationErrorKind::CorruptState,
            "durable personal worker state is corrupt or noncanonical",
        ),
        PersonalWorkerStoreErrorKind::InvalidDocument => PersonalWorkerStoreMutationError::invalid(
            "personal worker mutation does not produce a valid durable document",
        ),
    }
}
