use super::*;
use crate::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use crate::execution_admission::{EpochMillis, ExecutionResourceLimits};
use crate::lima_lifecycle::LimaResourceProfile;
use crate::rust_verification_envelope::{
    CargoTargetDirectoryIdentity, RustBuildScriptInclusion, RustCacheContract,
    RustCacheIdentityClass, RustCargoProfileKind, RustCompilationContract, RustConcurrencyPlan,
    RustFeatureSelection, RustRetryPolicy, RustRetryPolicyId, RustRuntimeConcurrency,
    RustTargetDirectoryId, RustTargetTriple, RustToolchainId, RustToolchainIdentity,
    RustVerificationEnvelopeDefinition, RustVerificationScope,
};
use crate::verification_profile::{
    CacheId, CapabilityId, PackageId, RepositoryCommandId, RepositoryCommandIdentity,
    VerificationProfileId,
};

const GIB: u64 = 1024 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;

fn epoch(value: u64) -> EpochMillis {
    EpochMillis::new(value).expect("epoch")
}

fn digest(character: char) -> Sha256Digest {
    Sha256Digest::parse(&format!("sha256:{}", character.to_string().repeat(64))).expect("digest")
}

fn repository() -> RepositoryRef {
    RepositoryRef::parse("openai/codex").expect("repository")
}

fn source() -> RustVerificationSourceIdentity {
    RustVerificationSourceIdentity::new(
        repository(),
        CommitId::parse(&"a".repeat(40)).expect("commit"),
        GitTreeId::parse(&"b".repeat(40)).expect("tree"),
    )
}

fn command() -> RepositoryCommandIdentity {
    RepositoryCommandIdentity::new(
        repository(),
        RepositoryCommandId::parse("codex-core-lib").expect("command"),
        digest('c'),
    )
}

fn envelope(maximum_execution_millis: u64) -> RustVerificationEnvelope {
    let concurrency = RustConcurrencyPlan::new(
        2,
        RustRuntimeConcurrency::Libtest {
            test_threads: 1,
            filter: None,
        },
        vec![],
    )
    .expect("concurrency");
    let resources = crate::rust_verification_envelope::RustResourceEnvelope::new(
        LimaResourceProfile::Work,
        ExecutionResourceLimits::new(4_000, 6 * GIB, 2_048).expect("limits"),
        concurrency,
        5 * GIB,
        GIB,
        maximum_execution_millis,
    )
    .expect("resources");
    let toolchain = RustToolchainIdentity::new(
        RustToolchainId::parse("rust-1.88.0").expect("toolchain"),
        digest('d'),
        RustTargetTriple::parse("aarch64-apple-darwin").expect("host"),
        RustTargetTriple::parse("aarch64-unknown-linux-gnu").expect("target"),
    );
    let compilation = RustCompilationContract::new(
        toolchain,
        RustCargoProfileKind::Test,
        RustFeatureSelection::NoDefault,
        RustBuildScriptInclusion::Included,
    );
    let cache = RustCacheContract::new(
        RustCacheIdentityClass::RepositoryScoped,
        CargoTargetDirectoryIdentity::new(
            RustTargetDirectoryId::parse("codex-target").expect("target directory"),
            CacheId::parse("codex-cache").expect("cache"),
            digest('e'),
        ),
    );
    RustVerificationEnvelope::new(RustVerificationEnvelopeDefinition {
        profile_id: VerificationProfileId::parse("codex-core-focused").expect("profile"),
        source: source(),
        command: command(),
        scope: RustVerificationScope::LibraryTests {
            package: PackageId::parse("codex-core").expect("package"),
        },
        compilation,
        resources,
        cache,
        required_capabilities: vec![CapabilityId::parse("cargo").expect("capability")],
        retry: RustRetryPolicy::no_retry(
            RustRetryPolicyId::parse("no-retry").expect("retry policy"),
        ),
    })
    .expect("envelope")
}

fn process_group(generation: u64) -> RustProcessGroupIdentity {
    RustProcessGroupIdentity::new(
        RustProcessGroupId::parse("rust-attempt-group").expect("group"),
        RustProcessGroupGeneration::new(generation).expect("generation"),
    )
}

fn envelope_binding(envelope: &RustVerificationEnvelope) -> RustMemoryDiagnosticEnvelopeBinding {
    RustMemoryDiagnosticEnvelopeBinding::from_envelope(
        envelope,
        digest('f'),
        RustVerificationAttemptId::parse("attempt-1").expect("attempt"),
        process_group(1),
    )
}

fn counters(oom: u64, oom_kill: u64) -> RustMemoryEventCounters {
    RustMemoryEventCounters::new(0, 0, 0, oom, oom_kill).expect("counters")
}

fn cgroup_observation(
    binding: RustObservationBinding,
    observed_at: u64,
    current: u64,
    peak: u64,
    events: RustMemoryEventCounters,
) -> RustCgroupMemoryObservation {
    RustCgroupMemoryObservation::new(binding, epoch(observed_at), 6 * GIB, current, peak, events)
        .expect("cgroup observation")
}

fn executed_input(
    envelope: &RustVerificationEnvelope,
    phase: RustExecutionPhase,
    termination: RustProcessTermination,
    after_events: RustMemoryEventCounters,
) -> RustMemoryDiagnosticInput {
    let envelope_binding = envelope_binding(envelope);
    let observation_binding = envelope_binding.observation_binding();
    RustMemoryDiagnosticInput::new(
        envelope_binding,
        RustDiagnosticTiming::new(epoch(2_200), Some(epoch(1_200)), 500, 500).expect("timing"),
        RustPreflightMemoryObservation::new(
            observation_binding.clone(),
            epoch(1_000),
            5 * GIB,
            GIB,
        )
        .expect("preflight"),
        RustTerminalObservation::new(
            observation_binding.clone(),
            epoch(2_000),
            phase,
            termination,
        ),
        RustCgroupMemoryEvidence::Complete {
            before: cgroup_observation(
                observation_binding.clone(),
                1_100,
                512 * MIB,
                512 * MIB,
                counters(0, 0),
            ),
            after: cgroup_observation(observation_binding, 2_100, 256 * MIB, 5 * GIB, after_events),
        },
    )
}

fn refused_input(envelope: &RustVerificationEnvelope) -> RustMemoryDiagnosticInput {
    let envelope_binding = envelope_binding(envelope);
    let observation_binding = envelope_binding.observation_binding();
    RustMemoryDiagnosticInput::new(
        envelope_binding,
        RustDiagnosticTiming::new(epoch(1_200), None, 500, 500).expect("timing"),
        RustPreflightMemoryObservation::new(
            observation_binding.clone(),
            epoch(1_000),
            4 * GIB,
            GIB,
        )
        .expect("preflight"),
        RustTerminalObservation::new(
            observation_binding,
            epoch(1_200),
            RustExecutionPhase::NotStarted,
            RustProcessTermination::NotStarted,
        ),
        RustCgroupMemoryEvidence::NotCreated,
    )
}

#[test]
fn envelope_binding_derives_identity_and_authority_atomically() {
    let envelope = envelope(12_345);
    let binding = envelope_binding(&envelope);

    assert_eq!(
        binding.identity().verification_profile_id(),
        envelope.profile_id()
    );
    assert_eq!(binding.identity().source(), envelope.source());
    assert_eq!(binding.identity().command(), envelope.command());
    assert_eq!(binding.identity().envelope_digest(), &digest('f'));
    assert_eq!(binding.authority().maximum_execution_millis(), 12_345);
    assert_eq!(binding.authority().reserved_memory_bytes(), 6 * GIB);
}

#[test]
fn sufficient_headroom_and_zero_exit_succeeds() {
    let envelope = envelope(10_000);
    let report = classify_rust_memory(executed_input(
        &envelope,
        RustExecutionPhase::Completed,
        RustProcessTermination::Exited { code: 0 },
        counters(0, 0),
    ))
    .expect("classification");

    assert_eq!(
        report.classification,
        RustMemoryDiagnosticClassification::Succeeded
    );
    assert_eq!(report.authority.cargo_build_jobs(), 2);
    assert_eq!(report.authority.runtime_test_threads(), Some(1));
    assert_eq!(report.authority.reserved_memory_bytes(), 6 * GIB);
}

#[test]
fn insufficient_actual_headroom_refuses_before_execution() {
    let envelope = envelope(10_000);
    let report = classify_rust_memory(refused_input(&envelope)).expect("classification");

    assert_eq!(
        report.classification,
        RustMemoryDiagnosticClassification::MemoryPressureRefused
    );
    assert!(matches!(report.cgroup, RustCgroupMemorySummary::NotCreated));
}

#[test]
fn signal_nine_without_oom_kill_evidence_is_inconclusive() {
    let envelope = envelope(10_000);
    let report = classify_rust_memory(executed_input(
        &envelope,
        RustExecutionPhase::Link,
        RustProcessTermination::Signaled {
            signal: RustSignal::new(9).expect("signal"),
        },
        counters(0, 0),
    ))
    .expect("classification");

    assert_eq!(
        report.classification,
        RustMemoryDiagnosticClassification::Inconclusive
    );
}

#[test]
fn matching_oom_kill_delta_and_signal_nine_proves_memory_exhaustion() {
    let envelope = envelope(10_000);
    let report = classify_rust_memory(executed_input(
        &envelope,
        RustExecutionPhase::Link,
        RustProcessTermination::Signaled {
            signal: RustSignal::new(9).expect("signal"),
        },
        counters(1, 1),
    ))
    .expect("classification");

    assert_eq!(
        report.classification,
        RustMemoryDiagnosticClassification::MemoryExhausted
    );
    let RustCgroupMemorySummary::Complete { events, .. } = report.cgroup else {
        panic!("complete evidence")
    };
    assert_eq!(events.oom_kill, 1);
}

#[test]
fn observation_from_another_attempt_or_generation_is_refused() {
    let envelope = envelope(10_000);
    let mut input = executed_input(
        &envelope,
        RustExecutionPhase::Completed,
        RustProcessTermination::Exited { code: 0 },
        counters(0, 0),
    );
    let RustCgroupMemoryEvidence::Complete { after, .. } = &mut input.cgroup else {
        panic!("complete evidence")
    };
    after.binding.process_group = process_group(2);

    let error = classify_rust_memory(input).expect_err("identity drift");
    assert_eq!(error.code, "observation_identity_drift");
}

#[test]
fn typed_compile_link_and_test_failures_remain_distinct() {
    let envelope = envelope(10_000);
    for (phase, expected) in [
        (
            RustExecutionPhase::Compile,
            RustMemoryDiagnosticClassification::CompileFailed,
        ),
        (
            RustExecutionPhase::Link,
            RustMemoryDiagnosticClassification::LinkFailed,
        ),
        (
            RustExecutionPhase::Test,
            RustMemoryDiagnosticClassification::TestFailed,
        ),
    ] {
        let report = classify_rust_memory(executed_input(
            &envelope,
            phase,
            RustProcessTermination::Exited { code: 1 },
            counters(0, 0),
        ))
        .expect("classification");
        assert_eq!(report.classification, expected);
    }
}

#[test]
fn timeout_and_runner_loss_do_not_become_oom_without_exact_evidence() {
    let timeout_envelope = envelope(500);
    let mut timeout = executed_input(
        &timeout_envelope,
        RustExecutionPhase::Test,
        RustProcessTermination::Timeout,
        counters(0, 0),
    );
    timeout.timing =
        RustDiagnosticTiming::new(epoch(2_200), Some(epoch(1_200)), 500, 500).expect("timing");
    let timeout_report = classify_rust_memory(timeout).expect("timeout");
    assert_eq!(
        timeout_report.classification,
        RustMemoryDiagnosticClassification::Timeout
    );

    let envelope = envelope(10_000);
    let mut lost = executed_input(
        &envelope,
        RustExecutionPhase::Compile,
        RustProcessTermination::RunnerLost,
        counters(0, 0),
    );
    let RustCgroupMemoryEvidence::Complete { before, .. } = lost.cgroup else {
        panic!("complete evidence")
    };
    lost.cgroup = RustCgroupMemoryEvidence::UnavailableAfterRunnerLoss { before };
    let lost_report = classify_rust_memory(lost).expect("runner loss");
    assert_eq!(
        lost_report.classification,
        RustMemoryDiagnosticClassification::RunnerLost
    );
}

#[test]
fn stale_and_future_observations_fail_closed() {
    let envelope = envelope(10_000);
    let mut stale = executed_input(
        &envelope,
        RustExecutionPhase::Completed,
        RustProcessTermination::Exited { code: 0 },
        counters(0, 0),
    );
    stale.timing =
        RustDiagnosticTiming::new(epoch(2_200), Some(epoch(1_900)), 100, 500).expect("timing");
    let error = classify_rust_memory(stale).expect_err("stale preflight");
    assert_eq!(error.code, "stale_observation");

    let mut future = executed_input(
        &envelope,
        RustExecutionPhase::Completed,
        RustProcessTermination::Exited { code: 0 },
        counters(0, 0),
    );
    future.terminal.observed_at = epoch(2_300);
    let error = classify_rust_memory(future).expect_err("future terminal");
    assert_eq!(error.code, "future_observation");
}

#[test]
fn counter_reversal_and_overflow_are_refused() {
    let envelope = envelope(10_000);
    let mut reversal = executed_input(
        &envelope,
        RustExecutionPhase::Link,
        RustProcessTermination::Exited { code: 1 },
        counters(0, 0),
    );
    let RustCgroupMemoryEvidence::Complete { before, after } = &mut reversal.cgroup else {
        panic!("complete evidence")
    };
    before.events = counters(2, 1);
    after.events = counters(1, 1);
    let error = classify_rust_memory(reversal).expect_err("counter reversal");
    assert_eq!(error.code, "memory_event_counter_reversal");

    let error = RustMemoryEventCounters::new(0, 0, 0, MAX_RUST_MEMORY_EVENT_COUNTER + 1, 0)
        .expect_err("counter overflow");
    assert_eq!(error.code, "memory_event_counter_overflow");
}

#[test]
fn positive_oom_kill_with_incompatible_terminal_evidence_is_refused() {
    let envelope = envelope(10_000);
    let error = classify_rust_memory(executed_input(
        &envelope,
        RustExecutionPhase::Compile,
        RustProcessTermination::Exited { code: 1 },
        counters(1, 1),
    ))
    .expect_err("contradictory OOM evidence");

    assert_eq!(error.code, "oom_terminal_mismatch");
}

#[test]
fn refused_preflight_cannot_carry_execution_evidence() {
    let envelope = envelope(10_000);
    let mut input = refused_input(&envelope);
    input.timing =
        RustDiagnosticTiming::new(epoch(2_200), Some(epoch(1_200)), 500, 500).expect("timing");
    input.terminal = RustTerminalObservation::new(
        input.observation_binding(),
        epoch(2_000),
        RustExecutionPhase::Compile,
        RustProcessTermination::Exited { code: 1 },
    );

    let error = classify_rust_memory(input).expect_err("contradiction");
    assert_eq!(error.code, "preflight_execution_contradiction");
}

#[test]
fn public_json_and_debug_never_contain_private_process_or_command_material() {
    let envelope = envelope(10_000);
    let report = classify_rust_memory(executed_input(
        &envelope,
        RustExecutionPhase::Completed,
        RustProcessTermination::Exited { code: 0 },
        counters(0, 0),
    ))
    .expect("classification");
    let json = serde_json::to_string(&report).expect("json");
    let debug = format!("{report:?}");

    for forbidden in [
        "/sys/fs/cgroup",
        "/proc/1234",
        "cargo test",
        "CARGO_HOME",
        "stderr",
        "stdout",
        "github_pat_",
        "sk-proj-",
        "PID",
    ] {
        assert!(!json.contains(forbidden));
        assert!(!debug.contains(forbidden));
    }
    assert!(json.contains("codex-core-focused"));
    assert!(json.contains("attempt-1"));
    assert!(json.contains("rust-attempt-group"));
}
