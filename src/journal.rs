use std::collections::BTreeSet;
use std::fmt;

use serde::Serialize;

pub const JOURNAL_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionLane {
    Operator,
    Root,
    RunnerUser,
    Github,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RollbackClass {
    Reversible,
    Compensating,
    Irreversible,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Preconditions {
    pub evidence: Vec<String>,
}

impl Preconditions {
    #[must_use]
    pub fn new(evidence: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            evidence: evidence.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlannedMutation {
    pub id: String,
    pub lane: ExecutionLane,
    pub summary: String,
    pub rollback: RollbackClass,
    pub preconditions: Preconditions,
}

impl PlannedMutation {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        lane: ExecutionLane,
        summary: impl Into<String>,
        rollback: RollbackClass,
        preconditions: Preconditions,
    ) -> Self {
        Self {
            id: id.into(),
            lane,
            summary: summary.into(),
            rollback,
            preconditions,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionReceipt {
    public_summary: String,
}

impl ActionReceipt {
    #[must_use]
    pub fn public(summary: impl Into<String>) -> Self {
        Self {
            public_summary: summary.into(),
        }
    }

    #[must_use]
    pub fn summary(&self) -> &str {
        &self.public_summary
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionFailure {
    code: String,
    public_message: String,
}

impl ActionFailure {
    #[must_use]
    pub fn public(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            public_message: message.into(),
        }
    }

    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.public_message
    }
}

pub trait MutationExecutor {
    fn execute(&mut self, action: &PlannedMutation) -> Result<ActionReceipt, ActionFailure>;

    fn rollback(
        &mut self,
        action: &PlannedMutation,
        receipt: &ActionReceipt,
    ) -> Result<ActionReceipt, ActionFailure>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionOutcome {
    Pending,
    Executing,
    Completed,
    Failed,
    Skipped,
    RollbackInProgress,
    RolledBack,
    Compensated,
    RollbackFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JournalRecord {
    pub action: PlannedMutation,
    pub outcome: ActionOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecutionJournal {
    pub schema_version: u8,
    pub records: Vec<JournalRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped_after: Option<String>,
}

impl ExecutionJournal {
    #[must_use]
    pub fn completed(&self) -> bool {
        self.records
            .iter()
            .all(|record| record.outcome == ActionOutcome::Completed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanValidationError {
    pub problems: Vec<String>,
}

impl fmt::Display for PlanValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "mutation plan validation failed")?;
        for problem in &self.problems {
            writeln!(formatter, "- {problem}")?;
        }
        Ok(())
    }
}

impl std::error::Error for PlanValidationError {}

/// Execute a sequence and retain enough public state to explain partial failure.
///
/// The executor contract accepts only public receipts and public failures. Secret-bearing process
/// details must be handled and redacted below this layer.
///
/// # Errors
///
/// Returns a validation error before calling the executor when action identities or precondition
/// evidence are invalid.
pub fn execute_plan(
    actions: Vec<PlannedMutation>,
    executor: &mut impl MutationExecutor,
    allow_irreversible: bool,
) -> Result<ExecutionJournal, PlanValidationError> {
    validate_plan(&actions).map_err(|problems| PlanValidationError { problems })?;

    let mut journal = ExecutionJournal {
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
    };

    let unconfirmed_irreversible = if allow_irreversible {
        None
    } else {
        journal
            .records
            .iter()
            .position(|record| record.action.rollback == RollbackClass::Irreversible)
    };
    if let Some(index) = unconfirmed_irreversible {
        journal.records[index].outcome = ActionOutcome::Skipped;
        journal.records[index].message =
            Some("irreversible action requires explicit confirmation".to_owned());
        journal.stopped_after = Some(journal.records[index].action.id.clone());
        return Ok(journal);
    }

    let mut completed = Vec::<(usize, ActionReceipt)>::new();
    for index in 0..journal.records.len() {
        match executor.execute(&journal.records[index].action) {
            Ok(receipt) => {
                journal.records[index].outcome = ActionOutcome::Completed;
                journal.records[index].message = Some(receipt.summary().to_owned());
                completed.push((index, receipt));
            }
            Err(failure) => {
                journal.records[index].outcome = ActionOutcome::Failed;
                journal.records[index].message =
                    Some(format!("{}: {}", failure.code(), failure.message()));
                journal.stopped_after = Some(journal.records[index].action.id.clone());
                rollback_completed(&mut journal, executor, &completed);
                break;
            }
        }
    }

    Ok(journal)
}

fn rollback_completed(
    journal: &mut ExecutionJournal,
    executor: &mut impl MutationExecutor,
    completed: &[(usize, ActionReceipt)],
) {
    for (index, receipt) in completed.iter().rev() {
        let record = &mut journal.records[*index];
        match record.action.rollback {
            RollbackClass::Irreversible => continue,
            RollbackClass::Reversible | RollbackClass::Compensating => {
                match executor.rollback(&record.action, receipt) {
                    Ok(rollback_receipt) => {
                        record.outcome = match record.action.rollback {
                            RollbackClass::Reversible => ActionOutcome::RolledBack,
                            RollbackClass::Compensating => ActionOutcome::Compensated,
                            RollbackClass::Irreversible => unreachable!(),
                        };
                        record.message = Some(rollback_receipt.summary().to_owned());
                    }
                    Err(failure) => {
                        record.outcome = ActionOutcome::RollbackFailed;
                        record.message = Some(format!("{}: {}", failure.code(), failure.message()));
                    }
                }
            }
        }
    }
}

/// Validate immutable action identities before an execution journal is created.
///
/// # Errors
///
/// Returns public validation messages for empty IDs, duplicate IDs, missing summaries, or missing
/// precondition evidence.
pub fn validate_plan(actions: &[PlannedMutation]) -> Result<(), Vec<String>> {
    let mut problems = Vec::new();
    let mut ids = BTreeSet::new();

    for action in actions {
        if action.id.is_empty() {
            problems.push("action ID must not be empty".to_owned());
        } else if !ids.insert(&action.id) {
            problems.push(format!("duplicate action ID {:?}", action.id));
        }
        if action.summary.is_empty() {
            problems.push(format!("action {:?} must have a summary", action.id));
        }
        if action.preconditions.evidence.is_empty() {
            problems.push(format!(
                "action {:?} must record precondition evidence",
                action.id
            ));
        }
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::{
        ActionFailure, ActionOutcome, ActionReceipt, ExecutionLane, MutationExecutor,
        PlannedMutation, Preconditions, RollbackClass, execute_plan, validate_plan,
    };

    #[derive(Default)]
    struct FakeExecutor {
        fail_execute: BTreeSet<String>,
        fail_rollback: BTreeSet<String>,
        executions: Vec<String>,
        rollbacks: Vec<String>,
        receipts: BTreeMap<String, String>,
        private_secret: String,
    }

    impl MutationExecutor for FakeExecutor {
        fn execute(&mut self, action: &PlannedMutation) -> Result<ActionReceipt, ActionFailure> {
            self.executions.push(action.id.clone());
            let _secret_was_available_below_the_journal = !self.private_secret.is_empty();
            if self.fail_execute.contains(&action.id) {
                Err(ActionFailure::public("execute_failed", "bounded failure"))
            } else {
                Ok(ActionReceipt::public(
                    self.receipts
                        .get(&action.id)
                        .cloned()
                        .unwrap_or_else(|| format!("completed {}", action.id)),
                ))
            }
        }

        fn rollback(
            &mut self,
            action: &PlannedMutation,
            _receipt: &ActionReceipt,
        ) -> Result<ActionReceipt, ActionFailure> {
            self.rollbacks.push(action.id.clone());
            if self.fail_rollback.contains(&action.id) {
                Err(ActionFailure::public(
                    "rollback_failed",
                    "bounded rollback failure",
                ))
            } else {
                Ok(ActionReceipt::public(format!("reverted {}", action.id)))
            }
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

    #[test]
    fn complete_success_is_retained() {
        let mut executor = FakeExecutor::default();
        let journal = execute_plan(
            vec![
                action("one", RollbackClass::Reversible),
                action("two", RollbackClass::Compensating),
            ],
            &mut executor,
            false,
        )
        .expect("valid plan");

        assert!(journal.completed());
        assert_eq!(executor.executions, ["one", "two"]);
        assert!(executor.rollbacks.is_empty());
    }

    #[test]
    fn partial_failure_rolls_back_in_reverse_order() {
        let mut executor = FakeExecutor {
            fail_execute: BTreeSet::from(["three".to_owned()]),
            ..FakeExecutor::default()
        };
        let journal = execute_plan(
            vec![
                action("one", RollbackClass::Reversible),
                action("two", RollbackClass::Compensating),
                action("three", RollbackClass::Reversible),
                action("four", RollbackClass::Reversible),
            ],
            &mut executor,
            false,
        )
        .expect("valid plan");

        assert_eq!(executor.rollbacks, ["two", "one"]);
        assert_eq!(journal.records[0].outcome, ActionOutcome::RolledBack);
        assert_eq!(journal.records[1].outcome, ActionOutcome::Compensated);
        assert_eq!(journal.records[2].outcome, ActionOutcome::Failed);
        assert_eq!(journal.records[3].outcome, ActionOutcome::Pending);
    }

    #[test]
    fn rollback_failure_is_retained() {
        let mut executor = FakeExecutor {
            fail_execute: BTreeSet::from(["two".to_owned()]),
            fail_rollback: BTreeSet::from(["one".to_owned()]),
            ..FakeExecutor::default()
        };
        let journal = execute_plan(
            vec![
                action("one", RollbackClass::Reversible),
                action("two", RollbackClass::Reversible),
            ],
            &mut executor,
            false,
        )
        .expect("valid plan");

        assert_eq!(journal.records[0].outcome, ActionOutcome::RollbackFailed);
        assert_eq!(journal.records[1].outcome, ActionOutcome::Failed);
    }

    #[test]
    fn irreversible_action_blocks_the_batch_without_confirmation() {
        let mut executor = FakeExecutor::default();
        let journal = execute_plan(
            vec![
                action("safe", RollbackClass::Reversible),
                action("danger", RollbackClass::Irreversible),
                action("later", RollbackClass::Reversible),
            ],
            &mut executor,
            false,
        )
        .expect("valid plan");

        assert_eq!(journal.records[0].outcome, ActionOutcome::Pending);
        assert_eq!(journal.records[1].outcome, ActionOutcome::Skipped);
        assert_eq!(journal.records[2].outcome, ActionOutcome::Pending);
        assert!(executor.executions.is_empty());
    }

    #[test]
    fn journal_serialization_contains_only_public_messages() {
        let mut executor = FakeExecutor {
            receipts: BTreeMap::from([("one".to_owned(), "public result".to_owned())]),
            private_secret: "registration-token".to_owned(),
            ..FakeExecutor::default()
        };
        let journal = execute_plan(
            vec![action("one", RollbackClass::Reversible)],
            &mut executor,
            false,
        )
        .expect("valid plan");
        let json = serde_json::to_string(&journal).expect("serialize journal");

        assert!(json.contains("public result"));
        assert!(!json.contains("registration-token"));
    }

    #[test]
    fn invalid_plan_never_reaches_the_executor() {
        let invalid = vec![
            action("duplicate", RollbackClass::Reversible),
            action("duplicate", RollbackClass::Reversible),
        ];
        let mut executor = FakeExecutor::default();
        let error = execute_plan(invalid, &mut executor, false).expect_err("invalid plan");

        assert!(
            error
                .problems
                .iter()
                .any(|problem| problem.contains("duplicate"))
        );
        assert!(executor.executions.is_empty());
    }

    #[test]
    fn plan_validation_rejects_duplicate_ids_and_missing_evidence() {
        let mut invalid = action("duplicate", RollbackClass::Reversible);
        invalid.preconditions.evidence.clear();
        let error = validate_plan(&[invalid.clone(), invalid]).expect_err("invalid plan");

        assert!(error.iter().any(|problem| problem.contains("duplicate")));
        assert!(error.iter().any(|problem| problem.contains("precondition")));
    }
}
