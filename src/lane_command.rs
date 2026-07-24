use std::fmt;
use std::path::{Component, Path};

use serde::Serialize;

use crate::journal::{ExecutionLane, PlannedMutation};
use crate::process::CommandSpec;

const APT_GET: &str = "/usr/bin/apt-get";
const GROUPADD: &str = "/usr/sbin/groupadd";
const USERADD: &str = "/usr/sbin/useradd";
const LOGINCTL: &str = "/usr/bin/loginctl";
const RUNUSER: &str = "/usr/sbin/runuser";
const ENV: &str = "/usr/bin/env";
const PODMAN: &str = "/usr/bin/podman";
const GIT: &str = "/usr/bin/git";
const NOLOGIN: &str = "/usr/sbin/nologin";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PackageName(String);

impl PackageName {
    /// Validate one Debian package name used by the reviewed apt command.
    ///
    /// # Errors
    ///
    /// Returns an error for empty, option-shaped, uppercase, unbounded, or unsafe names.
    pub fn parse(value: &str) -> Result<Self, LaneCommandError> {
        if value.is_empty()
            || value.len() > 100
            || value.starts_with('-')
            || !value.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'+' | b'.' | b'-')
            })
        {
            return Err(LaneCommandError::single(
                "package name must be 1 to 100 lowercase ASCII letters, digits, '+', '.', or '-', and must not begin with '-'",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct LinuxAccountName(String);

impl LinuxAccountName {
    /// Validate one lowercase Linux account or group name.
    ///
    /// # Errors
    ///
    /// Returns an error unless the value begins with a lowercase letter or underscore and contains
    /// only lowercase letters, digits, underscores, and hyphens.
    pub fn parse(value: &str) -> Result<Self, LaneCommandError> {
        let mut bytes = value.bytes();
        let first_valid = bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte == b'_');
        if !first_valid
            || value.len() > 32
            || !bytes.all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
            })
        {
            return Err(LaneCommandError::single(
                "Linux account name must be a safe lowercase username",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunnerUserContext {
    username: LinuxAccountName,
    uid: u32,
    primary_gid: u32,
    home: String,
    runtime_directory: String,
}

impl RunnerUserContext {
    /// Build the reviewed runner-user execution identity.
    ///
    /// The runtime directory is fixed to `/run/user/UID`; ownership and existence are verified by
    /// the later Linux lane executor.
    ///
    /// # Errors
    ///
    /// Returns an error for UID or primary GID zero, unsafe names, or noncanonical absolute home paths.
    pub fn new(
        username: LinuxAccountName,
        uid: u32,
        primary_gid: u32,
        home: &str,
    ) -> Result<Self, LaneCommandError> {
        if uid == 0 || primary_gid == 0 {
            return Err(LaneCommandError::single(
                "runner-user UID and primary GID must be greater than zero",
            ));
        }
        let home = canonical_absolute_path("runner-user home", home)?;
        Ok(Self {
            username,
            uid,
            primary_gid,
            home,
            runtime_directory: format!("/run/user/{uid}"),
        })
    }

    #[must_use]
    pub fn username(&self) -> &LinuxAccountName {
        &self.username
    }

    #[must_use]
    pub fn uid(&self) -> u32 {
        self.uid
    }

    #[must_use]
    pub fn primary_gid(&self) -> u32 {
        self.primary_gid
    }

    #[must_use]
    pub fn home(&self) -> &str {
        &self.home
    }

    #[must_use]
    pub fn runtime_directory(&self) -> &str {
        &self.runtime_directory
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneCommandKind {
    AptInstall,
    EnsureSystemGroup,
    EnsureSystemUser,
    EnableLinger,
    RunnerPodmanInfo,
    RunnerGitVersion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LaneCommand {
    action_id: String,
    lane: ExecutionLane,
    kind: LaneCommandKind,
    spec: CommandSpec,
}

impl LaneCommand {
    /// Build the reviewed noninteractive apt installation command.
    ///
    /// # Errors
    ///
    /// Returns an error when the action is not assigned to the root lane or no packages are given.
    pub fn apt_install(
        action: &PlannedMutation,
        packages: &[PackageName],
    ) -> Result<Self, LaneCommandError> {
        require_lane(action, ExecutionLane::Root)?;
        if packages.is_empty() {
            return Err(LaneCommandError::single(
                "apt installation requires at least one package",
            ));
        }
        let mut spec = CommandSpec::new(APT_GET)
            .argument("install")
            .argument("--yes")
            .argument("--no-install-recommends");
        for package in packages {
            spec = spec.argument(package.as_str());
        }
        Ok(Self::new(action, LaneCommandKind::AptInstall, spec))
    }

    /// Build the reviewed system-group creation command.
    ///
    /// # Errors
    ///
    /// Returns an error when the action is not assigned to the root lane.
    pub fn ensure_system_group(
        action: &PlannedMutation,
        group: &LinuxAccountName,
    ) -> Result<Self, LaneCommandError> {
        require_lane(action, ExecutionLane::Root)?;
        let spec = CommandSpec::new(GROUPADD)
            .argument("--system")
            .argument(group.as_str());
        Ok(Self::new(action, LaneCommandKind::EnsureSystemGroup, spec))
    }

    /// Build the reviewed system-user creation command.
    ///
    /// # Errors
    ///
    /// Returns an error when the action is not assigned to the root lane or the home path is not
    /// canonical and absolute.
    pub fn ensure_system_user(
        action: &PlannedMutation,
        user: &LinuxAccountName,
        primary_group: &LinuxAccountName,
        home: &str,
    ) -> Result<Self, LaneCommandError> {
        require_lane(action, ExecutionLane::Root)?;
        let home = canonical_absolute_path("system-user home", home)?;
        let spec = CommandSpec::new(USERADD)
            .argument("--system")
            .argument("--gid")
            .argument(primary_group.as_str())
            .argument("--home-dir")
            .argument(home)
            .argument("--shell")
            .argument(NOLOGIN)
            .argument("--no-create-home")
            .argument(user.as_str());
        Ok(Self::new(action, LaneCommandKind::EnsureSystemUser, spec))
    }

    /// Build the reviewed linger-enablement command.
    ///
    /// # Errors
    ///
    /// Returns an error when the action is not assigned to the root lane.
    pub fn enable_linger(
        action: &PlannedMutation,
        user: &LinuxAccountName,
    ) -> Result<Self, LaneCommandError> {
        require_lane(action, ExecutionLane::Root)?;
        let spec = CommandSpec::new(LOGINCTL)
            .argument("enable-linger")
            .argument(user.as_str());
        Ok(Self::new(action, LaneCommandKind::EnableLinger, spec))
    }

    /// Build a runner-user `podman info` command behind the reviewed `runuser` boundary.
    ///
    /// # Errors
    ///
    /// Returns an error when the action is not assigned to the runner-user lane.
    pub fn runner_podman_info(
        action: &PlannedMutation,
        runner: &RunnerUserContext,
    ) -> Result<Self, LaneCommandError> {
        require_lane(action, ExecutionLane::RunnerUser)?;
        let spec = runner_user_spec(runner, PODMAN, &["info", "--format", "json"]);
        Ok(Self::new(action, LaneCommandKind::RunnerPodmanInfo, spec))
    }

    /// Build a runner-user `git --version` command behind the reviewed `runuser` boundary.
    ///
    /// # Errors
    ///
    /// Returns an error when the action is not assigned to the runner-user lane.
    pub fn runner_git_version(
        action: &PlannedMutation,
        runner: &RunnerUserContext,
    ) -> Result<Self, LaneCommandError> {
        require_lane(action, ExecutionLane::RunnerUser)?;
        let spec = runner_user_spec(runner, GIT, &["--version"]);
        Ok(Self::new(action, LaneCommandKind::RunnerGitVersion, spec))
    }

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
    pub fn spec(&self) -> &CommandSpec {
        &self.spec
    }

    #[must_use]
    pub fn required_programs(&self) -> Vec<&Path> {
        let outer = self.spec.program.as_path();
        match self.kind {
            LaneCommandKind::RunnerPodmanInfo => {
                vec![outer, Path::new(ENV), Path::new(PODMAN)]
            }
            LaneCommandKind::RunnerGitVersion => {
                vec![outer, Path::new(ENV), Path::new(GIT)]
            }
            LaneCommandKind::AptInstall
            | LaneCommandKind::EnsureSystemGroup
            | LaneCommandKind::EnsureSystemUser
            | LaneCommandKind::EnableLinger => vec![outer],
        }
    }

    fn new(action: &PlannedMutation, kind: LaneCommandKind, spec: CommandSpec) -> Self {
        Self {
            action_id: action.id.clone(),
            lane: action.lane,
            kind,
            spec,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LaneCommandError {
    pub problems: Vec<String>,
}

impl LaneCommandError {
    fn single(problem: impl Into<String>) -> Self {
        Self {
            problems: vec![problem.into()],
        }
    }
}

impl fmt::Display for LaneCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "lane command validation failed")?;
        for problem in &self.problems {
            writeln!(formatter, "- {problem}")?;
        }
        Ok(())
    }
}

impl std::error::Error for LaneCommandError {}

fn runner_user_spec(
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

fn require_lane(action: &PlannedMutation, expected: ExecutionLane) -> Result<(), LaneCommandError> {
    if action.lane == expected {
        Ok(())
    } else {
        Err(LaneCommandError::single(format!(
            "action {:?} is assigned to {:?}, but this command requires {:?}",
            action.id, action.lane, expected
        )))
    }
}

fn canonical_absolute_path(field: &str, value: &str) -> Result<String, LaneCommandError> {
    if value.is_empty()
        || value.len() > 4_096
        || value.ends_with('/')
        || value.chars().any(char::is_control)
    {
        return Err(LaneCommandError::single(format!(
            "{field} must be a canonical absolute path"
        )));
    }
    let path = Path::new(value);
    if !path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(LaneCommandError::single(format!(
            "{field} must be a canonical absolute path without aliases"
        )));
    }
    Ok(value.to_owned())
}

#[cfg(test)]
mod tests {
    use crate::journal::{ExecutionLane, PlannedMutation, Preconditions, RollbackClass};
    use crate::process::CommandValue;

    use super::{
        APT_GET, ENV, GIT, GROUPADD, LOGINCTL, LaneCommand, LaneCommandKind, LinuxAccountName,
        NOLOGIN, PODMAN, PackageName, RUNUSER, RunnerUserContext, USERADD,
    };

    fn action(lane: ExecutionLane) -> PlannedMutation {
        PlannedMutation::new(
            "prepare-host",
            lane,
            "prepare host",
            RollbackClass::Compensating,
            Preconditions::new(["host inspected"]),
        )
    }

    fn account(value: &str) -> LinuxAccountName {
        LinuxAccountName::parse(value).expect("valid account name")
    }

    #[test]
    fn apt_command_uses_fixed_program_arguments_and_empty_environment() {
        let command = LaneCommand::apt_install(
            &action(ExecutionLane::Root),
            &[
                PackageName::parse("podman").expect("package"),
                PackageName::parse("git").expect("package"),
            ],
        )
        .expect("apt command");
        assert_eq!(command.kind(), LaneCommandKind::AptInstall);
        assert_eq!(command.spec().program.to_str(), Some(APT_GET));
        assert_eq!(
            command.spec().displayed_argv(),
            [
                APT_GET,
                "install",
                "--yes",
                "--no-install-recommends",
                "podman",
                "git",
            ]
        );
        assert!(command.spec().environment.is_empty());
    }

    #[test]
    fn root_account_commands_have_reviewed_argv() {
        let root = action(ExecutionLane::Root);
        let group = account("project-runner");
        let user = account("project-runner");
        assert_eq!(
            LaneCommand::ensure_system_group(&root, &group)
                .expect("group command")
                .spec()
                .displayed_argv(),
            [GROUPADD, "--system", "project-runner"]
        );
        assert_eq!(
            LaneCommand::ensure_system_user(&root, &user, &group, "/var/lib/project-runner",)
                .expect("user command")
                .spec()
                .displayed_argv(),
            [
                USERADD,
                "--system",
                "--gid",
                "project-runner",
                "--home-dir",
                "/var/lib/project-runner",
                "--shell",
                NOLOGIN,
                "--no-create-home",
                "project-runner",
            ]
        );
        assert_eq!(
            LaneCommand::enable_linger(&root, &user)
                .expect("linger command")
                .spec()
                .displayed_argv(),
            [LOGINCTL, "enable-linger", "project-runner"]
        );
    }

    #[test]
    fn runner_user_commands_have_exact_runuser_boundary_and_environment() {
        let runner = RunnerUserContext::new(account("project-runner"), 1001, 1001, "/srv/runner")
            .expect("runner context");
        let action = action(ExecutionLane::RunnerUser);
        let podman = LaneCommand::runner_podman_info(&action, &runner).expect("podman command");
        assert_eq!(
            podman.spec().displayed_argv(),
            [
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
        );
        let git = LaneCommand::runner_git_version(&action, &runner).expect("git command");
        assert_eq!(
            git.spec().displayed_argv(),
            [
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
        assert!(
            git.spec()
                .arguments
                .iter()
                .all(|value| matches!(value, CommandValue::Plain(_)))
        );
    }

    #[test]
    fn lane_mismatch_fails_before_a_command_is_constructed() {
        let error = LaneCommand::ensure_system_group(
            &action(ExecutionLane::RunnerUser),
            &account("project-runner"),
        )
        .expect_err("lane mismatch must fail");
        assert!(error.problems[0].contains("requires Root"));
    }

    #[test]
    fn untrusted_names_paths_and_root_runner_identity_are_rejected() {
        PackageName::parse("--option").expect_err("option-shaped package must fail");
        PackageName::parse("Podman").expect_err("uppercase package must fail");
        LinuxAccountName::parse("root/user").expect_err("unsafe account must fail");
        RunnerUserContext::new(account("project-runner"), 0, 1001, "/srv/runner")
            .expect_err("root runner user must fail");
        RunnerUserContext::new(account("project-runner"), 1001, 0, "/srv/runner")
            .expect_err("root primary group must fail");
        RunnerUserContext::new(account("project-runner"), 1001, 0, "/srv/runner")
            .expect_err("root primary group must fail");
        RunnerUserContext::new(account("project-runner"), 1001, 1001, "/srv/../root")
            .expect_err("aliased home must fail");
    }
}
