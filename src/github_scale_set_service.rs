// The coordinator is private until guest JIT handoff and the supervised worker command are wired.
#![allow(dead_code)]

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::disposable_attempt_catalog::DisposableAttemptCatalogDocument;
use crate::disposable_clone_runtime::{
    DisposableCloneAdmissionBinding, DisposableCloneAdmissionObservation,
    DisposableCloneAdmissionSource, DisposableCloneRuntime, DisposableCloneRuntimeError,
    DisposableCloneRuntimeReceipt, admission_seal,
};
use crate::disposable_worker_reconciler::{DisposableAttemptId, DisposableAttemptPhase};
use crate::execution_admission::EpochMillis;
use crate::github_scale_set_bridge::{
    ScaleSetBridgeClient, ScaleSetBridgeError, ScaleSetBridgeIdentity, ScaleSetBridgePoll,
    ScaleSetStatistics,
};
use crate::github_scale_set_consumer::{
    ScaleSetConsumerError, ScaleSetConsumerPolicy, apply_scale_set_ack_outcome,
    apply_scale_set_event,
};
use crate::github_scale_set_inbox::ScaleSetInboxError;
use crate::unix_personal_worker_store::UnixPersonalWorkerStore;

const MESSAGE_FRESHNESS_MILLIS: u64 = 30_000;

pub(crate) trait ScaleSetBridgeSession {
    fn poll(&mut self, available_capacity: u16) -> Result<ScaleSetBridgePoll, ScaleSetBridgeError>;
    fn ack(&mut self, message_id: u32) -> Result<Vec<u64>, ScaleSetBridgeError>;
}

impl ScaleSetBridgeSession for ScaleSetBridgeClient {
    fn poll(&mut self, available_capacity: u16) -> Result<ScaleSetBridgePoll, ScaleSetBridgeError> {
        ScaleSetBridgeClient::poll(self, available_capacity)
    }

    fn ack(&mut self, message_id: u32) -> Result<Vec<u64>, ScaleSetBridgeError> {
        ScaleSetBridgeClient::ack(self, message_id)
    }
}

pub(crate) trait ScaleSetServiceClock {
    fn now(&self) -> Result<EpochMillis, ScaleSetServiceError>;
}

struct SystemScaleSetServiceClock;

impl ScaleSetServiceClock for SystemScaleSetServiceClock {
    fn now(&self) -> Result<EpochMillis, ScaleSetServiceError> {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| ScaleSetServiceError::new("scale_set_clock_unavailable"))?
            .as_millis();
        let millis = u64::try_from(millis)
            .map_err(|_| ScaleSetServiceError::new("scale_set_clock_unavailable"))?;
        EpochMillis::new(millis)
            .map_err(|_| ScaleSetServiceError::new("scale_set_clock_unavailable"))
    }
}

struct DurableScaleSetCloneAdmission {
    binding: DisposableCloneAdmissionBinding,
}

impl admission_seal::Sealed for DurableScaleSetCloneAdmission {
    fn scale_set_admission_binding(&self) -> Option<&DisposableCloneAdmissionBinding> {
        Some(&self.binding)
    }
}

impl DisposableCloneAdmissionSource for DurableScaleSetCloneAdmission {
    fn observe(
        &self,
        catalog: &DisposableAttemptCatalogDocument,
        reservation: &crate::disposable_attempt_catalog::DisposableAttemptReservation,
    ) -> Result<DisposableCloneAdmissionObservation, DisposableCloneRuntimeError> {
        if !matches!(
            reservation.attempt().phase(),
            DisposableAttemptPhase::Reserved | DisposableAttemptPhase::CloneAuthorized
        ) || reservation.attempt().github_job_id().is_none()
            || reservation.attempt().result().is_some()
        {
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_scale_set_admission_invalid",
            ));
        }
        Ok(DisposableCloneAdmissionObservation::new(
            catalog,
            reservation,
            self.binding.observed_at(),
            self.binding.expires_at(),
            true,
            false,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScaleSetServiceDisposition {
    Idle(ScaleSetStatistics),
    CloneAdmissionRefreshed { attempt_id: String },
    MessagePersisted { message_id: u32 },
    EventApplied { message_id: u32, event_index: usize },
    MessageAcknowledged { message_id: u32 },
    AckOutcomeApplied { message_id: u32 },
    UnprovisionedReleased { attempt_id: String },
    AttemptRetired { attempt_id: String },
}

#[derive(Debug)]
pub(crate) enum ScaleSetWorkerDisposition {
    NoCandidate,
    CloneAuthorized { attempt_id: String },
    CloneCompleted(DisposableCloneRuntimeReceipt),
}

pub(crate) struct ScaleSetService<B, C> {
    store: UnixPersonalWorkerStore,
    bridge: B,
    policy: ScaleSetConsumerPolicy,
    source_identity: ScaleSetBridgeIdentity,
    clock: C,
}

impl ScaleSetService<ScaleSetBridgeClient, SystemScaleSetServiceClock> {
    pub(crate) fn new(
        store: UnixPersonalWorkerStore,
        bridge: ScaleSetBridgeClient,
        policy: ScaleSetConsumerPolicy,
    ) -> Result<Self, ScaleSetServiceError> {
        Self::with_parts(store, bridge, policy, SystemScaleSetServiceClock)
    }
}

impl<B: ScaleSetBridgeSession, C: ScaleSetServiceClock> ScaleSetService<B, C> {
    fn with_parts(
        mut store: UnixPersonalWorkerStore,
        bridge: B,
        policy: ScaleSetConsumerPolicy,
        clock: C,
    ) -> Result<Self, ScaleSetServiceError> {
        let source_identity = policy.source_identity().clone();
        store
            .initialize_scale_set_inbox(&source_identity)
            .map_err(ScaleSetServiceError::from_inbox)?;
        Ok(Self {
            store,
            bridge,
            policy,
            source_identity,
            clock,
        })
    }

    pub(crate) fn reconcile_once(
        &mut self,
    ) -> Result<ScaleSetServiceDisposition, ScaleSetServiceError> {
        let (inbox, catalog) = self
            .store
            .load_scale_set_control_state(&self.source_identity)
            .map_err(ScaleSetServiceError::from_inbox)?;

        if let Some(pending) = inbox.pending() {
            if pending.ack_started() {
                return Err(ScaleSetServiceError::new("scale_set_ack_outcome_unknown"));
            }
            if pending.next_event().is_some() {
                let message_id = pending.message_id();
                let event_index = pending.next_event_index();
                let policy = &self.policy;
                let clock = &self.clock;
                self.store
                    .apply_next_scale_set_event(
                        &self.source_identity,
                        inbox.revision(),
                        |pending, event, catalog| {
                            let now = clock.now()?;
                            apply_scale_set_event(policy, pending, event, catalog, now)
                                .map(|next| (next, ()))
                                .map_err(ScaleSetServiceError::from_consumer)
                        },
                    )
                    .map_err(ScaleSetServiceError::from_inbox)??;
                return Ok(ScaleSetServiceDisposition::EventApplied {
                    message_id,
                    event_index,
                });
            }

            let message_id = pending.message_id();
            let bridge = &mut self.bridge;
            self.store
                .acknowledge_scale_set_message(
                    &self.source_identity,
                    inbox.revision(),
                    |message_id| {
                        bridge
                            .ack(message_id)
                            .map_err(ScaleSetServiceError::from_bridge)
                    },
                )
                .map_err(ScaleSetServiceError::from_inbox)??;
            return Ok(ScaleSetServiceDisposition::MessageAcknowledged { message_id });
        }

        if let Some(receipt) = inbox
            .last_ack()
            .filter(|receipt| !receipt.outcome_applied())
        {
            let message_id = receipt.message_id();
            let policy = &self.policy;
            self.store
                .apply_scale_set_ack_outcome(
                    &self.source_identity,
                    inbox.revision(),
                    |receipt, catalog| {
                        apply_scale_set_ack_outcome(policy, receipt, catalog)
                            .map(|next| (next, ()))
                            .map_err(ScaleSetServiceError::from_consumer)
                    },
                )
                .map_err(ScaleSetServiceError::from_inbox)??;
            return Ok(ScaleSetServiceDisposition::AckOutcomeApplied { message_id });
        }

        let mut releasing = catalog.active().iter().filter(|reservation| {
            reservation.attempt().phase() == DisposableAttemptPhase::UnprovisionedReleasing
        });
        if let Some(reservation) = releasing.next() {
            if releasing.next().is_some() {
                return Err(ScaleSetServiceError::new(
                    "scale_set_capacity_invariant_violated",
                ));
            }
            let attempt_id = reservation.attempt().attempt_id().clone();
            self.store
                .complete_scale_set_unprovisioned_attempt(&self.source_identity, &attempt_id)
                .map_err(ScaleSetServiceError::from_inbox)?;
            return Ok(ScaleSetServiceDisposition::UnprovisionedReleased {
                attempt_id: attempt_id.as_str().to_owned(),
            });
        }

        if let Some(reservation) = catalog
            .active()
            .iter()
            .find(|reservation| reservation.attempt().phase() == DisposableAttemptPhase::Complete)
        {
            let attempt_id = reservation.attempt().attempt_id().clone();
            self.store
                .retire_scale_set_complete_attempt(&self.source_identity, &attempt_id)
                .map_err(ScaleSetServiceError::from_inbox)?;
            return Ok(ScaleSetServiceDisposition::AttemptRetired {
                attempt_id: attempt_id.as_str().to_owned(),
            });
        }

        let available_capacity = available_capacity(&catalog)?;
        match self
            .bridge
            .poll(available_capacity)
            .map_err(ScaleSetServiceError::from_bridge)?
        {
            ScaleSetBridgePoll::Idle { statistics } => {
                let Some(reservation) = catalog.active().iter().find(|reservation| {
                    reservation.attempt().github_job_id().is_some()
                        && matches!(
                            reservation.attempt().phase(),
                            DisposableAttemptPhase::Reserved
                                | DisposableAttemptPhase::CloneAuthorized
                        )
                }) else {
                    return Ok(ScaleSetServiceDisposition::Idle(statistics));
                };
                let observed_at = self.clock.now()?;
                let not_after = observed_at
                    .get()
                    .checked_add(MESSAGE_FRESHNESS_MILLIS)
                    .and_then(|value| EpochMillis::new(value).ok())
                    .ok_or_else(|| ScaleSetServiceError::new("scale_set_clock_unavailable"))?;
                let attempt_id = reservation.attempt().attempt_id().clone();
                self.store
                    .record_scale_set_idle(
                        &self.source_identity,
                        inbox.revision(),
                        &attempt_id,
                        observed_at,
                        not_after,
                    )
                    .map_err(ScaleSetServiceError::from_inbox)?;
                Ok(ScaleSetServiceDisposition::CloneAdmissionRefreshed {
                    attempt_id: attempt_id.as_str().to_owned(),
                })
            }
            ScaleSetBridgePoll::Message {
                message_id,
                statistics: _,
                events,
            } => {
                let observed_at = self.clock.now()?;
                let not_after = observed_at
                    .get()
                    .checked_add(MESSAGE_FRESHNESS_MILLIS)
                    .and_then(|value| EpochMillis::new(value).ok())
                    .ok_or_else(|| ScaleSetServiceError::new("scale_set_clock_unavailable"))?;
                self.store
                    .record_scale_set_message(
                        &self.source_identity,
                        inbox.revision(),
                        message_id,
                        observed_at,
                        not_after,
                        events,
                    )
                    .map_err(ScaleSetServiceError::from_inbox)?;
                Ok(ScaleSetServiceDisposition::MessagePersisted { message_id })
            }
        }
    }

    /// Advance at most one acquired worker mutation after a fresh zero-capacity idle poll.
    pub(crate) fn progress_worker_once(
        &mut self,
        runtime: &DisposableCloneRuntime,
    ) -> Result<ScaleSetWorkerDisposition, ScaleSetServiceError> {
        let (inbox, catalog) = self
            .store
            .load_scale_set_control_state(&self.source_identity)
            .map_err(ScaleSetServiceError::from_inbox)?;
        if inbox.requires_reconciliation() {
            return Err(ScaleSetServiceError::new(
                "scale_set_reconciliation_required",
            ));
        }
        let mut candidates = catalog.active().iter().filter(|reservation| {
            matches!(
                reservation.attempt().phase(),
                DisposableAttemptPhase::Reserved | DisposableAttemptPhase::CloneAuthorized
            )
        });
        let Some(candidate) = candidates.next() else {
            return Ok(ScaleSetWorkerDisposition::NoCandidate);
        };
        if candidates.next().is_some() {
            return Err(ScaleSetServiceError::new(
                "scale_set_capacity_invariant_violated",
            ));
        }
        let attempt_id = candidate.attempt().attempt_id().clone();
        match candidate.attempt().phase() {
            DisposableAttemptPhase::Reserved => {
                self.authorize_reserved_attempt(runtime, &attempt_id)?;
                Ok(ScaleSetWorkerDisposition::CloneAuthorized {
                    attempt_id: attempt_id.as_str().to_owned(),
                })
            }
            DisposableAttemptPhase::CloneAuthorized => {
                let receipt = self.clone_authorized_attempt(runtime, &attempt_id)?;
                Ok(ScaleSetWorkerDisposition::CloneCompleted(receipt))
            }
            _ => unreachable!("candidate phase was filtered"),
        }
    }

    /// Persist one reserved attempt's exact target-absence checkpoint.
    pub(crate) fn authorize_reserved_attempt(
        &mut self,
        runtime: &DisposableCloneRuntime,
        attempt_id: &DisposableAttemptId,
    ) -> Result<(), ScaleSetServiceError> {
        let admission = self.clone_admission(attempt_id)?;
        runtime
            .authorize_once(attempt_id, &admission)
            .map_err(ScaleSetServiceError::from_clone)
    }

    /// Execute one already-authorized clone against the exact settled durable Scale Set source.
    ///
    /// The clone transaction reopens the inbox and catalog under their canonical lock. A pending
    /// event, unresolved acknowledgement, source-identity mismatch, job-less reservation, or
    /// cancellation/release phase refuses before the clone-start checkpoint.
    pub(crate) fn clone_authorized_attempt(
        &mut self,
        runtime: &DisposableCloneRuntime,
        attempt_id: &DisposableAttemptId,
    ) -> Result<DisposableCloneRuntimeReceipt, ScaleSetServiceError> {
        let admission = self.clone_admission(attempt_id)?;
        runtime
            .clone_once(attempt_id, &admission)
            .map_err(ScaleSetServiceError::from_clone)
    }

    fn clone_admission(
        &mut self,
        attempt_id: &DisposableAttemptId,
    ) -> Result<DurableScaleSetCloneAdmission, ScaleSetServiceError> {
        let (inbox, catalog) = self
            .store
            .load_scale_set_control_state(&self.source_identity)
            .map_err(ScaleSetServiceError::from_inbox)?;
        if inbox.requires_reconciliation() {
            return Err(ScaleSetServiceError::new(
                "scale_set_reconciliation_required",
            ));
        }
        let idle = inbox
            .last_idle()
            .ok_or_else(|| ScaleSetServiceError::new("scale_set_clone_admission_missing"))?;
        let reservation = catalog
            .find_active(attempt_id)
            .ok_or_else(|| ScaleSetServiceError::new("scale_set_clone_attempt_missing"))?;
        if idle.catalog_revision() != catalog.revision()
            || idle.attempt_id() != reservation.attempt().attempt_id()
            || idle.attempt_revision() != reservation.attempt().revision()
            || idle.capacity_claim_id() != reservation.attempt().capacity_claim_id()
        {
            return Err(ScaleSetServiceError::new("scale_set_clone_admission_stale"));
        }
        Ok(DurableScaleSetCloneAdmission {
            binding: DisposableCloneAdmissionBinding::new(&self.source_identity, idle),
        })
    }

    #[cfg(test)]
    fn into_parts(self) -> (UnixPersonalWorkerStore, B) {
        (self.store, self.bridge)
    }
}

fn available_capacity(
    catalog: &DisposableAttemptCatalogDocument,
) -> Result<u16, ScaleSetServiceError> {
    let usage = catalog
        .host_usage()
        .map_err(|_| ScaleSetServiceError::new("scale_set_capacity_invalid"))?;
    Ok(u16::from(usage.workers() == 0))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScaleSetServiceError {
    code: &'static str,
}

impl ScaleSetServiceError {
    const fn new(code: &'static str) -> Self {
        Self { code }
    }

    const fn from_bridge(error: ScaleSetBridgeError) -> Self {
        Self::new(error.code())
    }

    fn from_inbox(error: ScaleSetInboxError) -> Self {
        Self::new(error.code())
    }

    fn from_consumer(error: ScaleSetConsumerError) -> Self {
        Self::new(error.code())
    }

    fn from_clone(error: DisposableCloneRuntimeError) -> Self {
        Self::new(error.code())
    }

    pub(crate) const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for ScaleSetServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScaleSetServiceError")
            .field("code", &self.code)
            .finish()
    }
}

impl fmt::Display for ScaleSetServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code)
    }
}

impl std::error::Error for ScaleSetServiceError {}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::disposable_attempt_catalog::{
        DisposableAttemptCatalog, DisposableAttemptCatalogStore,
    };
    use crate::disposable_prepared_template::current_disposable_prepared_template;
    use crate::disposable_worker_reconciler::{DisposableAttemptPhase, DisposableWorkerResources};
    use crate::github_scale_set_bridge::{ScaleSetBridgeEvent, ScaleSetBridgeJobEvidence};
    use crate::github_scale_set_protocol::ScaleSetJobId;

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new() -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "smolrunner-scale-set-service-{}-{sequence}",
                std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o750)).unwrap();
            Self(path)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct FixedClock(EpochMillis);

    impl ScaleSetServiceClock for FixedClock {
        fn now(&self) -> Result<EpochMillis, ScaleSetServiceError> {
            Ok(self.0)
        }
    }

    struct FakeBridge {
        polls: VecDeque<ScaleSetBridgePoll>,
        acquired: VecDeque<Vec<u64>>,
        capacities: Vec<u16>,
        acknowledgements: Vec<u32>,
    }

    impl ScaleSetBridgeSession for FakeBridge {
        fn poll(
            &mut self,
            available_capacity: u16,
        ) -> Result<ScaleSetBridgePoll, ScaleSetBridgeError> {
            self.capacities.push(available_capacity);
            Ok(self.polls.pop_front().expect("expected fake poll"))
        }

        fn ack(&mut self, message_id: u32) -> Result<Vec<u64>, ScaleSetBridgeError> {
            self.acknowledgements.push(message_id);
            Ok(self.acquired.pop_front().unwrap_or_default())
        }
    }

    fn statistics() -> ScaleSetStatistics {
        ScaleSetStatistics {
            available_jobs: 1,
            acquired_jobs: 0,
            assigned_jobs: 0,
            running_jobs: 0,
            registered_runners: 0,
            busy_runners: 0,
            idle_runners: 0,
        }
    }

    fn source_identity() -> ScaleSetBridgeIdentity {
        ScaleSetBridgeIdentity::parse(&format!("sha256:{}", "55".repeat(32))).unwrap()
    }

    fn event() -> ScaleSetBridgeEvent {
        ScaleSetBridgeEvent::Available(ScaleSetBridgeJobEvidence {
            runner_request_id: 41,
            repository: "project".to_owned(),
            owner: "example".to_owned(),
            job_id: ScaleSetJobId::parse("job-1").unwrap(),
            workflow_run_id: 99,
            request_labels: vec!["smolrunner".to_owned()],
        })
    }

    fn assigned_event() -> ScaleSetBridgeEvent {
        let ScaleSetBridgeEvent::Available(job) = event() else {
            unreachable!();
        };
        ScaleSetBridgeEvent::Assigned(job)
    }

    fn canceled_event() -> ScaleSetBridgeEvent {
        let ScaleSetBridgeEvent::Available(job) = event() else {
            unreachable!();
        };
        ScaleSetBridgeEvent::Completed {
            job,
            runner: None,
            result: crate::github_scale_set_protocol::ScaleSetJobResult::parse("canceled").unwrap(),
        }
    }

    #[test]
    fn one_tick_at_a_time_persists_applies_acks_and_reconciles_capacity() {
        let root = TempRoot::new();
        let store = UnixPersonalWorkerStore::open_or_create_disposable_catalog(&root.0).unwrap();
        let mut catalog = DisposableAttemptCatalog::new(store);
        catalog.initialize().unwrap();
        let store = catalog.into_store();
        let policy = ScaleSetConsumerPolicy::new(
            source_identity(),
            23,
            "project",
            "example",
            &["smolrunner".to_owned()],
            DisposableWorkerResources::new(2_000, 2 << 30, 20 << 30).unwrap(),
            &current_disposable_prepared_template().unwrap(),
        )
        .unwrap();
        let bridge = FakeBridge {
            polls: VecDeque::from([
                ScaleSetBridgePoll::Message {
                    message_id: 7,
                    statistics: statistics(),
                    events: vec![event()],
                },
                ScaleSetBridgePoll::Idle {
                    statistics: statistics(),
                },
            ]),
            acquired: VecDeque::from([Vec::new()]),
            capacities: Vec::new(),
            acknowledgements: Vec::new(),
        };
        let mut service = ScaleSetService::with_parts(
            store,
            bridge,
            policy,
            FixedClock(EpochMillis::new(100_000).unwrap()),
        )
        .unwrap();

        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::MessagePersisted { message_id: 7 }
        ));
        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::EventApplied {
                message_id: 7,
                event_index: 0
            }
        ));
        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::MessageAcknowledged { message_id: 7 }
        ));
        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::AckOutcomeApplied { message_id: 7 }
        ));
        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::UnprovisionedReleased { .. }
        ));
        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::AttemptRetired { .. }
        ));
        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::Idle(_)
        ));

        let (mut store, bridge) = service.into_parts();
        let catalog = DisposableAttemptCatalogStore::load(&store)
            .unwrap()
            .unwrap();
        assert!(catalog.active().is_empty());
        assert_eq!(catalog.tombstones().len(), 1);
        assert_eq!(bridge.capacities, [1, 1]);
        assert_eq!(bridge.acknowledgements, [7]);
        assert!(DisposableAttemptCatalogStore::recover(&mut store).is_ok());
    }

    #[test]
    fn acquired_reservation_requires_a_durable_zero_capacity_idle_poll_before_clone() {
        let root = TempRoot::new();
        let store = UnixPersonalWorkerStore::open_or_create_disposable_catalog(&root.0).unwrap();
        let mut catalog = DisposableAttemptCatalog::new(store);
        catalog.initialize().unwrap();
        let store = catalog.into_store();
        let policy = ScaleSetConsumerPolicy::new(
            source_identity(),
            23,
            "project",
            "example",
            &["smolrunner".to_owned()],
            DisposableWorkerResources::new(2_000, 2 << 30, 20 << 30).unwrap(),
            &current_disposable_prepared_template().unwrap(),
        )
        .unwrap();
        let bridge = FakeBridge {
            polls: VecDeque::from([
                ScaleSetBridgePoll::Message {
                    message_id: 7,
                    statistics: statistics(),
                    events: vec![event()],
                },
                ScaleSetBridgePoll::Idle {
                    statistics: statistics(),
                },
            ]),
            acquired: VecDeque::from([vec![41]]),
            capacities: Vec::new(),
            acknowledgements: Vec::new(),
        };
        let mut service = ScaleSetService::with_parts(
            store,
            bridge,
            policy,
            FixedClock(EpochMillis::new(100_000).unwrap()),
        )
        .unwrap();

        for _ in 0..4 {
            service.reconcile_once().unwrap();
        }
        let attempt_id = match service.reconcile_once().unwrap() {
            ScaleSetServiceDisposition::CloneAdmissionRefreshed { attempt_id } => {
                DisposableAttemptId::parse(&attempt_id).unwrap()
            }
            other => panic!("unexpected disposition: {other:?}"),
        };
        let admission = service.clone_admission(&attempt_id).unwrap();
        assert!(admission_seal::Sealed::scale_set_admission_binding(&admission).is_some());

        let (mut store, bridge) = service.into_parts();
        let (inbox, catalog) = store
            .load_scale_set_control_state(&source_identity())
            .unwrap();
        let reservation = &catalog.active()[0];
        let idle = inbox.last_idle().unwrap();
        assert_eq!(idle.catalog_revision(), catalog.revision());
        assert_eq!(idle.attempt_id(), reservation.attempt().attempt_id());
        assert_eq!(idle.attempt_revision(), reservation.attempt().revision());
        assert_eq!(bridge.capacities, [1, 0]);
    }

    #[test]
    fn clone_authorization_consumes_idle_evidence_before_the_clone_command() {
        let root = TempRoot::new();
        let store = UnixPersonalWorkerStore::open_or_create_disposable_catalog(&root.0).unwrap();
        let mut catalog = DisposableAttemptCatalog::new(store);
        catalog.initialize().unwrap();
        let store = catalog.into_store();
        let policy = ScaleSetConsumerPolicy::new(
            source_identity(),
            23,
            "project",
            "example",
            &["smolrunner".to_owned()],
            DisposableWorkerResources::new(2_000, 2 << 30, 20 << 30).unwrap(),
            &current_disposable_prepared_template().unwrap(),
        )
        .unwrap();
        let bridge = FakeBridge {
            polls: VecDeque::from([
                ScaleSetBridgePoll::Message {
                    message_id: 7,
                    statistics: statistics(),
                    events: vec![event()],
                },
                ScaleSetBridgePoll::Idle {
                    statistics: statistics(),
                },
                ScaleSetBridgePoll::Idle {
                    statistics: statistics(),
                },
            ]),
            acquired: VecDeque::from([vec![41]]),
            capacities: Vec::new(),
            acknowledgements: Vec::new(),
        };
        let mut service = ScaleSetService::with_parts(
            store,
            bridge,
            policy.clone(),
            FixedClock(EpochMillis::new(100_000).unwrap()),
        )
        .unwrap();

        for _ in 0..4 {
            service.reconcile_once().unwrap();
        }
        let attempt_id = match service.reconcile_once().unwrap() {
            ScaleSetServiceDisposition::CloneAdmissionRefreshed { attempt_id } => {
                DisposableAttemptId::parse(&attempt_id).unwrap()
            }
            other => panic!("unexpected disposition: {other:?}"),
        };

        let (store, bridge) = service.into_parts();
        let mut catalog = DisposableAttemptCatalog::new(store);
        let current = catalog.load().unwrap();
        let reservation = current.find_active(&attempt_id).unwrap();
        catalog
            .transition(
                current.revision(),
                &attempt_id,
                reservation.attempt().revision(),
                crate::disposable_attempt_catalog::DisposableAttemptCatalogAction::AuthorizeClone,
            )
            .unwrap();
        let mut service = ScaleSetService::with_parts(
            catalog.into_store(),
            bridge,
            policy,
            FixedClock(EpochMillis::new(100_000).unwrap()),
        )
        .unwrap();

        let error = match service.clone_admission(&attempt_id) {
            Ok(_) => panic!("stale idle evidence unexpectedly remained usable"),
            Err(error) => error,
        };
        assert_eq!(error.code(), "scale_set_clone_admission_stale");
        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::CloneAdmissionRefreshed { attempt_id: refreshed }
                if refreshed == attempt_id.as_str()
        ));
        assert!(service.clone_admission(&attempt_id).is_ok());

        let (_, bridge) = service.into_parts();
        assert_eq!(bridge.capacities, [1, 0, 0]);
    }

    #[test]
    fn acquired_job_cancellation_before_clone_is_durably_releasable() {
        let root = TempRoot::new();
        let store = UnixPersonalWorkerStore::open_or_create_disposable_catalog(&root.0).unwrap();
        let mut catalog = DisposableAttemptCatalog::new(store);
        catalog.initialize().unwrap();
        let store = catalog.into_store();
        let policy = ScaleSetConsumerPolicy::new(
            source_identity(),
            23,
            "project",
            "example",
            &["smolrunner".to_owned()],
            DisposableWorkerResources::new(2_000, 2 << 30, 20 << 30).unwrap(),
            &current_disposable_prepared_template().unwrap(),
        )
        .unwrap();
        let bridge = FakeBridge {
            polls: VecDeque::from([
                ScaleSetBridgePoll::Message {
                    message_id: 7,
                    statistics: statistics(),
                    events: vec![event()],
                },
                ScaleSetBridgePoll::Message {
                    message_id: 8,
                    statistics: statistics(),
                    events: vec![assigned_event()],
                },
                ScaleSetBridgePoll::Message {
                    message_id: 9,
                    statistics: statistics(),
                    events: vec![canceled_event()],
                },
            ]),
            acquired: VecDeque::from([vec![41], Vec::new(), Vec::new()]),
            capacities: Vec::new(),
            acknowledgements: Vec::new(),
        };
        let mut service = ScaleSetService::with_parts(
            store,
            bridge,
            policy,
            FixedClock(EpochMillis::new(100_000).unwrap()),
        )
        .unwrap();

        for message_id in [7, 8, 9] {
            assert!(matches!(
                service.reconcile_once().unwrap(),
                ScaleSetServiceDisposition::MessagePersisted { message_id: actual }
                    if actual == message_id
            ));
            assert!(matches!(
                service.reconcile_once().unwrap(),
                ScaleSetServiceDisposition::EventApplied { message_id: actual, .. }
                    if actual == message_id
            ));
            assert!(matches!(
                service.reconcile_once().unwrap(),
                ScaleSetServiceDisposition::MessageAcknowledged { message_id: actual }
                    if actual == message_id
            ));
            assert!(matches!(
                service.reconcile_once().unwrap(),
                ScaleSetServiceDisposition::AckOutcomeApplied { message_id: actual }
                    if actual == message_id
            ));
        }

        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::UnprovisionedReleased { .. }
        ));
        assert!(matches!(
            service.reconcile_once().unwrap(),
            ScaleSetServiceDisposition::AttemptRetired { .. }
        ));

        let (store, bridge) = service.into_parts();
        let catalog = DisposableAttemptCatalogStore::load(&store)
            .unwrap()
            .unwrap();
        let attempt = &catalog.tombstones()[0];
        assert_eq!(attempt.phase(), DisposableAttemptPhase::Complete);
        assert_eq!(attempt.github_job_id().unwrap().as_str(), "job-1");
        assert_eq!(attempt.result().unwrap().as_str(), "canceled");
        assert!(attempt.vm_identity().is_none());
        assert_eq!(bridge.capacities, [1, 0, 0]);
        assert_eq!(bridge.acknowledgements, [7, 8, 9]);
    }
}
