#[test]
fn idle_ready_observation_binds_exact_commands_identity_and_freshness() {
    let executor = ScriptedExecutor::new(running_steps(false, false));
    let observation = adapter()
        .observe(
            &request(),
            &source(LimaRuntimeState::Running, true, 130),
            &executor,
            &FakeClock::new([100, 105]),
        )
        .expect("idle-ready observation");
    let report = observation.report();

    assert_eq!(
        report.schema_version,
        ACTIONS_RUNNER_READINESS_SCHEMA_VERSION
    );
    assert_eq!(report.state, ActionsRunnerReadinessState::IdleReady);
    assert_eq!(report.instance.as_str(), "smolrunner");
    assert_eq!(report.runner_name.as_str(), "smolrunner-macbook");
    assert_eq!(report.timing.started_at_unix_seconds, 100);
    assert_eq!(report.timing.observed_at_unix_seconds, 105);
    assert_eq!(report.timing.expires_at_unix_seconds, 130);
    assert_eq!(report.timing.duration_seconds, 5);
    let identity = report
        .configured_identity
        .as_ref()
        .expect("configured identity");
    assert_eq!(identity.runner_name.as_str(), "smolrunner-macbook");
    assert_eq!(
        identity.configuration_digest.as_str(),
        format!("sha256:{CONFIG_HEX}")
    );
    assert_eq!(
        identity.runner_root,
        LimaFilesystemObjectIdentity {
            device_id: 2049,
            inode: 500,
        }
    );
    assert_eq!(executor.remaining(), 0);

    let commands = executor.seen();
    assert_eq!(commands.len(), 26);
    assert_eq!(
        commands[0].displayed_argv(),
        vec![
            "/opt/homebrew/bin/limactl",
            "--tty=false",
            "shell",
            "smolrunner",
            "--",
            "/usr/bin/stat",
            "-Lc",
            "%d:%i",
            "--",
            "[REDACTED]",
        ]
    );
    assert_eq!(
        commands[7].displayed_argv(),
        vec![
            "/opt/homebrew/bin/limactl",
            "--tty=false",
            "shell",
            "smolrunner",
            "--",
            "/usr/bin/pgrep",
            "-x",
            "Runner.Listener",
        ]
    );
    assert_eq!(
        commands[9].displayed_argv(),
        vec![
            "/opt/homebrew/bin/limactl",
            "--tty=false",
            "shell",
            "smolrunner",
            "--",
            "/usr/bin/readlink",
            "-e",
            "--",
            "/proc/42/exe",
        ]
    );
    for command in &commands {
        assert_eq!(
            command
                .environment
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["HOME", "LANG", "LC_ALL", "LIMA_HOME", "PATH"]
        );
        assert_eq!(
            command.environment.get("LIMA_HOME"),
            Some(&CommandValue::Plain(LIMA_HOME.to_owned()))
        );
        assert_eq!(
            command.environment.get("HOME"),
            Some(&CommandValue::Plain(LIMACTL_SAFE_HOME.to_owned()))
        );
        assert_eq!(
            command.environment.get("PATH"),
            Some(&CommandValue::Plain(LIMACTL_SAFE_PATH.to_owned()))
        );
        let argv = command.displayed_argv();
        assert!(!argv.iter().any(|argument| {
            matches!(
                argument.as_str(),
                "/bin/sh"
                    | "/bin/bash"
                    | "/usr/bin/env"
                    | "-c"
                    | "-lc"
                    | "config.sh"
                    | "svc.sh"
                    | "remove"
                    | "token"
            )
        }));
    }
}
