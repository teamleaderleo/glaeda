// This durable vocabulary lands immediately before the same-lock M3 consumer that uses it. Keep
// it private, and remove this allowance when the Unix store and service loop are connected.
#![allow(dead_code)]

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::disposable_attempt_catalog::{
    DisposableAttemptCatalogRevision, DisposableAttemptReservation,
};
use crate::disposable_attempt_state::DisposableAttemptRevision;
use crate::disposable_worker_reconciler::{CapacityClaimId, DisposableAttemptId};
use crate::execution_admission::EpochMillis;
use crate::github_scale_set_bridge::{
    ScaleSetBridgeEvent, ScaleSetBridgeIdentity, ScaleSetBridgeJobEvidence,
};
use crate::github_scale_set_protocol::{
    ScaleSetJobId, ScaleSetJobResult, ScaleSetRunnerId, ScaleSetRunnerName, ScaleSetRunnerReference,
};

pub(crate) const GITHUB_SCALE_SET_INBOX_SCHEMA_VERSION: u8 = 1;
pub(crate) const MAX_GITHUB_SCALE_SET_INBOX_BYTES: usize = 128 * 1024;
const MAX_INBOX_REVISION: u64 = 1_000_000_000_000;
const MAX_MESSAGE_EVENTS: usize = 50;
const MAX_ACKNOWLEDGED_MESSAGES: u64 = 1_000_000_000_000;
const MAX_APPLIED_EVENTS: u64 = 50_000_000_000_000;
const MAX_IDLE_OBSERVATIONS: u64 = 1_000_000_000_000;
const MAX_LABELS: usize = 32;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub(crate) struct ScaleSetInboxRevision(u64);

impl ScaleSetInboxRevision {
    fn new(value: u64) -> Result<Self, ScaleSetInboxError> {
        if !(1..=MAX_INBOX_REVISION).contains(&value) {
            return Err(ScaleSetInboxError::new("inbox_revision_invalid"));
        }
        Ok(Self(value))
    }

    pub(crate) const fn get(self) -> u64 {
        self.0
    }

    fn next(self) -> Result<Self, ScaleSetInboxError> {
        Self::new(
            self.0
                .checked_add(1)
                .ok_or_else(|| ScaleSetInboxError::new("inbox_revision_exhausted"))?,
        )
        .map_err(|_| ScaleSetInboxError::new("inbox_revision_exhausted"))
    }
}

impl fmt::Debug for ScaleSetInboxRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ScaleSetInboxRevision(<private>)")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ScaleSetAckReceipt {
    message_id: u32,
    events: Vec<ScaleSetBridgeEvent>,
    acquired_request_ids: Vec<u64>,
    outcome_applied: bool,
}

impl ScaleSetAckReceipt {
    pub(crate) const fn message_id(&self) -> u32 {
        self.message_id
    }

    pub(crate) fn events(&self) -> &[ScaleSetBridgeEvent] {
        &self.events
    }

    pub(crate) fn acquired_request_ids(&self) -> &[u64] {
        &self.acquired_request_ids
    }

    pub(crate) const fn outcome_applied(&self) -> bool {
        self.outcome_applied
    }

    pub(crate) fn offered_request_ids(&self) -> Vec<u64> {
        available_request_ids(&self.events)
    }
}

impl fmt::Debug for ScaleSetAckReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScaleSetAckReceipt")
            .field("message_id", &self.message_id)
            .field("event_count", &self.events.len())
            .field("acquired_count", &self.acquired_request_ids.len())
            .field("outcome_applied", &self.outcome_applied)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PendingScaleSetMessage {
    message_id: u32,
    observed_at: EpochMillis,
    not_after: EpochMillis,
    events: Vec<ScaleSetBridgeEvent>,
    next_event_index: usize,
    ack_started: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ScaleSetIdleObservation {
    observed_at: EpochMillis,
    not_after: EpochMillis,
    catalog_revision: DisposableAttemptCatalogRevision,
    attempt_id: DisposableAttemptId,
    attempt_revision: DisposableAttemptRevision,
    capacity_claim_id: CapacityClaimId,
}

impl ScaleSetIdleObservation {
    pub(crate) const fn observed_at(&self) -> EpochMillis {
        self.observed_at
    }

    pub(crate) const fn not_after(&self) -> EpochMillis {
        self.not_after
    }

    pub(crate) const fn catalog_revision(&self) -> DisposableAttemptCatalogRevision {
        self.catalog_revision
    }

    pub(crate) const fn attempt_id(&self) -> &DisposableAttemptId {
        &self.attempt_id
    }

    pub(crate) const fn attempt_revision(&self) -> DisposableAttemptRevision {
        self.attempt_revision
    }

    pub(crate) const fn capacity_claim_id(&self) -> &CapacityClaimId {
        &self.capacity_claim_id
    }
}

impl fmt::Debug for ScaleSetIdleObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScaleSetIdleObservation")
            .field("observed_at", &self.observed_at)
            .field("not_after", &self.not_after)
            .field("catalog_revision", &self.catalog_revision.get())
            .field("attempt", &"<bound>")
            .finish()
    }
}

impl PendingScaleSetMessage {
    pub(crate) const fn message_id(&self) -> u32 {
        self.message_id
    }

    pub(crate) const fn observed_at(&self) -> EpochMillis {
        self.observed_at
    }

    pub(crate) const fn not_after(&self) -> EpochMillis {
        self.not_after
    }

    pub(crate) fn events(&self) -> &[ScaleSetBridgeEvent] {
        &self.events
    }

    pub(crate) const fn next_event_index(&self) -> usize {
        self.next_event_index
    }

    pub(crate) const fn ack_started(&self) -> bool {
        self.ack_started
    }

    pub(crate) fn next_event(&self) -> Option<&ScaleSetBridgeEvent> {
        self.events.get(self.next_event_index)
    }

    fn available_request_ids(&self) -> Vec<u64> {
        available_request_ids(&self.events)
    }
}

impl fmt::Debug for PendingScaleSetMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PendingScaleSetMessage")
            .field("message_id", &self.message_id)
            .field("observed_at", &self.observed_at)
            .field("not_after", &self.not_after)
            .field("event_count", &self.events.len())
            .field("next_event_index", &self.next_event_index)
            .field("ack_started", &self.ack_started)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ScaleSetInboxDocument {
    schema_version: u8,
    source_identity: ScaleSetBridgeIdentity,
    revision: ScaleSetInboxRevision,
    acknowledged_messages: u64,
    applied_events: u64,
    idle_observations: u64,
    last_idle: Option<ScaleSetIdleObservation>,
    last_ack: Option<ScaleSetAckReceipt>,
    pending: Option<PendingScaleSetMessage>,
}

impl ScaleSetInboxDocument {
    pub(crate) const fn empty(source_identity: ScaleSetBridgeIdentity) -> Self {
        Self {
            schema_version: GITHUB_SCALE_SET_INBOX_SCHEMA_VERSION,
            source_identity,
            revision: ScaleSetInboxRevision(1),
            acknowledged_messages: 0,
            applied_events: 0,
            idle_observations: 0,
            last_idle: None,
            last_ack: None,
            pending: None,
        }
    }

    pub(crate) const fn revision(&self) -> ScaleSetInboxRevision {
        self.revision
    }

    pub(crate) const fn source_identity(&self) -> &ScaleSetBridgeIdentity {
        &self.source_identity
    }

    pub(crate) const fn is_initial(&self) -> bool {
        self.revision.0 == 1
            && self.acknowledged_messages == 0
            && self.applied_events == 0
            && self.idle_observations == 0
            && self.last_idle.is_none()
            && self.last_ack.is_none()
            && self.pending.is_none()
    }

    pub(crate) const fn last_ack(&self) -> Option<&ScaleSetAckReceipt> {
        self.last_ack.as_ref()
    }

    pub(crate) const fn pending(&self) -> Option<&PendingScaleSetMessage> {
        self.pending.as_ref()
    }

    pub(crate) const fn last_idle(&self) -> Option<&ScaleSetIdleObservation> {
        self.last_idle.as_ref()
    }

    pub(crate) fn requires_reconciliation(&self) -> bool {
        self.pending.is_some()
            || self
                .last_ack
                .as_ref()
                .is_some_and(|ack| !ack.outcome_applied)
    }

    pub(crate) fn record_idle(
        &self,
        observed_at: EpochMillis,
        not_after: EpochMillis,
        catalog_revision: DisposableAttemptCatalogRevision,
        reservation: &DisposableAttemptReservation,
    ) -> Result<Self, ScaleSetInboxError> {
        self.validate()?;
        if self.requires_reconciliation()
            || not_after < observed_at
            || reservation.attempt().github_job_id().is_none()
            || !matches!(
                reservation.attempt().phase(),
                crate::disposable_worker_reconciler::DisposableAttemptPhase::Reserved
                    | crate::disposable_worker_reconciler::DisposableAttemptPhase::CloneAuthorized
            )
        {
            return Err(ScaleSetInboxError::new("inbox_idle_refused"));
        }
        let mut next = self.clone();
        next.revision = self.revision.next()?;
        next.idle_observations = self
            .idle_observations
            .checked_add(1)
            .ok_or_else(|| ScaleSetInboxError::new("inbox_history_exhausted"))?;
        next.last_idle = Some(ScaleSetIdleObservation {
            observed_at,
            not_after,
            catalog_revision,
            attempt_id: reservation.attempt().attempt_id().clone(),
            attempt_revision: reservation.attempt().revision(),
            capacity_claim_id: reservation.attempt().capacity_claim_id().clone(),
        });
        next.validate()?;
        Ok(next)
    }

    pub(crate) fn record(
        &self,
        message_id: u32,
        observed_at: EpochMillis,
        not_after: EpochMillis,
        events: Vec<ScaleSetBridgeEvent>,
    ) -> Result<Self, ScaleSetInboxError> {
        self.validate()?;
        if message_id == 0
            || self.pending.is_some()
            || self
                .last_ack
                .as_ref()
                .is_some_and(|ack| message_id <= ack.message_id)
            || self
                .last_ack
                .as_ref()
                .is_some_and(|ack| !ack.outcome_applied)
            || events.len() > MAX_MESSAGE_EVENTS
            || not_after < observed_at
        {
            return Err(ScaleSetInboxError::new("inbox_message_refused"));
        }
        validate_events(&events)?;
        let mut next = self.clone();
        next.revision = self.revision.next()?;
        next.last_idle = None;
        next.pending = Some(PendingScaleSetMessage {
            message_id,
            observed_at,
            not_after,
            events,
            next_event_index: 0,
            ack_started: false,
        });
        next.validate()?;
        Ok(next)
    }

    pub(crate) fn mark_next_event_applied(
        &self,
        message_id: u32,
        expected_event_index: usize,
    ) -> Result<Self, ScaleSetInboxError> {
        self.validate()?;
        let pending = self
            .pending
            .as_ref()
            .filter(|pending| {
                pending.message_id == message_id
                    && !pending.ack_started
                    && pending.next_event_index == expected_event_index
                    && pending.next_event_index < pending.events.len()
            })
            .ok_or_else(|| ScaleSetInboxError::new("inbox_event_conflict"))?;
        let mut next = self.clone();
        next.revision = self.revision.next()?;
        next.pending
            .as_mut()
            .expect("pending message was checked")
            .next_event_index = pending.next_event_index + 1;
        next.validate()?;
        Ok(next)
    }

    pub(crate) fn begin_ack(&self, message_id: u32) -> Result<Self, ScaleSetInboxError> {
        self.validate()?;
        let pending = self
            .pending
            .as_ref()
            .filter(|pending| {
                pending.message_id == message_id
                    && !pending.ack_started
                    && pending.next_event_index == pending.events.len()
            })
            .ok_or_else(|| ScaleSetInboxError::new("inbox_ack_refused"))?;
        let mut next = self.clone();
        next.revision = self.revision.next()?;
        next.pending
            .as_mut()
            .expect("pending message was checked")
            .ack_started = true;
        debug_assert_eq!(pending.message_id, message_id);
        next.validate()?;
        Ok(next)
    }

    pub(crate) fn complete_ack(
        &self,
        message_id: u32,
        mut acquired_request_ids: Vec<u64>,
    ) -> Result<Self, ScaleSetInboxError> {
        self.validate()?;
        let pending = self
            .pending
            .as_ref()
            .filter(|pending| pending.message_id == message_id && pending.ack_started)
            .ok_or_else(|| ScaleSetInboxError::new("inbox_ack_refused"))?;
        acquired_request_ids.sort_unstable();
        if acquired_request_ids.contains(&0)
            || acquired_request_ids
                .windows(2)
                .any(|pair| pair[0] == pair[1])
        {
            return Err(ScaleSetInboxError::new("inbox_acquisition_invalid"));
        }
        let offered_request_ids = pending.available_request_ids();
        if acquired_request_ids
            .iter()
            .any(|id| !offered_request_ids.contains(id))
        {
            return Err(ScaleSetInboxError::new("inbox_acquisition_invalid"));
        }
        let event_count = u64::try_from(pending.events.len())
            .map_err(|_| ScaleSetInboxError::new("inbox_history_invalid"))?;
        let mut next = self.clone();
        next.revision = self.revision.next()?;
        next.acknowledged_messages = self
            .acknowledged_messages
            .checked_add(1)
            .ok_or_else(|| ScaleSetInboxError::new("inbox_history_exhausted"))?;
        next.applied_events = self
            .applied_events
            .checked_add(event_count)
            .ok_or_else(|| ScaleSetInboxError::new("inbox_history_exhausted"))?;
        next.last_ack = Some(ScaleSetAckReceipt {
            message_id,
            events: pending.events.clone(),
            acquired_request_ids,
            outcome_applied: false,
        });
        next.pending = None;
        next.validate()?;
        Ok(next)
    }

    pub(crate) fn mark_ack_outcome_applied(
        &self,
        message_id: u32,
    ) -> Result<Self, ScaleSetInboxError> {
        self.validate()?;
        let ack = self
            .last_ack
            .as_ref()
            .filter(|ack| ack.message_id == message_id && !ack.outcome_applied)
            .ok_or_else(|| ScaleSetInboxError::new("inbox_ack_outcome_refused"))?;
        if self.pending.is_some() {
            return Err(ScaleSetInboxError::new("inbox_ack_outcome_refused"));
        }
        let mut next = self.clone();
        next.revision = self.revision.next()?;
        next.last_ack
            .as_mut()
            .expect("ack receipt was checked")
            .outcome_applied = true;
        debug_assert_eq!(ack.message_id, message_id);
        next.validate()?;
        Ok(next)
    }

    pub(crate) fn validate_successor_of(&self, current: &Self) -> Result<(), ScaleSetInboxError> {
        current.validate()?;
        self.validate()?;
        if self.revision != current.revision.next()? {
            return Err(ScaleSetInboxError::new("inbox_successor_invalid"));
        }
        let valid = match (&current.pending, &self.pending) {
            (None, Some(next)) => {
                current.acknowledged_messages == self.acknowledged_messages
                    && current.applied_events == self.applied_events
                    && current.idle_observations == self.idle_observations
                    && current.last_ack == self.last_ack
                    && self.last_idle.is_none()
                    && next.next_event_index == 0
                    && !next.ack_started
                    && next.message_id > current.last_ack.as_ref().map_or(0, |ack| ack.message_id)
            }
            (Some(prior), Some(next)) => {
                current.acknowledged_messages == self.acknowledged_messages
                    && current.applied_events == self.applied_events
                    && current.idle_observations == self.idle_observations
                    && current.last_idle == self.last_idle
                    && current.last_ack == self.last_ack
                    && prior.message_id == next.message_id
                    && prior.events == next.events
                    && ((next.next_event_index == prior.next_event_index + 1
                        && !prior.ack_started
                        && !next.ack_started)
                        || (next.next_event_index == prior.next_event_index
                            && prior.next_event_index == prior.events.len()
                            && !prior.ack_started
                            && next.ack_started))
            }
            (Some(prior), None) => {
                let next_acknowledged_messages = current.acknowledged_messages.checked_add(1);
                let next_applied_events = u64::try_from(prior.events.len())
                    .ok()
                    .and_then(|count| current.applied_events.checked_add(count));
                prior.ack_started
                    && prior.next_event_index == prior.events.len()
                    && current.idle_observations == self.idle_observations
                    && current.last_idle == self.last_idle
                    && Some(self.acknowledged_messages) == next_acknowledged_messages
                    && Some(self.applied_events) == next_applied_events
                    && self.last_ack.as_ref().is_some_and(|ack| {
                        ack.message_id == prior.message_id
                            && ack.events == prior.events
                            && !ack.outcome_applied
                    })
            }
            (None, None) => {
                let common_history = current.acknowledged_messages == self.acknowledged_messages
                    && current.applied_events == self.applied_events;
                let ack_outcome = current.idle_observations == self.idle_observations
                    && current.last_idle == self.last_idle
                    && current.last_ack.as_ref().is_some_and(|prior| {
                        self.last_ack.as_ref().is_some_and(|next| {
                            prior.message_id == next.message_id
                                && prior.events == next.events
                                && prior.acquired_request_ids == next.acquired_request_ids
                                && !prior.outcome_applied
                                && next.outcome_applied
                        })
                    });
                let idle_observation = current
                    .idle_observations
                    .checked_add(1)
                    .is_some_and(|next| next == self.idle_observations)
                    && current.last_ack == self.last_ack
                    && self.last_idle.is_some();
                common_history && (ack_outcome || idle_observation)
            }
        };
        if valid && self.source_identity == current.source_identity {
            Ok(())
        } else {
            Err(ScaleSetInboxError::new("inbox_successor_invalid"))
        }
    }

    fn validate(&self) -> Result<(), ScaleSetInboxError> {
        if self.schema_version != GITHUB_SCALE_SET_INBOX_SCHEMA_VERSION
            || self.acknowledged_messages > MAX_ACKNOWLEDGED_MESSAGES
            || self.applied_events > MAX_APPLIED_EVENTS
            || self.idle_observations > MAX_IDLE_OBSERVATIONS
            || self.last_ack.is_some() != (self.acknowledged_messages > 0)
            || (self.idle_observations == 0 && self.last_idle.is_some())
        {
            return Err(ScaleSetInboxError::new("inbox_document_invalid"));
        }
        if let Some(ack) = &self.last_ack {
            if ack.message_id == 0 {
                return Err(ScaleSetInboxError::new("inbox_document_invalid"));
            }
            validate_events(&ack.events)?;
            let offered_request_ids = ack.offered_request_ids();
            validate_positive_unique(&ack.acquired_request_ids)?;
            if ack
                .acquired_request_ids
                .iter()
                .any(|id| !offered_request_ids.contains(id))
            {
                return Err(ScaleSetInboxError::new("inbox_acquisition_invalid"));
            }
        }
        if let Some(idle) = &self.last_idle
            && idle.not_after < idle.observed_at
        {
            return Err(ScaleSetInboxError::new("inbox_timing_invalid"));
        }
        let pending_cost = if let Some(pending) = &self.pending {
            if pending.message_id == 0
                || pending.not_after < pending.observed_at
                || pending.events.len() > MAX_MESSAGE_EVENTS
                || pending.next_event_index > pending.events.len()
                || pending.ack_started && pending.next_event_index != pending.events.len()
                || self
                    .last_ack
                    .as_ref()
                    .is_some_and(|ack| pending.message_id <= ack.message_id)
            {
                return Err(ScaleSetInboxError::new("inbox_document_invalid"));
            }
            validate_events(&pending.events)?;
            1_u64
                .checked_add(
                    u64::try_from(pending.next_event_index)
                        .map_err(|_| ScaleSetInboxError::new("inbox_history_invalid"))?,
                )
                .and_then(|cost| cost.checked_add(u64::from(pending.ack_started)))
                .ok_or_else(|| ScaleSetInboxError::new("inbox_history_invalid"))?
        } else {
            0
        };
        let unresolved_ack_outcome = u64::from(
            self.last_ack
                .as_ref()
                .is_some_and(|ack| !ack.outcome_applied),
        );
        let expected_revision = 1_u64
            .checked_add(
                self.acknowledged_messages
                    .checked_mul(4)
                    .ok_or_else(|| ScaleSetInboxError::new("inbox_history_invalid"))?,
            )
            .and_then(|revision| revision.checked_add(self.applied_events))
            .and_then(|revision| revision.checked_add(self.idle_observations))
            .and_then(|revision| revision.checked_add(pending_cost))
            .and_then(|revision| revision.checked_sub(unresolved_ack_outcome))
            .ok_or_else(|| ScaleSetInboxError::new("inbox_history_invalid"))?;
        if self.revision != ScaleSetInboxRevision::new(expected_revision)? {
            return Err(ScaleSetInboxError::new("inbox_history_invalid"));
        }
        Ok(())
    }
}

impl fmt::Debug for ScaleSetInboxDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScaleSetInboxDocument")
            .field("schema_version", &self.schema_version)
            .field("source_identity", &self.source_identity)
            .field("revision", &self.revision)
            .field("acknowledged_messages", &self.acknowledged_messages)
            .field("applied_events", &self.applied_events)
            .field("idle_observations", &self.idle_observations)
            .field("last_idle", &self.last_idle)
            .field("last_ack", &self.last_ack)
            .field("pending", &self.pending)
            .finish()
    }
}

pub(crate) fn encode_scale_set_inbox(
    document: &ScaleSetInboxDocument,
) -> Result<Vec<u8>, ScaleSetInboxError> {
    document.validate()?;
    let wire = InboxWire::from(document);
    let mut bytes = serde_json::to_vec_pretty(&wire)
        .map_err(|_| ScaleSetInboxError::new("inbox_encode_failed"))?;
    bytes.push(b'\n');
    if bytes.len() > MAX_GITHUB_SCALE_SET_INBOX_BYTES {
        return Err(ScaleSetInboxError::new("inbox_document_too_large"));
    }
    Ok(bytes)
}

pub(crate) fn decode_scale_set_inbox(
    bytes: &[u8],
) -> Result<ScaleSetInboxDocument, ScaleSetInboxError> {
    if bytes.is_empty() || bytes.len() > MAX_GITHUB_SCALE_SET_INBOX_BYTES {
        return Err(ScaleSetInboxError::new("inbox_document_too_large"));
    }
    let version: VersionWire = serde_json::from_slice(bytes)
        .map_err(|_| ScaleSetInboxError::new("inbox_decode_failed"))?;
    if version.schema_version != GITHUB_SCALE_SET_INBOX_SCHEMA_VERSION {
        return Err(ScaleSetInboxError::new("inbox_version_incompatible"));
    }
    let wire: InboxWire = serde_json::from_slice(bytes)
        .map_err(|_| ScaleSetInboxError::new("inbox_decode_failed"))?;
    let document = ScaleSetInboxDocument::try_from(wire)?;
    if encode_scale_set_inbox(&document)? != bytes {
        return Err(ScaleSetInboxError::new("inbox_noncanonical"));
    }
    Ok(document)
}

#[derive(Deserialize)]
struct VersionWire {
    schema_version: u8,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InboxWire {
    schema_version: u8,
    source_identity: String,
    revision: u64,
    acknowledged_messages: u64,
    applied_events: u64,
    idle_observations: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_idle: Option<IdleWire>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_ack: Option<AckWire>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pending: Option<PendingWire>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AckWire {
    message_id: u32,
    events: Vec<EventWire>,
    acquired_request_ids: Vec<u64>,
    outcome_applied: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingWire {
    message_id: u32,
    observed_at_millis: u64,
    not_after_millis: u64,
    events: Vec<EventWire>,
    next_event_index: usize,
    ack_started: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdleWire {
    observed_at_millis: u64,
    not_after_millis: u64,
    catalog_revision: u64,
    attempt_id: String,
    attempt_revision: u64,
    capacity_claim_id: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EventWire {
    kind: String,
    runner_request_id: u64,
    repository: String,
    owner: String,
    job_id: String,
    workflow_run_id: u64,
    request_labels: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runner_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runner_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<String>,
}

impl From<&ScaleSetInboxDocument> for InboxWire {
    fn from(document: &ScaleSetInboxDocument) -> Self {
        Self {
            schema_version: document.schema_version,
            source_identity: document.source_identity.as_str().to_owned(),
            revision: document.revision.get(),
            acknowledged_messages: document.acknowledged_messages,
            applied_events: document.applied_events,
            idle_observations: document.idle_observations,
            last_idle: document.last_idle.as_ref().map(|idle| IdleWire {
                observed_at_millis: idle.observed_at.get(),
                not_after_millis: idle.not_after.get(),
                catalog_revision: idle.catalog_revision.get(),
                attempt_id: idle.attempt_id.as_str().to_owned(),
                attempt_revision: idle.attempt_revision.get(),
                capacity_claim_id: idle.capacity_claim_id.as_str().to_owned(),
            }),
            last_ack: document.last_ack.as_ref().map(|ack| AckWire {
                message_id: ack.message_id,
                events: ack.events.iter().map(EventWire::from).collect(),
                acquired_request_ids: ack.acquired_request_ids.clone(),
                outcome_applied: ack.outcome_applied,
            }),
            pending: document.pending.as_ref().map(|pending| PendingWire {
                message_id: pending.message_id,
                observed_at_millis: pending.observed_at.get(),
                not_after_millis: pending.not_after.get(),
                events: pending.events.iter().map(EventWire::from).collect(),
                next_event_index: pending.next_event_index,
                ack_started: pending.ack_started,
            }),
        }
    }
}

impl TryFrom<InboxWire> for ScaleSetInboxDocument {
    type Error = ScaleSetInboxError;

    fn try_from(wire: InboxWire) -> Result<Self, Self::Error> {
        let document = Self {
            schema_version: wire.schema_version,
            source_identity: ScaleSetBridgeIdentity::parse(&wire.source_identity)
                .map_err(|_| ScaleSetInboxError::new("inbox_source_invalid"))?,
            revision: ScaleSetInboxRevision::new(wire.revision)?,
            acknowledged_messages: wire.acknowledged_messages,
            applied_events: wire.applied_events,
            idle_observations: wire.idle_observations,
            last_idle: wire
                .last_idle
                .map(|idle| {
                    Ok(ScaleSetIdleObservation {
                        observed_at: EpochMillis::new(idle.observed_at_millis)
                            .map_err(|_| ScaleSetInboxError::new("inbox_timing_invalid"))?,
                        not_after: EpochMillis::new(idle.not_after_millis)
                            .map_err(|_| ScaleSetInboxError::new("inbox_timing_invalid"))?,
                        catalog_revision: DisposableAttemptCatalogRevision::new(
                            idle.catalog_revision,
                        )
                        .map_err(|_| ScaleSetInboxError::new("inbox_idle_invalid"))?,
                        attempt_id: DisposableAttemptId::parse(&idle.attempt_id)
                            .map_err(|_| ScaleSetInboxError::new("inbox_idle_invalid"))?,
                        attempt_revision: DisposableAttemptRevision::new(idle.attempt_revision)
                            .map_err(|_| ScaleSetInboxError::new("inbox_idle_invalid"))?,
                        capacity_claim_id: CapacityClaimId::parse(&idle.capacity_claim_id)
                            .map_err(|_| ScaleSetInboxError::new("inbox_idle_invalid"))?,
                    })
                })
                .transpose()?,
            last_ack: wire
                .last_ack
                .map(|ack| {
                    Ok(ScaleSetAckReceipt {
                        message_id: ack.message_id,
                        events: ack
                            .events
                            .into_iter()
                            .map(ScaleSetBridgeEvent::try_from)
                            .collect::<Result<Vec<_>, _>>()?,
                        acquired_request_ids: ack.acquired_request_ids,
                        outcome_applied: ack.outcome_applied,
                    })
                })
                .transpose()?,
            pending: wire
                .pending
                .map(|pending| {
                    Ok(PendingScaleSetMessage {
                        message_id: pending.message_id,
                        observed_at: EpochMillis::new(pending.observed_at_millis)
                            .map_err(|_| ScaleSetInboxError::new("inbox_timing_invalid"))?,
                        not_after: EpochMillis::new(pending.not_after_millis)
                            .map_err(|_| ScaleSetInboxError::new("inbox_timing_invalid"))?,
                        events: pending
                            .events
                            .into_iter()
                            .map(ScaleSetBridgeEvent::try_from)
                            .collect::<Result<Vec<_>, _>>()?,
                        next_event_index: pending.next_event_index,
                        ack_started: pending.ack_started,
                    })
                })
                .transpose()?,
        };
        document.validate()?;
        Ok(document)
    }
}

impl From<&ScaleSetBridgeEvent> for EventWire {
    fn from(event: &ScaleSetBridgeEvent) -> Self {
        let (kind, job, runner, result) = match event {
            ScaleSetBridgeEvent::Available(job) => ("available", job, None, None),
            ScaleSetBridgeEvent::Assigned(job) => ("assigned", job, None, None),
            ScaleSetBridgeEvent::Started { job, runner } => ("started", job, Some(runner), None),
            ScaleSetBridgeEvent::Completed {
                job,
                runner,
                result,
            } => ("completed", job, runner.as_ref(), Some(result)),
        };
        Self {
            kind: kind.to_owned(),
            runner_request_id: job.runner_request_id,
            repository: job.repository.clone(),
            owner: job.owner.clone(),
            job_id: job.job_id.as_str().to_owned(),
            workflow_run_id: job.workflow_run_id,
            request_labels: job.request_labels.clone(),
            runner_id: runner.map(|runner| runner.id.get()),
            runner_name: runner.map(|runner| runner.name.as_str().to_owned()),
            result: result.map(|result| result.as_str().to_owned()),
        }
    }
}

impl TryFrom<EventWire> for ScaleSetBridgeEvent {
    type Error = ScaleSetInboxError;

    fn try_from(wire: EventWire) -> Result<Self, Self::Error> {
        let job = ScaleSetBridgeJobEvidence {
            runner_request_id: wire.runner_request_id,
            repository: wire.repository,
            owner: wire.owner,
            job_id: ScaleSetJobId::parse(&wire.job_id)
                .map_err(|_| ScaleSetInboxError::new("inbox_event_invalid"))?,
            workflow_run_id: wire.workflow_run_id,
            request_labels: wire.request_labels,
        };
        let runner = match (wire.runner_id, wire.runner_name) {
            (Some(id), Some(name)) => Some(ScaleSetRunnerReference::new(
                ScaleSetRunnerId::new(id)
                    .map_err(|_| ScaleSetInboxError::new("inbox_event_invalid"))?,
                ScaleSetRunnerName::parse(&name)
                    .map_err(|_| ScaleSetInboxError::new("inbox_event_invalid"))?,
            )),
            (None, None) => None,
            _ => return Err(ScaleSetInboxError::new("inbox_event_invalid")),
        };
        let result = wire
            .result
            .map(|result| {
                ScaleSetJobResult::parse(&result)
                    .map_err(|_| ScaleSetInboxError::new("inbox_event_invalid"))
            })
            .transpose()?;
        let event = match wire.kind.as_str() {
            "available" if runner.is_none() && result.is_none() => Self::Available(job),
            "assigned" if runner.is_none() && result.is_none() => Self::Assigned(job),
            "started" if runner.is_some() && result.is_none() => Self::Started {
                job,
                runner: runner.expect("runner presence checked"),
            },
            "completed" if result.is_some() => {
                let result = result.expect("result presence checked");
                if runner.is_none() && result.as_str() != "canceled" {
                    return Err(ScaleSetInboxError::new("inbox_event_invalid"));
                }
                Self::Completed {
                    job,
                    runner,
                    result,
                }
            }
            _ => return Err(ScaleSetInboxError::new("inbox_event_invalid")),
        };
        validate_events(std::slice::from_ref(&event))?;
        Ok(event)
    }
}

fn validate_events(events: &[ScaleSetBridgeEvent]) -> Result<(), ScaleSetInboxError> {
    let mut available = BTreeSet::new();
    for event in events {
        let job = match event {
            ScaleSetBridgeEvent::Available(job) => {
                if !available.insert(job.runner_request_id) {
                    return Err(ScaleSetInboxError::new("inbox_event_invalid"));
                }
                job
            }
            ScaleSetBridgeEvent::Assigned(job)
            | ScaleSetBridgeEvent::Started { job, .. }
            | ScaleSetBridgeEvent::Completed { job, .. } => job,
        };
        if job.runner_request_id == 0
            || job.workflow_run_id == 0
            || !bounded_token(&job.repository, 100)
            || !bounded_token(&job.owner, 100)
            || job.request_labels.len() > MAX_LABELS
            || job
                .request_labels
                .iter()
                .any(|label| !bounded_token(label, 100))
        {
            return Err(ScaleSetInboxError::new("inbox_event_invalid"));
        }
    }
    Ok(())
}

fn available_request_ids(events: &[ScaleSetBridgeEvent]) -> Vec<u64> {
    let mut request_ids = events
        .iter()
        .filter_map(|event| match event {
            ScaleSetBridgeEvent::Available(job) => Some(job.runner_request_id),
            _ => None,
        })
        .collect::<Vec<_>>();
    request_ids.sort_unstable();
    request_ids
}

fn validate_positive_unique(values: &[u64]) -> Result<(), ScaleSetInboxError> {
    if values.contains(&0) || values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(ScaleSetInboxError::new("inbox_acquisition_invalid"));
    }
    Ok(())
}

fn bounded_token(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ScaleSetInboxError {
    code: &'static str,
}

impl ScaleSetInboxError {
    pub(crate) const fn new(code: &'static str) -> Self {
        Self { code }
    }

    pub(crate) const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for ScaleSetInboxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScaleSetInboxError")
            .field("code", &self.code)
            .finish()
    }
}

impl fmt::Display for ScaleSetInboxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code)
    }
}

impl std::error::Error for ScaleSetInboxError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn source_identity() -> ScaleSetBridgeIdentity {
        ScaleSetBridgeIdentity::parse(&format!("sha256:{}", "11".repeat(32))).unwrap()
    }

    fn empty() -> ScaleSetInboxDocument {
        ScaleSetInboxDocument::empty(source_identity())
    }

    fn record(
        document: &ScaleSetInboxDocument,
        message_id: u32,
        events: Vec<ScaleSetBridgeEvent>,
    ) -> ScaleSetInboxDocument {
        document
            .record(
                message_id,
                EpochMillis::new(100_000).unwrap(),
                EpochMillis::new(120_000).unwrap(),
                events,
            )
            .unwrap()
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
    fn message_is_applied_before_ack_and_replays_canonically() {
        let document = record(
            &empty(),
            7,
            vec![
                ScaleSetBridgeEvent::Available(job(41, "job-1")),
                ScaleSetBridgeEvent::Assigned(job(41, "job-1")),
            ],
        );
        assert_eq!(document.revision().get(), 2);
        assert!(document.begin_ack(7).is_err());
        let document = document.mark_next_event_applied(7, 0).unwrap();
        let document = document.mark_next_event_applied(7, 1).unwrap();
        let started = document.begin_ack(7).unwrap();
        assert!(started.pending().unwrap().ack_started());
        assert_eq!(started.revision().get(), 5);
        let completed = started.complete_ack(7, vec![41]).unwrap();
        assert_eq!(completed.revision().get(), 6);
        assert_eq!(completed.last_ack().unwrap().message_id(), 7);
        assert_eq!(completed.last_ack().unwrap().offered_request_ids(), [41]);
        assert_eq!(completed.last_ack().unwrap().acquired_request_ids(), [41]);
        assert!(!completed.last_ack().unwrap().outcome_applied());
        assert!(completed.pending().is_none());

        let bytes = encode_scale_set_inbox(&completed).unwrap();
        assert_eq!(decode_scale_set_inbox(&bytes).unwrap(), completed);
        let reconciled = completed.mark_ack_outcome_applied(7).unwrap();
        assert_eq!(reconciled.revision().get(), 7);
        assert!(reconciled.last_ack().unwrap().outcome_applied());
    }

    #[test]
    fn acknowledgement_started_is_durable_recovery_debt() {
        let recorded = record(
            &empty(),
            7,
            vec![ScaleSetBridgeEvent::Available(job(41, "job-1"))],
        );
        let applied = recorded.mark_next_event_applied(7, 0).unwrap();
        let started = applied.begin_ack(7).unwrap();
        let replayed = decode_scale_set_inbox(&encode_scale_set_inbox(&started).unwrap()).unwrap();
        assert!(replayed.pending().unwrap().ack_started());
        assert!(
            replayed
                .record(
                    8,
                    EpochMillis::new(100_000).unwrap(),
                    EpochMillis::new(120_000).unwrap(),
                    Vec::new(),
                )
                .is_err()
        );
        assert!(replayed.mark_next_event_applied(7, 1).is_err());
    }

    #[test]
    fn invalid_acquisition_and_impossible_revision_fail_closed() {
        let recorded = record(
            &empty(),
            7,
            vec![ScaleSetBridgeEvent::Available(job(41, "job-1"))],
        )
        .mark_next_event_applied(7, 0)
        .unwrap()
        .begin_ack(7)
        .unwrap();
        assert_eq!(
            recorded.complete_ack(7, vec![42]).unwrap_err().code(),
            "inbox_acquisition_invalid"
        );

        let mut wire: serde_json::Value =
            serde_json::from_slice(&encode_scale_set_inbox(&empty()).unwrap()).unwrap();
        wire["revision"] = 2.into();
        let mut bytes = serde_json::to_vec_pretty(&wire).unwrap();
        bytes.push(b'\n');
        assert!(decode_scale_set_inbox(&bytes).is_err());
    }

    #[test]
    fn successor_validation_rejects_skips_and_rebinding() {
        let empty = empty();
        let recorded = record(
            &empty,
            7,
            vec![ScaleSetBridgeEvent::Available(job(41, "job-1"))],
        );
        recorded.validate_successor_of(&empty).unwrap();
        let applied = recorded.mark_next_event_applied(7, 0).unwrap();
        applied.validate_successor_of(&recorded).unwrap();
        let started = applied.begin_ack(7).unwrap();
        started.validate_successor_of(&applied).unwrap();
        let completed = started.complete_ack(7, vec![41]).unwrap();
        completed.validate_successor_of(&started).unwrap();
        let reconciled = completed.mark_ack_outcome_applied(7).unwrap();
        reconciled.validate_successor_of(&completed).unwrap();
        assert!(completed.validate_successor_of(&empty).is_err());
    }

    #[test]
    fn incompatible_version_precedes_current_schema_validation() {
        let bytes = br#"{
  "schema_version": 2,
  "future_field": true
}
"#;
        assert_eq!(
            decode_scale_set_inbox(bytes).unwrap_err().code(),
            "inbox_version_incompatible"
        );
    }

    #[test]
    fn persisted_ack_receipt_must_bind_acquisition_to_the_offered_set() {
        let bytes = br#"{
  "schema_version": 1,
  "source_identity": "sha256:1111111111111111111111111111111111111111111111111111111111111111",
  "revision": 4,
  "acknowledged_messages": 1,
  "applied_events": 0,
  "idle_observations": 0,
  "last_ack": {
    "message_id": 7,
    "events": [{
      "kind": "available",
      "runner_request_id": 41,
      "repository": "project",
      "owner": "example",
      "job_id": "job-1",
      "workflow_run_id": 99,
      "request_labels": ["smolrunner"]
    }],
    "acquired_request_ids": [42],
    "outcome_applied": false
  }
}
"#;
        assert_eq!(
            decode_scale_set_inbox(bytes).unwrap_err().code(),
            "inbox_acquisition_invalid"
        );
    }
}
