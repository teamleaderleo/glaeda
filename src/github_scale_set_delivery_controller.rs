#![allow(dead_code)]

use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;

use crate::disposable_attempt_catalog::DisposableAttemptCatalog;
use crate::execution_admission::EpochMillis;
use crate::github_scale_set_bridge::{
    ScaleSetBridgeClient, ScaleSetBridgeError, ScaleSetBridgePoll,
};
use crate::github_scale_set_delivery::ScaleSetDelivery;
use crate::github_scale_set_delivery_consumer::ScaleSetDeliveryConsumerPolicy;
use crate::github_scale_set_delivery_state::{
    ScaleSetDeliveryRecoveryPhase, ScaleSetDeliveryRecoveryState,
};
use crate::github_scale_set_protocol::ScaleSetRunnerRequestId;
use crate::personal_worker_store::PersonalWorkerStoreError;
use crate::unix_personal_worker_store::UnixPersonalWorkerStore;
use crate::unix_personal_worker_store::scale_set_delivery_recovery::ScaleSetExternalTransaction;

/// Narrow bridge surface consumed by the durable controller.
///
/// The production implementation delegates protocol semantics to the pinned official-client
/// bridge. The trait remains private so callers cannot substitute external mutation authority.
trait DeliveryBridge {
    fn poll(&mut self) -> Result<ScaleSetBridgePoll, ScaleSetBridgeError>;
    fn ack(&mut self, message_id: u32) -> Result<Vec<u64>, ScaleSetBridgeError>;
    fn acquire(
        &mut self,
        request_ids: &[ScaleSetRunnerRequestId],
    ) -> Result<Vec<ScaleSetRunnerRequestId>, ScaleSetBridgeError>;
    fn poison(&mut self);
}

impl DeliveryBridge for ScaleSetBridgeClient {
    fn poll(&mut self) -> Result<ScaleSetBridgePoll, ScaleSetBridgeError> {
        self.poll()
    }

    fn ack(&mut self, message_id: u32) -> Result<Vec<u64>, ScaleSetBridgeError> {
        self.ack(message_id)
    }

    fn acquire(
        &mut self,
        request_ids: &[ScaleSetRunnerRequestId],
    ) -> Result<Vec<ScaleSetRunnerRequestId>, ScaleSetBridgeError> {
        self.acquire(request_ids)
    }

    fn poison(&mut self) {
        self.poison();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScaleSetDeliveryControllerDisposition {
    Idle,
    Acknowledged { acquired: usize },
    AcquisitionRecoveryObserved { acquired: usize },
    RecoveryRequired,
}

/// Advance at most one exact Scale Set delivery across its external acknowledgement boundary.
///
/// A new message is reconciled and published before acknowledgement begins. A durable
/// `AcknowledgementStarted` phase is never replayed through `ack`; a fresh bridge may only invoke
/// standalone acquisition for the exact retained available request IDs. Empty acquisition replay
/// stays explicit recovery debt for later service lifecycle evidence.
pub(crate) fn consume_scale_set_delivery_once(
    root_path: &Path,
    policy: &ScaleSetDeliveryConsumerPolicy,
    bridge: &mut ScaleSetBridgeClient,
    observed_at: EpochMillis,
) -> Result<ScaleSetDeliveryControllerDisposition, ScaleSetDeliveryControllerError> {
    consume_with_bridge(root_path, policy, bridge, observed_at)
}

fn consume_with_bridge<B: DeliveryBridge>(
    root_path: &Path,
    policy: &ScaleSetDeliveryConsumerPolicy,
    bridge: &mut B,
    observed_at: EpochMillis,
) -> Result<ScaleSetDeliveryControllerDisposition, ScaleSetDeliveryControllerError> {
    let mut recovery_store =
        UnixPersonalWorkerStore::open_or_create_scale_set_delivery_recovery(root_path)
            .map_err(map_store_error)?;
    if let Some(current) = recovery_store
        .load_scale_set_delivery_recovery()
        .map_err(map_store_error)?
    {
        return resume_delivery(
            root_path,
            policy,
            bridge,
            observed_at,
            &mut recovery_store,
            current,
        );
    }
    drop(recovery_store);

    let poll = bridge.poll().map_err(map_bridge_error)?;
    let delivery = match ScaleSetDelivery::from_bridge_poll(&poll) {
        Ok(Some(delivery)) => delivery,
        Ok(None) => return Ok(ScaleSetDeliveryControllerDisposition::Idle),
        Err(_) => {
            bridge.poison();
            return Err(controller_error("scale_set_delivery_invalid"));
        }
    };
    let publication = (|| {
        let catalog_store = UnixPersonalWorkerStore::open_or_create_disposable_catalog(root_path)
            .map_err(|_| controller_error("scale_set_catalog_unavailable"))?;
        let catalog = DisposableAttemptCatalog::new(catalog_store)
            .load()
            .map_err(|_| controller_error("scale_set_catalog_unavailable"))?;
        let mut paired =
            UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root_path)
                .map_err(map_store_error)?;
        let result = paired
            .publish_scale_set_reconciled_delivery(
                catalog.revision(),
                policy,
                &delivery,
                observed_at,
            )
            .map_err(map_store_error)?;
        Ok::<_, ScaleSetDeliveryControllerError>((paired, result.1))
    })();
    let (paired, recovery) = match publication {
        Ok(value) => value,
        Err(error) => {
            bridge.poison();
            return Err(error);
        }
    };
    drop(paired);

    let mut recovery_store =
        UnixPersonalWorkerStore::open_or_create_scale_set_delivery_recovery(root_path)
            .map_err(map_store_error)?;
    acknowledge_delivery(&mut recovery_store, bridge, recovery)
}

fn resume_delivery<B: DeliveryBridge>(
    root_path: &Path,
    policy: &ScaleSetDeliveryConsumerPolicy,
    bridge: &mut B,
    observed_at: EpochMillis,
    recovery_store: &mut UnixPersonalWorkerStore,
    current: ScaleSetDeliveryRecoveryState,
) -> Result<ScaleSetDeliveryControllerDisposition, ScaleSetDeliveryControllerError> {
    match current.phase() {
        ScaleSetDeliveryRecoveryPhase::Reconciled => {
            let poll = bridge.poll().map_err(map_bridge_error)?;
            let observed = match ScaleSetDelivery::from_bridge_poll(&poll) {
                Ok(Some(delivery)) => delivery,
                Ok(None) => {
                    bridge.poison();
                    return Err(controller_error("scale_set_delivery_recovery_required"));
                }
                Err(_) => {
                    bridge.poison();
                    return Err(controller_error("scale_set_delivery_invalid"));
                }
            };
            if &observed != current.delivery() {
                bridge.poison();
                return Err(controller_error("scale_set_delivery_recovery_conflict"));
            }
            let mut paired =
                UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root_path)
                    .map_err(|error| {
                        bridge.poison();
                        map_store_error(error)
                    })?;
            if let Err(error) = paired.publish_scale_set_reconciled_delivery(
                current.prior_catalog_revision(),
                policy,
                &observed,
                observed_at,
            ) {
                bridge.poison();
                return Err(map_store_error(error));
            }
            drop(paired);
            acknowledge_delivery(recovery_store, bridge, current)
        }
        ScaleSetDeliveryRecoveryPhase::AcknowledgementStarted => {
            recover_acquisition(recovery_store, bridge, current)
        }
        ScaleSetDeliveryRecoveryPhase::AcquisitionRecoveryObserved { acquired }
            if acquired.is_empty() =>
        {
            recover_acquisition(recovery_store, bridge, current)
        }
        ScaleSetDeliveryRecoveryPhase::AcquisitionRecoveryObserved { acquired } => Ok(
            ScaleSetDeliveryControllerDisposition::AcquisitionRecoveryObserved {
                acquired: acquired.len(),
            },
        ),
        ScaleSetDeliveryRecoveryPhase::Acknowledged { acquired } => {
            Ok(ScaleSetDeliveryControllerDisposition::Acknowledged {
                acquired: acquired.len(),
            })
        }
    }
}

fn acknowledge_delivery<B: DeliveryBridge>(
    store: &mut UnixPersonalWorkerStore,
    bridge: &mut B,
    current: ScaleSetDeliveryRecoveryState,
) -> Result<ScaleSetDeliveryControllerDisposition, ScaleSetDeliveryControllerError> {
    let available = current
        .delivery()
        .available_request_ids()
        .map_err(|_| controller_error("scale_set_delivery_invalid"))?
        .into_iter()
        .collect::<BTreeSet<_>>();
    let transaction = store
        .acknowledge_scale_set_delivery_locked(&current, |message_id| {
            let acquired = bridge
                .ack(message_id)
                .map_err(map_bridge_error)?
                .into_iter()
                .map(|value| {
                    ScaleSetRunnerRequestId::new(value)
                        .map_err(|_| controller_error("scale_set_ack_response_invalid"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            if acquired
                .iter()
                .any(|request_id| !available.contains(request_id))
            {
                bridge.poison();
                return Err(controller_error("scale_set_ack_response_invalid"));
            }
            Ok(acquired)
        })
        .map_err(map_store_error)?;
    let acknowledged = match transaction {
        ScaleSetExternalTransaction::Completed(state) => state,
        ScaleSetExternalTransaction::ExternalFailed(error) => return Err(error),
    };
    let ScaleSetDeliveryRecoveryPhase::Acknowledged { acquired } = acknowledged.phase() else {
        return Err(controller_error("scale_set_delivery_recovery_conflict"));
    };
    Ok(ScaleSetDeliveryControllerDisposition::Acknowledged {
        acquired: acquired.len(),
    })
}

fn recover_acquisition<B: DeliveryBridge>(
    store: &mut UnixPersonalWorkerStore,
    bridge: &mut B,
    current: ScaleSetDeliveryRecoveryState,
) -> Result<ScaleSetDeliveryControllerDisposition, ScaleSetDeliveryControllerError> {
    if current
        .delivery()
        .available_request_ids()
        .map_err(|_| controller_error("scale_set_delivery_invalid"))?
        .is_empty()
    {
        return Ok(ScaleSetDeliveryControllerDisposition::RecoveryRequired);
    }
    let transaction = store
        .recover_scale_set_acquisition_locked(&current, |available| {
            bridge.acquire(available).map_err(map_bridge_error)
        })
        .map_err(map_store_error)?;
    let successor = match transaction {
        ScaleSetExternalTransaction::Completed(state) => state,
        ScaleSetExternalTransaction::ExternalFailed(error) => return Err(error),
    };
    let ScaleSetDeliveryRecoveryPhase::AcquisitionRecoveryObserved { acquired } = successor.phase()
    else {
        return Err(controller_error("scale_set_delivery_recovery_conflict"));
    };
    if acquired.is_empty() {
        Ok(ScaleSetDeliveryControllerDisposition::RecoveryRequired)
    } else {
        Ok(
            ScaleSetDeliveryControllerDisposition::AcquisitionRecoveryObserved {
                acquired: acquired.len(),
            },
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScaleSetDeliveryControllerError {
    code: &'static str,
}

impl ScaleSetDeliveryControllerError {
    pub(crate) const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for ScaleSetDeliveryControllerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScaleSetDeliveryControllerError")
            .field("code", &self.code)
            .finish()
    }
}

impl fmt::Display for ScaleSetDeliveryControllerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code)
    }
}

impl std::error::Error for ScaleSetDeliveryControllerError {}

const fn controller_error(code: &'static str) -> ScaleSetDeliveryControllerError {
    ScaleSetDeliveryControllerError { code }
}

fn map_bridge_error(_: ScaleSetBridgeError) -> ScaleSetDeliveryControllerError {
    controller_error("scale_set_bridge_failed")
}

fn map_store_error(_: PersonalWorkerStoreError) -> ScaleSetDeliveryControllerError {
    controller_error("scale_set_delivery_store_failed")
}

#[cfg(test)]
mod tests;
