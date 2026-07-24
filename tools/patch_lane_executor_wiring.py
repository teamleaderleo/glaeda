from pathlib import Path

executor_path = Path("src/lane_executor.rs")
executor = executor_path.read_text(encoding="utf-8")
if executor.startswith("use std::cell::Cell;\n"):
    executor = executor.removeprefix("use std::cell::Cell;\n")
old = "    use std::cell::RefCell;\n"
new = "    use std::cell::{Cell, RefCell};\n    use std::io;\n"
if executor.count(old) != 1:
    raise SystemExit("unexpected lane-executor test imports")
executor = executor.replace(old, new, 1)
executor_path.write_text(executor, encoding="utf-8")

runner_path = Path("src/runner_user.rs")
runner = runner_path.read_text(encoding="utf-8")
marker = "#[cfg(test)]\nmod tests {\n"
fixture = '''#[cfg(test)]
pub(crate) fn verified_runner_user_for_test(
    username: &str,
    uid: u32,
    primary_gid: u32,
    home: &str,
) -> VerifiedRunnerUser {
    VerifiedRunnerUser {
        username: LinuxAccountName::parse(username).expect("test runner username"),
        uid,
        primary_gid,
        home: home.to_owned(),
        runtime_directory: PathBuf::from(format!("/run/user/{uid}")),
        subordinate_uid_count: MIN_SUBORDINATE_ID_COUNT,
        subordinate_gid_count: MIN_SUBORDINATE_ID_COUNT,
    }
}

'''
if runner.count(marker) != 1 or "verified_runner_user_for_test" in runner:
    raise SystemExit("unexpected runner-user test fixture state")
runner = runner.replace(marker, fixture + marker, 1)
runner_path.write_text(runner, encoding="utf-8")

lib_path = Path("src/lib.rs")
lib = lib_path.read_text(encoding="utf-8")
old = '''pub mod lane_command;
#[cfg(target_os = "linux")]
pub mod lane_executable;
'''
new = '''pub mod lane_command;
/// Privilege-gated execution for reviewed root and runner-user lane commands.
#[cfg(target_os = "linux")]
pub mod lane_executor;
#[cfg(target_os = "linux")]
pub mod lane_executable;
'''
if lib.count(old) != 1:
    raise SystemExit("unexpected lane module exports")
lib = lib.replace(old, new, 1)
lib_path.write_text(lib, encoding="utf-8")
