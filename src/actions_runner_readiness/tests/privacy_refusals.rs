#[test]
fn public_json_and_debug_suppress_private_paths_processes_and_raw_output() {
    let private_marker = "PRIVATE_RUNNER_RAW_OUTPUT";
    let mut steps = running_steps(false, false);
    let ScriptedStep::Output(proc_identity) = &mut steps[7] else {
        panic!("process identity");
    };
    proc_identity.stderr = private_marker.to_owned();
    let failure = adapter()
        .observe(
            &request(),
            &source(LimaRuntimeState::Running, true, 130),
            &ScriptedExecutor::new(steps),
            &FakeClock::new([100]),
        )
        .expect_err("private failure evidence");
    let failure_json = serde_json::to_string(&failure).expect("failure json");
    let failure_debug = format!("{failure:?}");
    for private in [LIMA_HOME, RUNNER_ROOT, DRAIN_MARKER, private_marker, "42"] {
        assert!(!failure_json.contains(private));
        assert!(!failure_debug.contains(private));
    }
    assert!(failure_debug.contains(REDACTED));

    let observation = adapter()
        .observe(
            &request(),
            &source(LimaRuntimeState::Running, true, 130),
            &ScriptedExecutor::new(running_steps(false, false)),
            &FakeClock::new([100, 101]),
        )
        .expect("public observation");
    let json = serde_json::to_string(&observation).expect("observation json");
    let debug = format!("{observation:?}");
    for private in [
        LIMA_HOME,
        RUNNER_ROOT,
        DRAIN_MARKER,
        "Runner.Listener",
        "42",
    ] {
        assert!(!json.contains(private));
        assert!(!debug.contains(private));
    }
    assert!(debug.contains(REDACTED));
    assert!(
        observation.private_evidence().commands()[5]
            .record()
            .stdout
            .contains(RUNNER_ROOT)
    );
}

#[test]
fn request_rejects_aliased_relative_external_and_non_utf8_paths() {
    let instance = LimaInstanceName::parse("smolrunner").expect("instance");
    let name = ActionsRunnerName::parse("smolrunner-macbook").expect("name");
    let expected_digest = digest();
    let aliased = ActionsRunnerReadinessRequest::new(
        instance.clone(),
        name.clone(),
        "/Users/operator/.lima/../.lima",
        RUNNER_ROOT,
        DRAIN_MARKER,
        expected_digest.clone(),
    )
    .expect_err("aliased home");
    assert_eq!(
        aliased.code,
        ActionsRunnerReadinessRefusalCode::InvalidInput
    );

    let relative = ActionsRunnerReadinessRequest::new(
        instance.clone(),
        name.clone(),
        LIMA_HOME,
        "relative/runner",
        DRAIN_MARKER,
        expected_digest.clone(),
    )
    .expect_err("relative root");
    assert_eq!(
        relative.code,
        ActionsRunnerReadinessRefusalCode::InvalidInput
    );

    let external_marker = ActionsRunnerReadinessRequest::new(
        instance,
        name,
        LIMA_HOME,
        RUNNER_ROOT,
        "/tmp/draining",
        expected_digest,
    )
    .expect_err("external marker");
    assert_eq!(
        external_marker.code,
        ActionsRunnerReadinessRefusalCode::InvalidInput
    );

    #[cfg(unix)]
    {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let non_utf8 = PathBuf::from(OsString::from_vec(vec![b'/', b't', b'm', b'p', b'/', 0xff]));
        let failure = ActionsRunnerReadinessRequest::new(
            LimaInstanceName::parse("smolrunner").expect("instance"),
            ActionsRunnerName::parse("runner").expect("name"),
            LIMA_HOME,
            non_utf8,
            DRAIN_MARKER,
            digest(),
        )
        .expect_err("non-UTF-8 root");
        assert_eq!(
            failure.code,
            ActionsRunnerReadinessRefusalCode::InvalidInput
        );
    }
}
