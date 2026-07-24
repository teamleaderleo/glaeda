use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path};

use serde::Serialize;

use crate::journal::ExecutionLane;
use crate::lane_command::{LaneCommand, LaneCommandKind, LinuxAccountName, PackageName};
use crate::lane_executable::verify_lane_command;
use crate::process::{
    CommandExecutor, CommandSpec, CommandValue, ExecutionRecord, ProcessExecutor,
};
use crate::runner_user::{MIN_SUBORDINATE_ID_COUNT, VerifiedRunnerUser};

const PROC_SELF_STATUS: &str = "/proc/self/status";
const MAX_PROC_STATUS_BYTES: usize = 64 * 1024;
const APT_GET: &str = "/usr/bin/apt-get";
const GROUPADD: &str = "/usr/sbin/groupadd";
const USERADD: &str = "/usr/sbin/useradd";
const LOGINCTL: &str = "/usr/bin/loginctl";
const RUNUSER: &str = "/usr/sbin/runuser";
const ENV: &str = "/usr/bin/env";
const PODMAN: &str = "/usr/bin/podman";
const GIT: &str = "/usr/bin/git";
const NOLOGIN: &str = "/usr/sbin/nologin";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneExecutionErrorKind {
    LaneMismatch,
    InvalidCommand,
    InvalidRunnerEvidence,
    UnsupportedPrivilege,
    ExecutableVerification,
    Spawn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LaneExecutionError {
    kind: LaneExecutionErrorKind,
    public_message: String,
}

impl LaneExecutionError {
    #[must_use]
    pub fn kind(&self) -> LaneExecutionErrorKind {
        self.kind
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.public_message
    }

    #[must_use]
    pub fn public(kind: LaneExecutionErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            public_message: message.into(),
        }
    }
}

impl fmt::Display for LaneExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.public_message)
    }
}

impl std::error::Error for LaneExecutionError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LaneExecutionReceipt {
    action_id: String,
    lane: ExecutionLane,
    kind: LaneCommandKind,
    status: Option<i32>,
    success: bool,
}

impl LaneExecutionReceipt {
    #[must_use]
    pub fn action_id(&self) -> &str {
        &self.action_id
    }

    #[must_use]
    pub fn lane(&self) -> ExecutionLane {
        self.lane
    }

    #[must_use]
    pub fn kind(&self) -> LaneCommandKind {
        self.kind
    }

    #[must_use]
    pub fn status(&self) -> Option<i32> {
        self.status
    }

    #[must_use]
    pub fn success(&self) -> bool {
        self.success
    }

    fn from_record(command: &LaneCommand, record: ExecutionRecord) -> Self {
        Self {
            action_id: command.action_id().to_owned(),
            lane: command.lane(),
            kind: command.kind(),
            status: record.status,
            success: record.success,
        }
    }
}

pub trait PrivilegeProbe {
    /// Return the process's effective Linux UID.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when privilege evidence cannot be obtained or parsed safely.
    fn effective_uid(&self) -> io::Result<u32>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ProcStatusPrivilegeProbe;

impl PrivilegeProbe for ProcStatusPrivilegeProbe {
    fn effective_uid(&self) -> io::Result<u32> {
        let bytes = fs::read(PROC_SELF_STATUS)?;
        if bytes.len() > MAX_PROC_STATUS_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "process status exceeds the configured size limit",
            ));
        }
        let input = std::str::from_utf8(&bytes).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "process status is not UTF-8")
        })?;
        parse_effective_uid(input)
    }
}

pub trait ReviewedExecutableVerifier {
    /// Verify all reviewed executables required by one typed command.
    ///
    /// # Errors
    ///
    /// Returns a bounded execution error when any executable lacks trusted evidence.
    fn verify(&self, command: &LaneCommand) -> Result<(), LaneExecutionError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemExecutableVerifier;

impl ReviewedExecutableVerifier for SystemExecutableVerifier {
    fn verify(&self, command: &LaneCommand) -> Result<(), LaneExecutionError> {
        verify_lane_command(command).map(|_| ()).map_err(|error| {
            LaneExecutionError::public(
                LaneExecutionErrorKind::ExecutableVerification,
                error.message().to_owned(),
            )
        })
    }
}

#[derive(Debug)]
pub struct RootLaneExecutor<E, V, P> {
    process: E,
    executables: V,
    privilege: P,
}

impl<E, V, P> RootLaneExecutor<E, V, P> {
    #[must_use]
    pub const fn new(process: E, executables: V, privilege: P) -> Self {
        Self {
            process,
            executables,
            privilege,
        }
    }
}

impl RootLaneExecutor<ProcessExecutor, SystemExecutableVerifier, ProcStatusPrivilegeProbe> {
    #[must_use]
    pub const fn system() -> Self {
        Self::new(
            ProcessExecutor,
            SystemExecutableVerifier,
            ProcStatusPrivilegeProbe,
        )
    }
}

impl<E, V, P> RootLaneExecutor<E, V, P>
where
    E: CommandExecutor,
    V: ReviewedExecutableVerifier,
    P: PrivilegeProbe,
{
    /// Execute one reviewed root-lane command after all evidence checks succeed.
    ///
    /// The returned receipt intentionally excludes child stdout and stderr.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for lane mismatch, malformed command shape, unsupported privilege,
    /// untrusted executables, or process startup failure.
    pub fn execute(
        &self,
        command: &LaneCommand,
    ) -> Result<LaneExecutionReceipt, LaneExecutionError> {
        require_lane(command, ExecutionLane::Root)?;
        validate_root_command(command)?;
        require_effective_root(&self.privilege)?;
        self.executables.verify(command)?;
        execute_bounded(&self.process, command)
    }
}

#[derive(Debug)]
pub struct RunnerUserLaneExecutor<E, V, P> {
    process: E,
    executables: V,
    privilege: P,
}

impl<E, V, P> RunnerUserLaneExecutor<E, V, P> {
    #[must_use]
    pub const fn new(process: E, executables: V, privilege: P) -> Self {
        Self {
            process,
            executables,
            privilege,
        }
    }
}

impl RunnerUserLaneExecutor<ProcessExecutor, SystemExecutableVerifier, ProcStatusPrivilegeProbe> {
    #[must_use]
    pub const fn system() -> Self {
        Self::new(
            ProcessExecutor,
            SystemExecutableVerifier,
            ProcStatusPrivilegeProbe,
        )
    }
}

impl<E, V, P> RunnerUserLaneExecutor<E, V, P>
where
    E: CommandExecutor,
    V: ReviewedExecutableVerifier,
    P: PrivilegeProbe,
{
    /// Execute one reviewed runner-user command through the sealed runuser and environment boundary.
    ///
    /// The caller must supply previously verified account, runtime-directory, and subordinate-ID
    /// evidence. The returned receipt intentionally excludes child stdout and stderr.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for lane mismatch, mismatched runner evidence, malformed command
    /// shape, unsupported privilege, untrusted executables, or process startup failure.
    pub fn execute(
        &self,
        command: &LaneCommand,
        runner: &VerifiedRunnerUser,
    ) -> Result<LaneExecutionReceipt, LaneExecutionError> {
        require_lane(command, ExecutionLane::RunnerUser)?;
        validate_runner_evidence(runner)?;
        validate_runner_command(command, runner)?;
        require_effective_root(&self.privilege)?;
        self.executables.verify(command)?;
        execute_bounded(&self.process, command)
    }
}

fn execute_bounded(
    process: &impl CommandExecutor,
    command: &LaneCommand,
) -> Result<LaneExecutionReceipt, LaneExecutionError> {
    process
        .execute(command.spec())
        .map(|record| LaneExecutionReceipt::from_record(command, record))
        .map_err(|_| {
            LaneExecutionError::public(
                LaneExecutionErrorKind::Spawn,
                "reviewed lane process could not be started",
            )
        })
}

fn require_lane(command: &LaneCommand, expected: ExecutionLane) -> Result<(), LaneExecutionError> {
    if command.lane() == expected {
        Ok(())
    } else {
        Err(LaneExecutionError::public(
            LaneExecutionErrorKind::LaneMismatch,
            format!(
                "action {} is assigned to {:?}, but this executor accepts only {:?}",
                command.action_id(),
                command.lane(),
                expected
            ),
        ))
    }
}

fn require_effective_root(probe: &impl PrivilegeProbe) -> Result<(), LaneExecutionError> {
    let effective_uid = probe.effective_uid().map_err(|_| {
        LaneExecutionError::public(
            LaneExecutionErrorKind::UnsupportedPrivilege,
            "could not verify effective Linux privilege; no mutation was attempted",
        )
    })?;
    if effective_uid == 0 {
        Ok(())
    } else {
        Err(LaneExecutionError::public(
            LaneExecutionErrorKind::UnsupportedPrivilege,
            "mutating lane execution requires effective UID 0; rerun through explicit elevation such as sudo smolrunner apply",
        ))
    }
}

fn validate_root_command(command: &LaneCommand) -> Result<(), LaneExecutionError> {
    let spec = command.spec();
    if !spec.environment.is_empty() {
        return Err(invalid_command(
            "root-lane commands must have an empty child environment",
        ));
    }
    let arguments = plain_arguments(spec)?;
    match command.kind() {
        LaneCommandKind::AptInstall => validate_apt(spec, &arguments),
        LaneCommandKind::EnsureSystemGroup => validate_groupadd(spec, &arguments),
        LaneCommandKind::EnsureSystemUser => validate_useradd(spec, &arguments),
        LaneCommandKind::EnableLinger => validate_loginctl(spec, &arguments),
        LaneCommandKind::RunnerPodmanInfo | LaneCommandKind::RunnerGitVersion => Err(
            invalid_command("runner-user command kind cannot execute in the root lane"),
        ),
    }
}

fn validate_apt(spec: &CommandSpec, arguments: &[&str]) -> Result<(), LaneExecutionError> {
    if spec.program != Path::new(APT_GET)
        || arguments.len() < 4
        || arguments[..3] != ["install", "--yes", "--no-install-recommends"]
        || arguments[3..]
            .iter()
            .any(|package| PackageName::parse(package).is_err())
    {
        return Err(invalid_command("apt command shape is not reviewed"));
    }
    Ok(())
}

fn validate_groupadd(spec: &CommandSpec, arguments: &[&str]) -> Result<(), LaneExecutionError> {
    if spec.program != Path::new(GROUPADD)
        || arguments.len() != 2
        || arguments[0] != "--system"
        || LinuxAccountName::parse(arguments[1]).is_err()
    {
        return Err(invalid_command(
            "group creation command shape is not reviewed",
        ));
    }
    Ok(())
}

fn validate_useradd(spec: &CommandSpec, arguments: &[&str]) -> Result<(), LaneExecutionError> {
    let valid = spec.program == Path::new(USERADD)
        && arguments.len() == 9
        && arguments[0] == "--system"
        && arguments[1] == "--gid"
        && LinuxAccountName::parse(arguments[2]).is_ok()
        && arguments[3] == "--home-dir"
        && canonical_absolute_path(arguments[4])
        && arguments[5] == "--shell"
        && arguments[6] == NOLOGIN
        && arguments[7] == "--no-create-home"
        && LinuxAccountName::parse(arguments[8]).is_ok();
    if valid {
        Ok(())
    } else {
        Err(invalid_command(
            "user creation command shape is not reviewed",
        ))
    }
}

fn validate_loginctl(spec: &CommandSpec, arguments: &[&str]) -> Result<(), LaneExecutionError> {
    if spec.program != Path::new(LOGINCTL)
        || arguments.len() != 2
        || arguments[0] != "enable-linger"
        || LinuxAccountName::parse(arguments[1]).is_err()
    {
        return Err(invalid_command("linger command shape is not reviewed"));
    }
    Ok(())
}

fn validate_runner_evidence(runner: &VerifiedRunnerUser) -> Result<(), LaneExecutionError> {
    if runner.uid() == 0
        || runner.primary_gid() == 0
        || runner.subordinate_uid_count() < MIN_SUBORDINATE_ID_COUNT
        || runner.subordinate_gid_count() < MIN_SUBORDINATE_ID_COUNT
        || !canonical_absolute_path(runner.home())
        || runner.runtime_directory() != Path::new(&format!("/run/user/{}", runner.uid()))
    {
        return Err(LaneExecutionError::public(
            LaneExecutionErrorKind::InvalidRunnerEvidence,
            "runner-user evidence is incomplete or unsafe",
        ));
    }
    Ok(())
}

fn validate_runner_command(
    command: &LaneCommand,
    runner: &VerifiedRunnerUser,
) -> Result<(), LaneExecutionError> {
    let spec = command.spec();
    if !spec.environment.is_empty() || spec.program != Path::new(RUNUSER) {
        return Err(invalid_command(
            "runner-user command outer boundary is not reviewed",
        ));
    }
    let (inner_program, inner_arguments): (&str, &[&str]) = match command.kind() {
        LaneCommandKind::RunnerPodmanInfo => (PODMAN, &["info", "--format", "json"]),
        LaneCommandKind::RunnerGitVersion => (GIT, &["--version"]),
        LaneCommandKind::AptInstall
        | LaneCommandKind::EnsureSystemGroup
        | LaneCommandKind::EnsureSystemUser
        | LaneCommandKind::EnableLinger => {
            return Err(invalid_command(
                "root command kind cannot execute in the runner-user lane",
            ));
        }
    };

    let mut expected = vec![
        "--user".to_owned(),
        runner.username().as_str().to_owned(),
        "--".to_owned(),
        ENV.to_owned(),
        "--ignore-environment".to_owned(),
        format!("HOME={}", runner.home()),
        format!("USER={}", runner.username().as_str()),
        format!("LOGNAME={}", runner.username().as_str()),
        format!("XDG_RUNTIME_DIR={}", runner.runtime_directory().display()),
        inner_program.to_owned(),
    ];
    expected.extend(
        inner_arguments
            .iter()
            .map(|argument| (*argument).to_owned()),
    );
    let arguments = plain_arguments(spec)?;
    if arguments
        .iter()
        .copied()
        .ne(expected.iter().map(String::as_str))
    {
        return Err(invalid_command(
            "runner-user command argv or environment boundary differs from verified evidence",
        ));
    }
    Ok(())
}

fn plain_arguments(spec: &CommandSpec) -> Result<Vec<&str>, LaneExecutionError> {
    spec.arguments
        .iter()
        .map(|value| match value {
            CommandValue::Plain(value) => Ok(value.as_str()),
            CommandValue::Secret(_) => Err(invalid_command(
                "lane commands cannot contain secret or opaque argv values",
            )),
        })
        .collect()
}

fn canonical_absolute_path(value: &str) -> bool {
    let path = Path::new(value);
    !value.is_empty()
        && value.len() <= 4_096
        && !value.ends_with('/')
        && !value.chars().any(char::is_control)
        && path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

fn invalid_command(message: impl Into<String>) -> LaneExecutionError {
    LaneExecutionError::public(LaneExecutionErrorKind::InvalidCommand, message)
}

fn parse_effective_uid(input: &str) -> io::Result<u32> {
    let mut effective_uid = None;
    for line in input.lines() {
        let mut fields = line.split_whitespace();
        if fields.next() != Some("Uid:") {
            continue;
        }
        if effective_uid.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "process status contains multiple UID records",
            ));
        }
        let values = fields.collect::<Vec<_>>();
        if values.len() != 4 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "process status UID record is malformed",
            ));
        }
        let value = values[1];
        let parsed = value.parse::<u32>().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "process status effective UID is malformed",
            )
        })?;
        if parsed.to_string() != value {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "process status effective UID is noncanonical",
            ));
        }
        effective_uid = Some(parsed);
    }
    effective_uid.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "process status is missing the UID record",
        )
    })
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::io;

    use crate::journal::{ExecutionLane, PlannedMutation, Preconditions, RollbackClass};
    use crate::lane_command::{LaneCommand, LinuxAccountName, PackageName, RunnerUserContext};
    use crate::process::{CommandExecutor, CommandSpec, ExecutionRecord};
    use crate::runner_user::verified_runner_user_for_test;

    use super::{
        ENV, GIT, LaneExecutionError, LaneExecutionErrorKind, PODMAN, PrivilegeProbe,
        ReviewedExecutableVerifier, RootLaneExecutor, RunnerUserLaneExecutor, parse_effective_uid,
    };

    struct FakeProcess {
        calls: RefCell<Vec<CommandSpec>>,
        record: ExecutionRecord,
        fail: bool,
    }

    impl FakeProcess {
        fn successful() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                record: ExecutionRecord {
                    argv: vec!["not returned".to_owned()],
                    environment_keys: vec!["SECRET".to_owned()],
                    status: Some(0),
                    success: true,
                    stdout: "sensitive stdout".to_owned(),
                    stderr: "sensitive stderr".to_owned(),
                },
                fail: false,
            }
        }
    }

    impl CommandExecutor for FakeProcess {
        fn execute(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord> {
            self.calls.borrow_mut().push(spec.clone());
            if self.fail {
                Err(io::Error::other("private spawn failure"))
            } else {
                Ok(self.record.clone())
            }
        }
    }

    #[derive(Default)]
    struct FakeVerifier {
        calls: Cell<usize>,
        fail: bool,
    }

    impl ReviewedExecutableVerifier for FakeVerifier {
        fn verify(&self, _command: &LaneCommand) -> Result<(), LaneExecutionError> {
            self.calls.set(self.calls.get() + 1);
            if self.fail {
                Err(LaneExecutionError::public(
                    LaneExecutionErrorKind::ExecutableVerification,
                    "reviewed executable evidence failed",
                ))
            } else {
                Ok(())
            }
        }
    }

    struct FakePrivilege {
        uid: u32,
        fail: bool,
    }

    impl PrivilegeProbe for FakePrivilege {
        fn effective_uid(&self) -> io::Result<u32> {
            if self.fail {
                Err(io::Error::other("private privilege failure"))
            } else {
                Ok(self.uid)
            }
        }
    }

    fn action(id: &str, lane: ExecutionLane) -> PlannedMutation {
        PlannedMutation::new(
            id,
            lane,
            "test lane execution",
            RollbackClass::Reversible,
            Preconditions::new(["evidence verified"]),
        )
    }

    fn account(value: &str) -> LinuxAccountName {
        LinuxAccountName::parse(value).expect("account name")
    }

    fn runner_context(username: &str, uid: u32, gid: u32) -> RunnerUserContext {
        RunnerUserContext::new(account(username), uid, gid, "/srv/project-runner")
            .expect("runner context")
    }

    #[test]
    fn root_executor_delivers_exact_empty_environment_command() {
        let process = FakeProcess::successful();
        let executables = FakeVerifier::default();
        let executor = RootLaneExecutor::new(
            process,
            executables,
            FakePrivilege {
                uid: 0,
                fail: false,
            },
        );
        let command = LaneCommand::apt_install(
            &action("install-tools", ExecutionLane::Root),
            &[PackageName::parse("podman").expect("package")],
        )
        .expect("apt command");

        let receipt = executor.execute(&command).expect("execute root lane");
        assert!(receipt.success());
        assert_eq!(receipt.status(), Some(0));
        let calls = executor.process.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].environment.is_empty());
        assert_eq!(
            calls[0].displayed_argv(),
            [
                "/usr/bin/apt-get",
                "install",
                "--yes",
                "--no-install-recommends",
                "podman",
            ]
        );
        let json = serde_json::to_string(&receipt).expect("serialize receipt");
        assert!(!json.contains("sensitive"));
        assert!(!json.contains("SECRET"));
    }

    #[test]
    fn runner_executor_delivers_exact_sealed_environment_boundary() {
        let process = FakeProcess::successful();
        let executables = FakeVerifier::default();
        let executor = RunnerUserLaneExecutor::new(
            process,
            executables,
            FakePrivilege {
                uid: 0,
                fail: false,
            },
        );
        let context = runner_context("project-runner", 1001, 1001);
        let runner =
            verified_runner_user_for_test("project-runner", 1001, 1001, "/srv/project-runner");
        let command = LaneCommand::runner_git_version(
            &action("inspect-git", ExecutionLane::RunnerUser),
            &context,
        )
        .expect("runner command");

        let receipt = executor
            .execute(&command, &runner)
            .expect("execute runner-user lane");
        assert!(receipt.success());
        let calls = executor.process.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].environment.is_empty());
        assert_eq!(
            calls[0].displayed_argv(),
            [
                "/usr/sbin/runuser",
                "--user",
                "project-runner",
                "--",
                ENV,
                "--ignore-environment",
                "HOME=/srv/project-runner",
                "USER=project-runner",
                "LOGNAME=project-runner",
                "XDG_RUNTIME_DIR=/run/user/1001",
                GIT,
                "--version",
            ]
        );
    }

    #[test]
    fn lane_mismatch_fails_before_evidence_or_process_execution() {
        let process = FakeProcess::successful();
        let executables = FakeVerifier::default();
        let executor = RootLaneExecutor::new(
            process,
            executables,
            FakePrivilege {
                uid: 0,
                fail: false,
            },
        );
        let context = runner_context("project-runner", 1001, 1001);
        let command = LaneCommand::runner_podman_info(
            &action("inspect-podman", ExecutionLane::RunnerUser),
            &context,
        )
        .expect("runner command");

        let error = executor
            .execute(&command)
            .expect_err("root executor must reject runner command");
        assert_eq!(error.kind(), LaneExecutionErrorKind::LaneMismatch);
        assert_eq!(executor.executables.calls.get(), 0);
        assert!(executor.process.calls.borrow().is_empty());
    }

    #[test]
    fn unsupported_privilege_fails_with_recovery_instruction_before_mutation() {
        let process = FakeProcess::successful();
        let executables = FakeVerifier::default();
        let executor = RootLaneExecutor::new(
            process,
            executables,
            FakePrivilege {
                uid: 1000,
                fail: false,
            },
        );
        let command = LaneCommand::apt_install(
            &action("install-tools", ExecutionLane::Root),
            &[PackageName::parse("git").expect("package")],
        )
        .expect("apt command");

        let error = executor
            .execute(&command)
            .expect_err("non-root execution must fail");
        assert_eq!(error.kind(), LaneExecutionErrorKind::UnsupportedPrivilege);
        assert!(error.message().contains("explicit elevation"));
        assert_eq!(executor.executables.calls.get(), 0);
        assert!(executor.process.calls.borrow().is_empty());
    }

    #[test]
    fn runner_evidence_mismatch_fails_before_executable_or_process_execution() {
        let process = FakeProcess::successful();
        let executables = FakeVerifier::default();
        let executor = RunnerUserLaneExecutor::new(
            process,
            executables,
            FakePrivilege {
                uid: 0,
                fail: false,
            },
        );
        let context = runner_context("project-runner", 1001, 1001);
        let runner = verified_runner_user_for_test("other-runner", 1002, 1002, "/srv/other-runner");
        let command = LaneCommand::runner_podman_info(
            &action("inspect-podman", ExecutionLane::RunnerUser),
            &context,
        )
        .expect("runner command");

        let error = executor
            .execute(&command, &runner)
            .expect_err("mismatched runner evidence must fail");
        assert_eq!(error.kind(), LaneExecutionErrorKind::InvalidCommand);
        assert_eq!(executor.executables.calls.get(), 0);
        assert!(executor.process.calls.borrow().is_empty());
    }

    #[test]
    fn executable_verification_failure_stops_process_execution() {
        let process = FakeProcess::successful();
        let executables = FakeVerifier {
            calls: Cell::new(0),
            fail: true,
        };
        let executor = RootLaneExecutor::new(
            process,
            executables,
            FakePrivilege {
                uid: 0,
                fail: false,
            },
        );
        let command = LaneCommand::apt_install(
            &action("install-tools", ExecutionLane::Root),
            &[PackageName::parse("git").expect("package")],
        )
        .expect("apt command");

        let error = executor
            .execute(&command)
            .expect_err("untrusted executable must fail");
        assert_eq!(error.kind(), LaneExecutionErrorKind::ExecutableVerification);
        assert_eq!(executor.executables.calls.get(), 1);
        assert!(executor.process.calls.borrow().is_empty());
    }

    #[test]
    fn proc_status_parser_requires_one_canonical_effective_uid() {
        assert_eq!(
            parse_effective_uid("Name:\tsmolrunner\nUid:\t1000\t0\t1000\t1000\n")
                .expect("effective UID"),
            0
        );
        for input in [
            "Name:\tsmolrunner\n",
            "Uid:\t1000\t00\t1000\t1000\n",
            "Uid:\t1000\t0\t1000\n",
            "Uid:\t1000\t0\t1000\t1000\nUid:\t1000\t0\t1000\t1000\n",
        ] {
            parse_effective_uid(input).expect_err("malformed UID evidence must fail");
        }
    }

    #[test]
    fn runner_command_constants_remain_absolute_reviewed_paths() {
        assert_eq!(ENV, "/usr/bin/env");
        assert_eq!(PODMAN, "/usr/bin/podman");
        assert_eq!(GIT, "/usr/bin/git");
    }
}
