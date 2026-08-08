use std::fmt;

use serde::Serialize;

use crate::execution_admission::EpochMillis;
use crate::operator_config::{OperatorConfig, OperatorConfigIdentity};
use crate::operator_error::{OperatorErrorCode, OperatorPublicError};
use crate::personal_worker_queue::{
    PersonalWorkerActivityEvidence, PersonalWorkerProfileObservation,
    PersonalWorkerQueueGeneration, PersonalWorkerQueueInput,
};
use crate::personal_worker_store::{
    PERSONAL_WORKER_STORE_SCHEMA_VERSION, PersonalWorkerStoreDocument, PersonalWorkerStoreError,
    PersonalWorkerStoreErrorKind, PersonalWorkerStoreInitializationDisposition,
    PersonalWorkerStoreRevision,
};
#[cfg(unix)]
use crate::unix_personal_worker_store::{
    PersonalWorkerStoreReadOnlyInspection, UnixPersonalWorkerStore,
};

pub const PERSONAL_WORKER_OPERATOR_STORE_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersonalWorkerInitializationInput {
    initialized_at: EpochMillis,
}

impl PersonalWorkerInitializationInput {
    #[must_use]
    pub const fn new(initialized_at: EpochMillis) -> Self {
        Self { initialized_at }
    }

    #[must_use]
    pub const fn initialized_at(self) -> EpochMillis {
        self.initialized_at
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct OpenedPersonalWorkerState {
    config_identity: OperatorConfigIdentity,
    document: PersonalWorkerStoreDocument,
}

impl OpenedPersonalWorkerState {
    #[must_use]
    pub const fn config_identity(&self) -> &OperatorConfigIdentity {
        &self.config_identity
    }

    #[must_use]
    pub const fn document(&self) -> &PersonalWorkerStoreDocument {
        &self.document
    }
}

impl fmt::Debug for OpenedPersonalWorkerState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenedPersonalWorkerState")
            .field("config_identity", &self.config_identity)
            .field("store_revision", &self.document.revision())
            .field("queue_generation", &self.document.queue().generation)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerInitializationDisposition {
    Initialized,
    AlreadyInitialized,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerInitializationReceipt {
    schema_version: u8,
    disposition: PersonalWorkerInitializationDisposition,
    config_identity: OperatorConfigIdentity,
    durable_schema_version: u8,
    store_revision: PersonalWorkerStoreRevision,
    queue_generation: PersonalWorkerQueueGeneration,
    observed_at: EpochMillis,
    profile_observation: PersonalWorkerProfileObservation,
    activity_evidence: PersonalWorkerActivityEvidence,
    queued_count: u32,
    active_count: u32,
    cache_lease_count: u32,
    terminal_tombstone_count: u32,
    bytes_written: usize,
}

impl PersonalWorkerInitializationReceipt {
    fn from_document(
        disposition: PersonalWorkerInitializationDisposition,
        config_identity: OperatorConfigIdentity,
        document: &PersonalWorkerStoreDocument,
        bytes_written: usize,
    ) -> Result<Self, PersonalWorkerOperatorStoreError> {
        Ok(Self {
            schema_version: PERSONAL_WORKER_OPERATOR_STORE_SCHEMA_VERSION,
            disposition,
            config_identity,
            durable_schema_version: document.schema_version(),
            store_revision: document.revision(),
            queue_generation: document.queue().generation,
            observed_at: document.queue().observed_at,
            profile_observation: document.queue().profile_observation,
            activity_evidence: document.queue().activity_evidence,
            queued_count: bounded_count(document.queue().queued.len())?,
            active_count: bounded_count(document.queue().active.len())?,
            cache_lease_count: bounded_count(document.cache_leases().len())?,
            terminal_tombstone_count: bounded_count(document.terminal_tombstones().len())?,
            bytes_written,
        })
    }

    #[must_use]
    pub const fn disposition(&self) -> PersonalWorkerInitializationDisposition {
        self.disposition
    }

    #[must_use]
    pub const fn config_identity(&self) -> &OperatorConfigIdentity {
        &self.config_identity
    }

    #[must_use]
    pub const fn durable_schema_version(&self) -> u8 {
        self.durable_schema_version
    }

    #[must_use]
    pub const fn store_revision(&self) -> PersonalWorkerStoreRevision {
        self.store_revision
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
    pub const fn profile_observation(&self) -> PersonalWorkerProfileObservation {
        self.profile_observation
    }

    #[must_use]
    pub const fn activity_evidence(&self) -> PersonalWorkerActivityEvidence {
        self.activity_evidence
    }

    #[must_use]
    pub const fn queued_count(&self) -> u32 {
        self.queued_count
    }

    #[must_use]
    pub const fn active_count(&self) -> u32 {
        self.active_count
    }

    #[must_use]
    pub const fn cache_lease_count(&self) -> u32 {
        self.cache_lease_count
    }

    #[must_use]
    pub const fn terminal_tombstone_count(&self) -> u32 {
        self.terminal_tombstone_count
    }

    #[must_use]
    pub const fn bytes_written(&self) -> usize {
        self.bytes_written
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerOperatorStoreErrorKind {
    Missing,
    UnsafeFilesystem,
    CorruptState,
    VersionIncompatible,
    RecoveryRequired,
    Busy,
    Unavailable,
    UnsupportedPlatform,
    InvalidInitialState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerOperatorStoreError {
    kind: PersonalWorkerOperatorStoreErrorKind,
    public_error: OperatorPublicError,
}

impl PersonalWorkerOperatorStoreError {
    #[must_use]
    pub const fn kind(&self) -> PersonalWorkerOperatorStoreErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn public_error(&self) -> OperatorPublicError {
        self.public_error
    }

    const fn new(kind: PersonalWorkerOperatorStoreErrorKind, code: OperatorErrorCode) -> Self {
        Self {
            kind,
            public_error: OperatorPublicError::from_code(code),
        }
    }
}

impl fmt::Display for PersonalWorkerOperatorStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.public_error.fmt(formatter)
    }
}

impl std::error::Error for PersonalWorkerOperatorStoreError {}

pub struct PersonalWorkerOperatorStore;

impl PersonalWorkerOperatorStore {
    /// Open one config-bound current document without creating or recovering durable state.
    pub fn open_current(
        config: &OperatorConfig,
    ) -> Result<OpenedPersonalWorkerState, PersonalWorkerOperatorStoreError> {
        #[cfg(not(unix))]
        {
            let _ = config;
            Err(PersonalWorkerOperatorStoreError::new(
                PersonalWorkerOperatorStoreErrorKind::UnsupportedPlatform,
                OperatorErrorCode::UnsupportedPlatform,
            ))
        }
        #[cfg(unix)]
        {
            let store =
                UnixPersonalWorkerStore::open_existing_read_only(config.state_root().as_path())
                    .map_err(map_store_error)?;
            match store.inspect_read_only().map_err(map_store_error)? {
                PersonalWorkerStoreReadOnlyInspection::Missing => {
                    Err(PersonalWorkerOperatorStoreError::new(
                        PersonalWorkerOperatorStoreErrorKind::Missing,
                        OperatorErrorCode::DurableStateMissing,
                    ))
                }
                PersonalWorkerStoreReadOnlyInspection::Current(document) => {
                    Ok(OpenedPersonalWorkerState {
                        config_identity: config.identity().clone(),
                        document,
                    })
                }
                PersonalWorkerStoreReadOnlyInspection::RecoveryRequired { .. } => {
                    Err(PersonalWorkerOperatorStoreError::new(
                        PersonalWorkerOperatorStoreErrorKind::RecoveryRequired,
                        OperatorErrorCode::DurableStateRecoveryRequired,
                    ))
                }
            }
        }
    }

    /// Publish the exact v2 initial document only when current and staged state are absent.
    pub fn initialize(
        config: &OperatorConfig,
        input: PersonalWorkerInitializationInput,
    ) -> Result<PersonalWorkerInitializationReceipt, PersonalWorkerOperatorStoreError> {
        let initial = initial_document(input)?;
        #[cfg(not(unix))]
        {
            let _ = (config, initial);
            Err(PersonalWorkerOperatorStoreError::new(
                PersonalWorkerOperatorStoreErrorKind::UnsupportedPlatform,
                OperatorErrorCode::UnsupportedPlatform,
            ))
        }
        #[cfg(unix)]
        {
            let receipt = UnixPersonalWorkerStore::initialize_if_clean(
                config.state_root().as_path(),
                &initial,
            )
            .map_err(map_store_error)?;
            match receipt.disposition() {
                PersonalWorkerStoreInitializationDisposition::Created => {
                    PersonalWorkerInitializationReceipt::from_document(
                        PersonalWorkerInitializationDisposition::Initialized,
                        config.identity().clone(),
                        &initial,
                        receipt.bytes_written(),
                    )
                }
                PersonalWorkerStoreInitializationDisposition::AlreadyExists => {
                    let opened = Self::open_current(config)?;
                    PersonalWorkerInitializationReceipt::from_document(
                        PersonalWorkerInitializationDisposition::AlreadyInitialized,
                        config.identity().clone(),
                        opened.document(),
                        0,
                    )
                }
                PersonalWorkerStoreInitializationDisposition::RecoveryRequired => {
                    Err(PersonalWorkerOperatorStoreError::new(
                        PersonalWorkerOperatorStoreErrorKind::RecoveryRequired,
                        OperatorErrorCode::DurableStateRecoveryRequired,
                    ))
                }
            }
        }
    }
}

fn initial_document(
    input: PersonalWorkerInitializationInput,
) -> Result<PersonalWorkerStoreDocument, PersonalWorkerOperatorStoreError> {
    let queue = PersonalWorkerQueueInput {
        generation: PersonalWorkerQueueGeneration::new(1).map_err(|_| invalid_initial_state())?,
        observed_at: input.initialized_at(),
        profile_observation: PersonalWorkerProfileObservation::Unobserved,
        activity_evidence: PersonalWorkerActivityEvidence::Never,
        queued: Vec::new(),
        active: Vec::new(),
        pending_profile_change: None,
    };
    let document =
        PersonalWorkerStoreDocument::new(queue, Vec::new()).map_err(|_| invalid_initial_state())?;
    if document.schema_version() != PERSONAL_WORKER_STORE_SCHEMA_VERSION {
        return Err(invalid_initial_state());
    }
    Ok(document)
}

fn bounded_count(value: usize) -> Result<u32, PersonalWorkerOperatorStoreError> {
    u32::try_from(value).map_err(|_| invalid_initial_state())
}

const fn invalid_initial_state() -> PersonalWorkerOperatorStoreError {
    PersonalWorkerOperatorStoreError::new(
        PersonalWorkerOperatorStoreErrorKind::InvalidInitialState,
        OperatorErrorCode::DurableStateCorrupt,
    )
}

fn map_store_error(error: PersonalWorkerStoreError) -> PersonalWorkerOperatorStoreError {
    match error.kind() {
        PersonalWorkerStoreErrorKind::Missing => PersonalWorkerOperatorStoreError::new(
            PersonalWorkerOperatorStoreErrorKind::Missing,
            OperatorErrorCode::DurableStateMissing,
        ),
        PersonalWorkerStoreErrorKind::UnsafeFilesystem => PersonalWorkerOperatorStoreError::new(
            PersonalWorkerOperatorStoreErrorKind::UnsafeFilesystem,
            OperatorErrorCode::DurableStateUnsafe,
        ),
        PersonalWorkerStoreErrorKind::VersionIncompatible => PersonalWorkerOperatorStoreError::new(
            PersonalWorkerOperatorStoreErrorKind::VersionIncompatible,
            OperatorErrorCode::DurableStateVersionIncompatible,
        ),
        PersonalWorkerStoreErrorKind::CorruptState
        | PersonalWorkerStoreErrorKind::InvalidDocument => PersonalWorkerOperatorStoreError::new(
            PersonalWorkerOperatorStoreErrorKind::CorruptState,
            OperatorErrorCode::DurableStateCorrupt,
        ),
        PersonalWorkerStoreErrorKind::Busy => PersonalWorkerOperatorStoreError::new(
            PersonalWorkerOperatorStoreErrorKind::Busy,
            OperatorErrorCode::DurableStateBusy,
        ),
        PersonalWorkerStoreErrorKind::RevisionConflict | PersonalWorkerStoreErrorKind::Io => {
            PersonalWorkerOperatorStoreError::new(
                PersonalWorkerOperatorStoreErrorKind::Unavailable,
                OperatorErrorCode::DurableStateCorrupt,
            )
        }
    }
}
