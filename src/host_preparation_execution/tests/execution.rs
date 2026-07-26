use crate::lane_command::LaneCommandKind;

use super::super::{HostPreparationExecutionDisposition, execute_confirmed_host_preparation};
use super::fixture::{confirmed, mapping_proposal, migration_proposal, package_proposal};
use super::recording::{RecordingCheckpoint, RecordingRunner};

#[test]
fn confirmed_package_phase_is_durable_and_completed() {
    let mut runner = RecordingRunner::default();
    let mut checkpoint = RecordingCheckpoint::default();
    let report = execute_confirmed_host_preparation(
        confirmed(package_proposal()),
        &mut runner,
        &mut checkpoint,
    )
    .expect("execution report");
    assert_eq!(
        report.disposition,
        HostPreparationExecutionDisposition::Completed
    );
    assert!(!report.fresh_observation_required());
    assert_eq!(report.phase_id, "host-preparation-root-phase");
    assert_eq!(runner.commands, [LaneCommandKind::AptInstall]);
    assert_eq!(checkpoint.calls, 3);
    assert!(report.journal.completed());
    assert!(report.continuation_barriers.is_empty());
    assert!(report.deferred_actions.is_empty());
}

#[test]
fn mapping_phase_stops_at_barrier_without_migration() {
    let mut runner = RecordingRunner::default();
    let mut checkpoint = RecordingCheckpoint::default();
    let report = execute_confirmed_host_preparation(
        confirmed(mapping_proposal()),
        &mut runner,
        &mut checkpoint,
    )
    .expect("execution report");
    assert_eq!(
        report.disposition,
        HostPreparationExecutionDisposition::FreshObservationRequired
    );
    assert!(report.fresh_observation_required());
    assert_eq!(runner.commands, [LaneCommandKind::EnsureSubordinateUids]);
    assert_eq!(report.continuation_barriers.len(), 1);
    assert_eq!(report.deferred_actions.len(), 1);
    assert!(
        !runner
            .commands
            .contains(&LaneCommandKind::RunnerPodmanMigrate)
    );
}

#[test]
fn freshly_planned_migration_phase_executes_only_runner_migration() {
    let mut runner = RecordingRunner::default();
    let mut checkpoint = RecordingCheckpoint::default();
    let report = execute_confirmed_host_preparation(
        confirmed(migration_proposal()),
        &mut runner,
        &mut checkpoint,
    )
    .expect("execution report");
    assert_eq!(
        report.disposition,
        HostPreparationExecutionDisposition::Completed
    );
    assert_eq!(report.phase_id, "host-preparation-runner-migration-phase");
    assert_eq!(runner.commands, [LaneCommandKind::RunnerPodmanMigrate]);
    assert!(report.continuation_barriers.is_empty());
    assert!(report.deferred_actions.is_empty());
}
