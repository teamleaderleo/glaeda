use std::fmt;

use serde::Serialize;

use crate::artifact::{RepositoryRef, Sha256Digest};
use crate::execution_receipt::{
    ExecutionReceipt, ExecutionReceiptAction, ExecutionReceiptActionOutcome,
    ExecutionReceiptContinuation, ExecutionReceiptDisposition, ExecutionReceiptError,
    ReceiptTimestamp,
};
use crate::host_preparation_execution::{
    HOST_PREPARATION_EXECUTION_SCHEMA_VERSION, HostPreparationExecutionDisposition,
    HostPreparationExecutionReport,
};
use crate::journal::{ActionOutcome, JOURNAL_SCHEMA_VERSION, JournalRecord};
use crate::state::JournalId;

#[derive(Debug, Clone)]
pub struct HostPreparationReceiptContext {
    pub execution_id: JournalId,
    pub source_digest: Sha256Digest,
    pub started_at: ReceiptTimestamp,
    pub terminal_at: ReceiptTimestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostPreparationReceiptMappingErrorKind {
    UnsupportedExecutionSchema,
    UnsupportedJournalSchema,
    InvalidRepositoryIdentity,
    NonTerminalJournal,
    InvalidReceipt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostPreparationReceiptMappingError {
    kind: HostPreparationReceiptMappingErrorKind,
    public_message: &'static str,
}

impl HostPreparationReceiptMappingError {
    #[must_use]
    pub const fn kind(&self) -> HostPreparationReceiptMappingErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn message(&self) -> &'static str {
        self.public_message
    }

    const fn new(
        kind: HostPreparationReceiptMappingErrorKind,
        public_message: &'static str,
    ) -> Self {
        Self {
            kind,
            public_message,
        }
    }

    const fn unsupported_execution_schema() -> Self {
        Self::new(
            HostPreparationReceiptMappingErrorKind::UnsupportedExecutionSchema,
            "host-preparation execution schema is not supported",
        )
    }

    const fn unsupported_journal_schema() -> Self {
        Self::new(
            HostPreparationReceiptMappingErrorKind::UnsupportedJournalSchema,
            "host-preparation journal schema is not supported",
        )
    }

    const fn invalid_repository_identity() -> Self {
        Self::new(
            HostPreparationReceiptMappingErrorKind::InvalidRepositoryIdentity,
            "host-preparation repository identity is invalid",
        )
    }

    const fn non_terminal_journal() -> Self {
        Self::new(
            HostPreparationReceiptMappingErrorKind::NonTerminalJournal,
            "host-preparation journal contains a non-terminal action",
        )
    }

    const fn invalid_receipt() -> Self {
        Self::new(
            HostPreparationReceiptMappingErrorKind::InvalidReceipt,
            "host-preparation execution cannot produce a valid external receipt",
        )
    }
}

impl fmt::Display for HostPreparationReceiptMappingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.public_message)
    }
}

impl std::error::Error for HostPreparationReceiptMappingError {}

/// Map one terminal durable host-preparation report into external execution receipt v1.
///
/// The caller supplies the durable journal identity, exact reviewed-source digest, and explicit
/// start/terminal times. The mapper copies only bounded action identity, execution lane, rollback
/// class, typed outcomes, generic stable failure codes, and continuation identities. Journal prose,
/// precondition evidence, complete host observations, executable paths, commands, and process output
/// remain outside the receipt.
///
/// # Errors
///
/// Returns a bounded error for unsupported schemas, an invalid repository identity, a non-terminal
/// journal record, or report semantics that cannot satisfy the external receipt contract.
pub fn map_host_preparation_execution_receipt(
    report: &HostPreparationExecutionReport,
    context: HostPreparationReceiptContext,
) -> Result<ExecutionReceipt, HostPreparationReceiptMappingError> {
    if report.schema_version != HOST_PREPARATION_EXECUTION_SCHEMA_VERSION {
        return Err(HostPreparationReceiptMappingError::unsupported_execution_schema());
    }
    if report.journal.schema_version != JOURNAL_SCHEMA_VERSION {
        return Err(HostPreparationReceiptMappingError::unsupported_journal_schema());
    }

    let repository = RepositoryRef::parse(&report.source.repository)
        .map_err(|_| HostPreparationReceiptMappingError::invalid_repository_identity())?;
    let actions = report
        .journal
        .records
        .iter()
        .map(map_action)
        .collect::<Result<Vec<_>, _>>()?;
    let disposition = map_disposition(report.disposition);
    let fresh_observation_required = matches!(
        report.disposition,
        HostPreparationExecutionDisposition::FreshObservationRequired
    );
    let barriers = if fresh_observation_required {
        report
            .continuation_barriers
            .iter()
            .map(|barrier| barrier.id.as_str())
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let deferred_actions = report
        .deferred_actions
        .iter()
        .map(|action| action.id.as_str())
        .collect::<Vec<_>>();
    let continuation =
        ExecutionReceiptContinuation::new(fresh_observation_required, barriers, deferred_actions)
            .map_err(map_receipt_error)?;

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
    .map_err(map_receipt_error)
}

fn map_action(
    record: &JournalRecord,
) -> Result<ExecutionReceiptAction, HostPreparationReceiptMappingError> {
    let (outcome, failure_code) = match record.outcome {
        ActionOutcome::Pending => (ExecutionReceiptActionOutcome::NotRun, None),
        ActionOutcome::Executing | ActionOutcome::RollbackInProgress => {
            return Err(HostPreparationReceiptMappingError::non_terminal_journal());
        }
        ActionOutcome::Completed => (ExecutionReceiptActionOutcome::Completed, None),
        ActionOutcome::Failed => (
            ExecutionReceiptActionOutcome::Failed,
            Some("action-execution-failed"),
        ),
        ActionOutcome::Skipped => (ExecutionReceiptActionOutcome::Skipped, None),
        ActionOutcome::RolledBack => (ExecutionReceiptActionOutcome::RolledBack, None),
        ActionOutcome::Compensated => (ExecutionReceiptActionOutcome::Compensated, None),
        ActionOutcome::RollbackFailed => (
            ExecutionReceiptActionOutcome::RollbackFailed,
            Some("action-rollback-failed"),
        ),
    };
    ExecutionReceiptAction::new(
        &record.action.id,
        record.action.lane,
        record.action.rollback,
        outcome,
        failure_code,
    )
    .map_err(map_receipt_error)
}

const fn map_disposition(
    disposition: HostPreparationExecutionDisposition,
) -> ExecutionReceiptDisposition {
    match disposition {
        HostPreparationExecutionDisposition::Completed => ExecutionReceiptDisposition::Completed,
        HostPreparationExecutionDisposition::ActionFailed => {
            ExecutionReceiptDisposition::ActionFailed
        }
        HostPreparationExecutionDisposition::FreshObservationRequired => {
            ExecutionReceiptDisposition::FreshObservationRequired
        }
    }
}

fn map_receipt_error(_: ExecutionReceiptError) -> HostPreparationReceiptMappingError {
    HostPreparationReceiptMappingError::invalid_receipt()
}
