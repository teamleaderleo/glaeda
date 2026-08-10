#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use smolrunner::disposable_attempt_catalog::{
    DisposableAttemptCatalog, DisposableAttemptCatalogDocument, DisposableAttemptReservation,
};
use smolrunner::disposable_attempt_state::DisposableAttemptState;
use smolrunner::disposable_prepared_template::{
    DisposablePreparedTemplateIdentity, current_disposable_prepared_template,
};
use smolrunner::disposable_worker_reconciler::{
    CapacityClaimId, DisposableAttemptId, DisposableAttemptPhase, DisposableVmId,
    DisposableVmObservation, DisposableWorkerAction, DisposableWorkerReconcileInput,
    DisposableWorkerResources, ScaleSetRunnerObservation, reconcile_attempt,
};
use smolrunner::execution_admission::EpochMillis;
use smolrunner::github_scale_set_protocol::{
    ScaleSetJobEvent, ScaleSetJobId, ScaleSetJobResult, ScaleSetRunnerId, ScaleSetRunnerName,
    ScaleSetRunnerReference,
};
use smolrunner::unix_personal_worker_store::UnixPersonalWorkerStore;

static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

struct TempRoot(PathBuf);

impl TempRoot {
    fn new() -> Self {
        let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "smolrunner-durable-reconcile-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create state root");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).expect("private state root");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn epoch(value: u64) -> EpochMillis {
    EpochMillis::new(value).unwrap()
}

fn template_digest() -> DisposablePreparedTemplateIdentity {
    current_disposable_prepared_template()
        .unwrap()
        .identity()
        .unwrap()
}

fn attempt_id() -> DisposableAttemptId {
    DisposableAttemptId::parse("attempt-restart-1").unwrap()
}

fn runner() -> ScaleSetRunnerReference {
    ScaleSetRunnerReference::new(
        ScaleSetRunnerId::new(71).unwrap(),
        ScaleSetRunnerName::parse("smol-attempt-restart-1").unwrap(),
    )
}

fn initialize(root: &Path) -> DisposableAttemptCatalogDocument {
    let store = UnixPersonalWorkerStore::open_or_create_disposable_catalog(root).unwrap();
    let mut catalog = DisposableAttemptCatalog::new(store);
    let (empty, _) = catalog.initialize().unwrap();
    let state = DisposableAttemptState::reserved(
        attempt_id(),
        CapacityClaimId::parse("claim-restart-1").unwrap(),
        DisposableVmId::parse("vm-restart-1").unwrap(),
        ScaleSetRunnerName::parse("smol-attempt-restart-1").unwrap(),
        epoch(100_000),
    );
    catalog
        .reserve(
            empty.revision(),
            DisposableAttemptReservation::new(
                state,
                DisposableWorkerResources::new(2_000, 4_000, 8_000).unwrap(),
                template_digest(),
            )
            .unwrap(),
        )
        .unwrap()
        .0
}

fn tick(
    root: &Path,
    vm: DisposableVmObservation,
    runner_observation: ScaleSetRunnerObservation,
    job_event: Option<ScaleSetJobEvent>,
    capacity_reserved: bool,
) -> (DisposableAttemptCatalogDocument, DisposableWorkerAction) {
    tick_with_cancellation(
        root,
        vm,
        runner_observation,
        job_event,
        capacity_reserved,
        false,
    )
}

fn tick_with_cancellation(
    root: &Path,
    vm: DisposableVmObservation,
    runner_observation: ScaleSetRunnerObservation,
    job_event: Option<ScaleSetJobEvent>,
    capacity_reserved: bool,
    cancellation_requested: bool,
) -> (DisposableAttemptCatalogDocument, DisposableWorkerAction) {
    let store = UnixPersonalWorkerStore::open_or_create_disposable_catalog(root).unwrap();
    let mut catalog = DisposableAttemptCatalog::new(store);
    let current = catalog.load().unwrap();
    let reservation = current.find_active(&attempt_id()).unwrap();
    let state = reservation.attempt().clone();
    let action = reconcile_attempt(DisposableWorkerReconcileInput {
        now: epoch(1_000),
        attempt: &state,
        vm,
        runner: runner_observation,
        job_event,
        capacity_reserved,
        cancellation_requested,
    })
    .unwrap();
    let next = if let DisposableWorkerAction::Persist { transition } = &action {
        catalog
            .transition(
                current.revision(),
                state.attempt_id(),
                state.revision(),
                transition.clone(),
            )
            .unwrap()
            .0
    } else {
        current
    };
    drop(catalog);

    let reopened = UnixPersonalWorkerStore::open_or_create_disposable_catalog(root).unwrap();
    let reopened = DisposableAttemptCatalog::new(reopened).load().unwrap();
    assert_eq!(
        reopened, next,
        "every tick must survive a controller restart"
    );
    (reopened, action)
}

#[test]
fn every_durable_checkpoint_reopens_without_duplicate_capacity_or_cleanup_loss() {
    let root = TempRoot::new();
    let initial = initialize(root.path());
    assert_eq!(initial.host_usage().unwrap().workers(), 1);

    let (state, action) = tick(
        root.path(),
        DisposableVmObservation::Absent,
        ScaleSetRunnerObservation::Absent,
        None,
        true,
    );
    assert!(matches!(action, DisposableWorkerAction::Persist { .. }));
    assert_eq!(
        state.find_active(&attempt_id()).unwrap().attempt().phase(),
        DisposableAttemptPhase::CloneAuthorized
    );

    for _ in 0..2 {
        let (_, action) = tick(
            root.path(),
            DisposableVmObservation::Absent,
            ScaleSetRunnerObservation::Absent,
            None,
            true,
        );
        assert_eq!(action, DisposableWorkerAction::CloneVm);
    }

    for _ in 0..2 {
        let (_, action) = tick(
            root.path(),
            DisposableVmObservation::Stopped,
            ScaleSetRunnerObservation::Absent,
            None,
            true,
        );
        assert_eq!(action, DisposableWorkerAction::DiscardIncompleteVm);
    }

    let (state, _) = tick(
        root.path(),
        DisposableVmObservation::Ready,
        ScaleSetRunnerObservation::Absent,
        None,
        true,
    );
    assert_eq!(
        state.find_active(&attempt_id()).unwrap().attempt().phase(),
        DisposableAttemptPhase::Registering
    );
    for _ in 0..2 {
        let (_, action) = tick(
            root.path(),
            DisposableVmObservation::Ready,
            ScaleSetRunnerObservation::Absent,
            None,
            true,
        );
        assert_eq!(action, DisposableWorkerAction::GenerateJitAndStartRunner);
    }

    let exact_runner = runner();
    let (state, _) = tick(
        root.path(),
        DisposableVmObservation::Ready,
        ScaleSetRunnerObservation::IdleReady {
            runner: exact_runner.clone(),
        },
        None,
        true,
    );
    assert_eq!(
        state.find_active(&attempt_id()).unwrap().attempt().phase(),
        DisposableAttemptPhase::Waiting
    );

    let exact_job = ScaleSetJobId::parse("opaque-job/restart-1").unwrap();
    let (state, _) = tick(
        root.path(),
        DisposableVmObservation::Ready,
        ScaleSetRunnerObservation::IdleReady {
            runner: exact_runner.clone(),
        },
        Some(ScaleSetJobEvent::Started {
            runner: exact_runner.clone(),
            job_id: exact_job.clone(),
        }),
        true,
    );
    assert_eq!(
        state.find_active(&attempt_id()).unwrap().attempt().phase(),
        DisposableAttemptPhase::Running
    );

    let (state, _) = tick(
        root.path(),
        DisposableVmObservation::Ready,
        ScaleSetRunnerObservation::IdleReady {
            runner: exact_runner,
        },
        Some(ScaleSetJobEvent::Completed {
            runner: Some(runner()),
            job_id: exact_job,
            result: ScaleSetJobResult::parse("succeeded").unwrap(),
        }),
        true,
    );
    assert_eq!(
        state.find_active(&attempt_id()).unwrap().attempt().phase(),
        DisposableAttemptPhase::Terminal
    );

    let (state, _) = tick(
        root.path(),
        DisposableVmObservation::Ready,
        ScaleSetRunnerObservation::RegistrationOnly { runner: runner() },
        None,
        true,
    );
    assert_eq!(
        state.find_active(&attempt_id()).unwrap().attempt().phase(),
        DisposableAttemptPhase::Destroying
    );
    for _ in 0..2 {
        let (_, action) = tick(
            root.path(),
            DisposableVmObservation::Ready,
            ScaleSetRunnerObservation::RegistrationOnly { runner: runner() },
            None,
            true,
        );
        assert_eq!(action, DisposableWorkerAction::DestroyVm);
    }

    let (state, _) = tick(
        root.path(),
        DisposableVmObservation::Absent,
        ScaleSetRunnerObservation::RegistrationOnly { runner: runner() },
        None,
        true,
    );
    assert_eq!(
        state.find_active(&attempt_id()).unwrap().attempt().phase(),
        DisposableAttemptPhase::Deregistering
    );
    for _ in 0..2 {
        let (_, action) = tick(
            root.path(),
            DisposableVmObservation::Absent,
            ScaleSetRunnerObservation::RegistrationOnly { runner: runner() },
            None,
            true,
        );
        assert_eq!(
            action,
            DisposableWorkerAction::DeleteRunner { runner: runner() }
        );
    }

    let (state, _) = tick(
        root.path(),
        DisposableVmObservation::Absent,
        ScaleSetRunnerObservation::Absent,
        None,
        true,
    );
    assert_eq!(
        state.find_active(&attempt_id()).unwrap().attempt().phase(),
        DisposableAttemptPhase::Releasing
    );
    for _ in 0..2 {
        let (_, action) = tick(
            root.path(),
            DisposableVmObservation::Absent,
            ScaleSetRunnerObservation::Absent,
            None,
            true,
        );
        assert_eq!(action, DisposableWorkerAction::ReleaseCapacity);
    }

    let (state, _) = tick(
        root.path(),
        DisposableVmObservation::Absent,
        ScaleSetRunnerObservation::Absent,
        None,
        false,
    );
    assert_eq!(
        state.find_active(&attempt_id()).unwrap().attempt().phase(),
        DisposableAttemptPhase::Complete
    );

    let store = UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
    let catalog = DisposableAttemptCatalog::new(store).load().unwrap();
    assert_eq!(catalog.host_usage().unwrap().workers(), 0);
}

#[test]
fn lost_reserved_capacity_completes_without_acquiring_vm_cleanup_authority() {
    let root = TempRoot::new();
    initialize(root.path());

    let (catalog, action) = tick(
        root.path(),
        DisposableVmObservation::Ready,
        ScaleSetRunnerObservation::Absent,
        None,
        false,
    );
    assert!(matches!(
        action,
        DisposableWorkerAction::Persist {
            transition: smolrunner::disposable_attempt_catalog::DisposableAttemptCatalogAction::CompleteUnprovisioned
        }
    ));
    let attempt = catalog.find_active(&attempt_id()).unwrap().attempt();
    assert_eq!(attempt.phase(), DisposableAttemptPhase::Complete);
    assert_eq!(attempt.revision().get(), 2);
    assert!(attempt.runner_id().is_none());
    assert!(attempt.github_job_id().is_none());
    assert_eq!(catalog.host_usage().unwrap().workers(), 0);

    let (_, replay) = tick(
        root.path(),
        DisposableVmObservation::Ready,
        ScaleSetRunnerObservation::Absent,
        None,
        false,
    );
    assert_eq!(
        replay,
        DisposableWorkerAction::Blocked {
            code: "completed_attempt_retains_external_state"
        }
    );

    let (_, absent_replay) = tick(
        root.path(),
        DisposableVmObservation::Absent,
        ScaleSetRunnerObservation::Absent,
        None,
        false,
    );
    assert_eq!(absent_replay, DisposableWorkerAction::NoOp);
}

#[test]
fn reserved_cancellation_survives_restart_before_capacity_release() {
    let root = TempRoot::new();
    initialize(root.path());

    let (catalog, checkpoint) = tick_with_cancellation(
        root.path(),
        DisposableVmObservation::Absent,
        ScaleSetRunnerObservation::Absent,
        None,
        true,
        true,
    );
    assert!(matches!(
        checkpoint,
        DisposableWorkerAction::Persist {
            transition: smolrunner::disposable_attempt_catalog::DisposableAttemptCatalogAction::BeginUnprovisionedRelease
        }
    ));
    assert_eq!(
        catalog
            .find_active(&attempt_id())
            .unwrap()
            .attempt()
            .phase(),
        DisposableAttemptPhase::UnprovisionedReleasing
    );

    // The transient cancellation input is deliberately absent after reopening.
    let (_, release) = tick(
        root.path(),
        DisposableVmObservation::Absent,
        ScaleSetRunnerObservation::Absent,
        None,
        true,
    );
    assert_eq!(release, DisposableWorkerAction::ReleaseCapacity);

    let (complete, _) = tick(
        root.path(),
        DisposableVmObservation::Absent,
        ScaleSetRunnerObservation::Absent,
        None,
        false,
    );
    assert_eq!(
        complete
            .find_active(&attempt_id())
            .unwrap()
            .attempt()
            .phase(),
        DisposableAttemptPhase::Complete
    );
    assert_eq!(complete.host_usage().unwrap().workers(), 0);
}

#[test]
fn stale_registration_is_bound_before_delete_and_recovery_never_uses_name_alone() {
    let root = TempRoot::new();
    initialize(root.path());

    tick(
        root.path(),
        DisposableVmObservation::Absent,
        ScaleSetRunnerObservation::Absent,
        None,
        true,
    );
    tick(
        root.path(),
        DisposableVmObservation::Stopped,
        ScaleSetRunnerObservation::Absent,
        None,
        true,
    );
    tick(
        root.path(),
        DisposableVmObservation::Ready,
        ScaleSetRunnerObservation::Absent,
        None,
        true,
    );

    let (catalog, action) = tick(
        root.path(),
        DisposableVmObservation::Ready,
        ScaleSetRunnerObservation::RegistrationOnly { runner: runner() },
        None,
        true,
    );
    assert!(matches!(
        action,
        DisposableWorkerAction::Persist {
            transition:
                smolrunner::disposable_attempt_catalog::DisposableAttemptCatalogAction::RecordRegistration(_)
        }
    ));
    assert_eq!(
        catalog
            .find_active(&attempt_id())
            .unwrap()
            .attempt()
            .runner_id()
            .unwrap()
            .get(),
        71
    );

    tick(
        root.path(),
        DisposableVmObservation::Ready,
        ScaleSetRunnerObservation::RegistrationOnly { runner: runner() },
        None,
        true,
    );
    tick(
        root.path(),
        DisposableVmObservation::Absent,
        ScaleSetRunnerObservation::RegistrationOnly { runner: runner() },
        None,
        true,
    );

    for _ in 0..2 {
        let (_, action) = tick(
            root.path(),
            DisposableVmObservation::Absent,
            ScaleSetRunnerObservation::RegistrationOnly { runner: runner() },
            None,
            true,
        );
        assert_eq!(
            action,
            DisposableWorkerAction::DeleteRunner { runner: runner() }
        );
    }

    let (catalog, _) = tick(
        root.path(),
        DisposableVmObservation::Absent,
        ScaleSetRunnerObservation::Absent,
        None,
        true,
    );
    let state = catalog.find_active(&attempt_id()).unwrap().attempt();
    assert_eq!(state.phase(), DisposableAttemptPhase::Releasing);

    let replacement = ScaleSetRunnerReference::new(
        ScaleSetRunnerId::new(72).unwrap(),
        ScaleSetRunnerName::parse("smol-attempt-restart-1").unwrap(),
    );
    assert_eq!(
        reconcile_attempt(DisposableWorkerReconcileInput {
            now: epoch(1_000),
            attempt: state,
            vm: DisposableVmObservation::Absent,
            runner: ScaleSetRunnerObservation::RegistrationOnly {
                runner: replacement,
            },
            job_event: None,
            capacity_reserved: true,
            cancellation_requested: false,
        })
        .unwrap_err()
        .code(),
        "runner_identity_drift"
    );
}
