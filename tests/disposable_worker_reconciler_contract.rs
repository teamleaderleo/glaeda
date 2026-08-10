use smolrunner::disposable_attempt_catalog::DisposableAttemptCatalogAction;
use smolrunner::disposable_attempt_state::DisposableAttemptState;
use smolrunner::disposable_worker_reconciler::{
    CapacityClaimId, DisposableAttemptId, DisposableAttemptPhase, DisposableHostBudget,
    DisposableHostUsage, DisposableVmId, DisposableWorkerAction, DisposableWorkerObservationTarget,
    DisposableWorkerReconcileInput, DisposableWorkerResources, ExactObjectObservation,
    ScaleSetDemand, ScaleSetRunnerObservation, plan_capacity, reconcile_attempt,
};
use smolrunner::execution_admission::EpochMillis;
use smolrunner::github_scale_set_protocol::{
    ScaleSetJobEvent, ScaleSetJobId, ScaleSetJobResult, ScaleSetRunnerId, ScaleSetRunnerName,
    ScaleSetRunnerReference,
};

fn time(value: u64) -> EpochMillis {
    EpochMillis::new(value).unwrap()
}

fn resources(cpu: u32, memory: u64, disk: u64) -> DisposableWorkerResources {
    DisposableWorkerResources::new(cpu, memory, disk).unwrap()
}

fn attempt() -> DisposableAttemptState {
    DisposableAttemptState::reserved(
        DisposableAttemptId::parse("attempt-1").unwrap(),
        CapacityClaimId::parse("claim-1").unwrap(),
        DisposableVmId::parse("vm-1").unwrap(),
        ScaleSetRunnerName::parse("smol-attempt-1").unwrap(),
        time(10_000),
    )
}

fn runner(id: u64) -> ScaleSetRunnerReference {
    ScaleSetRunnerReference::new(
        ScaleSetRunnerId::new(id).unwrap(),
        ScaleSetRunnerName::parse("smol-attempt-1").unwrap(),
    )
}

fn job(value: &str) -> ScaleSetJobId {
    ScaleSetJobId::parse(value).unwrap()
}

fn result(value: &str) -> ScaleSetJobResult {
    ScaleSetJobResult::parse(value).unwrap()
}

fn input<'a>(
    attempt: &'a DisposableAttemptState,
    vm: ExactObjectObservation,
    runner: ScaleSetRunnerObservation,
) -> DisposableWorkerReconcileInput<'a> {
    DisposableWorkerReconcileInput {
        now: time(1_000),
        attempt,
        vm,
        runner,
        job_event: None,
        capacity_reserved: true,
        cancellation_requested: false,
    }
}

fn persist(action: DisposableAttemptCatalogAction) -> DisposableWorkerAction {
    DisposableWorkerAction::Persist { transition: action }
}

fn apply(
    state: &DisposableAttemptState,
    action: &DisposableWorkerAction,
) -> DisposableAttemptState {
    let DisposableWorkerAction::Persist { transition } = action else {
        panic!("action is not durable");
    };
    match transition {
        DisposableAttemptCatalogAction::BeginProvisioning => state.begin_provisioning(),
        DisposableAttemptCatalogAction::BeginRegistration => state.begin_registration(),
        DisposableAttemptCatalogAction::RecordRegistration(runner) => {
            state.record_registration(runner)
        }
        DisposableAttemptCatalogAction::RecordRunnerReady(runner) => {
            state.record_runner_ready(runner)
        }
        DisposableAttemptCatalogAction::RecordAssigned(job_id) => {
            state.record_assigned(job_id.clone())
        }
        DisposableAttemptCatalogAction::RecordRunning { runner, job_id } => {
            state.record_running(runner, job_id.clone())
        }
        DisposableAttemptCatalogAction::RecordTerminal {
            runner,
            job_id,
            result,
        } => state.record_terminal(runner.as_ref(), job_id.clone(), result.clone()),
        DisposableAttemptCatalogAction::BeginCleanup => state.begin_cleanup(),
        DisposableAttemptCatalogAction::AdvanceCleanup(phase) => state.advance_cleanup(*phase),
    }
    .unwrap()
}

#[test]
fn capacity_is_bounded_by_demand_global_workers_and_every_resource() {
    let demand = ScaleSetDemand::new(10, 2, time(900), time(1_100)).unwrap();
    let budget = DisposableHostBudget::new(3, resources(12_000, 24_000, 300_000)).unwrap();
    let usage = DisposableHostUsage::new(1, resources(4_000, 8_000, 100_000)).unwrap();
    let plan = plan_capacity(
        time(1_000),
        demand,
        budget,
        usage,
        resources(4_000, 8_000, 100_000),
    )
    .unwrap();
    assert_eq!(plan.advertised_max_capacity(), 3);
    assert_eq!(plan.desired_workers(), 3);
    assert_eq!(plan.additional_workers(), 2);

    let disk_limited = DisposableHostBudget::new(10, resources(40_000, 80_000, 150_000)).unwrap();
    let plan = plan_capacity(
        time(1_000),
        demand,
        disk_limited,
        DisposableHostUsage::zero(),
        resources(4_000, 8_000, 100_000),
    )
    .unwrap();
    assert_eq!(plan.advertised_max_capacity(), 1);
    assert_eq!(plan.additional_workers(), 1);

    let idle = ScaleSetDemand::new(0, 0, time(900), time(1_100)).unwrap();
    let plan = plan_capacity(
        time(1_000),
        idle,
        budget,
        DisposableHostUsage::zero(),
        resources(4_000, 8_000, 100_000),
    )
    .unwrap();
    assert_eq!(plan.desired_workers(), 0);
    assert_eq!(plan.additional_workers(), 0);
}

#[test]
fn stale_demand_and_overcommitted_usage_fail_closed() {
    assert_eq!(
        ScaleSetDemand::new(2, 3, time(900), time(1_100))
            .unwrap_err()
            .code(),
        "invalid_scale_set_demand"
    );
    let demand = ScaleSetDemand::new(2, 1, time(900), time(950)).unwrap();
    let budget = DisposableHostBudget::new(1, resources(4_000, 8_000, 100_000)).unwrap();
    assert_eq!(
        plan_capacity(
            time(1_000),
            demand,
            budget,
            DisposableHostUsage::zero(),
            resources(4_000, 8_000, 100_000),
        )
        .unwrap_err()
        .code(),
        "stale_scale_set_demand"
    );
}

#[test]
fn happy_path_uses_canonical_durable_transitions() {
    let mut state = attempt();
    let action = reconcile_attempt(input(
        &state,
        ExactObjectObservation::Absent,
        ScaleSetRunnerObservation::Absent,
    ))
    .unwrap();
    assert_eq!(
        action,
        persist(DisposableAttemptCatalogAction::BeginProvisioning)
    );
    state = apply(&state, &action);

    assert_eq!(
        reconcile_attempt(input(
            &state,
            ExactObjectObservation::Absent,
            ScaleSetRunnerObservation::Absent,
        ))
        .unwrap(),
        DisposableWorkerAction::ProvisionVm
    );
    let action = reconcile_attempt(input(
        &state,
        ExactObjectObservation::Matching,
        ScaleSetRunnerObservation::Absent,
    ))
    .unwrap();
    state = apply(&state, &action);
    assert_eq!(state.phase(), DisposableAttemptPhase::Registering);
    assert_eq!(
        reconcile_attempt(input(
            &state,
            ExactObjectObservation::Matching,
            ScaleSetRunnerObservation::Absent,
        ))
        .unwrap(),
        DisposableWorkerAction::GenerateJitAndStartRunner
    );

    let exact_runner = runner(41);
    let action = reconcile_attempt(input(
        &state,
        ExactObjectObservation::Matching,
        ScaleSetRunnerObservation::IdleReady {
            runner: exact_runner.clone(),
        },
    ))
    .unwrap();
    state = apply(&state, &action);
    assert_eq!(state.phase(), DisposableAttemptPhase::Waiting);

    let mut started = input(
        &state,
        ExactObjectObservation::Matching,
        ScaleSetRunnerObservation::IdleReady {
            runner: exact_runner.clone(),
        },
    );
    started.job_event = Some(ScaleSetJobEvent::Started {
        runner: exact_runner.clone(),
        job_id: job("job-A/opaque"),
    });
    let action = reconcile_attempt(started).unwrap();
    state = apply(&state, &action);
    assert_eq!(state.phase(), DisposableAttemptPhase::Running);

    let mut completed = input(
        &state,
        ExactObjectObservation::Matching,
        ScaleSetRunnerObservation::IdleReady {
            runner: exact_runner.clone(),
        },
    );
    completed.job_event = Some(ScaleSetJobEvent::Completed {
        runner: Some(exact_runner),
        job_id: job("job-A/opaque"),
        result: result("future service result"),
    });
    let action = reconcile_attempt(completed).unwrap();
    state = apply(&state, &action);
    assert_eq!(state.phase(), DisposableAttemptPhase::Terminal);
    assert_eq!(state.result().unwrap().as_str(), "future service result");
}

#[test]
fn crash_discovered_registration_is_deleted_before_regeneration() {
    let mut state = attempt().begin_provisioning().unwrap();
    state = state.begin_registration().unwrap();
    let registered = ScaleSetRunnerObservation::RegistrationOnly { runner: runner(41) };
    assert_eq!(
        reconcile_attempt(input(&state, ExactObjectObservation::Matching, registered,)).unwrap(),
        DisposableWorkerAction::DeleteRunner
    );
    assert_eq!(
        reconcile_attempt(input(
            &state,
            ExactObjectObservation::Matching,
            ScaleSetRunnerObservation::Absent,
        ))
        .unwrap(),
        DisposableWorkerAction::GenerateJitAndStartRunner
    );
}

#[test]
fn completion_before_assignment_is_durable_and_still_cleans_up() {
    let state = attempt();
    let mut completed = input(
        &state,
        ExactObjectObservation::Absent,
        ScaleSetRunnerObservation::Absent,
    );
    completed.job_event = Some(ScaleSetJobEvent::Completed {
        runner: None,
        job_id: job("cancelled-before-assignment"),
        result: result("canceled"),
    });
    let action = reconcile_attempt(completed).unwrap();
    let state = apply(&state, &action);
    assert_eq!(state.phase(), DisposableAttemptPhase::Terminal);
    assert_eq!(
        reconcile_attempt(input(
            &state,
            ExactObjectObservation::Absent,
            ScaleSetRunnerObservation::Absent,
        ))
        .unwrap(),
        persist(DisposableAttemptCatalogAction::BeginCleanup)
    );
}

#[test]
fn terminal_cleanup_orders_vm_runner_and_capacity_release() {
    let exact_runner = runner(41);
    let mut state = attempt()
        .record_terminal(None, job("job-cleanup"), result("failed"))
        .unwrap()
        .begin_cleanup()
        .unwrap();
    assert_eq!(
        reconcile_attempt(input(
            &state,
            ExactObjectObservation::Matching,
            ScaleSetRunnerObservation::RegistrationOnly {
                runner: exact_runner.clone(),
            },
        ))
        .unwrap(),
        DisposableWorkerAction::DestroyVm
    );
    let action = reconcile_attempt(input(
        &state,
        ExactObjectObservation::Absent,
        ScaleSetRunnerObservation::RegistrationOnly {
            runner: exact_runner.clone(),
        },
    ))
    .unwrap();
    state = apply(&state, &action);
    assert_eq!(state.phase(), DisposableAttemptPhase::Deregistering);
    assert_eq!(
        reconcile_attempt(input(
            &state,
            ExactObjectObservation::Absent,
            ScaleSetRunnerObservation::RegistrationOnly {
                runner: exact_runner,
            },
        ))
        .unwrap(),
        DisposableWorkerAction::DeleteRunner
    );
    let action = reconcile_attempt(input(
        &state,
        ExactObjectObservation::Absent,
        ScaleSetRunnerObservation::Absent,
    ))
    .unwrap();
    state = apply(&state, &action);
    assert_eq!(state.phase(), DisposableAttemptPhase::Releasing);
    assert_eq!(
        reconcile_attempt(input(
            &state,
            ExactObjectObservation::Absent,
            ScaleSetRunnerObservation::Absent,
        ))
        .unwrap(),
        DisposableWorkerAction::ReleaseCapacity
    );
    let mut released = input(
        &state,
        ExactObjectObservation::Absent,
        ScaleSetRunnerObservation::Absent,
    );
    released.capacity_reserved = false;
    let action = reconcile_attempt(released).unwrap();
    state = apply(&state, &action);
    assert_eq!(state.phase(), DisposableAttemptPhase::Complete);
}

#[test]
fn cancellation_expiry_and_lost_capacity_converge_to_cleanup() {
    let state = attempt();
    let mut cancelled = input(
        &state,
        ExactObjectObservation::Absent,
        ScaleSetRunnerObservation::Absent,
    );
    cancelled.cancellation_requested = true;
    assert_eq!(
        reconcile_attempt(cancelled).unwrap(),
        persist(DisposableAttemptCatalogAction::BeginCleanup)
    );
    let mut expired = input(
        &state,
        ExactObjectObservation::Absent,
        ScaleSetRunnerObservation::Absent,
    );
    expired.now = time(10_001);
    assert_eq!(
        reconcile_attempt(expired).unwrap(),
        persist(DisposableAttemptCatalogAction::BeginCleanup)
    );
    let mut lost = input(
        &state,
        ExactObjectObservation::Absent,
        ScaleSetRunnerObservation::Absent,
    );
    lost.capacity_reserved = false;
    assert_eq!(
        reconcile_attempt(lost).unwrap(),
        persist(DisposableAttemptCatalogAction::BeginCleanup)
    );
}

#[test]
fn unknown_conflicting_and_identity_drift_never_authorize_mutation() {
    let state = attempt();
    assert_eq!(
        reconcile_attempt(input(
            &state,
            ExactObjectObservation::Conflicting,
            ScaleSetRunnerObservation::Absent,
        ))
        .unwrap(),
        DisposableWorkerAction::Blocked {
            code: "conflicting_vm_identity"
        }
    );
    let mut state = state.begin_provisioning().unwrap();
    state = state.begin_registration().unwrap();
    let wrong = ScaleSetRunnerReference::new(
        ScaleSetRunnerId::new(9).unwrap(),
        ScaleSetRunnerName::parse("another-runner").unwrap(),
    );
    assert_eq!(
        reconcile_attempt(input(
            &state,
            ExactObjectObservation::Matching,
            ScaleSetRunnerObservation::IdleReady { runner: wrong },
        ))
        .unwrap_err()
        .code(),
        "runner_identity_drift"
    );
}

#[test]
fn duplicate_job_event_is_a_no_op_but_identity_drift_is_refused() {
    let exact_runner = runner(41);
    let state = attempt()
        .begin_provisioning()
        .unwrap()
        .begin_registration()
        .unwrap()
        .record_runner_ready(&exact_runner)
        .unwrap()
        .record_running(&exact_runner, job("job-one"))
        .unwrap();
    let mut duplicate = input(
        &state,
        ExactObjectObservation::Matching,
        ScaleSetRunnerObservation::IdleReady {
            runner: exact_runner.clone(),
        },
    );
    duplicate.job_event = Some(ScaleSetJobEvent::Started {
        runner: exact_runner.clone(),
        job_id: job("job-one"),
    });
    assert_eq!(
        reconcile_attempt(duplicate).unwrap(),
        DisposableWorkerAction::Wait
    );

    let mut drift = input(
        &state,
        ExactObjectObservation::Matching,
        ScaleSetRunnerObservation::IdleReady {
            runner: exact_runner.clone(),
        },
    );
    drift.job_event = Some(ScaleSetJobEvent::Started {
        runner: exact_runner,
        job_id: job("job-two"),
    });
    assert_eq!(
        reconcile_attempt(drift).unwrap_err().code(),
        "github_job_identity_drift"
    );
}

#[test]
fn late_duplicate_event_does_not_reverse_cleanup() {
    let exact_runner = runner(41);
    let state = attempt()
        .record_terminal(Some(&exact_runner), job("job-late"), result("succeeded"))
        .unwrap()
        .begin_cleanup()
        .unwrap();
    let mut duplicate = input(
        &state,
        ExactObjectObservation::Matching,
        ScaleSetRunnerObservation::RegistrationOnly {
            runner: exact_runner.clone(),
        },
    );
    duplicate.job_event = Some(ScaleSetJobEvent::Completed {
        runner: Some(exact_runner),
        job_id: job("job-late"),
        result: result("succeeded"),
    });
    assert_eq!(
        reconcile_attempt(duplicate).unwrap(),
        DisposableWorkerAction::DestroyVm
    );
}

#[test]
fn public_action_json_is_bounded_and_contains_no_private_material() {
    let state = attempt();
    let action = reconcile_attempt(input(
        &state,
        ExactObjectObservation::Unknown,
        ScaleSetRunnerObservation::Unknown,
    ))
    .unwrap();
    let encoded = serde_json::to_string(&action).unwrap();
    assert!(encoded.len() < 512);
    assert!(!encoded.contains("token"));
    assert_eq!(
        action,
        persist(DisposableAttemptCatalogAction::BeginProvisioning)
    );
    let observing = DisposableWorkerAction::Observe {
        target: DisposableWorkerObservationTarget::Vm,
    };
    assert!(serde_json::to_string(&observing).unwrap().contains("vm"));
}
