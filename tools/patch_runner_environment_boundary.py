from pathlib import Path

lane_path = Path("src/lane_command.rs")
lane = lane_path.read_text(encoding="utf-8")

old = 'const RUNUSER: &str = "/usr/sbin/runuser";\nconst PODMAN: &str = "/usr/bin/podman";\n'
new = 'const RUNUSER: &str = "/usr/sbin/runuser";\nconst ENV: &str = "/usr/bin/env";\nconst PODMAN: &str = "/usr/bin/podman";\n'
if lane.count(old) != 1:
    raise SystemExit("unexpected runner executable constants")
lane = lane.replace(old, new, 1)

old = '''        match self.kind {
            LaneCommandKind::RunnerPodmanInfo => vec![outer, Path::new(PODMAN)],
            LaneCommandKind::RunnerGitVersion => vec![outer, Path::new(GIT)],
'''
new = '''        match self.kind {
            LaneCommandKind::RunnerPodmanInfo => {
                vec![outer, Path::new(ENV), Path::new(PODMAN)]
            }
            LaneCommandKind::RunnerGitVersion => {
                vec![outer, Path::new(ENV), Path::new(GIT)]
            }
'''
if lane.count(old) != 1:
    raise SystemExit("unexpected required-program mapping")
lane = lane.replace(old, new, 1)

start = lane.index("fn runner_user_spec(")
end = lane.index("\nfn require_lane", start)
replacement = '''fn runner_user_spec(
    runner: &RunnerUserContext,
    inner_program: &str,
    arguments: &[&str],
) -> CommandSpec {
    let mut spec = CommandSpec::new(RUNUSER)
        .argument("--user")
        .argument(runner.username.as_str())
        .argument("--")
        .argument(ENV)
        .argument("--ignore-environment")
        .argument(format!("HOME={}", runner.home()))
        .argument(format!("USER={}", runner.username.as_str()))
        .argument(format!("LOGNAME={}", runner.username.as_str()))
        .argument(format!("XDG_RUNTIME_DIR={}", runner.runtime_directory()))
        .argument(inner_program);
    for argument in arguments {
        spec = spec.argument(*argument);
    }
    spec
}
'''
lane = lane[:start] + replacement + lane[end:]

old = '''        APT_GET, GIT, GROUPADD, LOGINCTL, LaneCommand, LaneCommandKind, LinuxAccountName, NOLOGIN,
        PODMAN, PackageName, RUNUSER, RunnerUserContext, USERADD,
'''
new = '''        APT_GET, ENV, GIT, GROUPADD, LOGINCTL, LaneCommand, LaneCommandKind, LinuxAccountName,
        NOLOGIN, PODMAN, PackageName, RUNUSER, RunnerUserContext, USERADD,
'''
if lane.count(old) != 1:
    raise SystemExit("unexpected lane-command test import")
lane = lane.replace(old, new, 1)

old = '''            [
                RUNUSER,
                "--user",
                "project-runner",
                "--",
                PODMAN,
                "info",
                "--format",
                "json",
            ]
'''
new = '''            [
                RUNUSER,
                "--user",
                "project-runner",
                "--",
                ENV,
                "--ignore-environment",
                "HOME=/srv/runner",
                "USER=project-runner",
                "LOGNAME=project-runner",
                "XDG_RUNTIME_DIR=/run/user/1001",
                PODMAN,
                "info",
                "--format",
                "json",
            ]
'''
if lane.count(old) != 1:
    raise SystemExit("unexpected podman argv fixture")
lane = lane.replace(old, new, 1)

old = '''            [RUNUSER, "--user", "project-runner", "--", GIT, "--version"]
        );
        assert_eq!(
            git.spec()
                .environment
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["HOME", "LOGNAME", "USER", "XDG_RUNTIME_DIR"]
        );
        assert_eq!(
            git.spec().environment.get("HOME"),
            Some(&CommandValue::Plain("/srv/runner".to_owned()))
        );
        assert_eq!(
            git.spec().environment.get("XDG_RUNTIME_DIR"),
            Some(&CommandValue::Plain("/run/user/1001".to_owned()))
        );
'''
new = '''            [
                RUNUSER,
                "--user",
                "project-runner",
                "--",
                ENV,
                "--ignore-environment",
                "HOME=/srv/runner",
                "USER=project-runner",
                "LOGNAME=project-runner",
                "XDG_RUNTIME_DIR=/run/user/1001",
                GIT,
                "--version",
            ]
        );
        assert!(git.spec().environment.is_empty());
        assert!(git
            .spec()
            .arguments
            .iter()
            .all(|value| matches!(value, CommandValue::Plain(_))));
'''
if lane.count(old) != 1:
    raise SystemExit("unexpected git argv and environment fixture")
lane = lane.replace(old, new, 1)

lane_path.write_text(lane, encoding="utf-8")

executable_path = Path("src/lane_executable.rs")
executable = executable_path.read_text(encoding="utf-8")
old = '''        assert_eq!(verified.len(), 2);
        assert_eq!(verified[0].path(), Path::new("/usr/sbin/runuser"));
        assert_eq!(verified[1].path(), Path::new("/usr/bin/git"));
'''
new = '''        assert_eq!(verified.len(), 3);
        assert_eq!(verified[0].path(), Path::new("/usr/sbin/runuser"));
        assert_eq!(verified[1].path(), Path::new("/usr/bin/env"));
        assert_eq!(verified[2].path(), Path::new("/usr/bin/git"));
'''
if executable.count(old) != 1:
    raise SystemExit("unexpected executable verification fixture")
executable = executable.replace(old, new, 1)
executable_path.write_text(executable, encoding="utf-8")
