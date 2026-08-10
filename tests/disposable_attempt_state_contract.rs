use smolrunner::disposable_attempt_state::{
    DISPOSABLE_ATTEMPT_STATE_SCHEMA_VERSION, DisposableAttemptState,
    decode_disposable_attempt_state, encode_disposable_attempt_state,
};
use smolrunner::disposable_worker_reconciler::{
    CapacityClaimId, DisposableAttemptId, DisposableAttemptPhase, DisposableVmId,
};
use smolrunner::execution_admission::EpochMillis;
use smolrunner::github_scale_set_protocol::{
    ScaleSetJobId, ScaleSetJobResult, ScaleSetRunnerId, ScaleSetRunnerName, ScaleSetRunnerReference,
};

fn reserved() -> DisposableAttemptState {
    DisposableAttemptState::reserved(
        DisposableAttemptId::parse("attempt-1").unwrap(),
        CapacityClaimId::parse("claim-1").unwrap(),
        DisposableVmId::parse("vm-attempt-1").unwrap(),
        ScaleSetRunnerName::parse("smol-attempt-1").unwrap(),
        EpochMillis::new(50_000).unwrap(),
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

#[test]
fn registration_and_listener_readiness_are_distinct_durable_checkpoints() {
    let registering = reserved()
        .begin_provisioning()
        .unwrap()
        .begin_registration()
        .unwrap();
    let registered = registering.record_registration(&runner(41)).unwrap();

    assert_eq!(registered.phase(), DisposableAttemptPhase::Registering);
    assert_eq!(registered.runner_id().unwrap().get(), 41);
    assert!(registered.github_job_id().is_none());

    let ready = registered.record_runner_ready(&runner(41)).unwrap();
    assert_eq!(ready.phase(), DisposableAttemptPhase::Waiting);
    assert_eq!(ready.runner_id().unwrap().get(), 41);
    assert!(ready.revision().get() > registered.revision().get());
}

#[test]
fn job_assignment_does_not_bind_a_runner_before_job_started() {
    let assigned = reserved()
        .begin_provisioning()
        .unwrap()
        .begin_registration()
        .unwrap()
        .record_assigned(job("job_opaque-7"))
        .unwrap();

    assert_eq!(assigned.phase(), DisposableAttemptPhase::Assigned);
    assert!(assigned.runner_id().is_none());
    assert_eq!(assigned.github_job_id().unwrap().as_str(), "job_opaque-7");

    let running = assigned
        .record_running(&runner(9), job("job_opaque-7"))
        .unwrap();
    assert_eq!(running.phase(), DisposableAttemptPhase::Running);
    assert_eq!(running.runner_id().unwrap().get(), 9);
    assert_eq!(running.github_job_id().unwrap().as_str(), "job_opaque-7");
}

#[test]
fn preassignment_completion_can_be_terminal_without_runner_identity() {
    let terminal = reserved()
        .record_terminal(
            None,
            job("cancel-before-runner"),
            ScaleSetJobResult::parse("canceled").unwrap(),
        )
        .unwrap();

    assert_eq!(terminal.phase(), DisposableAttemptPhase::Terminal);
    assert!(terminal.runner_id().is_none());
    assert_eq!(
        terminal.github_job_id().unwrap().as_str(),
        "cancel-before-runner"
    );
    assert_eq!(terminal.result().unwrap().as_str(), "canceled");
}

#[test]
fn unknown_completion_result_still_reaches_cleanup() {
    let terminal = reserved()
        .record_terminal(
            None,
            job("job-future-result"),
            ScaleSetJobResult::parse("future-service-result").unwrap(),
        )
        .unwrap();
    let destroying = terminal.begin_cleanup().unwrap();
    let deregistering = destroying
        .advance_cleanup(DisposableAttemptPhase::Deregistering)
        .unwrap();
    let releasing = deregistering
        .advance_cleanup(DisposableAttemptPhase::Releasing)
        .unwrap();
    let complete = releasing
        .advance_cleanup(DisposableAttemptPhase::Complete)
        .unwrap();

    assert_eq!(complete.phase(), DisposableAttemptPhase::Complete);
    assert_eq!(complete.result().unwrap().as_str(), "future-service-result");
}

#[test]
fn exact_runner_and_job_identity_drift_fails_closed() {
    let registered = reserved()
        .begin_provisioning()
        .unwrap()
        .begin_registration()
        .unwrap()
        .record_registration(&runner(7))
        .unwrap();

    assert_eq!(
        registered
            .record_runner_ready(&runner(8))
            .unwrap_err()
            .code(),
        "identity_drift"
    );

    let running = registered
        .record_runner_ready(&runner(7))
        .unwrap()
        .record_assigned(job("job-a"))
        .unwrap()
        .record_running(&runner(7), job("job-a"))
        .unwrap();
    assert_eq!(
        running
            .record_terminal(
                Some(&runner(7)),
                job("job-b"),
                ScaleSetJobResult::parse("succeeded").unwrap(),
            )
            .unwrap_err()
            .code(),
        "identity_drift"
    );
}

#[test]
fn canonical_codec_round_trips_exact_state_and_revision() {
    let state = reserved()
        .begin_provisioning()
        .unwrap()
        .begin_registration()
        .unwrap()
        .record_registration(&runner(77))
        .unwrap();
    let encoded = encode_disposable_attempt_state(&state).unwrap();
    let decoded = decode_disposable_attempt_state(&encoded).unwrap();

    assert_eq!(decoded, state);
    assert_eq!(
        decoded.schema_version(),
        DISPOSABLE_ATTEMPT_STATE_SCHEMA_VERSION
    );
    assert_eq!(decoded.revision(), state.revision());
    assert_eq!(decoded.runner_name().as_str(), "smol-attempt-1");
    assert_eq!(decoded.runner_id().unwrap().get(), 77);
}

#[test]
fn codec_rejects_future_versions_unknown_fields_and_inconsistent_phase_evidence() {
    let base = encode_disposable_attempt_state(&reserved()).unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&base).unwrap();

    value["schema_version"] = serde_json::json!(2);
    let future = serde_json::to_vec(&value).unwrap();
    assert_eq!(
        decode_disposable_attempt_state(&future).unwrap_err().code(),
        "version_incompatible"
    );

    value["schema_version"] = serde_json::json!(1);
    value["unexpected"] = serde_json::json!(true);
    let unknown = serde_json::to_vec(&value).unwrap();
    assert_eq!(
        decode_disposable_attempt_state(&unknown)
            .unwrap_err()
            .code(),
        "invalid_document"
    );

    let mut inconsistent: serde_json::Value = serde_json::from_slice(&base).unwrap();
    inconsistent["phase"] = serde_json::json!("running");
    inconsistent["runner_id"] = serde_json::json!(12);
    let inconsistent = serde_json::to_vec(&inconsistent).unwrap();
    assert_eq!(
        decode_disposable_attempt_state(&inconsistent)
            .unwrap_err()
            .code(),
        "invalid_document"
    );
}
