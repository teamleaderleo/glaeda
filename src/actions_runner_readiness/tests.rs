use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::lima_observation::{
    LimaArchitecture, LimaConfiguredInstance, LimaGuestResources, LimaObservedGuest,
    LimaPersistentIdentity, LimaVmType,
};
use crate::process::{CommandExecutor, CommandSpec, CommandValue, ExecutionRecord};

use super::*;

const LIMA_HOME: &str = "/Users/operator/.lima";
const RUNNER_ROOT: &str = "/home/runner/actions-runner";
const DRAIN_MARKER: &str = "/home/runner/actions-runner/.smolrunner-draining";
const CONFIG_HEX: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const LISTENER_PID: u32 = 42;
const WORKER_PID: u32 = 43;

#[derive(Debug)]
enum ScriptedStep {
    Output(ScriptedOutput),
    IoError(io::ErrorKind),
}

#[derive(Debug)]
struct ScriptedOutput {
    stdout: String,
    stderr: String,
    status: Option<i32>,
    success: bool,
    argv_override: Option<Vec<String>>,
    environment_override: Option<Vec<String>>,
}

impl ScriptedOutput {
    fn success(stdout: impl Into<String>) -> Self {
        Self {
            stdout: stdout.into(),
            stderr: String::new(),
            status: Some(0),
            success: true,
            argv_override: None,
            environment_override: None,
        }
    }

    fn absent() -> Self {
        Self {
            stdout: String::new(),
            stderr: String::new(),
            status: Some(1),
            success: false,
            argv_override: None,
            environment_override: None,
        }
    }
}

#[derive(Debug, Default)]
struct ScriptedExecutor {
    steps: Mutex<VecDeque<ScriptedStep>>,
    seen: Mutex<Vec<CommandSpec>>,
}

impl ScriptedExecutor {
    fn new(steps: impl IntoIterator<Item = ScriptedStep>) -> Self {
        Self {
            steps: Mutex::new(steps.into_iter().collect()),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn seen(&self) -> Vec<CommandSpec> {
        self.seen.lock().expect("seen lock").clone()
    }

    fn remaining(&self) -> usize {
        self.steps.lock().expect("steps lock").len()
    }
}

impl CommandExecutor for ScriptedExecutor {
    fn execute(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord> {
        self.seen.lock().expect("seen lock").push(spec.clone());
        match self
            .steps
            .lock()
            .expect("steps lock")
            .pop_front()
            .expect("scripted runner readiness command")
        {
            ScriptedStep::IoError(kind) => Err(io::Error::new(kind, "private scripted failure")),
            ScriptedStep::Output(output) => Ok(ExecutionRecord {
                argv: output
                    .argv_override
                    .unwrap_or_else(|| spec.displayed_argv()),
                environment_keys: output
                    .environment_override
                    .unwrap_or_else(|| spec.environment.keys().cloned().collect::<Vec<_>>()),
                status: output.status,
                success: output.success,
                stdout: output.stdout,
                stderr: output.stderr,
            }),
        }
    }
}

#[derive(Debug)]
struct FakeClock {
    values: Mutex<VecDeque<io::Result<u64>>>,
}

impl FakeClock {
    fn new(values: impl IntoIterator<Item = u64>) -> Self {
        Self {
            values: Mutex::new(values.into_iter().map(Ok).collect()),
        }
    }
}

impl LimaObservationClock for FakeClock {
    fn unix_seconds(&self) -> io::Result<u64> {
        self.values
            .lock()
            .expect("clock lock")
            .pop_front()
            .expect("scripted readiness clock")
    }
}

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

    assert_eq!(report.schema_version, ACTIONS_RUNNER_READINESS_SCHEMA_VERSION);
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
    assert_eq!(commands.len(), 16);
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
        commands[3].displayed_argv(),
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
        commands[5].displayed_argv(),
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
            vec!["LANG", "LC_ALL", "LIMA_HOME"]
        );
        assert_eq!(
            command.environment.get("LIMA_HOME"),
            Some(&CommandValue::Plain(LIMA_HOME.to_owned()))
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
    assert_eq!(booting.report().state, ActionsRunnerReadinessState::Starting);
    assert!(boot_executor.seen().is_empty());

    let starting = adapter()
        .observe(
            &request(),
            &source(LimaRuntimeState::Running, true, 130),
            &ScriptedExecutor::new(running_steps_without_processes(false)),
            &FakeClock::new([100, 101]),
        )
        .expect("starting listener observation");
    assert_eq!(starting.report().state, ActionsRunnerReadinessState::Starting);
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
    assert_eq!(draining.report().state, ActionsRunnerReadinessState::Draining);
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
    assert_eq!(expired.private_evidence().commands().len(), 16);
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
    for private in [LIMA_HOME, RUNNER_ROOT, DRAIN_MARKER, "Runner.Listener", "42"] {
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
    let digest = digest();
    let aliased = ActionsRunnerReadinessRequest::new(
        instance.clone(),
        name.clone(),
        "/Users/operator/.lima/../.lima",
        RUNNER_ROOT,
        DRAIN_MARKER,
        digest.clone(),
    )
    .expect_err("aliased home");
    assert_eq!(aliased.code, ActionsRunnerReadinessRefusalCode::InvalidInput);

    let relative = ActionsRunnerReadinessRequest::new(
        instance.clone(),
        name.clone(),
        LIMA_HOME,
        "relative/runner",
        DRAIN_MARKER,
        digest.clone(),
    )
    .expect_err("relative root");
    assert_eq!(relative.code, ActionsRunnerReadinessRefusalCode::InvalidInput);

    let external_marker = ActionsRunnerReadinessRequest::new(
        instance,
        name,
        LIMA_HOME,
        RUNNER_ROOT,
        "/tmp/draining",
        digest,
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
        assert_eq!(failure.code, ActionsRunnerReadinessRefusalCode::InvalidInput);
    }
}

fn adapter() -> ActionsRunnerReadinessAdapter {
    ActionsRunnerReadinessAdapter::new("/opt/homebrew/bin/limactl").expect("adapter")
}

fn request() -> ActionsRunnerReadinessRequest {
    ActionsRunnerReadinessRequest::new(
        LimaInstanceName::parse("smolrunner").expect("instance"),
        ActionsRunnerName::parse("smolrunner-macbook").expect("runner name"),
        LIMA_HOME,
        RUNNER_ROOT,
        DRAIN_MARKER,
        digest(),
    )
    .expect("request")
}

fn digest() -> Sha256Digest {
    Sha256Digest::parse(&format!("sha256:{CONFIG_HEX}")).expect("digest")
}

fn source(
    runtime_state: LimaRuntimeState,
    with_guest: bool,
    expires_at: u64,
) -> LimaInstanceObservationReport {
    let guest = if with_guest {
        LimaGuestObservation::Observed(LimaObservedGuest {
            resources: LimaGuestResources {
                architecture: LimaArchitecture::Aarch64,
                cpus: 4,
                memory_bytes: 3 * 1024 * 1024 * 1024,
            },
            persistent_identity: LimaPersistentIdentity {
                guest_machine_id_digest: Sha256Digest::parse(&format!(
                    "sha256:{}",
                    "a".repeat(64)
                ))
                .expect("machine digest"),
                root_filesystem: LimaFilesystemObjectIdentity {
                    device_id: 2049,
                    inode: 2,
                },
                cache_directory: LimaFilesystemObjectIdentity {
                    device_id: 2049,
                    inode: 12_345,
                },
            },
        })
    } else {
        LimaGuestObservation::NotRunning { runtime_state }
    };
    LimaInstanceObservationReport {
        schema_version: crate::lima_observation::LIMA_OBSERVATION_SCHEMA_VERSION,
        instance: LimaInstanceName::parse("smolrunner").expect("instance"),
        configured: LimaConfiguredInstance {
            runtime_state,
            vm_type: LimaVmType::Vz,
            architecture: LimaArchitecture::Aarch64,
            cpus: 4,
            memory_bytes: 3 * 1024 * 1024 * 1024,
            primary_disk_bytes: 80 * 1024 * 1024 * 1024,
        },
        guest,
        timing: LimaObservationTiming {
            started_at_unix_seconds: 90,
            observed_at_unix_seconds: 95,
            expires_at_unix_seconds: expires_at,
            duration_seconds: 5,
            freshness: LimaObservationFreshness::Fresh,
        },
    }
}

fn running_steps(with_worker: bool, draining: bool) -> Vec<ScriptedStep> {
    let mut steps = Vec::new();
    append_identity(&mut steps);
    steps.push(ScriptedStep::Output(if draining {
        ScriptedOutput::success("")
    } else {
        ScriptedOutput::absent()
    }));
    append_process_snapshot(&mut steps, true, with_worker, LISTENER_PID, WORKER_PID);
    append_process_snapshot(&mut steps, true, with_worker, LISTENER_PID, WORKER_PID);
    append_identity(&mut steps);
    steps.push(ScriptedStep::Output(if draining {
        ScriptedOutput::success("")
    } else {
        ScriptedOutput::absent()
    }));
    steps
}

fn running_steps_without_processes(draining: bool) -> Vec<ScriptedStep> {
    let mut steps = Vec::new();
    append_identity(&mut steps);
    steps.push(ScriptedStep::Output(if draining {
        ScriptedOutput::success("")
    } else {
        ScriptedOutput::absent()
    }));
    append_process_snapshot(&mut steps, false, false, LISTENER_PID, WORKER_PID);
    append_process_snapshot(&mut steps, false, false, LISTENER_PID, WORKER_PID);
    append_identity(&mut steps);
    steps.push(ScriptedStep::Output(if draining {
        ScriptedOutput::success("")
    } else {
        ScriptedOutput::absent()
    }));
    steps
}

fn worker_without_listener_steps() -> Vec<ScriptedStep> {
    let mut steps = Vec::new();
    append_identity(&mut steps);
    steps.push(ScriptedStep::Output(ScriptedOutput::absent()));
    append_process_snapshot(&mut steps, false, true, LISTENER_PID, WORKER_PID);
    append_process_snapshot(&mut steps, false, true, LISTENER_PID, WORKER_PID);
    append_identity(&mut steps);
    steps.push(ScriptedStep::Output(ScriptedOutput::absent()));
    steps
}

fn append_identity(steps: &mut Vec<ScriptedStep>) {
    steps.push(ScriptedStep::Output(ScriptedOutput::success("2049:500\n")));
    steps.push(ScriptedStep::Output(ScriptedOutput::success(format!(
        "{CONFIG_HEX}  [REDACTED]\n"
    ))));
}

fn append_process_snapshot(
    steps: &mut Vec<ScriptedStep>,
    listener: bool,
    worker: bool,
    listener_pid: u32,
    worker_pid: u32,
) {
    steps.push(ScriptedStep::Output(if listener {
        ScriptedOutput::success(format!("{listener_pid}\n"))
    } else {
        ScriptedOutput::absent()
    }));
    steps.push(ScriptedStep::Output(if worker {
        ScriptedOutput::success(format!("{worker_pid}\n"))
    } else {
        ScriptedOutput::absent()
    }));
    if listener {
        append_process_identity(steps, LISTENER_NAME, 4200);
    }
    if worker {
        append_process_identity(steps, WORKER_NAME, 4300);
    }
}

fn append_process_identity(steps: &mut Vec<ScriptedStep>, process_name: &str, inode: u64) {
    steps.push(ScriptedStep::Output(ScriptedOutput::success(format!(
        "{RUNNER_ROOT}/bin/{process_name}\n"
    ))));
    steps.push(ScriptedStep::Output(ScriptedOutput::success(format!(
        "{RUNNER_ROOT}\n"
    ))));
    steps.push(ScriptedStep::Output(ScriptedOutput::success(format!(
        "900:{inode}\n"
    ))));
}
