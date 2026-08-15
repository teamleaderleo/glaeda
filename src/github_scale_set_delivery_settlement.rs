#![allow(dead_code)]

use std::collections::BTreeSet;
use std::fmt;

use crate::disposable_attempt_catalog::{
    DisposableAttemptCatalogAction, DisposableAttemptCatalogDocument,
};
use crate::disposable_worker_reconciler::DisposableAttemptPhase;
use crate::github_scale_set_delivery_state::{
    ScaleSetDeliveryRecoveryPhase, ScaleSetDeliveryRecoveryState,
};

/// Derive the only catalog that may remain after one conclusively acknowledged delivery settles.
///
/// Positively acquired requests retain their exact attempt. A request that was only Available and
/// is conclusively unacquired is completed without VM-cleanup authority and moved to bounded replay
/// history. Later lifecycle evidence wins over an empty acknowledgement response: an Assigned,
/// Running, Terminal, or already-complete attempt is retained rather than discarded.
pub(crate) fn settle_scale_set_delivery_catalog(
    recovery: &ScaleSetDeliveryRecoveryState,
    catalog: &DisposableAttemptCatalogDocument,
) -> Result<DisposableAttemptCatalogDocument, ScaleSetDeliverySettlementError> {
    if !recovery.matches_catalog(catalog) {
        return Err(settlement_error("scale_set_settlement_catalog_drift"));
    }
    let acquired = match recovery.phase() {
        ScaleSetDeliveryRecoveryPhase::Acknowledged { acquired } => acquired,
        ScaleSetDeliveryRecoveryPhase::AcquisitionRecoveryObserved { acquired }
            if !acquired.is_empty() =>
        {
            acquired
        }
        ScaleSetDeliveryRecoveryPhase::LifecycleAcknowledged { resolution } => {
            resolution.acquired()
        }
        ScaleSetDeliveryRecoveryPhase::SettlementPrepared { acquired, .. } => acquired,
        _ => return Err(settlement_error("scale_set_settlement_evidence_incomplete")),
    };
    let acquired = acquired.iter().copied().collect::<BTreeSet<_>>();
    let available = recovery
        .delivery()
        .available_request_ids()
        .map_err(|_| settlement_error("scale_set_settlement_delivery_invalid"))?;

    let mut next = catalog.clone();
    for request_id in available {
        if acquired.contains(&request_id) {
            if next.find_active_by_runner_request_id(request_id).is_none()
                && next
                    .find_tombstone_by_runner_request_id(request_id)
                    .is_none()
            {
                return Err(settlement_error("scale_set_settlement_attempt_missing"));
            }
            continue;
        }

        let Some(reservation) = next.find_active_by_runner_request_id(request_id) else {
            if next
                .find_tombstone_by_runner_request_id(request_id)
                .is_some()
            {
                continue;
            }
            return Err(settlement_error("scale_set_settlement_attempt_missing"));
        };
        if reservation.attempt().phase() != DisposableAttemptPhase::Reserved {
            if reservation.attempt().github_job_id().is_some()
                || reservation.attempt().phase() == DisposableAttemptPhase::Complete
            {
                continue;
            }
            return Err(settlement_error("scale_set_settlement_attempt_conflict"));
        }

        let attempt_id = reservation.attempt().attempt_id().clone();
        let releasing = next
            .replace_attempt(
                &attempt_id,
                reservation.attempt().revision(),
                DisposableAttemptCatalogAction::BeginUnprovisionedRelease,
            )
            .map_err(|_| settlement_error("scale_set_settlement_attempt_conflict"))?;
        let releasing_attempt = releasing
            .find_active_by_runner_request_id(request_id)
            .ok_or_else(|| settlement_error("scale_set_settlement_attempt_missing"))?;
        let complete = releasing
            .replace_attempt(
                &attempt_id,
                releasing_attempt.attempt().revision(),
                DisposableAttemptCatalogAction::CompleteUnprovisioned,
            )
            .map_err(|_| settlement_error("scale_set_settlement_attempt_conflict"))?;
        let complete_attempt = complete
            .find_active_by_runner_request_id(request_id)
            .ok_or_else(|| settlement_error("scale_set_settlement_attempt_missing"))?;
        next = complete
            .retire_complete(&attempt_id, complete_attempt.attempt().revision())
            .map_err(|_| settlement_error("scale_set_settlement_attempt_conflict"))?;
    }
    Ok(next)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScaleSetDeliverySettlementError {
    code: &'static str,
}

impl ScaleSetDeliverySettlementError {
    pub(crate) const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for ScaleSetDeliverySettlementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScaleSetDeliverySettlementError")
            .field("code", &self.code)
            .finish()
    }
}

impl fmt::Display for ScaleSetDeliverySettlementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code)
    }
}

impl std::error::Error for ScaleSetDeliverySettlementError {}

const fn settlement_error(code: &'static str) -> ScaleSetDeliverySettlementError {
    ScaleSetDeliverySettlementError { code }
}
