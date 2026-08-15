use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::disposable_attempt_catalog::{
    DisposableAttemptCatalog, DisposableAttemptReservation, MemoryDisposableAttemptCatalogStore,
};
use crate::disposable_attempt_state::DisposableAttemptState;
use crate::disposable_prepared_template::{
    DisposablePreparedTemplateIdentity, current_disposable_prepared_template,
};
use crate::disposable_worker_reconciler::{
    CapacityClaimId, DisposableAttemptId, DisposableVmId, DisposableWorkerResources,
};
use crate::execution_admission::EpochMillis;
use crate::github_scale_set_bridge::{
    ScaleSetBridgeEvent, ScaleSetBridgeJobEvidence, ScaleSetBridgePoll, ScaleSetStatistics,
};
use crate::github_scale_set_protocol::{ScaleSetJobId, ScaleSetRunnerName};

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

fn delivery(message_id: u32, request_id: u64) -> ScaleSetDelivery {
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
        events: vec![ScaleSetBridgeEvent::Available(ScaleSetBridgeJobEvidence {
            runner_request_id: request_id,
            repository: "project".to_owned(),
            owner: "example".to_owned(),
            job_id: ScaleSetJobId::parse(&format!("job-{request_id}")).expect("job id"),
            workflow_run_id: 99,
            request_labels: vec!["smolrunner".to_owned()],
        })],
    })
    .expect("canonical delivery")
    .expect("message delivery")
}

fn write_private(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).expect("write private fixture");
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("set private fixture mode");
}

#[test]
fn publishes_catalog_and_delivery_under_one_recoverable_transaction() {
    let root = TempRoot::new("publish");
    let empty = initialize_catalog(&root);
    let next = successor(1);
    let delivery = delivery(7, 41);

    let mut store =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("open reconcile transaction");
    let (written_catalog, recovery) = store
        .publish_scale_set_reconciled_delivery(empty.revision(), &next, &delivery)
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
fn delivery_only_future_stage_rolls_back_without_advancing_catalog() {
    let root = TempRoot::new("delivery-only-future");
    let empty = initialize_catalog(&root);
    let next = successor(1);
    let recovery = ScaleSetDeliveryRecoveryState::reconciled(delivery(8, 42), next.revision())
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
    initialize_catalog(&root);
    let next = successor(1);
    let recovery = ScaleSetDeliveryRecoveryState::reconciled(delivery(9, 43), next.revision())
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
    let next = successor(1);
    let delivery = delivery(10, 44);
    let mut store =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("open reconcile transaction");
    let fault = inject_publication_fault(PublicationFaultPoint::PublishRename);
    let error = store
        .publish_scale_set_reconciled_delivery(empty.revision(), &next, &delivery)
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
fn directory_sync_failure_after_catalog_rename_completes_delivery_on_reopen() {
    let root = TempRoot::new("directory-sync-fault");
    let empty = initialize_catalog(&root);
    let next = successor(1);
    let delivery = delivery(11, 45);
    let mut store =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("open reconcile transaction");
    let fault = inject_publication_fault(PublicationFaultPoint::PublicationDirectorySync);
    let error = store
        .publish_scale_set_reconciled_delivery(empty.revision(), &next, &delivery)
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
fn zero_change_reconciliation_and_exact_replay_are_idempotent() {
    let root = TempRoot::new("zero-change");
    let empty = initialize_catalog(&root);
    let delivery = delivery(12, 46);
    let mut store =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("open reconcile transaction");
    let (_, initial) = store
        .publish_scale_set_reconciled_delivery(empty.revision(), &empty, &delivery)
        .expect("publish zero-change reconciliation");
    let (same_catalog, same_recovery) = store
        .publish_scale_set_reconciled_delivery(empty.revision(), &empty, &delivery)
        .expect("replay exact reconciliation");
    assert_eq!(same_catalog, empty);
    assert_eq!(same_recovery, initial);
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
        .publish_scale_set_reconciled_delivery(empty.revision(), &empty, &first)
        .expect("publish first delivery");
    let error = store
        .publish_scale_set_reconciled_delivery(empty.revision(), &empty, &second)
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
