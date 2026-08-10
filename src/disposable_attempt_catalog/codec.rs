use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    DISPOSABLE_ATTEMPT_CATALOG_SCHEMA_VERSION, DisposableAttemptCatalogDocument,
    DisposableAttemptCatalogRevision, DisposableAttemptReservation, MAX_ACTIVE_DISPOSABLE_ATTEMPTS,
    MAX_DISPOSABLE_ATTEMPT_TOMBSTONES,
};
use crate::disposable_attempt_state::{
    DisposableAttemptState, decode_disposable_attempt_state, encode_disposable_attempt_state,
};
use crate::disposable_worker_reconciler::DisposableWorkerResources;

pub const MAX_DISPOSABLE_ATTEMPT_CATALOG_DOCUMENT_BYTES: usize = 1_048_576;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogWire {
    schema_version: u8,
    revision: u64,
    active: Vec<ReservationWire>,
    tombstones: Vec<Value>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReservationWire {
    attempt: Value,
    resources: ResourceWire,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceWire {
    cpu_millis: u32,
    memory_bytes: u64,
    disk_bytes: u64,
}

/// Encode one fully validated disposable-attempt catalog as canonical bounded JSON.
///
/// # Errors
///
/// Returns an error when catalog invariants fail, embedded attempt state cannot encode, or the
/// canonical document exceeds the reviewed byte limit.
pub fn encode_disposable_attempt_catalog(
    document: &DisposableAttemptCatalogDocument,
) -> Result<Vec<u8>, DisposableAttemptCatalogCodecError> {
    document.validate().map_err(|_| {
        codec_error(
            DisposableAttemptCatalogCodecErrorKind::CorruptState,
            "catalog invariants are invalid",
        )
    })?;

    let active = document
        .active
        .iter()
        .map(|reservation| {
            Ok(ReservationWire {
                attempt: encode_attempt_value(&reservation.attempt)?,
                resources: ResourceWire {
                    cpu_millis: reservation.resources.cpu_millis(),
                    memory_bytes: reservation.resources.memory_bytes(),
                    disk_bytes: reservation.resources.disk_bytes(),
                },
            })
        })
        .collect::<Result<Vec<_>, DisposableAttemptCatalogCodecError>>()?;
    let tombstones = document
        .tombstones
        .iter()
        .map(encode_attempt_value)
        .collect::<Result<Vec<_>, DisposableAttemptCatalogCodecError>>()?;

    let wire = CatalogWire {
        schema_version: document.schema_version,
        revision: document.revision.get(),
        active,
        tombstones,
    };
    let encoded = serde_json::to_vec(&wire).map_err(|_| {
        codec_error(
            DisposableAttemptCatalogCodecErrorKind::InvalidDocument,
            "disposable attempt catalog cannot encode",
        )
    })?;
    if encoded.len() > MAX_DISPOSABLE_ATTEMPT_CATALOG_DOCUMENT_BYTES {
        return Err(codec_error(
            DisposableAttemptCatalogCodecErrorKind::DocumentTooLarge,
            "disposable attempt catalog exceeds the reviewed byte limit",
        ));
    }
    Ok(encoded)
}

/// Decode, canonicalize, and fully revalidate one durable disposable-attempt catalog.
///
/// # Errors
///
/// Returns an error for malformed, oversized, future-version, non-canonical, or internally
/// inconsistent documents.
pub fn decode_disposable_attempt_catalog(
    bytes: &[u8],
) -> Result<DisposableAttemptCatalogDocument, DisposableAttemptCatalogCodecError> {
    if bytes.len() > MAX_DISPOSABLE_ATTEMPT_CATALOG_DOCUMENT_BYTES {
        return Err(codec_error(
            DisposableAttemptCatalogCodecErrorKind::DocumentTooLarge,
            "disposable attempt catalog exceeds the reviewed byte limit",
        ));
    }
    let wire: CatalogWire = serde_json::from_slice(bytes).map_err(|_| {
        codec_error(
            DisposableAttemptCatalogCodecErrorKind::InvalidDocument,
            "disposable attempt catalog JSON is invalid",
        )
    })?;
    if wire.schema_version != DISPOSABLE_ATTEMPT_CATALOG_SCHEMA_VERSION {
        return Err(codec_error(
            DisposableAttemptCatalogCodecErrorKind::VersionIncompatible,
            "disposable attempt catalog schema version is unsupported",
        ));
    }
    if wire.active.len() > MAX_ACTIVE_DISPOSABLE_ATTEMPTS
        || wire.tombstones.len() > MAX_DISPOSABLE_ATTEMPT_TOMBSTONES
    {
        return Err(codec_error(
            DisposableAttemptCatalogCodecErrorKind::CorruptState,
            "disposable attempt catalog exceeds a reviewed entry bound",
        ));
    }

    let revision = DisposableAttemptCatalogRevision::new(wire.revision).map_err(|_| {
        codec_error(
            DisposableAttemptCatalogCodecErrorKind::CorruptState,
            "disposable attempt catalog revision is invalid",
        )
    })?;
    let active = wire
        .active
        .into_iter()
        .map(|reservation| {
            let attempt = decode_attempt_value(reservation.attempt)?;
            let resources = DisposableWorkerResources::new(
                reservation.resources.cpu_millis,
                reservation.resources.memory_bytes,
                reservation.resources.disk_bytes,
            )
            .map_err(|_| {
                codec_error(
                    DisposableAttemptCatalogCodecErrorKind::CorruptState,
                    "disposable attempt reservation resources are invalid",
                )
            })?;
            Ok(DisposableAttemptReservation { attempt, resources })
        })
        .collect::<Result<Vec<_>, DisposableAttemptCatalogCodecError>>()?;
    let tombstones = wire
        .tombstones
        .into_iter()
        .map(decode_attempt_value)
        .collect::<Result<Vec<_>, DisposableAttemptCatalogCodecError>>()?;

    let document = DisposableAttemptCatalogDocument {
        schema_version: DISPOSABLE_ATTEMPT_CATALOG_SCHEMA_VERSION,
        revision,
        active,
        tombstones,
    };
    document.validate().map_err(|_| {
        codec_error(
            DisposableAttemptCatalogCodecErrorKind::CorruptState,
            "disposable attempt catalog invariants are invalid",
        )
    })?;

    let canonical = encode_disposable_attempt_catalog(&document)?;
    if canonical != bytes {
        return Err(codec_error(
            DisposableAttemptCatalogCodecErrorKind::NonCanonical,
            "disposable attempt catalog is not in canonical JSON form",
        ));
    }
    Ok(document)
}

fn encode_attempt_value(
    attempt: &DisposableAttemptState,
) -> Result<Value, DisposableAttemptCatalogCodecError> {
    let encoded = encode_disposable_attempt_state(attempt).map_err(|_| {
        codec_error(
            DisposableAttemptCatalogCodecErrorKind::CorruptState,
            "embedded disposable attempt cannot encode",
        )
    })?;
    serde_json::from_slice(&encoded).map_err(|_| {
        codec_error(
            DisposableAttemptCatalogCodecErrorKind::CorruptState,
            "embedded disposable attempt JSON is invalid",
        )
    })
}

fn decode_attempt_value(
    value: Value,
) -> Result<DisposableAttemptState, DisposableAttemptCatalogCodecError> {
    let encoded = serde_json::to_vec(&value).map_err(|_| {
        codec_error(
            DisposableAttemptCatalogCodecErrorKind::InvalidDocument,
            "embedded disposable attempt cannot decode",
        )
    })?;
    decode_disposable_attempt_state(&encoded).map_err(|error| {
        let kind = if error.code() == "version_incompatible" {
            DisposableAttemptCatalogCodecErrorKind::VersionIncompatible
        } else {
            DisposableAttemptCatalogCodecErrorKind::CorruptState
        };
        codec_error(kind, "embedded disposable attempt state is invalid")
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableAttemptCatalogCodecErrorKind {
    InvalidDocument,
    VersionIncompatible,
    DocumentTooLarge,
    NonCanonical,
    CorruptState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DisposableAttemptCatalogCodecError {
    kind: DisposableAttemptCatalogCodecErrorKind,
    message: &'static str,
}

impl DisposableAttemptCatalogCodecError {
    #[must_use]
    pub const fn kind(&self) -> DisposableAttemptCatalogCodecErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn message(&self) -> &'static str {
        self.message
    }
}

impl fmt::Display for DisposableAttemptCatalogCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for DisposableAttemptCatalogCodecError {}

const fn codec_error(
    kind: DisposableAttemptCatalogCodecErrorKind,
    message: &'static str,
) -> DisposableAttemptCatalogCodecError {
    DisposableAttemptCatalogCodecError { kind, message }
}
