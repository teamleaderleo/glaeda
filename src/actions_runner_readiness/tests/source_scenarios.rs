#[test]
fn offline_starting_busy_and_draining_are_distinct() {
    let offline_executor = ScriptedExecutor::default();
    let offline = adapter()
        .observe(
            &request(),
            &source(LimaRuntimeState::Stopped, false, 130),
            &offline_executor,
            &FakeClock::new([100]),
        )
        .expect("offline observation");
    assert_eq!(offline.report().state, ActionsRunnerReadinessState::Offline);
    assert!(offline.report().configured_identity.is_none());
    assert!(offline_executor.seen().is_empty());

    let boot_executor = ScriptedExecutor::default();
    let booting = adapter()
        .observe(
            &request(),
            &source(LimaRuntimeState::Installing, false, 130),
            &boot_executor,
            &FakeClock::new([100]),
        )
        .expect("starting VM observation");
    assert_eq!(
        booting.report().state,
        ActionsRunnerReadinessState::Starting
    );
    assert!(boot_executor.seen().is_empty());

    let starting = adapter()
        .observe(
            &request(),
            &source(LimaRuntimeState::Running, true, 130),
            &ScriptedExecutor::new(running_steps_without_processes(false)),
            &FakeClock::new([100, 101]),
        )
        .expect("starting listener observation");
    assert_eq!(
        starting.report().state,
        ActionsRunnerReadinessState::Starting
    );
    assert!(starting.report().configured_identity.is_some());

    let busy = adapter()
        .observe(
            &request(),
            &source(LimaRuntimeState::Running, true, 130),
            &ScriptedExecutor::new(running_steps(true, false)),
            &FakeClock::new([100, 101]),
        )
        .expect("busy observation");
    assert_eq!(busy.report().state, ActionsRunnerReadinessState::Busy);

    let draining = adapter()
        .observe(
            &request(),
            &source(LimaRuntimeState::Running, true, 130),
            &ScriptedExecutor::new(running_steps(true, true)),
            &FakeClock::new([100, 101]),
        )
        .expect("draining observation");
    assert_eq!(
        draining.report().state,
        ActionsRunnerReadinessState::Draining
    );
}

#[test]
fn stale_source_or_expired_during_probe_returns_stale_without_claiming_identity() {
    let stale_executor = ScriptedExecutor::default();
    let stale = adapter()
        .observe(
            &request(),
            &source(LimaRuntimeState::Running, true, 99),
            &stale_executor,
            &FakeClock::new([100]),
        )
        .expect("stale source");
    assert_eq!(stale.report().state, ActionsRunnerReadinessState::Stale);
    assert_eq!(
        stale.report().timing.freshness,
        LimaObservationFreshness::Stale
    );
    assert!(stale.report().configured_identity.is_none());
    assert!(stale_executor.seen().is_empty());

    let executor = ScriptedExecutor::new(running_steps(false, false));
    let expired = adapter()
        .observe(
            &request(),
            &source(LimaRuntimeState::Running, true, 130),
            &executor,
            &FakeClock::new([100, 131]),
        )
        .expect("expired while probing");
    assert_eq!(expired.report().state, ActionsRunnerReadinessState::Stale);
    assert!(expired.report().configured_identity.is_none());
    assert_eq!(expired.private_evidence().commands().len(), 26);
}

#[test]
fn source_mismatch_broken_source_and_missing_guest_fail_closed() {
    let mut wrong_instance = source(LimaRuntimeState::Stopped, false, 130);
    wrong_instance.instance = LimaInstanceName::parse("other").expect("other instance");
    let mismatch = adapter()
        .observe(
            &request(),
            &wrong_instance,
            &ScriptedExecutor::default(),
            &FakeClock::new([]),
        )
        .expect_err("source instance mismatch");
    assert_eq!(
        mismatch.code,
        ActionsRunnerReadinessRefusalCode::SourceInstanceMismatch
    );

    let broken = adapter()
        .observe(
            &request(),
            &source(LimaRuntimeState::Broken, false, 130),
            &ScriptedExecutor::default(),
            &FakeClock::new([100]),
        )
        .expect_err("broken source");
    assert_eq!(
        broken.code,
        ActionsRunnerReadinessRefusalCode::SourceUnavailable
    );

    let missing_guest = adapter()
        .observe(
            &request(),
            &source(LimaRuntimeState::Running, false, 130),
            &ScriptedExecutor::default(),
            &FakeClock::new([100]),
        )
        .expect_err("missing guest");
    assert_eq!(
        missing_guest.code,
        ActionsRunnerReadinessRefusalCode::SourceGuestMismatch
    );
}
