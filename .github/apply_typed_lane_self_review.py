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
    "use std::io;",
    "use std::io::{self, Read};",
)

replace_once(
    "src/lane_executor.rs",
    """        let bytes = fs::read(PROC_SELF_STATUS)?;
        if bytes.len() > MAX_PROC_STATUS_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "process status exceeds the configured size limit",
            ));
        }
        let input = std::str::from_utf8(&bytes).map_err(|_| {""",
    """        let mut bytes = Vec::with_capacity(MAX_PROC_STATUS_BYTES + 1);
        let mut reader =
            fs::File::open(PROC_SELF_STATUS)?.take((MAX_PROC_STATUS_BYTES + 1) as u64);
        reader.read_to_end(&mut bytes)?;
        if bytes.len() > MAX_PROC_STATUS_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "process status exceeds the configured size limit",
            ));
        }
        let input = std::str::from_utf8(&bytes).map_err(|_| {""",
)

replace_once(
    "src/lane_command.rs",
    """            LaneCommandKind::AptInstall
            | LaneCommandKind::EnsureSystemGroup
            | LaneCommandKind::EnsureSystemUser
            | LaneCommandKind::EnableLinger => vec![outer],""",
    """            LaneCommandKind::EnsureSystemUser => vec![outer, Path::new(NOLOGIN)],
            LaneCommandKind::AptInstall
            | LaneCommandKind::EnsureSystemGroup
            | LaneCommandKind::EnableLinger => vec![outer],""",
)

replace_once(
    "src/lane_command.rs",
    """mod tests {
    use crate::journal::{ExecutionLane, PlannedMutation, Preconditions, RollbackClass};""",
    """mod tests {
    use std::path::Path;

    use crate::journal::{ExecutionLane, PlannedMutation, Preconditions, RollbackClass};""",
)

replace_once(
    "src/lane_command.rs",
    """    #[test]
    fn runner_user_commands_have_exact_runuser_boundary_and_environment() {""",
    """    #[test]
    fn system_user_command_requires_useradd_and_nologin_verification() {
        let root = action(ExecutionLane::Root);
        let account = account("project-runner");
        let command = LaneCommand::ensure_system_user(
            &root,
            &account,
            &account,
            "/var/lib/project-runner",
        )
        .expect("user command");
        assert_eq!(
            command.required_programs(),
            [Path::new(USERADD), Path::new(NOLOGIN)]
        );
    }

    #[test]
    fn runner_user_commands_have_exact_runuser_boundary_and_environment() {""",
)

replace_once(
    "docs/adr/0015-typed-privilege-lane-executors.md",
    """4. verify every required executable immediately before process creation; and
5. delegate only to the shell-free bounded `ProcessExecutor`.""",
    """4. verify every required executable immediately before process creation, including the configured `nologin` shell for user creation; and
5. delegate only to the shell-free bounded `ProcessExecutor`.""",
)

replace_once(
    "docs/adr/0015-typed-privilege-lane-executors.md",
    """The root executor accepts only reviewed root command kinds. Its `CommandSpec` environment must be empty, and no manifest value can choose the program or introduce a shell, `sudo`, `su`, `runuser`, or arbitrary environment variable.""",
    """The root executor accepts only reviewed root command kinds. Its `CommandSpec` environment must be empty, and no manifest value can choose the program or introduce a shell, `sudo`, `su`, `runuser`, or arbitrary environment variable. Effective-UID evidence is read through a fixed-size reader, so even malformed process-status input cannot cause an unbounded allocation.""",
)
