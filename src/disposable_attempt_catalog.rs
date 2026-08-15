use std::collections::BTreeSet;
use std::fmt;

use serde::Serialize;

use crate::disposable_attempt_state::{DisposableAttemptRevision, DisposableAttemptState};
use crate::disposable_prepared_template::DisposablePreparedTemplateIdentity;
use crate::disposable_worker_reconciler::{
    DisposableAttemptId, DisposableAttemptPhase, DisposableHostUsage, DisposableVmIdentity,
    DisposableWorkerResources,
};
use crate::github_scale_set_protocol::{ScaleSetJobId, ScaleSetJobResult, ScaleSetRunnerReference};

mod codec;
pub use codec::{
    DisposableAttemptCatalogCodecError, DisposableAttemptCatalogCodecErrorKind,
    MAX_DISPOSABLE_ATTEMPT_CATALOG_DOCUMENT_BYTES, decode_disposable_attempt_catalog,
    encode_disposable_attempt_catalog,
};

pub const DISPOSABLE_ATTEMPT_CATALOG_SCHEMA_VERSION: u8 = 6;
pub const MAX_ACTIVE_DISPOSABLE_ATTEMPTS: usize = 64;
pub const MAX_DISPOSABLE_ATTEMPT_TOMBSTONES: usize = 64;
const MAX_DISPOSABLE_ATTEMPT_CATALOG_REVISION: u64 = 1_000_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct DisposableAttemptCatalogRevision(u64);

impl DisposableAttemptCatalogRevision {
    /// Construct one bounded positive catalog revision.
    ///
    /// # Errors
    ///
    /// Returns an error for zero or a value beyond the reviewed revision space.
    pub fn new(value: u64) -> Result<Self, DisposableAttemptCatalogError> {
        if !(1..=MAX_DISPOSABLE_ATTEMPT_CATALOG_REVISION).contains(&value) {
            return Err(catalog_error(
                DisposableAttemptCatalogErrorKind::CorruptState,
                "disposable attempt catalog revision is outside the bounded positive range",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    fn next(self) -> Result<Self, DisposableAttemptCatalogError> {
        let value = self.0.checked_add(1).ok_or_else(|| {
            catalog_error(
                DisposableAttemptCatalogErrorKind::Conflict,
                "disposable attempt catalog revision cannot advance",
            )
        })?;
        Self::new(value).map_err(|_| {
            catalog_error(
                DisposableAttemptCatalogErrorKind::Conflict,
                "disposable attempt catalog revision cannot advance",
            )
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisposableAttemptReservation {
    attempt: DisposableAttemptState,
    resources: DisposableWorkerResources,
    prepared_template_identity: DisposablePreparedTemplateIdentity,
}

impl DisposableAttemptReservation {
    /// Bind one reserved attempt to the exact host resources and prepared-template generation it
    /// owns until release completes.
    ///
    /// # Errors
    ///
    /// Returns an error unless the attempt is the first reserved revision.
    pub fn new(
        attempt: DisposableAttemptState,
        resources: DisposableWorkerResources,
        prepared_template_identity: DisposablePreparedTemplateIdentity,
    ) -> Result<Self, DisposableAttemptCatalogError> {
        if attempt.phase() != DisposableAttemptPhase::Reserved || attempt.revision().get() != 1 {
            return Err(catalog_error(
                DisposableAttemptCatalogErrorKind::CorruptState,
                "new disposable reservation must begin at reserved attempt revision one",
            ));
        }
        Ok(Self {
            attempt,
            resources,
            prepared_template_identity,
        })
    }

    #[must_use]
    pub const fn attempt(&self) -> &DisposableAttemptState {
        &self.attempt
    }

    #[must_use]
    pub const fn resources(&self) -> DisposableWorkerResources {
        self.resources
    }

    /// Return the exact prepared-template generation reserved for this attempt.
    #[must_use]
    pub const fn prepared_template_identity(&self) -> &DisposablePreparedTemplateIdentity {
        &self.prepared_template_identity
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisposableAttemptCatalogDocument {
    schema_version: u8,
    revision: DisposableAttemptCatalogRevision,
    active: Vec<DisposableAttemptReservation>,
    tombstones: Vec<DisposableAttemptState>,
}

impl DisposableAttemptCatalogDocument {
    /// Construct the canonical empty catalog at revision one.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            schema_version: DISPOSABLE_ATTEMPT_CATALOG_SCHEMA_VERSION,
            revision: DisposableAttemptCatalogRevision(1),
            active: Vec::new(),
            tombstones: Vec::new(),
        }
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn revision(&self) -> DisposableAttemptCatalogRevision {
        self.revision
    }

    #[must_use]
    pub fn active(&self) -> &[DisposableAttemptReservation] {
        &self.active
    }

    #[must_use]
    pub fn tombstones(&self) -> &[DisposableAttemptState] {
        &self.tombstones
    }

    #[must_use]
    pub fn find_active(
        &self,
        attempt_id: &DisposableAttemptId,
    ) -> Option<&DisposableAttemptReservation> {
        self.active
            .iter()
            .find(|reservation| reservation.attempt.attempt_id() == attempt_id)
    }

    #[must_use]
    pub fn find_tombstone(
        &self,
        attempt_id: &DisposableAttemptId,
    ) -> Option<&DisposableAttemptState> {
        self.tombstones
            .iter()
            .find(|attempt| attempt.attempt_id() == attempt_id)
    }

    /// Derive the exact currently reserved host usage from durable active attempts.
    ///
    /// A `complete` attempt has already released capacity and therefore remains catalogued without
    /// contributing resource usage until it is moved into bounded replay history.
    ///
    /// # Errors
    ///
    /// Returns an error if the durable resource sum exceeds the bounded worker-resource envelope.
    pub fn host_usage(&self) -> Result<DisposableHostUsage, DisposableAttemptCatalogError> {
        let mut workers = 0_u16;
        let mut cpu_millis = 0_u32;
        let mut memory_bytes = 0_u64;
        let mut disk_bytes = 0_u64;

        for reservation in &self.active {
            if reservation.attempt.phase() == DisposableAttemptPhase::Complete {
                continue;
            }
            workers = workers.checked_add(1).ok_or_else(resource_overflow)?;
            cpu_millis = cpu_millis
                .checked_add(reservation.resources.cpu_millis())
                .ok_or_else(resource_overflow)?;
            memory_bytes = memory_bytes
                .checked_add(reservation.resources.memory_bytes())
                .ok_or_else(resource_overflow)?;
            disk_bytes = disk_bytes
                .checked_add(reservation.resources.disk_bytes())
                .ok_or_else(resource_overflow)?;
        }

        if workers == 0 {
            return Ok(DisposableHostUsage::zero());
        }
        let resources = DisposableWorkerResources::new(cpu_millis, memory_bytes, disk_bytes)
            .map_err(|_| resource_overflow())?;
        DisposableHostUsage::new(workers, resources).map_err(|_| resource_overflow())
    }

    pub(crate) fn reserve(
        &self,
        reservation: DisposableAttemptReservation,
    ) -> Result<Self, DisposableAttemptCatalogError> {
        if let Some(existing) = self.find_active(reservation.attempt.attempt_id()) {
            if existing == &reservation {
                return Ok(self.clone());
            }
            return Err(catalog_error(
                DisposableAttemptCatalogErrorKind::AlreadyExists,
                "disposable attempt already exists with different durable evidence",
            ));
        }
        if self
            .find_tombstone(reservation.attempt.attempt_id())
            .is_some()
        {
            return Err(catalog_error(
                DisposableAttemptCatalogErrorKind::AlreadyExists,
                "completed disposable attempt identity remains in replay history",
            ));
        }
        if self.active.len() >= MAX_ACTIVE_DISPOSABLE_ATTEMPTS {
            return Err(catalog_error(
                DisposableAttemptCatalogErrorKind::LimitExceeded,
                "active disposable attempt limit is reached",
            ));
        }

        let mut next = self.clone();
        next.revision = self.revision.next()?;
        next.active.push(reservation);
        next.validate()?;
        Ok(next)
    }

    pub(crate) fn replace_attempt(
        &self,
        attempt_id: &DisposableAttemptId,
        expected_attempt_revision: DisposableAttemptRevision,
        action: DisposableAttemptCatalogAction,
    ) -> Result<Self, DisposableAttemptCatalogError> {
        let index = self
            .active
            .iter()
            .position(|reservation| reservation.attempt.attempt_id() == attempt_id)
            .ok_or_else(|| {
                catalog_error(
                    DisposableAttemptCatalogErrorKind::Missing,
                    "disposable attempt does not exist",
                )
            })?;
        let current = &self.active[index];
        if current.attempt.revision() != expected_attempt_revision {
            return Err(stale_attempt_revision(
                expected_attempt_revision,
                current.attempt.revision(),
            ));
        }
        let successor = apply_action(&current.attempt, action)?;
        if successor == current.attempt {
            return Ok(self.clone());
        }

        let mut next = self.clone();
        next.revision = self.revision.next()?;
        next.active[index].attempt = successor;
        next.validate()?;
        Ok(next)
    }

    pub(crate) fn retire_complete(
        &self,
        attempt_id: &DisposableAttemptId,
        expected_attempt_revision: DisposableAttemptRevision,
    ) -> Result<Self, DisposableAttemptCatalogError> {
        let index = self
            .active
            .iter()
            .position(|reservation| reservation.attempt.attempt_id() == attempt_id)
            .ok_or_else(|| {
                catalog_error(
                    DisposableAttemptCatalogErrorKind::Missing,
                    "disposable attempt does not exist",
                )
            })?;
        let current = &self.active[index];
        if current.attempt.revision() != expected_attempt_revision {
            return Err(stale_attempt_revision(
                expected_attempt_revision,
                current.attempt.revision(),
            ));
        }
        if current.attempt.phase() != DisposableAttemptPhase::Complete {
            return Err(catalog_error(
                DisposableAttemptCatalogErrorKind::InvalidAction,
                "only a complete disposable attempt may enter replay history",
            ));
        }

        let mut next = self.clone();
        next.revision = self.revision.next()?;
        let retired = next.active.remove(index).attempt;
        next.tombstones.push(retired);
        if next.tombstones.len() > MAX_DISPOSABLE_ATTEMPT_TOMBSTONES {
            next.tombstones.remove(0);
        }
        next.validate()?;
        Ok(next)
    }

    /// Bind the exact post-clone VM identity through the private runtime transaction.
    pub(crate) fn bind_vm_identity_after_clone(
        &self,
        attempt_id: &DisposableAttemptId,
        expected_attempt_revision: DisposableAttemptRevision,
        identity: DisposableVmIdentity,
    ) -> Result<Self, DisposableAttemptCatalogError> {
        let index = self
            .active
            .iter()
            .position(|reservation| reservation.attempt.attempt_id() == attempt_id)
            .ok_or_else(|| {
                catalog_error(
                    DisposableAttemptCatalogErrorKind::Missing,
                    "disposable attempt does not exist",
                )
            })?;
        let current = &self.active[index];
        if current.attempt.revision() != expected_attempt_revision {
            return Err(stale_attempt_revision(
                expected_attempt_revision,
                current.attempt.revision(),
            ));
        }
        let successor = current
            .attempt
            .bind_vm_identity_after_clone(identity)
            .map_err(|_| {
                catalog_error(
                    DisposableAttemptCatalogErrorKind::InvalidAction,
                    "clone completion cannot bind VM identity",
                )
            })?;
        let mut next = self.clone();
        next.revision = self.revision.next()?;
        next.active[index].attempt = successor;
        next.validate()?;
        Ok(next)
    }

    fn validate(&self) -> Result<(), DisposableAttemptCatalogError> {
        if self.schema_version != DISPOSABLE_ATTEMPT_CATALOG_SCHEMA_VERSION {
            return Err(catalog_error(
                DisposableAttemptCatalogErrorKind::CorruptState,
                "disposable attempt catalog schema version is unsupported",
            ));
        }
        if self.active.len() > MAX_ACTIVE_DISPOSABLE_ATTEMPTS
            || self.tombstones.len() > MAX_DISPOSABLE_ATTEMPT_TOMBSTONES
        {
            return Err(catalog_error(
                DisposableAttemptCatalogErrorKind::CorruptState,
                "disposable attempt catalog exceeds a reviewed entry bound",
            ));
        }

        // Catalog revision one is the empty document. Reserving an attempt and every later
        // attempt-revision advance each consume one catalog revision; retirement consumes one
        // more. Before replay history fills this sum is exact. Once the FIFO is full, evicted
        // tombstones make the retained sum only a lower bound on the monotonic catalog revision.
        let represented_revision = self
            .active
            .iter()
            .try_fold(1_u64, |revision, reservation| {
                revision.checked_add(reservation.attempt.revision().get())
            })
            .and_then(|revision| {
                self.tombstones
                    .iter()
                    .try_fold(revision, |revision, attempt| {
                        revision
                            .checked_add(attempt.revision().get())?
                            .checked_add(1)
                    })
            })
            .ok_or_else(|| {
                catalog_error(
                    DisposableAttemptCatalogErrorKind::CorruptState,
                    "disposable attempt catalog represented revision overflows",
                )
            })?;
        let revision_is_consistent = if self.tombstones.len() < MAX_DISPOSABLE_ATTEMPT_TOMBSTONES {
            self.revision.get() == represented_revision
        } else {
            self.revision.get() >= represented_revision
        };
        if !revision_is_consistent {
            return Err(catalog_error(
                DisposableAttemptCatalogErrorKind::CorruptState,
                "disposable attempt catalog revision conflicts with represented history",
            ));
        }
        if self.active.iter().any(|reservation| {
            reservation.attempt.phase() == DisposableAttemptPhase::Reserved
                && reservation.attempt.revision().get() != 1
        }) {
            return Err(catalog_error(
                DisposableAttemptCatalogErrorKind::CorruptState,
                "reserved disposable attempt must remain at attempt revision one",
            ));
        }

        let mut attempt_ids = BTreeSet::new();
        let mut claim_ids = BTreeSet::new();
        let mut vm_ids = BTreeSet::new();
        let mut vm_identities = BTreeSet::new();
        let mut runner_names = BTreeSet::new();
        let mut runner_request_ids = BTreeSet::new();
        let mut runner_ids = BTreeSet::new();
        let mut job_ids = BTreeSet::new();

        for attempt in self
            .active
            .iter()
            .map(|reservation| &reservation.attempt)
            .chain(self.tombstones.iter())
        {
            if !attempt_ids.insert(attempt.attempt_id().clone())
                || !claim_ids.insert(attempt.capacity_claim_id().clone())
                || !vm_ids.insert(attempt.vm_id().clone())
                || !runner_names.insert(attempt.runner_name().clone())
                || !runner_request_ids.insert(attempt.runner_request_id())
            {
                return Err(duplicate_identity());
            }
            if attempt
                .vm_identity()
                .is_some_and(|identity| !vm_identities.insert(identity.clone()))
            {
                return Err(duplicate_identity());
            }
            if attempt
                .runner_id()
                .is_some_and(|runner_id| !runner_ids.insert(runner_id))
            {
                return Err(duplicate_identity());
            }
            if attempt
                .github_job_id()
                .is_some_and(|job_id| !job_ids.insert(job_id.clone()))
            {
                return Err(duplicate_identity());
            }
        }
        if self
            .tombstones
            .iter()
            .any(|attempt| attempt.phase() != DisposableAttemptPhase::Complete)
        {
            return Err(catalog_error(
                DisposableAttemptCatalogErrorKind::CorruptState,
                "disposable attempt replay history contains an incomplete attempt",
            ));
        }
        self.host_usage()?;
        Ok(())
    }

    pub(crate) fn validate_successor_of(
        &self,
        current: &Self,
    ) -> Result<(), DisposableAttemptCatalogError> {
        current.validate()?;
        self.validate()?;
        if self.revision != current.revision.next()? {
            return Err(invalid_store_successor());
        }

        let reservation = self
            .active
            .last()
            .filter(|_| self.active.len() == current.active.len() + 1)
            .and_then(|reservation| current.reserve(reservation.clone()).ok())
            .is_some_and(|candidate| candidate == *self);
        let transition =
            if self.active.len() == current.active.len() && self.tombstones == current.tombstones {
                let mut changes = self
                    .active
                    .iter()
                    .zip(&current.active)
                    .filter(|(next, prior)| next != prior);
                changes.next().is_some_and(|(next, prior)| {
                    changes.next().is_none()
                        && next.resources == prior.resources
                        && next.prepared_template_identity == prior.prepared_template_identity
                        && next.attempt.is_exact_successor_of(&prior.attempt)
                })
            } else {
                false
            };
        let retirement = current.active.iter().any(|reservation| {
            current
                .retire_complete(
                    reservation.attempt.attempt_id(),
                    reservation.attempt.revision(),
                )
                .is_ok_and(|candidate| candidate == *self)
        });
        if reservation || transition || retirement {
            Ok(())
        } else {
            Err(invalid_store_successor())
        }
    }

    /// Validate either an ordinary successor or the one private post-clone identity bind.
    pub(crate) fn validate_recovery_successor_of(
        &self,
        current: &Self,
    ) -> Result<(), DisposableAttemptCatalogError> {
        if self.validate_successor_of(current).is_ok() {
            return Ok(());
        }
        current.validate()?;
        self.validate()?;
        if self.revision != current.revision.next()?
            || self.active.len() != current.active.len()
            || self.tombstones != current.tombstones
        {
            return Err(invalid_store_successor());
        }
        let mut changes = self
            .active
            .iter()
            .zip(&current.active)
            .filter(|(next, prior)| next != prior);
        let valid = changes.next().is_some_and(|(next, prior)| {
            changes.next().is_none()
                && next.resources == prior.resources
                && next.prepared_template_identity == prior.prepared_template_identity
                && prior.attempt.phase() == DisposableAttemptPhase::CloneStarted
                && prior.attempt.revision().get() == 3
                && prior.attempt.vm_identity().is_none()
                && next.attempt.phase() == DisposableAttemptPhase::CloneStarted
                && next.attempt.revision().get() == 4
                && next.attempt.vm_identity().is_some()
                && next.attempt.attempt_id() == prior.attempt.attempt_id()
                && next.attempt.capacity_claim_id() == prior.attempt.capacity_claim_id()
                && next.attempt.vm_id() == prior.attempt.vm_id()
                && next.attempt.runner_name() == prior.attempt.runner_name()
                && next.attempt.runner_request_id() == prior.attempt.runner_request_id()
                && next.attempt.runner_id() == prior.attempt.runner_id()
                && next.attempt.github_job_id() == prior.attempt.github_job_id()
                && next.attempt.result() == prior.attempt.result()
                && next.attempt.not_after() == prior.attempt.not_after()
        });
        if valid {
            Ok(())
        } else {
            Err(invalid_store_successor())
        }
    }

    pub(crate) fn checkpoint_clone_started(
        &self,
        attempt_id: &DisposableAttemptId,
        expected_attempt_revision: DisposableAttemptRevision,
    ) -> Result<Self, DisposableAttemptCatalogError> {
        self.replace_attempt(
            attempt_id,
            expected_attempt_revision,
            DisposableAttemptCatalogAction::RecordCloneStarted,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "transition", rename_all = "snake_case")]
pub enum DisposableAttemptCatalogAction {
    /// Legacy API action retained for explicit fail-closed classification; it never advances v3.
    BeginProvisioning,
    AuthorizeClone,
    RecordCloneStarted,
    BeginUnprovisionedRelease,
    CompleteUnprovisioned,
    BeginRegistration,
    RecordRegistration(ScaleSetRunnerReference),
    RecordRunnerReady(ScaleSetRunnerReference),
    RecordAssigned(ScaleSetJobId),
    RecordRunning {
        runner: ScaleSetRunnerReference,
        job_id: ScaleSetJobId,
    },
    RecordTerminal {
        runner: Option<ScaleSetRunnerReference>,
        job_id: ScaleSetJobId,
        result: ScaleSetJobResult,
    },
    BeginCleanup,
    AdvanceCleanup(DisposableAttemptPhase),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableAttemptCatalogWriteDisposition {
    Created,
    Replaced,
    Satisfied,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DisposableAttemptCatalogWriteReceipt {
    pub disposition: DisposableAttemptCatalogWriteDisposition,
    pub catalog_revision: DisposableAttemptCatalogRevision,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt_revision: Option<DisposableAttemptRevision>,
}

impl DisposableAttemptCatalogWriteReceipt {
    #[must_use]
    pub const fn new(
        disposition: DisposableAttemptCatalogWriteDisposition,
        catalog_revision: DisposableAttemptCatalogRevision,
        attempt_revision: Option<DisposableAttemptRevision>,
    ) -> Self {
        Self {
            disposition,
            catalog_revision,
            attempt_revision,
        }
    }
}

pub trait DisposableAttemptCatalogStore {
    /// Recover or refuse any unsettled durable publication before a mutation reads current state.
    fn recover(&mut self) -> Result<(), DisposableAttemptCatalogError> {
        Ok(())
    }

    fn load(
        &self,
    ) -> Result<Option<DisposableAttemptCatalogDocument>, DisposableAttemptCatalogError>;

    fn create(
        &mut self,
        document: &DisposableAttemptCatalogDocument,
    ) -> Result<DisposableAttemptCatalogWriteReceipt, DisposableAttemptCatalogError>;

    fn replace_if_revision(
        &mut self,
        expected_revision: DisposableAttemptCatalogRevision,
        document: &DisposableAttemptCatalogDocument,
    ) -> Result<DisposableAttemptCatalogWriteReceipt, DisposableAttemptCatalogError>;
}

#[derive(Debug)]
pub struct DisposableAttemptCatalog<S> {
    store: S,
}

impl<S> DisposableAttemptCatalog<S> {
    #[must_use]
    pub const fn new(store: S) -> Self {
        Self { store }
    }

    #[must_use]
    pub fn into_store(self) -> S {
        self.store
    }
}

impl<S: DisposableAttemptCatalogStore> DisposableAttemptCatalog<S> {
    /// Create the empty canonical catalog once or return the existing snapshot unchanged.
    ///
    /// # Errors
    ///
    /// Returns a store error if durable state cannot be loaded or initialized.
    pub fn initialize(
        &mut self,
    ) -> Result<
        (
            DisposableAttemptCatalogDocument,
            DisposableAttemptCatalogWriteReceipt,
        ),
        DisposableAttemptCatalogError,
    > {
        self.store.recover()?;
        if let Some(current) = self.store.load()? {
            current.validate()?;
            return Ok((
                current.clone(),
                DisposableAttemptCatalogWriteReceipt::new(
                    DisposableAttemptCatalogWriteDisposition::Satisfied,
                    current.revision(),
                    None,
                ),
            ));
        }
        let document = DisposableAttemptCatalogDocument::empty();
        match self.store.create(&document) {
            Ok(receipt) => Ok((document, receipt)),
            Err(error) if error.kind() == DisposableAttemptCatalogErrorKind::AlreadyExists => {
                let current = self.store.load()?.ok_or(error)?;
                current.validate()?;
                Ok((
                    current.clone(),
                    DisposableAttemptCatalogWriteReceipt::new(
                        DisposableAttemptCatalogWriteDisposition::Satisfied,
                        current.revision(),
                        None,
                    ),
                ))
            }
            Err(error) => Err(error),
        }
    }

    /// Load and validate the current catalog without mutation.
    ///
    /// # Errors
    ///
    /// Returns `Missing` before initialization or a bounded store/corruption error.
    pub fn load(&self) -> Result<DisposableAttemptCatalogDocument, DisposableAttemptCatalogError> {
        let document = self.store.load()?.ok_or_else(|| {
            catalog_error(
                DisposableAttemptCatalogErrorKind::Missing,
                "disposable attempt catalog is not initialized",
            )
        })?;
        document.validate()?;
        Ok(document)
    }

    /// Atomically reserve one attempt and its host resources under the expected catalog revision.
    ///
    /// Exact duplicate reservation is idempotent only while the persisted attempt remains the same
    /// reserved revision. Identity reuse after progress or completion fails closed.
    pub fn reserve(
        &mut self,
        expected_catalog_revision: DisposableAttemptCatalogRevision,
        reservation: DisposableAttemptReservation,
    ) -> Result<
        (
            DisposableAttemptCatalogDocument,
            DisposableAttemptCatalogWriteReceipt,
        ),
        DisposableAttemptCatalogError,
    > {
        self.store.recover()?;
        let current = self.load()?;
        require_catalog_revision(&current, expected_catalog_revision)?;
        let next = current.reserve(reservation.clone())?;
        if next == current {
            return Ok((
                current.clone(),
                DisposableAttemptCatalogWriteReceipt::new(
                    DisposableAttemptCatalogWriteDisposition::Satisfied,
                    current.revision(),
                    Some(reservation.attempt.revision()),
                ),
            ));
        }
        let receipt = self
            .store
            .replace_if_revision(expected_catalog_revision, &next)?;
        Ok((
            next,
            DisposableAttemptCatalogWriteReceipt {
                attempt_revision: Some(reservation.attempt.revision()),
                ..receipt
            },
        ))
    }

    /// Atomically apply one typed lifecycle observation/action to one exact attempt revision.
    pub fn transition(
        &mut self,
        expected_catalog_revision: DisposableAttemptCatalogRevision,
        attempt_id: &DisposableAttemptId,
        expected_attempt_revision: DisposableAttemptRevision,
        action: DisposableAttemptCatalogAction,
    ) -> Result<
        (
            DisposableAttemptCatalogDocument,
            DisposableAttemptCatalogWriteReceipt,
        ),
        DisposableAttemptCatalogError,
    > {
        self.store.recover()?;
        let current = self.load()?;
        require_catalog_revision(&current, expected_catalog_revision)?;
        let next = current.replace_attempt(attempt_id, expected_attempt_revision, action)?;
        if next == current {
            return Ok((
                current.clone(),
                DisposableAttemptCatalogWriteReceipt::new(
                    DisposableAttemptCatalogWriteDisposition::Satisfied,
                    current.revision(),
                    Some(expected_attempt_revision),
                ),
            ));
        }
        let attempt_revision = next
            .find_active(attempt_id)
            .map(|reservation| reservation.attempt.revision());
        let receipt = self
            .store
            .replace_if_revision(expected_catalog_revision, &next)?;
        Ok((
            next,
            DisposableAttemptCatalogWriteReceipt {
                attempt_revision,
                ..receipt
            },
        ))
    }

    /// Move one complete attempt into bounded replay history and stop counting its catalog slot.
    pub fn retire_complete(
        &mut self,
        expected_catalog_revision: DisposableAttemptCatalogRevision,
        attempt_id: &DisposableAttemptId,
        expected_attempt_revision: DisposableAttemptRevision,
    ) -> Result<
        (
            DisposableAttemptCatalogDocument,
            DisposableAttemptCatalogWriteReceipt,
        ),
        DisposableAttemptCatalogError,
    > {
        self.store.recover()?;
        let current = self.load()?;
        require_catalog_revision(&current, expected_catalog_revision)?;
        let next = current.retire_complete(attempt_id, expected_attempt_revision)?;
        let receipt = self
            .store
            .replace_if_revision(expected_catalog_revision, &next)?;
        Ok((
            next,
            DisposableAttemptCatalogWriteReceipt {
                attempt_revision: Some(expected_attempt_revision),
                ..receipt
            },
        ))
    }
}

#[derive(Debug, Default)]
pub struct MemoryDisposableAttemptCatalogStore {
    document: Option<DisposableAttemptCatalogDocument>,
}

impl DisposableAttemptCatalogStore for MemoryDisposableAttemptCatalogStore {
    fn load(
        &self,
    ) -> Result<Option<DisposableAttemptCatalogDocument>, DisposableAttemptCatalogError> {
        Ok(self.document.clone())
    }

    fn create(
        &mut self,
        document: &DisposableAttemptCatalogDocument,
    ) -> Result<DisposableAttemptCatalogWriteReceipt, DisposableAttemptCatalogError> {
        if self.document.is_some() {
            return Err(catalog_error(
                DisposableAttemptCatalogErrorKind::AlreadyExists,
                "disposable attempt catalog already exists",
            ));
        }
        document.validate()?;
        if document != &DisposableAttemptCatalogDocument::empty() {
            return Err(invalid_store_successor());
        }
        self.document = Some(document.clone());
        Ok(DisposableAttemptCatalogWriteReceipt::new(
            DisposableAttemptCatalogWriteDisposition::Created,
            document.revision(),
            None,
        ))
    }

    fn replace_if_revision(
        &mut self,
        expected_revision: DisposableAttemptCatalogRevision,
        document: &DisposableAttemptCatalogDocument,
    ) -> Result<DisposableAttemptCatalogWriteReceipt, DisposableAttemptCatalogError> {
        let current = self.document.as_ref().ok_or_else(|| {
            catalog_error(
                DisposableAttemptCatalogErrorKind::Missing,
                "disposable attempt catalog is not initialized",
            )
        })?;
        if current.revision() != expected_revision {
            return Err(stale_catalog_revision(
                expected_revision,
                current.revision(),
            ));
        }
        document.validate_successor_of(current)?;
        self.document = Some(document.clone());
        Ok(DisposableAttemptCatalogWriteReceipt::new(
            DisposableAttemptCatalogWriteDisposition::Replaced,
            document.revision(),
            None,
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableAttemptCatalogErrorKind {
    AlreadyExists,
    Missing,
    Conflict,
    IdentityDrift,
    InvalidAction,
    LimitExceeded,
    CorruptState,
    Busy,
    Io,
    UnsafeFilesystem,
    VersionIncompatible,
    RecoveryRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DisposableAttemptCatalogError {
    kind: DisposableAttemptCatalogErrorKind,
    message: &'static str,
}

impl DisposableAttemptCatalogError {
    #[must_use]
    pub const fn kind(&self) -> DisposableAttemptCatalogErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn message(&self) -> &'static str {
        self.message
    }

    #[must_use]
    pub(crate) const fn from_store(
        kind: DisposableAttemptCatalogErrorKind,
        message: &'static str,
    ) -> Self {
        Self { kind, message }
    }
}

impl fmt::Display for DisposableAttemptCatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for DisposableAttemptCatalogError {}

fn apply_action(
    current: &DisposableAttemptState,
    action: DisposableAttemptCatalogAction,
) -> Result<DisposableAttemptState, DisposableAttemptCatalogError> {
    let result = match action {
        DisposableAttemptCatalogAction::BeginProvisioning => current.begin_provisioning(),
        DisposableAttemptCatalogAction::AuthorizeClone => current.authorize_clone(),
        DisposableAttemptCatalogAction::RecordCloneStarted => current.record_clone_started(),
        DisposableAttemptCatalogAction::BeginUnprovisionedRelease => {
            current.begin_unprovisioned_release()
        }
        DisposableAttemptCatalogAction::CompleteUnprovisioned => current.complete_unprovisioned(),
        DisposableAttemptCatalogAction::BeginRegistration => current.begin_registration(),
        DisposableAttemptCatalogAction::RecordRegistration(runner) => {
            current.record_registration(&runner)
        }
        DisposableAttemptCatalogAction::RecordRunnerReady(runner) => {
            current.record_runner_ready(&runner)
        }
        DisposableAttemptCatalogAction::RecordAssigned(job_id) => current.record_assigned(job_id),
        DisposableAttemptCatalogAction::RecordRunning { runner, job_id } => {
            current.record_running(&runner, job_id)
        }
        DisposableAttemptCatalogAction::RecordTerminal {
            runner,
            job_id,
            result,
        } => current.record_terminal(runner.as_ref(), job_id, result),
        DisposableAttemptCatalogAction::BeginCleanup => current.begin_cleanup(),
        DisposableAttemptCatalogAction::AdvanceCleanup(phase) => current.advance_cleanup(phase),
    };
    result.map_err(|error| {
        let kind = if error.code() == "identity_drift" {
            DisposableAttemptCatalogErrorKind::IdentityDrift
        } else {
            DisposableAttemptCatalogErrorKind::InvalidAction
        };
        catalog_error(
            kind,
            match error.code() {
                "identity_drift" => "disposable attempt action conflicts with durable identity",
                "invalid_transition" => {
                    "disposable attempt action is invalid for the current phase"
                }
                _ => "disposable attempt action produced invalid durable state",
            },
        )
    })
}

fn require_catalog_revision(
    current: &DisposableAttemptCatalogDocument,
    expected: DisposableAttemptCatalogRevision,
) -> Result<(), DisposableAttemptCatalogError> {
    if current.revision() != expected {
        return Err(stale_catalog_revision(expected, current.revision()));
    }
    Ok(())
}

fn stale_catalog_revision(
    _expected: DisposableAttemptCatalogRevision,
    _actual: DisposableAttemptCatalogRevision,
) -> DisposableAttemptCatalogError {
    catalog_error(
        DisposableAttemptCatalogErrorKind::Conflict,
        "stale disposable attempt catalog revision",
    )
}

fn stale_attempt_revision(
    _expected: DisposableAttemptRevision,
    _actual: DisposableAttemptRevision,
) -> DisposableAttemptCatalogError {
    catalog_error(
        DisposableAttemptCatalogErrorKind::Conflict,
        "stale disposable attempt revision",
    )
}

fn duplicate_identity() -> DisposableAttemptCatalogError {
    catalog_error(
        DisposableAttemptCatalogErrorKind::CorruptState,
        "disposable attempt catalog contains duplicate ownership identity",
    )
}

fn resource_overflow() -> DisposableAttemptCatalogError {
    catalog_error(
        DisposableAttemptCatalogErrorKind::LimitExceeded,
        "durable disposable resource usage exceeds the bounded host envelope",
    )
}

fn invalid_store_successor() -> DisposableAttemptCatalogError {
    catalog_error(
        DisposableAttemptCatalogErrorKind::Conflict,
        "replacement catalog is not one exact authorized successor",
    )
}

const fn catalog_error(
    kind: DisposableAttemptCatalogErrorKind,
    message: &'static str,
) -> DisposableAttemptCatalogError {
    DisposableAttemptCatalogError { kind, message }
}

#[cfg(test)]
mod clone_identity_tests {
    use super::*;
    use crate::disposable_attempt_state::DisposableAttemptState;
    use crate::disposable_prepared_template::current_disposable_prepared_template;
    use crate::disposable_worker_reconciler::{
        CapacityClaimId, DisposableVmId, DisposableWorkerResources,
    };
    use crate::execution_admission::EpochMillis;
    use crate::github_scale_set_protocol::{ScaleSetRunnerName, ScaleSetRunnerRequestId};

    fn started_catalog() -> (DisposableAttemptCatalogDocument, DisposableAttemptId) {
        let mut catalog =
            DisposableAttemptCatalog::new(MemoryDisposableAttemptCatalogStore::default());
        let (empty, _) = catalog.initialize().unwrap();
        let attempt_id = DisposableAttemptId::parse("attempt-clone-bind").unwrap();
        let attempt = DisposableAttemptState::reserved(
            attempt_id.clone(),
            CapacityClaimId::parse("claim-clone-bind").unwrap(),
            DisposableVmId::parse("vm-clone-bind").unwrap(),
            ScaleSetRunnerName::parse("smol-clone-bind").unwrap(),
            ScaleSetRunnerRequestId::new(41).unwrap(),
            EpochMillis::new(50_000).unwrap(),
        );
        let reservation = DisposableAttemptReservation::new(
            attempt,
            DisposableWorkerResources::new(2_000, 2 << 30, 20 << 30).unwrap(),
            current_disposable_prepared_template()
                .unwrap()
                .identity()
                .unwrap(),
        )
        .unwrap();
        let (reserved, _) = catalog.reserve(empty.revision(), reservation).unwrap();
        let (authorized, _) = catalog
            .transition(
                reserved.revision(),
                &attempt_id,
                reserved
                    .find_active(&attempt_id)
                    .unwrap()
                    .attempt()
                    .revision(),
                DisposableAttemptCatalogAction::AuthorizeClone,
            )
            .unwrap();
        let (started, _) = catalog
            .transition(
                authorized.revision(),
                &attempt_id,
                authorized
                    .find_active(&attempt_id)
                    .unwrap()
                    .attempt()
                    .revision(),
                DisposableAttemptCatalogAction::RecordCloneStarted,
            )
            .unwrap();
        (started, attempt_id)
    }

    #[test]
    fn only_recovery_accepts_the_private_post_clone_identity_successor() {
        let (started, attempt_id) = started_catalog();
        let attempt_revision = started
            .find_active(&attempt_id)
            .unwrap()
            .attempt()
            .revision();
        let identity = DisposableVmIdentity::parse(&format!("sha256:{}", "44".repeat(32))).unwrap();
        let bound = started
            .bind_vm_identity_after_clone(&attempt_id, attempt_revision, identity)
            .unwrap();

        assert!(bound.validate_successor_of(&started).is_err());
        bound.validate_recovery_successor_of(&started).unwrap();
        assert_eq!(
            bound
                .find_active(&attempt_id)
                .unwrap()
                .attempt()
                .revision()
                .get(),
            4
        );
        assert!(
            bound
                .find_active(&attempt_id)
                .unwrap()
                .attempt()
                .vm_identity()
                .is_some()
        );
    }
}
