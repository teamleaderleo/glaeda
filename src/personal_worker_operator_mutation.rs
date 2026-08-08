use std::fmt;

use serde::Serialize;

use crate::execution_admission::{
    EpochMillis, ExecutionAdmissionIdentity, ExecutionRequestId, ExecutionResourceLimits,
    FallbackProfileEligibility, RunnerProfileId,
};
use crate::operator_config::{OperatorConfig, OperatorConfigIdentity};
use crate::operator_error::{OperatorErrorCode, OperatorPublicError};
use crate::personal_worker_queue::{
    PersonalWorkerCacheAccessMode, PersonalWorkerCacheNamespace, PersonalWorkerCancellationState,
    PersonalWorkerJobRequest, PersonalWorkerPriority, PersonalWorkerQueueGeneration,
    PersonalWorkerSourceIdentity,
};
use crate::personal_worker_store::{
    PersonalWorkerStore, PersonalWorkerStoreDocument, PersonalWorkerStoreError,
    PersonalWorkerStoreErrorKind, PersonalWorkerStoreRevision,
};
use crate::personal_worker_store_transaction::{
    PersonalWorkerStoreMutation, PersonalWorkerStoreMutationError,
    PersonalWorkerStoreMutationErrorKind, PersonalWorkerStoreMutationReceipt,
    apply_personal_worker_store_mutation,
};
#[cfg(unix)]
use crate::unix_personal_worker_store::UnixPersonalWorkerStore;

pub const PERSONAL_WORKER_OPERATOR_MUTATION_SCHEMA_VERSION: u8 = 1;

#[derive(Clone, PartialEq, Eq)]
pub struct PersonalWorkerSubmissionInput {
    request_id: ExecutionRequestId,
    runner_profile_id: RunnerProfileId,
    source: PersonalWorkerSourceIdentity,
    priority: PersonalWorkerPriority,
    requested_limits: ExecutionResourceLimits,
    cache_namespace: PersonalWorkerCacheNamespace,
    cache_access: PersonalWorkerCacheAccessMode,
    submitted_at: EpochMillis,
    operator_deadline: Option<EpochMillis>,
}

impl PersonalWorkerSubmissionInput {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub const fn new(
        request_id: ExecutionRequestId,
        runner_profile_id: RunnerProfileId,
        source: PersonalWorkerSourceIdentity,
        priority: PersonalWorkerPriority,
        requested_limits: ExecutionResourceLimits,
        cache_namespace: PersonalWorkerCacheNamespace,
        cache_access: PersonalWorkerCacheAccessMode,
        submitted_at: EpochMillis,
        operator_deadline: Option<EpochMillis>,
    ) -> Self {
        Self {
            request_id,
            runner_profile_id,
            source,
            priority,
            requested_limits,
            cache_namespace,
            cache_access,
            submitted_at,
            operator_deadline,
        }
    }
}

impl fmt::Debug for PersonalWorkerSubmissionInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersonalWorkerSubmissionInput")
            .field("private_request", &"redacted")
            .field("submitted_at", &self.submitted_at)
            .field("operator_deadline", &self.operator_deadline)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersonalWorkerMutationExpectation {
    store_revision: PersonalWorkerStoreRevision,
    queue_generation: PersonalWorkerQueueGeneration,
    observed_at: EpochMillis,
}

impl PersonalWorkerMutationExpectation {
    #[must_use]
    pub const fn new(
        store_revision: PersonalWorkerStoreRevision,
        queue_generation: PersonalWorkerQueueGeneration,
        observed_at: EpochMillis,
    ) -> Self {
        Self {
            store_revision,
            queue_generation,
            observed_at,
        }
    }

    #[must_use]
    pub const fn store_revision(self) -> PersonalWorkerStoreRevision {
        self.store_revision
    }

    #[must_use]
    pub const fn queue_generation(self) -> PersonalWorkerQueueGeneration {
        self.queue_generation
    }

    #[must_use]
    pub const fn observed_at(self) -> EpochMillis {
        self.observed_at
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerOperatorMutationReceipt {
    schema_version: u8,
    config_identity: OperatorConfigIdentity,
    attempts: u8,
    mutation: PersonalWorkerStoreMutationReceipt,
}

impl PersonalWorkerOperatorMutationReceipt {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn config_identity(&self) -> &OperatorConfigIdentity {
        &self.config_identity
    }

    #[must_use]
    pub const fn attempts(&self) -> u8 {
        self.attempts
    }

    #[must_use]
    pub const fn mutation(&self) -> &PersonalWorkerStoreMutationReceipt {
        &self.mutation
    }
}

impl fmt::Debug for PersonalWorkerOperatorMutationReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersonalWorkerOperatorMutationReceipt")
            .field("schema_version", &self.schema_version)
            .field("config_identity", &self.config_identity)
            .field("attempts", &self.attempts)
            .field("mutation", &self.mutation)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerOperatorMutationErrorKind {
    Missing,
    UnsafeFilesystem,
    CorruptState,
    VersionIncompatible,
    Busy,
    Unavailable,
    UnsupportedPlatform,
    StaleRevision,
    StaleQueueGeneration,
    StaleObservation,
    NotFound,
    Conflict,
    CapacityReached,
    InvalidMutation,
    InvalidTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerOperatorMutationError {
    kind: PersonalWorkerOperatorMutationErrorKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    public_error: Option<OperatorPublicError>,
}

impl PersonalWorkerOperatorMutationError {
    #[must_use]
    pub const fn kind(&self) -> PersonalWorkerOperatorMutationErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn public_error(&self) -> Option<OperatorPublicError> {
        self.public_error
    }

    const fn new(
        kind: PersonalWorkerOperatorMutationErrorKind,
        public_error: Option<OperatorPublicError>,
    ) -> Self {
        Self { kind, public_error }
    }
}

impl fmt::Display for PersonalWorkerOperatorMutationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(error) = self.public_error {
            return error.fmt(formatter);
        }
        formatter.write_str(match self.kind {
            PersonalWorkerOperatorMutationErrorKind::StaleObservation => {
                "personal worker snapshot observation is stale"
            }
            PersonalWorkerOperatorMutationErrorKind::NotFound => {
                "personal worker mutation target does not exist"
            }
            PersonalWorkerOperatorMutationErrorKind::InvalidTime => {
                "personal worker mutation time evidence is invalid"
            }
            PersonalWorkerOperatorMutationErrorKind::InvalidMutation => {
                "personal worker mutation is invalid"
            }
            _ => "personal worker mutation could not be completed",
        })
    }
}

impl std::error::Error for PersonalWorkerOperatorMutationError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OperatorMutationClass {
    Submit,
    Cancel,
}

pub struct PersonalWorkerOperatorMutationService;

impl PersonalWorkerOperatorMutationService {
    pub fn submit(
        config: &OperatorConfig,
        input: PersonalWorkerSubmissionInput,
        observed_at: EpochMillis,
        expectation: Option<PersonalWorkerMutationExpectation>,
    ) -> Result<PersonalWorkerOperatorMutationReceipt, PersonalWorkerOperatorMutationError> {
        if input.submitted_at > observed_at
            || input
                .operator_deadline
                .is_some_and(|deadline| deadline <= input.submitted_at)
        {
            return Err(PersonalWorkerOperatorMutationError::new(
                PersonalWorkerOperatorMutationErrorKind::InvalidTime,
                None,
            ));
        }
        let request = PersonalWorkerJobRequest {
            identity: ExecutionAdmissionIdentity::new(
                input.request_id,
                config.default_verification_profile().clone(),
                input.runner_profile_id,
            ),
            source: input.source,
            priority: input.priority,
            requested_limits: input.requested_limits,
            cache_namespace: input.cache_namespace,
            cache_access: input.cache_access,
            submitted_at: input.submitted_at,
            operator_deadline: input.operator_deadline,
            cancellation: PersonalWorkerCancellationState::Active,
            fallback_eligibility: FallbackProfileEligibility::ineligible(),
        };
        apply_operator_mutation(
            config,
            PersonalWorkerStoreMutation::Submit {
                request,
                observed_at,
            },
            expectation,
            OperatorMutationClass::Submit,
        )
    }

    pub fn cancel_queued(
        config: &OperatorConfig,
        request_id: ExecutionRequestId,
        cancelled_at: EpochMillis,
        expectation: Option<PersonalWorkerMutationExpectation>,
    ) -> Result<PersonalWorkerOperatorMutationReceipt, PersonalWorkerOperatorMutationError> {
        apply_operator_mutation(
            config,
            PersonalWorkerStoreMutation::Cancel {
                request_id,
                cancelled_at,
                draining_admission: None,
            },
            expectation,
            OperatorMutationClass::Cancel,
        )
    }
}

#[cfg(unix)]
fn apply_operator_mutation(
    config: &OperatorConfig,
    mutation: PersonalWorkerStoreMutation,
    expectation: Option<PersonalWorkerMutationExpectation>,
    class: OperatorMutationClass,
) -> Result<PersonalWorkerOperatorMutationReceipt, PersonalWorkerOperatorMutationError> {
    let mut store = UnixPersonalWorkerStore::open_existing_read_only(config.state_root().as_path())
        .map_err(map_store_error)?;
    let current = recover_and_load(&mut store)?;
    let (revision, generation) = match expectation {
        Some(expectation) => {
            validate_expectation(&current, expectation)?;
            (expectation.store_revision(), expectation.queue_generation())
        }
        None => (current.revision(), current.queue().generation),
    };

    match apply_personal_worker_store_mutation(&mut store, revision, generation, mutation.clone()) {
        Ok(receipt) => Ok(wrap_receipt(config, 1, receipt)),
        Err(error)
            if expectation.is_none()
                && matches!(
                    error.kind(),
                    PersonalWorkerStoreMutationErrorKind::StaleRevision
                        | PersonalWorkerStoreMutationErrorKind::StaleQueueGeneration
                ) =>
        {
            let refreshed = recover_and_load(&mut store)?;
            let second = apply_personal_worker_store_mutation(
                &mut store,
                refreshed.revision(),
                refreshed.queue().generation,
                mutation,
            )
            .map_err(|error| map_mutation_error(error, class))?;
            Ok(wrap_receipt(config, 2, second))
        }
        Err(error) => Err(map_mutation_error(error, class)),
    }
}

#[cfg(not(unix))]
fn apply_operator_mutation(
    _config: &OperatorConfig,
    _mutation: PersonalWorkerStoreMutation,
    _expectation: Option<PersonalWorkerMutationExpectation>,
    _class: OperatorMutationClass,
) -> Result<PersonalWorkerOperatorMutationReceipt, PersonalWorkerOperatorMutationError> {
    Err(operator_error(
        PersonalWorkerOperatorMutationErrorKind::UnsupportedPlatform,
        OperatorErrorCode::UnsupportedPlatform,
    ))
}

fn wrap_receipt(
    config: &OperatorConfig,
    attempts: u8,
    mutation: PersonalWorkerStoreMutationReceipt,
) -> PersonalWorkerOperatorMutationReceipt {
    PersonalWorkerOperatorMutationReceipt {
        schema_version: PERSONAL_WORKER_OPERATOR_MUTATION_SCHEMA_VERSION,
        config_identity: config.identity().clone(),
        attempts,
        mutation,
    }
}

#[cfg(unix)]
fn recover_and_load(
    store: &mut UnixPersonalWorkerStore,
) -> Result<PersonalWorkerStoreDocument, PersonalWorkerOperatorMutationError> {
    store.recover().map_err(map_store_error)?;
    store.load().map_err(map_store_error)?.ok_or_else(|| {
        operator_error(
            PersonalWorkerOperatorMutationErrorKind::Missing,
            OperatorErrorCode::DurableStateMissing,
        )
    })
}

fn validate_expectation(
    current: &PersonalWorkerStoreDocument,
    expectation: PersonalWorkerMutationExpectation,
) -> Result<(), PersonalWorkerOperatorMutationError> {
    if current.revision() != expectation.store_revision() {
        return Err(operator_error(
            PersonalWorkerOperatorMutationErrorKind::StaleRevision,
            OperatorErrorCode::DurableStateRevisionStale,
        ));
    }
    if current.queue().generation != expectation.queue_generation() {
        return Err(operator_error(
            PersonalWorkerOperatorMutationErrorKind::StaleQueueGeneration,
            OperatorErrorCode::DurableStateGenerationStale,
        ));
    }
    if current.queue().observed_at != expectation.observed_at() {
        return Err(PersonalWorkerOperatorMutationError::new(
            PersonalWorkerOperatorMutationErrorKind::StaleObservation,
            None,
        ));
    }
    Ok(())
}

fn map_store_error(error: PersonalWorkerStoreError) -> PersonalWorkerOperatorMutationError {
    match error.kind() {
        PersonalWorkerStoreErrorKind::Missing => operator_error(
            PersonalWorkerOperatorMutationErrorKind::Missing,
            OperatorErrorCode::DurableStateMissing,
        ),
        PersonalWorkerStoreErrorKind::UnsafeFilesystem => operator_error(
            PersonalWorkerOperatorMutationErrorKind::UnsafeFilesystem,
            OperatorErrorCode::DurableStateUnsafe,
        ),
        PersonalWorkerStoreErrorKind::VersionIncompatible => operator_error(
            PersonalWorkerOperatorMutationErrorKind::VersionIncompatible,
            OperatorErrorCode::DurableStateVersionIncompatible,
        ),
        PersonalWorkerStoreErrorKind::CorruptState
        | PersonalWorkerStoreErrorKind::InvalidDocument => operator_error(
            PersonalWorkerOperatorMutationErrorKind::CorruptState,
            OperatorErrorCode::DurableStateCorrupt,
        ),
        PersonalWorkerStoreErrorKind::Busy => operator_error(
            PersonalWorkerOperatorMutationErrorKind::Busy,
            OperatorErrorCode::DurableStateBusy,
        ),
        PersonalWorkerStoreErrorKind::RevisionConflict | PersonalWorkerStoreErrorKind::Io => {
            PersonalWorkerOperatorMutationError::new(
                PersonalWorkerOperatorMutationErrorKind::Unavailable,
                None,
            )
        }
    }
}

fn map_mutation_error(
    error: PersonalWorkerStoreMutationError,
    class: OperatorMutationClass,
) -> PersonalWorkerOperatorMutationError {
    match error.kind() {
        PersonalWorkerStoreMutationErrorKind::MissingState => operator_error(
            PersonalWorkerOperatorMutationErrorKind::Missing,
            OperatorErrorCode::DurableStateMissing,
        ),
        PersonalWorkerStoreMutationErrorKind::StaleRevision => operator_error(
            PersonalWorkerOperatorMutationErrorKind::StaleRevision,
            OperatorErrorCode::DurableStateRevisionStale,
        ),
        PersonalWorkerStoreMutationErrorKind::StaleQueueGeneration => operator_error(
            PersonalWorkerOperatorMutationErrorKind::StaleQueueGeneration,
            OperatorErrorCode::DurableStateGenerationStale,
        ),
        PersonalWorkerStoreMutationErrorKind::NotFound => PersonalWorkerOperatorMutationError::new(
            PersonalWorkerOperatorMutationErrorKind::NotFound,
            None,
        ),
        PersonalWorkerStoreMutationErrorKind::Conflict => operator_error(
            PersonalWorkerOperatorMutationErrorKind::Conflict,
            match class {
                OperatorMutationClass::Submit => OperatorErrorCode::JobConflict,
                OperatorMutationClass::Cancel => OperatorErrorCode::CancellationConflict,
            },
        ),
        PersonalWorkerStoreMutationErrorKind::CapacityReached => operator_error(
            PersonalWorkerOperatorMutationErrorKind::CapacityReached,
            OperatorErrorCode::QueueCapacityReached,
        ),
        PersonalWorkerStoreMutationErrorKind::InvalidMutation => {
            PersonalWorkerOperatorMutationError::new(
                PersonalWorkerOperatorMutationErrorKind::InvalidMutation,
                None,
            )
        }
        PersonalWorkerStoreMutationErrorKind::Busy => operator_error(
            PersonalWorkerOperatorMutationErrorKind::Busy,
            OperatorErrorCode::DurableStateBusy,
        ),
        PersonalWorkerStoreMutationErrorKind::Io => PersonalWorkerOperatorMutationError::new(
            PersonalWorkerOperatorMutationErrorKind::Unavailable,
            None,
        ),
        PersonalWorkerStoreMutationErrorKind::UnsafeFilesystem => operator_error(
            PersonalWorkerOperatorMutationErrorKind::UnsafeFilesystem,
            OperatorErrorCode::DurableStateUnsafe,
        ),
        PersonalWorkerStoreMutationErrorKind::VersionIncompatible => operator_error(
            PersonalWorkerOperatorMutationErrorKind::VersionIncompatible,
            OperatorErrorCode::DurableStateVersionIncompatible,
        ),
        PersonalWorkerStoreMutationErrorKind::CorruptState => operator_error(
            PersonalWorkerOperatorMutationErrorKind::CorruptState,
            OperatorErrorCode::DurableStateCorrupt,
        ),
    }
}

const fn operator_error(
    kind: PersonalWorkerOperatorMutationErrorKind,
    code: OperatorErrorCode,
) -> PersonalWorkerOperatorMutationError {
    PersonalWorkerOperatorMutationError::new(kind, Some(OperatorPublicError::from_code(code)))
}
