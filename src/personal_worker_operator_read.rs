use std::fmt;

use serde::Serialize;

use crate::operator_config::{OperatorConfig, OperatorConfigIdentity};
use crate::operator_error::{OperatorErrorCode, OperatorPublicError};
use crate::personal_worker_operator_store::{
    PersonalWorkerOperatorStore, PersonalWorkerOperatorStoreError,
    PersonalWorkerOperatorStoreErrorKind,
};
use crate::personal_worker_queue::PersonalWorkerQueueGeneration;
use crate::personal_worker_read_model::{
    PersonalWorkerJobReadRequest, PersonalWorkerJobView, PersonalWorkerQueuePage,
    PersonalWorkerQueuePageRequest, PersonalWorkerReadError, PersonalWorkerReadErrorKind,
    PersonalWorkerStatusView, personal_worker_job_view, personal_worker_queue_page,
    personal_worker_status,
};
use crate::personal_worker_store::{PersonalWorkerStoreDocument, PersonalWorkerStoreRevision};

pub const PERSONAL_WORKER_OPERATOR_READ_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersonalWorkerSnapshotExpectation {
    store_revision: PersonalWorkerStoreRevision,
    queue_generation: PersonalWorkerQueueGeneration,
}

impl PersonalWorkerSnapshotExpectation {
    #[must_use]
    pub const fn new(
        store_revision: PersonalWorkerStoreRevision,
        queue_generation: PersonalWorkerQueueGeneration,
    ) -> Self {
        Self {
            store_revision,
            queue_generation,
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
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerOperatorRead<T> {
    schema_version: u8,
    config_identity: OperatorConfigIdentity,
    view: T,
}

impl<T> PersonalWorkerOperatorRead<T> {
    fn new(config_identity: OperatorConfigIdentity, view: T) -> Self {
        Self {
            schema_version: PERSONAL_WORKER_OPERATOR_READ_SCHEMA_VERSION,
            config_identity,
            view,
        }
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn config_identity(&self) -> &OperatorConfigIdentity {
        &self.config_identity
    }

    #[must_use]
    pub const fn view(&self) -> &T {
        &self.view
    }

    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn for_verification_plan_test(
        config_identity: OperatorConfigIdentity,
        view: T,
    ) -> Self {
        Self::new(config_identity, view)
    }
}

impl<T> fmt::Debug for PersonalWorkerOperatorRead<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersonalWorkerOperatorRead")
            .field("schema_version", &self.schema_version)
            .field("config_identity", &self.config_identity)
            .field("view", &"redacted")
            .finish()
    }
}

pub type PersonalWorkerOperatorStatusRead = PersonalWorkerOperatorRead<PersonalWorkerStatusView>;
pub type PersonalWorkerOperatorQueueRead = PersonalWorkerOperatorRead<PersonalWorkerQueuePage>;
pub type PersonalWorkerOperatorJobRead = PersonalWorkerOperatorRead<PersonalWorkerJobView>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerOperatorReadErrorKind {
    Missing,
    UnsafeFilesystem,
    CorruptState,
    VersionIncompatible,
    RecoveryRequired,
    Busy,
    Unavailable,
    UnsupportedPlatform,
    StaleRevision,
    StaleQueueGeneration,
    InvalidPage,
    OffsetOutOfBounds,
    NotFound,
    InvalidDocument,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerOperatorReadError {
    kind: PersonalWorkerOperatorReadErrorKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    public_error: Option<OperatorPublicError>,
}

impl PersonalWorkerOperatorReadError {
    #[must_use]
    pub const fn kind(&self) -> PersonalWorkerOperatorReadErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn public_error(&self) -> Option<OperatorPublicError> {
        self.public_error
    }

    const fn new(
        kind: PersonalWorkerOperatorReadErrorKind,
        public_error: Option<OperatorPublicError>,
    ) -> Self {
        Self { kind, public_error }
    }
}

impl fmt::Display for PersonalWorkerOperatorReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(error) = self.public_error {
            return error.fmt(formatter);
        }
        formatter.write_str(match self.kind {
            PersonalWorkerOperatorReadErrorKind::InvalidPage => {
                "personal worker queue page is invalid"
            }
            PersonalWorkerOperatorReadErrorKind::OffsetOutOfBounds => {
                "personal worker queue page offset exceeds the exact snapshot"
            }
            PersonalWorkerOperatorReadErrorKind::NotFound => {
                "personal worker job is not provable in the exact durable snapshot"
            }
            _ => "personal worker state could not be read",
        })
    }
}

impl std::error::Error for PersonalWorkerOperatorReadError {}

pub struct PersonalWorkerOperatorReadService;

impl PersonalWorkerOperatorReadService {
    pub fn read_status(
        config: &OperatorConfig,
        expectation: Option<PersonalWorkerSnapshotExpectation>,
    ) -> Result<PersonalWorkerOperatorStatusRead, PersonalWorkerOperatorReadError> {
        let opened = PersonalWorkerOperatorStore::open_current(config).map_err(map_store_error)?;
        if let Some(expectation) = expectation {
            validate_snapshot(opened.document(), expectation)?;
        }
        let view = personal_worker_status(opened.document()).map_err(map_read_error)?;
        Ok(PersonalWorkerOperatorRead::new(
            opened.config_identity().clone(),
            view,
        ))
    }

    pub fn read_queue_page(
        config: &OperatorConfig,
        request: PersonalWorkerQueuePageRequest,
    ) -> Result<PersonalWorkerOperatorQueueRead, PersonalWorkerOperatorReadError> {
        let opened = PersonalWorkerOperatorStore::open_current(config).map_err(map_store_error)?;
        let view =
            personal_worker_queue_page(opened.document(), request).map_err(map_read_error)?;
        Ok(PersonalWorkerOperatorRead::new(
            opened.config_identity().clone(),
            view,
        ))
    }

    pub fn read_job(
        config: &OperatorConfig,
        request: PersonalWorkerJobReadRequest,
    ) -> Result<PersonalWorkerOperatorJobRead, PersonalWorkerOperatorReadError> {
        let opened = PersonalWorkerOperatorStore::open_current(config).map_err(map_store_error)?;
        let view = personal_worker_job_view(opened.document(), request).map_err(map_read_error)?;
        Ok(PersonalWorkerOperatorRead::new(
            opened.config_identity().clone(),
            view,
        ))
    }
}

fn validate_snapshot(
    document: &PersonalWorkerStoreDocument,
    expectation: PersonalWorkerSnapshotExpectation,
) -> Result<(), PersonalWorkerOperatorReadError> {
    if document.revision() != expectation.store_revision() {
        return Err(operator_error(
            PersonalWorkerOperatorReadErrorKind::StaleRevision,
            OperatorErrorCode::DurableStateRevisionStale,
        ));
    }
    if document.queue().generation != expectation.queue_generation() {
        return Err(operator_error(
            PersonalWorkerOperatorReadErrorKind::StaleQueueGeneration,
            OperatorErrorCode::DurableStateGenerationStale,
        ));
    }
    Ok(())
}

fn map_store_error(error: PersonalWorkerOperatorStoreError) -> PersonalWorkerOperatorReadError {
    let kind = match error.kind() {
        PersonalWorkerOperatorStoreErrorKind::Missing => {
            PersonalWorkerOperatorReadErrorKind::Missing
        }
        PersonalWorkerOperatorStoreErrorKind::UnsafeFilesystem => {
            PersonalWorkerOperatorReadErrorKind::UnsafeFilesystem
        }
        PersonalWorkerOperatorStoreErrorKind::CorruptState
        | PersonalWorkerOperatorStoreErrorKind::InvalidInitialState => {
            PersonalWorkerOperatorReadErrorKind::CorruptState
        }
        PersonalWorkerOperatorStoreErrorKind::VersionIncompatible => {
            PersonalWorkerOperatorReadErrorKind::VersionIncompatible
        }
        PersonalWorkerOperatorStoreErrorKind::RecoveryRequired => {
            PersonalWorkerOperatorReadErrorKind::RecoveryRequired
        }
        PersonalWorkerOperatorStoreErrorKind::Busy => PersonalWorkerOperatorReadErrorKind::Busy,
        PersonalWorkerOperatorStoreErrorKind::Unavailable => {
            PersonalWorkerOperatorReadErrorKind::Unavailable
        }
        PersonalWorkerOperatorStoreErrorKind::UnsupportedPlatform => {
            PersonalWorkerOperatorReadErrorKind::UnsupportedPlatform
        }
    };
    PersonalWorkerOperatorReadError::new(kind, Some(error.public_error()))
}

fn map_read_error(error: PersonalWorkerReadError) -> PersonalWorkerOperatorReadError {
    match error.kind() {
        PersonalWorkerReadErrorKind::StaleRevision => operator_error(
            PersonalWorkerOperatorReadErrorKind::StaleRevision,
            OperatorErrorCode::DurableStateRevisionStale,
        ),
        PersonalWorkerReadErrorKind::StaleQueueGeneration => operator_error(
            PersonalWorkerOperatorReadErrorKind::StaleQueueGeneration,
            OperatorErrorCode::DurableStateGenerationStale,
        ),
        PersonalWorkerReadErrorKind::InvalidPage => PersonalWorkerOperatorReadError::new(
            PersonalWorkerOperatorReadErrorKind::InvalidPage,
            None,
        ),
        PersonalWorkerReadErrorKind::OffsetOutOfBounds => PersonalWorkerOperatorReadError::new(
            PersonalWorkerOperatorReadErrorKind::OffsetOutOfBounds,
            None,
        ),
        PersonalWorkerReadErrorKind::NotFound => PersonalWorkerOperatorReadError::new(
            PersonalWorkerOperatorReadErrorKind::NotFound,
            None,
        ),
        PersonalWorkerReadErrorKind::InvalidDocument => operator_error(
            PersonalWorkerOperatorReadErrorKind::InvalidDocument,
            OperatorErrorCode::DurableStateCorrupt,
        ),
    }
}

const fn operator_error(
    kind: PersonalWorkerOperatorReadErrorKind,
    code: OperatorErrorCode,
) -> PersonalWorkerOperatorReadError {
    PersonalWorkerOperatorReadError::new(kind, Some(OperatorPublicError::from_code(code)))
}
