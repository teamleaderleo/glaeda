#![allow(dead_code)]

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::disposable_attempt_catalog::DisposableAttemptCatalogRevision;
use crate::github_scale_set_delivery::{
    MAX_SCALE_SET_DELIVERY_BYTES, ScaleSetDelivery, decode_scale_set_delivery,
    encode_scale_set_delivery,
};
use crate::github_scale_set_protocol::ScaleSetRunnerRequestId;

pub(crate) const SCALE_SET_DELIVERY_RECOVERY_SCHEMA_VERSION: u8 = 1;
pub(crate) const MAX_SCALE_SET_DELIVERY_RECOVERY_BYTES: usize =
    MAX_SCALE_SET_DELIVERY_BYTES + 16 * 1024;
const MAX_DELIVERY_RECOVERY_REVISION: u64 = 1_000_000_000_000;

/// Pure recovery state for one durably reconciled Runner Scale Set delivery.
///
/// This document deliberately distinguishes a positively observed combined `ack` response from a
/// standalone acquisition replay after bridge death. An empty replay acquisition therefore stays
/// recovery evidence and never becomes proof that the earlier external operation succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScaleSetDeliveryRecoveryState {
    schema_version: u8,
    revision: u64,
    catalog_revision: DisposableAttemptCatalogRevision,
    delivery: ScaleSetDelivery,
    phase: ScaleSetDeliveryRecoveryPhase,
}

impl ScaleSetDeliveryRecoveryState {
    pub(crate) fn reconciled(
        delivery: ScaleSetDelivery,
        catalog_revision: DisposableAttemptCatalogRevision,
    ) -> Result<Self, ScaleSetDeliveryRecoveryError> {
        let state = Self {
            schema_version: SCALE_SET_DELIVERY_RECOVERY_SCHEMA_VERSION,
            revision: 1,
            catalog_revision,
            delivery,
            phase: ScaleSetDeliveryRecoveryPhase::Reconciled,
        };
        state.validate()?;
        Ok(state)
    }

    #[must_use]
    pub(crate) const fn revision(&self) -> u64 {
        self.revision
    }

    #[must_use]
    pub(crate) const fn catalog_revision(&self) -> DisposableAttemptCatalogRevision {
        self.catalog_revision
    }

    #[must_use]
    pub(crate) const fn delivery(&self) -> &ScaleSetDelivery {
        &self.delivery
    }

    #[must_use]
    pub(crate) const fn phase(&self) -> &ScaleSetDeliveryRecoveryPhase {
        &self.phase
    }

    pub(crate) fn begin_ack(&self) -> Result<Self, ScaleSetDeliveryRecoveryError> {
        if self.phase != ScaleSetDeliveryRecoveryPhase::Reconciled {
            return Err(recovery_error(
                ScaleSetDeliveryRecoveryErrorKind::Conflict,
                "only a reconciled delivery may begin acknowledgement",
            ));
        }
        self.successor(ScaleSetDeliveryRecoveryPhase::AcknowledgementStarted)
    }

    pub(crate) fn record_ack_response(
        &self,
        acquired: &[ScaleSetRunnerRequestId],
    ) -> Result<Self, ScaleSetDeliveryRecoveryError> {
        if self.phase != ScaleSetDeliveryRecoveryPhase::AcknowledgementStarted {
            return Err(recovery_error(
                ScaleSetDeliveryRecoveryErrorKind::Conflict,
                "acknowledgement response requires a started acknowledgement",
            ));
        }
        let acquired = self.validated_acquired(acquired)?;
        self.successor(ScaleSetDeliveryRecoveryPhase::Acknowledged { acquired })
    }

    pub(crate) fn record_recovery_acquire(
        &self,
        acquired: &[ScaleSetRunnerRequestId],
    ) -> Result<Self, ScaleSetDeliveryRecoveryError> {
        let mut observed = match &self.phase {
            ScaleSetDeliveryRecoveryPhase::AcknowledgementStarted => Vec::new(),
            ScaleSetDeliveryRecoveryPhase::AcquisitionRecoveryObserved { acquired } => {
                acquired.clone()
            }
            _ => {
                return Err(recovery_error(
                    ScaleSetDeliveryRecoveryErrorKind::Conflict,
                    "standalone acquisition replay requires ambiguous acknowledgement state",
                ));
            }
        };
        let newly_acquired = self.validated_acquired(acquired)?;
        let mut seen = observed.iter().copied().collect::<BTreeSet<_>>();
        for request_id in newly_acquired {
            if seen.insert(request_id) {
                observed.push(request_id);
            }
        }
        observed.sort_unstable();
        self.successor(ScaleSetDeliveryRecoveryPhase::AcquisitionRecoveryObserved {
            acquired: observed,
        })
    }

    fn successor(
        &self,
        phase: ScaleSetDeliveryRecoveryPhase,
    ) -> Result<Self, ScaleSetDeliveryRecoveryError> {
        let revision = self.revision.checked_add(1).ok_or_else(|| {
            recovery_error(
                ScaleSetDeliveryRecoveryErrorKind::Conflict,
                "delivery recovery revision cannot advance",
            )
        })?;
        let state = Self {
            schema_version: self.schema_version,
            revision,
            catalog_revision: self.catalog_revision,
            delivery: self.delivery.clone(),
            phase,
        };
        state.validate()?;
        Ok(state)
    }

    fn validated_acquired(
        &self,
        acquired: &[ScaleSetRunnerRequestId],
    ) -> Result<Vec<ScaleSetRunnerRequestId>, ScaleSetDeliveryRecoveryError> {
        let available = self
            .delivery
            .available_request_ids()
            .map_err(|_| corrupt_state())?
            .into_iter()
            .collect::<BTreeSet<_>>();
        let mut seen = BTreeSet::new();
        for request_id in acquired {
            if !available.contains(request_id) || !seen.insert(*request_id) {
                return Err(recovery_error(
                    ScaleSetDeliveryRecoveryErrorKind::Conflict,
                    "acquired request evidence is duplicate or foreign to the durable delivery",
                ));
            }
        }
        let mut result = seen.into_iter().collect::<Vec<_>>();
        result.sort_unstable();
        Ok(result)
    }

    fn validate(&self) -> Result<(), ScaleSetDeliveryRecoveryError> {
        if self.schema_version != SCALE_SET_DELIVERY_RECOVERY_SCHEMA_VERSION
            || !(1..=MAX_DELIVERY_RECOVERY_REVISION).contains(&self.revision)
        {
            return Err(corrupt_state());
        }
        encode_scale_set_delivery(&self.delivery).map_err(|_| corrupt_state())?;
        match &self.phase {
            ScaleSetDeliveryRecoveryPhase::Reconciled
            | ScaleSetDeliveryRecoveryPhase::AcknowledgementStarted => Ok(()),
            ScaleSetDeliveryRecoveryPhase::Acknowledged { acquired }
            | ScaleSetDeliveryRecoveryPhase::AcquisitionRecoveryObserved { acquired } => {
                self.validated_acquired(acquired).map(|_| ())
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScaleSetDeliveryRecoveryPhase {
    Reconciled,
    AcknowledgementStarted,
    Acknowledged {
        acquired: Vec<ScaleSetRunnerRequestId>,
    },
    AcquisitionRecoveryObserved {
        acquired: Vec<ScaleSetRunnerRequestId>,
    },
}

/// Encode one validated recovery state into bounded canonical JSON bytes.
pub(crate) fn encode_scale_set_delivery_recovery(
    state: &ScaleSetDeliveryRecoveryState,
) -> Result<Vec<u8>, ScaleSetDeliveryRecoveryError> {
    state.validate()?;
    let wire = RecoveryWire::from_state(state);
    let bytes = serde_json::to_vec(&wire).map_err(|_| {
        recovery_error(
            ScaleSetDeliveryRecoveryErrorKind::InvalidDocument,
            "Scale Set delivery recovery state cannot encode",
        )
    })?;
    if bytes.len() > MAX_SCALE_SET_DELIVERY_RECOVERY_BYTES {
        return Err(recovery_error(
            ScaleSetDeliveryRecoveryErrorKind::DocumentTooLarge,
            "Scale Set delivery recovery state exceeds the reviewed byte limit",
        ));
    }
    Ok(bytes)
}

/// Decode, fully revalidate, and require canonical recovery-state bytes.
pub(crate) fn decode_scale_set_delivery_recovery(
    bytes: &[u8],
) -> Result<ScaleSetDeliveryRecoveryState, ScaleSetDeliveryRecoveryError> {
    if bytes.len() > MAX_SCALE_SET_DELIVERY_RECOVERY_BYTES {
        return Err(recovery_error(
            ScaleSetDeliveryRecoveryErrorKind::DocumentTooLarge,
            "Scale Set delivery recovery state exceeds the reviewed byte limit",
        ));
    }
    let version: RecoveryVersion = serde_json::from_slice(bytes).map_err(|_| invalid_document())?;
    if version.schema_version != SCALE_SET_DELIVERY_RECOVERY_SCHEMA_VERSION {
        return Err(recovery_error(
            ScaleSetDeliveryRecoveryErrorKind::VersionIncompatible,
            "Scale Set delivery recovery schema version is unsupported",
        ));
    }
    let wire: RecoveryWire = serde_json::from_slice(bytes).map_err(|_| invalid_document())?;
    let state = wire.into_state()?;
    state.validate()?;
    if encode_scale_set_delivery_recovery(&state)? != bytes {
        return Err(recovery_error(
            ScaleSetDeliveryRecoveryErrorKind::NonCanonical,
            "Scale Set delivery recovery state is not canonical JSON",
        ));
    }
    Ok(state)
}

#[derive(Deserialize)]
struct RecoveryVersion {
    schema_version: u8,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecoveryWire {
    schema_version: u8,
    revision: u64,
    catalog_revision: u64,
    delivery: serde_json::Value,
    phase: RecoveryPhaseWire,
}

impl RecoveryWire {
    fn from_state(state: &ScaleSetDeliveryRecoveryState) -> Self {
        let delivery_bytes =
            encode_scale_set_delivery(&state.delivery).expect("validated delivery must encode");
        let delivery = serde_json::from_slice(&delivery_bytes)
            .expect("canonical delivery bytes must be valid JSON");
        Self {
            schema_version: state.schema_version,
            revision: state.revision,
            catalog_revision: state.catalog_revision.get(),
            delivery,
            phase: RecoveryPhaseWire::from_phase(&state.phase),
        }
    }

    fn into_state(self) -> Result<ScaleSetDeliveryRecoveryState, ScaleSetDeliveryRecoveryError> {
        let delivery_bytes = serde_json::to_vec(&self.delivery).map_err(|_| invalid_document())?;
        let delivery = decode_scale_set_delivery(&delivery_bytes).map_err(|_| corrupt_state())?;
        let catalog_revision = DisposableAttemptCatalogRevision::new(self.catalog_revision)
            .map_err(|_| corrupt_state())?;
        let state = ScaleSetDeliveryRecoveryState {
            schema_version: self.schema_version,
            revision: self.revision,
            catalog_revision,
            delivery,
            phase: self.phase.into_phase()?,
        };
        state.validate()?;
        Ok(state)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
enum RecoveryPhaseWire {
    Reconciled,
    AcknowledgementStarted,
    Acknowledged { acquired_request_ids: Vec<u64> },
    AcquisitionRecoveryObserved { acquired_request_ids: Vec<u64> },
}

impl RecoveryPhaseWire {
    fn from_phase(phase: &ScaleSetDeliveryRecoveryPhase) -> Self {
        match phase {
            ScaleSetDeliveryRecoveryPhase::Reconciled => Self::Reconciled,
            ScaleSetDeliveryRecoveryPhase::AcknowledgementStarted => Self::AcknowledgementStarted,
            ScaleSetDeliveryRecoveryPhase::Acknowledged { acquired } => Self::Acknowledged {
                acquired_request_ids: acquired.iter().map(|id| id.get()).collect(),
            },
            ScaleSetDeliveryRecoveryPhase::AcquisitionRecoveryObserved { acquired } => {
                Self::AcquisitionRecoveryObserved {
                    acquired_request_ids: acquired.iter().map(|id| id.get()).collect(),
                }
            }
        }
    }

    fn into_phase(self) -> Result<ScaleSetDeliveryRecoveryPhase, ScaleSetDeliveryRecoveryError> {
        match self {
            Self::Reconciled => Ok(ScaleSetDeliveryRecoveryPhase::Reconciled),
            Self::AcknowledgementStarted => {
                Ok(ScaleSetDeliveryRecoveryPhase::AcknowledgementStarted)
            }
            Self::Acknowledged {
                acquired_request_ids,
            } => Ok(ScaleSetDeliveryRecoveryPhase::Acknowledged {
                acquired: parse_request_ids(acquired_request_ids)?,
            }),
            Self::AcquisitionRecoveryObserved {
                acquired_request_ids,
            } => Ok(ScaleSetDeliveryRecoveryPhase::AcquisitionRecoveryObserved {
                acquired: parse_request_ids(acquired_request_ids)?,
            }),
        }
    }
}

fn parse_request_ids(
    values: Vec<u64>,
) -> Result<Vec<ScaleSetRunnerRequestId>, ScaleSetDeliveryRecoveryError> {
    values
        .into_iter()
        .map(|value| ScaleSetRunnerRequestId::new(value).map_err(|_| corrupt_state()))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ScaleSetDeliveryRecoveryErrorKind {
    InvalidDocument,
    VersionIncompatible,
    DocumentTooLarge,
    NonCanonical,
    CorruptState,
    Conflict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct ScaleSetDeliveryRecoveryError {
    kind: ScaleSetDeliveryRecoveryErrorKind,
    message: &'static str,
}

impl ScaleSetDeliveryRecoveryError {
    #[must_use]
    pub(crate) const fn kind(self) -> ScaleSetDeliveryRecoveryErrorKind {
        self.kind
    }
}

impl fmt::Display for ScaleSetDeliveryRecoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ScaleSetDeliveryRecoveryError {}

const fn recovery_error(
    kind: ScaleSetDeliveryRecoveryErrorKind,
    message: &'static str,
) -> ScaleSetDeliveryRecoveryError {
    ScaleSetDeliveryRecoveryError { kind, message }
}

const fn corrupt_state() -> ScaleSetDeliveryRecoveryError {
    recovery_error(
        ScaleSetDeliveryRecoveryErrorKind::CorruptState,
        "Scale Set delivery recovery state is corrupt",
    )
}

const fn invalid_document() -> ScaleSetDeliveryRecoveryError {
    recovery_error(
        ScaleSetDeliveryRecoveryErrorKind::InvalidDocument,
        "Scale Set delivery recovery JSON is invalid",
    )
}

#[cfg(test)]
mod tests {
    use crate::github_scale_set_bridge::{
        ScaleSetBridgeEvent, ScaleSetBridgeJobEvidence, ScaleSetBridgePoll, ScaleSetStatistics,
    };
    use crate::github_scale_set_protocol::ScaleSetJobId;

    use super::*;

    fn delivery() -> ScaleSetDelivery {
        ScaleSetDelivery::from_bridge_poll(&ScaleSetBridgePoll::Message {
            message_id: 7,
            statistics: ScaleSetStatistics {
                available_jobs: 2,
                acquired_jobs: 0,
                assigned_jobs: 0,
                running_jobs: 0,
                registered_runners: 0,
                busy_runners: 0,
                idle_runners: 0,
            },
            events: vec![
                ScaleSetBridgeEvent::Available(job(41, "job-1")),
                ScaleSetBridgeEvent::Available(job(42, "job-2")),
            ],
        })
        .unwrap()
        .expect("message must produce delivery")
    }

    fn job(request_id: u64, job_id: &str) -> ScaleSetBridgeJobEvidence {
        ScaleSetBridgeJobEvidence {
            runner_request_id: request_id,
            repository: "project".to_owned(),
            owner: "example".to_owned(),
            job_id: ScaleSetJobId::parse(job_id).unwrap(),
            workflow_run_id: 99,
            request_labels: vec!["smolrunner".to_owned()],
        }
    }

    #[test]
    fn ack_response_is_distinct_from_recovery_acquisition() {
        let initial = ScaleSetDeliveryRecoveryState::reconciled(
            delivery(),
            DisposableAttemptCatalogRevision::new(8).unwrap(),
        )
        .unwrap();
        let started = initial.begin_ack().unwrap();
        let acquired = [ScaleSetRunnerRequestId::new(41).unwrap()];
        let acknowledged = started.record_ack_response(&acquired).unwrap();
        assert!(matches!(
            acknowledged.phase(),
            ScaleSetDeliveryRecoveryPhase::Acknowledged { acquired } if acquired == &acquired.to_vec()
        ));

        let recovery = started.record_recovery_acquire(&[]).unwrap();
        assert!(matches!(
            recovery.phase(),
            ScaleSetDeliveryRecoveryPhase::AcquisitionRecoveryObserved { acquired } if acquired.is_empty()
        ));
    }

    #[test]
    fn repeated_recovery_acquisition_unions_positive_evidence() {
        let started = ScaleSetDeliveryRecoveryState::reconciled(
            delivery(),
            DisposableAttemptCatalogRevision::new(8).unwrap(),
        )
        .unwrap()
        .begin_ack()
        .unwrap();
        let first = started
            .record_recovery_acquire(&[ScaleSetRunnerRequestId::new(42).unwrap()])
            .unwrap();
        let second = first
            .record_recovery_acquire(&[ScaleSetRunnerRequestId::new(41).unwrap()])
            .unwrap();
        assert!(matches!(
            second.phase(),
            ScaleSetDeliveryRecoveryPhase::AcquisitionRecoveryObserved { acquired }
                if acquired.iter().map(|id| id.get()).collect::<Vec<_>>() == vec![41, 42]
        ));
    }

    #[test]
    fn foreign_or_duplicate_acquisition_evidence_conflicts() {
        let started = ScaleSetDeliveryRecoveryState::reconciled(
            delivery(),
            DisposableAttemptCatalogRevision::new(8).unwrap(),
        )
        .unwrap()
        .begin_ack()
        .unwrap();
        assert_eq!(
            started
                .record_ack_response(&[ScaleSetRunnerRequestId::new(43).unwrap()])
                .unwrap_err()
                .kind(),
            ScaleSetDeliveryRecoveryErrorKind::Conflict
        );
        let duplicate = ScaleSetRunnerRequestId::new(41).unwrap();
        assert_eq!(
            started
                .record_ack_response(&[duplicate, duplicate])
                .unwrap_err()
                .kind(),
            ScaleSetDeliveryRecoveryErrorKind::Conflict
        );
    }

    #[test]
    fn recovery_codec_is_canonical_and_versioned() {
        let state = ScaleSetDeliveryRecoveryState::reconciled(
            delivery(),
            DisposableAttemptCatalogRevision::new(8).unwrap(),
        )
        .unwrap()
        .begin_ack()
        .unwrap();
        let encoded = encode_scale_set_delivery_recovery(&state).unwrap();
        assert_eq!(decode_scale_set_delivery_recovery(&encoded).unwrap(), state);

        let future = String::from_utf8(encoded.clone())
            .unwrap()
            .replacen("\"schema_version\":1", "\"schema_version\":2", 1)
            .into_bytes();
        assert_eq!(
            decode_scale_set_delivery_recovery(&future)
                .unwrap_err()
                .kind(),
            ScaleSetDeliveryRecoveryErrorKind::VersionIncompatible
        );

        let mut noncanonical = b" ".to_vec();
        noncanonical.extend_from_slice(&encoded);
        assert_eq!(
            decode_scale_set_delivery_recovery(&noncanonical)
                .unwrap_err()
                .kind(),
            ScaleSetDeliveryRecoveryErrorKind::NonCanonical
        );
    }
}
