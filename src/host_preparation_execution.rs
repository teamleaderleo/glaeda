use std::fmt;

use serde::Serialize;

use crate::durable_journal::{DurableCheckpointError, DurableExecutionError, JournalCheckpoint};
use crate::durable_lane_execution::{LaneCommandRunner, execute_lane_plan_durably};
use crate::host_preparation_command::{
    HostPreparationCommandDecision, HostPreparationCommandDisposition,
};
use crate::host_preparation_plan::{
    DeferredHostPreparationAction, FreshObservationBarrier, HostPreparationResult,
    HostReadinessSourceIdentity,
};
use crate::journal::ExecutionJournal;

pub const HOST_PREPARATION_EXECUTION_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostPreparationExecutionDisposition {
    Completed,
    ActionFailed,
    FreshObservationRequired,
}

#[derive(Debug, Clone, Serialize)]
pub struct HostPreparationExecutionReport {
    pub schema_version: u8,
    pub source: HostReadinessSourceIdentity,
    pub phase_id: String,
    pub disposition: HostPreparationExecutionDisposition,
    pub journal: ExecutionJournal,
    pub continuation_barriers: Vec<FreshObservationBarrier>,
    pub deferred_actions: Vec<DeferredHostPreparationAction>,
}

impl HostPreparationExecutionReport {
    #[must_use]
    pub fn fresh_observation_required(&self) -> bool {
        self.disposition != HostPreparationExecutionDisposition::Completed
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostPreparationExecutionErrorKind {
    DecisionNotConfirmed,
    InconsistentDecision,
    DurablePlan,
    JournalCheckpoint,
}

#[derive(Debug, Clone, Serialize)]
pub struct HostPreparationExecutionError {
    kind: HostPreparationExecutionErrorKind,
    public_message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    problems: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    checkpoint: Option<Box<DurableCheckpointError>>,
}

impl HostPreparationExecutionError {
    #[must_use]
    pub const fn kind(&self) -> HostPreparationExecutionErrorKind {
        self.kind
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.public_message
    }

    #[must_use]
    pub fn problems(&self) -> &[String] {
        &self.problems
    }

    #[must_use]
    pub fn checkpoint(&self) -> Option<&DurableCheckpointError> {
        self.checkpoint.as_deref()
    }

    fn decision_not_confirmed(disposition: HostPreparationCommandDisposition) -> Self {
        Self {
            kind: HostPreparationExecutionErrorKind::DecisionNotConfirmed,
            public_message: format!(
                "host preparation requires a confirmed executable decision; received {disposition:?}"
            ),
            problems: Vec::new(),
            checkpoint: None,
        }
    }

    fn inconsistent_decision() -> Self {
        Self {
            kind: HostPreparationExecutionErrorKind::InconsistentDecision,
            public_message:
                "a confirmed host-preparation decision did not contain one executable phase"
                    .to_owned(),
            problems: Vec::new(),
            checkpoint: None,
        }
    }

    fn durable_plan(problems: Vec<String>) -> Self {
        Self {
            kind: HostPreparationExecutionErrorKind::DurablePlan,
            public_message:
                "the confirmed host-preparation phase failed durable plan validation before execution"
                    .to_owned(),
            problems,
            checkpoint: None,
        }
    }

    fn journal_checkpoint(checkpoint: DurableCheckpointError) -> Self {
        Self {
            kind: HostPreparationExecutionErrorKind::JournalCheckpoint,
            public_message: "host preparation stopped because a durable journal checkpoint failed"
                .to_owned(),
            problems: Vec::new(),
            checkpoint: Some(Box::new(checkpoint)),
        }
    }
}

impl fmt::Display for HostPreparationExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.public_message)
    }
}

impl std::error::Error for HostPreparationExecutionError {}

/// Execute exactly one confirmed host-preparation phase through injected durable boundaries.
///
/// The caller supplies an already reviewed typed command runner and journal checkpoint. This
/// function never constructs commands, opens a state store, selects a system identity, or continues
/// through a fresh-observation barrier. Irreversible execution is enabled only because the consumed
/// decision was created by the exact confirmation contract.
///
/// # Errors
///
/// Returns a bounded error before the first checkpoint when the decision is not confirmed or is
/// internally inconsistent. Durable plan and checkpoint failures preserve only their existing
/// public evidence.
pub fn execute_confirmed_host_preparation(
    decision: HostPreparationCommandDecision,
    runner: &mut impl LaneCommandRunner,
    checkpoint: &mut impl JournalCheckpoint,
) -> Result<HostPreparationExecutionReport, HostPreparationExecutionError> {
    let disposition = decision.disposition();
    if disposition != HostPreparationCommandDisposition::Confirmed {
        return Err(HostPreparationExecutionError::decision_not_confirmed(
            disposition,
        ));
    }

    let proposal = decision.into_proposal();
    let source = proposal.source.identity.clone();
    let HostPreparationResult::Executable {
        phase,
        continuation_barriers,
        deferred_actions,
    } = proposal.result
    else {
        return Err(HostPreparationExecutionError::inconsistent_decision());
    };
    let phase_id = phase.id.clone();
    let journal = execute_lane_plan_durably(phase.into_durable_plan(), runner, checkpoint, true)
        .map_err(map_durable_error)?;
    let disposition = if journal.completed() {
        if continuation_barriers.is_empty() {
            HostPreparationExecutionDisposition::Completed
        } else {
            HostPreparationExecutionDisposition::FreshObservationRequired
        }
    } else {
        HostPreparationExecutionDisposition::ActionFailed
    };
    Ok(HostPreparationExecutionReport {
        schema_version: HOST_PREPARATION_EXECUTION_SCHEMA_VERSION,
        source,
        phase_id,
        disposition,
        journal,
        continuation_barriers,
        deferred_actions,
    })
}

#[must_use]
pub fn render_human(report: &HostPreparationExecutionReport) -> String {
    let mut output = format!(
        "SmolRunner host preparation execution\n\nSource: {} schema {} for {}\nPhase: {}\n",
        report.source.kind, report.source.schema_version, report.source.repository, report.phase_id
    );
    for record in &report.journal.records {
        output.push_str(&format!("- {}: {:?}\n", record.action.id, record.outcome));
        if let Some(message) = &record.message {
            output.push_str(&format!("  {message}\n"));
        }
    }
    match report.disposition {
        HostPreparationExecutionDisposition::Completed => {
            output.push_str("\n[COMPLETED] The confirmed phase reached terminal success.\n");
        }
        HostPreparationExecutionDisposition::ActionFailed => {
            output.push_str(
                "\n[STOPPED] An action failed. Re-observe exact host state before planning recovery.\n",
            );
        }
        HostPreparationExecutionDisposition::FreshObservationRequired => {
            output.push_str(
                "\n[FRESH OBSERVATION] The phase completed and planning stops at the reviewed observation barrier.\n",
            );
        }
    }
    output
}

fn map_durable_error(error: DurableExecutionError) -> HostPreparationExecutionError {
    match error {
        DurableExecutionError::Plan(error) => {
            HostPreparationExecutionError::durable_plan(error.problems)
        }
        DurableExecutionError::Checkpoint(error) => {
            HostPreparationExecutionError::journal_checkpoint(*error)
        }
    }
}

impl fmt::Display for HostPreparationExecutionReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&render_human(self))
    }
}

#[cfg(test)]
mod tests;
