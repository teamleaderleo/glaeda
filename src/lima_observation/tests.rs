use std::collections::VecDeque;
use std::io;
use std::sync::Mutex;

use crate::process::{CommandExecutor, CommandSpec, CommandValue, ExecutionRecord};

use super::*;

const LIMA_HOME: &str = "/Users/operator/.lima";
const INSTANCE_DIRECTORY: &str = "/Users/operator/.lima/smolrunner";
const CACHE_PATH: &str = "/home/runner/.cache/cargo";
const CONFIGURED_MEMORY: u64 = 3 * 1024 * 1024 * 1024;
const CONFIGURED_DISK: u64 = 80 * 1024 * 1024 * 1024;
const OBSERVED_PAGES: u64 = 770_000;

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
            .expect("scripted observation command")
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
            .expect("scripted clock value")
    }
}

#[test]
fn running_instance_produces_exact_bounded_observation() {
    let executor = ScriptedExecutor::new(running_steps());
    let observation = adapter()
        .observe(&request(30), &executor, &FakeClock::new([100, 105]))
        .expect("running observation");
    let report = observation.report();

    assert_eq!(report.schema_version, LIMA_OBSERVATION_SCHEMA_VERSION);
    assert_eq!(report.instance.as_str(), "smolrunner");
    assert_eq!(report.configured.runtime_state, LimaRuntimeState::Running);
    assert_eq!(report.configured.vm_type, LimaVmType::Vz);
    assert_eq!(report.configured.architecture, LimaArchitecture::Aarch64);
    assert_eq!(report.configured.cpus, 4);
    assert_eq!(report.configured.memory_bytes, CONFIGURED_MEMORY);
    assert_eq!(report.configured.primary_disk_bytes, CONFIGURED_DISK);
    assert_eq!(report.timing.started_at_unix_seconds, 100);
    assert_eq!(report.timing.observed_at_unix_seconds, 105);
    assert_eq!(report.timing.duration_seconds, 5);
    assert_eq!(
        report.timing.freshness_at(135),
        LimaObservationFreshness::Fresh
    );
    assert_eq!(
        report.timing.freshness_at(136),
        LimaObservationFreshness::Stale
    );
    assert_eq!(
        report.timing.freshness_at(99),
        LimaObservationFreshness::Future
    );

    let LimaGuestObservation::Observed(guest) = &report.guest else {
        panic!("running guest observation");
    };
    assert_eq!(guest.resources.architecture, LimaArchitecture::Aarch64);
    assert_eq!(guest.resources.cpus, 4);
    assert_eq!(guest.resources.memory_bytes, 4096 * OBSERVED_PAGES);
    let expected_machine_digest = format!("sha256:{}", "a".repeat(64));
    assert_eq!(
        guest.persistent_identity.guest_machine_id_digest.as_str(),
        expected_machine_digest
    );
    assert_eq!(
        guest.persistent_identity.root_filesystem,
        LimaFilesystemObjectIdentity {
            device_id: 2049,
            inode: 2,
        }
    );
    assert_eq!(
        guest.persistent_identity.cache_directory,
        LimaFilesystemObjectIdentity {
            device_id: 2049,
            inode: 12_345,
        }
    );
    assert_eq!(observation.private_evidence().commands().len(), 9);
    assert_eq!(executor.remaining(), 0);

    let commands = executor.seen();
    assert_eq!(commands.len(), 9);
    assert_eq!(
        commands[0].displayed_argv(),
        vec![
            "/opt/homebrew/bin/limactl",
            "--tty=false",
            "list",
            "--format=json",
            "--all-fields",
            "smolrunner",
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
                "/bin/sh" | "/bin/bash" | "/usr/bin/env" | "-c" | "-lc"
            )
        }));
    }
    assert_eq!(
        commands[7].displayed_argv(),
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
    assert_eq!(commands[8].displayed_argv(), commands[0].displayed_argv());
}

#[test]
fn stopped_instance_is_observed_without_guest_execution() {
    let executor = ScriptedExecutor::new(stopped_steps());
    let observation = adapter()
        .observe(&request(30), &executor, &FakeClock::new([200, 200]))
        .expect("stopped observation");

    assert!(matches!(
        &observation.report().guest,
        LimaGuestObservation::NotRunning {
            runtime_state: LimaRuntimeState::Stopped,
        }
    ));
    assert_eq!(observation.private_evidence().commands().len(), 2);
    assert_eq!(executor.seen().len(), 2);
}

#[test]
fn instance_parser_refuses_missing_duplicate_and_malformed_evidence() {
    let missing = parse_instance_output("").expect_err("missing instance");
    assert_eq!(
        missing.code,
        LimaObservationRefusalCode::MissingInstanceEvidence
    );

    let duplicate = parse_instance_output(&format!(
        "{}{}",
        instance_json("Running", INSTANCE_DIRECTORY, "vz", "aarch64", 4),
        instance_json("Running", INSTANCE_DIRECTORY, "vz", "aarch64", 4)
    ))
    .expect_err("duplicate instance");
    assert_eq!(
        duplicate.code,
        LimaObservationRefusalCode::DuplicateInstanceEvidence
    );

    let malformed = parse_instance_output("not-json\n").expect_err("malformed instance");
    assert_eq!(
        malformed.code,
        LimaObservationRefusalCode::MalformedInstanceEvidence
    );

    let duplicate_field = parse_instance_output(&format!(
        "{{\"name\":\"smolrunner\",\"name\":\"other\",\"status\":\"Running\",\"dir\":\"{INSTANCE_DIRECTORY}\",\"vmType\":\"vz\",\"arch\":\"aarch64\",\"cpus\":4,\"memory\":{CONFIGURED_MEMORY},\"disk\":{CONFIGURED_DISK}}}\n"
    ))
    .expect_err("duplicate field");
    assert_eq!(
        duplicate_field.code,
        LimaObservationRefusalCode::DuplicateInstanceEvidence
    );
}

#[test]
fn aliased_and_mismatched_instance_evidence_is_refused() {
    let aliased = run_stopped_failure(instance_json(
        "Stopped",
        "/Users/operator/.lima/./smolrunner",
        "vz",
        "aarch64",
        4,
    ));
    assert_eq!(aliased.code, LimaObservationRefusalCode::AliasedEvidence);

    let wrong_name = run_stopped_failure(
        instance_json("Stopped", INSTANCE_DIRECTORY, "vz", "aarch64", 4)
            .replace("\"smolrunner\"", "\"other\""),
    );
    assert_eq!(
        wrong_name.code,
        LimaObservationRefusalCode::InstanceMismatch
    );

    let wrong_directory = run_stopped_failure(instance_json(
        "Stopped",
        "/Users/operator/.lima/other",
        "vz",
        "aarch64",
        4,
    ));
    assert_eq!(
        wrong_directory.code,
        LimaObservationRefusalCode::InstanceDirectoryMismatch
    );

    let wrong_vm = run_stopped_failure(instance_json(
        "Stopped",
        INSTANCE_DIRECTORY,
        "qemu",
        "aarch64",
        4,
    ));
    assert_eq!(wrong_vm.code, LimaObservationRefusalCode::VmTypeMismatch);

    let arch_alias = run_stopped_failure(instance_json(
        "Stopped",
        INSTANCE_DIRECTORY,
        "vz",
        "arm64",
        4,
    ));
    assert_eq!(arch_alias.code, LimaObservationRefusalCode::AliasedEvidence);

    let wrong_arch = run_stopped_failure(instance_json(
        "Stopped",
        INSTANCE_DIRECTORY,
        "vz",
        "x86_64",
        4,
    ));
    assert_eq!(
        wrong_arch.code,
        LimaObservationRefusalCode::ArchitectureMismatch
    );
}

#[test]
fn runtime_or_resource_drift_is_refused_by_final_instance_observation() {
    let executor = ScriptedExecutor::new([
        ScriptedStep::Output(ScriptedOutput::success(instance_json(
            "Stopped",
            INSTANCE_DIRECTORY,
            "vz",
            "aarch64",
            4,
        ))),
        ScriptedStep::Output(ScriptedOutput::success(instance_json(
            "Running",
            INSTANCE_DIRECTORY,
            "vz",
            "aarch64",
            4,
        ))),
    ]);
    let failure = adapter()
        .observe(&request(30), &executor, &FakeClock::new([100]))
        .expect_err("instance drift");

    assert_eq!(failure.code, LimaObservationRefusalCode::InstanceDrift);
    assert_eq!(failure.phase, LimaObservationPhase::InstanceObservation);
    assert_eq!(failure.private_evidence().commands().len(), 2);
}

#[test]
fn stale_observation_is_refused_with_private_evidence_retained() {
    let executor = ScriptedExecutor::new(stopped_steps());
    let failure = adapter()
        .observe(&request(30), &executor, &FakeClock::new([100, 131]))
        .expect_err("stale observation");

    assert_eq!(failure.code, LimaObservationRefusalCode::StaleObservation);
    assert_eq!(failure.phase, LimaObservationPhase::Freshness);
    assert_eq!(failure.private_evidence().commands().len(), 2);
}

#[test]
fn guest_architecture_alias_and_mismatch_are_refused() {
    let alias = running_failure_with_override(0, "arm64\n");
    assert_eq!(alias.code, LimaObservationRefusalCode::AliasedEvidence);
    assert_eq!(alias.phase, LimaObservationPhase::GuestArchitecture);

    let mismatch = running_failure_with_override(0, "x86_64\n");
    assert_eq!(
        mismatch.code,
        LimaObservationRefusalCode::GuestArchitectureMismatch
    );
}

#[test]
fn guest_cpu_memory_and_missing_identity_evidence_fail_closed() {
    let cpu = running_failure_with_override(1, "8\n");
    assert_eq!(cpu.code, LimaObservationRefusalCode::GuestCpuMismatch);

    let memory = running_failure_with_override(3, "100\n");
    assert_eq!(memory.code, LimaObservationRefusalCode::GuestMemoryMismatch);

    let missing = running_failure_with_override(4, "");
    assert_eq!(
        missing.code,
        LimaObservationRefusalCode::MissingGuestEvidence
    );
    assert_eq!(missing.phase, LimaObservationPhase::GuestMachineIdentity);
}

#[test]
fn spawn_record_identity_and_output_bounds_are_refused() {
    let spawn = ScriptedExecutor::new([ScriptedStep::IoError(io::ErrorKind::NotFound)]);
    let spawn_failure = adapter()
        .observe(&request(30), &spawn, &FakeClock::new([100]))
        .expect_err("spawn failure");
    assert_eq!(
        spawn_failure.code,
        LimaObservationRefusalCode::CommandFailed
    );
    assert!(spawn_failure.private_evidence().commands().is_empty());

    let mut wrong_identity = ScriptedOutput::success(instance_json(
        "Stopped",
        INSTANCE_DIRECTORY,
        "vz",
        "aarch64",
        4,
    ));
    wrong_identity.argv_override = Some(vec!["/bin/sh".to_owned()]);
    let identity = ScriptedExecutor::new([ScriptedStep::Output(wrong_identity)]);
    let identity_failure = adapter()
        .observe(&request(30), &identity, &FakeClock::new([100]))
        .expect_err("command identity");
    assert_eq!(
        identity_failure.code,
        LimaObservationRefusalCode::CommandIdentityMismatch
    );
    assert_eq!(identity_failure.private_evidence().commands().len(), 1);

    let oversized = ScriptedExecutor::new([ScriptedStep::Output(ScriptedOutput::success(
        "x".repeat(MAX_LIMA_OBSERVATION_OUTPUT_BYTES + 1),
    ))]);
    let oversized_failure = adapter()
        .observe(&request(30), &oversized, &FakeClock::new([100]))
        .expect_err("bounded output");
    assert_eq!(
        oversized_failure.code,
        LimaObservationRefusalCode::UnboundedOutput
    );
}

#[test]
fn public_json_and_debug_exclude_private_paths_and_raw_output() {
    let private_marker = "PRIVATE_RAW_LIMA_OUTPUT";
    let mut steps = running_steps();
    let ScriptedStep::Output(instance_step) = &mut steps[0] else {
        panic!("instance step");
    };
    instance_step.stdout = instance_step.stdout.replace(
        "\"errors\":[]",
        &format!("\"message\":\"{private_marker}\",\"errors\":[]"),
    );
    let executor = ScriptedExecutor::new(steps);
    let observation = adapter()
        .observe(&request(30), &executor, &FakeClock::new([100, 101]))
        .expect("observation");

    let json = serde_json::to_string(&observation).expect("json");
    let debug = format!("{observation:?}");
    for private in [LIMA_HOME, INSTANCE_DIRECTORY, CACHE_PATH, private_marker] {
        assert!(!json.contains(private));
        assert!(!debug.contains(private));
    }
    assert!(debug.contains(REDACTED_PRIVATE_EVIDENCE));
    assert!(
        observation.private_evidence().commands()[0]
            .record()
            .stdout
            .contains("smolrunner")
    );
}

#[test]
fn request_rejects_aliased_private_paths() {
    let instance = LimaInstanceName::parse("smolrunner").expect("instance");
    let aliased_home = LimaObservationRequest::new(
        instance.clone(),
        "/Users/operator/.lima/../.lima",
        LimaVmType::Vz,
        LimaArchitecture::Aarch64,
        CACHE_PATH,
        30,
    )
    .expect_err("aliased Lima home");
    assert_eq!(aliased_home.code, LimaObservationRefusalCode::InvalidInput);

    let relative_cache = LimaObservationRequest::new(
        instance,
        LIMA_HOME,
        LimaVmType::Vz,
        LimaArchitecture::Aarch64,
        "relative/cache",
        30,
    )
    .expect_err("relative cache");
    assert_eq!(
        relative_cache.code,
        LimaObservationRefusalCode::InvalidInput
    );
}

#[test]
fn complete_request_identity_binds_private_source_cache_and_policy_without_disclosure() {
    let baseline = request(30);
    let different_home = LimaObservationRequest::new(
        LimaInstanceName::parse("smolrunner").expect("instance"),
        "/Users/operator/.lima-other",
        LimaVmType::Vz,
        LimaArchitecture::Aarch64,
        CACHE_PATH,
        30,
    )
    .expect("different home");
    let different_cache = LimaObservationRequest::new(
        LimaInstanceName::parse("smolrunner").expect("instance"),
        LIMA_HOME,
        LimaVmType::Vz,
        LimaArchitecture::Aarch64,
        "/home/runner/.cache/other",
        30,
    )
    .expect("different cache");
    let different_policy = request(31);

    assert_eq!(baseline.request_identity(), request(30).request_identity());
    assert_ne!(
        baseline.request_identity(),
        different_home.request_identity()
    );
    assert_ne!(
        baseline.request_identity(),
        different_cache.request_identity()
    );
    assert_ne!(
        baseline.request_identity(),
        different_policy.request_identity()
    );

    let debug = format!("{:?}", baseline.request_identity());
    for private in [LIMA_HOME, CACHE_PATH, "/Users/operator/.lima-other"] {
        assert!(!debug.contains(private));
    }
    assert!(debug.contains("sha256:"));
}

#[cfg(unix)]
#[test]
fn request_rejects_non_utf8_private_paths() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let non_utf8_home =
        std::path::PathBuf::from(OsString::from_vec(vec![b'/', b't', b'm', b'p', b'/', 0xff]));
    let failure = LimaObservationRequest::new(
        LimaInstanceName::parse("smolrunner").expect("instance"),
        non_utf8_home,
        LimaVmType::Vz,
        LimaArchitecture::Aarch64,
        CACHE_PATH,
        30,
    )
    .expect_err("non-UTF-8 Lima home");

    assert_eq!(failure.code, LimaObservationRefusalCode::InvalidInput);
}

fn adapter() -> LimaObservationAdapter {
    LimaObservationAdapter::new("/opt/homebrew/bin/limactl").expect("adapter")
}

fn request(max_age_seconds: u64) -> LimaObservationRequest {
    LimaObservationRequest::new(
        LimaInstanceName::parse("smolrunner").expect("instance"),
        LIMA_HOME,
        LimaVmType::Vz,
        LimaArchitecture::Aarch64,
        CACHE_PATH,
        max_age_seconds,
    )
    .expect("request")
}

fn running_steps() -> Vec<ScriptedStep> {
    vec![
        ScriptedStep::Output(ScriptedOutput::success(instance_json(
            "Running",
            INSTANCE_DIRECTORY,
            "vz",
            "aarch64",
            4,
        ))),
        ScriptedStep::Output(ScriptedOutput::success("aarch64\n")),
        ScriptedStep::Output(ScriptedOutput::success("4\n")),
        ScriptedStep::Output(ScriptedOutput::success("4096\n")),
        ScriptedStep::Output(ScriptedOutput::success(format!("{OBSERVED_PAGES}\n"))),
        ScriptedStep::Output(ScriptedOutput::success(format!(
            "{}  /etc/machine-id\n",
            "a".repeat(64)
        ))),
        ScriptedStep::Output(ScriptedOutput::success("2049:2\n")),
        ScriptedStep::Output(ScriptedOutput::success("2049:12345\n")),
        ScriptedStep::Output(ScriptedOutput::success(instance_json(
            "Running",
            INSTANCE_DIRECTORY,
            "vz",
            "aarch64",
            4,
        ))),
    ]
}

fn stopped_steps() -> Vec<ScriptedStep> {
    vec![
        ScriptedStep::Output(ScriptedOutput::success(instance_json(
            "Stopped",
            INSTANCE_DIRECTORY,
            "vz",
            "aarch64",
            4,
        ))),
        ScriptedStep::Output(ScriptedOutput::success(instance_json(
            "Stopped",
            INSTANCE_DIRECTORY,
            "vz",
            "aarch64",
            4,
        ))),
    ]
}

fn running_failure_with_override(guest_step: usize, stdout: &str) -> LimaObservationFailure {
    let mut steps = running_steps();
    let ScriptedStep::Output(output) = &mut steps[guest_step + 1] else {
        panic!("guest output step");
    };
    output.stdout = stdout.to_owned();
    adapter()
        .observe(
            &request(30),
            &ScriptedExecutor::new(steps),
            &FakeClock::new([100, 101]),
        )
        .expect_err("running observation failure")
}

fn run_stopped_failure(stdout: String) -> LimaObservationFailure {
    adapter()
        .observe(
            &request(30),
            &ScriptedExecutor::new([ScriptedStep::Output(ScriptedOutput::success(stdout))]),
            &FakeClock::new([100, 101]),
        )
        .expect_err("stopped observation failure")
}

fn instance_json(
    status: &str,
    directory: &str,
    vm_type: &str,
    architecture: &str,
    cpus: u16,
) -> String {
    format!(
        "{{\"name\":\"smolrunner\",\"status\":\"{status}\",\"dir\":\"{directory}\",\"vmType\":\"{vm_type}\",\"arch\":\"{architecture}\",\"cpus\":{cpus},\"memory\":{CONFIGURED_MEMORY},\"disk\":{CONFIGURED_DISK},\"errors\":[]}}\n"
    )
}
