use crate::durable_journal::JournalCheckpointPhase;
use crate::host_preparation_command::{HostPreparationCommandDisposition, decide_host_preparation};
use crate::journal::ActionOutcome;
use crate::lane_command::LaneCommandKind;

use super::super::{
    HostPreparationExecutionDisposition, HostPreparationExecutionErrorKind,
    execute_confirmed_host_preparation, render_human,
};
use super::fixture::{confirmed, package_proposal};
use super::recording::{RecordingCheckpoint, RecordingRunner};

#[test]
fn unconfirmed_decision_never_reaches_runner_or_checkpoint() {
    let decision = decide_host_preparation(package_proposal(), None).expect("decision");
    assert_eq!(
        decision.disposition(),
        HostPreparationCommandDisposition::ConfirmationRequired
    );
    let mut runner = RecordingRunner::default();
    let mut checkpoint = RecordingCheckpoint::default();
    let error = execute_confirmed_host_preparation(decision, &mut runner, &mut checkpoint)
        .expect_err("confirmation must be required");
    assert_eq!(
        error.kind(),
        HostPreparationExecutionErrorKind::DecisionNotConfirmed
    );
    assert!(runner.commands.is_empty());
    assert_eq!(checkpoint.calls, 0);
}

#[test]
fn action_failure_returns_terminal_public_journal() {
    let mut runner = RecordingRunner {
        commands: Vec::new(),
        fail_on: Some(LaneCommandKind::AptInstall),
    };
    let mut checkpoint = RecordingCheckpoint::default();
    let report = execute_confirmed_host_preparation(
        confirmed(package_proposal()),
        &mut runner,
        &mut checkpoint,
    )
    .expect("execution report");
    assert_eq!(
        report.disposition,
        HostPreparationExecutionDisposition::ActionFailed
    );
    assert!(report.fresh_observation_required());
    assert_eq!(report.journal.records[0].outcome, ActionOutcome::Failed);
    assert_eq!(
        report.journal.stopped_after.as_deref(),
        Some("install-debian-host-prerequisites")
    );
    assert_eq!(checkpoint.calls, 3);
}

#[test]
fn checkpoint_failure_preserves_last_durable_and_attempted_snapshots() {
    let mut runner = RecordingRunner::default();
    let mut checkpoint = RecordingCheckpoint {
        calls: 0,
        fail_on_call: Some(2),
        snapshots: Vec::new(),
    };
    let error = execute_confirmed_host_preparation(
        confirmed(package_proposal()),
        &mut runner,
        &mut checkpoint,
    )
    .expect_err("checkpoint failure expected");
    assert_eq!(
        error.kind(),
        HostPreparationExecutionErrorKind::JournalCheckpoint
    );
    let durable = error.checkpoint().expect("checkpoint evidence");
    assert_eq!(durable.phase(), JournalCheckpointPhase::BeforeExecute);
    assert_eq!(
        durable.last_durable().expect("last durable").records[0].outcome,
        ActionOutcome::Pending
    );
    assert_eq!(
        durable.attempted().records[0].outcome,
        ActionOutcome::Executing
    );
    assert!(runner.commands.is_empty());
}

#[test]
fn execution_output_excludes_private_command_material_and_raw_evidence() {
    let mut runner = RecordingRunner::default();
    let mut checkpoint = RecordingCheckpoint::default();
    let report = execute_confirmed_host_preparation(
        confirmed(package_proposal()),
        &mut runner,
        &mut checkpoint,
    )
    .expect("execution report");
    let json = serde_json::to_string(&report).expect("serialize report");
    let human = render_human(&report);
    for forbidden in [
        "raw execution sentinel",
        "durable_plan",
        "\"spec\"",
        "\"program\"",
        "\"arguments\"",
        "\"environment\"",
        "/usr/bin/apt-get",
    ] {
        assert!(!json.contains(forbidden), "JSON leaked {forbidden}");
        assert!(
            !human.contains(forbidden),
            "human output leaked {forbidden}"
        );
    }
}
