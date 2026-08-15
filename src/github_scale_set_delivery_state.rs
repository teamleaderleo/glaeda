#![allow(dead_code)]

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::artifact::Sha256Digest;
use crate::disposable_attempt_catalog::{
    DisposableAttemptCatalogDocument, DisposableAttemptCatalogRevision,
    MAX_DISPOSABLE_ATTEMPT_CATALOG_DOCUMENT_BYTES, decode_disposable_attempt_catalog,
    encode_disposable_attempt_catalog,
};
use crate::github_scale_set_delivery::{
    MAX_SCALE_SET_DELIVERY_BYTES, ScaleSetDelivery, decode_scale_set_delivery,
    encode_scale_set_delivery,
};
use crate::github_scale_set_protocol::ScaleSetRunnerRequestId;

pub(crate) const SCALE_SET_DELIVERY_RECOVERY_SCHEMA_VERSION: u8 = 3;
pub(crate) const MAX_SCALE_SET_DELIVERY_RECOVERY_BYTES: usize = MAX_SCALE_SET_DELIVERY_BYTES * 4
    + MAX_DISPOSABLE_ATTEMPT_CATALOG_DOCUMENT_BYTES * 2
    + 64 * 1024;
const MAX_DELIVERY_RECOVERY_REVISION: u64 = 1_000_000_000_000;
const CATALOG_BINDING_DOMAIN: &[u8] = b"smolrunner.scale-set-catalog-binding.v1\0";

/// Pure recovery state for one durably reconciled Runner Scale Set delivery.
///
/// This document deliberately distinguishes a positively observed combined `ack` response from a
/// standalone acquisition replay after bridge death. An empty replay acquisition therefore stays
/// recovery evidence and never becomes proof that the earlier external operation succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScaleSetDeliveryRecoveryState {
    schema_version: u8,
    revision: u64,
    prior_catalog_revision: DisposableAttemptCatalogRevision,
    prior_catalog_digest: Sha256Digest,
    catalog_revision: DisposableAttemptCatalogRevision,
    catalog_digest: Sha256Digest,
    delivery: ScaleSetDelivery,
    phase: ScaleSetDeliveryRecoveryPhase,
}

impl ScaleSetDeliveryRecoveryState {
    pub(crate) fn reconciled(
        delivery: ScaleSetDelivery,
        prior_catalog: &DisposableAttemptCatalogDocument,
        catalog: &DisposableAttemptCatalogDocument,
    ) -> Result<Self, ScaleSetDeliveryRecoveryError> {
        let state = Self {
            schema_version: SCALE_SET_DELIVERY_RECOVERY_SCHEMA_VERSION,
            revision: 1,
            prior_catalog_revision: prior_catalog.revision(),
            prior_catalog_digest: catalog_digest(prior_catalog)?,
            catalog_revision: catalog.revision(),
            catalog_digest: catalog_digest(catalog)?,
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
    pub(crate) const fn prior_catalog_revision(&self) -> DisposableAttemptCatalogRevision {
        self.prior_catalog_revision
    }

    pub(crate) fn matches_prior_catalog(&self, catalog: &DisposableAttemptCatalogDocument) -> bool {
        self.prior_catalog_revision == catalog.revision()
            && catalog_digest(catalog).is_ok_and(|digest| digest == self.prior_catalog_digest)
    }

    pub(crate) fn matches_catalog(&self, catalog: &DisposableAttemptCatalogDocument) -> bool {
        self.catalog_revision == catalog.revision()
            && catalog_digest(catalog).is_ok_and(|digest| digest == self.catalog_digest)
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
        match &self.phase {
            ScaleSetDeliveryRecoveryPhase::Reconciled => {
                self.successor(ScaleSetDeliveryRecoveryPhase::AcknowledgementStarted)
            }
            ScaleSetDeliveryRecoveryPhase::AcknowledgementStarted => Ok(self.clone()),
            _ => Err(recovery_error(
                ScaleSetDeliveryRecoveryErrorKind::Conflict,
                "only a reconciled delivery may begin acknowledgement",
            )),
        }
    }

    pub(crate) fn record_ack_response(
        &self,
        acquired: &[ScaleSetRunnerRequestId],
    ) -> Result<Self, ScaleSetDeliveryRecoveryError> {
        let acquired = self.validated_acquired(acquired)?;
        match &self.phase {
            ScaleSetDeliveryRecoveryPhase::AcknowledgementStarted => {
                self.successor(ScaleSetDeliveryRecoveryPhase::Acknowledged { acquired })
            }
            ScaleSetDeliveryRecoveryPhase::Acknowledged { acquired: current }
                if current == &acquired =>
            {
                Ok(self.clone())
            }
            _ => Err(recovery_error(
                ScaleSetDeliveryRecoveryErrorKind::Conflict,
                "acknowledgement response conflicts with the durable recovery phase",
            )),
        }
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
        let phase =
            ScaleSetDeliveryRecoveryPhase::AcquisitionRecoveryObserved { acquired: observed };
        if self.phase == phase {
            return Ok(self.clone());
        }
        self.successor(phase)
    }

    pub(crate) fn prepare_settlement(
        &self,
        prior_catalog: &DisposableAttemptCatalogDocument,
        target_catalog: &DisposableAttemptCatalogDocument,
    ) -> Result<Self, ScaleSetDeliveryRecoveryError> {
        if !self.matches_catalog(prior_catalog) {
            return Err(recovery_error(
                ScaleSetDeliveryRecoveryErrorKind::Conflict,
                "Scale Set settlement prior catalog does not match durable recovery",
            ));
        }
        self.prepare_settlement_binding(
            prior_catalog.clone(),
            target_catalog.revision(),
            catalog_digest(target_catalog)?,
        )
    }

    pub(crate) fn prepare_settlement_binding(
        &self,
        prior_catalog: DisposableAttemptCatalogDocument,
        catalog_revision: DisposableAttemptCatalogRevision,
        catalog_digest: Sha256Digest,
    ) -> Result<Self, ScaleSetDeliveryRecoveryError> {
        let (proof, acquired) = match &self.phase {
            ScaleSetDeliveryRecoveryPhase::Acknowledged { acquired } => (
                ScaleSetDeliverySettlementProof::Acknowledged,
                acquired.clone(),
            ),
            ScaleSetDeliveryRecoveryPhase::AcquisitionRecoveryObserved { acquired }
                if !acquired.is_empty() =>
            {
                (
                    ScaleSetDeliverySettlementProof::AcquisitionRecovery,
                    acquired.clone(),
                )
            }
            _ => {
                return Err(recovery_error(
                    ScaleSetDeliveryRecoveryErrorKind::Conflict,
                    "Scale Set delivery lacks conclusive settlement evidence",
                ));
            }
        };
        self.successor(ScaleSetDeliveryRecoveryPhase::SettlementPrepared {
            proof,
            acquired,
            prior_catalog,
            catalog_revision,
            catalog_digest,
        })
    }

    pub(crate) fn matches_settlement_catalog(
        &self,
        catalog: &DisposableAttemptCatalogDocument,
    ) -> bool {
        matches!(
            &self.phase,
            ScaleSetDeliveryRecoveryPhase::SettlementPrepared {
                catalog_revision,
                catalog_digest: expected,
                ..
            } if *catalog_revision == catalog.revision()
                && catalog_digest(catalog).is_ok_and(|actual| actual == *expected)
        )
    }

    pub(crate) fn settlement_acquired(&self) -> Option<&[ScaleSetRunnerRequestId]> {
        match &self.phase {
            ScaleSetDeliveryRecoveryPhase::SettlementPrepared { acquired, .. } => Some(acquired),
            _ => None,
        }
    }

    pub(crate) fn settlement_prior_catalog(&self) -> Option<&DisposableAttemptCatalogDocument> {
        match &self.phase {
            ScaleSetDeliveryRecoveryPhase::SettlementPrepared { prior_catalog, .. } => {
                Some(prior_catalog)
            }
            _ => None,
        }
    }

    fn successor(
        &self,
        phase: ScaleSetDeliveryRecoveryPhase,
    ) -> Result<Self, ScaleSetDeliveryRecoveryError> {
        let revision = self
            .revision
            .checked_add(1)
            .filter(|revision| *revision <= MAX_DELIVERY_RECOVERY_REVISION)
            .ok_or_else(|| {
                recovery_error(
                    ScaleSetDeliveryRecoveryErrorKind::Conflict,
                    "delivery recovery revision cannot advance",
                )
            })?;
        let state = Self {
            schema_version: self.schema_version,
            revision,
            prior_catalog_revision: self.prior_catalog_revision,
            prior_catalog_digest: self.prior_catalog_digest.clone(),
            catalog_revision: self.catalog_revision,
            catalog_digest: self.catalog_digest.clone(),
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
        Ok(seen.into_iter().collect())
    }

    fn validate(&self) -> Result<(), ScaleSetDeliveryRecoveryError> {
        if self.schema_version != SCALE_SET_DELIVERY_RECOVERY_SCHEMA_VERSION
            || !(1..=MAX_DELIVERY_RECOVERY_REVISION).contains(&self.revision)
            || self.prior_catalog_revision.get() > self.catalog_revision.get()
            || (self.prior_catalog_revision == self.catalog_revision
                && self.prior_catalog_digest != self.catalog_digest)
        {
            return Err(corrupt_state());
        }
        encode_scale_set_delivery(&self.delivery).map_err(|_| corrupt_state())?;
        match &self.phase {
            ScaleSetDeliveryRecoveryPhase::Reconciled if self.revision == 1 => Ok(()),
            ScaleSetDeliveryRecoveryPhase::AcknowledgementStarted if self.revision == 2 => Ok(()),
            ScaleSetDeliveryRecoveryPhase::Acknowledged { acquired } if self.revision == 3 => {
                self.validated_acquired(acquired).map(|_| ())
            }
            ScaleSetDeliveryRecoveryPhase::AcquisitionRecoveryObserved { acquired }
                if self.revision >= 3 =>
            {
                self.validated_acquired(acquired).map(|_| ())
            }
            ScaleSetDeliveryRecoveryPhase::SettlementPrepared {
                proof,
                acquired,
                prior_catalog,
                catalog_revision,
                catalog_digest,
            } => {
                self.validated_acquired(acquired)?;
                if !self.matches_catalog(prior_catalog) {
                    return Err(corrupt_state());
                }
                if matches!(proof, ScaleSetDeliverySettlementProof::AcquisitionRecovery)
                    && acquired.is_empty()
                {
                    return Err(corrupt_state());
                }
                if (matches!(proof, ScaleSetDeliverySettlementProof::Acknowledged)
                    && self.revision != 4)
                    || (matches!(proof, ScaleSetDeliverySettlementProof::AcquisitionRecovery)
                        && self.revision < 4)
                {
                    return Err(corrupt_state());
                }
                if catalog_revision.get() < self.catalog_revision.get()
                    || (*catalog_revision == self.catalog_revision
                        && catalog_digest != &self.catalog_digest)
                {
                    return Err(corrupt_state());
                }
                Ok(())
            }
            _ => Err(corrupt_state()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScaleSetDeliverySettlementProof {
    Acknowledged,
    AcquisitionRecovery,
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
    SettlementPrepared {
        proof: ScaleSetDeliverySettlementProof,
        acquired: Vec<ScaleSetRunnerRequestId>,
        prior_catalog: DisposableAttemptCatalogDocument,
        catalog_revision: DisposableAttemptCatalogRevision,
        catalog_digest: Sha256Digest,
    },
}

/// Encode one validated recovery state into bounded canonical JSON bytes.
pub(crate) fn encode_scale_set_delivery_recovery(
    state: &ScaleSetDeliveryRecoveryState,
) -> Result<Vec<u8>, ScaleSetDeliveryRecoveryError> {
    state.validate()?;
    let wire = RecoveryWire::from_state(state)?;
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
    prior_catalog_revision: u64,
    prior_catalog_digest: String,
    catalog_revision: u64,
    catalog_digest: String,
    delivery_json: String,
    phase: RecoveryPhaseWire,
}

impl RecoveryWire {
    fn from_state(
        state: &ScaleSetDeliveryRecoveryState,
    ) -> Result<Self, ScaleSetDeliveryRecoveryError> {
        let delivery_bytes =
            encode_scale_set_delivery(&state.delivery).map_err(|_| corrupt_state())?;
        let delivery_json = String::from_utf8(delivery_bytes).map_err(|_| corrupt_state())?;
        Ok(Self {
            schema_version: state.schema_version,
            revision: state.revision,
            prior_catalog_revision: state.prior_catalog_revision.get(),
            prior_catalog_digest: state.prior_catalog_digest.as_str().to_owned(),
            catalog_revision: state.catalog_revision.get(),
            catalog_digest: state.catalog_digest.as_str().to_owned(),
            delivery_json,
            phase: RecoveryPhaseWire::from_phase(&state.phase)?,
        })
    }

    fn into_state(self) -> Result<ScaleSetDeliveryRecoveryState, ScaleSetDeliveryRecoveryError> {
        let delivery = decode_scale_set_delivery(self.delivery_json.as_bytes())
            .map_err(|_| corrupt_state())?;
        let prior_catalog_revision =
            DisposableAttemptCatalogRevision::new(self.prior_catalog_revision)
                .map_err(|_| corrupt_state())?;
        let prior_catalog_digest =
            Sha256Digest::parse(&self.prior_catalog_digest).map_err(|_| corrupt_state())?;
        let catalog_revision = DisposableAttemptCatalogRevision::new(self.catalog_revision)
            .map_err(|_| corrupt_state())?;
        let catalog_digest =
            Sha256Digest::parse(&self.catalog_digest).map_err(|_| corrupt_state())?;
        let state = ScaleSetDeliveryRecoveryState {
            schema_version: self.schema_version,
            revision: self.revision,
            prior_catalog_revision,
            prior_catalog_digest,
            catalog_revision,
            catalog_digest,
            delivery,
            phase: self.phase.into_phase()?,
        };
        state.validate()?;
        Ok(state)
    }
}

fn catalog_digest(
    catalog: &DisposableAttemptCatalogDocument,
) -> Result<Sha256Digest, ScaleSetDeliveryRecoveryError> {
    let bytes = encode_disposable_attempt_catalog(catalog).map_err(|_| corrupt_state())?;
    let mut hasher = Sha256::new();
    hasher.update(CATALOG_BINDING_DOMAIN);
    hasher.update(
        u64::try_from(bytes.len())
            .map_err(|_| corrupt_state())?
            .to_be_bytes(),
    );
    hasher.update(bytes);
    Sha256Digest::parse(&format!("sha256:{:x}", hasher.finalize())).map_err(|_| corrupt_state())
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
enum RecoveryPhaseWire {
    Reconciled,
    AcknowledgementStarted,
    Acknowledged {
        acquired_request_ids: Vec<u64>,
    },
    AcquisitionRecoveryObserved {
        acquired_request_ids: Vec<u64>,
    },
    SettlementPrepared {
        proof: SettlementProofWire,
        acquired_request_ids: Vec<u64>,
        prior_catalog_json: String,
        catalog_revision: u64,
        catalog_digest: String,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SettlementProofWire {
    Acknowledged,
    AcquisitionRecovery,
}

impl RecoveryPhaseWire {
    fn from_phase(
        phase: &ScaleSetDeliveryRecoveryPhase,
    ) -> Result<Self, ScaleSetDeliveryRecoveryError> {
        Ok(match phase {
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
            ScaleSetDeliveryRecoveryPhase::SettlementPrepared {
                proof,
                acquired,
                prior_catalog,
                catalog_revision,
                catalog_digest,
            } => Self::SettlementPrepared {
                proof: match proof {
                    ScaleSetDeliverySettlementProof::Acknowledged => {
                        SettlementProofWire::Acknowledged
                    }
                    ScaleSetDeliverySettlementProof::AcquisitionRecovery => {
                        SettlementProofWire::AcquisitionRecovery
                    }
                },
                acquired_request_ids: acquired.iter().map(|id| id.get()).collect(),
                prior_catalog_json: String::from_utf8(
                    encode_disposable_attempt_catalog(prior_catalog)
                        .map_err(|_| corrupt_state())?,
                )
                .map_err(|_| corrupt_state())?,
                catalog_revision: catalog_revision.get(),
                catalog_digest: catalog_digest.as_str().to_owned(),
            },
        })
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
            Self::SettlementPrepared {
                proof,
                acquired_request_ids,
                prior_catalog_json,
                catalog_revision,
                catalog_digest,
            } => Ok(ScaleSetDeliveryRecoveryPhase::SettlementPrepared {
                proof: match proof {
                    SettlementProofWire::Acknowledged => {
                        ScaleSetDeliverySettlementProof::Acknowledged
                    }
                    SettlementProofWire::AcquisitionRecovery => {
                        ScaleSetDeliverySettlementProof::AcquisitionRecovery
                    }
                },
                acquired: parse_request_ids(acquired_request_ids)?,
                prior_catalog: decode_disposable_attempt_catalog(prior_catalog_json.as_bytes())
                    .map_err(|_| corrupt_state())?,
                catalog_revision: DisposableAttemptCatalogRevision::new(catalog_revision)
                    .map_err(|_| corrupt_state())?,
                catalog_digest: Sha256Digest::parse(&catalog_digest)
                    .map_err(|_| corrupt_state())?,
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

    fn catalog() -> DisposableAttemptCatalogDocument {
        DisposableAttemptCatalogDocument::empty()
    }

    #[test]
    fn ack_response_is_distinct_from_recovery_acquisition() {
        let initial =
            ScaleSetDeliveryRecoveryState::reconciled(delivery(), &catalog(), &catalog()).unwrap();
        let started = initial.begin_ack().unwrap();
        assert_eq!(started.begin_ack().unwrap().revision(), started.revision());

        let expected = vec![ScaleSetRunnerRequestId::new(41).unwrap()];
        let acknowledged = started.record_ack_response(&expected).unwrap();
        match acknowledged.phase() {
            ScaleSetDeliveryRecoveryPhase::Acknowledged { acquired } => {
                assert_eq!(acquired, &expected);
            }
            phase => panic!("unexpected acknowledged phase: {phase:?}"),
        }
        assert_eq!(
            acknowledged
                .record_ack_response(&expected)
                .unwrap()
                .revision(),
            acknowledged.revision()
        );

        let recovery = started.record_recovery_acquire(&[]).unwrap();
        assert!(matches!(
            recovery.phase(),
            ScaleSetDeliveryRecoveryPhase::AcquisitionRecoveryObserved { acquired } if acquired.is_empty()
        ));
        assert_eq!(
            recovery.record_recovery_acquire(&[]).unwrap().revision(),
            recovery.revision()
        );
    }

    #[test]
    fn repeated_recovery_acquisition_unions_positive_evidence() {
        let started = ScaleSetDeliveryRecoveryState::reconciled(delivery(), &catalog(), &catalog())
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
    fn settlement_requires_conclusive_acknowledgement_evidence_and_binds_target_catalog() {
        let started = ScaleSetDeliveryRecoveryState::reconciled(delivery(), &catalog(), &catalog())
            .unwrap()
            .begin_ack()
            .unwrap();
        let empty_recovery = started.record_recovery_acquire(&[]).unwrap();
        assert_eq!(
            empty_recovery
                .prepare_settlement(&catalog(), &catalog())
                .unwrap_err()
                .kind(),
            ScaleSetDeliveryRecoveryErrorKind::Conflict
        );

        let acknowledged = started.record_ack_response(&[]).unwrap();
        let prepared = acknowledged
            .prepare_settlement(&catalog(), &catalog())
            .unwrap();
        assert!(prepared.matches_settlement_catalog(&catalog()));
        assert_eq!(prepared.settlement_acquired(), Some([].as_slice()));
        assert_eq!(
            decode_scale_set_delivery_recovery(
                &encode_scale_set_delivery_recovery(&prepared).unwrap()
            )
            .unwrap(),
            prepared
        );

        let positive = started
            .record_recovery_acquire(&[ScaleSetRunnerRequestId::new(41).unwrap()])
            .unwrap()
            .prepare_settlement(&catalog(), &catalog())
            .unwrap();
        assert_eq!(positive.settlement_acquired().unwrap().len(), 1);
    }

    #[test]
    fn foreign_or_duplicate_acquisition_evidence_conflicts() {
        let started = ScaleSetDeliveryRecoveryState::reconciled(delivery(), &catalog(), &catalog())
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
        let state = ScaleSetDeliveryRecoveryState::reconciled(delivery(), &catalog(), &catalog())
            .unwrap()
            .begin_ack()
            .unwrap();
        let encoded = encode_scale_set_delivery_recovery(&state).unwrap();
        assert_eq!(decode_scale_set_delivery_recovery(&encoded).unwrap(), state);

        let prior = String::from_utf8(encoded.clone())
            .unwrap()
            .replacen("\"schema_version\":3", "\"schema_version\":2", 1)
            .into_bytes();
        assert_eq!(
            decode_scale_set_delivery_recovery(&prior)
                .unwrap_err()
                .kind(),
            ScaleSetDeliveryRecoveryErrorKind::VersionIncompatible
        );

        let future = String::from_utf8(encoded.clone())
            .unwrap()
            .replacen("\"schema_version\":3", "\"schema_version\":4", 1)
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
