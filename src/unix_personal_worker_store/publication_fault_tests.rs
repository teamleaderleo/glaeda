use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::disposable_attempt_catalog::DisposableAttemptCatalogRevision;
use crate::github_scale_set_bridge::{
    ScaleSetBridgeEvent, ScaleSetBridgeJobEvidence, ScaleSetBridgePoll, ScaleSetStatistics,
};
use crate::github_scale_set_delivery::ScaleSetDelivery;
use crate::github_scale_set_delivery_state::ScaleSetDeliveryRecoveryState;
use crate::github_scale_set_protocol::{ScaleSetJobId, ScaleSetRunnerRequestId};
use crate::personal_worker_store::PersonalWorkerStoreErrorKind;

use super::publication_fault::{PublicationFaultPoint, inject_publication_fault};
use super::scale_set_delivery_recovery::STAGED_DELIVERY_RECOVERY_DOCUMENT;
use super::{STORE_DIRECTORY, UnixPersonalWorkerStore};

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
            "smolrunner-scale-set-publication-fault-{label}-{}-{sequence}",
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

    fn staged_delivery(&self) -> PathBuf {
        self.path
            .join(STORE_DIRECTORY)
            .join(STAGED_DELIVERY_RECOVERY_DOCUMENT)
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

fn job(request_id: u64, job_id: &str) -> ScaleSetBridgeJobEvidence {
    ScaleSetBridgeJobEvidence {
        runner_request_id: request_id,
        repository: "project".to_owned(),
        owner: "example".to_owned(),
        job_id: ScaleSetJobId::parse(job_id).expect("job id"),
        workflow_run_id: 99,
        request_labels: vec!["smolrunner".to_owned()],
    }
}

fn initial() -> ScaleSetDeliveryRecoveryState {
    let delivery = ScaleSetDelivery::from_bridge_poll(&ScaleSetBridgePoll::Message {
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
        events: vec![ScaleSetBridgeEvent::Available(job(41, "job-1"))],
    })
    .expect("delivery")
    .expect("message delivery");
    ScaleSetDeliveryRecoveryState::reconciled(
        delivery,
        DisposableAttemptCatalogRevision::new(8).expect("catalog revision"),
    )
    .expect("initial recovery state")
}

fn assert_io(error: &crate::personal_worker_store::PersonalWorkerStoreError) {
    assert_eq!(error.kind(), PersonalWorkerStoreErrorKind::Io);
}

#[test]
fn pre_rename_publication_faults_leave_initial_delivery_unpublished() {
    for point in [
        PublicationFaultPoint::StageWrite,
        PublicationFaultPoint::StageFileSync,
        PublicationFaultPoint::PublishRename,
    ] {
        let root = TempRoot::new(match point {
            PublicationFaultPoint::StageWrite => "create-write",
            PublicationFaultPoint::StageFileSync => "create-file-sync",
            PublicationFaultPoint::PublishRename => "create-rename",
            PublicationFaultPoint::PublicationDirectorySync => unreachable!(),
        });
        let mut store =
            UnixPersonalWorkerStore::open_or_create_scale_set_delivery_recovery(root.path())
                .expect("open delivery store");
        let state = initial();
        let fault = inject_publication_fault(point);
        let error = store
            .create_scale_set_delivery_recovery(&state)
            .expect_err("injected publication failure");
        assert_io(&error);
        drop(fault);

        assert_eq!(
            store
                .load_scale_set_delivery_recovery()
                .expect("load after injected fault"),
            None
        );
        assert!(!root.staged_delivery().exists());
        drop(store);

        let reopened =
            UnixPersonalWorkerStore::open_or_create_scale_set_delivery_recovery(root.path())
                .expect("reopen delivery store");
        assert_eq!(
            reopened
                .load_scale_set_delivery_recovery()
                .expect("load reopened state"),
            None
        );
    }
}

#[test]
fn directory_sync_fault_reports_error_after_initial_delivery_is_visible() {
    let root = TempRoot::new("create-directory-sync");
    let mut store =
        UnixPersonalWorkerStore::open_or_create_scale_set_delivery_recovery(root.path())
            .expect("open delivery store");
    let state = initial();
    let fault = inject_publication_fault(PublicationFaultPoint::PublicationDirectorySync);
    let error = store
        .create_scale_set_delivery_recovery(&state)
        .expect_err("directory sync failure");
    assert_io(&error);
    drop(fault);

    assert_eq!(
        store
            .load_scale_set_delivery_recovery()
            .expect("load visible publication"),
        Some(state.clone())
    );
    assert!(!root.staged_delivery().exists());
    drop(store);

    let reopened = UnixPersonalWorkerStore::open_or_create_scale_set_delivery_recovery(root.path())
        .expect("reopen delivery store");
    assert_eq!(
        reopened
            .load_scale_set_delivery_recovery()
            .expect("load reopened publication"),
        Some(state)
    );
}

#[test]
fn replacement_faults_preserve_old_or_publish_successor_truthfully() {
    for point in [
        PublicationFaultPoint::StageWrite,
        PublicationFaultPoint::StageFileSync,
        PublicationFaultPoint::PublishRename,
        PublicationFaultPoint::PublicationDirectorySync,
    ] {
        let root = TempRoot::new(match point {
            PublicationFaultPoint::StageWrite => "replace-write",
            PublicationFaultPoint::StageFileSync => "replace-file-sync",
            PublicationFaultPoint::PublishRename => "replace-rename",
            PublicationFaultPoint::PublicationDirectorySync => "replace-directory-sync",
        });
        let mut store =
            UnixPersonalWorkerStore::open_or_create_scale_set_delivery_recovery(root.path())
                .expect("open delivery store");
        let current = initial();
        store
            .create_scale_set_delivery_recovery(&current)
            .expect("publish initial recovery state");
        let successor = current.begin_ack().expect("ack successor");

        let fault = inject_publication_fault(point);
        let error = store
            .replace_scale_set_delivery_recovery(current.revision(), &successor)
            .expect_err("injected replacement failure");
        assert_io(&error);
        drop(fault);

        let expected = if point == PublicationFaultPoint::PublicationDirectorySync {
            successor.clone()
        } else {
            current.clone()
        };
        assert_eq!(
            store
                .load_scale_set_delivery_recovery()
                .expect("load after replacement fault"),
            Some(expected.clone())
        );
        assert!(!root.staged_delivery().exists());
        drop(store);

        let reopened =
            UnixPersonalWorkerStore::open_or_create_scale_set_delivery_recovery(root.path())
                .expect("reopen delivery store");
        assert_eq!(
            reopened
                .load_scale_set_delivery_recovery()
                .expect("load reopened replacement state"),
            Some(expected)
        );
    }
}

#[test]
fn fault_guard_clears_an_unconsumed_thread_local_fault() {
    let root = TempRoot::new("guard-clear");
    let mut store =
        UnixPersonalWorkerStore::open_or_create_scale_set_delivery_recovery(root.path())
            .expect("open delivery store");
    {
        let _fault = inject_publication_fault(PublicationFaultPoint::PublishRename);
    }
    let state = initial();
    store
        .create_scale_set_delivery_recovery(&state)
        .expect("dropped guard clears injection");
    assert_eq!(
        store
            .load_scale_set_delivery_recovery()
            .expect("load published state"),
        Some(state)
    );
}
