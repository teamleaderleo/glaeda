from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    if text.count(old) != 1:
        raise SystemExit(f"expected {label} exactly once, found {text.count(old)}")
    return text.replace(old, new, 1)


def patch_lane_command() -> None:
    path = Path("src/lane_command.rs")
    text = path.read_text()

    if 'const USERMOD: &str' not in text:
        text = replace_once(
            text,
            'const USERADD: &str = "/usr/sbin/useradd";\n',
            'const USERADD: &str = "/usr/sbin/useradd";\n'
            'const USERMOD: &str = "/usr/sbin/usermod";\n'
            'const INSTALL: &str = "/usr/bin/install";\n',
            "lane command USERADD constant",
        )

    if '    EnsureHomeDirectory,\n' not in text:
        text = replace_once(
            text,
            '    EnsureSystemUser,\n    EnableLinger,\n',
            '    EnsureSystemUser,\n'
            '    EnsureHomeDirectory,\n'
            '    EnsureSubordinateUids,\n'
            '    EnsureSubordinateGids,\n'
            '    EnableLinger,\n',
            "lane command enum variants",
        )

    if 'pub fn ensure_home_directory' not in text:
        marker = '    /// Build the reviewed linger-enablement command.\n'
        methods = r'''    /// Build the reviewed runner home-directory creation command.
    ///
    /// # Errors
    ///
    /// Returns an error when the action is not assigned to the root lane or the home path is not
    /// canonical and absolute.
    pub fn ensure_home_directory(
        action: &PlannedMutation,
        user: &LinuxAccountName,
        primary_group: &LinuxAccountName,
        home: &str,
    ) -> Result<Self, LaneCommandError> {
        require_lane(action, ExecutionLane::Root)?;
        let home = canonical_absolute_path("runner home", home)?;
        let spec = CommandSpec::new(INSTALL)
            .argument("--directory")
            .argument("--mode")
            .argument("0750")
            .argument("--owner")
            .argument(user.as_str())
            .argument("--group")
            .argument(primary_group.as_str())
            .argument("--")
            .argument(home);
        Ok(Self::new(
            action,
            LaneCommandKind::EnsureHomeDirectory,
            spec,
        ))
    }

    /// Build the reviewed subordinate-UID assignment command.
    ///
    /// # Errors
    ///
    /// Returns an error for a lane mismatch or an empty, overflowing subordinate range.
    pub fn ensure_subordinate_uids(
        action: &PlannedMutation,
        user: &LinuxAccountName,
        start: u32,
        count: u32,
    ) -> Result<Self, LaneCommandError> {
        require_lane(action, ExecutionLane::Root)?;
        let range = subordinate_range_argument(start, count)?;
        let spec = CommandSpec::new(USERMOD)
            .argument("--add-subuids")
            .argument(range)
            .argument("--")
            .argument(user.as_str());
        Ok(Self::new(
            action,
            LaneCommandKind::EnsureSubordinateUids,
            spec,
        ))
    }

    /// Build the reviewed subordinate-GID assignment command.
    ///
    /// # Errors
    ///
    /// Returns an error for a lane mismatch or an empty, overflowing subordinate range.
    pub fn ensure_subordinate_gids(
        action: &PlannedMutation,
        user: &LinuxAccountName,
        start: u32,
        count: u32,
    ) -> Result<Self, LaneCommandError> {
        require_lane(action, ExecutionLane::Root)?;
        let range = subordinate_range_argument(start, count)?;
        let spec = CommandSpec::new(USERMOD)
            .argument("--add-subgids")
            .argument(range)
            .argument("--")
            .argument(user.as_str());
        Ok(Self::new(
            action,
            LaneCommandKind::EnsureSubordinateGids,
            spec,
        ))
    }

'''
        text = replace_once(text, marker, methods + marker, "linger method marker")

    required_old = '''            LaneCommandKind::EnsureSystemUser => vec![outer, Path::new(NOLOGIN)],
            LaneCommandKind::AptInstall
            | LaneCommandKind::EnsureSystemGroup
            | LaneCommandKind::EnableLinger => vec![outer],
'''
    required_new = '''            LaneCommandKind::EnsureSystemUser => vec![outer, Path::new(NOLOGIN)],
            LaneCommandKind::AptInstall
            | LaneCommandKind::EnsureSystemGroup
            | LaneCommandKind::EnsureHomeDirectory
            | LaneCommandKind::EnsureSubordinateUids
            | LaneCommandKind::EnsureSubordinateGids
            | LaneCommandKind::EnableLinger => vec![outer],
'''
    required_section = text[text.index("pub fn required_programs"):]
    if 'LaneCommandKind::EnsureHomeDirectory' not in required_section:
        text = replace_once(text, required_old, required_new, "required programs match")

    if 'fn subordinate_range_argument' not in text:
        marker = 'fn runner_user_spec(\n'
        helper = r'''fn subordinate_range_argument(start: u32, count: u32) -> Result<String, LaneCommandError> {
    if start == 0 || count == 0 {
        return Err(LaneCommandError::single(
            "subordinate-ID range must begin above zero and contain at least one ID",
        ));
    }
    let end = u64::from(start) + u64::from(count) - 1;
    let end = u32::try_from(end).map_err(|_| {
        LaneCommandError::single("subordinate-ID range exceeds the 32-bit ID space")
    })?;
    Ok(format!("{start}-{end}"))
}

'''
        text = replace_once(text, marker, helper + marker, "runner_user_spec marker")

    imports_old = '''        APT_GET, ENV, GIT, GROUPADD, LOGINCTL, LaneCommand, LaneCommandKind, LinuxAccountName,
        NOLOGIN, PODMAN, PackageName, RUNUSER, RunnerUserContext, USERADD,
'''
    imports_new = '''        APT_GET, ENV, GIT, GROUPADD, INSTALL, LOGINCTL, LaneCommand, LaneCommandKind,
        LinuxAccountName, NOLOGIN, PODMAN, PackageName, RUNUSER, RunnerUserContext, USERADD,
        USERMOD,
'''
    if 'GROUPADD, INSTALL, LOGINCTL' not in text:
        text = replace_once(text, imports_old, imports_new, "lane command test imports")

    if 'expect("home command")' not in text:
        marker = '''        assert_eq!(
            LaneCommand::enable_linger(&root, &user)
'''
        assertions = r'''        assert_eq!(
            LaneCommand::ensure_home_directory(
                &root,
                &user,
                &group,
                "/var/lib/project-runner",
            )
            .expect("home command")
            .spec()
            .displayed_argv(),
            [
                INSTALL,
                "--directory",
                "--mode",
                "0750",
                "--owner",
                "project-runner",
                "--group",
                "project-runner",
                "--",
                "/var/lib/project-runner",
            ]
        );
        assert_eq!(
            LaneCommand::ensure_subordinate_uids(&root, &user, 100_000, 65_536)
                .expect("subuid command")
                .spec()
                .displayed_argv(),
            [
                USERMOD,
                "--add-subuids",
                "100000-165535",
                "--",
                "project-runner",
            ]
        );
        assert_eq!(
            LaneCommand::ensure_subordinate_gids(&root, &user, 200_000, 65_536)
                .expect("subgid command")
                .spec()
                .displayed_argv(),
            [
                USERMOD,
                "--add-subgids",
                "200000-265535",
                "--",
                "project-runner",
            ]
        );
'''
        text = replace_once(text, marker, assertions + marker, "linger command test marker")

    if 'overflowing subordinate range must fail' not in text:
        marker = '''        RunnerUserContext::new(account("project-runner"), 1001, 1001, "/srv/../root")
            .expect_err("aliased home must fail");
'''
        extra = r'''        LaneCommand::ensure_home_directory(
            &action(ExecutionLane::Root),
            &account("project-runner"),
            &account("project-runner"),
            "/var/lib/../root",
        )
        .expect_err("aliased home command must fail");
        LaneCommand::ensure_subordinate_uids(
            &action(ExecutionLane::Root),
            &account("project-runner"),
            u32::MAX,
            2,
        )
        .expect_err("overflowing subordinate range must fail");
'''
        text = replace_once(text, marker, marker + extra, "unsafe command test marker")

    path.write_text(text)


def patch_lane_executor() -> None:
    path = Path("src/lane_executor.rs")
    text = path.read_text()

    if 'const USERMOD: &str' not in text:
        text = replace_once(
            text,
            'const USERADD: &str = "/usr/sbin/useradd";\n',
            'const USERADD: &str = "/usr/sbin/useradd";\n'
            'const USERMOD: &str = "/usr/sbin/usermod";\n'
            'const INSTALL: &str = "/usr/bin/install";\n',
            "executor USERADD constant",
        )

    root_old = '''        LaneCommandKind::EnsureSystemUser => validate_useradd(spec, &arguments),
        LaneCommandKind::EnableLinger => validate_loginctl(spec, &arguments),
'''
    root_new = '''        LaneCommandKind::EnsureSystemUser => validate_useradd(spec, &arguments),
        LaneCommandKind::EnsureHomeDirectory => validate_install(spec, &arguments),
        LaneCommandKind::EnsureSubordinateUids => {
            validate_usermod(spec, &arguments, "--add-subuids")
        }
        LaneCommandKind::EnsureSubordinateGids => {
            validate_usermod(spec, &arguments, "--add-subgids")
        }
        LaneCommandKind::EnableLinger => validate_loginctl(spec, &arguments),
'''
    if 'LaneCommandKind::EnsureHomeDirectory => validate_install' not in text:
        text = replace_once(text, root_old, root_new, "root validator match")

    runner_old = '''        | LaneCommandKind::EnsureSystemUser
        | LaneCommandKind::EnableLinger => {
'''
    runner_new = '''        | LaneCommandKind::EnsureSystemUser
        | LaneCommandKind::EnsureHomeDirectory
        | LaneCommandKind::EnsureSubordinateUids
        | LaneCommandKind::EnsureSubordinateGids
        | LaneCommandKind::EnableLinger => {
'''
    runner_section = text[text.index("fn validate_runner_command"):]
    if '| LaneCommandKind::EnsureHomeDirectory' not in runner_section:
        text = replace_once(text, runner_old, runner_new, "runner validator root kinds")

    if 'fn validate_install' not in text:
        marker = 'fn validate_loginctl(spec: &CommandSpec, arguments: &[&str]) -> Result<(), LaneExecutionError> {\n'
        validators = r'''fn validate_install(spec: &CommandSpec, arguments: &[&str]) -> Result<(), LaneExecutionError> {
    let valid = spec.program == Path::new(INSTALL)
        && arguments.len() == 9
        && arguments[0] == "--directory"
        && arguments[1] == "--mode"
        && arguments[2] == "0750"
        && arguments[3] == "--owner"
        && LinuxAccountName::parse(arguments[4]).is_ok()
        && arguments[5] == "--group"
        && LinuxAccountName::parse(arguments[6]).is_ok()
        && arguments[7] == "--"
        && canonical_absolute_path(arguments[8]);
    if valid {
        Ok(())
    } else {
        Err(invalid_command(
            "runner home-directory command shape is not reviewed",
        ))
    }
}

fn validate_usermod(
    spec: &CommandSpec,
    arguments: &[&str],
    expected_option: &str,
) -> Result<(), LaneExecutionError> {
    let valid = spec.program == Path::new(USERMOD)
        && arguments.len() == 4
        && arguments[0] == expected_option
        && valid_subordinate_range(arguments[1])
        && arguments[2] == "--"
        && LinuxAccountName::parse(arguments[3]).is_ok();
    if valid {
        Ok(())
    } else {
        Err(invalid_command(
            "subordinate-ID command shape is not reviewed",
        ))
    }
}

fn valid_subordinate_range(value: &str) -> bool {
    let Some((start, end)) = value.split_once('-') else {
        return false;
    };
    let Some(start) = canonical_u32(start) else {
        return false;
    };
    let Some(end) = canonical_u32(end) else {
        return false;
    };
    start > 0 && start <= end
}

fn canonical_u32(value: &str) -> Option<u32> {
    let parsed = value.parse::<u32>().ok()?;
    (parsed.to_string() == value).then_some(parsed)
}

'''
        text = replace_once(text, marker, validators + marker, "loginctl validator marker")

    imports_old = '''        ReviewedExecutableVerifier, RunnerEvidence, execute_root_lane, execute_runner_user_lane,
        parse_effective_uid,
'''
    imports_new = '''        ReviewedExecutableVerifier, RunnerEvidence, execute_root_lane, execute_runner_user_lane,
        parse_effective_uid, validate_root_command,
'''
    if 'parse_effective_uid, validate_root_command' not in text:
        text = replace_once(text, imports_old, imports_new, "executor test imports")

    if 'root_validator_accepts_reviewed_account_preparation_commands' not in text:
        marker = '''    #[test]
    fn root_executor_delivers_exact_empty_environment_and_retains_record() {
'''
        test = r'''    #[test]
    fn root_validator_accepts_reviewed_account_preparation_commands() {
        let root = action("prepare-account", ExecutionLane::Root);
        let account = account("project-runner");
        for command in [
            LaneCommand::ensure_home_directory(
                &root,
                &account,
                &account,
                "/var/lib/project-runner",
            )
            .expect("home command"),
            LaneCommand::ensure_subordinate_uids(&root, &account, 100_000, 65_536)
                .expect("subuid command"),
            LaneCommand::ensure_subordinate_gids(&root, &account, 200_000, 65_536)
                .expect("subgid command"),
        ] {
            validate_root_command(&command).expect("reviewed root command");
        }
    }

'''
        text = replace_once(text, marker, test + marker, "root executor test marker")

    path.write_text(text)


def harden_plan() -> None:
    path = Path("src/runner_account_plan.rs")
    text = path.read_text()
    text = replace_once(
        text,
        '        || value.len() > 4_096\n',
        '        || value.len() > 4_000\n',
        "runner home public-text cap",
    )
    path.write_text(text)


patch_lane_command()
patch_lane_executor()
harden_plan()
