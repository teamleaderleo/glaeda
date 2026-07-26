use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};

use crate::artifact::{ArtifactIdentity, ArtifactKind, CommitId, RepositoryRef, Sha256Digest};
use crate::lane_command::{LinuxAccountName, RunnerUserContext};
use crate::process::{CommandSpec, ExecutionRecord};
use crate::renderprove_execution::{
    RenderproveCommand, RenderproveExecutionContext, plan_renderprove_command,
};
use crate::renderprove_verification::{
    RenderproveEvidencePolicy, RenderproveReviewNetworkPolicy, RenderproveSourceIdentity,
    RenderproveVerificationRequest, RenderproveWorkerImageIdentity,
};

use super::{
    FilesystemIdentity, RenderproveRuntime, RenderproveSubprocessError,
    RenderproveSubprocessErrorKind, build_process, execute_with_runtime,
};

fn digest(character: char) -> Sha256Digest {
    Sha256Digest::parse(&format!("sha256:{}", character.to_string().repeat(64))).expect("digest")
}

fn command() -> RenderproveCommand {
    let repository = RepositoryRef::parse("example/project").expect("repository");
    let commit = CommitId::parse(&"1a".repeat(20)).expect("commit");
    let request = RenderproveVerificationRequest::new(
        RenderproveSourceIdentity::new(repository.clone(), commit.clone()),
        ArtifactIdentity::new(repository, commit, ArtifactKind::OciImage, digest('a')),
        RenderproveWorkerImageIdentity::new("registry.example/worker@reviewed", digest('b'))
            .expect("worker"),
        digest('c'),
        RenderproveEvidencePolicy::new(".smolrunner/renderprove", 1_024).expect("evidence"),
        RenderproveReviewNetworkPolicy::LoopbackOnly,
    )
    .expect("request");
    let runner = RunnerUserContext::new(
        LinuxAccountName::parse("project-runner").expect("runner"),
        1001,
        1001,
        "/var/lib/project-runner",
    )
    .expect("runner context");
    let context = RenderproveExecutionContext::new(
        "/srv/smolrunner/workspaces/job-1",
        "/opt/renderprove",
        runner,
    )
    .expect("context");
    plan_renderprove_command(request, &context).expect("command")
}

fn record(command: &RenderproveCommand, status: i32) -> ExecutionRecord {
    ExecutionRecord {
        argv: command.spec().displayed_argv(),
        environment_keys: command.spec().environment.keys().cloned().collect(),
        status: Some(status),
        success: status == 0,
        stdout: "private stdout".to_owned(),
        stderr: "private stderr".to_owned(),
    }
}

fn identity(path: impl Into<PathBuf>, inode: u64) -> FilesystemIdentity {
    FilesystemIdentity {
        physical_path: path.into(),
        device: 7,
        inode,
        owner_uid: 1001,
        mode: 0o755,
    }
}

struct FakeRuntime {
    directories: RefCell<VecDeque<Result<FilesystemIdentity, RenderproveSubprocessError>>>,
    executables: RefCell<
        BTreeMap<PathBuf, VecDeque<Result<FilesystemIdentity, RenderproveSubprocessError>>>,
    >,
    execution: RefCell<Option<Result<ExecutionRecord, RenderproveSubprocessError>>>,
    executed: RefCell<Vec<(CommandSpec, PathBuf)>>,
}

impl FakeRuntime {
    fn stable(
        command: &RenderproveCommand,
        execution: Result<ExecutionRecord, RenderproveSubprocessError>,
    ) -> Self {
        let cwd = identity(command.working_directory(), 11);
        let mut executables = BTreeMap::new();
        for (index, path) in command.required_programs().into_iter().enumerate() {
            let observed = identity(path, 20 + index as u64);
            executables.insert(
                path.to_path_buf(),
                VecDeque::from([Ok(observed.clone()), Ok(observed)]),
            );
        }
        Self {
            directories: RefCell::new(VecDeque::from([Ok(cwd.clone()), Ok(cwd)])),
            executables: RefCell::new(executables),
            execution: RefCell::new(Some(execution)),
            executed: RefCell::new(Vec::new()),
        }
    }

    fn identity_error(stage: &'static str, message: &'static str) -> RenderproveSubprocessError {
        RenderproveSubprocessError::identity(stage, message)
    }
}

impl RenderproveRuntime for FakeRuntime {
    fn observe_directory(
        &self,
        _path: &Path,
    ) -> Result<FilesystemIdentity, RenderproveSubprocessError> {
        self.directories
            .borrow_mut()
            .pop_front()
            .expect("directory observation")
    }

    fn observe_executable(
        &self,
        path: &Path,
    ) -> Result<FilesystemIdentity, RenderproveSubprocessError> {
        self.executables
            .borrow_mut()
            .get_mut(path)
            .expect("program observation queue")
            .pop_front()
            .expect("program observation")
    }

    fn execute(
        &self,
        spec: &CommandSpec,
        working_directory: &Path,
    ) -> Result<ExecutionRecord, RenderproveSubprocessError> {
        self.executed
            .borrow_mut()
            .push((spec.clone(), working_directory.to_path_buf()));
        self.execution
            .borrow_mut()
            .take()
            .expect("execution result")
    }
}

#[test]
fn exact_cwd_executes_only_the_reviewed_command() {
    let command = command();
    let runtime = FakeRuntime::stable(&command, Ok(record(&command, 0)));
    let observation = execute_with_runtime(&command, &runtime).expect("observation");

    assert_eq!(observation.working_directory(), command.working_directory());
    let executed = runtime.executed.borrow();
    assert_eq!(executed.len(), 1);
    assert_eq!(&executed[0].0, command.spec());
    assert_eq!(executed[0].1, command.working_directory());
    assert_ne!(executed[0].0.program, Path::new("/bin/sh"));
    assert_ne!(executed[0].0.program, Path::new("/bin/bash"));
}

#[test]
fn wrong_physical_cwd_fails_before_spawn() {
    let command = command();
    let runtime = FakeRuntime::stable(&command, Ok(record(&command, 0)));
    runtime.directories.borrow_mut()[0]
        .as_mut()
        .expect("cwd")
        .physical_path = PathBuf::from("/srv/smolrunner/workspaces/other");

    let error = execute_with_runtime(&command, &runtime).expect_err("cwd drift");
    assert_eq!(error.kind(), RenderproveSubprocessErrorKind::Identity);
    assert_eq!(error.stage(), "working_directory");
    assert!(runtime.executed.borrow().is_empty());
}

#[test]
fn missing_wrapper_fails_before_spawn() {
    let command = command();
    let runtime = FakeRuntime::stable(&command, Ok(record(&command, 0)));
    let wrapper = command.required_programs()[2].to_path_buf();
    runtime
        .executables
        .borrow_mut()
        .get_mut(&wrapper)
        .expect("wrapper")[0] = Err(FakeRuntime::identity_error(
        "required_program",
        "reviewed executable is missing",
    ));

    let error = execute_with_runtime(&command, &runtime).expect_err("missing wrapper");
    assert_eq!(error.kind(), RenderproveSubprocessErrorKind::Identity);
    assert!(runtime.executed.borrow().is_empty());
}

#[test]
fn missing_runuser_fails_before_spawn() {
    let command = command();
    let runtime = FakeRuntime::stable(&command, Ok(record(&command, 0)));
    let runuser = command.required_programs()[0].to_path_buf();
    runtime
        .executables
        .borrow_mut()
        .get_mut(&runuser)
        .expect("runuser")[0] = Err(FakeRuntime::identity_error(
        "required_program",
        "reviewed executable is missing",
    ));

    let error = execute_with_runtime(&command, &runtime).expect_err("missing runuser");
    assert_eq!(error.kind(), RenderproveSubprocessErrorKind::Identity);
    assert!(runtime.executed.borrow().is_empty());
}

#[test]
fn output_limit_and_spawn_failures_are_typed() {
    let command = command();
    let runtime = FakeRuntime::stable(
        &command,
        Err(RenderproveSubprocessError::output_limit("stdout")),
    );
    let error = execute_with_runtime(&command, &runtime).expect_err("output limit");
    assert_eq!(error.kind(), RenderproveSubprocessErrorKind::OutputLimit);

    let runtime = FakeRuntime::stable(
        &command,
        Err(RenderproveSubprocessError::spawn("spawn failed")),
    );
    let error = execute_with_runtime(&command, &runtime).expect_err("spawn failure");
    assert_eq!(error.kind(), RenderproveSubprocessErrorKind::Spawn);
}

#[test]
fn success_and_nonzero_status_return_typed_observations() {
    let command = command();
    for status in [0, 17] {
        let runtime = FakeRuntime::stable(&command, Ok(record(&command, status)));
        let observation = execute_with_runtime(&command, &runtime).expect("observation");
        assert_eq!(observation.record().status, Some(status));
        assert_eq!(observation.record().success, status == 0);
    }
}

#[test]
fn unrepresentable_status_is_typed() {
    let command = command();
    let mut invalid = record(&command, 0);
    invalid.status = None;
    invalid.success = false;
    let runtime = FakeRuntime::stable(&command, Ok(invalid));
    let error = execute_with_runtime(&command, &runtime).expect_err("status failure");
    assert_eq!(error.kind(), RenderproveSubprocessErrorKind::Status);
}

#[test]
fn executable_identity_change_after_spawn_fails_closed() {
    let command = command();
    let runtime = FakeRuntime::stable(&command, Ok(record(&command, 0)));
    let wrapper = command.required_programs()[2].to_path_buf();
    runtime
        .executables
        .borrow_mut()
        .get_mut(&wrapper)
        .expect("wrapper")[1]
        .as_mut()
        .expect("identity")
        .inode = 999;

    let error = execute_with_runtime(&command, &runtime).expect_err("identity change");
    assert_eq!(error.kind(), RenderproveSubprocessErrorKind::Identity);
}

#[test]
fn private_output_and_paths_are_redacted_from_debug() {
    let command = command();
    let runtime = FakeRuntime::stable(&command, Ok(record(&command, 0)));
    let observation = execute_with_runtime(&command, &runtime).expect("observation");
    let debug = format!("{observation:?}");
    for private in [
        "private stdout",
        "private stderr",
        "/srv/smolrunner/workspaces/job-1",
        "/opt/renderprove",
        ".smolrunner/renderprove",
    ] {
        assert!(!debug.contains(private));
    }
}

#[test]
fn built_process_uses_exact_cwd_scrubbed_environment_and_no_shell() {
    let spec = CommandSpec::new("/usr/bin/env").environment("ONLY", "reviewed");
    let process = build_process(&spec, Path::new("/"));
    assert_eq!(process.get_program(), std::ffi::OsStr::new("/usr/bin/env"));
    assert_eq!(process.get_current_dir(), Some(Path::new("/")));
    assert_eq!(process.get_args().count(), 0);
    assert_eq!(
        process.get_envs().collect::<Vec<_>>(),
        vec![(
            std::ffi::OsStr::new("ONLY"),
            Some(std::ffi::OsStr::new("reviewed"))
        )]
    );

    let env = Path::new("/usr/bin/env");
    if env.is_file() {
        let output = build_process(&CommandSpec::new(env), Path::new("/"))
            .output()
            .expect("execute env directly");
        assert!(output.status.success());
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }
}
