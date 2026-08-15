use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::disposable_attempt_catalog::{
    DisposableAttemptCatalog, DisposableAttemptCatalogAction, DisposableAttemptCatalogErrorKind,
    DisposableAttemptCatalogStore, DisposableAttemptReservation,
    MemoryDisposableAttemptCatalogStore, encode_disposable_attempt_catalog,
};
use crate::disposable_attempt_state::DisposableAttemptState;
use crate::disposable_prepared_template::{
    DisposablePreparedTemplateIdentity, current_disposable_prepared_template,
};
use crate::disposable_worker_reconciler::{
    CapacityClaimId, DisposableAttemptId, DisposableVmId, DisposableVmIdentity,
    DisposableWorkerResources,
};
use crate::execution_admission::EpochMillis;
use crate::github_scale_set_bridge::{
    ScaleSetBridgeEvent, ScaleSetBridgeJobEvidence, ScaleSetBridgePoll, ScaleSetStatistics,
};
use crate::github_scale_set_protocol::{
    ScaleSetJobId, ScaleSetJobResult, ScaleSetRunnerId, ScaleSetRunnerName,
    ScaleSetRunnerReference, ScaleSetRunnerRequestId,
};

use super::super::super::publication_fault::{PublicationFaultPoint, inject_publication_fault};
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
            "smolrunner-scale-set-reconcile-{label}-{}-{sequence}",
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

    fn store_directory(&self) -> PathBuf {
        self.path.join(STORE_DIRECTORY)
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

fn initialize_catalog(root: &TempRoot) -> DisposableAttemptCatalogDocument {
    let store = UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path())
        .expect("open disposable catalog");
    let mut catalog = DisposableAttemptCatalog::new(store);
    catalog.initialize().expect("initialize catalog").0
}

fn successor(index: usize) -> DisposableAttemptCatalogDocument {
    let mut catalog = DisposableAttemptCatalog::new(MemoryDisposableAttemptCatalogStore::default());
    let (empty, _) = catalog.initialize().expect("initialize memory catalog");
    catalog
        .reserve(empty.revision(), reservation(index))
        .expect("reserve successor")
        .0
}

fn reservation(index: usize) -> DisposableAttemptReservation {
    DisposableAttemptReservation::new(
        DisposableAttemptState::reserved(
            DisposableAttemptId::parse(&format!("attempt-{index}")).expect("attempt id"),
            CapacityClaimId::parse(&format!("claim-{index}")).expect("claim id"),
            DisposableVmId::parse(&format!("vm-{index}")).expect("vm id"),
            ScaleSetRunnerName::parse(&format!("smol-attempt-{index}")).expect("runner name"),
            ScaleSetRunnerRequestId::new(1_000 + u64::try_from(index).expect("bounded index"))
                .expect("runner request id"),
            EpochMillis::new(100_000 + u64::try_from(index).expect("bounded index"))
                .expect("expiry"),
        ),
        DisposableWorkerResources::new(1_000, 2_000, 3_000).expect("resources"),
        prepared_template_identity(),
    )
    .expect("reservation")
}

fn prepared_template_identity() -> DisposablePreparedTemplateIdentity {
    current_disposable_prepared_template()
        .expect("current prepared template")
        .identity()
        .expect("prepared template identity")
}

fn consumer_policy() -> ScaleSetDeliveryConsumerPolicy {
    ScaleSetDeliveryConsumerPolicy::new(
        23,
        "project",
        "example",
        &["smolrunner".to_owned()],
        DisposableWorkerResources::new(2_000, 2 << 30, 20 << 30).expect("consumer resources"),
        &current_disposable_prepared_template().expect("current prepared template"),
    )
    .expect("consumer policy")
}

fn observed_at() -> EpochMillis {
    EpochMillis::new(100_000).expect("observation time")
}

fn reconciled_catalog(
    current: &DisposableAttemptCatalogDocument,
    delivery: &ScaleSetDelivery,
) -> DisposableAttemptCatalogDocument {
    reconcile_scale_set_delivery(&consumer_policy(), delivery, current, observed_at())
        .expect("reconcile delivery")
}

fn delivery(message_id: u32, request_id: u64) -> ScaleSetDelivery {
    delivery_with_events(
        message_id,
        vec![ScaleSetBridgeEvent::Available(job(request_id))],
    )
}

fn delivery_with_events(message_id: u32, events: Vec<ScaleSetBridgeEvent>) -> ScaleSetDelivery {
    ScaleSetDelivery::from_bridge_poll(&ScaleSetBridgePoll::Message {
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
        events,
    })
    .expect("canonical delivery")
    .expect("message delivery")
}

fn job(request_id: u64) -> ScaleSetBridgeJobEvidence {
    ScaleSetBridgeJobEvidence {
        runner_request_id: request_id,
        repository: "project".to_owned(),
        owner: "example".to_owned(),
        job_id: ScaleSetJobId::parse(&format!("job-{request_id}")).expect("job id"),
        workflow_run_id: 99,
        request_labels: vec!["smolrunner".to_owned()],
    }
}

fn registering_catalog(request_id: u64) -> DisposableAttemptCatalogDocument {
    let available = delivery(90, request_id);
    let mut catalog = reconciled_catalog(&DisposableAttemptCatalogDocument::empty(), &available);
    let attempt_id = catalog.active()[0].attempt().attempt_id().clone();
    let mut attempt_revision = catalog.active()[0].attempt().revision();
    for action in [
        DisposableAttemptCatalogAction::AuthorizeClone,
        DisposableAttemptCatalogAction::RecordCloneStarted,
    ] {
        catalog = catalog
            .replace_attempt(&attempt_id, attempt_revision, action)
            .expect("advance clone state");
        attempt_revision = catalog.active()[0].attempt().revision();
    }
    catalog = catalog
        .bind_vm_identity_after_clone(
            &attempt_id,
            attempt_revision,
            DisposableVmIdentity::parse(&format!("sha256:{}", "11".repeat(32)))
                .expect("VM identity"),
        )
        .expect("bind VM identity");
    attempt_revision = catalog.active()[0].attempt().revision();
    catalog
        .replace_attempt(
            &attempt_id,
            attempt_revision,
            DisposableAttemptCatalogAction::BeginRegistration,
        )
        .expect("begin registration")
}

fn write_private(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).expect("write private fixture");
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("set private fixture mode");
}

fn prepare_ambiguous(
    root: &TempRoot,
    message_id: u32,
    request_id: u64,
) -> ScaleSetDeliveryRecoveryState {
    let current = DisposableAttemptCatalog::new(
        UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path())
            .expect("open catalog"),
    )
    .load()
    .expect("load catalog");
    let mut paired =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("open paired transaction");
    let initial = paired
        .publish_scale_set_reconciled_delivery(
            current.revision(),
            &consumer_policy(),
            &delivery(message_id, request_id),
            observed_at(),
        )
        .expect("publish available delivery")
        .1;
    drop(paired);
    let started = initial.begin_ack().expect("begin acknowledgement");
    let ambiguous = started
        .record_recovery_acquire(&[])
        .expect("record empty recovery acquisition");
    let mut store =
        UnixPersonalWorkerStore::open_or_create_scale_set_delivery_recovery(root.path())
            .expect("open recovery store");
    store
        .replace_scale_set_delivery_recovery(initial.revision(), &started)
        .expect("publish acknowledgement start");
    store
        .replace_scale_set_delivery_recovery(started.revision(), &ambiguous)
        .expect("publish empty acquisition evidence");
    ambiguous
}

#[test]
fn publishes_catalog_and_delivery_under_one_recoverable_transaction() {
    let root = TempRoot::new("publish");
    let empty = initialize_catalog(&root);
    let delivery = delivery(7, 41);
    let next = reconciled_catalog(&empty, &delivery);

    let mut store =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("open reconcile transaction");
    let (written_catalog, recovery) = store
        .publish_scale_set_reconciled_delivery(
            empty.revision(),
            &consumer_policy(),
            &delivery,
            observed_at(),
        )
        .expect("publish paired reconciliation");
    assert_eq!(written_catalog, next);
    assert_eq!(recovery.catalog_revision(), next.revision());
    assert_eq!(recovery.delivery(), &delivery);
    drop(store);

    let reopened =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("reopen paired transaction");
    assert_eq!(
        reopened
            .load_catalog_named(CATALOG_DOCUMENT)
            .expect("load catalog"),
        Some(next)
    );
    assert_eq!(
        reopened
            .load_scale_set_delivery_recovery()
            .expect("load delivery recovery"),
        Some(recovery)
    );
}

#[test]
fn lifecycle_resolution_replaces_the_original_fence_without_changing_reserved_capacity() {
    let root = TempRoot::new("lifecycle-resolution");
    initialize_catalog(&root);
    let ambiguous = prepare_ambiguous(&root, 70, 91);
    let assigned = delivery_with_events(71, vec![ScaleSetBridgeEvent::Assigned(job(91))]);
    let mut store =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("open resolution transaction");
    let resolved = store
        .publish_scale_set_lifecycle_resolution(&ambiguous, &consumer_policy(), &assigned)
        .expect("publish lifecycle resolution");
    assert!(matches!(
        resolved.phase(),
        ScaleSetDeliveryRecoveryPhase::LifecycleReconciled { resolution }
            if resolution.delivery() == &assigned && resolution.acquired().len() == 1
    ));
    let catalog = store
        .load_catalog_named(CATALOG_DOCUMENT)
        .expect("load catalog")
        .expect("catalog exists");
    assert_eq!(catalog.active().len(), 1);
    assert!(resolved.matches_catalog(&catalog));
}

#[test]
fn assigned_resolution_recovers_delivery_after_directory_sync_ambiguity() {
    let root = TempRoot::new("lifecycle-assigned-recovery");
    initialize_catalog(&root);
    let ambiguous = prepare_ambiguous(&root, 75, 96);
    let assigned = delivery_with_events(76, vec![ScaleSetBridgeEvent::Assigned(job(96))]);
    let mut store =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("open resolution transaction");
    let fault = inject_publication_fault(PublicationFaultPoint::PublicationDirectorySync);
    assert_eq!(
        store
            .publish_scale_set_lifecycle_resolution(&ambiguous, &consumer_policy(), &assigned,)
            .expect_err("directory sync ambiguity")
            .kind(),
        PersonalWorkerStoreErrorKind::Io
    );
    drop(fault);
    drop(store);

    let reopened =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("recover lifecycle delivery");
    let catalog = reopened
        .load_catalog_named(CATALOG_DOCUMENT)
        .expect("load catalog")
        .expect("catalog exists");
    assert_eq!(catalog.active().len(), 1);
    let recovery = reopened
        .load_scale_set_delivery_recovery()
        .expect("load recovery")
        .expect("recovery exists");
    assert!(matches!(
        recovery.phase(),
        ScaleSetDeliveryRecoveryPhase::LifecycleReconciled { resolution }
            if resolution.delivery() == &assigned
    ));
    assert!(recovery.matches_catalog(&catalog));
}

#[test]
fn canceled_resolution_recovers_catalog_then_recovery_after_directory_sync_ambiguity() {
    let root = TempRoot::new("lifecycle-cancel-recovery");
    initialize_catalog(&root);
    let ambiguous = prepare_ambiguous(&root, 80, 101);
    let canceled = delivery_with_events(
        81,
        vec![ScaleSetBridgeEvent::Completed {
            job: job(101),
            runner: None,
            result: ScaleSetJobResult::parse("canceled").expect("result"),
        }],
    );
    let mut store =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("open resolution transaction");
    let fault = inject_publication_fault(PublicationFaultPoint::PublicationDirectorySync);
    assert_eq!(
        store
            .publish_scale_set_lifecycle_resolution(&ambiguous, &consumer_policy(), &canceled,)
            .expect_err("directory sync ambiguity")
            .kind(),
        PersonalWorkerStoreErrorKind::Io
    );
    drop(fault);
    drop(store);

    let reopened =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("recover lifecycle pair");
    let catalog = reopened
        .load_catalog_named(CATALOG_DOCUMENT)
        .expect("load catalog")
        .expect("catalog exists");
    assert!(catalog.active().is_empty());
    assert!(
        catalog
            .find_tombstone_by_runner_request_id(ScaleSetRunnerRequestId::new(101).unwrap())
            .is_some()
    );
    let recovery = reopened
        .load_scale_set_delivery_recovery()
        .expect("load recovery")
        .expect("recovery exists");
    assert!(matches!(
        recovery.phase(),
        ScaleSetDeliveryRecoveryPhase::LifecycleReconciled { resolution }
            if resolution.delivery() == &canceled
    ));
    assert!(recovery.matches_catalog(&catalog));
}

#[test]
fn delivery_only_future_stage_rolls_back_without_advancing_catalog() {
    let root = TempRoot::new("delivery-only-future");
    let empty = initialize_catalog(&root);
    let next = successor(1);
    let recovery = ScaleSetDeliveryRecoveryState::reconciled(delivery(8, 42), &empty, &next)
        .expect("recovery state");
    let store =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("open reconcile transaction");
    let mut staged = store
        .stage_scale_set_delivery(&recovery)
        .expect("stage delivery");
    staged.disarm();
    drop(staged);
    drop(store);

    let reopened =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("recover future delivery stage");
    assert_eq!(
        reopened
            .load_catalog_named(CATALOG_DOCUMENT)
            .expect("load catalog"),
        Some(empty)
    );
    assert_eq!(
        reopened
            .load_scale_set_delivery_recovery()
            .expect("load recovery"),
        None
    );
    assert!(
        !root
            .store_directory()
            .join(STAGED_DELIVERY_RECOVERY_DOCUMENT)
            .exists()
    );
}

#[test]
fn both_exact_stages_recover_catalog_then_delivery() {
    let root = TempRoot::new("both-stages");
    let empty = initialize_catalog(&root);
    let next = successor(1);
    let recovery = ScaleSetDeliveryRecoveryState::reconciled(delivery(9, 43), &empty, &next)
        .expect("recovery state");
    let store =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("open reconcile transaction");
    let mut staged_delivery = store
        .stage_scale_set_delivery(&recovery)
        .expect("stage delivery");
    staged_delivery.disarm();
    let mut staged_catalog = store.stage_catalog(&next).expect("stage catalog");
    staged_catalog.disarm();
    drop(staged_catalog);
    drop(staged_delivery);
    drop(store);

    let reopened =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("recover paired stages");
    assert_eq!(
        reopened
            .load_catalog_named(CATALOG_DOCUMENT)
            .expect("load catalog"),
        Some(next)
    );
    assert_eq!(
        reopened
            .load_scale_set_delivery_recovery()
            .expect("load recovery"),
        Some(recovery)
    );
}

#[test]
fn catalog_only_stage_keeps_ordinary_catalog_recovery_semantics() {
    let root = TempRoot::new("catalog-only");
    initialize_catalog(&root);
    let next = successor(1);
    let store =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("open reconcile transaction");
    let mut staged_catalog = store.stage_catalog(&next).expect("stage catalog");
    staged_catalog.disarm();
    drop(staged_catalog);
    drop(store);

    let reopened =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("recover ordinary catalog stage");
    assert_eq!(
        reopened
            .load_catalog_named(CATALOG_DOCUMENT)
            .expect("load catalog"),
        Some(next)
    );
    assert_eq!(
        reopened
            .load_scale_set_delivery_recovery()
            .expect("load recovery"),
        None
    );
}

#[test]
fn rename_failure_retains_delivery_marker_then_recovery_rolls_it_back() {
    let root = TempRoot::new("rename-fault");
    let empty = initialize_catalog(&root);
    let delivery = delivery(10, 44);
    let mut store =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("open reconcile transaction");
    let fault = inject_publication_fault(PublicationFaultPoint::PublishRename);
    let error = store
        .publish_scale_set_reconciled_delivery(
            empty.revision(),
            &consumer_policy(),
            &delivery,
            observed_at(),
        )
        .expect_err("catalog rename fault");
    assert_eq!(error.kind(), PersonalWorkerStoreErrorKind::Io);
    drop(fault);
    assert!(
        root.store_directory()
            .join(STAGED_DELIVERY_RECOVERY_DOCUMENT)
            .exists()
    );
    assert!(
        !root
            .store_directory()
            .join(STAGED_CATALOG_DOCUMENT)
            .exists()
    );
    drop(store);

    let reopened =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("recover rename failure");
    assert_eq!(
        reopened
            .load_catalog_named(CATALOG_DOCUMENT)
            .expect("load catalog"),
        Some(empty)
    );
    assert_eq!(
        reopened
            .load_scale_set_delivery_recovery()
            .expect("load recovery"),
        None
    );
}

#[test]
fn one_delivery_atomically_publishes_a_multi_revision_lifecycle_fold() {
    let root = TempRoot::new("multi-revision-fold");
    initialize_catalog(&root);
    let prior = registering_catalog(61);
    let catalog_path = root.store_directory().join(CATALOG_DOCUMENT);
    write_private(
        &catalog_path,
        &encode_disposable_attempt_catalog(&prior).expect("encode prior catalog"),
    );

    let runner = ScaleSetRunnerReference::new(
        ScaleSetRunnerId::new(501).expect("runner id"),
        prior.active()[0].attempt().runner_name().clone(),
    );
    let delivery = delivery_with_events(
        17,
        vec![
            ScaleSetBridgeEvent::Assigned(job(61)),
            ScaleSetBridgeEvent::Started {
                job: job(61),
                runner: runner.clone(),
            },
            ScaleSetBridgeEvent::Completed {
                job: job(61),
                runner: Some(runner),
                result: ScaleSetJobResult::parse("succeeded").expect("job result"),
            },
        ],
    );
    let expected = reconciled_catalog(&prior, &delivery);
    assert_eq!(expected.revision().get(), prior.revision().get() + 3);

    let mut store =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("open reconcile transaction");
    let (published, recovery) = store
        .publish_scale_set_reconciled_delivery(
            prior.revision(),
            &consumer_policy(),
            &delivery,
            observed_at(),
        )
        .expect("publish multi-revision fold");
    assert_eq!(published, expected);
    assert!(recovery.matches_prior_catalog(&prior));
    assert!(recovery.matches_catalog(&published));
    let (replayed, replay_recovery) = store
        .publish_scale_set_reconciled_delivery(
            prior.revision(),
            &consumer_policy(),
            &delivery,
            observed_at(),
        )
        .expect("replay multi-revision fold");
    assert_eq!(replayed, published);
    assert_eq!(replay_recovery, recovery);
    drop(store);

    let reopened =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("reopen multi-revision transaction");
    assert_eq!(
        reopened
            .load_catalog_named(CATALOG_DOCUMENT)
            .expect("load catalog"),
        Some(expected)
    );
    assert_eq!(
        reopened
            .load_scale_set_delivery_recovery()
            .expect("load delivery recovery"),
        Some(recovery)
    );
}

#[test]
fn directory_sync_failure_after_catalog_rename_completes_delivery_on_reopen() {
    let root = TempRoot::new("directory-sync-fault");
    let empty = initialize_catalog(&root);
    let delivery = delivery(11, 45);
    let next = reconciled_catalog(&empty, &delivery);
    let mut store =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("open reconcile transaction");
    let fault = inject_publication_fault(PublicationFaultPoint::PublicationDirectorySync);
    let error = store
        .publish_scale_set_reconciled_delivery(
            empty.revision(),
            &consumer_policy(),
            &delivery,
            observed_at(),
        )
        .expect_err("catalog directory sync fault");
    assert_eq!(error.kind(), PersonalWorkerStoreErrorKind::Io);
    drop(fault);
    assert_eq!(
        store
            .load_catalog_named(CATALOG_DOCUMENT)
            .expect("load visible catalog"),
        Some(next.clone())
    );
    assert!(
        root.store_directory()
            .join(STAGED_DELIVERY_RECOVERY_DOCUMENT)
            .exists()
    );
    drop(store);

    let reopened =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("recover directory sync ambiguity");
    let recovered = reopened
        .load_scale_set_delivery_recovery()
        .expect("load recovered delivery")
        .expect("recovered delivery exists");
    assert_eq!(recovered.catalog_revision(), next.revision());
    assert_eq!(recovered.delivery(), &delivery);
}

#[test]
fn exact_reconciliation_replay_is_idempotent() {
    let root = TempRoot::new("exact-replay");
    let empty = initialize_catalog(&root);
    let delivery = delivery(12, 46);
    let expected = reconciled_catalog(&empty, &delivery);
    let mut store =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("open reconcile transaction");
    let (_, initial) = store
        .publish_scale_set_reconciled_delivery(
            empty.revision(),
            &consumer_policy(),
            &delivery,
            observed_at(),
        )
        .expect("publish reconciliation");
    let (same_catalog, same_recovery) = store
        .publish_scale_set_reconciled_delivery(
            empty.revision(),
            &consumer_policy(),
            &delivery,
            observed_at(),
        )
        .expect("replay exact reconciliation");
    assert_eq!(same_catalog, expected);
    assert_eq!(same_recovery, initial);
}

#[test]
fn exact_existing_event_publishes_a_zero_change_delivery_binding() {
    let root = TempRoot::new("zero-change");
    initialize_catalog(&root);
    let delivery = delivery(18, 62);
    let current = reconciled_catalog(&DisposableAttemptCatalogDocument::empty(), &delivery);
    write_private(
        &root.store_directory().join(CATALOG_DOCUMENT),
        &encode_disposable_attempt_catalog(&current).expect("encode current catalog"),
    );

    let mut store =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("open reconcile transaction");
    let (published, recovery) = store
        .publish_scale_set_reconciled_delivery(
            current.revision(),
            &consumer_policy(),
            &delivery,
            observed_at(),
        )
        .expect("publish zero-change reconciliation");
    assert_eq!(published, current);
    assert!(recovery.matches_prior_catalog(&current));
    assert!(recovery.matches_catalog(&current));
    assert!(
        !root
            .store_directory()
            .join(STAGED_CATALOG_DOCUMENT)
            .exists()
    );
}

#[test]
fn foreign_live_delivery_blocks_another_reconciliation() {
    let root = TempRoot::new("live-conflict");
    let empty = initialize_catalog(&root);
    let first = delivery(13, 47);
    let second = delivery(14, 48);
    let mut store =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("open reconcile transaction");
    store
        .publish_scale_set_reconciled_delivery(
            empty.revision(),
            &consumer_policy(),
            &first,
            observed_at(),
        )
        .expect("publish first delivery");
    let error = store
        .publish_scale_set_reconciled_delivery(
            empty.revision(),
            &consumer_policy(),
            &second,
            observed_at(),
        )
        .expect_err("foreign live delivery must block");
    assert_eq!(error.kind(), PersonalWorkerStoreErrorKind::RevisionConflict);
}

#[test]
fn malformed_delivery_only_stage_is_preserved_as_corruption() {
    let root = TempRoot::new("malformed-stage");
    initialize_catalog(&root);
    let store =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("open reconcile transaction");
    drop(store);
    write_private(
        &root
            .store_directory()
            .join(STAGED_DELIVERY_RECOVERY_DOCUMENT),
        b"not canonical JSON\n",
    );
    assert_eq!(
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect_err("malformed stage must fail closed")
            .kind(),
        PersonalWorkerStoreErrorKind::CorruptState
    );
    assert!(
        root.store_directory()
            .join(STAGED_DELIVERY_RECOVERY_DOCUMENT)
            .exists()
    );
}

#[test]
fn future_delivery_marker_blocks_a_competing_same_revision_catalog() {
    let root = TempRoot::new("future-delivery-fence");
    let empty = initialize_catalog(&root);
    let intended = successor(1);
    let competing = successor(2);
    assert_eq!(intended.revision(), competing.revision());
    let recovery = ScaleSetDeliveryRecoveryState::reconciled(delivery(15, 49), &empty, &intended)
        .expect("future delivery state");

    let mut ordinary = UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path())
        .expect("open ordinary catalog writer");
    let paired =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("open paired transaction");
    let mut staged = paired
        .stage_scale_set_delivery(&recovery)
        .expect("stage future delivery");
    staged.disarm();
    drop(staged);
    drop(paired);

    let error = DisposableAttemptCatalogStore::replace_if_revision(
        &mut ordinary,
        empty.revision(),
        &competing,
    )
    .expect_err("future delivery must fence an unrelated catalog successor");
    assert_eq!(
        error.kind(),
        DisposableAttemptCatalogErrorKind::RecoveryRequired
    );
    assert!(
        root.store_directory()
            .join(STAGED_DELIVERY_RECOVERY_DOCUMENT)
            .exists()
    );
    drop(ordinary);

    let recovered =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("recover intended paired transaction");
    assert_eq!(
        recovered
            .load_catalog_named(CATALOG_DOCUMENT)
            .expect("load fenced catalog"),
        Some(empty)
    );
    assert_eq!(
        recovered
            .load_scale_set_delivery_recovery()
            .expect("load rolled-back delivery"),
        None
    );
}

#[test]
fn delivery_binding_refuses_a_same_revision_different_catalog_stage() {
    let root = TempRoot::new("different-catalog-digest");
    let empty = initialize_catalog(&root);
    let intended = successor(1);
    let competing = successor(2);
    assert_eq!(intended.revision(), competing.revision());
    let recovery = ScaleSetDeliveryRecoveryState::reconciled(delivery(19, 63), &empty, &intended)
        .expect("recovery state");
    let store =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("open reconcile transaction");
    let mut staged_delivery = store
        .stage_scale_set_delivery(&recovery)
        .expect("stage delivery");
    staged_delivery.disarm();
    let mut staged_catalog = store.stage_catalog(&competing).expect("stage catalog");
    staged_catalog.disarm();
    drop(staged_catalog);
    drop(staged_delivery);
    drop(store);

    assert_eq!(
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect_err("different catalog bytes must fail closed")
            .kind(),
        PersonalWorkerStoreErrorKind::CorruptState
    );
    assert!(
        root.store_directory()
            .join(STAGED_CATALOG_DOCUMENT)
            .exists()
    );
    assert!(
        root.store_directory()
            .join(STAGED_DELIVERY_RECOVERY_DOCUMENT)
            .exists()
    );
}

#[test]
fn live_delivery_blocks_a_later_catalog_successor() {
    let root = TempRoot::new("live-delivery-fence");
    let empty = initialize_catalog(&root);
    let delivery = delivery(16, 50);
    let one = reconciled_catalog(&empty, &delivery);
    let current_attempt = one.active()[0].attempt();
    let two = one
        .replace_attempt(
            current_attempt.attempt_id(),
            current_attempt.revision(),
            DisposableAttemptCatalogAction::AuthorizeClone,
        )
        .expect("later catalog successor");

    let mut store =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("open paired transaction");
    store
        .publish_scale_set_reconciled_delivery(
            empty.revision(),
            &consumer_policy(),
            &delivery,
            observed_at(),
        )
        .expect("publish live delivery");

    let error =
        DisposableAttemptCatalogStore::replace_if_revision(&mut store, one.revision(), &two)
            .expect_err("live delivery must fence later catalog mutation");
    assert_eq!(
        error.kind(),
        DisposableAttemptCatalogErrorKind::RecoveryRequired
    );
    assert_eq!(
        store
            .load_catalog_named(CATALOG_DOCUMENT)
            .expect("load preserved catalog"),
        Some(one.clone())
    );
    let current_delivery = store
        .load_scale_set_delivery_named(DELIVERY_RECOVERY_DOCUMENT)
        .expect("load preserved delivery")
        .expect("live delivery exists");
    assert_eq!(current_delivery.catalog_revision(), one.revision());
    assert_eq!(current_delivery.delivery(), &delivery);
}
