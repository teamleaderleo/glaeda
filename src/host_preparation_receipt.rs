use std::collections::BTreeSet;
use std::fmt;

use serde::Serialize;

use crate::artifact::{RepositoryRef, Sha256Digest};
use crate::execution_receipt::{
    ExecutionReceipt, ExecutionReceiptAction, ExecutionReceiptActionOutcome,
    ExecutionReceiptContinuation, ExecutionReceiptDisposition, ReceiptTimestamp,
};
use crate::host_preparation_execution::{
    HOST_PREPARATION_EXECUTION_SCHEMA_VERSION, HostPreparationExecutionDisposition,
    HostPreparationExecutionReport,
};
use crate::journal::{ActionOutcome, JOURNAL_SCHEMA_VERSION, JournalRecord};
use crate::state::JournalId;

const ACTION_FAILURE_CODE: &str = "host-preparation-action-failed";
const ROLLBACK_FAILURE_CODE: &str = "host-preparation-rollback-failed";

#[derive(Debug, Clone)]
pub struct HostPreparationReceiptContext {
    execution_id: JournalId,
    source_digest: Sha256Digest,
    started_at: ReceiptTimestamp,
    terminal_at: ReceiptTimestamp,
}

impl HostPreparationReceiptContext {
    #[must_use]
    pub const fn new(
        execution_id: JournalId,
        source_digest: Sha256Digest,
        started_at: ReceiptTimestamp,
        terminal_at: ReceiptTimestamp,
    ) -> Self {
        Self {
            execution_id,
            source_digest,
            started_at,
            terminal_at,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostPreparationReceiptErrorKind {
    UnsupportedExecutionSchema,
    UnsupportedJournalSchema,
    InvalidRepositoryIdentity,
    NonterminalJournal,
    InconsistentExecutionReport,
    ReceiptValidation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostPreparationReceiptError {
    kind: HostPreparationReceiptErrorKind,
    public_message: String,
}

impl HostPreparationReceiptError {
    #[must_use]
    pub const fn kind(&self) -> HostPreparationReceiptErrorKind {
        self.kind
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.public_message
    }

    fn fixed(kind: HostPreparationReceiptErrorKind, message: &'static str) -> Self {
        Self {
            kind,
            public_message: message.to_owned(),
        }
    }

    fn unsupported_execution_schema() -> Self {
        Self::fixed(
            HostPreparationReceiptErrorKind::UnsupportedExecutionSchema,
            "the host-preparation execution schema is unsupported",
        )
    }

    fn unsupported_journal_schema() -> Self {
        Self::fixed(
            HostPreparationReceiptErrorKind::UnsupportedJournalSchema,
            "the host-preparation journal schema is unsupported",
        )
    }

    fn invalid_repository_identity() -> Self {
        Self::fixed(
            HostPreparationReceiptErrorKind::InvalidRepositoryIdentity,
            "the host-preparation repository identity is invalid",
        )
    }

    fn nonterminal_journal() -> Self {
        Self::fixed(
            HostPreparationReceiptErrorKind::NonterminalJournal,
            "the host-preparation journal contains an in-progress action",
        )
    }

    fn inconsistent_execution_report() -> Self {
        Self::fixed(
            HostPreparationReceiptErrorKind::InconsistentExecutionReport,
            "the host-preparation execution report is internally inconsistent",
        )
    }

    fn receipt_validation() -> Self {
        Self::fixed(
            HostPreparationReceiptErrorKind::ReceiptValidation,
            "the host-preparation execution could not be represented by receipt schema v1",
        )
    }
}

impl fmt::Display for HostPreparationReceiptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.public_message)
    }
}

impl std::error::Error for HostPreparationReceiptError {}

/// Map one terminal host-preparation execution into the accepted external receipt v1 document.
///
/// The source digest and timestamps come from explicit reviewed boundaries. The mapper never hashes
/// the complete source report, parses journal prose, or carries paths, preconditions, commands,
/// process output, or observation evidence into the receipt.
///
/// # Errors
///
/// Returns a bounded error for unsupported schemas, invalid repository identity, an in-progress
/// journal, inconsistent terminal semantics, duplicate continuation identities, or receipt contract
/// rejection.
pub fn map_host_preparation_execution_receipt(
    report: &HostPreparationExecutionReport,
    context: HostPreparationReceiptContext,
) -> Result<ExecutionReceipt, HostPreparationReceiptError> {
    if report.schema_version != HOST_PREPARATION_EXECUTION_SCHEMA_VERSION {
        return Err(HostPreparationReceiptError::unsupported_execution_schema());
    }
    if report.journal.schema_version != JOURNAL_SCHEMA_VERSION {
        return Err(HostPreparationReceiptError::unsupported_journal_schema());
    }
    if report.journal.records.is_empty() {
        return Err(HostPreparationReceiptError::inconsistent_execution_report());
    }

    let repository = RepositoryRef::parse(&report.source.repository)
        .map_err(|_| HostPreparationReceiptError::invalid_repository_identity())?;
    let actions = report
        .journal
        .records
        .iter()
        .map(|record| map_action(record, report.disposition))
        .collect::<Result<Vec<_>, _>>()?;
    let (disposition, continuation) = map_terminal_state(report)?;

    ExecutionReceipt::new_host_preparation(
        context.execution_id,
        repository,
        context.source_digest,
        &report.phase_id,
        context.started_at,
        context.terminal_at,
        disposition,
        actions,
        continuation,
    )
    .map_err(|_| HostPreparationReceiptError::receipt_validation())
}

fn map_action(
    record: &JournalRecord,
    disposition: HostPreparationExecutionDisposition,
) -> Result<ExecutionReceiptAction, HostPreparationReceiptError> {
    let (outcome, failure_code) = match record.outcome {
        ActionOutcome::Completed => (ExecutionReceiptActionOutcome::Completed, None),
        ActionOutcome::Failed => (
            ExecutionReceiptActionOutcome::Failed,
            Some(ACTION_FAILURE_CODE),
        ),
        ActionOutcome::Skipped => (ExecutionReceiptActionOutcome::Skipped, None),
        ActionOutcome::Pending
            if disposition == HostPreparationExecutionDisposition::ActionFailed =>
        {
            (ExecutionReceiptActionOutcome::NotRun, None)
        }
        ActionOutcome::RolledBack => (ExecutionReceiptActionOutcome::RolledBack, None),
        ActionOutcome::Compensated => (ExecutionReceiptActionOutcome::Compensated, None),
        ActionOutcome::RollbackFailed => (
            ExecutionReceiptActionOutcome::RollbackFailed,
            Some(ROLLBACK_FAILURE_CODE),
        ),
        ActionOutcome::Pending => {
            return Err(HostPreparationReceiptError::inconsistent_execution_report());
        }
        ActionOutcome::Executing | ActionOutcome::RollbackInProgress => {
            return Err(HostPreparationReceiptError::nonterminal_journal());
        }
    };

    ExecutionReceiptAction::new(
        &record.action.id,
        record.action.lane,
        record.action.rollback,
        outcome,
        failure_code,
    )
    .map_err(|_| HostPreparationReceiptError::receipt_validation())
}

fn map_terminal_state(
    report: &HostPreparationExecutionReport,
) -> Result<(ExecutionReceiptDisposition, ExecutionReceiptContinuation), HostPreparationReceiptError>
{
    match report.disposition {
        HostPreparationExecutionDisposition::Completed => {
            if !report.continuation_barriers.is_empty() || !report.deferred_actions.is_empty() {
                return Err(HostPreparationReceiptError::inconsistent_execution_report());
            }
            let continuation =
                ExecutionReceiptContinuation::new(false, [] as [&str; 0], [] as [&str; 0])
                    .map_err(|_| HostPreparationReceiptError::receipt_validation())?;
            Ok((ExecutionReceiptDisposition::Completed, continuation))
        }
        HostPreparationExecutionDisposition::ActionFailed => {
            let deferred = sorted_unique(
                report
                    .deferred_actions
                    .iter()
                    .map(|action| action.id.as_str()),
            )?;
            let continuation = ExecutionReceiptContinuation::new(
                false,
                [] as [&str; 0],
                deferred.iter().map(String::as_str),
            )
            .map_err(|_| HostPreparationReceiptError::receipt_validation())?;
            Ok((ExecutionReceiptDisposition::ActionFailed, continuation))
        }
        HostPreparationExecutionDisposition::FreshObservationRequired => {
            let barriers = sorted_unique(
                report
                    .continuation_barriers
                    .iter()
                    .map(|barrier| barrier.id.as_str()),
            )?;
            let deferred = sorted_unique(
                report
                    .deferred_actions
                    .iter()
                    .map(|action| action.id.as_str()),
            )?;
            let continuation = ExecutionReceiptContinuation::new(
                true,
                barriers.iter().map(String::as_str),
                deferred.iter().map(String::as_str),
            )
            .map_err(|_| HostPreparationReceiptError::receipt_validation())?;
            Ok((
                ExecutionReceiptDisposition::FreshObservationRequired,
                continuation,
            ))
        }
    }
}

fn sorted_unique<'a>(
    values: impl IntoIterator<Item = &'a str>,
) -> Result<Vec<String>, HostPreparationReceiptError> {
    let mut unique = BTreeSet::new();
    for value in values {
        if !unique.insert(value.to_owned()) {
            return Err(HostPreparationReceiptError::inconsistent_execution_report());
        }
    }
    Ok(unique.into_iter().collect())
}
