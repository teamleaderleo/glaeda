use std::fmt;

use serde::Serialize;

use crate::journal::{ActionFailure, ActionReceipt, ExecutionLane, PlannedMutation, RollbackClass};
use crate::lane_command::{LaneCommand, LaneCommandKind};
use crate::lane_executor::LaneExecutionErrorKind;
use crate::process::{ExecutionRecord, MAX_CAPTURED_STREAM_BYTES};

pub const DEBIAN_PACKAGE_RECOVERY_SCHEMA_VERSION: u8 = 1;

const NOT_STARTED_CODE: &str = "debian-package-install-not-started";
const NONZERO_CODE: &str = "debian-package-install-nonzero";
const UNCERTAIN_CODE: &str = "debian-package-install-uncertain";

#[derive(Debug, Clone, Copy)]
pub enum DebianPackageAttemptEvidence<'a> {
    ProcessRecord(&'a ExecutionRecord),
    LaneFailure(LaneExecutionErrorKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DebianPackageAttemptState {
    NotStarted,
    ExitedSuccessfully,
    ExitedNonzero,
    ExecutionUncertain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DebianPackageRecoveryStep {
    RepairPreconditionsAndReplan,
    ReobserveBeforeContinue,
    ReobserveBeforeRetry,
    ReobserveBeforeDecision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DebianPackageRecoveryReport {
    pub schema_version: u8,
    pub action_id: String,
    pub state: DebianPackageAttemptState,
    pub rollback_class: RollbackClass,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_status: Option<i32>,
    pub fresh_package_observation_required: bool,
    pub package_action_satisfied: bool,
    pub automatic_rollback_allowed: bool,
    pub next_step: DebianPackageRecoveryStep,
    pub public_summary: String,
}

impl DebianPackageRecoveryReport {
    /// Convert the process attempt classification into bounded journal material.
    ///
    /// A successful value records only the apt process attempt. It does not mean the package action
    /// is satisfied; callers must enforce `fresh_package_observation_required` before completion.
    pub fn attempt_journal_result(&self) -> Result<ActionReceipt, ActionFailure> {
        match self.state {
            DebianPackageAttemptState::ExitedSuccessfully => {
                Ok(ActionReceipt::public(self.public_summary.clone()))
            }
            DebianPackageAttemptState::NotStarted => Err(ActionFailure::public(
                NOT_STARTED_CODE,
                self.public_summary.clone(),
            )),
            DebianPackageAttemptState::ExitedNonzero => Err(ActionFailure::public(
                NONZERO_CODE,
                self.public_summary.clone(),
            )),
            DebianPackageAttemptState::ExecutionUncertain => Err(ActionFailure::public(
                UNCERTAIN_CODE,
                self.public_summary.clone(),
            )),
        }
    }
}

/// Classify one reviewed Debian-family package installation attempt for durable recovery.
///
/// A process exit is only attempt evidence. Every started `apt-get install` attempt requires a
/// fresh bounded package observation before dependent work or retry. Nonzero and uncertain attempts
/// may have partially changed host state. Automatic package removal is never treated as rollback.
///
/// # Errors
///
/// Returns an error when the action is not compensating root work, the command is not the reviewed
/// apt installation command for that action, or a supplied process record does not match the exact
/// command boundary and bounded-record contract.
pub fn classify_debian_package_attempt(
    action: &PlannedMutation,
    command: &LaneCommand,
    evidence: DebianPackageAttemptEvidence<'_>,
) -> Result<DebianPackageRecoveryReport, DebianPackageRecoveryError> {
    validate_boundary(action, command)?;

    let (state, exit_status, observation_required, next_step, public_summary) = match evidence {
        DebianPackageAttemptEvidence::ProcessRecord(record) => {
            validate_process_record(command, record)?;
            classify_process_record(record)
        }
        DebianPackageAttemptEvidence::LaneFailure(kind) => classify_lane_failure(kind),
    };

    Ok(DebianPackageRecoveryReport {
        schema_version: DEBIAN_PACKAGE_RECOVERY_SCHEMA_VERSION,
        action_id: action.id.clone(),
        state,
        rollback_class: RollbackClass::Compensating,
        exit_status,
        fresh_package_observation_required: observation_required,
        package_action_satisfied: false,
        automatic_rollback_allowed: false,
        next_step,
        public_summary,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DebianPackageRecoveryError {
    pub problems: Vec<String>,
}

impl DebianPackageRecoveryError {
    fn single(problem: impl Into<String>) -> Self {
        Self {
            problems: vec![problem.into()],
        }
    }
}

impl fmt::Display for DebianPackageRecoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "Debian package recovery classification failed")?;
        for problem in &self.problems {
            writeln!(formatter, "- {problem}")?;
        }
        Ok(())
    }
}

impl std::error::Error for DebianPackageRecoveryError {}

fn validate_boundary(
    action: &PlannedMutation,
    command: &LaneCommand,
) -> Result<(), DebianPackageRecoveryError> {
    let mut problems = Vec::new();
    if action.id != command.action_id() {
        problems.push("package action and command identities do not match".to_owned());
    }
    if action.lane != ExecutionLane::Root || command.lane() != ExecutionLane::Root {
        problems.push("Debian package installation must use the root execution lane".to_owned());
    }
    if action.rollback != RollbackClass::Compensating {
        problems.push("Debian package installation must be classified as compensating".to_owned());
    }
    if command.kind() != LaneCommandKind::AptInstall {
        problems.push(
            "recovery classification accepts only the reviewed apt install command".to_owned(),
        );
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(DebianPackageRecoveryError { problems })
    }
}

fn validate_process_record(
    command: &LaneCommand,
    record: &ExecutionRecord,
) -> Result<(), DebianPackageRecoveryError> {
    let expected_environment_keys = command
        .spec()
        .environment
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    if record.argv != command.spec().displayed_argv()
        || record.environment_keys != expected_environment_keys
    {
        return Err(DebianPackageRecoveryError::single(
            "package process record does not match the reviewed command boundary",
        ));
    }
    if record.stdout.len() > MAX_CAPTURED_STREAM_BYTES
        || record.stderr.len() > MAX_CAPTURED_STREAM_BYTES
    {
        return Err(DebianPackageRecoveryError::single(
            "package process record exceeds the bounded output contract",
        ));
    }
    Ok(())
}

fn classify_process_record(
    record: &ExecutionRecord,
) -> (
    DebianPackageAttemptState,
    Option<i32>,
    bool,
    DebianPackageRecoveryStep,
    String,
) {
    match (record.status, record.success) {
        (Some(0), true) => (
            DebianPackageAttemptState::ExitedSuccessfully,
            Some(0),
            true,
            DebianPackageRecoveryStep::ReobserveBeforeContinue,
            "apt-get exited successfully; dependent work remains blocked until a fresh package observation confirms the desired state"
                .to_owned(),
        ),
        (Some(status @ 1..=255), false) => (
            DebianPackageAttemptState::ExitedNonzero,
            Some(status),
            true,
            DebianPackageRecoveryStep::ReobserveBeforeRetry,
            format!(
                "apt-get exited with status {status}; package state may be partially changed and must be re-observed before a new plan or retry"
            ),
        ),
        (status, _) => (
            DebianPackageAttemptState::ExecutionUncertain,
            status,
            true,
            DebianPackageRecoveryStep::ReobserveBeforeDecision,
            "apt-get completion evidence is inconsistent or interrupted; package state may have changed and must be re-observed before any decision"
                .to_owned(),
        ),
    }
}

fn classify_lane_failure(
    kind: LaneExecutionErrorKind,
) -> (
    DebianPackageAttemptState,
    Option<i32>,
    bool,
    DebianPackageRecoveryStep,
    String,
) {
    match kind {
        LaneExecutionErrorKind::Process => (
            DebianPackageAttemptState::ExecutionUncertain,
            None,
            true,
            DebianPackageRecoveryStep::ReobserveBeforeDecision,
            "apt-get did not produce a complete bounded process record; host state may have changed and must be re-observed before any decision"
                .to_owned(),
        ),
        LaneExecutionErrorKind::LaneMismatch
        | LaneExecutionErrorKind::InvalidCommand
        | LaneExecutionErrorKind::InvalidRunnerEvidence
        | LaneExecutionErrorKind::UnsupportedPrivilege
        | LaneExecutionErrorKind::ExecutableVerification => (
            DebianPackageAttemptState::NotStarted,
            None,
            false,
            DebianPackageRecoveryStep::RepairPreconditionsAndReplan,
            "apt-get was not started because a pre-execution boundary check failed; repair the boundary and regenerate the plan"
                .to_owned(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use crate::journal::{ExecutionLane, PlannedMutation, Preconditions, RollbackClass};
    use crate::lane_command::{LaneCommand, LinuxAccountName, PackageName};
    use crate::lane_executor::LaneExecutionErrorKind;
    use crate::process::{ExecutionRecord, MAX_CAPTURED_STREAM_BYTES};

    use super::{
        DebianPackageAttemptEvidence, DebianPackageAttemptState, DebianPackageRecoveryStep,
        classify_debian_package_attempt,
    };

    fn action(rollback: RollbackClass) -> PlannedMutation {
        PlannedMutation::new(
            "install-debian-host-prerequisites",
            ExecutionLane::Root,
            "install Debian host prerequisites",
            rollback,
            Preconditions::new(["packages observed absent"]),
        )
    }

    fn command(action: &PlannedMutation) -> LaneCommand {
        LaneCommand::apt_install(
            action,
            &[
                PackageName::parse("podman").expect("package"),
                PackageName::parse("uidmap").expect("package"),
            ],
        )
        .expect("apt command")
    }

    fn record(command: &LaneCommand, status: Option<i32>, success: bool) -> ExecutionRecord {
        ExecutionRecord {
            argv: command.spec().displayed_argv(),
            environment_keys: Vec::new(),
            status,
            success,
            stdout: "private apt output must not enter the recovery report".to_owned(),
            stderr: "private apt diagnostics must not enter the recovery report".to_owned(),
        }
    }

    #[test]
    fn successful_exit_requires_fresh_observation_before_dependent_work() {
        let action = action(RollbackClass::Compensating);
        let command = command(&action);
        let process = record(&command, Some(0), true);
        let report = classify_debian_package_attempt(
            &action,
            &command,
            DebianPackageAttemptEvidence::ProcessRecord(&process),
        )
        .expect("success report");
        assert_eq!(report.state, DebianPackageAttemptState::ExitedSuccessfully);
        assert!(report.fresh_package_observation_required);
        assert!(!report.package_action_satisfied);
        assert!(!report.automatic_rollback_allowed);
        assert_eq!(
            report.next_step,
            DebianPackageRecoveryStep::ReobserveBeforeContinue
        );
        assert!(report.attempt_journal_result().is_ok());
    }

    #[test]
    fn nonzero_exit_requires_reobservation_before_retry() {
        let action = action(RollbackClass::Compensating);
        let command = command(&action);
        let process = record(&command, Some(100), false);
        let report = classify_debian_package_attempt(
            &action,
            &command,
            DebianPackageAttemptEvidence::ProcessRecord(&process),
        )
        .expect("nonzero report");
        assert_eq!(report.state, DebianPackageAttemptState::ExitedNonzero);
        assert_eq!(report.exit_status, Some(100));
        assert!(report.fresh_package_observation_required);
        assert_eq!(
            report.next_step,
            DebianPackageRecoveryStep::ReobserveBeforeRetry
        );
        let failure = report
            .attempt_journal_result()
            .expect_err("journal failure");
        assert_eq!(failure.code(), "debian-package-install-nonzero");
    }

    #[test]
    fn interrupted_or_inconsistent_evidence_is_uncertain() {
        let action = action(RollbackClass::Compensating);
        let command = command(&action);
        for process in [
            record(&command, None, false),
            record(&command, Some(0), false),
        ] {
            let report = classify_debian_package_attempt(
                &action,
                &command,
                DebianPackageAttemptEvidence::ProcessRecord(&process),
            )
            .expect("uncertain report");
            assert_eq!(report.state, DebianPackageAttemptState::ExecutionUncertain);
            assert!(report.fresh_package_observation_required);
            assert_eq!(
                report.next_step,
                DebianPackageRecoveryStep::ReobserveBeforeDecision
            );
        }

        let report = classify_debian_package_attempt(
            &action,
            &command,
            DebianPackageAttemptEvidence::LaneFailure(LaneExecutionErrorKind::Process),
        )
        .expect("process failure report");
        assert_eq!(report.state, DebianPackageAttemptState::ExecutionUncertain);
    }

    #[test]
    fn pre_execution_failures_are_not_mislabeled_as_host_mutation() {
        let action = action(RollbackClass::Compensating);
        let command = command(&action);
        for kind in [
            LaneExecutionErrorKind::LaneMismatch,
            LaneExecutionErrorKind::InvalidCommand,
            LaneExecutionErrorKind::InvalidRunnerEvidence,
            LaneExecutionErrorKind::UnsupportedPrivilege,
            LaneExecutionErrorKind::ExecutableVerification,
        ] {
            let report = classify_debian_package_attempt(
                &action,
                &command,
                DebianPackageAttemptEvidence::LaneFailure(kind),
            )
            .expect("not-started report");
            assert_eq!(report.state, DebianPackageAttemptState::NotStarted);
            assert!(!report.fresh_package_observation_required);
            assert_eq!(
                report.next_step,
                DebianPackageRecoveryStep::RepairPreconditionsAndReplan
            );
        }
    }

    #[test]
    fn boundary_and_record_mismatches_fail_before_classification() {
        let reversible = action(RollbackClass::Reversible);
        let reversible_command = command(&reversible);
        classify_debian_package_attempt(
            &reversible,
            &reversible_command,
            DebianPackageAttemptEvidence::LaneFailure(LaneExecutionErrorKind::Process),
        )
        .expect_err("reversible apt action must fail");

        let action = action(RollbackClass::Compensating);
        let command = command(&action);
        let mut process = record(&command, Some(0), true);
        process.argv.push("unexpected".to_owned());
        classify_debian_package_attempt(
            &action,
            &command,
            DebianPackageAttemptEvidence::ProcessRecord(&process),
        )
        .expect_err("mismatched argv must fail");

        let mut oversized = record(&command, Some(0), true);
        oversized.stdout = "x".repeat(MAX_CAPTURED_STREAM_BYTES + 1);
        classify_debian_package_attempt(
            &action,
            &command,
            DebianPackageAttemptEvidence::ProcessRecord(&oversized),
        )
        .expect_err("oversized record must fail");
    }

    #[test]
    fn serialized_report_excludes_process_output() {
        let action = action(RollbackClass::Compensating);
        let command = command(&action);
        let process = record(&command, Some(100), false);
        let report = classify_debian_package_attempt(
            &action,
            &command,
            DebianPackageAttemptEvidence::ProcessRecord(&process),
        )
        .expect("report");
        let json = serde_json::to_string(&report).expect("serialize report");
        assert!(!json.contains("private apt output"));
        assert!(!json.contains("private apt diagnostics"));
    }

    #[test]
    fn non_apt_command_is_rejected() {
        let action = action(RollbackClass::Compensating);
        let account = LinuxAccountName::parse("project-runner").expect("account");
        let command = LaneCommand::ensure_system_group(&action, &account).expect("group command");
        classify_debian_package_attempt(
            &action,
            &command,
            DebianPackageAttemptEvidence::LaneFailure(LaneExecutionErrorKind::Process),
        )
        .expect_err("non-apt command must fail");
    }
}
