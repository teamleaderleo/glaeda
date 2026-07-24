from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    file = Path(path)
    text = file.read_text()
    count = text.count(old)
    if count != 1:
        raise RuntimeError(
            f"expected exactly one match in {path}, found {count}: {old[:120]!r}"
        )
    file.write_text(text.replace(old, new, 1))


replace_once(
    "src/lane_executor.rs",
    """impl ReviewedExecutableVerifier for SystemExecutableVerifier {
    fn verify(&self, command: &LaneCommand) -> Result<(), LaneExecutionError> {
        verify_lane_command(command).map(|_| ()).map_err(|error| {
            LaneExecutionError::new(
                LaneExecutionErrorKind::ExecutableVerification,
                error.message().to_owned(),
            )
        })
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct RootLaneExecutor;""",
    """impl ReviewedExecutableVerifier for SystemExecutableVerifier {
    fn verify(&self, command: &LaneCommand) -> Result<(), LaneExecutionError> {
        verify_lane_command(command).map(|_| ()).map_err(|error| {
            LaneExecutionError::new(
                LaneExecutionErrorKind::ExecutableVerification,
                error.message().to_owned(),
            )
        })
    }
}

trait RunnerEvidence {
    fn username(&self) -> &LinuxAccountName;
    fn uid(&self) -> u32;
    fn primary_gid(&self) -> u32;
    fn home(&self) -> &str;
    fn runtime_directory(&self) -> &Path;
    fn subordinate_uid_count(&self) -> u64;
    fn subordinate_gid_count(&self) -> u64;
}

impl RunnerEvidence for VerifiedRunnerUser {
    fn username(&self) -> &LinuxAccountName {
        self.username()
    }

    fn uid(&self) -> u32 {
        self.uid()
    }

    fn primary_gid(&self) -> u32 {
        self.primary_gid()
    }

    fn home(&self) -> &str {
        self.home()
    }

    fn runtime_directory(&self) -> &Path {
        self.runtime_directory()
    }

    fn subordinate_uid_count(&self) -> u64 {
        self.subordinate_uid_count()
    }

    fn subordinate_gid_count(&self) -> u64 {
        self.subordinate_gid_count()
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct RootLaneExecutor;""",
)

replace_once(
    "src/lane_executor.rs",
    """fn execute_runner_user_lane(
    process: &impl CommandExecutor,
    executables: &impl ReviewedExecutableVerifier,
    privilege: &impl PrivilegeProbe,
    command: &LaneCommand,
    runner: &VerifiedRunnerUser,
) -> Result<LaneExecutionRecord, LaneExecutionError> {""",
    """fn execute_runner_user_lane(
    process: &impl CommandExecutor,
    executables: &impl ReviewedExecutableVerifier,
    privilege: &impl PrivilegeProbe,
    command: &LaneCommand,
    runner: &impl RunnerEvidence,
) -> Result<LaneExecutionRecord, LaneExecutionError> {""",
)

replace_once(
    "src/lane_executor.rs",
    "fn validate_runner_evidence(runner: &VerifiedRunnerUser) -> Result<(), LaneExecutionError> {",
    "fn validate_runner_evidence(runner: &impl RunnerEvidence) -> Result<(), LaneExecutionError> {",
)

replace_once(
    "src/lane_executor.rs",
    """fn validate_runner_command(
    command: &LaneCommand,
    runner: &VerifiedRunnerUser,
) -> Result<(), LaneExecutionError> {""",
    """fn validate_runner_command(
    command: &LaneCommand,
    runner: &impl RunnerEvidence,
) -> Result<(), LaneExecutionError> {""",
)

replace_once(
    "src/lane_executor.rs",
    """    use crate::process::{CommandExecutor, CommandSpec, ExecutionRecord};
    use crate::runner_user::verified_runner_user_for_test;

    use super::{
        ENV, GIT, LaneExecutionError, LaneExecutionErrorKind, PODMAN, PrivilegeProbe,
        ReviewedExecutableVerifier, execute_root_lane, execute_runner_user_lane,
        parse_effective_uid,
    };""",
    """    use crate::process::{CommandExecutor, CommandSpec, ExecutionRecord};
    use crate::runner_user::MIN_SUBORDINATE_ID_COUNT;

    use super::{
        ENV, GIT, LaneExecutionError, LaneExecutionErrorKind, PODMAN, PrivilegeProbe,
        ReviewedExecutableVerifier, RunnerEvidence, execute_root_lane, execute_runner_user_lane,
        parse_effective_uid,
    };""",
)

replace_once(
    "src/lane_executor.rs",
    """    fn runner_context(username: &str, uid: u32, gid: u32) -> RunnerUserContext {
        RunnerUserContext::new(account(username), uid, gid, "/srv/project-runner")
            .expect("runner context")
    }

    #[test]""",
    """    fn runner_context(username: &str, uid: u32, gid: u32) -> RunnerUserContext {
        RunnerUserContext::new(account(username), uid, gid, "/srv/project-runner")
            .expect("runner context")
    }

    struct FakeRunnerEvidence {
        username: LinuxAccountName,
        uid: u32,
        primary_gid: u32,
        home: String,
        runtime_directory: String,
        subordinate_uid_count: u64,
        subordinate_gid_count: u64,
    }

    impl RunnerEvidence for FakeRunnerEvidence {
        fn username(&self) -> &LinuxAccountName {
            &self.username
        }

        fn uid(&self) -> u32 {
            self.uid
        }

        fn primary_gid(&self) -> u32 {
            self.primary_gid
        }

        fn home(&self) -> &str {
            &self.home
        }

        fn runtime_directory(&self) -> &std::path::Path {
            std::path::Path::new(&self.runtime_directory)
        }

        fn subordinate_uid_count(&self) -> u64 {
            self.subordinate_uid_count
        }

        fn subordinate_gid_count(&self) -> u64 {
            self.subordinate_gid_count
        }
    }

    fn runner_evidence(username: &str, uid: u32, gid: u32, home: &str) -> FakeRunnerEvidence {
        FakeRunnerEvidence {
            username: account(username),
            uid,
            primary_gid: gid,
            home: home.to_owned(),
            runtime_directory: format!("/run/user/{uid}"),
            subordinate_uid_count: MIN_SUBORDINATE_ID_COUNT,
            subordinate_gid_count: MIN_SUBORDINATE_ID_COUNT,
        }
    }

    #[test]""",
)

for old, new in [
    (
        'verified_runner_user_for_test("project-runner", 1001, 1001, "/srv/project-runner")',
        'runner_evidence("project-runner", 1001, 1001, "/srv/project-runner")',
    ),
    (
        'verified_runner_user_for_test("other-runner", 1002, 1002, "/srv/other-runner")',
        'runner_evidence("other-runner", 1002, 1002, "/srv/other-runner")',
    ),
    (
        'verified_runner_user_for_test("project-runner", 0, 1001, "/srv/project-runner")',
        'runner_evidence("project-runner", 0, 1001, "/srv/project-runner")',
    ),
]:
    replace_once("src/lane_executor.rs", old, new)

replace_once(
    "src/lib.rs",
    """pub mod lane_command;
#[cfg(target_os = "linux")]
pub mod lane_executable;""",
    """pub mod lane_command;
#[cfg(target_os = "linux")]
pub mod lane_executable;
#[cfg(target_os = "linux")]
pub mod lane_executor;""",
)

replace_once(
    "docs/ROADMAP.md",
    "- [ ] Root and runner-user lane implementations.",
    "- [x] Typed root and runner-user lane executors with sealed command and environment boundaries.\n- [ ] Integrate lane executors with durable reconciliation journals.",
)
