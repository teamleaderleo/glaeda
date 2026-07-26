use crate::durable_journal::{JournalCheckpoint, JournalCheckpointFailure};
use crate::durable_lane_execution::LaneCommandRunner;
use crate::journal::{ActionFailure, ActionReceipt, ExecutionJournal};
use crate::lane_command::{LaneCommand, LaneCommandKind};

#[derive(Default)]
pub(super) struct RecordingRunner {
    pub(super) commands: Vec<LaneCommandKind>,
    pub(super) fail_on: Option<LaneCommandKind>,
}

impl LaneCommandRunner for RecordingRunner {
    fn run(&mut self, command: &LaneCommand) -> Result<ActionReceipt, ActionFailure> {
        let kind = command.kind();
        self.commands.push(kind);
        if self.fail_on == Some(kind) {
            Err(ActionFailure::public(
                "test_failure",
                "bounded execution failure",
            ))
        } else {
            Ok(ActionReceipt::public("bounded execution receipt"))
        }
    }
}

#[derive(Default)]
pub(super) struct RecordingCheckpoint {
    pub(super) calls: usize,
    pub(super) fail_on_call: Option<usize>,
    pub(super) snapshots: Vec<ExecutionJournal>,
}

impl JournalCheckpoint for RecordingCheckpoint {
    fn checkpoint(&mut self, journal: &ExecutionJournal) -> Result<(), JournalCheckpointFailure> {
        self.calls += 1;
        if self.fail_on_call == Some(self.calls) {
            return Err(JournalCheckpointFailure::public(
                "bounded checkpoint failure",
            ));
        }
        self.snapshots.push(journal.clone());
        Ok(())
    }
}
