use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::disposable_worker_reconciler::{
    CapacityClaimId, DisposableAttempt, DisposableAttemptId, DisposableAttemptPhase,
    DisposableHostUsage, DisposableVmId, DisposableWorkerAction, DisposableWorkerResources,
    GitHubJobConclusion, GitHubJobId, MAX_DISPOSABLE_WORKERS, PersistedDisposableAttempt,
    ScaleSetRunnerId,
};
use crate::execution_admission::EpochMillis;

pub const DISPOSABLE_WORKER_STORE_SCHEMA_VERSION: u8 = 1;
pub const MAX_DISPOSABLE_WORKER_STORE_BYTES: usize = 1_048_576;
pub const MAX_DISPOSABLE_WORKER_ATTEMPTS: usize = 512;
pub const MAX_ACTIVE_DISPOSABLE_WORKERS: usize = MAX_DISPOSABLE_WORKERS as usize;
pub const MAX_DISPOSABLE_WORKER_COMPLETION_TOMBSTONES: usize = 4_096;
const MAX_DISPOSABLE_WORKER_STORE_REVISION: u64 = 1_000_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct DisposableWorkerStoreRevision(u64);

impl DisposableWorkerStoreRevision {
    pub fn new(value: u64) -> Result<Self, DisposableWorkerStoreError> {
        if !(1..=MAX_DISPOSABLE_WORKER_STORE_REVISION).contains(&value) {
            return Err(DisposableWorkerStoreError::invalid_document());
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    fn next(self) -> Result<Self, DisposableWorkerStoreError> {
        self.0
            .checked_add(1)
            .filter(|value| *value <= MAX_DISPOSABLE_WORKER_STORE_REVISION)
            .map(Self)
            .ok_or_else(DisposableWorkerStoreError::revision_conflict)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableWorkerStoreMutationDisposition {
    Applied,
    Duplicate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisposableWorkerStoreMutation {
    disposition: DisposableWorkerStoreMutationDisposition,
    document: DisposableWorkerStoreDocument,
}

impl DisposableWorkerStoreMutation {
    #[must_use]
    pub const fn disposition(&self) -> DisposableWorkerStoreMutationDisposition {
        self.disposition
    }

    #[must_use]
    pub const fn document(&self) -> &DisposableWorkerStoreDocument {
        &self.document
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisposableWorkerStoreDocument {
    schema_version: u8,
    revision: DisposableWorkerStoreRevision,
    attempts: Vec<DisposableAttempt>,
    completed_attempt_ids: Vec<DisposableAttemptId>,
}

impl DisposableWorkerStoreDocument {
    pub fn new() -> Result<Self, DisposableWorkerStoreError> {
        Self::from_parts(
            DisposableWorkerStoreRevision::new(1)?,
            Vec::new(),
            Vec::new(),
        )
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn revision(&self) -> DisposableWorkerStoreRevision {
        self.revision
    }

    #[must_use]
    pub fn attempts(&self) -> &[DisposableAttempt] {
        &self.attempts
    }

    #[must_use]
    pub fn completed_attempt_ids(&self) -> &[DisposableAttemptId] {
        &self.completed_attempt_ids
    }

    pub fn host_usage(&self) -> Result<DisposableHostUsage, DisposableWorkerStoreError> {
        let held = self
            .attempts
            .iter()
            .filter(|attempt| attempt.capacity_reserved())
            .collect::<Vec<_>>();
        if held.is_empty() {
            return Ok(DisposableHostUsage::zero());
        }
        let mut cpu_millis = 0_u32;
        let mut memory_bytes = 0_u64;
        let mut disk_bytes = 0_u64;
        for attempt in &held {
            let resources = attempt.resources();
            cpu_millis = cpu_millis
                .checked_add(resources.cpu_millis())
                .ok_or_else(DisposableWorkerStoreError::invalid_document)?;
            memory_bytes = memory_bytes
                .checked_add(resources.memory_bytes())
                .ok_or_else(DisposableWorkerStoreError::invalid_document)?;
            disk_bytes = disk_bytes
                .checked_add(resources.disk_bytes())
                .ok_or_else(DisposableWorkerStoreError::invalid_document)?;
        }
        let resources = DisposableWorkerResources::new(cpu_millis, memory_bytes, disk_bytes)
            .map_err(|_| DisposableWorkerStoreError::invalid_document())?;
        DisposableHostUsage::new(
            u16::try_from(held.len())
                .map_err(|_| DisposableWorkerStoreError::invalid_document())?,
            resources,
        )
        .map_err(|_| DisposableWorkerStoreError::invalid_document())
    }

    pub fn reserve_attempt(
        &self,
        attempt: DisposableAttempt,
    ) -> Result<DisposableWorkerStoreMutation, DisposableWorkerStoreError> {
        attempt
            .validate_for_persistence()
            .map_err(|_| DisposableWorkerStoreError::invalid_document())?;
        if attempt.phase() != DisposableAttemptPhase::Reserved || !attempt.capacity_reserved() {
            return Err(DisposableWorkerStoreError::invalid_transition());
        }
        if let Some(existing) = self
            .attempts
            .iter()
            .find(|existing| existing.attempt_id() == attempt.attempt_id())
        {
            if existing == &attempt {
                return Ok(self.duplicate());
            }
            return Err(DisposableWorkerStoreError::conflict());
        }
        if self.completed_attempt_ids.contains(attempt.attempt_id()) {
            return Ok(self.duplicate());
        }
        if self.attempts.len() >= MAX_DISPOSABLE_WORKER_ATTEMPTS
            || self
                .attempts
                .iter()
                .filter(|attempt| attempt.capacity_reserved())
                .count()
                >= MAX_ACTIVE_DISPOSABLE_WORKERS
        {
            return Err(DisposableWorkerStoreError::capacity_reached());
        }
        if self.attempts.iter().any(|existing| {
            existing.capacity_claim_id() == attempt.capacity_claim_id()
                || existing.vm_id() == attempt.vm_id()
                || existing.runner_id() == attempt.runner_id()
        }) {
            return Err(DisposableWorkerStoreError::conflict());
        }
        let mut attempts = self.attempts.clone();
        attempts.push(attempt);
        self.applied(attempts, self.completed_attempt_ids.clone())
    }

    pub fn checkpoint_attempt(
        &self,
        attempt_id: &DisposableAttemptId,
        action: &DisposableWorkerAction,
    ) -> Result<DisposableWorkerStoreMutation, DisposableWorkerStoreError> {
        let position = self
            .attempts
            .iter()
            .position(|attempt| attempt.attempt_id() == attempt_id)
            .ok_or_else(DisposableWorkerStoreError::not_found)?;
        let current = &self.attempts[position];
        let next = current
            .checkpoint(action)
            .map_err(|_| DisposableWorkerStoreError::invalid_transition())?;
        let mut attempts = self.attempts.clone();
        attempts[position] = next;
        self.applied(attempts, self.completed_attempt_ids.clone())
    }

    pub fn prune_complete_attempt(
        &self,
        attempt_id: &DisposableAttemptId,
    ) -> Result<DisposableWorkerStoreMutation, DisposableWorkerStoreError> {
        let Some(position) = self
            .attempts
            .iter()
            .position(|attempt| attempt.attempt_id() == attempt_id)
        else {
            return Ok(self.duplicate());
        };
        if self.attempts[position].phase() != DisposableAttemptPhase::Complete {
            return Err(DisposableWorkerStoreError::invalid_transition());
        }
        let mut attempts = self.attempts.clone();
        let completed = attempts.remove(position);
        let mut completed_attempt_ids = self.completed_attempt_ids.clone();
        if completed_attempt_ids.len() == MAX_DISPOSABLE_WORKER_COMPLETION_TOMBSTONES {
            completed_attempt_ids.remove(0);
        }
        completed_attempt_ids.push(completed.attempt_id().clone());
        self.applied(attempts, completed_attempt_ids)
    }

    pub fn validate_successor_of(&self, previous: &Self) -> Result<(), DisposableWorkerStoreError> {
        if self.revision != previous.revision.next()? {
            return Err(DisposableWorkerStoreError::revision_conflict());
        }
        self.validate()?;

        if self.attempts.len() == previous.attempts.len() + 1
            && self.attempts[..previous.attempts.len()] == previous.attempts
            && self.completed_attempt_ids == previous.completed_attempt_ids
            && self
                .attempts
                .last()
                .is_some_and(|attempt| attempt.phase() == DisposableAttemptPhase::Reserved)
        {
            return Ok(());
        }
        if previous.attempts.len() == self.attempts.len() + 1 {
            let removed = first_difference(&previous.attempts, &self.attempts);
            let mut expected_tombstones = previous.completed_attempt_ids.clone();
            if expected_tombstones.len() == MAX_DISPOSABLE_WORKER_COMPLETION_TOMBSTONES {
                expected_tombstones.remove(0);
            }
            expected_tombstones.push(previous.attempts[removed].attempt_id().clone());
            if previous.attempts[removed].phase() == DisposableAttemptPhase::Complete
                && previous.attempts[..removed] == self.attempts[..removed]
                && previous.attempts[removed + 1..] == self.attempts[removed..]
                && self.completed_attempt_ids == expected_tombstones
            {
                return Ok(());
            }
        }
        if self.attempts.len() == previous.attempts.len() {
            if self.completed_attempt_ids != previous.completed_attempt_ids {
                return Err(DisposableWorkerStoreError::revision_conflict());
            }
            let differences = self
                .attempts
                .iter()
                .zip(&previous.attempts)
                .enumerate()
                .filter(|(_, (next, old))| next != old)
                .collect::<Vec<_>>();
            if let [(position, (next, old))] = differences.as_slice()
                && self.attempts[..*position] == previous.attempts[..*position]
                && self.attempts[*position + 1..] == previous.attempts[*position + 1..]
            {
                return next
                    .validate_successor_of(old)
                    .map_err(|_| DisposableWorkerStoreError::revision_conflict());
            }
        }
        Err(DisposableWorkerStoreError::revision_conflict())
    }

    fn from_parts(
        revision: DisposableWorkerStoreRevision,
        attempts: Vec<DisposableAttempt>,
        completed_attempt_ids: Vec<DisposableAttemptId>,
    ) -> Result<Self, DisposableWorkerStoreError> {
        let document = Self {
            schema_version: DISPOSABLE_WORKER_STORE_SCHEMA_VERSION,
            revision,
            attempts,
            completed_attempt_ids,
        };
        document.validate()?;
        Ok(document)
    }

    fn validate(&self) -> Result<(), DisposableWorkerStoreError> {
        if self.schema_version != DISPOSABLE_WORKER_STORE_SCHEMA_VERSION
            || self.attempts.len() > MAX_DISPOSABLE_WORKER_ATTEMPTS
            || self.completed_attempt_ids.len() > MAX_DISPOSABLE_WORKER_COMPLETION_TOMBSTONES
        {
            return Err(DisposableWorkerStoreError::invalid_document());
        }
        let active = self
            .attempts
            .iter()
            .filter(|attempt| attempt.capacity_reserved())
            .count();
        if active > MAX_ACTIVE_DISPOSABLE_WORKERS {
            return Err(DisposableWorkerStoreError::invalid_document());
        }
        let mut attempts = BTreeSet::new();
        let mut claims = BTreeSet::new();
        let mut vms = BTreeSet::new();
        let mut runners = BTreeSet::new();
        let completed = self.completed_attempt_ids.iter().collect::<BTreeSet<_>>();
        if completed.len() != self.completed_attempt_ids.len() {
            return Err(DisposableWorkerStoreError::invalid_document());
        }
        for attempt in &self.attempts {
            attempt
                .validate_for_persistence()
                .map_err(|_| DisposableWorkerStoreError::invalid_document())?;
            if !attempts.insert(attempt.attempt_id())
                || !claims.insert(attempt.capacity_claim_id())
                || !vms.insert(attempt.vm_id())
                || !runners.insert(attempt.runner_id())
                || completed.contains(attempt.attempt_id())
            {
                return Err(DisposableWorkerStoreError::invalid_document());
            }
        }
        self.host_usage()?;
        Ok(())
    }

    fn duplicate(&self) -> DisposableWorkerStoreMutation {
        DisposableWorkerStoreMutation {
            disposition: DisposableWorkerStoreMutationDisposition::Duplicate,
            document: self.clone(),
        }
    }

    fn applied(
        &self,
        attempts: Vec<DisposableAttempt>,
        completed_attempt_ids: Vec<DisposableAttemptId>,
    ) -> Result<DisposableWorkerStoreMutation, DisposableWorkerStoreError> {
        let document = Self::from_parts(self.revision.next()?, attempts, completed_attempt_ids)?;
        document.validate_successor_of(self)?;
        Ok(DisposableWorkerStoreMutation {
            disposition: DisposableWorkerStoreMutationDisposition::Applied,
            document,
        })
    }
}

fn first_difference(left: &[DisposableAttempt], right: &[DisposableAttempt]) -> usize {
    left.iter()
        .zip(right)
        .position(|(left, right)| left != right)
        .unwrap_or(right.len())
}

pub fn encode_disposable_worker_store_document(
    document: &DisposableWorkerStoreDocument,
) -> Result<Vec<u8>, DisposableWorkerStoreError> {
    document.validate()?;
    let wire = WireDocument::from(document);
    let mut bytes = serde_json::to_vec_pretty(&wire)
        .map_err(|_| DisposableWorkerStoreError::invalid_document())?;
    bytes.push(b'\n');
    if bytes.len() > MAX_DISPOSABLE_WORKER_STORE_BYTES {
        return Err(DisposableWorkerStoreError::invalid_document());
    }
    Ok(bytes)
}

pub fn decode_disposable_worker_store_document(
    bytes: &[u8],
) -> Result<DisposableWorkerStoreDocument, DisposableWorkerStoreError> {
    if bytes.len() > MAX_DISPOSABLE_WORKER_STORE_BYTES {
        return Err(DisposableWorkerStoreError::corrupt_state());
    }
    let version: WireVersion =
        serde_json::from_slice(bytes).map_err(|_| DisposableWorkerStoreError::corrupt_state())?;
    if version.schema_version != DISPOSABLE_WORKER_STORE_SCHEMA_VERSION {
        return Err(DisposableWorkerStoreError::version_incompatible());
    }
    let wire: WireDocument =
        serde_json::from_slice(bytes).map_err(|_| DisposableWorkerStoreError::corrupt_state())?;
    let document = DisposableWorkerStoreDocument::try_from(wire)?;
    if encode_disposable_worker_store_document(&document)
        .map_err(|_| DisposableWorkerStoreError::corrupt_state())?
        != bytes
    {
        return Err(DisposableWorkerStoreError::corrupt_state());
    }
    Ok(document)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableWorkerStoreErrorKind {
    Missing,
    Busy,
    Io,
    UnsafeFilesystem,
    VersionIncompatible,
    CorruptState,
    RevisionConflict,
    CapacityReached,
    NotFound,
    Conflict,
    InvalidTransition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DisposableWorkerStoreError {
    kind: DisposableWorkerStoreErrorKind,
    message: &'static str,
}

impl DisposableWorkerStoreError {
    #[must_use]
    pub const fn kind(&self) -> DisposableWorkerStoreErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn message(&self) -> &'static str {
        self.message
    }

    const fn new(kind: DisposableWorkerStoreErrorKind, message: &'static str) -> Self {
        Self { kind, message }
    }

    #[must_use]
    pub const fn public(kind: DisposableWorkerStoreErrorKind, message: &'static str) -> Self {
        Self::new(kind, message)
    }

    const fn version_incompatible() -> Self {
        Self::new(
            DisposableWorkerStoreErrorKind::VersionIncompatible,
            "durable disposable-worker state schema is incompatible",
        )
    }

    const fn corrupt_state() -> Self {
        Self::new(
            DisposableWorkerStoreErrorKind::CorruptState,
            "durable disposable-worker state is corrupt or noncanonical",
        )
    }

    const fn invalid_document() -> Self {
        Self::new(
            DisposableWorkerStoreErrorKind::CorruptState,
            "disposable-worker document is invalid",
        )
    }

    const fn revision_conflict() -> Self {
        Self::new(
            DisposableWorkerStoreErrorKind::RevisionConflict,
            "disposable-worker store revision or successor is stale",
        )
    }

    const fn capacity_reached() -> Self {
        Self::new(
            DisposableWorkerStoreErrorKind::CapacityReached,
            "disposable-worker durable capacity is exhausted",
        )
    }

    const fn not_found() -> Self {
        Self::new(
            DisposableWorkerStoreErrorKind::NotFound,
            "disposable-worker attempt does not exist",
        )
    }

    const fn conflict() -> Self {
        Self::new(
            DisposableWorkerStoreErrorKind::Conflict,
            "disposable-worker identity conflicts with durable state",
        )
    }

    const fn invalid_transition() -> Self {
        Self::new(
            DisposableWorkerStoreErrorKind::InvalidTransition,
            "disposable-worker attempt transition is invalid",
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableWorkerStoreWriteDisposition {
    Created,
    Replaced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DisposableWorkerStoreWriteReceipt {
    disposition: DisposableWorkerStoreWriteDisposition,
    revision: DisposableWorkerStoreRevision,
    bytes_written: usize,
}

impl DisposableWorkerStoreWriteReceipt {
    #[must_use]
    pub const fn new(
        disposition: DisposableWorkerStoreWriteDisposition,
        revision: DisposableWorkerStoreRevision,
        bytes_written: usize,
    ) -> Self {
        Self {
            disposition,
            revision,
            bytes_written,
        }
    }

    #[must_use]
    pub const fn disposition(&self) -> DisposableWorkerStoreWriteDisposition {
        self.disposition
    }

    #[must_use]
    pub const fn revision(&self) -> DisposableWorkerStoreRevision {
        self.revision
    }

    #[must_use]
    pub const fn bytes_written(&self) -> usize {
        self.bytes_written
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableWorkerStoreRecoveryDisposition {
    Clean,
    PublishedStaged,
    RemovedStaleStaged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DisposableWorkerStoreRecovery {
    disposition: DisposableWorkerStoreRecoveryDisposition,
    revision: Option<DisposableWorkerStoreRevision>,
}

impl DisposableWorkerStoreRecovery {
    #[must_use]
    pub const fn new(
        disposition: DisposableWorkerStoreRecoveryDisposition,
        revision: Option<DisposableWorkerStoreRevision>,
    ) -> Self {
        Self {
            disposition,
            revision,
        }
    }

    #[must_use]
    pub const fn disposition(&self) -> DisposableWorkerStoreRecoveryDisposition {
        self.disposition
    }

    #[must_use]
    pub const fn revision(&self) -> Option<DisposableWorkerStoreRevision> {
        self.revision
    }
}

pub trait DisposableWorkerStore {
    fn load(&self) -> Result<Option<DisposableWorkerStoreDocument>, DisposableWorkerStoreError>;

    fn create(
        &mut self,
        document: &DisposableWorkerStoreDocument,
    ) -> Result<DisposableWorkerStoreWriteReceipt, DisposableWorkerStoreError>;

    fn replace_if_revision(
        &mut self,
        expected_revision: DisposableWorkerStoreRevision,
        document: &DisposableWorkerStoreDocument,
    ) -> Result<DisposableWorkerStoreWriteReceipt, DisposableWorkerStoreError>;

    fn recover(&mut self) -> Result<DisposableWorkerStoreRecovery, DisposableWorkerStoreError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableWorkerStoreMutationClass {
    ReserveAttempt,
    CheckpointAttempt,
    PruneCompleteAttempt,
}

#[derive(Clone)]
pub enum DisposableWorkerStoreMutationIntent {
    ReserveAttempt {
        attempt: DisposableAttempt,
    },
    CheckpointAttempt {
        attempt_id: DisposableAttemptId,
        action: DisposableWorkerAction,
    },
    PruneCompleteAttempt {
        attempt_id: DisposableAttemptId,
    },
}

impl DisposableWorkerStoreMutationIntent {
    #[must_use]
    pub const fn class(&self) -> DisposableWorkerStoreMutationClass {
        match self {
            Self::ReserveAttempt { .. } => DisposableWorkerStoreMutationClass::ReserveAttempt,
            Self::CheckpointAttempt { .. } => DisposableWorkerStoreMutationClass::CheckpointAttempt,
            Self::PruneCompleteAttempt { .. } => {
                DisposableWorkerStoreMutationClass::PruneCompleteAttempt
            }
        }
    }
}

impl fmt::Debug for DisposableWorkerStoreMutationIntent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableWorkerStoreMutationIntent")
            .field("class", &self.class())
            .field("private_payload", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DisposableWorkerStoreMutationReceipt {
    disposition: DisposableWorkerStoreMutationDisposition,
    mutation: DisposableWorkerStoreMutationClass,
    old_revision: DisposableWorkerStoreRevision,
    new_revision: DisposableWorkerStoreRevision,
}

impl DisposableWorkerStoreMutationReceipt {
    #[must_use]
    pub const fn disposition(&self) -> DisposableWorkerStoreMutationDisposition {
        self.disposition
    }

    #[must_use]
    pub const fn mutation(&self) -> DisposableWorkerStoreMutationClass {
        self.mutation
    }

    #[must_use]
    pub const fn old_revision(&self) -> DisposableWorkerStoreRevision {
        self.old_revision
    }

    #[must_use]
    pub const fn new_revision(&self) -> DisposableWorkerStoreRevision {
        self.new_revision
    }
}

/// Recover and atomically apply one exact disposable-worker lifecycle mutation.
///
/// The revision check and the store's compare-and-swap publication prevent two controller turns
/// from provisioning or checkpointing the same attempt concurrently. Duplicate intents at the
/// exact expected revision are acknowledged without writing.
pub fn apply_disposable_worker_store_mutation(
    store: &mut impl DisposableWorkerStore,
    expected_revision: DisposableWorkerStoreRevision,
    intent: DisposableWorkerStoreMutationIntent,
) -> Result<DisposableWorkerStoreMutationReceipt, DisposableWorkerStoreError> {
    store.recover()?;
    let current = store.load()?.ok_or_else(|| {
        DisposableWorkerStoreError::public(
            DisposableWorkerStoreErrorKind::Missing,
            "durable disposable-worker state does not exist",
        )
    })?;
    if current.revision() != expected_revision {
        return Err(DisposableWorkerStoreError::revision_conflict());
    }
    let class = intent.class();
    let mutation = match intent {
        DisposableWorkerStoreMutationIntent::ReserveAttempt { attempt } => {
            current.reserve_attempt(attempt)?
        }
        DisposableWorkerStoreMutationIntent::CheckpointAttempt { attempt_id, action } => {
            current.checkpoint_attempt(&attempt_id, &action)?
        }
        DisposableWorkerStoreMutationIntent::PruneCompleteAttempt { attempt_id } => {
            current.prune_complete_attempt(&attempt_id)?
        }
    };
    if mutation.disposition() == DisposableWorkerStoreMutationDisposition::Duplicate {
        return Ok(DisposableWorkerStoreMutationReceipt {
            disposition: DisposableWorkerStoreMutationDisposition::Duplicate,
            mutation: class,
            old_revision: current.revision(),
            new_revision: current.revision(),
        });
    }
    let next = mutation.document();
    let write = store.replace_if_revision(expected_revision, next)?;
    if write.disposition() != DisposableWorkerStoreWriteDisposition::Replaced
        || write.revision() != next.revision()
    {
        return Err(DisposableWorkerStoreError::corrupt_state());
    }
    Ok(DisposableWorkerStoreMutationReceipt {
        disposition: DisposableWorkerStoreMutationDisposition::Applied,
        mutation: class,
        old_revision: current.revision(),
        new_revision: next.revision(),
    })
}

impl fmt::Display for DisposableWorkerStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for DisposableWorkerStoreError {}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireDocument {
    schema_version: u8,
    revision: u64,
    attempts: Vec<WireAttempt>,
    completed_attempt_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct WireVersion {
    schema_version: u8,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireAttempt {
    attempt_id: String,
    capacity_claim_id: String,
    vm_id: String,
    runner_id: String,
    resources: WireResources,
    phase: WirePhase,
    github_job_id: Option<u64>,
    conclusion: Option<WireConclusion>,
    not_after: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireResources {
    cpu_millis: u32,
    memory_bytes: u64,
    disk_bytes: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WirePhase {
    Reserved,
    Provisioning,
    Registering,
    Waiting,
    Assigned,
    Running,
    Terminal,
    Destroying,
    Deregistering,
    Releasing,
    Complete,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireConclusion {
    Success,
    Failure,
    Cancelled,
    TimedOut,
}

impl From<&DisposableWorkerStoreDocument> for WireDocument {
    fn from(value: &DisposableWorkerStoreDocument) -> Self {
        Self {
            schema_version: value.schema_version,
            revision: value.revision.get(),
            attempts: value.attempts.iter().map(Into::into).collect(),
            completed_attempt_ids: value
                .completed_attempt_ids
                .iter()
                .map(|attempt_id| attempt_id.as_str().to_owned())
                .collect(),
        }
    }
}

impl TryFrom<WireDocument> for DisposableWorkerStoreDocument {
    type Error = DisposableWorkerStoreError;

    fn try_from(value: WireDocument) -> Result<Self, Self::Error> {
        if value.schema_version != DISPOSABLE_WORKER_STORE_SCHEMA_VERSION {
            return Err(DisposableWorkerStoreError::version_incompatible());
        }
        Self::from_parts(
            DisposableWorkerStoreRevision::new(value.revision)
                .map_err(|_| DisposableWorkerStoreError::corrupt_state())?,
            value
                .attempts
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
            value
                .completed_attempt_ids
                .into_iter()
                .map(|attempt_id| {
                    DisposableAttemptId::parse(&attempt_id)
                        .map_err(|_| DisposableWorkerStoreError::corrupt_state())
                })
                .collect::<Result<_, _>>()?,
        )
        .map_err(|_| DisposableWorkerStoreError::corrupt_state())
    }
}

impl From<&DisposableAttempt> for WireAttempt {
    fn from(value: &DisposableAttempt) -> Self {
        Self {
            attempt_id: value.attempt_id().as_str().to_owned(),
            capacity_claim_id: value.capacity_claim_id().as_str().to_owned(),
            vm_id: value.vm_id().as_str().to_owned(),
            runner_id: value.runner_id().as_str().to_owned(),
            resources: WireResources {
                cpu_millis: value.resources().cpu_millis(),
                memory_bytes: value.resources().memory_bytes(),
                disk_bytes: value.resources().disk_bytes(),
            },
            phase: value.phase().into(),
            github_job_id: value.github_job_id().map(GitHubJobId::get),
            conclusion: value.conclusion().map(Into::into),
            not_after: value.not_after().get(),
        }
    }
}

impl TryFrom<WireAttempt> for DisposableAttempt {
    type Error = DisposableWorkerStoreError;

    fn try_from(value: WireAttempt) -> Result<Self, Self::Error> {
        DisposableAttempt::from_persisted(PersistedDisposableAttempt {
            attempt_id: DisposableAttemptId::parse(&value.attempt_id)
                .map_err(|_| DisposableWorkerStoreError::corrupt_state())?,
            capacity_claim_id: CapacityClaimId::parse(&value.capacity_claim_id)
                .map_err(|_| DisposableWorkerStoreError::corrupt_state())?,
            vm_id: DisposableVmId::parse(&value.vm_id)
                .map_err(|_| DisposableWorkerStoreError::corrupt_state())?,
            runner_id: ScaleSetRunnerId::parse(&value.runner_id)
                .map_err(|_| DisposableWorkerStoreError::corrupt_state())?,
            resources: DisposableWorkerResources::new(
                value.resources.cpu_millis,
                value.resources.memory_bytes,
                value.resources.disk_bytes,
            )
            .map_err(|_| DisposableWorkerStoreError::corrupt_state())?,
            phase: value.phase.into(),
            github_job_id: value
                .github_job_id
                .map(GitHubJobId::new)
                .transpose()
                .map_err(|_| DisposableWorkerStoreError::corrupt_state())?,
            conclusion: value.conclusion.map(Into::into),
            not_after: EpochMillis::new(value.not_after)
                .map_err(|_| DisposableWorkerStoreError::corrupt_state())?,
        })
        .map_err(|_| DisposableWorkerStoreError::corrupt_state())
    }
}

impl From<DisposableAttemptPhase> for WirePhase {
    fn from(value: DisposableAttemptPhase) -> Self {
        match value {
            DisposableAttemptPhase::Reserved => Self::Reserved,
            DisposableAttemptPhase::Provisioning => Self::Provisioning,
            DisposableAttemptPhase::Registering => Self::Registering,
            DisposableAttemptPhase::Waiting => Self::Waiting,
            DisposableAttemptPhase::Assigned => Self::Assigned,
            DisposableAttemptPhase::Running => Self::Running,
            DisposableAttemptPhase::Terminal => Self::Terminal,
            DisposableAttemptPhase::Destroying => Self::Destroying,
            DisposableAttemptPhase::Deregistering => Self::Deregistering,
            DisposableAttemptPhase::Releasing => Self::Releasing,
            DisposableAttemptPhase::Complete => Self::Complete,
        }
    }
}

impl From<WirePhase> for DisposableAttemptPhase {
    fn from(value: WirePhase) -> Self {
        match value {
            WirePhase::Reserved => Self::Reserved,
            WirePhase::Provisioning => Self::Provisioning,
            WirePhase::Registering => Self::Registering,
            WirePhase::Waiting => Self::Waiting,
            WirePhase::Assigned => Self::Assigned,
            WirePhase::Running => Self::Running,
            WirePhase::Terminal => Self::Terminal,
            WirePhase::Destroying => Self::Destroying,
            WirePhase::Deregistering => Self::Deregistering,
            WirePhase::Releasing => Self::Releasing,
            WirePhase::Complete => Self::Complete,
        }
    }
}

impl From<GitHubJobConclusion> for WireConclusion {
    fn from(value: GitHubJobConclusion) -> Self {
        match value {
            GitHubJobConclusion::Success => Self::Success,
            GitHubJobConclusion::Failure => Self::Failure,
            GitHubJobConclusion::Cancelled => Self::Cancelled,
            GitHubJobConclusion::TimedOut => Self::TimedOut,
        }
    }
}

impl From<WireConclusion> for GitHubJobConclusion {
    fn from(value: WireConclusion) -> Self {
        match value {
            WireConclusion::Success => Self::Success,
            WireConclusion::Failure => Self::Failure,
            WireConclusion::Cancelled => Self::Cancelled,
            WireConclusion::TimedOut => Self::TimedOut,
        }
    }
}
