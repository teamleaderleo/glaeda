use std::fmt;

use serde::{Deserialize, Serialize};

use crate::journal::{
    ActionOutcome, ExecutionJournal, ExecutionLane, JOURNAL_SCHEMA_VERSION, JournalRecord,
    PlannedMutation, Preconditions, RollbackClass, validate_plan,
};
use crate::state::{InstallationId, JournalId};

pub const JOURNAL_DOCUMENT_SCHEMA_VERSION: u8 = 1;

const MAX_JOURNAL_RECORDS: usize = 10_000;
const MAX_EVIDENCE_ITEMS: usize = 256;
const MAX_PUBLIC_TEXT_LEN: usize = 4_096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JournalStateDocument {
    document_type: JournalDocumentType,
    schema_version: u8,
    installation_id: InstallationId,
    journal_id: JournalId,
    journal: ExecutionJournal,
}

impl JournalStateDocument {
    /// Build one validated execution-journal state document.
    ///
    /// # Errors
    ///
    /// Returns an error for unsupported versions, invalid plans, inconsistent outcomes, malformed
    /// stop positions, or unbounded public text.
    pub fn new(
        installation_id: InstallationId,
        journal_id: JournalId,
        journal: ExecutionJournal,
    ) -> Result<Self, JournalDocumentError> {
        validate_execution_journal(&journal)?;
        Ok(Self {
            document_type: JournalDocumentType::Journal,
            schema_version: JOURNAL_DOCUMENT_SCHEMA_VERSION,
            installation_id,
            journal_id,
            journal,
        })
    }

    #[must_use]
    pub fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub fn installation_id(&self) -> &InstallationId {
        &self.installation_id
    }

    #[must_use]
    pub fn journal_id(&self) -> &JournalId {
        &self.journal_id
    }

    #[must_use]
    pub fn journal(&self) -> &ExecutionJournal {
        &self.journal
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum JournalDocumentType {
    Journal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JournalDocumentError {
    pub problems: Vec<String>,
}

impl JournalDocumentError {
    fn single(problem: impl Into<String>) -> Self {
        Self {
            problems: vec![problem.into()],
        }
    }
}

impl fmt::Display for JournalDocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "journal document validation failed")?;
        for problem in &self.problems {
            writeln!(formatter, "- {problem}")?;
        }
        Ok(())
    }
}

impl std::error::Error for JournalDocumentError {}

/// Serialize one validated journal document with stable, human-readable JSON.
///
/// # Errors
///
/// Returns an error only when JSON serialization fails.
pub fn encode_journal_document(
    document: &JournalStateDocument,
) -> Result<String, JournalDocumentError> {
    let mut encoded = serde_json::to_string_pretty(document).map_err(|error| {
        JournalDocumentError::single(format!("journal document serialization failed: {error}"))
    })?;
    encoded.push('\n');
    Ok(encoded)
}

/// Decode untrusted JSON through an exact journal schema and recovery-state validation.
///
/// # Errors
///
/// Returns an error for malformed JSON, unknown fields, unsupported versions, invalid plans,
/// inconsistent outcomes, or malformed stop positions.
pub fn decode_journal_document(input: &str) -> Result<JournalStateDocument, JournalDocumentError> {
    let wire: WireJournalStateDocument = serde_json::from_str(input).map_err(|error| {
        JournalDocumentError::single(format!("journal document JSON is invalid: {error}"))
    })?;
    wire.try_into()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireJournalStateDocument {
    document_type: WireJournalDocumentType,
    schema_version: u8,
    installation_id: String,
    journal_id: String,
    journal: WireExecutionJournal,
}

impl TryFrom<WireJournalStateDocument> for JournalStateDocument {
    type Error = JournalDocumentError;

    fn try_from(wire: WireJournalStateDocument) -> Result<Self, Self::Error> {
        let WireJournalDocumentType::Journal = wire.document_type;
        if wire.schema_version != JOURNAL_DOCUMENT_SCHEMA_VERSION {
            return Err(JournalDocumentError::single(format!(
                "journal document schema version {} is not supported",
                wire.schema_version
            )));
        }
        let installation_id = InstallationId::parse(&wire.installation_id)
            .map_err(|error| JournalDocumentError::single(error.to_string()))?;
        let journal_id = JournalId::parse(&wire.journal_id)
            .map_err(|error| JournalDocumentError::single(error.to_string()))?;
        Self::new(installation_id, journal_id, wire.journal.into())
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireJournalDocumentType {
    Journal,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireExecutionJournal {
    schema_version: u8,
    records: Vec<WireJournalRecord>,
    #[serde(default)]
    stopped_after: Option<String>,
}

impl From<WireExecutionJournal> for ExecutionJournal {
    fn from(wire: WireExecutionJournal) -> Self {
        Self {
            schema_version: wire.schema_version,
            records: wire.records.into_iter().map(Into::into).collect(),
            stopped_after: wire.stopped_after,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireJournalRecord {
    action: WirePlannedMutation,
    outcome: WireActionOutcome,
    #[serde(default)]
    message: Option<String>,
}

impl From<WireJournalRecord> for JournalRecord {
    fn from(wire: WireJournalRecord) -> Self {
        Self {
            action: wire.action.into(),
            outcome: wire.outcome.into(),
            message: wire.message,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WirePlannedMutation {
    id: String,
    lane: WireExecutionLane,
    summary: String,
    rollback: WireRollbackClass,
    preconditions: WirePreconditions,
}

impl From<WirePlannedMutation> for PlannedMutation {
    fn from(wire: WirePlannedMutation) -> Self {
        Self {
            id: wire.id,
            lane: wire.lane.into(),
            summary: wire.summary,
            rollback: wire.rollback.into(),
            preconditions: wire.preconditions.into(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WirePreconditions {
    evidence: Vec<String>,
}

impl From<WirePreconditions> for Preconditions {
    fn from(wire: WirePreconditions) -> Self {
        Self {
            evidence: wire.evidence,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireExecutionLane {
    Operator,
    Root,
    RunnerUser,
    Github,
}

impl From<WireExecutionLane> for ExecutionLane {
    fn from(wire: WireExecutionLane) -> Self {
        match wire {
            WireExecutionLane::Operator => Self::Operator,
            WireExecutionLane::Root => Self::Root,
            WireExecutionLane::RunnerUser => Self::RunnerUser,
            WireExecutionLane::Github => Self::Github,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireRollbackClass {
    Reversible,
    Compensating,
    Irreversible,
}

impl From<WireRollbackClass> for RollbackClass {
    fn from(wire: WireRollbackClass) -> Self {
        match wire {
            WireRollbackClass::Reversible => Self::Reversible,
            WireRollbackClass::Compensating => Self::Compensating,
            WireRollbackClass::Irreversible => Self::Irreversible,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireActionOutcome {
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

impl From<WireActionOutcome> for ActionOutcome {
    fn from(wire: WireActionOutcome) -> Self {
        match wire {
            WireActionOutcome::Pending => Self::Pending,
            WireActionOutcome::Executing => Self::Executing,
            WireActionOutcome::Completed => Self::Completed,
            WireActionOutcome::Failed => Self::Failed,
            WireActionOutcome::Skipped => Self::Skipped,
            WireActionOutcome::RollbackInProgress => Self::RollbackInProgress,
            WireActionOutcome::RolledBack => Self::RolledBack,
            WireActionOutcome::Compensated => Self::Compensated,
            WireActionOutcome::RollbackFailed => Self::RollbackFailed,
        }
    }
}

fn validate_execution_journal(journal: &ExecutionJournal) -> Result<(), JournalDocumentError> {
    let mut problems = Vec::new();
    if journal.schema_version != JOURNAL_SCHEMA_VERSION {
        problems.push(format!(
            "execution journal schema version {} is not supported",
            journal.schema_version
        ));
    }
    if journal.records.len() > MAX_JOURNAL_RECORDS {
        problems.push(format!(
            "execution journal contains more than {MAX_JOURNAL_RECORDS} records"
        ));
    }

    let actions = journal
        .records
        .iter()
        .map(|record| record.action.clone())
        .collect::<Vec<_>>();
    if let Err(found) = validate_plan(&actions) {
        problems.extend(found);
    }

    for (index, record) in journal.records.iter().enumerate() {
        validate_record(index, record, &mut problems);
    }
    validate_stop_position(journal, &mut problems);
    validate_recovery_position(journal, &mut problems);

    if problems.is_empty() {
        Ok(())
    } else {
        Err(JournalDocumentError { problems })
    }
}

fn validate_record(index: usize, record: &JournalRecord, problems: &mut Vec<String>) {
    let prefix = format!("records[{index}]");
    validate_public_text(&format!("{prefix}.action.id"), &record.action.id, problems);
    validate_public_text(
        &format!("{prefix}.action.summary"),
        &record.action.summary,
        problems,
    );
    if record.action.preconditions.evidence.len() > MAX_EVIDENCE_ITEMS {
        problems.push(format!(
            "{prefix}.action.preconditions contains more than {MAX_EVIDENCE_ITEMS} entries"
        ));
    }
    for (evidence_index, evidence) in record.action.preconditions.evidence.iter().enumerate() {
        validate_public_text(
            &format!("{prefix}.action.preconditions.evidence[{evidence_index}]"),
            evidence,
            problems,
        );
    }

    match record.outcome {
        ActionOutcome::Pending => {
            if record.message.is_some() {
                problems.push(format!("{prefix}.message must be absent while pending"));
            }
        }
        ActionOutcome::Executing => {
            if record.message.is_some() {
                problems.push(format!("{prefix}.message must be absent while executing"));
            }
        }
        ActionOutcome::Completed
        | ActionOutcome::Failed
        | ActionOutcome::Skipped
        | ActionOutcome::RollbackInProgress
        | ActionOutcome::RolledBack
        | ActionOutcome::Compensated
        | ActionOutcome::RollbackFailed => match record.message.as_deref() {
            Some(message) => validate_public_text(&format!("{prefix}.message"), message, problems),
            None => problems.push(format!(
                "{prefix}.message is required for outcome {:?}",
                record.outcome
            )),
        },
    }

    match record.outcome {
        ActionOutcome::RolledBack if record.action.rollback != RollbackClass::Reversible => {
            problems.push(format!(
                "{prefix} can be rolled_back only for a reversible action"
            ));
        }
        ActionOutcome::Compensated if record.action.rollback != RollbackClass::Compensating => {
            problems.push(format!(
                "{prefix} can be compensated only for a compensating action"
            ));
        }
        ActionOutcome::RollbackInProgress | ActionOutcome::RollbackFailed
            if record.action.rollback == RollbackClass::Irreversible =>
        {
            problems.push(format!(
                "{prefix} cannot report rollback state for an irreversible action"
            ));
        }
        _ => {}
    }
}

fn validate_stop_position(journal: &ExecutionJournal, problems: &mut Vec<String>) {
    let stopping_records = journal
        .records
        .iter()
        .enumerate()
        .filter(|(_, record)| {
            matches!(
                record.outcome,
                ActionOutcome::Failed | ActionOutcome::Skipped
            )
        })
        .collect::<Vec<_>>();
    if stopping_records.len() > 1 {
        problems.push(
            "execution journal contains more than one failed or skipped stop record".to_owned(),
        );
    }

    match journal.stopped_after.as_deref() {
        None => {
            if !stopping_records.is_empty() {
                problems.push(
                    "stopped_after is required when a record is failed or skipped".to_owned(),
                );
            }
        }
        Some(stopped_after) => {
            validate_public_text("stopped_after", stopped_after, problems);
            let Some(stop_index) = journal
                .records
                .iter()
                .position(|record| record.action.id == stopped_after)
            else {
                problems.push(format!(
                    "stopped_after {stopped_after:?} does not name a journal record"
                ));
                return;
            };
            if !matches!(
                journal.records[stop_index].outcome,
                ActionOutcome::Failed | ActionOutcome::Skipped
            ) {
                problems
                    .push("stopped_after must name the failed or skipped stop record".to_owned());
            }
            match journal.records[stop_index].outcome {
                ActionOutcome::Failed => {
                    if journal.records[..stop_index]
                        .iter()
                        .any(|record| record.outcome == ActionOutcome::Pending)
                    {
                        problems
                            .push("records before a failed stop cannot remain pending".to_owned());
                    }
                    if journal.records[stop_index + 1..]
                        .iter()
                        .any(|record| record.outcome != ActionOutcome::Pending)
                    {
                        problems.push("records after a failed stop must remain pending".to_owned());
                    }
                }
                ActionOutcome::Skipped => {
                    if journal.records.iter().enumerate().any(|(index, record)| {
                        index != stop_index && record.outcome != ActionOutcome::Pending
                    }) {
                        problems.push(
                            "all records other than a skipped preflight stop must remain pending"
                                .to_owned(),
                        );
                    }
                }
                _ => {}
            }
        }
    }
}

fn validate_recovery_position(journal: &ExecutionJournal, problems: &mut Vec<String>) {
    let active = journal
        .records
        .iter()
        .enumerate()
        .filter(|(_, record)| {
            matches!(
                record.outcome,
                ActionOutcome::Executing | ActionOutcome::RollbackInProgress
            )
        })
        .collect::<Vec<_>>();
    if active.len() > 1 {
        problems.push("execution journal contains more than one in-progress record".to_owned());
        return;
    }

    let Some((active_index, active_record)) = active.first().copied() else {
        return;
    };
    match active_record.outcome {
        ActionOutcome::Executing => {
            if journal.stopped_after.is_some() {
                problems
                    .push("an executing journal cannot also contain a stop position".to_owned());
            }
            if journal.records[..active_index]
                .iter()
                .any(|record| record.outcome != ActionOutcome::Completed)
            {
                problems.push("records before an executing action must be completed".to_owned());
            }
            if journal.records[active_index + 1..]
                .iter()
                .any(|record| record.outcome != ActionOutcome::Pending)
            {
                problems.push("records after an executing action must remain pending".to_owned());
            }
        }
        ActionOutcome::RollbackInProgress => {
            if journal.stopped_after.is_none() {
                problems.push("rollback_in_progress requires a failed stop position".to_owned());
            }
        }
        _ => unreachable!(),
    }
}

fn validate_public_text(field: &str, value: &str, problems: &mut Vec<String>) {
    if value.is_empty() {
        problems.push(format!("{field} must not be empty"));
    } else if value.len() > MAX_PUBLIC_TEXT_LEN {
        problems.push(format!(
            "{field} must not exceed {MAX_PUBLIC_TEXT_LEN} bytes"
        ));
    } else if value.chars().any(char::is_control) {
        problems.push(format!("{field} must not contain control characters"));
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::journal::{
        ActionOutcome, ExecutionJournal, ExecutionLane, JOURNAL_SCHEMA_VERSION, JournalRecord,
        PlannedMutation, Preconditions, RollbackClass,
    };
    use crate::state::{InstallationId, JournalId};

    use super::{JournalStateDocument, decode_journal_document, encode_journal_document};

    fn action(id: &str, rollback: RollbackClass) -> PlannedMutation {
        PlannedMutation::new(
            id,
            ExecutionLane::Root,
            format!("perform {id}"),
            rollback,
            Preconditions::new([format!("observed state for {id}")]),
        )
    }

    fn partial_failure_journal() -> ExecutionJournal {
        ExecutionJournal {
            schema_version: JOURNAL_SCHEMA_VERSION,
            records: vec![
                JournalRecord {
                    action: action("one", RollbackClass::Reversible),
                    outcome: ActionOutcome::RolledBack,
                    message: Some("reverted one".to_owned()),
                },
                JournalRecord {
                    action: action("two", RollbackClass::Reversible),
                    outcome: ActionOutcome::Failed,
                    message: Some("execute_failed: bounded failure".to_owned()),
                },
                JournalRecord {
                    action: action("three", RollbackClass::Reversible),
                    outcome: ActionOutcome::Pending,
                    message: None,
                },
            ],
            stopped_after: Some("two".to_owned()),
        }
    }

    fn document(journal: ExecutionJournal) -> JournalStateDocument {
        JournalStateDocument::new(
            InstallationId::parse("0123456789abcdef").expect("installation ID"),
            JournalId::parse("apply-00000001").expect("journal ID"),
            journal,
        )
        .expect("valid journal document")
    }

    #[test]
    fn partial_failure_round_trips_through_strict_json() {
        let document = document(partial_failure_journal());
        let encoded = encode_journal_document(&document).expect("encode journal");
        assert_eq!(
            decode_journal_document(&encoded).expect("decode journal"),
            document
        );
    }

    #[test]
    fn forward_document_and_journal_versions_fail_closed() {
        let mut value = serde_json::to_value(document(partial_failure_journal()))
            .expect("serialize journal value");
        value["schema_version"] = json!(2);
        let error = decode_journal_document(&value.to_string()).expect_err("forward version");
        assert!(
            error
                .problems
                .iter()
                .any(|problem| problem.contains("version 2"))
        );

        value["schema_version"] = json!(1);
        value["journal"]["schema_version"] = json!(2);
        let error = decode_journal_document(&value.to_string()).expect_err("journal version");
        assert!(
            error
                .problems
                .iter()
                .any(|problem| problem.contains("version 2"))
        );
    }

    #[test]
    fn unknown_secret_fields_are_rejected() {
        let mut value = serde_json::to_value(document(partial_failure_journal()))
            .expect("serialize journal value");
        value["token"] = json!("must-never-persist");
        decode_journal_document(&value.to_string()).expect_err("unknown field must fail");
    }

    #[test]
    fn stop_position_must_name_the_single_failed_or_skipped_record() {
        let mut journal = partial_failure_journal();
        journal.stopped_after = Some("missing".to_owned());
        let error = JournalStateDocument::new(
            InstallationId::parse("0123456789abcdef").expect("installation ID"),
            JournalId::parse("apply-00000001").expect("journal ID"),
            journal,
        )
        .expect_err("invalid stop position");
        assert!(
            error
                .problems
                .iter()
                .any(|problem| problem.contains("does not name"))
        );
    }

    #[test]
    fn skipped_irreversible_preflight_allows_earlier_records_to_remain_pending() {
        let journal = ExecutionJournal {
            schema_version: JOURNAL_SCHEMA_VERSION,
            records: vec![
                JournalRecord {
                    action: action("one", RollbackClass::Reversible),
                    outcome: ActionOutcome::Pending,
                    message: None,
                },
                JournalRecord {
                    action: action("two", RollbackClass::Irreversible),
                    outcome: ActionOutcome::Skipped,
                    message: Some("irreversible action requires explicit confirmation".to_owned()),
                },
                JournalRecord {
                    action: action("three", RollbackClass::Reversible),
                    outcome: ActionOutcome::Pending,
                    message: None,
                },
            ],
            stopped_after: Some("two".to_owned()),
        };

        document(journal);
    }

    #[test]
    fn executing_state_round_trips_and_rejects_a_second_active_record() {
        let mut journal = ExecutionJournal {
            schema_version: JOURNAL_SCHEMA_VERSION,
            records: vec![
                JournalRecord {
                    action: action("one", RollbackClass::Reversible),
                    outcome: ActionOutcome::Executing,
                    message: None,
                },
                JournalRecord {
                    action: action("two", RollbackClass::Reversible),
                    outcome: ActionOutcome::Pending,
                    message: None,
                },
            ],
            stopped_after: None,
        };
        let document = document(journal.clone());
        let encoded = encode_journal_document(&document).expect("encode journal");
        assert_eq!(
            decode_journal_document(&encoded).expect("decode journal"),
            document
        );

        journal.records[1].outcome = ActionOutcome::Executing;
        JournalStateDocument::new(
            InstallationId::parse("0123456789abcdef").expect("installation ID"),
            JournalId::parse("apply-00000001").expect("journal ID"),
            journal,
        )
        .expect_err("two active records must fail");
    }

    #[test]
    fn pending_and_rollback_outcomes_must_match_their_contracts() {
        let mut journal = partial_failure_journal();
        journal.records[0].outcome = ActionOutcome::Pending;
        journal.records[0].message = Some("should be absent".to_owned());
        journal.records[2].outcome = ActionOutcome::Compensated;
        journal.records[2].message = Some("compensated".to_owned());
        let error = JournalStateDocument::new(
            InstallationId::parse("0123456789abcdef").expect("installation ID"),
            JournalId::parse("apply-00000001").expect("journal ID"),
            journal,
        )
        .expect_err("invalid outcomes");
        assert!(
            error
                .problems
                .iter()
                .any(|problem| problem.contains("must be absent while pending"))
        );
        assert!(
            error
                .problems
                .iter()
                .any(|problem| problem.contains("compensating action"))
        );
    }
}
