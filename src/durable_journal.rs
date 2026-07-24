use std::fmt;

use serde::Serialize;

use crate::journal::{
    ActionOutcome, ActionReceipt, ExecutionJournal, JOURNAL_SCHEMA_VERSION, JournalRecord,
    MutationExecutor, PlanValidationError, PlannedMutation, RollbackClass, validate_plan,
};
use crate::journal_document::JournalStateDocument;
use crate::state::{InstallationId, JournalId};
use crate::state_store::{StateRecord, StateStore};

/// The durable boundary being crossed by one journal checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalCheckpointPhase {
    Initial,
    BeforeExecute,
    AfterExecute,
    BeforeRollback,
    AfterRollback,
}

impl JournalCheckpointPhase {
    const fn label(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::BeforeExecute => "before_execute",
            Self::AfterExecute => "after_execute",
            Self::BeforeRollback => "before_rollback",
            Self::AfterRollback => "after_rollback",
        }
    }
}

/// Public, redacted failure returned by a journal persistence boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JournalCheckpointFailure {
    public_message: String,
}

impl JournalCheckpointFailure {
    #[must_use]
    pub fn public(message: impl Into<String>) -> Self {
        Self {
            public_message: message.into(),
        }
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.public_message
    }
}

impl fmt::Display for JournalCheckpointFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.public_message)
    }
}

impl std::error::Error for JournalCheckpointFailure {}

/// Narrow persistence boundary for one complete execution-journal snapshot.
pub trait JournalCheckpoint {
    fn checkpoint(&mut self, journal: &ExecutionJournal) -> Result<(), JournalCheckpointFailure>;
}

/// A failed checkpoint retains both the last durable snapshot and the attempted next snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DurableCheckpointError {
    phase: JournalCheckpointPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    action_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_durable: Option<ExecutionJournal>,
    attempted: ExecutionJournal,
    failure: JournalCheckpointFailure,
}

impl DurableCheckpointError {
    #[must_use]
    pub fn phase(&self) -> JournalCheckpointPhase {
        self.phase
    }

    #[must_use]
    pub fn action_id(&self) -> Option<&str> {
        self.action_id.as_deref()
    }

    #[must_use]
    pub fn last_durable(&self) -> Option<&ExecutionJournal> {
        self.last_durable.as_ref()
    }

    #[must_use]
    pub fn attempted(&self) -> &ExecutionJournal {
        &self.attempted
    }

    #[must_use]
    pub fn failure(&self) -> &JournalCheckpointFailure {
        &self.failure
    }
}

impl fmt::Display for DurableCheckpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "journal checkpoint failed during {}",
            self.phase.label()
        )?;
        if let Some(action_id) = &self.action_id {
            write!(formatter, " for action {action_id:?}")?;
        }
        write!(formatter, ": {}", self.failure)
    }
}

impl std::error::Error for DurableCheckpointError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DurableExecutionError {
    Plan(PlanValidationError),
    Checkpoint(DurableCheckpointError),
}

impl fmt::Display for DurableExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Plan(error) => error.fmt(formatter),
            Self::Checkpoint(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for DurableExecutionError {}

/// Execute a validated mutation plan while checkpointing every uncertain boundary.
///
/// The initial all-pending journal is persisted before any executor call. Each action is then
/// persisted as `executing` before execution and as a terminal outcome afterwards. Rollback uses the
/// same two-phase pattern with `rollback_in_progress`. A checkpoint failure stops immediately: no
/// later action or rollback is attempted, and the returned error distinguishes the last durable
/// snapshot from the attempted next snapshot.
///
/// # Errors
///
/// Returns `Plan` before persistence or mutation when the plan is invalid. Returns `Checkpoint`
/// immediately when any journal snapshot cannot be persisted.
pub fn execute_plan_durably(
    actions: Vec<PlannedMutation>,
    executor: &mut impl MutationExecutor,
    checkpoint: &mut impl JournalCheckpoint,
    allow_irreversible: bool,
) -> Result<ExecutionJournal, DurableExecutionError> {
    validate_plan(&actions)
        .map_err(|problems| DurableExecutionError::Plan(PlanValidationError { problems }))?;

    let mut journal = pending_journal(actions);
    let mut last_durable = None;

    if let Some(index) = first_unconfirmed_irreversible(&journal, allow_irreversible) {
        journal.records[index].outcome = ActionOutcome::Skipped;
        journal.records[index].message =
            Some("irreversible action requires explicit confirmation".to_owned());
        journal.stopped_after = Some(journal.records[index].action.id.clone());
        persist_checkpoint(
            checkpoint,
            &journal,
            &mut last_durable,
            JournalCheckpointPhase::Initial,
            Some(index),
        )?;
        return Ok(journal);
    }

    persist_checkpoint(
        checkpoint,
        &journal,
        &mut last_durable,
        JournalCheckpointPhase::Initial,
        None,
    )?;

    let mut completed = Vec::<(usize, ActionReceipt)>::new();
    for index in 0..journal.records.len() {
        journal.records[index].outcome = ActionOutcome::Executing;
        journal.records[index].message = None;
        persist_checkpoint(
            checkpoint,
            &journal,
            &mut last_durable,
            JournalCheckpointPhase::BeforeExecute,
            Some(index),
        )?;

        match executor.execute(&journal.records[index].action) {
            Ok(receipt) => {
                journal.records[index].outcome = ActionOutcome::Completed;
                journal.records[index].message = Some(receipt.summary().to_owned());
                persist_checkpoint(
                    checkpoint,
                    &journal,
                    &mut last_durable,
                    JournalCheckpointPhase::AfterExecute,
                    Some(index),
                )?;
                completed.push((index, receipt));
            }
            Err(failure) => {
                journal.records[index].outcome = ActionOutcome::Failed;
                journal.records[index].message =
                    Some(format!("{}: {}", failure.code(), failure.message()));
                journal.stopped_after = Some(journal.records[index].action.id.clone());
                persist_checkpoint(
                    checkpoint,
                    &journal,
                    &mut last_durable,
                    JournalCheckpointPhase::AfterExecute,
                    Some(index),
                )?;
                rollback_completed_durably(
                    &mut journal,
                    executor,
                    checkpoint,
                    &completed,
                    &mut last_durable,
                )?;
                break;
            }
        }
    }

    Ok(journal)
}

fn pending_journal(actions: Vec<PlannedMutation>) -> ExecutionJournal {
    ExecutionJournal {
        schema_version: JOURNAL_SCHEMA_VERSION,
        records: actions
            .into_iter()
            .map(|action| JournalRecord {
                action,
                outcome: ActionOutcome::Pending,
                message: None,
            })
            .collect(),
        stopped_after: None,
    }
}

fn first_unconfirmed_irreversible(
    journal: &ExecutionJournal,
    allow_irreversible: bool,
) -> Option<usize> {
    if allow_irreversible {
        None
    } else {
        journal
            .records
            .iter()
            .position(|record| record.action.rollback == RollbackClass::Irreversible)
    }
}

fn rollback_completed_durably(
    journal: &mut ExecutionJournal,
    executor: &mut impl MutationExecutor,
    checkpoint: &mut impl JournalCheckpoint,
    completed: &[(usize, ActionReceipt)],
    last_durable: &mut Option<ExecutionJournal>,
) -> Result<(), DurableExecutionError> {
    for (index, receipt) in completed.iter().rev() {
        if journal.records[*index].action.rollback == RollbackClass::Irreversible {
            continue;
        }

        journal.records[*index].outcome = ActionOutcome::RollbackInProgress;
        persist_checkpoint(
            checkpoint,
            journal,
            last_durable,
            JournalCheckpointPhase::BeforeRollback,
            Some(*index),
        )?;

        match executor.rollback(&journal.records[*index].action, receipt) {
            Ok(rollback_receipt) => {
                journal.records[*index].outcome = match journal.records[*index].action.rollback {
                    RollbackClass::Reversible => ActionOutcome::RolledBack,
                    RollbackClass::Compensating => ActionOutcome::Compensated,
                    RollbackClass::Irreversible => unreachable!(),
                };
                journal.records[*index].message = Some(rollback_receipt.summary().to_owned());
            }
            Err(failure) => {
                journal.records[*index].outcome = ActionOutcome::RollbackFailed;
                journal.records[*index].message =
                    Some(format!("{}: {}", failure.code(), failure.message()));
            }
        }

        persist_checkpoint(
            checkpoint,
            journal,
            last_durable,
            JournalCheckpointPhase::AfterRollback,
            Some(*index),
        )?;
    }
    Ok(())
}

fn persist_checkpoint(
    checkpoint: &mut impl JournalCheckpoint,
    journal: &ExecutionJournal,
    last_durable: &mut Option<ExecutionJournal>,
    phase: JournalCheckpointPhase,
    action_index: Option<usize>,
) -> Result<(), DurableExecutionError> {
    match checkpoint.checkpoint(journal) {
        Ok(()) => {
            *last_durable = Some(journal.clone());
            Ok(())
        }
        Err(failure) => Err(DurableExecutionError::Checkpoint(DurableCheckpointError {
            phase,
            action_id: action_index.map(|index| journal.records[index].action.id.clone()),
            last_durable: last_durable.clone(),
            attempted: journal.clone(),
            failure,
        })),
    }
}

/// Adapter that writes each checkpoint as one canonical state-store journal document.
#[derive(Debug)]
pub struct StateStoreJournalCheckpoint<'a, S> {
    store: &'a mut S,
    installation_id: InstallationId,
    journal_id: JournalId,
}

impl<'a, S> StateStoreJournalCheckpoint<'a, S> {
    #[must_use]
    pub fn new(
        store: &'a mut S,
        installation_id: InstallationId,
        journal_id: JournalId,
    ) -> Self {
        Self {
            store,
            installation_id,
            journal_id,
        }
    }
}

impl<S: StateStore> JournalCheckpoint for StateStoreJournalCheckpoint<'_, S> {
    fn checkpoint(&mut self, journal: &ExecutionJournal) -> Result<(), JournalCheckpointFailure> {
        let document = JournalStateDocument::new(
            self.installation_id.clone(),
            self.journal_id.clone(),
            journal.clone(),
        )
        .map_err(|error| {
            JournalCheckpointFailure::public(format!(
                "journal snapshot failed validation before persistence: {error}"
            ))
        })?;
        let record = StateRecord::journal(document).map_err(|error| {
            JournalCheckpointFailure::public(format!(
                "journal snapshot could not be bound to durable state: {error}"
            ))
        })?;
        self.store.write_atomic(&record).map_err(|error| {
            JournalCheckpointFailure::public(format!(
                "journal snapshot could not be atomically persisted: {}",
                error.message()
            ))
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::str;

    use crate::journal::{
        ActionFailure, ExecutionLane, MutationExecutor, PlannedMutation, Preconditions,
        RollbackClass,
    };
    use crate::journal_document::decode_journal_document;
    use crate::state::{InstallationId, JournalId, StateComponent, StatePath};
    use crate::state_store::{
        StateRead, StateRecord, StateStore, StateStoreError, StateWriteDisposition,
        StateWriteReceipt,
    };

    use super::{
        DurableExecutionError, JournalCheckpoint, JournalCheckpointFailure,
        StateStoreJournalCheckpoint, execute_plan_durably,
    };

    #[derive(Default)]
    struct RecordingCheckpoint {
        calls: usize,
        fail_on_call: Option<usize>,
        snapshots: Vec<super::ExecutionJournal>,
    }

    impl JournalCheckpoint for RecordingCheckpoint {
        fn checkpoint(
            &mut self,
            journal: &super::ExecutionJournal,
        ) -> Result<(), JournalCheckpointFailure> {
            self.calls += 1;
            if self.fail_on_call == Some(self.calls) {
                return Err(JournalCheckpointFailure::public("bounded checkpoint failure"));
            }
            self.snapshots.push(journal.clone());
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeExecutor {
        fail_execute: BTreeSet<String>,
        executions: Vec<String>,
        rollbacks: Vec<String>,
    }

    impl MutationExecutor for FakeExecutor {
        fn execute(
            &mut self,
            action: &PlannedMutation,
        ) -> Result<super::ActionReceipt, ActionFailure> {
            self.executions.push(action.id.clone());
            if self.fail_execute.contains(&action.id) {
                Err(ActionFailure::public("execute_failed", "bounded failure"))
            } else {
                Ok(super::ActionReceipt::public(format!(
                    "completed {}",
                    action.id
                )))
            }
        }

        fn rollback(
            &mut self,
            action: &PlannedMutation,
            _receipt: &super::ActionReceipt,
        ) -> Result<super::ActionReceipt, ActionFailure> {
            self.rollbacks.push(action.id.clone());
            Ok(super::ActionReceipt::public(format!(
                "reverted {}",
                action.id
            )))
        }
    }

    fn action(id: &str, rollback: RollbackClass) -> PlannedMutation {
        PlannedMutation::new(
            id,
            ExecutionLane::Root,
            format!("perform {id}"),
            rollback,
            Preconditions::new([format!("observed state for {id}")]),
        )
    }

    fn outcomes(journal: &super::ExecutionJournal) -> Vec<super::ActionOutcome> {
        journal.records.iter().map(|record| record.outcome).collect()
    }

    #[test]
    fn successful_execution_checkpoints_every_uncertain_boundary() {
        let mut executor = FakeExecutor::default();
        let mut checkpoint = RecordingCheckpoint::default();
        let journal = execute_plan_durably(
            vec![
                action("one", RollbackClass::Reversible),
                action("two", RollbackClass::Compensating),
            ],
            &mut executor,
            &mut checkpoint,
            false,
        )
        .expect("durable execution");

        assert!(journal.completed());
        assert_eq!(executor.executions, ["one", "two"]);
        assert_eq!(checkpoint.snapshots.len(), 5);
        assert_eq!(
            outcomes(&checkpoint.snapshots[0]),
            [super::ActionOutcome::Pending, super::ActionOutcome::Pending]
        );
        assert_eq!(
            outcomes(&checkpoint.snapshots[1]),
            [
                super::ActionOutcome::Executing,
                super::ActionOutcome::Pending
            ]
        );
        assert_eq!(
            outcomes(&checkpoint.snapshots[2]),
            [
                super::ActionOutcome::Completed,
                super::ActionOutcome::Pending
            ]
        );
        assert_eq!(
            outcomes(&checkpoint.snapshots[3]),
            [
                super::ActionOutcome::Completed,
                super::ActionOutcome::Executing
            ]
        );
        assert_eq!(
            outcomes(&checkpoint.snapshots[4]),
            [
                super::ActionOutcome::Completed,
                super::ActionOutcome::Completed
            ]
        );
    }

    #[test]
    fn failure_before_execute_prevents_the_executor_call() {
        let mut executor = FakeExecutor::default();
        let mut checkpoint = RecordingCheckpoint {
            fail_on_call: Some(2),
            ..RecordingCheckpoint::default()
        };
        let error = execute_plan_durably(
            vec![action("one", RollbackClass::Reversible)],
            &mut executor,
            &mut checkpoint,
            false,
        )
        .expect_err("checkpoint must fail");

        assert!(executor.executions.is_empty());
        let DurableExecutionError::Checkpoint(error) = error else {
            panic!("expected checkpoint error");
        };
        assert_eq!(
            outcomes(error.last_durable().expect("initial snapshot")),
            [super::ActionOutcome::Pending]
        );
        assert_eq!(
            outcomes(error.attempted()),
            [super::ActionOutcome::Executing]
        );
    }

    #[test]
    fn failure_after_execute_exposes_the_last_durable_uncertain_state() {
        let mut executor = FakeExecutor::default();
        let mut checkpoint = RecordingCheckpoint {
            fail_on_call: Some(3),
            ..RecordingCheckpoint::default()
        };
        let error = execute_plan_durably(
            vec![action("one", RollbackClass::Reversible)],
            &mut executor,
            &mut checkpoint,
            false,
        )
        .expect_err("checkpoint must fail");

        assert_eq!(executor.executions, ["one"]);
        let DurableExecutionError::Checkpoint(error) = error else {
            panic!("expected checkpoint error");
        };
        assert_eq!(
            outcomes(error.last_durable().expect("executing snapshot")),
            [super::ActionOutcome::Executing]
        );
        assert_eq!(
            outcomes(error.attempted()),
            [super::ActionOutcome::Completed]
        );
    }

    #[test]
    fn rollback_is_checkpointed_before_and_after_the_inverse() {
        let mut executor = FakeExecutor {
            fail_execute: BTreeSet::from(["two".to_owned()]),
            ..FakeExecutor::default()
        };
        let mut checkpoint = RecordingCheckpoint::default();
        let journal = execute_plan_durably(
            vec![
                action("one", RollbackClass::Reversible),
                action("two", RollbackClass::Reversible),
            ],
            &mut executor,
            &mut checkpoint,
            false,
        )
        .expect("durable partial failure");

        assert_eq!(executor.rollbacks, ["one"]);
        assert_eq!(
            outcomes(&journal),
            [
                super::ActionOutcome::RolledBack,
                super::ActionOutcome::Failed
            ]
        );
        assert!(checkpoint.snapshots.iter().any(|snapshot| {
            outcomes(snapshot)
                == [
                    super::ActionOutcome::RollbackInProgress,
                    super::ActionOutcome::Failed,
                ]
        }));
    }

    #[derive(Default)]
    struct MemoryStore {
        entries: BTreeMap<Vec<String>, Vec<u8>>,
    }

    impl StateStore for MemoryStore {
        fn read(&self, path: &StatePath) -> Result<StateRead, StateStoreError> {
            let key = path
                .components()
                .iter()
                .map(StateComponent::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            Ok(self
                .entries
                .get(&key)
                .cloned()
                .map_or(StateRead::Missing, StateRead::Present))
        }

        fn write_atomic(
            &mut self,
            record: &StateRecord,
        ) -> Result<StateWriteReceipt, StateStoreError> {
            let key = record
                .path()
                .components()
                .iter()
                .map(StateComponent::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let disposition = if self.entries.insert(key, record.bytes().to_vec()).is_some() {
                StateWriteDisposition::Replaced
            } else {
                StateWriteDisposition::Created
            };
            Ok(StateWriteReceipt::new(disposition, record.bytes().len()))
        }
    }

    #[test]
    fn state_store_adapter_replaces_one_canonical_journal_document() {
        let mut store = MemoryStore::default();
        let mut executor = FakeExecutor::default();
        {
            let mut checkpoint = StateStoreJournalCheckpoint::new(
                &mut store,
                InstallationId::parse("0123456789abcdef").expect("installation ID"),
                JournalId::parse("apply-00000001").expect("journal ID"),
            );
            execute_plan_durably(
                vec![action("one", RollbackClass::Reversible)],
                &mut executor,
                &mut checkpoint,
                false,
            )
            .expect("durable execution");
        }

        assert_eq!(store.entries.len(), 1);
        let bytes = store.entries.values().next().expect("journal bytes");
        let document = decode_journal_document(str::from_utf8(bytes).expect("UTF-8 journal"))
            .expect("valid persisted journal");
        assert_eq!(
            outcomes(document.journal()),
            [super::ActionOutcome::Completed]
        );
    }
}
