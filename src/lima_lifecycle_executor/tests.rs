use std::cell::Cell;

use super::*;

fn revision(value: u64) -> HostBrokerStateRevision {
    HostBrokerStateRevision::new(value).expect("revision")
}

fn generation(value: u64) -> PersonalWorkerQueueGeneration {
    PersonalWorkerQueueGeneration::new(value).expect("generation")
}

fn epoch(value: u64) -> EpochMillis {
    EpochMillis::new(value).expect("epoch")
}

fn assert_guard_refuses_without_execution(
    accepted_revision: HostBrokerStateRevision,
    accepted_generation: PersonalWorkerQueueGeneration,
    current_revision: HostBrokerStateRevision,
    current_generation: PersonalWorkerQueueGeneration,
    observed_at: EpochMillis,
    execution_seconds: u64,
    expected: LimaLifecycleExecutionRefusalCode,
) {
    let calls = Cell::new(0_u8);
    let result = guard_then(
        accepted_revision,
        accepted_generation,
        current_revision,
        current_generation,
        observed_at,
        execution_seconds,
        || {
            calls.set(calls.get() + 1);
            Ok::<(), inner::LimaLifecycleExecutionFailure>(())
        },
    );
    let error = result.expect_err("guard refusal");
    assert_eq!(error.code, expected);
    assert_eq!(calls.get(), 0);
    assert!(error.private_evidence().commands().is_empty());
}

#[test]
fn durable_state_revision_drift_refuses_before_inner_execution() {
    assert_guard_refuses_without_execution(
        revision(7),
        generation(11),
        revision(8),
        generation(11),
        epoch(100_000),
        100,
        LimaLifecycleExecutionRefusalCode::StateMismatch,
    );
}

#[test]
fn durable_queue_generation_drift_refuses_before_inner_execution() {
    assert_guard_refuses_without_execution(
        revision(7),
        generation(11),
        revision(7),
        generation(12),
        epoch(100_000),
        100,
        LimaLifecycleExecutionRefusalCode::GenerationMismatch,
    );
}

#[test]
fn stale_lifecycle_observation_refuses_before_inner_execution() {
    assert_guard_refuses_without_execution(
        revision(7),
        generation(11),
        revision(7),
        generation(11),
        epoch(100_000),
        401,
        LimaLifecycleExecutionRefusalCode::StaleObservation,
    );
}

#[test]
fn future_lifecycle_observation_refuses_before_inner_execution() {
    assert_guard_refuses_without_execution(
        revision(7),
        generation(11),
        revision(7),
        generation(11),
        epoch(101_000),
        100,
        LimaLifecycleExecutionRefusalCode::StaleObservation,
    );
}

#[test]
fn exact_current_authority_and_fresh_lifecycle_reach_inner_execution() {
    let calls = Cell::new(0_u8);
    guard_then(
        revision(7),
        generation(11),
        revision(7),
        generation(11),
        epoch(100_000),
        100,
        || {
            calls.set(calls.get() + 1);
            Ok::<(), inner::LimaLifecycleExecutionFailure>(())
        },
    )
    .expect("guard success");
    assert_eq!(calls.get(), 1);
}
