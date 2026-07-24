from pathlib import Path

path = Path("src/lane_executor.rs")
text = path.read_text(encoding="utf-8")

text = text.replace("    pub fn public(kind: LaneExecutionErrorKind, message: impl Into<String>) -> Self {", "    fn new(kind: LaneExecutionErrorKind, message: impl Into<String>) -> Self {")
text = text.replace("LaneExecutionError::public(", "LaneExecutionError::new(")
text = text.replace("pub trait PrivilegeProbe {", "trait PrivilegeProbe {")
text = text.replace("pub trait ReviewedExecutableVerifier {", "trait ReviewedExecutableVerifier {")

start = text.index("#[derive(Debug)]\npub struct RootLaneExecutor")
end = text.index("\nfn execute_bounded(", start)
replacement = '''#[derive(Debug, Default, Clone, Copy)]
pub struct RootLaneExecutor;

impl RootLaneExecutor {
    #[must_use]
    pub const fn system() -> Self {
        Self
    }

    /// Execute one reviewed root-lane command after all evidence checks succeed.
    ///
    /// The returned receipt intentionally excludes child stdout and stderr.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for lane mismatch, malformed command shape, unsupported privilege,
    /// untrusted executables, or process startup failure.
    pub fn execute(&self, command: &LaneCommand) -> Result<LaneExecutionReceipt, LaneExecutionError> {
        execute_root_lane(
            &ProcessExecutor,
            &SystemExecutableVerifier,
            &ProcStatusPrivilegeProbe,
            command,
        )
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct RunnerUserLaneExecutor;

impl RunnerUserLaneExecutor {
    #[must_use]
    pub const fn system() -> Self {
        Self
    }

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
        execute_runner_user_lane(
            &ProcessExecutor,
            &SystemExecutableVerifier,
            &ProcStatusPrivilegeProbe,
            command,
            runner,
        )
    }
}

fn execute_root_lane(
    process: &impl CommandExecutor,
    executables: &impl ReviewedExecutableVerifier,
    privilege: &impl PrivilegeProbe,
    command: &LaneCommand,
) -> Result<LaneExecutionReceipt, LaneExecutionError> {
    require_lane(command, ExecutionLane::Root)?;
    validate_root_command(command)?;
    require_effective_root(privilege)?;
    executables.verify(command)?;
    execute_bounded(process, command)
}

fn execute_runner_user_lane(
    process: &impl CommandExecutor,
    executables: &impl ReviewedExecutableVerifier,
    privilege: &impl PrivilegeProbe,
    command: &LaneCommand,
    runner: &VerifiedRunnerUser,
) -> Result<LaneExecutionReceipt, LaneExecutionError> {
    require_lane(command, ExecutionLane::RunnerUser)?;
    validate_runner_evidence(runner)?;
    validate_runner_command(command, runner)?;
    require_effective_root(privilege)?;
    executables.verify(command)?;
    execute_bounded(process, command)
}
'''
text = text[:start] + replacement + text[end:]

old = '''        ENV, GIT, LaneExecutionError, LaneExecutionErrorKind, PODMAN, PrivilegeProbe,
        ReviewedExecutableVerifier, RootLaneExecutor, RunnerUserLaneExecutor, parse_effective_uid,
'''
new = '''        ENV, GIT, LaneExecutionError, LaneExecutionErrorKind, PODMAN, PrivilegeProbe,
        ReviewedExecutableVerifier, execute_root_lane, execute_runner_user_lane,
        parse_effective_uid,
'''
if text.count(old) != 1:
    raise SystemExit("unexpected test imports")
text = text.replace(old, new, 1)

replacements = [
('''        let executor = RootLaneExecutor::new(
            process,
            executables,
            FakePrivilege {
                uid: 0,
                fail: false,
            },
        );
''', '''        let privilege = FakePrivilege {
            uid: 0,
            fail: false,
        };
'''),
('''        let receipt = executor.execute(&command).expect("execute root lane");
''', '''        let receipt = execute_root_lane(&process, &executables, &privilege, &command)
            .expect("execute root lane");
'''),
('''        let calls = executor.process.calls.borrow();
''', '''        let calls = process.calls.borrow();
'''),
('''        let executor = RunnerUserLaneExecutor::new(
            process,
            executables,
            FakePrivilege {
                uid: 0,
                fail: false,
            },
        );
''', '''        let privilege = FakePrivilege {
            uid: 0,
            fail: false,
        };
'''),
('''        let receipt = executor
            .execute(&command, &runner)
            .expect("execute runner-user lane");
''', '''        let receipt = execute_runner_user_lane(
            &process,
            &executables,
            &privilege,
            &command,
            &runner,
        )
        .expect("execute runner-user lane");
'''),
('''        let error = executor
            .execute(&command)
            .expect_err("root executor must reject runner command");
''', '''        let error = execute_root_lane(&process, &executables, &privilege, &command)
            .expect_err("root executor must reject runner command");
'''),
('''        assert_eq!(executor.executables.calls.get(), 0);
        assert!(executor.process.calls.borrow().is_empty());
''', '''        assert_eq!(executables.calls.get(), 0);
        assert!(process.calls.borrow().is_empty());
'''),
('''        let executor = RootLaneExecutor::new(
            process,
            executables,
            FakePrivilege {
                uid: 1000,
                fail: false,
            },
        );
''', '''        let privilege = FakePrivilege {
            uid: 1000,
            fail: false,
        };
'''),
('''        let error = executor
            .execute(&command)
            .expect_err("non-root execution must fail");
''', '''        let error = execute_root_lane(&process, &executables, &privilege, &command)
            .expect_err("non-root execution must fail");
'''),
('''        let error = executor
            .execute(&command, &runner)
            .expect_err("mismatched runner evidence must fail");
''', '''        let error = execute_runner_user_lane(
            &process,
            &executables,
            &privilege,
            &command,
            &runner,
        )
        .expect_err("mismatched runner evidence must fail");
'''),
('''        let executor = RootLaneExecutor::new(
            process,
            executables,
            FakePrivilege {
                uid: 0,
                fail: false,
            },
        );
''', '''        let privilege = FakePrivilege {
            uid: 0,
            fail: false,
        };
'''),
('''        let error = executor
            .execute(&command)
            .expect_err("untrusted executable must fail");
''', '''        let error = execute_root_lane(&process, &executables, &privilege, &command)
            .expect_err("untrusted executable must fail");
'''),
('''        assert_eq!(executor.executables.calls.get(), 1);
        assert!(executor.process.calls.borrow().is_empty());
''', '''        assert_eq!(executables.calls.get(), 1);
        assert!(process.calls.borrow().is_empty());
'''),
]
for old, new in replacements:
    if old in text:
        text = text.replace(old, new, 1)

if "RootLaneExecutor::new" in text or "RunnerUserLaneExecutor::new" in text:
    raise SystemExit("public injectable executor construction remains")
if "executor.process" in text or "executor.executables" in text:
    raise SystemExit("stale generic executor test access remains")
path.write_text(text, encoding="utf-8")
