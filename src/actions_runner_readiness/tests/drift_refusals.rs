#[test]
fn configuration_process_identity_and_drain_drift_are_refused() {
    let mut config_steps = running_steps(false, false);
    let ScriptedStep::Output(config) = &mut config_steps[1] else {
        panic!("config output");
    };
    config.stdout = format!("{}  [REDACTED]\n", "c".repeat(64));
    let config_failure = adapter()
        .observe(
            &request(),
            &source(LimaRuntimeState::Running, true, 130),
            &ScriptedExecutor::new(config_steps),
            &FakeClock::new([100]),
        )
        .expect_err("configuration mismatch");
    assert_eq!(
        config_failure.code,
        ActionsRunnerReadinessRefusalCode::ConfigurationIdentityMismatch
    );

    let mut process_steps = running_steps(false, false);
    let ScriptedStep::Output(exe) = &mut process_steps[5] else {
        panic!("listener executable output");
    };
    exe.stdout = "/tmp/Runner.Listener\n".to_owned();
    let process_failure = adapter()
        .observe(
            &request(),
            &source(LimaRuntimeState::Running, true, 130),
            &ScriptedExecutor::new(process_steps),
            &FakeClock::new([100]),
        )
        .expect_err("process executable mismatch");
    assert_eq!(
        process_failure.code,
        ActionsRunnerReadinessRefusalCode::ProcessIdentityMismatch
    );

    let mut drift_steps = running_steps(false, false);
    let ScriptedStep::Output(final_listener) = &mut drift_steps[8] else {
        panic!("final listener query");
    };
    final_listener.stdout = "44\n".to_owned();
    let ScriptedStep::Output(final_exe) = &mut drift_steps[10] else {
        panic!("final listener executable");
    };
    final_exe.stdout = format!("{RUNNER_ROOT}/bin/Runner.Listener\n");
    let ScriptedStep::Output(final_cwd) = &mut drift_steps[11] else {
        panic!("final listener cwd");
    };
    final_cwd.stdout = format!("{RUNNER_ROOT}\n");
    let ScriptedStep::Output(final_proc) = &mut drift_steps[12] else {
        panic!("final listener proc");
    };
    final_proc.stdout = "900:4400\n".to_owned();
    let drift_failure = adapter()
        .observe(
            &request(),
            &source(LimaRuntimeState::Running, true, 130),
            &ScriptedExecutor::new(drift_steps),
            &FakeClock::new([100]),
        )
        .expect_err("process drift");
    assert_eq!(
        drift_failure.code,
        ActionsRunnerReadinessRefusalCode::ProcessDrift
    );

    let mut drain_steps = running_steps(false, false);
    drain_steps[15] = ScriptedStep::Output(ScriptedOutput::success(""));
    let drain_failure = adapter()
        .observe(
            &request(),
            &source(LimaRuntimeState::Running, true, 130),
            &ScriptedExecutor::new(drain_steps),
            &FakeClock::new([100]),
        )
        .expect_err("drain drift");
    assert_eq!(
        drain_failure.code,
        ActionsRunnerReadinessRefusalCode::DrainStateDrift
    );
}

#[test]
fn ambiguity_worker_without_listener_spawn_record_and_output_bounds_fail_closed() {
    let mut ambiguous_steps = running_steps(false, false);
    let ScriptedStep::Output(listener) = &mut ambiguous_steps[3] else {
        panic!("listener query");
    };
    listener.stdout = "42\n44\n".to_owned();
    let ambiguity = adapter()
        .observe(
            &request(),
            &source(LimaRuntimeState::Running, true, 130),
            &ScriptedExecutor::new(ambiguous_steps),
            &FakeClock::new([100]),
        )
        .expect_err("ambiguous listener");
    assert_eq!(
        ambiguity.code,
        ActionsRunnerReadinessRefusalCode::AmbiguousListener
    );

    let inconsistent = adapter()
        .observe(
            &request(),
            &source(LimaRuntimeState::Running, true, 130),
            &ScriptedExecutor::new(worker_without_listener_steps()),
            &FakeClock::new([100, 101]),
        )
        .expect_err("worker without listener");
    assert_eq!(
        inconsistent.code,
        ActionsRunnerReadinessRefusalCode::ProcessStateInconsistent
    );

    let spawn = adapter()
        .observe(
            &request(),
            &source(LimaRuntimeState::Running, true, 130),
            &ScriptedExecutor::new([ScriptedStep::IoError(io::ErrorKind::NotFound)]),
            &FakeClock::new([100]),
        )
        .expect_err("spawn failure");
    assert_eq!(spawn.code, ActionsRunnerReadinessRefusalCode::CommandFailed);
    assert!(spawn.private_evidence().commands().is_empty());

    let mut wrong_record = ScriptedOutput::success("2049:500\n");
    wrong_record.argv_override = Some(vec!["/bin/sh".to_owned()]);
    let record_failure = adapter()
        .observe(
            &request(),
            &source(LimaRuntimeState::Running, true, 130),
            &ScriptedExecutor::new([ScriptedStep::Output(wrong_record)]),
            &FakeClock::new([100]),
        )
        .expect_err("record identity");
    assert_eq!(
        record_failure.code,
        ActionsRunnerReadinessRefusalCode::CommandIdentityMismatch
    );

    let oversized = adapter()
        .observe(
            &request(),
            &source(LimaRuntimeState::Running, true, 130),
            &ScriptedExecutor::new([ScriptedStep::Output(ScriptedOutput::success(
                "x".repeat(MAX_ACTIONS_RUNNER_OUTPUT_BYTES + 1),
            ))]),
            &FakeClock::new([100]),
        )
        .expect_err("output bound");
    assert_eq!(
        oversized.code,
        ActionsRunnerReadinessRefusalCode::UnboundedOutput
    );
}
