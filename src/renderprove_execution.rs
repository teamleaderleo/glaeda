use std::fmt;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::journal::ExecutionLane;
use crate::lane_command::RunnerUserContext;
use crate::process::{CommandSpec, ExecutionRecord};
use crate::renderprove_verification::{
    RenderproveProcessFailure, RenderproveProcessOutcome, RenderproveReviewNetworkPolicy,
    RenderproveVerificationRequest,
};

pub const RENDERPROVE_EXECUTION_SCHEMA_VERSION: u8 = 1;
pub const RENDERPROVE_COMMAND_ID: &str = "renderprove.verify.render";

const RUNUSER: &str = "/usr/sbin/runuser";
const ENV: &str = "/usr/bin/env";
const CLEAN_PATH: &str = "/usr/local/bin:/usr/bin:/bin";
const WRAPPER_RELATIVE_PATH: &str = "examples/renderprove/run-renderprove-review.sh";
const RENDER_SUITE: &str = "render";

#[derive(Clone, PartialEq, Eq)]
pub struct RenderproveExecutionContext {
    workspace_root: PathBuf,
    renderprove_checkout: PathBuf,
    runner: RunnerUserContext,
}

impl RenderproveExecutionContext {
    /// Bind the disposable project workspace, reviewed Renderprove checkout, and runner identity.
    ///
    /// The reviewed checkout and disposable project workspace must be disjoint so repository code
    /// cannot replace the Renderprove implementation selected by the operator.
    ///
    /// # Errors
    ///
    /// Returns an error unless both paths are absolute, normalized, non-root UTF-8 paths that do
    /// not contain one another.
    pub fn new(
        workspace_root: impl Into<PathBuf>,
        renderprove_checkout: impl Into<PathBuf>,
        runner: RunnerUserContext,
    ) -> Result<Self, RenderproveExecutionError> {
        let workspace_root = validated_absolute_path("workspace_root", workspace_root.into())?;
        let renderprove_checkout =
            validated_absolute_path("renderprove_checkout", renderprove_checkout.into())?;
        if workspace_root.starts_with(&renderprove_checkout)
            || renderprove_checkout.starts_with(&workspace_root)
        {
            return Err(RenderproveExecutionError::new(
                "context.paths",
                "workspace and reviewed Renderprove checkout must be disjoint",
            ));
        }
        Ok(Self {
            workspace_root,
            renderprove_checkout,
            runner,
        })
    }

    #[must_use]
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    #[must_use]
    pub fn renderprove_checkout(&self) -> &Path {
        &self.renderprove_checkout
    }

    #[must_use]
    pub const fn runner(&self) -> &RunnerUserContext {
        &self.runner
    }
}

impl fmt::Debug for RenderproveExecutionContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderproveExecutionContext")
            .field("workspace_root", &"<private absolute path>")
            .field("renderprove_checkout", &"<private absolute path>")
            .field("runner", &"<reviewed runner-user context>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct RenderproveCommand {
    id: String,
    lane: ExecutionLane,
    request: RenderproveVerificationRequest,
    spec: CommandSpec,
    working_directory: PathBuf,
    wrapper_path: PathBuf,
}

impl RenderproveCommand {
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub const fn lane(&self) -> ExecutionLane {
        self.lane
    }

    #[must_use]
    pub const fn request(&self) -> &RenderproveVerificationRequest {
        &self.request
    }

    #[must_use]
    pub const fn spec(&self) -> &CommandSpec {
        &self.spec
    }

    #[must_use]
    pub fn working_directory(&self) -> &Path {
        &self.working_directory
    }

    #[must_use]
    pub fn required_programs(&self) -> [&Path; 3] {
        [Path::new(RUNUSER), Path::new(ENV), &self.wrapper_path]
    }
}

impl fmt::Debug for RenderproveCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderproveCommand")
            .field("id", &self.id)
            .field("lane", &self.lane)
            .field("request", &"<retained exact private request>")
            .field("program", &RUNUSER)
            .field("working_directory", &"<private reviewed working directory>")
            .field("wrapper", &"<fixed reviewed wrapper>")
            .field("suite", &RENDER_SUITE)
            .field("private_arguments", &5)
            .finish()
    }
}

/// Build the sole reviewed Renderprove repository-verification command.
///
/// The command enters the reviewed runner-user lane through `runuser`, clears the inner environment
/// with `/usr/bin/env -i`, supplies only fixed identity values plus redacted private path values,
/// invokes the checked-in wrapper, and passes the single fixed `render` suite argument. Deployed-
/// origin review remains deferred until a separately reviewed network adapter exists.
///
/// This function only plans a [`CommandSpec`]; it performs no subprocess execution or filesystem
/// observation.
///
/// # Errors
///
/// Returns an error for deployed-origin review or non-UTF-8 private paths.
pub fn plan_renderprove_command(
    request: RenderproveVerificationRequest,
    context: &RenderproveExecutionContext,
) -> Result<RenderproveCommand, RenderproveExecutionError> {
    if !matches!(
        request.network(),
        RenderproveReviewNetworkPolicy::LoopbackOnly
    ) {
        return Err(RenderproveExecutionError::new(
            "request.network",
            "must be loopback_only until a deployed-origin adapter is reviewed",
        ));
    }

    let checkout = context.renderprove_checkout.to_str().ok_or_else(|| {
        RenderproveExecutionError::new(
            "renderprove_checkout",
            "must be valid UTF-8 for the explicit child environment",
        )
    })?;
    let evidence_directory = request.evidence().directory().to_str().ok_or_else(|| {
        RenderproveExecutionError::new(
            "request.evidence.directory",
            "must be valid UTF-8 for the explicit child environment",
        )
    })?;
    let wrapper_path = context.renderprove_checkout.join(WRAPPER_RELATIVE_PATH);
    let wrapper = wrapper_path
        .to_str()
        .expect("validated UTF-8 checkout plus fixed ASCII wrapper path");
    let username = context.runner.username().as_str();

    let spec = CommandSpec::new(RUNUSER)
        .argument("--user")
        .argument(username)
        .argument("--")
        .argument(ENV)
        .argument("-i")
        .secret_argument(format!("HOME={}", context.runner.home()))
        .argument(format!("USER={username}"))
        .argument(format!("LOGNAME={username}"))
        .secret_argument(format!(
            "XDG_RUNTIME_DIR={}",
            context.runner.runtime_directory()
        ))
        .argument(format!("PATH={CLEAN_PATH}"))
        .secret_argument(format!("RENDERPROVE_CHECKOUT={checkout}"))
        .secret_argument(format!("SMOLRUNNER_EVIDENCE_DIR={evidence_directory}"))
        .secret_argument(wrapper)
        .argument(RENDER_SUITE);

    Ok(RenderproveCommand {
        id: RENDERPROVE_COMMAND_ID.to_owned(),
        lane: ExecutionLane::RunnerUser,
        request,
        spec,
        working_directory: context.workspace_root.clone(),
        wrapper_path,
    })
}

#[derive(Clone, PartialEq, Eq)]
struct RenderprovePrivateDiagnostics {
    stdout: String,
    stderr: String,
}

impl RenderprovePrivateDiagnostics {
    fn has_output(&self) -> bool {
        !self.stdout.is_empty() || !self.stderr.is_empty()
    }
}

impl fmt::Debug for RenderprovePrivateDiagnostics {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderprovePrivateDiagnostics")
            .field("stdout", &"<private diagnostic>")
            .field("stderr", &"<private diagnostic>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct RenderproveExecutionObservation {
    record: ExecutionRecord,
    working_directory: PathBuf,
    command_spec: CommandSpec,
}

impl RenderproveExecutionObservation {
    /// Pair one process record with the exact physical working directory and exact command values
    /// used for that process.
    ///
    /// This type performs no process or filesystem observation. The future subprocess adapter must
    /// supply the same private [`CommandSpec`] that it passed to the executor, together with the
    /// physical working directory selected for that execution. The command binding remains private:
    /// this type is not serializable and its `Debug` implementation does not expose the spec.
    ///
    /// # Errors
    ///
    /// Returns an error unless the observed directory is an absolute, normalized, non-root UTF-8
    /// path.
    pub fn new(
        record: ExecutionRecord,
        working_directory: impl Into<PathBuf>,
        command_spec: CommandSpec,
    ) -> Result<Self, RenderproveExecutionError> {
        Ok(Self {
            record,
            working_directory: validated_absolute_path(
                "execution.working_directory",
                working_directory.into(),
            )?,
            command_spec,
        })
    }

    #[must_use]
    pub const fn record(&self) -> &ExecutionRecord {
        &self.record
    }

    #[must_use]
    pub fn working_directory(&self) -> &Path {
        &self.working_directory
    }
}

impl fmt::Debug for RenderproveExecutionObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderproveExecutionObservation")
            .field("record", &"<private process observation>")
            .field("working_directory", &"<private observed working directory>")
            .field("command_spec", &"<private exact command binding>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct RenderproveExecutionReceipt {
    schema_version: u8,
    command_id: String,
    process: RenderproveProcessOutcome,
    #[serde(skip)]
    command: RenderproveCommand,
    #[serde(skip)]
    diagnostics: RenderprovePrivateDiagnostics,
}

impl RenderproveExecutionReceipt {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub fn command_id(&self) -> &str {
        &self.command_id
    }

    #[must_use]
    pub const fn process(&self) -> &RenderproveProcessOutcome {
        &self.process
    }

    #[must_use]
    pub const fn command(&self) -> &RenderproveCommand {
        &self.command
    }

    #[must_use]
    pub fn has_private_diagnostics(&self) -> bool {
        self.diagnostics.has_output()
    }
}

impl fmt::Debug for RenderproveExecutionReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderproveExecutionReceipt")
            .field("schema_version", &self.schema_version)
            .field("command_id", &self.command_id)
            .field("process", &self.process)
            .field("command", &"<retained reviewed command>")
            .field("diagnostics", &self.diagnostics)
            .finish()
    }
}

/// Bind one process record to the exact reviewed Renderprove command.
///
/// The observation must retain the full private command specification used for execution. Binding
/// compares that exact specification before relying on the redacted argv and environment-key views.
/// Raw stdout and stderr remain private and never enter serialized or `Debug` receipt output. A
/// failed process requires one explicit bounded failure classification; successful executions must
/// omit it. Missing, contradictory, or out-of-range exit evidence fails closed.
///
/// # Errors
///
/// Returns an error unless the observed working directory, private command specification, redacted
/// argv, and environment-key evidence match the command exactly and the process status is internally
/// consistent.
pub fn bind_renderprove_execution(
    command: RenderproveCommand,
    observation: RenderproveExecutionObservation,
    failure: Option<RenderproveProcessFailure>,
) -> Result<RenderproveExecutionReceipt, RenderproveExecutionError> {
    let RenderproveExecutionObservation {
        record: execution,
        working_directory,
        command_spec,
    } = observation;
    if working_directory != command.working_directory {
        return Err(RenderproveExecutionError::new(
            "execution.working_directory",
            "does not match the reviewed Renderprove command",
        ));
    }
    if command_spec != command.spec {
        return Err(RenderproveExecutionError::new(
            "execution.command",
            "private command values do not match the reviewed Renderprove command",
        ));
    }
    if execution.argv != command.spec.displayed_argv() {
        return Err(RenderproveExecutionError::new(
            "execution.argv",
            "does not match the reviewed Renderprove command",
        ));
    }
    let expected_environment_keys = command.spec.environment.keys().cloned().collect::<Vec<_>>();
    if execution.environment_keys != expected_environment_keys {
        return Err(RenderproveExecutionError::new(
            "execution.environment_keys",
            "do not match the reviewed Renderprove command",
        ));
    }

    let process = match (execution.status, execution.success, failure) {
        (Some(0), true, None) => RenderproveProcessOutcome::Succeeded,
        (Some(0), true, Some(_)) => {
            return Err(RenderproveExecutionError::new(
                "failure",
                "must be absent for a successful process",
            ));
        }
        (Some(code @ 1..=255), false, Some(reason)) => RenderproveProcessOutcome::Failed {
            exit_code: u8::try_from(code).expect("matched status range is bounded"),
            reason,
        },
        (Some(1..=255), false, None) => {
            return Err(RenderproveExecutionError::new(
                "failure",
                "must classify a failed process",
            ));
        }
        (None, false, _) => {
            return Err(RenderproveExecutionError::new(
                "execution.status",
                "missing status requires a separate cancellation receipt",
            ));
        }
        _ => {
            return Err(RenderproveExecutionError::new(
                "execution",
                "contains contradictory success or out-of-range status evidence",
            ));
        }
    };

    Ok(RenderproveExecutionReceipt {
        schema_version: RENDERPROVE_EXECUTION_SCHEMA_VERSION,
        command_id: command.id.clone(),
        process,
        command,
        diagnostics: RenderprovePrivateDiagnostics {
            stdout: execution.stdout,
            stderr: execution.stderr,
        },
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderproveExecutionError {
    pub field: String,
    pub problem: String,
}

impl RenderproveExecutionError {
    fn new(field: impl Into<String>, problem: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            problem: problem.into(),
        }
    }
}

impl fmt::Display for RenderproveExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", self.field, self.problem)
    }
}

impl std::error::Error for RenderproveExecutionError {}

fn validated_absolute_path(
    field: &str,
    path: PathBuf,
) -> Result<PathBuf, RenderproveExecutionError> {
    let mut normal_components = 0_usize;
    let valid = path.is_absolute()
        && path.to_str().is_some()
        && path.components().all(|component| match component {
            Component::Prefix(_) | Component::RootDir => true,
            Component::Normal(_) => {
                normal_components += 1;
                true
            }
            Component::CurDir | Component::ParentDir => false,
        });
    if !valid || normal_components == 0 {
        return Err(RenderproveExecutionError::new(
            field,
            "must be a normalized non-root absolute UTF-8 path",
        ));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::artifact::{ArtifactIdentity, ArtifactKind, CommitId, RepositoryRef, Sha256Digest};
    use crate::journal::ExecutionLane;
    use crate::lane_command::{LinuxAccountName, RunnerUserContext};
    use crate::process::{CommandSpec, ExecutionRecord};
    use crate::renderprove_verification::{
        RenderproveEvidencePolicy, RenderproveProcessFailure, RenderproveProcessOutcome,
        RenderproveReviewNetworkPolicy, RenderproveSourceIdentity, RenderproveVerificationRequest,
        RenderproveWorkerImageIdentity,
    };

    use super::{
        RENDERPROVE_COMMAND_ID, RenderproveCommand, RenderproveExecutionContext,
        RenderproveExecutionObservation, bind_renderprove_execution, plan_renderprove_command,
    };

    fn digest(character: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", character.to_string().repeat(64)))
            .expect("digest")
    }

    fn request_with_evidence(
        network: RenderproveReviewNetworkPolicy,
        evidence_directory: &str,
    ) -> RenderproveVerificationRequest {
        let repository = RepositoryRef::parse("example/project").expect("repository");
        let commit = CommitId::parse(&"1a".repeat(20)).expect("commit");
        RenderproveVerificationRequest::new(
            RenderproveSourceIdentity::new(repository.clone(), commit.clone()),
            ArtifactIdentity::new(repository, commit, ArtifactKind::OciImage, digest('a')),
            RenderproveWorkerImageIdentity::new("registry.example/worker@reviewed", digest('b'))
                .expect("worker"),
            digest('c'),
            RenderproveEvidencePolicy::new(evidence_directory, 1_024).expect("evidence"),
            network,
        )
        .expect("request")
    }

    fn request(network: RenderproveReviewNetworkPolicy) -> RenderproveVerificationRequest {
        request_with_evidence(network, ".smolrunner/renderprove")
    }

    fn runner() -> RunnerUserContext {
        RunnerUserContext::new(
            LinuxAccountName::parse("project-runner").expect("username"),
            1001,
            1001,
            "/var/lib/project-runner",
        )
        .expect("runner")
    }

    fn context() -> RenderproveExecutionContext {
        RenderproveExecutionContext::new(
            "/srv/smolrunner/workspaces/job-1",
            "/opt/renderprove",
            runner(),
        )
        .expect("context")
    }

    fn execution_record(spec: &CommandSpec) -> ExecutionRecord {
        ExecutionRecord {
            argv: spec.displayed_argv(),
            environment_keys: spec.environment.keys().cloned().collect(),
            status: Some(0),
            success: true,
            stdout: "private Renderprove log".to_owned(),
            stderr: "private browser warning".to_owned(),
        }
    }

    fn execution_with_spec(
        command: &RenderproveCommand,
        command_spec: CommandSpec,
    ) -> RenderproveExecutionObservation {
        RenderproveExecutionObservation::new(
            execution_record(&command_spec),
            command.working_directory().to_path_buf(),
            command_spec,
        )
        .expect("execution observation")
    }

    fn execution(command: &RenderproveCommand) -> RenderproveExecutionObservation {
        execution_with_spec(command, command.spec().clone())
    }

    fn assert_private_command_rejected(command: &RenderproveCommand, altered_spec: CommandSpec) {
        assert_eq!(
            altered_spec.displayed_argv(),
            command.spec().displayed_argv(),
            "the regression must preserve the same public argv shape"
        );
        assert_eq!(
            altered_spec.environment.keys().collect::<Vec<_>>(),
            command.spec().environment.keys().collect::<Vec<_>>(),
            "the regression must preserve the same public environment-key shape"
        );
        let observation = execution_with_spec(command, altered_spec);
        let error = bind_renderprove_execution(command.clone(), observation, None)
            .expect_err("private command drift must fail");
        assert_eq!(error.field, "execution.command");
    }

    #[test]
    fn planner_emits_one_fixed_runner_user_command_with_redacted_private_paths() {
        let command = plan_renderprove_command(
            request(RenderproveReviewNetworkPolicy::LoopbackOnly),
            &context(),
        )
        .expect("command");

        assert_eq!(command.id(), RENDERPROVE_COMMAND_ID);
        assert_eq!(command.lane(), ExecutionLane::RunnerUser);
        let expected_spec = CommandSpec::new("/usr/sbin/runuser")
            .argument("--user")
            .argument("project-runner")
            .argument("--")
            .argument("/usr/bin/env")
            .argument("-i")
            .secret_argument("HOME=/var/lib/project-runner")
            .argument("USER=project-runner")
            .argument("LOGNAME=project-runner")
            .secret_argument("XDG_RUNTIME_DIR=/run/user/1001")
            .argument("PATH=/usr/local/bin:/usr/bin:/bin")
            .secret_argument("RENDERPROVE_CHECKOUT=/opt/renderprove")
            .secret_argument("SMOLRUNNER_EVIDENCE_DIR=.smolrunner/renderprove")
            .secret_argument("/opt/renderprove/examples/renderprove/run-renderprove-review.sh")
            .argument("render");
        assert_eq!(command.spec(), &expected_spec);
        assert_eq!(
            command.working_directory(),
            Path::new("/srv/smolrunner/workspaces/job-1")
        );
        assert_eq!(
            command.spec().displayed_argv(),
            vec![
                "/usr/sbin/runuser",
                "--user",
                "project-runner",
                "--",
                "/usr/bin/env",
                "-i",
                "[REDACTED]",
                "USER=project-runner",
                "LOGNAME=project-runner",
                "[REDACTED]",
                "PATH=/usr/local/bin:/usr/bin:/bin",
                "[REDACTED]",
                "[REDACTED]",
                "[REDACTED]",
                "render",
            ]
        );
        assert!(command.spec().environment.is_empty());
        assert_eq!(
            command.required_programs()[2],
            Path::new("/opt/renderprove/examples/renderprove/run-renderprove-review.sh")
        );
        assert!(!command.required_programs()[2].starts_with(command.working_directory()));

        let json = serde_json::to_string(command.spec()).expect("command JSON");
        let debug = format!("{command:?}");
        for private_value in [
            "examples/renderprove/run-renderprove-review.sh",
            "/opt/renderprove",
            ".smolrunner/renderprove",
            "/srv/smolrunner/workspaces/job-1",
            "/opt/renderprove/examples/renderprove/run-renderprove-review.sh",
            "/var/lib/project-runner",
            "/run/user/1001",
        ] {
            assert!(!json.contains(private_value));
            assert!(!debug.contains(private_value));
        }
    }

    #[test]
    fn deployed_origin_and_unsafe_or_overlapping_context_paths_fail_closed() {
        let origin = crate::renderprove_verification::RenderproveDeployedOrigin::parse(
            "https://review.example.com",
        )
        .expect("origin");
        assert!(
            plan_renderprove_command(
                request(RenderproveReviewNetworkPolicy::DeployedOrigin { origin }),
                &context(),
            )
            .is_err()
        );
        assert!(RenderproveExecutionContext::new("/", "/opt/renderprove", runner()).is_err());
        assert!(
            RenderproveExecutionContext::new(
                "/srv/smolrunner/../escape",
                "/opt/renderprove",
                runner(),
            )
            .is_err()
        );
        assert!(
            RenderproveExecutionContext::new(
                "/srv/smolrunner/workspaces/job-1",
                "/srv/smolrunner/workspaces/job-1/tools/renderprove",
                runner(),
            )
            .is_err()
        );
        assert!(
            RenderproveExecutionContext::new(
                "/opt/renderprove/workspaces/job-1",
                "/opt/renderprove",
                runner(),
            )
            .is_err()
        );
    }

    #[test]
    fn ambient_and_wrong_working_directories_are_rejected_privately() {
        let command = plan_renderprove_command(
            request(RenderproveReviewNetworkPolicy::LoopbackOnly),
            &context(),
        )
        .expect("command");

        let ambient = std::env::current_dir().expect("ambient working directory");
        assert_ne!(ambient, command.working_directory());
        let observation = RenderproveExecutionObservation::new(
            execution_record(command.spec()),
            ambient,
            command.spec().clone(),
        )
        .expect("ambient observation");
        let debug = format!("{observation:?}");
        assert!(!debug.contains("/srv/smolrunner/workspaces/job-1"));
        assert!(!debug.contains("private Renderprove log"));
        assert!(!debug.contains("/opt/renderprove"));
        let error = bind_renderprove_execution(command.clone(), observation, None)
            .expect_err("ambient cwd must fail");
        assert_eq!(error.field, "execution.working_directory");

        let observation = RenderproveExecutionObservation::new(
            execution_record(command.spec()),
            "/srv/smolrunner/workspaces/job-2",
            command.spec().clone(),
        )
        .expect("wrong observation");
        let error = bind_renderprove_execution(command, observation, None)
            .expect_err("wrong cwd must fail");
        assert_eq!(error.field, "execution.working_directory");
    }

    #[test]
    fn private_checkout_and_evidence_values_must_match_exactly() {
        let command = plan_renderprove_command(
            request(RenderproveReviewNetworkPolicy::LoopbackOnly),
            &context(),
        )
        .expect("command");

        let altered_checkout = plan_renderprove_command(
            request(RenderproveReviewNetworkPolicy::LoopbackOnly),
            &RenderproveExecutionContext::new(
                "/srv/smolrunner/workspaces/job-1",
                "/opt/renderprove-other",
                runner(),
            )
            .expect("altered checkout context"),
        )
        .expect("altered checkout command");
        assert_private_command_rejected(&command, altered_checkout.spec().clone());

        let altered_evidence = plan_renderprove_command(
            request_with_evidence(
                RenderproveReviewNetworkPolicy::LoopbackOnly,
                ".smolrunner/renderprove-other",
            ),
            &context(),
        )
        .expect("altered evidence command");
        assert_private_command_rejected(&command, altered_evidence.spec().clone());

        let altered_workspace = plan_renderprove_command(
            request(RenderproveReviewNetworkPolicy::LoopbackOnly),
            &RenderproveExecutionContext::new(
                "/srv/smolrunner/workspaces/job-2",
                "/opt/renderprove",
                runner(),
            )
            .expect("altered workspace context"),
        )
        .expect("altered workspace command");
        assert_eq!(command.spec(), altered_workspace.spec());
    }

    #[test]
    fn exact_success_binds_while_private_values_stay_out_of_output() {
        let command = plan_renderprove_command(
            request(RenderproveReviewNetworkPolicy::LoopbackOnly),
            &context(),
        )
        .expect("command");
        let observation = execution(&command);
        let observation_debug = format!("{observation:?}");
        let receipt = bind_renderprove_execution(command, observation, None).expect("receipt");

        assert_eq!(receipt.command_id(), RENDERPROVE_COMMAND_ID);
        assert_eq!(receipt.process(), &RenderproveProcessOutcome::Succeeded);
        assert!(receipt.has_private_diagnostics());
        assert_eq!(receipt.diagnostics.stdout, "private Renderprove log");
        assert_eq!(receipt.diagnostics.stderr, "private browser warning");

        let json = serde_json::to_string(&receipt).expect("receipt JSON");
        assert_eq!(
            json,
            r#"{"schema_version":1,"command_id":"renderprove.verify.render","process":{"outcome":"succeeded"}}"#
        );
        let debug = format!("{receipt:?}");
        for private_value in [
            "examples/renderprove/run-renderprove-review.sh",
            "/opt/renderprove",
            ".smolrunner/renderprove",
            "/srv/smolrunner/workspaces/job-1",
            "/opt/renderprove/examples/renderprove/run-renderprove-review.sh",
            "/var/lib/project-runner",
            "/run/user/1001",
            "private Renderprove log",
            "private browser warning",
        ] {
            assert!(!json.contains(private_value));
            assert!(!debug.contains(private_value));
            assert!(!observation_debug.contains(private_value));
        }
    }

    #[test]
    fn exact_failure_requires_one_bounded_classification() {
        let command = plan_renderprove_command(
            request(RenderproveReviewNetworkPolicy::LoopbackOnly),
            &context(),
        )
        .expect("command");
        let mut observation = execution(&command);
        observation.record.status = Some(17);
        observation.record.success = false;
        let receipt = bind_renderprove_execution(
            command,
            observation,
            Some(RenderproveProcessFailure::Browser),
        )
        .expect("receipt");
        assert_eq!(
            receipt.process(),
            &RenderproveProcessOutcome::Failed {
                exit_code: 17,
                reason: RenderproveProcessFailure::Browser,
            }
        );
    }

    #[test]
    fn altered_public_evidence_and_inconsistent_status_are_rejected() {
        let command = plan_renderprove_command(
            request(RenderproveReviewNetworkPolicy::LoopbackOnly),
            &context(),
        )
        .expect("command");
        let mut observation = execution(&command);
        observation.record.argv.push("unexpected".to_owned());
        assert!(bind_renderprove_execution(command.clone(), observation, None).is_err());

        let mut observation = execution(&command);
        observation
            .record
            .environment_keys
            .push("UNEXPECTED".to_owned());
        assert!(bind_renderprove_execution(command.clone(), observation, None).is_err());

        let mut observation = execution(&command);
        observation.record.status = Some(1);
        observation.record.success = false;
        assert!(bind_renderprove_execution(command.clone(), observation, None).is_err());

        let mut observation = execution(&command);
        observation.record.status = None;
        observation.record.success = false;
        assert!(bind_renderprove_execution(command, observation, None).is_err());
    }
}
