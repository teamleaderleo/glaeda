use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use crate::execution_admission::{
    DrainAcknowledgement, EXECUTION_ADMISSION_SCHEMA_VERSION, EpochMillis,
    ExecutionAdmissionIdentity, ExecutionAdmissionInput, ExecutionAdmissionRecord,
    ExecutionAdmissionState, ExecutionRequestId, ExecutionResourceLimits,
    FallbackProfileEligibility, HostCapacityObservation, QueuePosition, ReservationEvidence,
    ReservationGeneration, ReservationId, RunnerProfileId, UnavailableReason,
};
use crate::personal_worker_queue::{
    PersonalWorkerActiveReservation, PersonalWorkerCacheAccessMode, PersonalWorkerCacheNamespace,
    PersonalWorkerCancellationState, PersonalWorkerJobRequest, PersonalWorkerPendingProfileChange,
    PersonalWorkerPriority, PersonalWorkerProfile, PersonalWorkerQueueGeneration,
    PersonalWorkerQueueInput, PersonalWorkerSourceIdentity, evaluate_personal_worker_queue,
};
use crate::verification_profile::{CacheId, VerificationProfileId};

pub const PERSONAL_WORKER_STORE_SCHEMA_VERSION: u8 = 1;
pub const MAX_PERSONAL_WORKER_STORE_BYTES: usize = 1_048_576;
pub const MAX_PERSONAL_WORKER_HISTORY_ENTRIES: usize = 32;
pub const MAX_PERSONAL_WORKER_TERMINAL_TOMBSTONES: usize = 32;
const MAX_PERSONAL_WORKER_STORE_REVISION: u64 = 1_000_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct PersonalWorkerStoreRevision(u64);

impl PersonalWorkerStoreRevision {
    pub fn new(value: u64) -> Result<Self, PersonalWorkerStoreError> {
        if !(1..=MAX_PERSONAL_WORKER_STORE_REVISION).contains(&value) {
            return Err(PersonalWorkerStoreError::invalid_document(
                "personal worker store revision is outside the bounded positive range",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    pub fn next(self) -> Result<Self, PersonalWorkerStoreError> {
        let next = self
            .0
            .checked_add(1)
            .filter(|value| *value <= MAX_PERSONAL_WORKER_STORE_REVISION)
            .ok_or_else(|| {
                PersonalWorkerStoreError::revision_conflict(
                    "personal worker store revision space is exhausted",
                )
            })?;
        Ok(Self(next))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonalWorkerDurableCacheLease {
    request_id: ExecutionRequestId,
    namespace: PersonalWorkerCacheNamespace,
    access: PersonalWorkerCacheAccessMode,
    reservation_id: ReservationId,
    reservation_generation: ReservationGeneration,
    acquired_at: EpochMillis,
}

impl PersonalWorkerDurableCacheLease {
    #[must_use]
    pub const fn new(
        request_id: ExecutionRequestId,
        namespace: PersonalWorkerCacheNamespace,
        access: PersonalWorkerCacheAccessMode,
        reservation_id: ReservationId,
        reservation_generation: ReservationGeneration,
        acquired_at: EpochMillis,
    ) -> Self {
        Self {
            request_id,
            namespace,
            access,
            reservation_id,
            reservation_generation,
            acquired_at,
        }
    }

    #[must_use]
    pub const fn request_id(&self) -> &ExecutionRequestId {
        &self.request_id
    }

    #[must_use]
    pub const fn namespace(&self) -> &PersonalWorkerCacheNamespace {
        &self.namespace
    }

    #[must_use]
    pub const fn access(&self) -> PersonalWorkerCacheAccessMode {
        self.access
    }

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerTerminalMutationClass {
    ReleaseCompletionAndCacheLease,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonalWorkerTerminalTombstone {
    mutation_class: PersonalWorkerTerminalMutationClass,
    request: PersonalWorkerJobRequest,
    terminal_admission: ExecutionAdmissionRecord,
    started_at: Option<EpochMillis>,
    cache_lease: PersonalWorkerDurableCacheLease,
    evidence_digest: Sha256Digest,
}

impl PersonalWorkerTerminalTombstone {
    pub fn new(
        request: PersonalWorkerJobRequest,
        terminal_admission: ExecutionAdmissionRecord,
        started_at: Option<EpochMillis>,
        cache_lease: PersonalWorkerDurableCacheLease,
    ) -> Result<Self, PersonalWorkerStoreError> {
        let mutation_class = PersonalWorkerTerminalMutationClass::ReleaseCompletionAndCacheLease;
        let evidence_digest = terminal_evidence_digest(
            mutation_class,
            &request,
            &terminal_admission,
            started_at,
            &cache_lease,
        )?;
        let tombstone = Self {
            mutation_class,
            request,
            terminal_admission,
            started_at,
            cache_lease,
            evidence_digest,
        };
        tombstone.validate()?;
        Ok(tombstone)
    }

    #[must_use]
    pub const fn mutation_class(&self) -> PersonalWorkerTerminalMutationClass {
        self.mutation_class
    }

    #[must_use]
    pub const fn request(&self) -> &PersonalWorkerJobRequest {
        &self.request
    }

    #[must_use]
    pub const fn terminal_admission(&self) -> &ExecutionAdmissionRecord {
        &self.terminal_admission
    }

    #[must_use]
    pub const fn started_at(&self) -> Option<EpochMillis> {
        self.started_at
    }

    #[must_use]
    pub const fn cache_lease(&self) -> &PersonalWorkerDurableCacheLease {
        &self.cache_lease
    }

    #[must_use]
    pub const fn evidence_digest(&self) -> &Sha256Digest {
        &self.evidence_digest
    }

    #[must_use]
    pub const fn completed_at(&self) -> EpochMillis {
        self.terminal_admission.observed_at()
    }

    fn validate(&self) -> Result<(), PersonalWorkerStoreError> {
        if self.terminal_admission.state() != ExecutionAdmissionState::Unavailable {
            return Err(PersonalWorkerStoreError::invalid_document(
                "terminal tombstone requires unavailable admission evidence",
            ));
        }
        if self.terminal_admission.identity() != &self.request.identity
            || self.cache_lease.request_id() != &self.request.identity.request_id
        {
            return Err(PersonalWorkerStoreError::invalid_document(
                "terminal tombstone request identity does not match its evidence",
            ));
        }
        if self.terminal_admission.requested_limits() != self.request.requested_limits
            || self.terminal_admission.fallback_eligibility() != &self.request.fallback_eligibility
        {
            return Err(PersonalWorkerStoreError::invalid_document(
                "terminal tombstone admission semantics drift from the exact request",
            ));
        }
        if self.cache_lease.namespace() != &self.request.cache_namespace
            || self.cache_lease.access() != self.request.cache_access
        {
            return Err(PersonalWorkerStoreError::invalid_document(
                "terminal tombstone cache lease does not match the exact request",
            ));
        }
        let reservation = self.terminal_admission.reservation().ok_or_else(|| {
            PersonalWorkerStoreError::invalid_document(
                "terminal tombstone requires exact reservation evidence",
            )
        })?;
        if self.cache_lease.reservation_id() != &reservation.id
            || self.cache_lease.reservation_generation() != reservation.generation
        {
            return Err(PersonalWorkerStoreError::invalid_document(
                "terminal tombstone cache lease is bound to different reservation evidence",
            ));
        }
        if self.cache_lease.acquired_at() < reservation.reserved_at
            || self.cache_lease.acquired_at() > self.terminal_admission.observed_at()
        {
            return Err(PersonalWorkerStoreError::invalid_document(
                "terminal tombstone cache lease time is outside reservation evidence",
            ));
        }
        if self.started_at.is_some_and(|started_at| {
            started_at < reservation.reserved_at
                || started_at > self.terminal_admission.observed_at()
        }) {
            return Err(PersonalWorkerStoreError::invalid_document(
                "terminal tombstone start time is outside reservation evidence",
            ));
        }
        let expected = terminal_evidence_digest(
            self.mutation_class,
            &self.request,
            &self.terminal_admission,
            self.started_at,
            &self.cache_lease,
        )?;
        if expected != self.evidence_digest {
            return Err(PersonalWorkerStoreError::invalid_document(
                "terminal tombstone evidence digest does not match its exact evidence",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerHistoryEntry {
    revision: PersonalWorkerStoreRevision,
    queue_generation: PersonalWorkerQueueGeneration,
    observed_at: EpochMillis,
    queued_count: u32,
    active_count: u32,
    cache_lease_count: u32,
    terminal_tombstone_count: u32,
    state_digest: Sha256Digest,
}

impl PersonalWorkerHistoryEntry {
    #[must_use]
    pub const fn revision(&self) -> PersonalWorkerStoreRevision {
        self.revision
    }

    #[must_use]
    pub const fn queue_generation(&self) -> PersonalWorkerQueueGeneration {
        self.queue_generation
    }

    #[must_use]
    pub const fn observed_at(&self) -> EpochMillis {
        self.observed_at
    }

    #[must_use]
    pub const fn state_digest(&self) -> &Sha256Digest {
        &self.state_digest
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonalWorkerStoreDocument {
    schema_version: u8,
    revision: PersonalWorkerStoreRevision,
    queue: PersonalWorkerQueueInput,
    cache_leases: Vec<PersonalWorkerDurableCacheLease>,
    terminal_tombstones: Vec<PersonalWorkerTerminalTombstone>,
    history: Vec<PersonalWorkerHistoryEntry>,
}

impl PersonalWorkerStoreDocument {
    pub fn new(
        queue: PersonalWorkerQueueInput,
        cache_leases: Vec<PersonalWorkerDurableCacheLease>,
    ) -> Result<Self, PersonalWorkerStoreError> {
        Self::from_parts(
            PersonalWorkerStoreRevision::new(1)?,
            queue,
            cache_leases,
            Vec::new(),
            Vec::new(),
        )
    }

    pub fn new_with_terminal_tombstones(
        queue: PersonalWorkerQueueInput,
        cache_leases: Vec<PersonalWorkerDurableCacheLease>,
        terminal_tombstones: Vec<PersonalWorkerTerminalTombstone>,
    ) -> Result<Self, PersonalWorkerStoreError> {
        Self::from_parts(
            PersonalWorkerStoreRevision::new(1)?,
            queue,
            cache_leases,
            terminal_tombstones,
            Vec::new(),
        )
    }

    pub fn advance(
        &self,
        queue: PersonalWorkerQueueInput,
        cache_leases: Vec<PersonalWorkerDurableCacheLease>,
    ) -> Result<Self, PersonalWorkerStoreError> {
        self.advance_with_terminal_tombstones(queue, cache_leases, self.terminal_tombstones.clone())
    }

    pub fn advance_with_terminal_tombstones(
        &self,
        queue: PersonalWorkerQueueInput,
        cache_leases: Vec<PersonalWorkerDurableCacheLease>,
        terminal_tombstones: Vec<PersonalWorkerTerminalTombstone>,
    ) -> Result<Self, PersonalWorkerStoreError> {
        let expected_generation = next_queue_generation(self.queue.generation)?;
        if queue.generation != expected_generation {
            return Err(PersonalWorkerStoreError::revision_conflict(
                "personal worker queue generation must advance exactly once",
            ));
        }
        if queue.observed_at < self.queue.observed_at {
            return Err(PersonalWorkerStoreError::revision_conflict(
                "personal worker store observation cannot move backwards",
            ));
        }
        validate_terminal_tombstone_ledger_shape(&self.terminal_tombstones, &terminal_tombstones)?;
        let mut history = self.history.clone();
        history.push(self.summary()?);
        if history.len() > MAX_PERSONAL_WORKER_HISTORY_ENTRIES {
            history.remove(0);
        }
        Self::from_parts(
            self.revision.next()?,
            queue,
            cache_leases,
            terminal_tombstones,
            history,
        )
    }

    fn from_parts(
        revision: PersonalWorkerStoreRevision,
        queue: PersonalWorkerQueueInput,
        cache_leases: Vec<PersonalWorkerDurableCacheLease>,
        terminal_tombstones: Vec<PersonalWorkerTerminalTombstone>,
        history: Vec<PersonalWorkerHistoryEntry>,
    ) -> Result<Self, PersonalWorkerStoreError> {
        let document = Self {
            schema_version: PERSONAL_WORKER_STORE_SCHEMA_VERSION,
            revision,
            queue,
            cache_leases,
            terminal_tombstones,
            history,
        };
        document.validate()?;
        Ok(document)
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn revision(&self) -> PersonalWorkerStoreRevision {
        self.revision
    }

    #[must_use]
    pub const fn queue(&self) -> &PersonalWorkerQueueInput {
        &self.queue
    }

    #[must_use]
    pub fn cache_leases(&self) -> &[PersonalWorkerDurableCacheLease] {
        &self.cache_leases
    }

    #[must_use]
    pub fn terminal_tombstones(&self) -> &[PersonalWorkerTerminalTombstone] {
        &self.terminal_tombstones
    }

    #[must_use]
    pub fn history(&self) -> &[PersonalWorkerHistoryEntry] {
        &self.history
    }

    pub fn validate_successor_of(&self, previous: &Self) -> Result<(), PersonalWorkerStoreError> {
        if self.revision != previous.revision.next()? {
            return Err(PersonalWorkerStoreError::revision_conflict(
                "replacement store revision must advance exactly once",
            ));
        }
        let expected_generation = next_queue_generation(previous.queue.generation)?;
        if self.queue.generation != expected_generation {
            return Err(PersonalWorkerStoreError::revision_conflict(
                "replacement queue generation must advance exactly once",
            ));
        }
        if self.queue.observed_at < previous.queue.observed_at {
            return Err(PersonalWorkerStoreError::revision_conflict(
                "replacement observation cannot move backwards",
            ));
        }
        validate_terminal_tombstone_successor(previous, self)?;
        let mut expected_history = previous.history.clone();
        expected_history.push(previous.summary()?);
        if expected_history.len() > MAX_PERSONAL_WORKER_HISTORY_ENTRIES {
            expected_history.remove(0);
        }
        if self.history != expected_history {
            return Err(PersonalWorkerStoreError::revision_conflict(
                "replacement history does not extend the exact persisted revision",
            ));
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), PersonalWorkerStoreError> {
        if self.schema_version != PERSONAL_WORKER_STORE_SCHEMA_VERSION {
            return Err(PersonalWorkerStoreError::invalid_document(
                "personal worker store schema version is unsupported",
            ));
        }
        evaluate_personal_worker_queue(&self.queue, None).map_err(|_| {
            PersonalWorkerStoreError::invalid_document(
                "personal worker queue state is semantically invalid",
            )
        })?;
        validate_cache_leases(&self.queue, &self.cache_leases)?;
        validate_terminal_tombstones(&self.queue, &self.terminal_tombstones)?;
        validate_history(
            self.revision,
            self.queue.generation,
            self.queue.observed_at,
            &self.history,
        )
    }

    fn summary(&self) -> Result<PersonalWorkerHistoryEntry, PersonalWorkerStoreError> {
        Ok(PersonalWorkerHistoryEntry {
            revision: self.revision,
            queue_generation: self.queue.generation,
            observed_at: self.queue.observed_at,
            queued_count: bounded_count(self.queue.queued.len())?,
            active_count: bounded_count(self.queue.active.len())?,
            cache_lease_count: bounded_count(self.cache_leases.len())?,
            terminal_tombstone_count: bounded_count(self.terminal_tombstones.len())?,
            state_digest: snapshot_digest(
                &self.queue,
                &self.cache_leases,
                &self.terminal_tombstones,
            )?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerStoreWriteDisposition {
    Created,
    Replaced,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerStoreWriteReceipt {
    disposition: PersonalWorkerStoreWriteDisposition,
    revision: PersonalWorkerStoreRevision,
    bytes_written: usize,
}

impl PersonalWorkerStoreWriteReceipt {
    #[must_use]
    pub const fn new(
        disposition: PersonalWorkerStoreWriteDisposition,
        revision: PersonalWorkerStoreRevision,
        bytes_written: usize,
    ) -> Self {
        Self {
            disposition,
            revision,
            bytes_written,
        }
    }

    #[must_use]
    pub const fn disposition(&self) -> PersonalWorkerStoreWriteDisposition {
        self.disposition
    }

    #[must_use]
    pub const fn revision(&self) -> PersonalWorkerStoreRevision {
        self.revision
    }

    #[must_use]
    pub const fn bytes_written(&self) -> usize {
        self.bytes_written
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerStoreRecoveryDisposition {
    Clean,
    PublishedStaged,
    RemovedStaleStaged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerStoreRecovery {
    disposition: PersonalWorkerStoreRecoveryDisposition,
    revision: Option<PersonalWorkerStoreRevision>,
}

impl PersonalWorkerStoreRecovery {
    #[must_use]
    pub const fn new(
        disposition: PersonalWorkerStoreRecoveryDisposition,
        revision: Option<PersonalWorkerStoreRevision>,
    ) -> Self {
        Self {
            disposition,
            revision,
        }
    }

    #[must_use]
    pub const fn disposition(&self) -> PersonalWorkerStoreRecoveryDisposition {
        self.disposition
    }

    #[must_use]
    pub const fn revision(&self) -> Option<PersonalWorkerStoreRevision> {
        self.revision
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerStoreInitializationDisposition {
    Created,
    AlreadyExists,
    RecoveryRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerStoreInitializationReceipt {
    disposition: PersonalWorkerStoreInitializationDisposition,
    revision: Option<PersonalWorkerStoreRevision>,
    bytes_written: usize,
}

impl PersonalWorkerStoreInitializationReceipt {
    #[must_use]
    pub const fn new(
        disposition: PersonalWorkerStoreInitializationDisposition,
        revision: Option<PersonalWorkerStoreRevision>,
        bytes_written: usize,
    ) -> Self {
        Self {
            disposition,
            revision,
            bytes_written,
        }
    }

    #[must_use]
    pub const fn disposition(&self) -> PersonalWorkerStoreInitializationDisposition {
        self.disposition
    }

    #[must_use]
    pub const fn revision(&self) -> Option<PersonalWorkerStoreRevision> {
        self.revision
    }

    #[must_use]
    pub const fn bytes_written(&self) -> usize {
        self.bytes_written
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerStoreErrorKind {
    InvalidDocument,
    RevisionConflict,
    Busy,
    Missing,
    Io,
    UnsafeFilesystem,
    VersionIncompatible,
    CorruptState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerStoreError {
    kind: PersonalWorkerStoreErrorKind,
    public_message: &'static str,
}

impl PersonalWorkerStoreError {
    #[must_use]
    pub const fn kind(&self) -> PersonalWorkerStoreErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn message(&self) -> &'static str {
        self.public_message
    }

    #[must_use]
    pub const fn new(kind: PersonalWorkerStoreErrorKind, public_message: &'static str) -> Self {
        Self {
            kind,
            public_message,
        }
    }

    const fn invalid_document(message: &'static str) -> Self {
        Self::new(PersonalWorkerStoreErrorKind::InvalidDocument, message)
    }

    const fn revision_conflict(message: &'static str) -> Self {
        Self::new(PersonalWorkerStoreErrorKind::RevisionConflict, message)
    }

    #[must_use]
    pub const fn version_incompatible() -> Self {
        Self::new(
            PersonalWorkerStoreErrorKind::VersionIncompatible,
            "durable personal worker state schema is incompatible",
        )
    }

    #[must_use]
    pub const fn corrupt_state() -> Self {
        Self::new(
            PersonalWorkerStoreErrorKind::CorruptState,
            "durable personal worker state is corrupt or noncanonical",
        )
    }
}

impl fmt::Display for PersonalWorkerStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.public_message)
    }
}

impl std::error::Error for PersonalWorkerStoreError {}

pub trait PersonalWorkerStore {
    fn load(&self) -> Result<Option<PersonalWorkerStoreDocument>, PersonalWorkerStoreError>;

    fn create(
        &mut self,
        document: &PersonalWorkerStoreDocument,
    ) -> Result<PersonalWorkerStoreWriteReceipt, PersonalWorkerStoreError>;

    fn replace_if_revision(
        &mut self,
        expected_revision: PersonalWorkerStoreRevision,
        document: &PersonalWorkerStoreDocument,
    ) -> Result<PersonalWorkerStoreWriteReceipt, PersonalWorkerStoreError>;

    fn recover(&mut self) -> Result<PersonalWorkerStoreRecovery, PersonalWorkerStoreError>;
}

pub fn encode_personal_worker_store_document(
    document: &PersonalWorkerStoreDocument,
) -> Result<Vec<u8>, PersonalWorkerStoreError> {
    document.validate()?;
    let wire = WireDocument::from(document);
    let mut encoded = serde_json::to_vec_pretty(&wire).map_err(|_| {
        PersonalWorkerStoreError::invalid_document(
            "personal worker store document could not be encoded",
        )
    })?;
    encoded.push(b'\n');
    if encoded.len() > MAX_PERSONAL_WORKER_STORE_BYTES {
        return Err(PersonalWorkerStoreError::invalid_document(
            "personal worker store document exceeds the bounded byte limit",
        ));
    }
    Ok(encoded)
}

pub fn decode_personal_worker_store_document(
    bytes: &[u8],
) -> Result<PersonalWorkerStoreDocument, PersonalWorkerStoreError> {
    if bytes.len() > MAX_PERSONAL_WORKER_STORE_BYTES {
        return Err(PersonalWorkerStoreError::corrupt_state());
    }
    let wire: WireDocument =
        serde_json::from_slice(bytes).map_err(|_| PersonalWorkerStoreError::corrupt_state())?;
    let document = PersonalWorkerStoreDocument::try_from(wire)?;
    let canonical = encode_personal_worker_store_document(&document)
        .map_err(|_| PersonalWorkerStoreError::corrupt_state())?;
    if canonical != bytes {
        return Err(PersonalWorkerStoreError::corrupt_state());
    }
    Ok(document)
}

fn next_queue_generation(
    generation: PersonalWorkerQueueGeneration,
) -> Result<PersonalWorkerQueueGeneration, PersonalWorkerStoreError> {
    let next = generation.get().checked_add(1).ok_or_else(|| {
        PersonalWorkerStoreError::revision_conflict(
            "personal worker queue generation space is exhausted",
        )
    })?;
    PersonalWorkerQueueGeneration::new(next).map_err(|_| {
        PersonalWorkerStoreError::revision_conflict(
            "personal worker queue generation space is exhausted",
        )
    })
}

fn validate_cache_leases(
    queue: &PersonalWorkerQueueInput,
    leases: &[PersonalWorkerDurableCacheLease],
) -> Result<(), PersonalWorkerStoreError> {
    if leases.len() != queue.active.len() {
        return Err(PersonalWorkerStoreError::invalid_document(
            "every active reservation must own exactly one durable cache lease",
        ));
    }
    let active = queue
        .active
        .iter()
        .map(|reservation| (reservation.request.identity.request_id.clone(), reservation))
        .collect::<BTreeMap<_, _>>();
    let mut owners = BTreeSet::new();
    let mut held =
        BTreeMap::<PersonalWorkerCacheNamespace, Vec<PersonalWorkerCacheAccessMode>>::new();
    for lease in leases {
        if !owners.insert(lease.request_id.clone()) {
            return Err(PersonalWorkerStoreError::invalid_document(
                "durable cache lease owner is duplicated",
            ));
        }
        let reservation = active.get(&lease.request_id).ok_or_else(|| {
            PersonalWorkerStoreError::invalid_document(
                "durable cache lease owner is not an active reservation",
            )
        })?;
        if lease.namespace != reservation.request.cache_namespace
            || lease.access != reservation.request.cache_access
        {
            return Err(PersonalWorkerStoreError::invalid_document(
                "durable cache lease does not match its active request",
            ));
        }
        let evidence = reservation.admission.reservation().ok_or_else(|| {
            PersonalWorkerStoreError::invalid_document(
                "durable cache lease requires reservation evidence",
            )
        })?;
        if lease.reservation_id != evidence.id
            || lease.reservation_generation != evidence.generation
        {
            return Err(PersonalWorkerStoreError::invalid_document(
                "durable cache lease is bound to different reservation evidence",
            ));
        }
        if lease.acquired_at < evidence.reserved_at
            || lease.acquired_at > reservation.admission.observed_at()
        {
            return Err(PersonalWorkerStoreError::invalid_document(
                "durable cache lease acquisition time is outside reservation evidence",
            ));
        }
        let modes = held.entry(lease.namespace.clone()).or_default();
        if cache_modes_conflict(modes, lease.access) {
            return Err(PersonalWorkerStoreError::invalid_document(
                "durable cache leases conflict for one namespace",
            ));
        }
        modes.push(lease.access);
    }
    Ok(())
}

fn cache_modes_conflict(
    held: &[PersonalWorkerCacheAccessMode],
    requested: PersonalWorkerCacheAccessMode,
) -> bool {
    if held.is_empty() {
        return false;
    }
    held.contains(&PersonalWorkerCacheAccessMode::Exclusive)
        || requested == PersonalWorkerCacheAccessMode::Exclusive
        || held.contains(&PersonalWorkerCacheAccessMode::Write)
        || requested == PersonalWorkerCacheAccessMode::Write
}

fn validate_terminal_tombstone_ledger_shape(
    previous: &[PersonalWorkerTerminalTombstone],
    next: &[PersonalWorkerTerminalTombstone],
) -> Result<(), PersonalWorkerStoreError> {
    if next == previous {
        return Ok(());
    }
    let exact_append = previous.len() < MAX_PERSONAL_WORKER_TERMINAL_TOMBSTONES
        && next.len() == previous.len() + 1
        && next[..previous.len()] == *previous;
    let exact_fifo_eviction = previous.len() == MAX_PERSONAL_WORKER_TERMINAL_TOMBSTONES
        && next.len() == MAX_PERSONAL_WORKER_TERMINAL_TOMBSTONES
        && next[..MAX_PERSONAL_WORKER_TERMINAL_TOMBSTONES - 1] == previous[1..];
    if exact_append || exact_fifo_eviction {
        return Ok(());
    }
    Err(PersonalWorkerStoreError::revision_conflict(
        "terminal tombstone ledger must remain exact or advance by one FIFO entry",
    ))
}

fn validate_terminal_tombstone_successor(
    previous: &PersonalWorkerStoreDocument,
    next: &PersonalWorkerStoreDocument,
) -> Result<(), PersonalWorkerStoreError> {
    let removed_active = previous
        .queue
        .active
        .iter()
        .filter(|active| {
            !next.queue.active.iter().any(|next_active| {
                next_active.request.identity.request_id == active.request.identity.request_id
            })
        })
        .collect::<Vec<_>>();

    if next.terminal_tombstones == previous.terminal_tombstones {
        if removed_active.is_empty() {
            return Ok(());
        }
        return Err(PersonalWorkerStoreError::revision_conflict(
            "active reservation removal requires exact appended terminal evidence",
        ));
    }

    validate_terminal_tombstone_ledger_shape(
        &previous.terminal_tombstones,
        &next.terminal_tombstones,
    )?;
    let appended = next.terminal_tombstones.last().ok_or_else(|| {
        PersonalWorkerStoreError::revision_conflict(
            "terminal tombstone append is missing exact evidence",
        )
    })?;
    if removed_active.len() != 1
        || removed_active[0].request.identity.request_id != appended.request.identity.request_id
    {
        return Err(PersonalWorkerStoreError::revision_conflict(
            "terminal tombstone append must bind the one exact removed active reservation",
        ));
    }
    let active = removed_active[0];
    if active.request != appended.request || active.started_at != appended.started_at {
        return Err(PersonalWorkerStoreError::revision_conflict(
            "terminal tombstone request or start evidence differs from the removed reservation",
        ));
    }
    let transition = active
        .admission
        .plan_transition(appended.terminal_admission.clone())
        .map_err(|_| {
            PersonalWorkerStoreError::revision_conflict(
                "terminal tombstone admission is not an allowed exact transition",
            )
        })?;
    if transition.resulting_record() != &appended.terminal_admission {
        return Err(PersonalWorkerStoreError::revision_conflict(
            "terminal tombstone admission differs from the exact transition result",
        ));
    }
    let previous_lease = previous
        .cache_leases
        .iter()
        .find(|lease| lease.request_id() == &appended.request.identity.request_id)
        .ok_or_else(|| {
            PersonalWorkerStoreError::revision_conflict(
                "terminal tombstone append requires the exact previous cache lease",
            )
        })?;
    if previous_lease != &appended.cache_lease
        || next
            .cache_leases
            .iter()
            .any(|lease| lease.request_id() == &appended.request.identity.request_id)
    {
        return Err(PersonalWorkerStoreError::revision_conflict(
            "terminal tombstone cache evidence does not match exact lease release",
        ));
    }
    if next.queue.queued != previous.queue.queued
        || next.queue.current_profile != previous.queue.current_profile
        || next.queue.last_activity_at != previous.queue.last_activity_at
        || next.queue.pending_profile_change != previous.queue.pending_profile_change
    {
        return Err(PersonalWorkerStoreError::revision_conflict(
            "terminal release successor changed unrelated queue policy state",
        ));
    }
    if next.queue.observed_at != appended.completed_at() {
        return Err(PersonalWorkerStoreError::revision_conflict(
            "terminal release successor observation must equal exact completion time",
        ));
    }
    let expected_active = previous
        .queue
        .active
        .iter()
        .filter(|candidate| {
            candidate.request.identity.request_id != appended.request.identity.request_id
        })
        .cloned()
        .collect::<Vec<_>>();
    if next.queue.active != expected_active {
        return Err(PersonalWorkerStoreError::revision_conflict(
            "terminal release successor must preserve every retained active reservation exactly",
        ));
    }
    let expected_leases = previous
        .cache_leases
        .iter()
        .filter(|lease| lease.request_id() != &appended.request.identity.request_id)
        .cloned()
        .collect::<Vec<_>>();
    if next.cache_leases != expected_leases {
        return Err(PersonalWorkerStoreError::revision_conflict(
            "terminal release successor must preserve every retained cache lease exactly",
        ));
    }
    Ok(())
}

fn validate_terminal_tombstones(
    queue: &PersonalWorkerQueueInput,
    tombstones: &[PersonalWorkerTerminalTombstone],
) -> Result<(), PersonalWorkerStoreError> {
    if tombstones.len() > MAX_PERSONAL_WORKER_TERMINAL_TOMBSTONES {
        return Err(PersonalWorkerStoreError::invalid_document(
            "personal worker terminal tombstone ledger exceeds its bounded entry limit",
        ));
    }
    let live_ids = queue
        .queued
        .iter()
        .map(|request| request.identity.request_id.clone())
        .chain(
            queue
                .active
                .iter()
                .map(|active| active.request.identity.request_id.clone()),
        )
        .collect::<BTreeSet<_>>();
    let mut terminal_ids = BTreeSet::new();
    let mut previous_completed_at = None;
    for tombstone in tombstones {
        tombstone.validate()?;
        let request_id = tombstone.request.identity.request_id.clone();
        if live_ids.contains(&request_id) {
            return Err(PersonalWorkerStoreError::invalid_document(
                "terminal tombstone request is still present in live queue state",
            ));
        }
        if !terminal_ids.insert(request_id) {
            return Err(PersonalWorkerStoreError::invalid_document(
                "terminal tombstone request identity is duplicated",
            ));
        }
        if previous_completed_at.is_some_and(|previous| tombstone.completed_at() < previous) {
            return Err(PersonalWorkerStoreError::invalid_document(
                "terminal tombstone completion observations move backwards",
            ));
        }
        previous_completed_at = Some(tombstone.completed_at());
    }
    Ok(())
}

fn validate_history(
    revision: PersonalWorkerStoreRevision,
    current_generation: PersonalWorkerQueueGeneration,
    current_observed_at: EpochMillis,
    history: &[PersonalWorkerHistoryEntry],
) -> Result<(), PersonalWorkerStoreError> {
    let retained_revisions = revision.get().saturating_sub(1);
    let expected_len_u64 = retained_revisions.min(MAX_PERSONAL_WORKER_HISTORY_ENTRIES as u64);
    let expected_len = usize::try_from(expected_len_u64).map_err(|_| {
        PersonalWorkerStoreError::invalid_document(
            "personal worker store history length is not representable",
        )
    })?;
    if history.len() != expected_len {
        return Err(PersonalWorkerStoreError::invalid_document(
            "personal worker store history does not cover the bounded revision window",
        ));
    }
    let history_len = u64::try_from(history.len()).map_err(|_| {
        PersonalWorkerStoreError::invalid_document(
            "personal worker store history length is not representable",
        )
    })?;
    let first_revision = revision.get().checked_sub(history_len).ok_or_else(|| {
        PersonalWorkerStoreError::invalid_document(
            "personal worker store history exceeds its current revision",
        )
    })?;
    let mut previous_generation: Option<u64> = None;
    let mut previous_observed_at = None;
    for (index, entry) in history.iter().enumerate() {
        let offset = u64::try_from(index).map_err(|_| {
            PersonalWorkerStoreError::invalid_document(
                "personal worker store history index is not representable",
            )
        })?;
        if entry.revision.get() != first_revision.saturating_add(offset) {
            return Err(PersonalWorkerStoreError::invalid_document(
                "personal worker store history revisions are not consecutive",
            ));
        }
        if let Some(previous) = previous_generation {
            let expected = previous.checked_add(1).ok_or_else(|| {
                PersonalWorkerStoreError::invalid_document(
                    "personal worker store history queue generation is exhausted",
                )
            })?;
            if entry.queue_generation.get() != expected {
                return Err(PersonalWorkerStoreError::invalid_document(
                    "personal worker store history queue generations are not consecutive",
                ));
            }
        }
        if previous_observed_at.is_some_and(|value| entry.observed_at.get() < value) {
            return Err(PersonalWorkerStoreError::invalid_document(
                "personal worker store history observations move backwards",
            ));
        }
        previous_generation = Some(entry.queue_generation.get());
        previous_observed_at = Some(entry.observed_at.get());
    }
    if let Some(last) = history.last() {
        let expected_generation = last
            .queue_generation
            .get()
            .checked_add(1)
            .and_then(|value| PersonalWorkerQueueGeneration::new(value).ok())
            .ok_or_else(|| {
                PersonalWorkerStoreError::invalid_document(
                    "personal worker store current queue generation cannot follow history",
                )
            })?;
        if current_generation != expected_generation {
            return Err(PersonalWorkerStoreError::invalid_document(
                "personal worker store current queue generation does not follow history",
            ));
        }
        if current_observed_at < last.observed_at {
            return Err(PersonalWorkerStoreError::invalid_document(
                "personal worker store current observation predates retained history",
            ));
        }
    }
    Ok(())
}

fn bounded_count(value: usize) -> Result<u32, PersonalWorkerStoreError> {
    u32::try_from(value).map_err(|_| {
        PersonalWorkerStoreError::invalid_document(
            "personal worker store count exceeds the bounded representation",
        )
    })
}

fn snapshot_digest(
    queue: &PersonalWorkerQueueInput,
    leases: &[PersonalWorkerDurableCacheLease],
    terminal_tombstones: &[PersonalWorkerTerminalTombstone],
) -> Result<Sha256Digest, PersonalWorkerStoreError> {
    let wire = WireSnapshot::from_parts(queue, leases, terminal_tombstones);
    let encoded = serde_json::to_vec(&wire).map_err(|_| {
        PersonalWorkerStoreError::invalid_document(
            "personal worker snapshot digest input could not be encoded",
        )
    })?;
    let digest = Sha256::digest(encoded);
    Sha256Digest::parse(&format!("sha256:{digest:x}")).map_err(|_| {
        PersonalWorkerStoreError::invalid_document(
            "personal worker snapshot digest could not be represented",
        )
    })
}

fn terminal_evidence_digest(
    mutation_class: PersonalWorkerTerminalMutationClass,
    request: &PersonalWorkerJobRequest,
    terminal_admission: &ExecutionAdmissionRecord,
    started_at: Option<EpochMillis>,
    cache_lease: &PersonalWorkerDurableCacheLease,
) -> Result<Sha256Digest, PersonalWorkerStoreError> {
    let wire = WireTerminalEvidence::from_parts(
        mutation_class,
        request,
        terminal_admission,
        started_at,
        cache_lease,
    );
    let encoded = serde_json::to_vec(&wire).map_err(|_| {
        PersonalWorkerStoreError::invalid_document(
            "terminal tombstone digest input could not be encoded",
        )
    })?;
    let digest = Sha256::digest(encoded);
    Sha256Digest::parse(&format!("sha256:{digest:x}")).map_err(|_| {
        PersonalWorkerStoreError::invalid_document(
            "terminal tombstone digest could not be represented",
        )
    })
}

macro_rules! wire_enum {
    ($wire:ident, $typed:ty, { $($variant:ident),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum $wire {
            $($variant),+
        }

        impl From<$typed> for $wire {
            fn from(value: $typed) -> Self {
                match value {
                    $(<$typed>::$variant => Self::$variant),+
                }
            }
        }

        impl From<$wire> for $typed {
            fn from(value: $wire) -> Self {
                match value {
                    $($wire::$variant => Self::$variant),+
                }
            }
        }
    };
}

wire_enum!(WirePriority, PersonalWorkerPriority, {
    Background,
    Normal,
    Interactive,
});
wire_enum!(WireProfile, PersonalWorkerProfile, {
    Stopped,
    Interactive,
    Work,
});
wire_enum!(WireCacheAccess, PersonalWorkerCacheAccessMode, {
    Read,
    Write,
    Exclusive,
});
wire_enum!(WireAdmissionState, ExecutionAdmissionState, {
    Requested,
    Admitted,
    Queued,
    Reserved,
    Starting,
    Running,
    Draining,
    Unavailable,
});
wire_enum!(WireAcknowledgement, DrainAcknowledgement, {
    Cancellation,
    Drain,
});
wire_enum!(WireUnavailableReason, UnavailableReason, {
    AdmissionRejected,
    CapacityUnavailable,
    HostUnavailable,
    ReservationExpired,
    Cancelled,
    Drained,
});
wire_enum!(
    WireTerminalMutationClass,
    PersonalWorkerTerminalMutationClass,
    { ReleaseCompletionAndCacheLease }
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireIdentity {
    request_id: String,
    verification_profile_id: String,
    runner_profile_id: String,
}

impl From<&ExecutionAdmissionIdentity> for WireIdentity {
    fn from(value: &ExecutionAdmissionIdentity) -> Self {
        Self {
            request_id: value.request_id.as_str().to_owned(),
            verification_profile_id: value.verification_profile_id.as_str().to_owned(),
            runner_profile_id: value.runner_profile_id.as_str().to_owned(),
        }
    }
}

impl TryFrom<WireIdentity> for ExecutionAdmissionIdentity {
    type Error = PersonalWorkerStoreError;

    fn try_from(value: WireIdentity) -> Result<Self, Self::Error> {
        Ok(Self::new(
            ExecutionRequestId::parse(&value.request_id)
                .map_err(|_| PersonalWorkerStoreError::corrupt_state())?,
            VerificationProfileId::parse(&value.verification_profile_id)
                .map_err(|_| PersonalWorkerStoreError::corrupt_state())?,
            RunnerProfileId::parse(&value.runner_profile_id)
                .map_err(|_| PersonalWorkerStoreError::corrupt_state())?,
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireLimits {
    cpu_millis: u32,
    memory_bytes: u64,
    pids: u32,
}

impl From<ExecutionResourceLimits> for WireLimits {
    fn from(value: ExecutionResourceLimits) -> Self {
        Self {
            cpu_millis: value.cpu_millis,
            memory_bytes: value.memory_bytes,
            pids: value.pids,
        }
    }
}

impl TryFrom<WireLimits> for ExecutionResourceLimits {
    type Error = PersonalWorkerStoreError;

    fn try_from(value: WireLimits) -> Result<Self, Self::Error> {
        Self::new(value.cpu_millis, value.memory_bytes, value.pids)
            .map_err(|_| PersonalWorkerStoreError::corrupt_state())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireSource {
    repository: String,
    commit: String,
    tree: String,
}

impl From<&PersonalWorkerSourceIdentity> for WireSource {
    fn from(value: &PersonalWorkerSourceIdentity) -> Self {
        Self {
            repository: value.repository.as_str().to_owned(),
            commit: value.commit.as_str().to_owned(),
            tree: value.tree.as_str().to_owned(),
        }
    }
}

impl TryFrom<WireSource> for PersonalWorkerSourceIdentity {
    type Error = PersonalWorkerStoreError;

    fn try_from(value: WireSource) -> Result<Self, Self::Error> {
        Ok(Self::new(
            RepositoryRef::parse(&value.repository)
                .map_err(|_| PersonalWorkerStoreError::corrupt_state())?,
            CommitId::parse(&value.commit)
                .map_err(|_| PersonalWorkerStoreError::corrupt_state())?,
            GitTreeId::parse(&value.tree).map_err(|_| PersonalWorkerStoreError::corrupt_state())?,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case", deny_unknown_fields)]
enum WireCacheNamespace {
    RepositoryBuild {
        cache_id: String,
        repository: String,
        namespace_digest: String,
    },
    SharedDownload {
        cache_id: String,
        namespace_digest: String,
    },
}

impl From<&PersonalWorkerCacheNamespace> for WireCacheNamespace {
    fn from(value: &PersonalWorkerCacheNamespace) -> Self {
        match value {
            PersonalWorkerCacheNamespace::RepositoryBuild {
                cache_id,
                repository,
                namespace_digest,
            } => Self::RepositoryBuild {
                cache_id: cache_id.as_str().to_owned(),
                repository: repository.as_str().to_owned(),
                namespace_digest: namespace_digest.as_str().to_owned(),
            },
            PersonalWorkerCacheNamespace::SharedDownload {
                cache_id,
                namespace_digest,
            } => Self::SharedDownload {
                cache_id: cache_id.as_str().to_owned(),
                namespace_digest: namespace_digest.as_str().to_owned(),
            },
        }
    }
}

impl TryFrom<WireCacheNamespace> for PersonalWorkerCacheNamespace {
    type Error = PersonalWorkerStoreError;

    fn try_from(value: WireCacheNamespace) -> Result<Self, Self::Error> {
        match value {
            WireCacheNamespace::RepositoryBuild {
                cache_id,
                repository,
                namespace_digest,
            } => Ok(Self::RepositoryBuild {
                cache_id: CacheId::parse(&cache_id)
                    .map_err(|_| PersonalWorkerStoreError::corrupt_state())?,
                repository: RepositoryRef::parse(&repository)
                    .map_err(|_| PersonalWorkerStoreError::corrupt_state())?,
                namespace_digest: Sha256Digest::parse(&namespace_digest)
                    .map_err(|_| PersonalWorkerStoreError::corrupt_state())?,
            }),
            WireCacheNamespace::SharedDownload {
                cache_id,
                namespace_digest,
            } => Ok(Self::SharedDownload {
                cache_id: CacheId::parse(&cache_id)
                    .map_err(|_| PersonalWorkerStoreError::corrupt_state())?,
                namespace_digest: Sha256Digest::parse(&namespace_digest)
                    .map_err(|_| PersonalWorkerStoreError::corrupt_state())?,
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum WireCancellation {
    Active,
    Cancelled { cancelled_at: u64 },
}

impl From<PersonalWorkerCancellationState> for WireCancellation {
    fn from(value: PersonalWorkerCancellationState) -> Self {
        match value {
            PersonalWorkerCancellationState::Active => Self::Active,
            PersonalWorkerCancellationState::Cancelled { cancelled_at } => Self::Cancelled {
                cancelled_at: cancelled_at.get(),
            },
        }
    }
}

impl TryFrom<WireCancellation> for PersonalWorkerCancellationState {
    type Error = PersonalWorkerStoreError;

    fn try_from(value: WireCancellation) -> Result<Self, Self::Error> {
        match value {
            WireCancellation::Active => Ok(Self::Active),
            WireCancellation::Cancelled { cancelled_at } => Ok(Self::Cancelled {
                cancelled_at: epoch(cancelled_at)?,
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum WireFallback {
    Eligible { runner_profile_id: String },
    Ineligible,
}

impl From<&FallbackProfileEligibility> for WireFallback {
    fn from(value: &FallbackProfileEligibility) -> Self {
        match value {
            FallbackProfileEligibility::Eligible { runner_profile_id } => Self::Eligible {
                runner_profile_id: runner_profile_id.as_str().to_owned(),
            },
            FallbackProfileEligibility::Ineligible => Self::Ineligible,
        }
    }
}

impl TryFrom<WireFallback> for FallbackProfileEligibility {
    type Error = PersonalWorkerStoreError;

    fn try_from(value: WireFallback) -> Result<Self, Self::Error> {
        match value {
            WireFallback::Eligible { runner_profile_id } => Ok(Self::eligible(
                RunnerProfileId::parse(&runner_profile_id)
                    .map_err(|_| PersonalWorkerStoreError::corrupt_state())?,
            )),
            WireFallback::Ineligible => Ok(Self::ineligible()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRequest {
    identity: WireIdentity,
    source: WireSource,
    priority: WirePriority,
    requested_limits: WireLimits,
    cache_namespace: WireCacheNamespace,
    cache_access: WireCacheAccess,
    submitted_at: u64,
    operator_deadline: Option<u64>,
    cancellation: WireCancellation,
    fallback_eligibility: WireFallback,
}

impl From<&PersonalWorkerJobRequest> for WireRequest {
    fn from(value: &PersonalWorkerJobRequest) -> Self {
        Self {
            identity: WireIdentity::from(&value.identity),
            source: WireSource::from(&value.source),
            priority: value.priority.into(),
            requested_limits: value.requested_limits.into(),
            cache_namespace: WireCacheNamespace::from(&value.cache_namespace),
            cache_access: value.cache_access.into(),
            submitted_at: value.submitted_at.get(),
            operator_deadline: value.operator_deadline.map(EpochMillis::get),
            cancellation: value.cancellation.into(),
            fallback_eligibility: WireFallback::from(&value.fallback_eligibility),
        }
    }
}

impl TryFrom<WireRequest> for PersonalWorkerJobRequest {
    type Error = PersonalWorkerStoreError;

    fn try_from(value: WireRequest) -> Result<Self, Self::Error> {
        Ok(Self {
            identity: value.identity.try_into()?,
            source: value.source.try_into()?,
            priority: value.priority.into(),
            requested_limits: value.requested_limits.try_into()?,
            cache_namespace: value.cache_namespace.try_into()?,
            cache_access: value.cache_access.into(),
            submitted_at: epoch(value.submitted_at)?,
            operator_deadline: value.operator_deadline.map(epoch).transpose()?,
            cancellation: value.cancellation.try_into()?,
            fallback_eligibility: value.fallback_eligibility.try_into()?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "knowledge", rename_all = "snake_case", deny_unknown_fields)]
enum WireQueuePosition {
    Known { position: u32 },
    Unknown,
}

impl From<QueuePosition> for WireQueuePosition {
    fn from(value: QueuePosition) -> Self {
        match value {
            QueuePosition::Known { position } => Self::Known { position },
            QueuePosition::Unknown => Self::Unknown,
        }
    }
}

impl TryFrom<WireQueuePosition> for QueuePosition {
    type Error = PersonalWorkerStoreError;

    fn try_from(value: WireQueuePosition) -> Result<Self, Self::Error> {
        match value {
            WireQueuePosition::Known { position } => QueuePosition::known(position)
                .map_err(|_| PersonalWorkerStoreError::corrupt_state()),
            WireQueuePosition::Unknown => Ok(QueuePosition::unknown()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireReservation {
    id: String,
    generation: u64,
    reserved_at: u64,
    expires_at: u64,
}

impl From<&ReservationEvidence> for WireReservation {
    fn from(value: &ReservationEvidence) -> Self {
        Self {
            id: value.id.as_str().to_owned(),
            generation: value.generation.get(),
            reserved_at: value.reserved_at.get(),
            expires_at: value.expires_at.get(),
        }
    }
}

impl TryFrom<WireReservation> for ReservationEvidence {
    type Error = PersonalWorkerStoreError;

    fn try_from(value: WireReservation) -> Result<Self, Self::Error> {
        Ok(Self::new(
            ReservationId::parse(&value.id)
                .map_err(|_| PersonalWorkerStoreError::corrupt_state())?,
            ReservationGeneration::new(value.generation)
                .map_err(|_| PersonalWorkerStoreError::corrupt_state())?,
            epoch(value.reserved_at)?,
            epoch(value.expires_at)?,
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireHostCapacity {
    observed_at: u64,
    capacity: WireLimits,
}

impl From<HostCapacityObservation> for WireHostCapacity {
    fn from(value: HostCapacityObservation) -> Self {
        Self {
            observed_at: value.observed_at.get(),
            capacity: value.capacity.into(),
        }
    }
}

impl TryFrom<WireHostCapacity> for HostCapacityObservation {
    type Error = PersonalWorkerStoreError;

    fn try_from(value: WireHostCapacity) -> Result<Self, Self::Error> {
        Ok(Self::new(
            epoch(value.observed_at)?,
            value.capacity.try_into()?,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireAdmission {
    schema_version: u8,
    identity: WireIdentity,
    state: WireAdmissionState,
    observed_at: u64,
    requested_limits: WireLimits,
    host_capacity: Option<WireHostCapacity>,
    applied_limits: Option<WireLimits>,
    queue_position: Option<WireQueuePosition>,
    reservation: Option<WireReservation>,
    acknowledgement: Option<WireAcknowledgement>,
    fallback_eligibility: WireFallback,
    unavailable_reason: Option<WireUnavailableReason>,
}

impl From<&ExecutionAdmissionRecord> for WireAdmission {
    fn from(value: &ExecutionAdmissionRecord) -> Self {
        Self {
            schema_version: value.schema_version(),
            identity: WireIdentity::from(value.identity()),
            state: value.state().into(),
            observed_at: value.observed_at().get(),
            requested_limits: value.requested_limits().into(),
            host_capacity: value.host_capacity().map(Into::into),
            applied_limits: value.applied_limits().map(Into::into),
            queue_position: value.queue_position().map(Into::into),
            reservation: value.reservation().map(Into::into),
            acknowledgement: value.acknowledgement().map(Into::into),
            fallback_eligibility: WireFallback::from(value.fallback_eligibility()),
            unavailable_reason: value.unavailable_reason().map(Into::into),
        }
    }
}

impl TryFrom<WireAdmission> for ExecutionAdmissionRecord {
    type Error = PersonalWorkerStoreError;

    fn try_from(value: WireAdmission) -> Result<Self, Self::Error> {
        if value.schema_version != EXECUTION_ADMISSION_SCHEMA_VERSION {
            return Err(PersonalWorkerStoreError::corrupt_state());
        }
        ExecutionAdmissionRecord::from_input(ExecutionAdmissionInput {
            identity: value.identity.try_into()?,
            state: value.state.into(),
            observed_at: epoch(value.observed_at)?,
            requested_limits: value.requested_limits.try_into()?,
            host_capacity: value.host_capacity.map(TryInto::try_into).transpose()?,
            applied_limits: value.applied_limits.map(TryInto::try_into).transpose()?,
            queue_position: value.queue_position.map(TryInto::try_into).transpose()?,
            reservation: value.reservation.map(TryInto::try_into).transpose()?,
            acknowledgement: value.acknowledgement.map(Into::into),
            fallback_eligibility: value.fallback_eligibility.try_into()?,
            unavailable_reason: value.unavailable_reason.map(Into::into),
        })
        .map_err(|_| PersonalWorkerStoreError::corrupt_state())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireActive {
    request: WireRequest,
    admission: WireAdmission,
    started_at: Option<u64>,
}

impl From<&PersonalWorkerActiveReservation> for WireActive {
    fn from(value: &PersonalWorkerActiveReservation) -> Self {
        Self {
            request: WireRequest::from(&value.request),
            admission: WireAdmission::from(&value.admission),
            started_at: value.started_at.map(EpochMillis::get),
        }
    }
}

impl TryFrom<WireActive> for PersonalWorkerActiveReservation {
    type Error = PersonalWorkerStoreError;

    fn try_from(value: WireActive) -> Result<Self, Self::Error> {
        Ok(Self {
            request: value.request.try_into()?,
            admission: value.admission.try_into()?,
            started_at: value.started_at.map(epoch).transpose()?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WirePendingProfileChange {
    target: WireProfile,
    requested_at: u64,
}

impl From<PersonalWorkerPendingProfileChange> for WirePendingProfileChange {
    fn from(value: PersonalWorkerPendingProfileChange) -> Self {
        Self {
            target: value.target.into(),
            requested_at: value.requested_at.get(),
        }
    }
}

impl TryFrom<WirePendingProfileChange> for PersonalWorkerPendingProfileChange {
    type Error = PersonalWorkerStoreError;

    fn try_from(value: WirePendingProfileChange) -> Result<Self, Self::Error> {
        Ok(Self {
            target: value.target.into(),
            requested_at: epoch(value.requested_at)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireQueue {
    generation: u64,
    observed_at: u64,
    current_profile: WireProfile,
    last_activity_at: u64,
    queued: Vec<WireRequest>,
    active: Vec<WireActive>,
    pending_profile_change: Option<WirePendingProfileChange>,
}

impl From<&PersonalWorkerQueueInput> for WireQueue {
    fn from(value: &PersonalWorkerQueueInput) -> Self {
        Self {
            generation: value.generation.get(),
            observed_at: value.observed_at.get(),
            current_profile: value.current_profile.into(),
            last_activity_at: value.last_activity_at.get(),
            queued: value.queued.iter().map(Into::into).collect(),
            active: value.active.iter().map(Into::into).collect(),
            pending_profile_change: value.pending_profile_change.map(Into::into),
        }
    }
}

impl TryFrom<WireQueue> for PersonalWorkerQueueInput {
    type Error = PersonalWorkerStoreError;

    fn try_from(value: WireQueue) -> Result<Self, Self::Error> {
        Ok(Self {
            generation: PersonalWorkerQueueGeneration::new(value.generation)
                .map_err(|_| PersonalWorkerStoreError::corrupt_state())?,
            observed_at: epoch(value.observed_at)?,
            current_profile: value.current_profile.into(),
            last_activity_at: epoch(value.last_activity_at)?,
            queued: value
                .queued
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
            active: value
                .active
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
            pending_profile_change: value
                .pending_profile_change
                .map(TryInto::try_into)
                .transpose()?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireCacheLease {
    request_id: String,
    namespace: WireCacheNamespace,
    access: WireCacheAccess,
    reservation_id: String,
    reservation_generation: u64,
    acquired_at: u64,
}

impl From<&PersonalWorkerDurableCacheLease> for WireCacheLease {
    fn from(value: &PersonalWorkerDurableCacheLease) -> Self {
        Self {
            request_id: value.request_id.as_str().to_owned(),
            namespace: WireCacheNamespace::from(&value.namespace),
            access: value.access.into(),
            reservation_id: value.reservation_id.as_str().to_owned(),
            reservation_generation: value.reservation_generation.get(),
            acquired_at: value.acquired_at.get(),
        }
    }
}

impl TryFrom<WireCacheLease> for PersonalWorkerDurableCacheLease {
    type Error = PersonalWorkerStoreError;

    fn try_from(value: WireCacheLease) -> Result<Self, Self::Error> {
        Ok(Self::new(
            ExecutionRequestId::parse(&value.request_id)
                .map_err(|_| PersonalWorkerStoreError::corrupt_state())?,
            value.namespace.try_into()?,
            value.access.into(),
            ReservationId::parse(&value.reservation_id)
                .map_err(|_| PersonalWorkerStoreError::corrupt_state())?,
            ReservationGeneration::new(value.reservation_generation)
                .map_err(|_| PersonalWorkerStoreError::corrupt_state())?,
            epoch(value.acquired_at)?,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireTerminalEvidence {
    mutation_class: WireTerminalMutationClass,
    request: WireRequest,
    terminal_admission: WireAdmission,
    started_at: Option<u64>,
    cache_lease: WireCacheLease,
}

impl WireTerminalEvidence {
    fn from_parts(
        mutation_class: PersonalWorkerTerminalMutationClass,
        request: &PersonalWorkerJobRequest,
        terminal_admission: &ExecutionAdmissionRecord,
        started_at: Option<EpochMillis>,
        cache_lease: &PersonalWorkerDurableCacheLease,
    ) -> Self {
        Self {
            mutation_class: mutation_class.into(),
            request: WireRequest::from(request),
            terminal_admission: WireAdmission::from(terminal_admission),
            started_at: started_at.map(EpochMillis::get),
            cache_lease: WireCacheLease::from(cache_lease),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireTerminalTombstone {
    mutation_class: WireTerminalMutationClass,
    request: WireRequest,
    terminal_admission: WireAdmission,
    started_at: Option<u64>,
    cache_lease: WireCacheLease,
    evidence_digest: String,
}

impl From<&PersonalWorkerTerminalTombstone> for WireTerminalTombstone {
    fn from(value: &PersonalWorkerTerminalTombstone) -> Self {
        Self {
            mutation_class: value.mutation_class.into(),
            request: WireRequest::from(&value.request),
            terminal_admission: WireAdmission::from(&value.terminal_admission),
            started_at: value.started_at.map(EpochMillis::get),
            cache_lease: WireCacheLease::from(&value.cache_lease),
            evidence_digest: value.evidence_digest.as_str().to_owned(),
        }
    }
}

impl TryFrom<WireTerminalTombstone> for PersonalWorkerTerminalTombstone {
    type Error = PersonalWorkerStoreError;

    fn try_from(value: WireTerminalTombstone) -> Result<Self, Self::Error> {
        let mutation_class: PersonalWorkerTerminalMutationClass = value.mutation_class.into();
        if mutation_class != PersonalWorkerTerminalMutationClass::ReleaseCompletionAndCacheLease {
            return Err(PersonalWorkerStoreError::corrupt_state());
        }
        let expected_digest = Sha256Digest::parse(&value.evidence_digest)
            .map_err(|_| PersonalWorkerStoreError::corrupt_state())?;
        let tombstone = PersonalWorkerTerminalTombstone::new(
            value.request.try_into()?,
            value.terminal_admission.try_into()?,
            value.started_at.map(epoch).transpose()?,
            value.cache_lease.try_into()?,
        )
        .map_err(|_| PersonalWorkerStoreError::corrupt_state())?;
        if tombstone.evidence_digest != expected_digest {
            return Err(PersonalWorkerStoreError::corrupt_state());
        }
        Ok(tombstone)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireHistoryEntry {
    revision: u64,
    queue_generation: u64,
    observed_at: u64,
    queued_count: u32,
    active_count: u32,
    cache_lease_count: u32,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    terminal_tombstone_count: u32,
    state_digest: String,
}

impl From<&PersonalWorkerHistoryEntry> for WireHistoryEntry {
    fn from(value: &PersonalWorkerHistoryEntry) -> Self {
        Self {
            revision: value.revision.get(),
            queue_generation: value.queue_generation.get(),
            observed_at: value.observed_at.get(),
            queued_count: value.queued_count,
            active_count: value.active_count,
            cache_lease_count: value.cache_lease_count,
            terminal_tombstone_count: value.terminal_tombstone_count,
            state_digest: value.state_digest.as_str().to_owned(),
        }
    }
}

impl TryFrom<WireHistoryEntry> for PersonalWorkerHistoryEntry {
    type Error = PersonalWorkerStoreError;

    fn try_from(value: WireHistoryEntry) -> Result<Self, Self::Error> {
        Ok(Self {
            revision: PersonalWorkerStoreRevision::new(value.revision)
                .map_err(|_| PersonalWorkerStoreError::corrupt_state())?,
            queue_generation: PersonalWorkerQueueGeneration::new(value.queue_generation)
                .map_err(|_| PersonalWorkerStoreError::corrupt_state())?,
            observed_at: epoch(value.observed_at)?,
            queued_count: value.queued_count,
            active_count: value.active_count,
            cache_lease_count: value.cache_lease_count,
            terminal_tombstone_count: value.terminal_tombstone_count,
            state_digest: Sha256Digest::parse(&value.state_digest)
                .map_err(|_| PersonalWorkerStoreError::corrupt_state())?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireSnapshot {
    queue: WireQueue,
    cache_leases: Vec<WireCacheLease>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    terminal_tombstones: Vec<WireTerminalTombstone>,
}

impl WireSnapshot {
    fn from_parts(
        queue: &PersonalWorkerQueueInput,
        cache_leases: &[PersonalWorkerDurableCacheLease],
        terminal_tombstones: &[PersonalWorkerTerminalTombstone],
    ) -> Self {
        Self {
            queue: WireQueue::from(queue),
            cache_leases: cache_leases.iter().map(Into::into).collect(),
            terminal_tombstones: terminal_tombstones.iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireDocument {
    schema_version: u8,
    revision: u64,
    queue: WireQueue,
    cache_leases: Vec<WireCacheLease>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    terminal_tombstones: Vec<WireTerminalTombstone>,
    history: Vec<WireHistoryEntry>,
}

impl From<&PersonalWorkerStoreDocument> for WireDocument {
    fn from(value: &PersonalWorkerStoreDocument) -> Self {
        Self {
            schema_version: value.schema_version,
            revision: value.revision.get(),
            queue: WireQueue::from(&value.queue),
            cache_leases: value.cache_leases.iter().map(Into::into).collect(),
            terminal_tombstones: value.terminal_tombstones.iter().map(Into::into).collect(),
            history: value.history.iter().map(Into::into).collect(),
        }
    }
}

impl TryFrom<WireDocument> for PersonalWorkerStoreDocument {
    type Error = PersonalWorkerStoreError;

    fn try_from(value: WireDocument) -> Result<Self, Self::Error> {
        if value.schema_version != PERSONAL_WORKER_STORE_SCHEMA_VERSION {
            return Err(PersonalWorkerStoreError::version_incompatible());
        }
        Self::from_parts(
            PersonalWorkerStoreRevision::new(value.revision)
                .map_err(|_| PersonalWorkerStoreError::corrupt_state())?,
            value.queue.try_into()?,
            value
                .cache_leases
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
            value
                .terminal_tombstones
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
            value
                .history
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
        )
        .map_err(|_| PersonalWorkerStoreError::corrupt_state())
    }
}

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

fn epoch(value: u64) -> Result<EpochMillis, PersonalWorkerStoreError> {
    EpochMillis::new(value).map_err(|_| PersonalWorkerStoreError::corrupt_state())
}
