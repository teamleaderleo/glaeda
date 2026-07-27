use std::fmt;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::execution_admission::EpochMillis;
use crate::lima_lifecycle::{
    LimaInstanceIdentity, LimaLifecycleObservation, LimaLifecycleState, LimaProfileGeneration,
    LimaResourceEnvelope, LimaResourceProfile,
};
use crate::lima_observation::{
    LimaGuestObservation, LimaInstanceName, LimaInstanceObservationReport, LimaObservationAdapter,
    LimaObservationClock, LimaObservationFreshness, LimaObservationRequest, LimaPersistentIdentity,
    LimaRuntimeState,
};
use crate::personal_worker_host_broker::{
    HostBrokerAction, HostBrokerPlan, HostBrokerStateRevision,
    PERSONAL_WORKER_HOST_BROKER_SCHEMA_VERSION,
};
use crate::personal_worker_queue::{PersonalWorkerProfile, PersonalWorkerQueueGeneration};
use crate::process::{CommandExecutor, CommandSpec, ExecutionRecord};

pub const LIMA_LIFECYCLE_EXECUTOR_SCHEMA_VERSION: u8 = 1;
pub const MAX_LIMA_LIFECYCLE_ACTION_AGE_MILLIS: u64 = 30_000;
pub const MAX_LIMA_LIFECYCLE_EXECUTOR_OUTPUT_BYTES: usize = 65_536;

const MAX_PRIVATE_PATH_BYTES: usize = 1_024;
const REDACTED_EXECUTION_EVIDENCE: &str = "<private-lima-lifecycle-execution-evidence>";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LimaLifecycleExecutionPhase {
    InputValidation,
    Stop,
    Edit,
    Start,
    Verify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LimaLifecycleExecutionRefusalCode {
    InvalidInput,
    ClockFailure,
    ExpiredAction,
    UnsupportedAction,
    StaleObservation,
    IdentityMismatch,
    GenerationMismatch,
    ActiveReservation,
    StateMismatch,
    ProfileMismatch,
    ResourceMismatch,
    PersistentIdentityMismatch,
    CommandFailed,
    CommandIdentityMismatch,
    UnboundedOutput,
    VerificationFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExecutionProblem {
    code: LimaLifecycleExecutionRefusalCode,
    phase: LimaLifecycleExecutionPhase,
    message: &'static str,
}

impl ExecutionProblem {
    const fn new(
        code: LimaLifecycleExecutionRefusalCode,
        phase: LimaLifecycleExecutionPhase,
        message: &'static str,
    ) -> Self {
        Self {
            code,
            phase,
            message,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct LimaLifecycleExecutionFailure {
    pub code: LimaLifecycleExecutionRefusalCode,
    pub phase: LimaLifecycleExecutionPhase,
    pub public_message: &'static str,
    #[serde(skip)]
    private_evidence: LimaLifecycleExecutionPrivateEvidence,
}

impl LimaLifecycleExecutionFailure {
    fn from_problem(
        problem: ExecutionProblem,
        private_evidence: LimaLifecycleExecutionPrivateEvidence,
    ) -> Self {
        Self {
            code: problem.code,
            phase: problem.phase,
            public_message: problem.message,
            private_evidence,
        }
    }

    #[must_use]
    pub const fn private_evidence(&self) -> &LimaLifecycleExecutionPrivateEvidence {
        &self.private_evidence
    }
}

impl fmt::Debug for LimaLifecycleExecutionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LimaLifecycleExecutionFailure")
            .field("code", &self.code)
            .field("phase", &self.phase)
            .field("public_message", &self.public_message)
            .field("private_evidence", &REDACTED_EXECUTION_EVIDENCE)
            .finish()
    }
}

impl fmt::Display for LimaLifecycleExecutionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.public_message)
    }
}

impl std::error::Error for LimaLifecycleExecutionFailure {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum LimaLifecycleExecutionAction {
    Start,
    Stop {
        target_after_stop: PersonalWorkerProfile,
    },
    ChangeProfile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LimaLifecycleExecutionReceipt {
    pub schema_version: u8,
    pub broker_state_revision: HostBrokerStateRevision,
    pub queue_generation: PersonalWorkerQueueGeneration,
    pub decision_at: EpochMillis,
    pub action: LimaLifecycleExecutionAction,
    pub identity: LimaInstanceIdentity,
    pub before_state: LimaLifecycleState,
    pub after_state: LimaLifecycleState,
    pub before_profile: LimaResourceProfile,
    pub after_profile: LimaResourceProfile,
    pub before_generation: LimaProfileGeneration,
    pub after_generation: LimaProfileGeneration,
    pub before_resources: LimaResourceEnvelope,
    pub after_resources: LimaResourceEnvelope,
    pub primary_disk_bytes: u64,
    pub persistent_identity: LimaPersistentIdentity,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct LimaLifecycleExecution {
    #[serde(flatten)]
    receipt: LimaLifecycleExecutionReceipt,
    #[serde(skip)]
    post_observation: LimaInstanceObservationReport,
    #[serde(skip)]
    private_evidence: LimaLifecycleExecutionPrivateEvidence,
}

impl LimaLifecycleExecution {
    #[must_use]
    pub const fn receipt(&self) -> &LimaLifecycleExecutionReceipt {
        &self.receipt
    }

    #[must_use]
    pub const fn post_observation(&self) -> &LimaInstanceObservationReport {
        &self.post_observation
    }

    #[must_use]
    pub const fn private_evidence(&self) -> &LimaLifecycleExecutionPrivateEvidence {
        &self.private_evidence
    }
}

impl fmt::Debug for LimaLifecycleExecution {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LimaLifecycleExecution")
            .field("receipt", &self.receipt)
            .field("post_observation", &self.post_observation)
            .field("private_evidence", &REDACTED_EXECUTION_EVIDENCE)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Default)]
pub struct LimaLifecycleExecutionPrivateEvidence {
    commands: Vec<LimaLifecyclePrivateCommandEvidence>,
}

impl LimaLifecycleExecutionPrivateEvidence {
    #[must_use]
    pub fn commands(&self) -> &[LimaLifecyclePrivateCommandEvidence] {
        &self.commands
    }
}

impl fmt::Debug for LimaLifecycleExecutionPrivateEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LimaLifecycleExecutionPrivateEvidence")
            .field("command_count", &self.commands.len())
            .field("records", &REDACTED_EXECUTION_EVIDENCE)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct LimaLifecyclePrivateCommandEvidence {
    phase: LimaLifecycleExecutionPhase,
    record: ExecutionRecord,
}

impl LimaLifecyclePrivateCommandEvidence {
    #[must_use]
    pub const fn phase(&self) -> LimaLifecycleExecutionPhase {
        self.phase
    }

    #[must_use]
    pub const fn record(&self) -> &ExecutionRecord {
        &self.record
    }
}

impl fmt::Debug for LimaLifecyclePrivateCommandEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LimaLifecyclePrivateCommandEvidence")
            .field("phase", &self.phase)
            .field("record", &REDACTED_EXECUTION_EVIDENCE)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedLimaLifecycleAction {
    state_revision: HostBrokerStateRevision,
    queue_generation: PersonalWorkerQueueGeneration,
    decision_at: EpochMillis,
    action: HostBrokerAction,
}

impl AcceptedLimaLifecycleAction {
    /// Retain one executable action from a pure broker plan.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal unless the plan schema is current and its action is exactly
    /// `start`, `stop`, or `change_profile`.
    pub fn from_plan(plan: &HostBrokerPlan) -> Result<Self, LimaLifecycleExecutionFailure> {
        if plan.schema_version() != PERSONAL_WORKER_HOST_BROKER_SCHEMA_VERSION {
            return Err(input_failure(
                LimaLifecycleExecutionRefusalCode::InvalidInput,
                "the broker plan schema is not supported by the Lima executor",
            ));
        }
        if !matches!(
            plan.action(),
            HostBrokerAction::Start { .. }
                | HostBrokerAction::Stop { .. }
                | HostBrokerAction::ChangeProfile { .. }
        ) {
            return Err(input_failure(
                LimaLifecycleExecutionRefusalCode::UnsupportedAction,
                "the broker action is not executable by the Lima lifecycle executor",
            ));
        }
        Ok(Self {
            state_revision: plan.state_revision(),
            queue_generation: plan.queue_generation(),
            decision_at: plan.decision_at(),
            action: plan.action().clone(),
        })
    }

    #[must_use]
    pub const fn action(&self) -> &HostBrokerAction {
        &self.action
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimaLifecycleObservationSourceError;

pub trait LimaLifecycleObservationSource {
    /// Return one fresh exact Lima observation report through the reviewed observer boundary.
    ///
    /// # Errors
    ///
    /// Returns a content-minimised error when observation fails.
    fn observe<E, C>(
        &self,
        request: &LimaObservationRequest,
        executor: &E,
        clock: &C,
    ) -> Result<LimaInstanceObservationReport, LimaLifecycleObservationSourceError>
    where
        E: CommandExecutor,
        C: LimaObservationClock;
}

impl LimaLifecycleObservationSource for LimaObservationAdapter {
    fn observe<E, C>(
        &self,
        request: &LimaObservationRequest,
        executor: &E,
        clock: &C,
    ) -> Result<LimaInstanceObservationReport, LimaLifecycleObservationSourceError>
    where
        E: CommandExecutor,
        C: LimaObservationClock,
    {
        LimaObservationAdapter::observe(self, request, executor, clock)
            .map(|observation| observation.report().clone())
            .map_err(|_| LimaLifecycleObservationSourceError)
    }
}

pub struct LimaLifecycleExecutionInput<'a> {
    pub accepted: &'a AcceptedLimaLifecycleAction,
    pub lifecycle: &'a LimaLifecycleObservation,
    pub current: &'a LimaInstanceObservationReport,
    pub expected_persistent_identity: &'a LimaPersistentIdentity,
    pub observation_request: &'a LimaObservationRequest,
}

#[derive(Clone, PartialEq, Eq)]
pub struct LimaLifecycleExecutor {
    limactl_program: PathBuf,
    lima_home: PathBuf,
    instance: LimaInstanceName,
}

impl LimaLifecycleExecutor {
    /// Construct the fixed direct-command executor for one reviewed Lima instance.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal unless the program and private Lima home are exact absolute paths.
    pub fn new(
        limactl_program: impl Into<PathBuf>,
        lima_home: impl Into<PathBuf>,
        instance: LimaInstanceName,
    ) -> Result<Self, LimaLifecycleExecutionFailure> {
        let limactl_program =
            validate_private_absolute_path(limactl_program.into()).map_err(|problem| {
                LimaLifecycleExecutionFailure::from_problem(problem, Default::default())
            })?;
        let lima_home = validate_private_absolute_path(lima_home.into()).map_err(|problem| {
            LimaLifecycleExecutionFailure::from_problem(problem, Default::default())
        })?;
        Ok(Self {
            limactl_program,
            lima_home,
            instance,
        })
    }

    /// Execute one accepted broker lifecycle action and verify the exact resulting Lima state.
    ///
    /// This method accepts no generic resource values, shell command, queue decision, clock policy,
    /// GitHub authority, runner registration, cache mutation, or arbitrary environment.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal before mutation for identity, generation, state, resource, or
    /// reservation drift and after mutation for command or fresh-observation failure.
    pub fn execute<O, E, C>(
        &self,
        input: LimaLifecycleExecutionInput<'_>,
        observation_source: &O,
        command_executor: &E,
        clock: &C,
    ) -> Result<LimaLifecycleExecution, LimaLifecycleExecutionFailure>
    where
        O: LimaLifecycleObservationSource,
        E: CommandExecutor,
        C: LimaObservationClock,
    {
        let mut evidence = LimaLifecycleExecutionPrivateEvidence::default();
        let result = self.execute_inner(
            input,
            observation_source,
            command_executor,
            clock,
            &mut evidence,
        );
        match result {
            Ok((receipt, post_observation)) => Ok(LimaLifecycleExecution {
                receipt,
                post_observation,
                private_evidence: evidence,
            }),
            Err(problem) => Err(LimaLifecycleExecutionFailure::from_problem(
                problem, evidence,
            )),
        }
    }

    fn execute_inner<O, E, C>(
        &self,
        input: LimaLifecycleExecutionInput<'_>,
        observation_source: &O,
        command_executor: &E,
        clock: &C,
        evidence: &mut LimaLifecycleExecutionPrivateEvidence,
    ) -> Result<(LimaLifecycleExecutionReceipt, LimaInstanceObservationReport), ExecutionProblem>
    where
        O: LimaLifecycleObservationSource,
        E: CommandExecutor,
        C: LimaObservationClock,
    {
        let execution_unix_seconds = clock.unix_seconds().map_err(|_| {
            ExecutionProblem::new(
                LimaLifecycleExecutionRefusalCode::ClockFailure,
                LimaLifecycleExecutionPhase::InputValidation,
                "the lifecycle execution clock could not be read",
            )
        })?;
        validate_common(self, &input, execution_unix_seconds)?;
        let before_disk = input.current.configured.primary_disk_bytes;

        match input.accepted.action() {
            HostBrokerAction::Start {
                identity,
                profile,
                profile_generation,
            } => {
                validate_action_identity(identity, input.lifecycle)?;
                require_no_reservation(input.lifecycle)?;
                require_lifecycle_state(input.lifecycle, LimaLifecycleState::Stopped)?;
                require_profile_and_generation(input.lifecycle, *profile, *profile_generation)?;
                validate_report(input.current, LimaRuntimeState::Stopped, *profile, None)?;

                self.run_command(
                    command_executor,
                    evidence,
                    LimaLifecycleExecutionPhase::Start,
                    self.start_command(),
                )?;
                let post = observe_verified(
                    observation_source,
                    input.observation_request,
                    command_executor,
                    clock,
                )?;
                validate_static_identity(input.current, &post)?;
                validate_report(
                    &post,
                    LimaRuntimeState::Running,
                    *profile,
                    Some(input.expected_persistent_identity),
                )?;
                Ok((
                    receipt(
                        input.accepted,
                        LimaLifecycleExecutionAction::Start,
                        input.lifecycle,
                        LimaLifecycleState::Running,
                        *profile_generation,
                        *profile,
                        before_disk,
                        input.expected_persistent_identity.clone(),
                    ),
                    post,
                ))
            }
            HostBrokerAction::Stop {
                identity,
                current_profile,
                profile_generation,
                target_after_stop,
            } => {
                validate_action_identity(identity, input.lifecycle)?;
                require_no_reservation(input.lifecycle)?;
                require_lifecycle_state(input.lifecycle, LimaLifecycleState::Running)?;
                require_profile_and_generation(
                    input.lifecycle,
                    *current_profile,
                    *profile_generation,
                )?;
                validate_report(
                    input.current,
                    LimaRuntimeState::Running,
                    *current_profile,
                    Some(input.expected_persistent_identity),
                )?;

                self.run_command(
                    command_executor,
                    evidence,
                    LimaLifecycleExecutionPhase::Stop,
                    self.stop_command(),
                )?;
                let post = observe_verified(
                    observation_source,
                    input.observation_request,
                    command_executor,
                    clock,
                )?;
                validate_static_identity(input.current, &post)?;
                validate_report(&post, LimaRuntimeState::Stopped, *current_profile, None)?;
                Ok((
                    receipt(
                        input.accepted,
                        LimaLifecycleExecutionAction::Stop {
                            target_after_stop: *target_after_stop,
                        },
                        input.lifecycle,
                        LimaLifecycleState::Stopped,
                        *profile_generation,
                        *current_profile,
                        before_disk,
                        input.expected_persistent_identity.clone(),
                    ),
                    post,
                ))
            }
            HostBrokerAction::ChangeProfile {
                identity,
                from_profile,
                to_profile,
                current_generation,
                next_generation,
            } => {
                validate_action_identity(identity, input.lifecycle)?;
                require_no_reservation(input.lifecycle)?;
                require_lifecycle_state(input.lifecycle, LimaLifecycleState::Stopped)?;
                require_profile_and_generation(
                    input.lifecycle,
                    *from_profile,
                    *current_generation,
                )?;
                if from_profile == to_profile {
                    return Err(ExecutionProblem::new(
                        LimaLifecycleExecutionRefusalCode::ProfileMismatch,
                        LimaLifecycleExecutionPhase::InputValidation,
                        "profile-change action must select a different reviewed profile",
                    ));
                }
                if current_generation.get().checked_add(1) != Some(next_generation.get()) {
                    return Err(ExecutionProblem::new(
                        LimaLifecycleExecutionRefusalCode::GenerationMismatch,
                        LimaLifecycleExecutionPhase::InputValidation,
                        "profile-change generation must advance by exactly one",
                    ));
                }
                validate_report(
                    input.current,
                    LimaRuntimeState::Stopped,
                    *from_profile,
                    None,
                )?;

                self.run_command(
                    command_executor,
                    evidence,
                    LimaLifecycleExecutionPhase::Edit,
                    self.edit_command(*to_profile),
                )?;
                let edited = observe_verified(
                    observation_source,
                    input.observation_request,
                    command_executor,
                    clock,
                )?;
                validate_static_identity(input.current, &edited)?;
                validate_report(&edited, LimaRuntimeState::Stopped, *to_profile, None)?;

                self.run_command(
                    command_executor,
                    evidence,
                    LimaLifecycleExecutionPhase::Start,
                    self.start_command(),
                )?;
                let post = observe_verified(
                    observation_source,
                    input.observation_request,
                    command_executor,
                    clock,
                )?;
                validate_static_identity(&edited, &post)?;
                validate_report(
                    &post,
                    LimaRuntimeState::Running,
                    *to_profile,
                    Some(input.expected_persistent_identity),
                )?;
                Ok((
                    receipt(
                        input.accepted,
                        LimaLifecycleExecutionAction::ChangeProfile,
                        input.lifecycle,
                        LimaLifecycleState::Running,
                        *next_generation,
                        *to_profile,
                        before_disk,
                        input.expected_persistent_identity.clone(),
                    ),
                    post,
                ))
            }
            _ => Err(ExecutionProblem::new(
                LimaLifecycleExecutionRefusalCode::UnsupportedAction,
                LimaLifecycleExecutionPhase::InputValidation,
                "the accepted broker action is outside the Lima lifecycle executor boundary",
            )),
        }
    }

    fn base_command(&self) -> CommandSpec {
        CommandSpec::new(&self.limactl_program)
            .secret_environment("LIMA_HOME", exact_private_path(&self.lima_home))
            .environment("LANG", "C")
            .environment("LC_ALL", "C")
    }

    fn stop_command(&self) -> CommandSpec {
        self.base_command()
            .argument("stop")
            .argument(self.instance.as_str())
    }

    fn start_command(&self) -> CommandSpec {
        self.base_command()
            .argument("start")
            .argument(self.instance.as_str())
    }

    fn edit_command(&self, profile: LimaResourceProfile) -> CommandSpec {
        let (cpus, memory_gib) = match profile {
            LimaResourceProfile::Interactive => ("4", "3"),
            LimaResourceProfile::Work => ("8", "10"),
        };
        self.base_command()
            .argument("edit")
            .argument("--tty=false")
            .argument("--cpus")
            .argument(cpus)
            .argument("--memory")
            .argument(memory_gib)
            .argument(self.instance.as_str())
    }

    fn run_command<E>(
        &self,
        executor: &E,
        evidence: &mut LimaLifecycleExecutionPrivateEvidence,
        phase: LimaLifecycleExecutionPhase,
        command: CommandSpec,
    ) -> Result<(), ExecutionProblem>
    where
        E: CommandExecutor,
    {
        let record = executor.execute(&command).map_err(|_| {
            ExecutionProblem::new(
                LimaLifecycleExecutionRefusalCode::CommandFailed,
                phase,
                "the reviewed Lima lifecycle command could not be executed",
            )
        })?;
        evidence.commands.push(LimaLifecyclePrivateCommandEvidence {
            phase,
            record: record.clone(),
        });
        if record.argv != command.displayed_argv()
            || record.environment_keys != command.environment.keys().cloned().collect::<Vec<_>>()
        {
            return Err(ExecutionProblem::new(
                LimaLifecycleExecutionRefusalCode::CommandIdentityMismatch,
                phase,
                "the lifecycle subprocess record does not match the reviewed command identity",
            ));
        }
        if record.stdout.len() > MAX_LIMA_LIFECYCLE_EXECUTOR_OUTPUT_BYTES
            || record.stderr.len() > MAX_LIMA_LIFECYCLE_EXECUTOR_OUTPUT_BYTES
        {
            return Err(ExecutionProblem::new(
                LimaLifecycleExecutionRefusalCode::UnboundedOutput,
                phase,
                "the lifecycle subprocess output exceeded the reviewed bound",
            ));
        }
        if record.status != Some(0) || !record.success || !record.stderr.is_empty() {
            return Err(ExecutionProblem::new(
                LimaLifecycleExecutionRefusalCode::CommandFailed,
                phase,
                "the reviewed Lima lifecycle command did not complete cleanly",
            ));
        }
        Ok(())
    }
}

impl fmt::Debug for LimaLifecycleExecutor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LimaLifecycleExecutor")
            .field("limactl_program", &"<reviewed-absolute-limactl>")
            .field("lima_home", &"<private-lima-home>")
            .field("instance", &self.instance)
            .finish()
    }
}

fn input_failure(
    code: LimaLifecycleExecutionRefusalCode,
    message: &'static str,
) -> LimaLifecycleExecutionFailure {
    LimaLifecycleExecutionFailure::from_problem(
        ExecutionProblem::new(code, LimaLifecycleExecutionPhase::InputValidation, message),
        Default::default(),
    )
}

fn validate_private_absolute_path(path: PathBuf) -> Result<PathBuf, ExecutionProblem> {
    let text = path.to_str().ok_or_else(|| {
        ExecutionProblem::new(
            LimaLifecycleExecutionRefusalCode::InvalidInput,
            LimaLifecycleExecutionPhase::InputValidation,
            "the reviewed lifecycle path must be exact UTF-8",
        )
    })?;
    if text.len() > MAX_PRIVATE_PATH_BYTES
        || text.bytes().any(|byte| byte.is_ascii_control())
        || !path.is_absolute()
        || path == Path::new("/")
        || text.contains("//")
        || text.ends_with('/')
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(ExecutionProblem::new(
            LimaLifecycleExecutionRefusalCode::InvalidInput,
            LimaLifecycleExecutionPhase::InputValidation,
            "the reviewed lifecycle path must be one bounded unaliased absolute path",
        ));
    }
    Ok(path)
}

fn exact_private_path(path: &Path) -> String {
    path.to_str()
        .expect("validated private lifecycle path remains UTF-8")
        .to_owned()
}

fn validate_common(
    executor: &LimaLifecycleExecutor,
    input: &LimaLifecycleExecutionInput<'_>,
    execution_unix_seconds: u64,
) -> Result<(), ExecutionProblem> {
    if input.lifecycle.identity().instance_id().as_str() != executor.instance.as_str()
        || input.current.instance.as_str() != executor.instance.as_str()
        || input.observation_request.instance().as_str() != executor.instance.as_str()
    {
        return Err(ExecutionProblem::new(
            LimaLifecycleExecutionRefusalCode::IdentityMismatch,
            LimaLifecycleExecutionPhase::InputValidation,
            "broker, lifecycle, observation, and executor instance identities must match exactly",
        ));
    }
    let execution_millis = execution_unix_seconds.checked_mul(1_000).ok_or_else(|| {
        ExecutionProblem::new(
            LimaLifecycleExecutionRefusalCode::ClockFailure,
            LimaLifecycleExecutionPhase::InputValidation,
            "the lifecycle execution clock exceeded the supported range",
        )
    })?;
    let decision_millis = input.accepted.decision_at.get();
    if execution_millis < decision_millis
        || execution_millis - decision_millis > MAX_LIMA_LIFECYCLE_ACTION_AGE_MILLIS
    {
        return Err(ExecutionProblem::new(
            LimaLifecycleExecutionRefusalCode::ExpiredAction,
            LimaLifecycleExecutionPhase::InputValidation,
            "the accepted lifecycle action is outside its bounded execution window",
        ));
    }
    if input.current.timing.freshness_at(execution_unix_seconds) != LimaObservationFreshness::Fresh
    {
        return Err(ExecutionProblem::new(
            LimaLifecycleExecutionRefusalCode::StaleObservation,
            LimaLifecycleExecutionPhase::InputValidation,
            "the current Lima observation is not fresh at lifecycle mutation time",
        ));
    }
    Ok(())
}

fn validate_action_identity(
    identity: &LimaInstanceIdentity,
    lifecycle: &LimaLifecycleObservation,
) -> Result<(), ExecutionProblem> {
    if identity != lifecycle.identity() {
        return Err(ExecutionProblem::new(
            LimaLifecycleExecutionRefusalCode::IdentityMismatch,
            LimaLifecycleExecutionPhase::InputValidation,
            "broker action identity must match the exact lifecycle identity",
        ));
    }
    Ok(())
}

fn require_no_reservation(lifecycle: &LimaLifecycleObservation) -> Result<(), ExecutionProblem> {
    if lifecycle.active_reservation_id().is_some() {
        return Err(ExecutionProblem::new(
            LimaLifecycleExecutionRefusalCode::ActiveReservation,
            LimaLifecycleExecutionPhase::InputValidation,
            "Lima lifecycle mutation is refused while a reservation is active",
        ));
    }
    Ok(())
}

fn require_lifecycle_state(
    lifecycle: &LimaLifecycleObservation,
    expected: LimaLifecycleState,
) -> Result<(), ExecutionProblem> {
    if lifecycle.state() != expected {
        return Err(ExecutionProblem::new(
            LimaLifecycleExecutionRefusalCode::StateMismatch,
            LimaLifecycleExecutionPhase::InputValidation,
            "broker action does not match the exact current lifecycle state",
        ));
    }
    Ok(())
}

fn require_profile_and_generation(
    lifecycle: &LimaLifecycleObservation,
    profile: LimaResourceProfile,
    generation: LimaProfileGeneration,
) -> Result<(), ExecutionProblem> {
    if lifecycle.profile() != profile {
        return Err(ExecutionProblem::new(
            LimaLifecycleExecutionRefusalCode::ProfileMismatch,
            LimaLifecycleExecutionPhase::InputValidation,
            "broker action profile must match the exact lifecycle profile",
        ));
    }
    if lifecycle.profile_generation() != generation {
        return Err(ExecutionProblem::new(
            LimaLifecycleExecutionRefusalCode::GenerationMismatch,
            LimaLifecycleExecutionPhase::InputValidation,
            "broker action generation must match the exact lifecycle generation",
        ));
    }
    Ok(())
}

fn validate_report(
    report: &LimaInstanceObservationReport,
    expected_state: LimaRuntimeState,
    profile: LimaResourceProfile,
    expected_persistent_identity: Option<&LimaPersistentIdentity>,
) -> Result<(), ExecutionProblem> {
    if report.configured.runtime_state != expected_state {
        return Err(ExecutionProblem::new(
            LimaLifecycleExecutionRefusalCode::StateMismatch,
            LimaLifecycleExecutionPhase::Verify,
            "observed Lima runtime state does not match the accepted lifecycle action",
        ));
    }
    let envelope = profile.envelope();
    if report.configured.cpus != envelope.vcpus
        || report.configured.memory_bytes != envelope.memory_bytes
    {
        return Err(ExecutionProblem::new(
            LimaLifecycleExecutionRefusalCode::ResourceMismatch,
            LimaLifecycleExecutionPhase::Verify,
            "configured Lima resources do not match the reviewed profile envelope",
        ));
    }
    match (&report.guest, expected_state, expected_persistent_identity) {
        (LimaGuestObservation::NotRunning { runtime_state }, LimaRuntimeState::Stopped, None)
            if *runtime_state == LimaRuntimeState::Stopped =>
        {
            Ok(())
        }
        (LimaGuestObservation::Observed(guest), LimaRuntimeState::Running, Some(expected)) => {
            if guest.resources.cpus != envelope.vcpus
                || guest.resources.memory_bytes == 0
                || guest.resources.memory_bytes > envelope.memory_bytes
            {
                return Err(ExecutionProblem::new(
                    LimaLifecycleExecutionRefusalCode::ResourceMismatch,
                    LimaLifecycleExecutionPhase::Verify,
                    "guest resources do not match the reviewed profile envelope",
                ));
            }
            if &guest.persistent_identity != expected {
                return Err(ExecutionProblem::new(
                    LimaLifecycleExecutionRefusalCode::PersistentIdentityMismatch,
                    LimaLifecycleExecutionPhase::Verify,
                    "guest machine, root filesystem, or cache identity changed",
                ));
            }
            Ok(())
        }
        _ => Err(ExecutionProblem::new(
            LimaLifecycleExecutionRefusalCode::VerificationFailed,
            LimaLifecycleExecutionPhase::Verify,
            "Lima guest evidence does not match the accepted lifecycle action",
        )),
    }
}

fn validate_static_identity(
    before: &LimaInstanceObservationReport,
    after: &LimaInstanceObservationReport,
) -> Result<(), ExecutionProblem> {
    if before.instance != after.instance
        || before.configured.vm_type != after.configured.vm_type
        || before.configured.architecture != after.configured.architecture
        || before.configured.primary_disk_bytes != after.configured.primary_disk_bytes
    {
        return Err(ExecutionProblem::new(
            LimaLifecycleExecutionRefusalCode::IdentityMismatch,
            LimaLifecycleExecutionPhase::Verify,
            "Lima instance, VM, architecture, or primary-disk identity changed",
        ));
    }
    Ok(())
}

fn observe_verified<O, E, C>(
    source: &O,
    request: &LimaObservationRequest,
    executor: &E,
    clock: &C,
) -> Result<LimaInstanceObservationReport, ExecutionProblem>
where
    O: LimaLifecycleObservationSource,
    E: CommandExecutor,
    C: LimaObservationClock,
{
    let report = source.observe(request, executor, clock).map_err(|_| {
        ExecutionProblem::new(
            LimaLifecycleExecutionRefusalCode::VerificationFailed,
            LimaLifecycleExecutionPhase::Verify,
            "fresh exact Lima verification failed after lifecycle mutation",
        )
    })?;
    if report.timing.freshness != LimaObservationFreshness::Fresh {
        return Err(ExecutionProblem::new(
            LimaLifecycleExecutionRefusalCode::StaleObservation,
            LimaLifecycleExecutionPhase::Verify,
            "post-mutation Lima verification is not fresh",
        ));
    }
    Ok(report)
}

#[allow(clippy::too_many_arguments)]
fn receipt(
    accepted: &AcceptedLimaLifecycleAction,
    action: LimaLifecycleExecutionAction,
    lifecycle: &LimaLifecycleObservation,
    after_state: LimaLifecycleState,
    after_generation: LimaProfileGeneration,
    after_profile: LimaResourceProfile,
    primary_disk_bytes: u64,
    persistent_identity: LimaPersistentIdentity,
) -> LimaLifecycleExecutionReceipt {
    LimaLifecycleExecutionReceipt {
        schema_version: LIMA_LIFECYCLE_EXECUTOR_SCHEMA_VERSION,
        broker_state_revision: accepted.state_revision,
        queue_generation: accepted.queue_generation,
        decision_at: accepted.decision_at,
        action,
        identity: lifecycle.identity().clone(),
        before_state: lifecycle.state(),
        after_state,
        before_profile: lifecycle.profile(),
        after_profile,
        before_generation: lifecycle.profile_generation(),
        after_generation,
        before_resources: lifecycle.profile().envelope(),
        after_resources: after_profile.envelope(),
        primary_disk_bytes,
        persistent_identity,
    }
}

#[cfg(test)]
#[path = "lima_lifecycle_executor_inner/tests.rs"]
mod tests;
