use std::collections::VecDeque;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::disposable_attempt_catalog::{
    DisposableAttemptCatalog, DisposableAttemptCatalogDocument, DisposableAttemptCatalogErrorKind,
    encode_disposable_attempt_catalog,
};
use crate::disposable_prepared_template::current_disposable_prepared_template;
use crate::disposable_worker_reconciler::DisposableWorkerResources;
use crate::github_scale_set_bridge::{
    ScaleSetBridgeEvent, ScaleSetBridgeJobEvidence, ScaleSetStatistics,
};
use crate::github_scale_set_delivery_state::ScaleSetDeliveryRecoveryPhase;
use crate::github_scale_set_protocol::{ScaleSetJobId, ScaleSetRunnerRequestId};

use super::*;

static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

struct TempRoot {
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl TempRoot {
    fn new(label: &str) -> Self {
        let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "smolrunner-scale-set-controller-{label}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create temporary state root");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o750))
            .expect("set temporary root mode");
        let metadata = fs::symlink_metadata(&path).expect("inspect temporary state root");
        Self {
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let Ok(metadata) = fs::symlink_metadata(&self.path) else {
            return;
        };
        if metadata.file_type().is_dir()
            && !metadata.file_type().is_symlink()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
        {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[derive(Default)]
struct FakeBridge {
    polls: VecDeque<ScaleSetBridgePoll>,
    acknowledgements: VecDeque<Result<Vec<u64>, ScaleSetBridgeError>>,
    acquisitions: VecDeque<Result<Vec<ScaleSetRunnerRequestId>, ScaleSetBridgeError>>,
    calls: Vec<&'static str>,
    poisoned: bool,
}

impl DeliveryBridge for FakeBridge {
    fn poll(&mut self, available_capacity: u16) -> Result<ScaleSetBridgePoll, ScaleSetBridgeError> {
        if self.poisoned {
            return Err(ScaleSetBridgeError::new("poisoned"));
        }
        assert_eq!(available_capacity, 1);
        self.calls.push("poll");
        Ok(self.polls.pop_front().expect("expected poll response"))
    }

    fn ack(&mut self, _: u32) -> Result<Vec<u64>, ScaleSetBridgeError> {
        if self.poisoned {
            return Err(ScaleSetBridgeError::new("poisoned"));
        }
        self.calls.push("ack");
        self.acknowledgements
            .pop_front()
            .expect("expected acknowledgement response")
    }

    fn acquire(
        &mut self,
        _: &[ScaleSetRunnerRequestId],
    ) -> Result<Vec<ScaleSetRunnerRequestId>, ScaleSetBridgeError> {
        if self.poisoned {
            return Err(ScaleSetBridgeError::new("poisoned"));
        }
        self.calls.push("acquire");
        self.acquisitions
            .pop_front()
            .expect("expected acquisition response")
    }

    fn poison(&mut self) {
        self.poisoned = true;
    }
}

struct LockCheckingBridge<'a> {
    root: &'a Path,
    poll: Option<ScaleSetBridgePoll>,
    saw_busy: bool,
}

impl DeliveryBridge for LockCheckingBridge<'_> {
    fn poll(&mut self, available_capacity: u16) -> Result<ScaleSetBridgePoll, ScaleSetBridgeError> {
        assert_eq!(available_capacity, 1);
        Ok(self.poll.take().expect("one poll"))
    }

    fn ack(&mut self, _: u32) -> Result<Vec<u64>, ScaleSetBridgeError> {
        self.saw_busy = UnixPersonalWorkerStore::open_or_create_disposable_catalog(self.root)
            .expect_err("ack callback must retain the canonical writer lock")
            .kind()
            == DisposableAttemptCatalogErrorKind::Busy;
        Ok(vec![48])
    }

    fn acquire(
        &mut self,
        _: &[ScaleSetRunnerRequestId],
    ) -> Result<Vec<ScaleSetRunnerRequestId>, ScaleSetBridgeError> {
        panic!("acquire is not expected")
    }

    fn poison(&mut self) {}
}

fn policy() -> ScaleSetDeliveryConsumerPolicy {
    ScaleSetDeliveryConsumerPolicy::new(
        23,
        "project",
        "example",
        &["smolrunner".to_owned()],
        DisposableWorkerResources::new(2_000, 2 << 30, 20 << 30).expect("resources"),
        &current_disposable_prepared_template().expect("prepared template"),
    )
    .expect("consumer policy")
}

fn observed_at() -> EpochMillis {
    EpochMillis::new(100_000).expect("observation time")
}

fn message(message_id: u32, request_id: u64) -> ScaleSetBridgePoll {
    ScaleSetBridgePoll::Message {
        message_id,
        statistics: ScaleSetStatistics {
            available_jobs: 1,
            acquired_jobs: 0,
            assigned_jobs: 0,
            running_jobs: 0,
            registered_runners: 0,
            busy_runners: 0,
            idle_runners: 0,
        },
        events: vec![ScaleSetBridgeEvent::Available(ScaleSetBridgeJobEvidence {
            runner_request_id: request_id,
            repository: "project".to_owned(),
            owner: "example".to_owned(),
            job_id: ScaleSetJobId::parse(&format!("job-{request_id}")).expect("job id"),
            workflow_run_id: 99,
            request_labels: vec!["smolrunner".to_owned()],
        })],
    }
}

fn initialize(root: &TempRoot) -> DisposableAttemptCatalogDocument {
    let store = UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path())
        .expect("open catalog");
    DisposableAttemptCatalog::new(store)
        .initialize()
        .expect("initialize catalog")
        .0
}

fn load_catalog(root: &TempRoot) -> DisposableAttemptCatalogDocument {
    DisposableAttemptCatalog::new(
        UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path())
            .expect("open catalog"),
    )
    .load()
    .expect("load catalog")
}

fn prepare_reconciled(root: &TempRoot, poll: &ScaleSetBridgePoll) -> ScaleSetDeliveryRecoveryState {
    let catalog = DisposableAttemptCatalog::new(
        UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path())
            .expect("open catalog"),
    )
    .load()
    .expect("load catalog");
    let delivery = ScaleSetDelivery::from_bridge_poll(poll)
        .expect("valid delivery")
        .expect("message delivery");
    let mut store =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("open paired transaction");
    store
        .publish_scale_set_reconciled_delivery(
            catalog.revision(),
            &policy(),
            &delivery,
            observed_at(),
        )
        .expect("publish delivery")
        .1
}

fn load_recovery(root: &TempRoot) -> ScaleSetDeliveryRecoveryState {
    UnixPersonalWorkerStore::open_or_create_scale_set_delivery_recovery(root.path())
        .expect("open recovery store")
        .load_scale_set_delivery_recovery()
        .expect("load recovery")
        .expect("recovery exists")
}

fn maybe_recovery(root: &TempRoot) -> Option<ScaleSetDeliveryRecoveryState> {
    UnixPersonalWorkerStore::open_or_create_scale_set_delivery_controller(root.path())
        .expect("open controller store")
        .load_scale_set_delivery_recovery()
        .expect("load recovery")
}

#[test]
fn poll_persists_before_ack_and_records_the_exact_acquired_subset() {
    let root = TempRoot::new("happy");
    initialize(&root);
    let mut bridge = FakeBridge {
        polls: VecDeque::from([message(7, 41)]),
        acknowledgements: VecDeque::from([Ok(vec![41])]),
        ..FakeBridge::default()
    };

    let result = consume_with_bridge(root.path(), &policy(), &mut bridge, observed_at())
        .expect("consume message");
    assert_eq!(
        result,
        ScaleSetDeliveryControllerDisposition::Settled { acquired: 1 }
    );
    assert_eq!(bridge.calls, ["poll", "ack"]);
    assert!(maybe_recovery(&root).is_none());
    assert!(
        load_catalog(&root)
            .find_active_by_runner_request_id(ScaleSetRunnerRequestId::new(41).unwrap())
            .is_some()
    );
}

#[test]
fn definitively_unacquired_available_request_retires_with_the_delivery_fence() {
    let root = TempRoot::new("unacquired");
    initialize(&root);
    let request = ScaleSetRunnerRequestId::new(141).expect("request id");
    let mut bridge = FakeBridge {
        polls: VecDeque::from([message(107, request.get())]),
        acknowledgements: VecDeque::from([Ok(Vec::new())]),
        ..FakeBridge::default()
    };

    assert_eq!(
        consume_with_bridge(root.path(), &policy(), &mut bridge, observed_at()).unwrap(),
        ScaleSetDeliveryControllerDisposition::Settled { acquired: 0 }
    );
    assert!(maybe_recovery(&root).is_none());
    let catalog = load_catalog(&root);
    assert!(catalog.active().is_empty());
    assert!(
        catalog
            .find_tombstone_by_runner_request_id(request)
            .is_some()
    );
}

#[test]
fn reconciled_restart_requires_exact_redelivery_before_ack() {
    let root = TempRoot::new("redelivery");
    initialize(&root);
    let poll = message(8, 42);
    prepare_reconciled(&root, &poll);
    let mut bridge = FakeBridge {
        polls: VecDeque::from([poll]),
        acknowledgements: VecDeque::from([Ok(vec![42])]),
        ..FakeBridge::default()
    };

    assert_eq!(
        consume_with_bridge(root.path(), &policy(), &mut bridge, observed_at()).unwrap(),
        ScaleSetDeliveryControllerDisposition::Settled { acquired: 1 }
    );
    assert_eq!(bridge.calls, ["poll", "ack"]);
    assert!(maybe_recovery(&root).is_none());
}

#[test]
fn acknowledgement_failure_leaves_started_and_never_replays_ack() {
    let root = TempRoot::new("ack-failure");
    initialize(&root);
    let mut failed_bridge = FakeBridge {
        polls: VecDeque::from([message(20, 47)]),
        acknowledgements: VecDeque::from([Err(ScaleSetBridgeError::new("injected"))]),
        ..FakeBridge::default()
    };

    assert_eq!(
        consume_with_bridge(root.path(), &policy(), &mut failed_bridge, observed_at())
            .unwrap_err()
            .code(),
        "scale_set_bridge_failed"
    );
    assert_eq!(failed_bridge.calls, ["poll", "ack"]);
    assert!(failed_bridge.poisoned);
    assert!(matches!(
        load_recovery(&root).phase(),
        ScaleSetDeliveryRecoveryPhase::AcknowledgementStarted
    ));

    assert_eq!(
        consume_with_bridge(root.path(), &policy(), &mut failed_bridge, observed_at())
            .unwrap_err()
            .code(),
        "scale_set_bridge_failed"
    );
    assert_eq!(failed_bridge.calls, ["poll", "ack"]);

    let request = ScaleSetRunnerRequestId::new(47).expect("request id");
    let mut recovery_bridge = FakeBridge {
        acquisitions: VecDeque::from([Ok(vec![request])]),
        ..FakeBridge::default()
    };
    assert_eq!(
        consume_with_bridge(root.path(), &policy(), &mut recovery_bridge, observed_at(),).unwrap(),
        ScaleSetDeliveryControllerDisposition::Settled { acquired: 1 }
    );
    assert_eq!(recovery_bridge.calls, ["acquire"]);
}

#[test]
fn canonical_writer_lock_spans_the_acknowledgement_call() {
    let root = TempRoot::new("ack-lock");
    initialize(&root);
    let mut bridge = LockCheckingBridge {
        root: root.path(),
        poll: Some(message(21, 48)),
        saw_busy: false,
    };

    assert_eq!(
        consume_with_bridge(root.path(), &policy(), &mut bridge, observed_at()).unwrap(),
        ScaleSetDeliveryControllerDisposition::Settled { acquired: 1 }
    );
    assert!(bridge.saw_busy);
}

#[test]
fn foreign_acknowledgement_response_poisons_and_preserves_started_recovery() {
    let root = TempRoot::new("foreign-ack");
    initialize(&root);
    let mut bridge = FakeBridge {
        polls: VecDeque::from([message(22, 49)]),
        acknowledgements: VecDeque::from([Ok(vec![999])]),
        ..FakeBridge::default()
    };

    assert_eq!(
        consume_with_bridge(root.path(), &policy(), &mut bridge, observed_at())
            .unwrap_err()
            .code(),
        "scale_set_ack_response_invalid"
    );
    assert!(bridge.poisoned);
    assert!(matches!(
        load_recovery(&root).phase(),
        ScaleSetDeliveryRecoveryPhase::AcknowledgementStarted
    ));
}

#[test]
fn acknowledgement_started_uses_only_standalone_acquisition_after_restart() {
    let root = TempRoot::new("acquire-recovery");
    initialize(&root);
    let initial = prepare_reconciled(&root, &message(9, 43));
    let started = initial.begin_ack().expect("begin acknowledgement");
    let mut store =
        UnixPersonalWorkerStore::open_or_create_scale_set_delivery_recovery(root.path())
            .expect("open recovery store");
    store
        .replace_scale_set_delivery_recovery(initial.revision(), &started)
        .expect("publish acknowledgement start");
    drop(store);
    let request = ScaleSetRunnerRequestId::new(43).expect("request id");
    let mut bridge = FakeBridge {
        acquisitions: VecDeque::from([Ok(vec![request])]),
        ..FakeBridge::default()
    };

    assert_eq!(
        consume_with_bridge(root.path(), &policy(), &mut bridge, observed_at()).unwrap(),
        ScaleSetDeliveryControllerDisposition::Settled { acquired: 1 }
    );
    assert_eq!(bridge.calls, ["acquire"]);
    assert!(maybe_recovery(&root).is_none());
}

#[test]
fn empty_acquisition_replay_remains_explicit_recovery_debt() {
    let root = TempRoot::new("empty-acquire");
    initialize(&root);
    let initial = prepare_reconciled(&root, &message(10, 44));
    let started = initial.begin_ack().expect("begin acknowledgement");
    let mut store =
        UnixPersonalWorkerStore::open_or_create_scale_set_delivery_recovery(root.path())
            .expect("open recovery store");
    store
        .replace_scale_set_delivery_recovery(initial.revision(), &started)
        .expect("publish acknowledgement start");
    drop(store);
    let mut bridge = FakeBridge {
        acquisitions: VecDeque::from([Ok(vec![])]),
        ..FakeBridge::default()
    };

    assert_eq!(
        consume_with_bridge(root.path(), &policy(), &mut bridge, observed_at()).unwrap(),
        ScaleSetDeliveryControllerDisposition::RecoveryRequired
    );
    assert_eq!(bridge.calls, ["acquire"]);
    assert!(matches!(
        load_recovery(&root).phase(),
        ScaleSetDeliveryRecoveryPhase::AcquisitionRecoveryObserved { acquired }
            if acquired.is_empty()
    ));
    assert_eq!(
        UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path())
            .expect_err("ambiguous acquisition must retain the catalog fence")
            .kind(),
        DisposableAttemptCatalogErrorKind::RecoveryRequired
    );
}

#[test]
fn conflicting_redelivery_never_crosses_the_ack_checkpoint() {
    let root = TempRoot::new("conflict");
    initialize(&root);
    let initial = prepare_reconciled(&root, &message(11, 45));
    let mut bridge = FakeBridge {
        polls: VecDeque::from([message(12, 46)]),
        ..FakeBridge::default()
    };

    assert_eq!(
        consume_with_bridge(root.path(), &policy(), &mut bridge, observed_at())
            .unwrap_err()
            .code(),
        "scale_set_delivery_recovery_conflict"
    );
    assert_eq!(bridge.calls, ["poll"]);
    assert!(bridge.poisoned);
    assert_eq!(load_recovery(&root), initial);
}

#[test]
fn post_poll_publication_failure_poisons_the_pending_bridge_session() {
    let root = TempRoot::new("publication-failure");
    let mut bridge = FakeBridge {
        polls: VecDeque::from([message(23, 50)]),
        ..FakeBridge::default()
    };

    assert_eq!(
        consume_with_bridge(root.path(), &policy(), &mut bridge, observed_at())
            .unwrap_err()
            .code(),
        "scale_set_catalog_unavailable"
    );
    assert!(bridge.poisoned);
    assert_eq!(bridge.calls, ["poll"]);
}

#[test]
fn catalog_drift_blocks_acquisition_before_the_external_call() {
    let root = TempRoot::new("catalog-drift");
    initialize(&root);
    let initial = prepare_reconciled(&root, &message(24, 51));
    let started = initial.begin_ack().expect("begin acknowledgement");
    let mut store =
        UnixPersonalWorkerStore::open_or_create_scale_set_delivery_recovery(root.path())
            .expect("open recovery store");
    store
        .replace_scale_set_delivery_recovery(initial.revision(), &started)
        .expect("publish acknowledgement start");
    drop(store);
    fs::write(
        root.path()
            .join(crate::unix_personal_worker_store::STORE_DIRECTORY)
            .join("disposable-attempt-catalog.json"),
        encode_disposable_attempt_catalog(&DisposableAttemptCatalogDocument::empty())
            .expect("encode replacement catalog"),
    )
    .expect("replace catalog bytes");
    let mut bridge = FakeBridge {
        acquisitions: VecDeque::from([Ok(vec![
            ScaleSetRunnerRequestId::new(51).expect("request id"),
        ])]),
        ..FakeBridge::default()
    };

    assert_eq!(
        consume_with_bridge(root.path(), &policy(), &mut bridge, observed_at())
            .unwrap_err()
            .code(),
        "scale_set_delivery_store_failed"
    );
    assert!(bridge.calls.is_empty());
}
