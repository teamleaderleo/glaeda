use smolrunner::disposable_worker_reconciler::{
    CapacityClaimId, DisposableAttemptId, DisposableAttemptPhase, DisposableHostBudget,
    DisposableHostUsage, DisposableVmId, DisposableWorkerAction, DisposableWorkerObservationTarget,
    DisposableWorkerReconcileInput, DisposableWorkerResources, ExactObjectObservation,
    GitHubJobConclusion, GitHubJobId, ScaleSetDemand, ScaleSetRunnerId, ScaleSetRunnerObservation,
    plan_capacity, reconcile_attempt,
};
use smolrunner::execution_admission::EpochMillis;

fn time(value: u64) -> EpochMillis {
    EpochMillis::new(value).unwrap()
}

fn resources(cpu: u32, memory: u64, disk: u64) -> DisposableWorkerResources {
    DisposableWorkerResources::new(cpu, memory, disk).unwrap()
}

fn attempt() -> smolrunner::disposable_worker_reconciler::DisposableAttempt {
    smolrunner::disposable_worker_reconciler::DisposableAttempt::reserved(
        DisposableAttemptId::parse("attempt-1").unwrap(),
        CapacityClaimId::parse("claim-1").unwrap(),
        DisposableVmId::parse("vm-1").unwrap(),
        ScaleSetRunnerId::parse("runner-1").unwrap(),
        time(10_000),
    )
}

fn input<'a>(
    attempt: &'a smolrunner::disposable_worker_reconciler::DisposableAttempt,
    vm: ExactObjectObservation,
    runner: ScaleSetRunnerObservation,
) -> DisposableWorkerReconcileInput<'a> {
    DisposableWorkerReconcileInput {
        now: time(1_000),
        attempt,
        vm,
        runner,
        capacity_reserved: true,
        cancellation_requested: false,
    }
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
    let unused = DisposableHostUsage::zero();
    let plan = plan_capacity(
        time(1_000),
        demand,
        disk_limited,
        unused,
        resources(4_000, 8_000, 100_000),
    )
    .unwrap();
    assert_eq!(plan.advertised_max_capacity(), 1);
    assert_eq!(plan.additional_workers(), 1);
}

#[test]
fn stale_or_inconsistent_demand_and_overcommitted_usage_fail_closed() {
    let demand = ScaleSetDemand::new(2, 3, time(900), time(1_100)).unwrap_err();
    assert_eq!(demand.code(), "invalid_scale_set_demand");

    let demand = ScaleSetDemand::new(2, 1, time(900), time(950)).unwrap();
    let budget = DisposableHostBudget::new(1, resources(4_000, 8_000, 100_000)).unwrap();
    let usage = DisposableHostUsage::zero();
    assert_eq!(
        plan_capacity(
            time(1_000),
            demand,
            budget,
            usage,
            resources(4_000, 8_000, 100_000)
        )
        .unwrap_err()
        .code(),
        "stale_scale_set_demand"
    );
}

#[test]
fn happy_path_is_checkpointed_before_each_external_mutation() {
    let mut state = attempt();
    let action = reconcile_attempt(input(
        &state,
        ExactObjectObservation::Absent,
        ScaleSetRunnerObservation::Absent,
    ))
    .unwrap();
    assert_eq!(
        action,
        DisposableWorkerAction::Checkpoint {
            phase: DisposableAttemptPhase::Provisioning
        }
    );
    state = state.checkpoint(&action).unwrap();

    assert_eq!(
        reconcile_attempt(input(
            &state,
            ExactObjectObservation::Absent,
            ScaleSetRunnerObservation::Absent
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
    state = state.checkpoint(&action).unwrap();
    assert_eq!(state.phase(), DisposableAttemptPhase::Registering);
    assert_eq!(
        reconcile_attempt(input(
            &state,
            ExactObjectObservation::Matching,
            ScaleSetRunnerObservation::Absent
        ))
        .unwrap(),
        DisposableWorkerAction::GenerateJitAndStartRunner
    );

    let action = reconcile_attempt(input(
        &state,
        ExactObjectObservation::Matching,
        ScaleSetRunnerObservation::Idle,
    ))
    .unwrap();
    state = state.checkpoint(&action).unwrap();
    let job = GitHubJobId::new(42).unwrap();
    let action = reconcile_attempt(input(
        &state,
        ExactObjectObservation::Matching,
        ScaleSetRunnerObservation::Assigned { github_job_id: job },
    ))
    .unwrap();
    state = state.checkpoint(&action).unwrap();
    assert_eq!(state.github_job_id(), Some(job));

    let action = reconcile_attempt(input(
        &state,
        ExactObjectObservation::Matching,
        ScaleSetRunnerObservation::Running { github_job_id: job },
    ))
    .unwrap();
    state = state.checkpoint(&action).unwrap();
    let action = reconcile_attempt(input(
        &state,
        ExactObjectObservation::Matching,
        ScaleSetRunnerObservation::Terminal {
            github_job_id: job,
            conclusion: GitHubJobConclusion::Success,
        },
    ))
    .unwrap();
    state = state.checkpoint(&action).unwrap();
    assert_eq!(state.phase(), DisposableAttemptPhase::Terminal);
}

#[test]
fn terminal_cleanup_destroys_vm_before_deleting_runner_and_releasing_capacity() {
    let job = GitHubJobId::new(42).unwrap();
    let mut state = attempt();
    for action in [
        DisposableWorkerAction::Checkpoint {
            phase: DisposableAttemptPhase::Provisioning,
        },
        DisposableWorkerAction::Checkpoint {
            phase: DisposableAttemptPhase::Registering,
        },
        DisposableWorkerAction::Checkpoint {
            phase: DisposableAttemptPhase::Waiting,
        },
        DisposableWorkerAction::RecordTerminal {
            github_job_id: job,
            conclusion: GitHubJobConclusion::Failure,
        },
        DisposableWorkerAction::Checkpoint {
            phase: DisposableAttemptPhase::Destroying,
        },
    ] {
        state = state.checkpoint(&action).unwrap();
    }
    assert_eq!(
        reconcile_attempt(input(
            &state,
            ExactObjectObservation::Matching,
            ScaleSetRunnerObservation::Terminal {
                github_job_id: job,
                conclusion: GitHubJobConclusion::Failure
            }
        ))
        .unwrap(),
        DisposableWorkerAction::DestroyVm
    );
    let action = reconcile_attempt(input(
        &state,
        ExactObjectObservation::Absent,
        ScaleSetRunnerObservation::Terminal {
            github_job_id: job,
            conclusion: GitHubJobConclusion::Failure,
        },
    ))
    .unwrap();
    state = state.checkpoint(&action).unwrap();
    assert_eq!(
        reconcile_attempt(input(
            &state,
            ExactObjectObservation::Absent,
            ScaleSetRunnerObservation::Idle
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
    state = state.checkpoint(&action).unwrap();
    assert_eq!(
        reconcile_attempt(input(
            &state,
            ExactObjectObservation::Absent,
            ScaleSetRunnerObservation::Absent
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
    state = state.checkpoint(&action).unwrap();
    let mut complete = input(
        &state,
        ExactObjectObservation::Absent,
        ScaleSetRunnerObservation::Absent,
    );
    complete.capacity_reserved = false;
    assert_eq!(
        reconcile_attempt(complete).unwrap(),
        DisposableWorkerAction::NoOp
    );
}

#[test]
fn cancellation_expiry_runner_loss_and_orphan_vm_all_converge_to_cleanup() {
    let reserved_attempt = attempt();
    let mut reserved = input(
        &reserved_attempt,
        ExactObjectObservation::Absent,
        ScaleSetRunnerObservation::Absent,
    );
    reserved.cancellation_requested = true;
    assert_eq!(
        reconcile_attempt(reserved).unwrap(),
        DisposableWorkerAction::Checkpoint {
            phase: DisposableAttemptPhase::Destroying
        }
    );

    let mut provisioning = attempt();
    provisioning = provisioning
        .checkpoint(&DisposableWorkerAction::Checkpoint {
            phase: DisposableAttemptPhase::Provisioning,
        })
        .unwrap();
    let mut expired = input(
        &provisioning,
        ExactObjectObservation::Matching,
        ScaleSetRunnerObservation::Absent,
    );
    expired.now = time(10_001);
    assert_eq!(
        reconcile_attempt(expired).unwrap(),
        DisposableWorkerAction::Checkpoint {
            phase: DisposableAttemptPhase::Destroying
        }
    );

    let mut waiting = provisioning
        .checkpoint(&DisposableWorkerAction::Checkpoint {
            phase: DisposableAttemptPhase::Registering,
        })
        .unwrap();
    waiting = waiting
        .checkpoint(&DisposableWorkerAction::Checkpoint {
            phase: DisposableAttemptPhase::Waiting,
        })
        .unwrap();
    assert_eq!(
        reconcile_attempt(input(
            &waiting,
            ExactObjectObservation::Matching,
            ScaleSetRunnerObservation::Absent
        ))
        .unwrap(),
        DisposableWorkerAction::Checkpoint {
            phase: DisposableAttemptPhase::Destroying
        }
    );
    assert_eq!(
        reconcile_attempt(input(
            &waiting,
            ExactObjectObservation::Absent,
            ScaleSetRunnerObservation::Idle
        ))
        .unwrap(),
        DisposableWorkerAction::Checkpoint {
            phase: DisposableAttemptPhase::Deregistering
        }
    );
}

#[test]
fn unknown_and_conflicting_external_state_never_authorize_mutation() {
    let mut state = attempt();
    state = state
        .checkpoint(&DisposableWorkerAction::Checkpoint {
            phase: DisposableAttemptPhase::Provisioning,
        })
        .unwrap();
    assert_eq!(
        reconcile_attempt(input(
            &state,
            ExactObjectObservation::Unknown,
            ScaleSetRunnerObservation::Unknown
        ))
        .unwrap(),
        DisposableWorkerAction::Observe {
            target: DisposableWorkerObservationTarget::Vm
        }
    );
    assert_eq!(
        reconcile_attempt(input(
            &state,
            ExactObjectObservation::Conflicting,
            ScaleSetRunnerObservation::Absent
        ))
        .unwrap(),
        DisposableWorkerAction::Blocked {
            code: "conflicting_vm_identity"
        }
    );
}

#[test]
fn an_attempt_can_never_change_the_actual_assigned_job() {
    let mut state = attempt();
    state = state
        .checkpoint(&DisposableWorkerAction::Checkpoint {
            phase: DisposableAttemptPhase::Provisioning,
        })
        .unwrap();
    state = state
        .checkpoint(&DisposableWorkerAction::Checkpoint {
            phase: DisposableAttemptPhase::Registering,
        })
        .unwrap();
    state = state
        .checkpoint(&DisposableWorkerAction::RecordAssigned {
            github_job_id: GitHubJobId::new(7).unwrap(),
        })
        .unwrap();
    let error = state
        .checkpoint(&DisposableWorkerAction::RecordRunning {
            github_job_id: GitHubJobId::new(8).unwrap(),
        })
        .unwrap_err();
    assert_eq!(error.code(), "github_job_identity_drift");
}

#[test]
fn duplicate_and_out_of_order_job_messages_do_not_regress_phase() {
    let job = GitHubJobId::new(7).unwrap();
    let mut state = attempt();
    state = state
        .checkpoint(&DisposableWorkerAction::Checkpoint {
            phase: DisposableAttemptPhase::Provisioning,
        })
        .unwrap();
    state = state
        .checkpoint(&DisposableWorkerAction::Checkpoint {
            phase: DisposableAttemptPhase::Registering,
        })
        .unwrap();
    state = state
        .checkpoint(&DisposableWorkerAction::RecordAssigned { github_job_id: job })
        .unwrap();
    assert_eq!(
        reconcile_attempt(input(
            &state,
            ExactObjectObservation::Matching,
            ScaleSetRunnerObservation::Assigned { github_job_id: job }
        ))
        .unwrap(),
        DisposableWorkerAction::Wait
    );
    state = state
        .checkpoint(&DisposableWorkerAction::RecordRunning { github_job_id: job })
        .unwrap();
    assert_eq!(
        reconcile_attempt(input(
            &state,
            ExactObjectObservation::Matching,
            ScaleSetRunnerObservation::Assigned { github_job_id: job }
        ))
        .unwrap(),
        DisposableWorkerAction::Wait
    );
}

#[test]
fn lost_capacity_reservation_never_starts_or_retains_a_worker() {
    let state = attempt();
    let mut without_reservation = input(
        &state,
        ExactObjectObservation::Absent,
        ScaleSetRunnerObservation::Absent,
    );
    without_reservation.capacity_reserved = false;
    assert_eq!(
        reconcile_attempt(without_reservation).unwrap(),
        DisposableWorkerAction::Checkpoint {
            phase: DisposableAttemptPhase::Destroying
        }
    );
}

#[test]
fn public_json_contains_only_bounded_identifiers_and_actions() {
    let state = attempt();
    let json = serde_json::to_string(&state).unwrap();
    assert!(json.contains("attempt-1"));
    assert!(!json.contains("token"));
    assert!(!json.contains("/Users/"));
}
