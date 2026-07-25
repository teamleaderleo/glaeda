use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::Serialize;

use crate::durable_journal::{
    DurableExecutionError, JournalCheckpoint, execute_plan_durably,
};
use crate::journal::{
    ActionFailure, ActionReceipt, ExecutionJournal, ExecutionLane, MutationExecutor,
    PlannedMutation, RollbackClass, validate_plan,
};
use crate::lane_command::{LaneCommand, LaneCommandKind};
use crate::lane_executor::{
    LaneExecutionError, LaneExecutionErrorKind, LaneExecutionRecord, RootLaneExecutor,
    RunnerUserLaneExecutor,
};
use crate::runner_user::VerifiedRunnerUser;

/// Public validation failure for one immutable action-to-command binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DurableLanePlanError {
    pub problems: Vec<String>,
}

impl fmt::Display for DurableLanePlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "durable lane plan validation failed")?;
        for problem in &self.problems {
            writeln!(formatter, "- {problem}")?;
        }
        Ok(())
    }
}

impl std::error::Error for DurableLanePlanError {}

/// A reviewed mutation plan bound one-to-one to typed lane commands.
///
/// The first durable execution slice accepts only irreversible actions. SmolRunner does not yet
/// claim that package, account, mapping, linger, or rootless Podman changes can be automatically
/// rolled back safely.
#[derive(Debug, Clone)]
pub struct DurableLanePlan {
    actions: Vec<PlannedMutation>,
    commands: BTreeMap<String, LaneCommand>,
}

impl DurableLanePlan {
    /// Validate and bind immutable journal actions to exact typed lane commands.
    ///
    /// # Errors
    ///
    /// Returns bounded problems for invalid actions, missing or extra commands, duplicate command
    /// IDs, lane mismatches, or any action that claims an automatic rollback class.
    pub fn new(
        actions: Vec<PlannedMutation>,
        commands: Vec<LaneCommand>,
    ) -> Result<Self, DurableLanePlanError> {
        let mut problems = validate_plan(&actions).err().unwrap_or_default();
        let action_ids = actions
            .iter()
            .map(|action| action.id.as_str())
            .collect::<BTreeSet<_>>();

        for action in &actions {
            if action.rollback != RollbackClass::Irreversible {
                problems.push(format!(
                    "action {:?} must be classified irreversible until automatic host rollback is implemented",
                    action.id
                ));
            }
        }

        let mut commands_by_id = BTreeMap::new();
        for command in commands {
            let action_id = command.action_id().to_owned();
            if commands_by_id.insert(action_id.clone(), command).is_some() {
                problems.push(format!("duplicate lane command for action {action_id:?}"));
            }
        }

        for action in &actions {
            match commands_by_id.get(&action.id) {
                None => problems.push(format!(
                    "action {:?} has no bound reviewed lane command",
                    action.id
                )),
                Some(command) if command.lane() != action.lane => problems.push(format!(
                    "action {:?} is assigned to {:?}, but its bound command is assigned to {:?}",
                    action.id,
                    action.lane,
                    command.lane()
                )),
                Some(_) => {}
            }
        }

        for command_id in commands_by_id.keys() {
            if !action_ids.contains(command_id.as_str()) {
                problems.push(format!(
                    "lane command {command_id:?} has no matching journal action"
                ));
            }
        }

        if problems.is_empty() {
            Ok(Self {
                actions,
                commands: commands_by_id,
            })
        } else {
            Err(DurableLanePlanError { problems })
        }
    }

    fn into_parts(self) -> (Vec<PlannedMutation>, BTreeMap<String, LaneCommand>) {
        (self.actions, self.commands)
    }
}

/// Narrow execution boundary used by the durable journal adapter.
///
/// Implementations receive only a previously validated typed command and must return only bounded
/// public receipt or failure text. Raw stdout, stderr, environment values, and operating-system
/// errors remain below this boundary.
pub trait LaneCommandRunner {
    fn run(&mut self, command: &LaneCommand) -> Result<ActionReceipt, ActionFailure>;
}

/// Production router for the existing typed root and runner-user executors.
#[derive(Debug, Clone, Copy)]
pub struct SystemLaneCommandRunner<'a> {
    runner_user: Option<&'a VerifiedRunnerUser>,
}

impl SystemLaneCommandRunner<'_> {
    #[must_use]
    pub const fn root_only() -> Self {
        Self { runner_user: None }
    }
}

impl<'a> SystemLaneCommandRunner<'a> {
    #[must_use]
    pub const fn with_runner_user(runner_user: &'a VerifiedRunnerUser) -> Self {
        Self {
            runner_user: Some(runner_user),
        }
    }
}

impl LaneCommandRunner for SystemLaneCommandRunner<'_> {
    fn run(&mut self, command: &LaneCommand) -> Result<ActionReceipt, ActionFailure> {
        match command.lane() {
            ExecutionLane::Root => map_lane_execution(RootLaneExecutor::system().execute(command)),
            ExecutionLane::RunnerUser => {
                let Some(runner_user) = self.runner_user else {
                    return Err(ActionFailure::public(
                        "runner_evidence_unavailable",
                        "runner-user lane execution requires exact verified account, runtime-directory, and subordinate-ID evidence; re-observe before retry",
                    ));
                };
                map_lane_execution(
                    RunnerUserLaneExecutor::system().execute(command, runner_user),
                )
            }
            ExecutionLane::Operator | ExecutionLane::Github => Err(ActionFailure::public(
                "unsupported_lane",
                "durable host mutation accepts only reviewed root or runner-user lane commands",
            )),
        }
    }
}

/// Execute a validated lane-command plan through the existing durable checkpoint state machine.
///
/// The all-pending journal is persisted first. Each command is then persisted as `executing` before
/// the runner is called and as a terminal result afterward. If the process is interrupted after the
/// before-execute checkpoint, durable recovery sees `executing` and must re-observe host state.
///
/// # Errors
///
/// Returns the existing durable plan or checkpoint error. Lane process failures are represented as
/// bounded failed journal records rather than returned as raw process errors.
pub fn execute_lane_plan_durably(
    plan: DurableLanePlan,
    runner: &mut impl LaneCommandRunner,
    checkpoint: &mut impl JournalCheckpoint,
    allow_irreversible: bool,
) -> Result<ExecutionJournal, DurableExecutionError> {
    let (actions, commands) = plan.into_parts();
    let mut executor = BoundLaneMutationExecutor {
        commands: &commands,
        runner,
    };
    execute_plan_durably(actions, &mut executor, checkpoint, allow_irreversible)
}

struct BoundLaneMutationExecutor<'a, R> {
    commands: &'a BTreeMap<String, LaneCommand>,
    runner: &'a mut R,
}

impl<R: LaneCommandRunner> MutationExecutor for BoundLaneMutationExecutor<'_, R> {
    fn execute(&mut self, action: &PlannedMutation) -> Result<ActionReceipt, ActionFailure> {
        let Some(command) = self.commands.get(&action.id) else {
            return Err(binding_failure());
        };
        if command.action_id() != action.id || command.lane() != action.lane {
            return Err(binding_failure());
        }
        self.runner.run(command)
    }

    fn rollback(
        &mut self,
        _action: &PlannedMutation,
        _receipt: &ActionReceipt,
    ) -> Result<ActionReceipt, ActionFailure> {
        Err(ActionFailure::public(
            "automatic_rollback_unavailable",
            "automatic host rollback is unavailable; re-observe exact host state before planning recovery",
        ))
    }
}

fn binding_failure() -> ActionFailure {
    ActionFailure::public(
        "lane_binding_mismatch",
        "durable lane action no longer matches its reviewed command; no command was executed",
    )
}

fn map_lane_execution(
    result: Result<LaneExecutionRecord, LaneExecutionError>,
) -> Result<ActionReceipt, ActionFailure> {
    match result {
        Ok(record) if record.success() => Ok(ActionReceipt::public(format!(
            "reviewed {} command completed",
            command_kind_name(record.kind())
        ))),
        Ok(_) => Err(ActionFailure::public(
            "lane_process_failed",
            "reviewed lane command exited unsuccessfully; host state must be re-observed before retry",
        )),
        Err(error) => Err(ActionFailure::public(
            lane_error_code(error.kind()),
            error.message().to_owned(),
        )),
    }
}

const fn lane_error_code(kind: LaneExecutionErrorKind) -> &'static str {
    match kind {
        LaneExecutionErrorKind::LaneMismatch => "lane_mismatch",
        LaneExecutionErrorKind::InvalidCommand => "invalid_lane_command",
        LaneExecutionErrorKind::InvalidRunnerEvidence => "invalid_runner_evidence",
        LaneExecutionErrorKind::UnsupportedPrivilege => "unsupported_privilege",
        LaneExecutionErrorKind::ExecutableVerification => "untrusted_executable",
        LaneExecutionErrorKind::Process => "lane_process_unknown",
    }
}

const fn command_kind_name(kind: LaneCommandKind) -> &'static str {
    match kind {
        LaneCommandKind::AptInstall => "package installation",
        LaneCommandKind::EnsureSystemGroup => "system group",
        LaneCommandKind::EnsureSystemUser => "system user",
        LaneCommandKind::EnsureHomeDirectory => "home directory",
        LaneCommandKind::EnsureSubordinateUids => "subordinate UID",
        LaneCommandKind::EnsureSubordinateGids => "subordinate GID",
        LaneCommandKind::EnableLinger => "linger",
        LaneCommandKind::RunnerPodmanInfo => "runner Podman information",
        LaneCommandKind::RunnerPodmanMigrate => "runner Podman migration",
        LaneCommandKind::RunnerGitVersion => "runner Git version",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use crate::durable_journal::{
        DurableExecutionError, JournalCheckpoint, JournalCheckpointFailure,
    };
    use crate::journal::{
        ActionFailure, ActionOutcome, ActionReceipt, ExecutionJournal, ExecutionLane,
        PlannedMutation, Preconditions, RollbackClass,
    };
    use crate::lane_command::{LaneCommand, LinuxAccountName, RunnerUserContext};

    use super::{
        DurableLanePlan, LaneCommandRunner, SystemLaneCommandRunner,
        execute_lane_plan_durably,
    };

    #[derive(Default)]
    struct FakeRunner {
        calls: Vec<String>,
        failures: BTreeSet<String>,
        private_secret: String,
    }

    impl LaneCommandRunner for FakeRunner {
        fn run(&mut self, command: &LaneCommand) -> Result<ActionReceipt, ActionFailure> {
            self.calls.push(command.action_id().to_owned());
            let _secret_remains_below_the_journal = !self.private_secret.is_empty();
            if self.failures.contains(command.action_id()) {
                Err(ActionFailure::public(
                    "bounded_lane_failure",
                    "bounded lane failure",
                ))
            } else {
                Ok(ActionReceipt::public(format!(
                    "completed {}",
                    command.action_id()
                )))
            }
        }
    }

    #[derive(Default)]
    struct RecordingCheckpoint {
        snapshots: Vec<ExecutionJournal>,
        fail_on_call: Option<usize>,
        calls: usize,
    }

    impl JournalCheckpoint for RecordingCheckpoint {
        fn checkpoint(
            &mut self,
            journal: &ExecutionJournal,
        ) -> Result<(), JournalCheckpointFailure> {
            let call = self.calls;
            self.calls += 1;
            if self.fail_on_call == Some(call) {
                return Err(JournalCheckpointFailure::public(
                    "bounded checkpoint failure",
                ));
            }
            self.snapshots.push(journal.clone());
            Ok(())
        }
    }

    fn action(id: &str, lane: ExecutionLane, rollback: RollbackClass) -> PlannedMutation {
        PlannedMutation::new(
            id,
            lane,
            format!("perform {id}"),
            rollback,
            Preconditions::new([format!("fresh evidence for {id}")]),
        )
    }

    fn root_command(action: &PlannedMutation) -> LaneCommand {
        LaneCommand::ensure_system_group(
            action,
            &LinuxAccountName::parse("project-runner").expect("group"),
        )
        .expect("root command")
    }

    #[test]
    fn bindings_reject_missing_extra_duplicate_lane_and_rollback_mismatches() {
        let root = action("root", ExecutionLane::Root, RollbackClass::Irreversible);
        let extra = action("extra", ExecutionLane::Root, RollbackClass::Irreversible);
        let reversible = action("reversible", ExecutionLane::Root, RollbackClass::Reversible);
        let runner_lane = action(
            "root",
            ExecutionLane::RunnerUser,
            RollbackClass::Irreversible,
        );
        let root_command = root_command(&root);
        let extra_command = root_command(&extra);

        let missing = DurableLanePlan::new(vec![root.clone()], Vec::new()).expect_err("missing");
        assert!(missing.problems.iter().any(|problem| problem.contains("no bound")));

        let extra_result = DurableLanePlan::new(
            vec![root.clone()],
            vec![root_command.clone(), extra_command],
        )
        .expect_err("extra");
        assert!(extra_result.problems.iter().any(|problem| problem.contains("no matching")));

        let duplicate = DurableLanePlan::new(
            vec![root.clone()],
            vec![root_command.clone(), root_command.clone()],
        )
        .expect_err("duplicate");
        assert!(duplicate.problems.iter().any(|problem| problem.contains("duplicate")));

        let lane = DurableLanePlan::new(vec![runner_lane], vec![root_command.clone()])
            .expect_err("lane mismatch");
        assert!(lane.problems.iter().any(|problem| problem.contains("assigned")));

        let rollback = DurableLanePlan::new(vec![reversible.clone()], vec![root_command(&reversible)])
            .expect_err("rollback");
        assert!(rollback.problems.iter().any(|problem| problem.contains("irreversible")));
    }

    #[test]
    fn success_checkpoints_pending_executing_and_completed_in_action_order() {
        let one = action("one", ExecutionLane::Root, RollbackClass::Irreversible);
        let two = action("two", ExecutionLane::Root, RollbackClass::Irreversible);
        let plan = DurableLanePlan::new(
            vec![one.clone(), two.clone()],
            vec![root_command(&two), root_command(&one)],
        )
        .expect("plan");
        let mut runner = FakeRunner::default();
        let mut checkpoint = RecordingCheckpoint::default();

        let journal = execute_lane_plan_durably(plan, &mut runner, &mut checkpoint, true)
            .expect("durable execution");

        assert_eq!(runner.calls, ["one", "two"]);
        assert!(journal.completed());
        assert_eq!(checkpoint.snapshots[0].records[0].outcome, ActionOutcome::Pending);
        assert_eq!(checkpoint.snapshots[1].records[0].outcome, ActionOutcome::Executing);
        assert_eq!(checkpoint.snapshots[2].records[0].outcome, ActionOutcome::Completed);
        assert_eq!(checkpoint.snapshots[3].records[1].outcome, ActionOutcome::Executing);
        assert_eq!(checkpoint.snapshots[4].records[1].outcome, ActionOutcome::Completed);
    }

    #[test]
    fn lane_failure_is_terminal_and_redacted_in_the_durable_journal() {
        let mutation = action("fail", ExecutionLane::Root, RollbackClass::Irreversible);
        let plan = DurableLanePlan::new(vec![mutation.clone()], vec![root_command(&mutation)])
            .expect("plan");
        let mut runner = FakeRunner {
            failures: BTreeSet::from(["fail".to_owned()]),
            private_secret: "token=private-secret".to_owned(),
            ..FakeRunner::default()
        };
        let mut checkpoint = RecordingCheckpoint::default();

        let journal = execute_lane_plan_durably(plan, &mut runner, &mut checkpoint, true)
            .expect("failed action is journaled");

        assert_eq!(journal.records[0].outcome, ActionOutcome::Failed);
        assert_eq!(checkpoint.snapshots.len(), 3);
        let json = serde_json::to_string(&journal).expect("serialize journal");
        assert!(json.contains("bounded_lane_failure"));
        assert!(!json.contains("private-secret"));
        assert!(!json.contains("token="));
        assert!(!json.contains("stdout"));
        assert!(!json.contains("stderr"));
        assert!(!json.contains("environment"));
    }

    #[test]
    fn failed_before_execute_checkpoint_prevents_runner_call() {
        let mutation = action("one", ExecutionLane::Root, RollbackClass::Irreversible);
        let plan = DurableLanePlan::new(vec![mutation.clone()], vec![root_command(&mutation)])
            .expect("plan");
        let mut runner = FakeRunner::default();
        let mut checkpoint = RecordingCheckpoint {
            fail_on_call: Some(1),
            ..RecordingCheckpoint::default()
        };

        let error = execute_lane_plan_durably(plan, &mut runner, &mut checkpoint, true)
            .expect_err("checkpoint fails");

        assert!(runner.calls.is_empty());
        let DurableExecutionError::Checkpoint(error) = error else {
            panic!("checkpoint error");
        };
        assert_eq!(error.last_durable().expect("initial snapshot").records[0].outcome, ActionOutcome::Pending);
        assert_eq!(error.attempted().records[0].outcome, ActionOutcome::Executing);
    }

    #[test]
    fn unconfirmed_irreversible_action_is_durably_skipped() {
        let mutation = action("one", ExecutionLane::Root, RollbackClass::Irreversible);
        let plan = DurableLanePlan::new(vec![mutation.clone()], vec![root_command(&mutation)])
            .expect("plan");
        let mut runner = FakeRunner::default();
        let mut checkpoint = RecordingCheckpoint::default();

        let journal = execute_lane_plan_durably(plan, &mut runner, &mut checkpoint, false)
            .expect("skip is durable");

        assert!(runner.calls.is_empty());
        assert_eq!(checkpoint.snapshots.len(), 1);
        assert_eq!(journal.records[0].outcome, ActionOutcome::Skipped);
    }

    #[test]
    fn runner_user_command_without_verified_evidence_fails_before_process_execution() {
        let mutation = action(
            "runner-git",
            ExecutionLane::RunnerUser,
            RollbackClass::Irreversible,
        );
        let context = RunnerUserContext::new(
            LinuxAccountName::parse("project-runner").expect("user"),
            1001,
            1001,
            "/var/lib/project-runner",
        )
        .expect("context");
        let command = LaneCommand::runner_git_version(&mutation, &context).expect("command");
        let mut runner = SystemLaneCommandRunner::root_only();

        let failure = runner.run(&command).expect_err("evidence required");

        assert_eq!(failure.code(), "runner_evidence_unavailable");
        assert!(failure.message().contains("re-observe"));
    }
}
