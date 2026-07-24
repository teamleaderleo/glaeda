use std::fmt;

use serde::{Deserialize, Serialize};

use crate::lease::{
    LEASE_SCHEMA_VERSION, LeaseId, LeaseIdentity, LeaseKind, LeaseRecord, LeaseState,
};
use crate::state::InstallationId;

pub const MAX_LEASE_DOCUMENT_BYTES: usize = 65_536;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LeaseRecordWire {
    schema_version: u8,
    identity: LeaseIdentityWire,
    state: String,
    revision: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LeaseIdentityWire {
    lease_id: String,
    installation_id: String,
    kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LeaseDocumentError {
    pub problems: Vec<String>,
}

impl LeaseDocumentError {
    fn single(problem: impl Into<String>) -> Self {
        Self {
            problems: vec![problem.into()],
        }
    }
}

impl fmt::Display for LeaseDocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "lease document validation failed")?;
        for problem in &self.problems {
            writeln!(formatter, "- {problem}")?;
        }
        Ok(())
    }
}

impl std::error::Error for LeaseDocumentError {}

/// Encode one validated lease record as stable, newline-terminated JSON.
///
/// # Errors
///
/// Returns an error when the in-memory record is semantically invalid, serialization fails, or the
/// encoded document exceeds the lease-document size limit.
pub fn encode_lease_document(record: &LeaseRecord) -> Result<String, LeaseDocumentError> {
    validate_record(record)?;
    let mut encoded = serde_json::to_string_pretty(record).map_err(|_| {
        LeaseDocumentError::single("could not serialize the validated lease document")
    })?;
    encoded.push('\n');
    if encoded.len() > MAX_LEASE_DOCUMENT_BYTES {
        return Err(LeaseDocumentError::single(format!(
            "encoded lease document exceeds {MAX_LEASE_DOCUMENT_BYTES} bytes"
        )));
    }
    Ok(encoded)
}

/// Decode and validate one persisted lease record.
///
/// Unknown fields and unknown schema versions fail closed. Validated identifier constructors are
/// applied after deserialization so persisted strings cannot bypass their invariants.
///
/// # Errors
///
/// Returns an error for oversized, malformed, unsupported, or semantically invalid documents.
pub fn decode_lease_document(bytes: &[u8]) -> Result<LeaseRecord, LeaseDocumentError> {
    if bytes.len() > MAX_LEASE_DOCUMENT_BYTES {
        return Err(LeaseDocumentError::single(format!(
            "lease document exceeds {MAX_LEASE_DOCUMENT_BYTES} bytes"
        )));
    }
    let wire: LeaseRecordWire = serde_json::from_slice(bytes)
        .map_err(|error| LeaseDocumentError::single(format!("invalid lease JSON: {error}")))?;
    if wire.schema_version != LEASE_SCHEMA_VERSION {
        return Err(LeaseDocumentError::single(format!(
            "unsupported lease schema version {}; only version {LEASE_SCHEMA_VERSION} is accepted",
            wire.schema_version
        )));
    }

    let lease_id = LeaseId::parse(&wire.identity.lease_id)
        .map_err(|error| LeaseDocumentError::single(error.to_string()))?;
    let installation_id = InstallationId::parse(&wire.identity.installation_id)
        .map_err(|error| LeaseDocumentError::single(error.to_string()))?;
    let kind = parse_kind(&wire.identity.kind)?;
    let state = parse_state(&wire.state)?;
    let record = LeaseRecord {
        schema_version: LEASE_SCHEMA_VERSION,
        identity: LeaseIdentity::new(lease_id, installation_id, kind),
        state,
        revision: wire.revision,
    };
    validate_record(&record)?;
    Ok(record)
}

fn validate_record(record: &LeaseRecord) -> Result<(), LeaseDocumentError> {
    if record.schema_version != LEASE_SCHEMA_VERSION {
        return Err(LeaseDocumentError::single(format!(
            "unsupported lease schema version {}; only version {LEASE_SCHEMA_VERSION} is accepted",
            record.schema_version
        )));
    }
    if record.identity.kind == LeaseKind::Run && record.state == LeaseState::Sleeping {
        return Err(LeaseDocumentError::single(
            "run leases cannot persist in the sleeping state",
        ));
    }
    let minimum_revision = match record.state {
        LeaseState::Pending => 0,
        LeaseState::Active | LeaseState::Releasing | LeaseState::Expired | LeaseState::Failed => 1,
        LeaseState::Sleeping | LeaseState::Released => 2,
    };
    if record.revision < minimum_revision {
        return Err(LeaseDocumentError::single(format!(
            "lease state {} requires revision {minimum_revision} or greater",
            record.state
        )));
    }
    Ok(())
}

fn parse_kind(value: &str) -> Result<LeaseKind, LeaseDocumentError> {
    match value {
        "run" => Ok(LeaseKind::Run),
        "workspace" => Ok(LeaseKind::Workspace),
        "preview" => Ok(LeaseKind::Preview),
        _ => Err(LeaseDocumentError::single(format!(
            "unknown lease kind {value:?}"
        ))),
    }
}

fn parse_state(value: &str) -> Result<LeaseState, LeaseDocumentError> {
    match value {
        "pending" => Ok(LeaseState::Pending),
        "active" => Ok(LeaseState::Active),
        "sleeping" => Ok(LeaseState::Sleeping),
        "releasing" => Ok(LeaseState::Releasing),
        "released" => Ok(LeaseState::Released),
        "expired" => Ok(LeaseState::Expired),
        "failed" => Ok(LeaseState::Failed),
        _ => Err(LeaseDocumentError::single(format!(
            "unknown lease state {value:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use crate::lease::{LeaseId, LeaseIdentity, LeaseKind, LeaseRecord, LeaseState};
    use crate::state::InstallationId;

    use super::{decode_lease_document, encode_lease_document};

    fn pending_preview() -> LeaseRecord {
        LeaseRecord::pending(LeaseIdentity::new(
            LeaseId::parse("preview-pr-42").expect("lease ID"),
            InstallationId::parse("installation-001").expect("installation ID"),
            LeaseKind::Preview,
        ))
    }

    #[test]
    fn document_round_trip_preserves_validated_lease_state() {
        let record = pending_preview();
        let encoded = encode_lease_document(&record).expect("encode lease");
        let decoded = decode_lease_document(encoded.as_bytes()).expect("decode lease");

        assert_eq!(decoded, record);
        assert!(encoded.ends_with('\n'));
    }

    #[test]
    fn unknown_fields_fail_closed() {
        let encoded = encode_lease_document(&pending_preview()).expect("encode lease");
        let changed = encoded.replacen("\"revision\": 0", "\"revision\": 0,\n  \"extra\": true", 1);

        assert!(decode_lease_document(changed.as_bytes()).is_err());
    }

    #[test]
    fn persisted_identifiers_are_revalidated() {
        let encoded = encode_lease_document(&pending_preview()).expect("encode lease");
        let changed = encoded.replace("preview-pr-42", "../preview");

        assert!(decode_lease_document(changed.as_bytes()).is_err());
    }

    #[test]
    fn unknown_versions_kinds_and_states_fail_closed() {
        let encoded = encode_lease_document(&pending_preview()).expect("encode lease");
        assert!(
            decode_lease_document(
                encoded
                    .replace("\"schema_version\": 1", "\"schema_version\": 2")
                    .as_bytes()
            )
            .is_err()
        );
        assert!(
            decode_lease_document(encoded.replace("\"preview\"", "\"unknown\"").as_bytes())
                .is_err()
        );
        assert!(
            decode_lease_document(encoded.replace("\"pending\"", "\"unknown\"").as_bytes())
                .is_err()
        );
    }

    #[test]
    fn impossible_kind_state_and_revision_combinations_fail_closed() {
        let encoded = encode_lease_document(&pending_preview()).expect("encode lease");
        let sleeping_run = encoded
            .replace("\"preview\"", "\"run\"")
            .replace("\"pending\"", "\"sleeping\"")
            .replace("\"revision\": 0", "\"revision\": 2");
        assert!(decode_lease_document(sleeping_run.as_bytes()).is_err());

        let active_at_zero = encoded.replace("\"pending\"", "\"active\"");
        assert!(decode_lease_document(active_at_zero.as_bytes()).is_err());
    }

    #[test]
    fn terminal_state_round_trip_is_supported() {
        let mut record = pending_preview();
        record.state = LeaseState::Expired;
        record.revision = 1;

        let encoded = encode_lease_document(&record).expect("encode lease");
        assert_eq!(
            decode_lease_document(encoded.as_bytes()).expect("decode lease"),
            record
        );
    }
}
