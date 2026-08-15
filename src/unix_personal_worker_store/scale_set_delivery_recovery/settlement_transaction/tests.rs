use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::disposable_attempt_catalog::{
    DisposableAttemptCatalog, DisposableAttemptCatalogAction, DisposableAttemptCatalogDocument,
};
use crate::disposable_prepared_template::current_disposable_prepared_template;
use crate::disposable_worker_reconciler::DisposableWorkerResources;
use crate::execution_admission::EpochMillis;
use crate::github_scale_set_bridge::{
    ScaleSetBridgeEvent, ScaleSetBridgeJobEvidence, ScaleSetBridgePoll, ScaleSetStatistics,
};
use crate::github_scale_set_delivery::ScaleSetDelivery;
use crate::github_scale_set_delivery_consumer::ScaleSetDeliveryConsumerPolicy;
use crate::github_scale_set_delivery_settlement::settle_scale_set_delivery_catalog;
use crate::github_scale_set_protocol::{ScaleSetJobId, ScaleSetRunnerRequestId};
use crate::personal_worker_store::PersonalWorkerStoreErrorKind;
use crate::unix_personal_worker_store::publication_fault::{
    PublicationFaultPoint, inject_publication_fault,
};
use crate::unix_personal_worker_store::{STORE_DIRECTORY, UnixPersonalWorkerStore};

use super::super::super::disposable_attempt_catalog::CATALOG_DOCUMENT;
use super::super::ScaleSetExternalTransaction;

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
            "smolrunner-scale-set-settlement-{label}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create temporary root");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o750))
            .expect("set temporary root mode");
        let metadata = fs::symlink_metadata(&path).expect("inspect temporary root");
        Self {
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn store_file(&self, name: &str) -> PathBuf {
        self.path.join(STORE_DIRECTORY).join(name)
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

fn delivery(request_id: u64) -> ScaleSetDelivery {
    ScaleSetDelivery::from_bridge_poll(&ScaleSetBridgePoll::Message {
        message_id: 7,
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
    .expect("valid delivery")
    .expect("message delivery")
}

fn initialize(root: &TempRoot) -> DisposableAttemptCatalogDocument {
    DisposableAttemptCatalog::new(
        UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path())
            .expect("open catalog"),
    )
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

fn acknowledged(
    root: &TempRoot,
    request_id: ScaleSetRunnerRequestId,
) -> crate::github_scale_set_delivery_state::ScaleSetDeliveryRecoveryState {
    let initial_catalog = initialize(root);
    let mut paired =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("open reconcile transaction");
    let recovery = paired
        .publish_scale_set_reconciled_delivery(
            initial_catalog.revision(),
            &policy(),
            &delivery(request_id.get()),
            EpochMillis::new(100_000).expect("observed at"),
        )
        .expect("publish reconciliation")
        .1;
    drop(paired);

    let mut store =
        UnixPersonalWorkerStore::open_or_create_scale_set_delivery_controller(root.path())
            .expect("open controller store");
    match store
        .acknowledge_scale_set_delivery_locked(&recovery, |_| {
            Ok::<_, ()>(Vec::<ScaleSetRunnerRequestId>::new())
        })
        .expect("acknowledgement transaction")
    {
        ScaleSetExternalTransaction::Completed(state) => state,
        ScaleSetExternalTransaction::ExternalFailed(()) => panic!("acknowledgement failed"),
    }
}

#[test]
fn settlement_recovers_when_prepared_publication_directory_sync_is_ambiguous() {
    let root = TempRoot::new("prepared-sync");
    let request = ScaleSetRunnerRequestId::new(41).expect("request id");
    let acknowledged = acknowledged(&root, request);
    let mut store =
        UnixPersonalWorkerStore::open_or_create_scale_set_delivery_controller(root.path())
            .expect("open controller store");
    let fault = inject_publication_fault(PublicationFaultPoint::PublicationDirectorySync);
    let error = store
        .settle_scale_set_delivery_locked(&acknowledged)
        .expect_err("injected directory-sync failure");
    assert_eq!(error.kind(), PersonalWorkerStoreErrorKind::Io);
    drop(fault);
    drop(store);

    let reopened =
        UnixPersonalWorkerStore::open_or_create_scale_set_delivery_controller(root.path())
            .expect("recover prepared settlement");
    assert!(
        reopened
            .load_scale_set_delivery_recovery()
            .expect("load recovery")
            .is_none()
    );
    drop(reopened);
    let catalog = load_catalog(&root);
    assert!(catalog.active().is_empty());
    assert!(
        catalog
            .find_tombstone_by_runner_request_id(request)
            .is_some()
    );
}

#[test]
fn settlement_recovers_after_catalog_publication_before_delivery_removal() {
    let root = TempRoot::new("catalog-published");
    let request = ScaleSetRunnerRequestId::new(42).expect("request id");
    let acknowledged = acknowledged(&root, request);
    let mut store =
        UnixPersonalWorkerStore::open_or_create_scale_set_delivery_controller(root.path())
            .expect("open controller store");
    let catalog = store
        .load_catalog_named(CATALOG_DOCUMENT)
        .expect("load catalog")
        .expect("catalog exists");
    let target = settle_scale_set_delivery_catalog(&acknowledged, &catalog)
        .expect("derive settlement target");
    let prepared = acknowledged
        .prepare_settlement(&catalog, &target)
        .expect("prepare settlement");
    store
        .replace_scale_set_delivery_recovery(acknowledged.revision(), &prepared)
        .expect("publish prepared settlement");
    {
        let _lock = store.acquire_mutation_lock().expect("acquire writer lock");
        let mut staged = store.stage_catalog(&target).expect("stage target catalog");
        store
            .publish_named_staged(&mut staged, CATALOG_DOCUMENT, false)
            .expect("publish target catalog");
    }
    drop(store);

    let reopened =
        UnixPersonalWorkerStore::open_or_create_scale_set_delivery_controller(root.path())
            .expect("recover removal");
    assert!(
        reopened
            .load_scale_set_delivery_recovery()
            .expect("load recovery")
            .is_none()
    );
    assert!(
        !root
            .store_file(super::super::DELIVERY_RECOVERY_DOCUMENT)
            .exists()
    );
}

#[test]
fn recovery_rejects_a_canonical_but_unrelated_target_catalog() {
    let root = TempRoot::new("unrelated-target");
    let request = ScaleSetRunnerRequestId::new(142).expect("request id");
    let acknowledged = acknowledged(&root, request);
    let mut store =
        UnixPersonalWorkerStore::open_or_create_scale_set_delivery_controller(root.path())
            .expect("open controller store");
    let catalog = store
        .load_catalog_named(CATALOG_DOCUMENT)
        .expect("load catalog")
        .expect("catalog exists");
    let reservation = catalog
        .find_active_by_runner_request_id(request)
        .expect("reserved attempt");
    let unrelated = catalog
        .replace_attempt(
            reservation.attempt().attempt_id(),
            reservation.attempt().revision(),
            DisposableAttemptCatalogAction::AuthorizeClone,
        )
        .expect("construct unrelated successor");
    let forged = acknowledged
        .prepare_settlement(&catalog, &unrelated)
        .expect("construct canonical forged settlement");
    store
        .replace_scale_set_delivery_recovery(acknowledged.revision(), &forged)
        .expect("publish forged prepared state");
    {
        let _lock = store.acquire_mutation_lock().expect("acquire writer lock");
        let mut staged = store
            .stage_catalog(&unrelated)
            .expect("stage unrelated catalog");
        store
            .publish_named_staged(&mut staged, CATALOG_DOCUMENT, false)
            .expect("publish unrelated catalog");
    }
    drop(store);

    assert_eq!(
        UnixPersonalWorkerStore::open_or_create_scale_set_delivery_controller(root.path())
            .expect_err("unrelated target must remain recovery debt")
            .kind(),
        PersonalWorkerStoreErrorKind::CorruptState
    );
    assert!(
        root.store_file(super::super::DELIVERY_RECOVERY_DOCUMENT)
            .exists()
    );
}

#[test]
fn controller_open_recovers_delivery_first_reconciliation_crash() {
    let root = TempRoot::new("reconcile-stage");
    let catalog = initialize(&root);
    let mut paired =
        UnixPersonalWorkerStore::open_or_create_scale_set_reconcile_transaction(root.path())
            .expect("open reconcile transaction");
    let fault = inject_publication_fault(PublicationFaultPoint::PublishRename);
    let error = paired
        .publish_scale_set_reconciled_delivery(
            catalog.revision(),
            &policy(),
            &delivery(43),
            EpochMillis::new(100_000).expect("observed at"),
        )
        .expect_err("inject catalog publication failure");
    assert_eq!(error.kind(), PersonalWorkerStoreErrorKind::Io);
    drop(fault);
    drop(paired);

    let controller =
        UnixPersonalWorkerStore::open_or_create_scale_set_delivery_controller(root.path())
            .expect("recover controller transaction");
    assert!(
        controller
            .load_scale_set_delivery_recovery()
            .expect("load recovery")
            .is_none()
    );
    drop(controller);
    assert_eq!(load_catalog(&root), catalog);
}
