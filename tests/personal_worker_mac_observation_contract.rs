#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io;
use std::time::Duration;

pub use smolrunner::{
    lima_lifecycle, lima_observation, mac_availability, macos_resource_observation,
    operator_config, process,
};

use smolrunner::lima_lifecycle::LimaResourceProfile;
use smolrunner::lima_observation::{
    LimaArchitecture, LimaInstanceName, LimaObservationAdapter, LimaObservationRequest,
    LimaRuntimeState, LimaVmType,
};
use smolrunner::mac_availability::{
    AvailabilityRequest, HostPowerSource, MemoryPressure, ObservationFreshness,
};
use smolrunner::macos_resource_observation::{
    lima_process_command, memory_pressure_command, power_command, swap_command,
};
use smolrunner::operator_config::{
    GuestWorkspacePath, OperatorConfig, OperatorIdlePolicy, OperatorOutputPreference,
    OperatorRemediationPreference, PersonalWorkerStateRoot,
};
use smolrunner::process::{CommandExecutor, CommandSpec, ExecutionRecord, TimedCommandExecutor};
use smolrunner::verification_profile::VerificationProfileId;

#[path = "../src/personal_worker_mac_observation.rs"]
mod personal_worker_mac_observation;

use personal_worker_mac_observation::{
    PersonalWorkerMacObservationAdapter, PersonalWorkerMacObservationClock,
    PersonalWorkerMacObservationErrorKind, logical_cpu_command, vm_stat_command,
};

const LIMA_HOME: &str = "/Users/operator/.lima";
const INSTANCE_DIRECTORY: &str = "/Users/operator/.lima/smolrunner";
const CACHE_PATH: &str = "/home/runner/.cache/cargo";
const INTERACTIVE_MEMORY: u64 = 3 * 1024 * 1024 * 1024;
const WORK_MEMORY: u64 = 10 * 1024 * 1024 * 1024;
const DISK_BYTES: u64 = 80 * 1024 * 1024 * 1024;

#[derive(Default)]
struct FakeExecutor {
    receipts: RefCell<BTreeMap<Vec<String>, VecDeque<ExecutionRecord>>>,
    seen: RefCell<Vec<(CommandSpec, Duration)>>,
}

impl FakeExecutor {
    fn with(self, command: CommandSpec, stdout: impl Into<String>) -> Self {
        self.with_record(
            command.clone(),
            ExecutionRecord {
                argv: command.displayed_argv(),
                environment_keys: command.environment.keys().cloned().collect(),
                status: Some(0),
                success: true,
                stdout: stdout.into(),
                stderr: String::new(),
            },
        )
    }

    fn with_record(mut self, command: CommandSpec, record: ExecutionRecord) -> Self {
        self.receipts
            .get_mut()
            .entry(command.displayed_argv())
            .or_default()
            .push_back(record);
        self
    }

    fn replacing(self, command: CommandSpec, stdout: impl Into<String>) -> Self {
        let record = ExecutionRecord {
            argv: command.displayed_argv(),
            environment_keys: command.environment.keys().cloned().collect(),
            status: Some(0),
            success: true,
            stdout: stdout.into(),
            stderr: String::new(),
        };
        self.replacing_record(command, record)
    }

    fn replacing_record(mut self, command: CommandSpec, record: ExecutionRecord) -> Self {
        self.receipts
            .get_mut()
            .insert(command.displayed_argv(), VecDeque::from([record]));
        self
    }

    fn seen(&self) -> Vec<(CommandSpec, Duration)> {
        self.seen.borrow().clone()
    }
}

impl CommandExecutor for FakeExecutor {
    fn execute(&self, _spec: &CommandSpec) -> io::Result<ExecutionRecord> {
        Err(io::Error::other("untimed execution is forbidden in B02"))
    }
}

impl TimedCommandExecutor for FakeExecutor {
    fn execute_with_timeout(
        &self,
        spec: &CommandSpec,
        timeout: Duration,
    ) -> io::Result<ExecutionRecord> {
        self.seen.borrow_mut().push((spec.clone(), timeout));
        self.receipts
            .borrow_mut()
            .get_mut(&spec.displayed_argv())
            .and_then(VecDeque::pop_front)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "private fixture missing"))
    }
}

struct FakeClock {
    values: RefCell<VecDeque<io::Result<u64>>>,
}

impl FakeClock {
    fn new(values: impl IntoIterator<Item = u64>) -> Self {
        Self {
            values: RefCell::new(values.into_iter().map(Ok).collect()),
        }
    }
}

impl PersonalWorkerMacObservationClock for FakeClock {
    fn unix_millis(&self) -> io::Result<u64> {
        self.values
            .borrow_mut()
            .pop_front()
            .expect("scripted B02 clock value")
    }
}

fn config(availability: AvailabilityRequest) -> OperatorConfig {
    OperatorConfig::new(
        PersonalWorkerStateRoot::parse("/Users/operator/Library/Application Support/SmolRunner")
            .expect("state root"),
        LimaInstanceName::parse("smolrunner").expect("instance"),
        GuestWorkspacePath::parse("/home/runner/workspace").expect("workspace"),
        VerificationProfileId::parse("smolrunner.required").expect("profile"),
        availability,
        OperatorIdlePolicy::new(600_000, 1_800_000).expect("idle policy"),
        OperatorOutputPreference::Json,
        OperatorRemediationPreference::CodesOnly,
    )
    .expect("operator config")
}

fn request(instance: &str) -> LimaObservationRequest {
    LimaObservationRequest::new(
        LimaInstanceName::parse(instance).expect("instance"),
        LIMA_HOME,
        LimaVmType::Vz,
        LimaArchitecture::Aarch64,
        CACHE_PATH,
        30,
    )
    .expect("Lima request")
}

fn adapter() -> PersonalWorkerMacObservationAdapter {
    PersonalWorkerMacObservationAdapter::new(30_000, Duration::from_secs(5)).expect("B02 adapter")
}

fn lima_adapter() -> LimaObservationAdapter {
    LimaObservationAdapter::new("/opt/homebrew/bin/limactl").expect("Lima adapter")
}

fn list_command(instance: &str) -> CommandSpec {
    CommandSpec::new("/opt/homebrew/bin/limactl")
        .environment("LIMA_HOME", LIMA_HOME)
        .environment("LANG", "C")
        .environment("LC_ALL", "C")
        .argument("--tty=false")
        .argument("list")
        .argument("--format=json")
        .argument("--all-fields")
        .argument(instance)
}

fn instance_json(instance: &str, cpus: u16, memory_bytes: u64, state: &str) -> String {
    format!(
        "{{\"name\":\"{instance}\",\"status\":\"{state}\",\"dir\":\"{LIMA_HOME}/{instance}\",\"vmType\":\"vz\",\"arch\":\"aarch64\",\"cpus\":{cpus},\"memory\":{memory_bytes},\"disk\":{DISK_BYTES},\"errors\":[]}}\n"
    )
}

fn complete_executor(cpus: u16, memory_bytes: u64) -> FakeExecutor {
    let list = list_command("smolrunner");
    let list_output = instance_json("smolrunner", cpus, memory_bytes, "Stopped");
    FakeExecutor::default()
        .with(memory_pressure_command(), "1\n")
        .with(
            swap_command(),
            "total = 4096.00M used = 1024.00M free = 3072.00M (encrypted)\n",
        )
        .with(
            power_command(),
            "Now drawing from 'AC Power'\n -InternalBattery-0\t100%; charged;\n",
        )
        .with(
            lima_process_command(),
            " 100 1 1.0 2048 00:05 /opt/homebrew/bin/limactl\n",
        )
        .with(
            vm_stat_command(),
            concat!(
                "Mach Virtual Memory Statistics: (page size of 16384 bytes)\n",
                "Pages free: 100000.\n",
                "Pages active: 200000.\n",
                "Pages inactive: 300000.\n",
                "Pages speculative: 100000.\n",
                "Pages purgeable: 50000.\n",
            ),
        )
        .with(logical_cpu_command(), "10\n")
        .with(list.clone(), list_output.clone())
        .with(list, list_output)
}

fn clock() -> FakeClock {
    FakeClock::new([100_000, 101_000, 102_000, 103_000])
}

#[test]
fn exact_stopped_observation_binds_config_host_lima_profile_and_one_timeout() {
    let executor = complete_executor(4, INTERACTIVE_MEMORY);
    let config = config(AvailabilityRequest::Away);
    let request = request("smolrunner");
    let observation = adapter()
        .observe(&config, &request, &lima_adapter(), &executor, &clock())
        .expect("complete B02 observation");
    let report = observation.report();

    assert_eq!(report.schema_version, 1);
    assert_eq!(report.config_identity, *config.identity());
    assert_eq!(report.requested_availability, AvailabilityRequest::Away);
    assert_eq!(report.timing.started_at_millis, 100_000);
    assert_eq!(report.timing.observed_at_millis, 103_000);
    assert_eq!(report.timing.duration_millis, 3_000);
    assert_eq!(report.timing.expires_at_millis, 133_000);
    assert_eq!(report.host_headroom.logical_cpu_count, 10);
    assert_eq!(
        report.host_headroom.available_memory_bytes,
        500_000 * 16_384
    );
    assert_eq!(
        report.host_resources.memory_pressure,
        MemoryPressure::Normal
    );
    assert_eq!(report.host_resources.power.source, HostPowerSource::Ac);
    assert_eq!(report.host_resources.freshness, ObservationFreshness::Fresh);
    assert_eq!(
        report.lima.configured.runtime_state,
        LimaRuntimeState::Stopped
    );
    assert_eq!(report.lima_profile, LimaResourceProfile::Interactive);
    assert_eq!(
        observation.lima_source_identity(),
        &request.source_identity()
    );

    let seen = executor.seen();
    assert_eq!(seen.len(), 8);
    assert!(
        seen.iter()
            .all(|(_, timeout)| *timeout == Duration::from_secs(5))
    );
    assert!(
        seen[..6]
            .iter()
            .all(|(command, _)| command.environment.is_empty())
    );
    assert!(seen[6..].iter().all(|(command, _)| {
        command
            .environment
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
            == vec!["LANG", "LC_ALL", "LIMA_HOME"]
    }));
    assert!(vm_stat_command().environment.is_empty());
    assert!(logical_cpu_command().environment.is_empty());

    let debug = format!("{observation:?}");
    let json = serde_json::to_string(report).expect("public report JSON");
    for private in [
        "/Users/operator",
        "/home/runner",
        "/opt/homebrew/bin/limactl",
        "InternalBattery",
    ] {
        assert!(!debug.contains(private), "debug leaked {private}");
        assert!(!json.contains(private), "JSON leaked {private}");
    }
}

#[test]
fn work_profile_is_exact_and_unreviewed_resource_envelopes_fail_closed() {
    let work = adapter()
        .observe(
            &config(AvailabilityRequest::Active),
            &request("smolrunner"),
            &lima_adapter(),
            &complete_executor(8, WORK_MEMORY),
            &clock(),
        )
        .expect("work observation");
    assert_eq!(work.report().lima_profile, LimaResourceProfile::Work);

    let error = adapter()
        .observe(
            &config(AvailabilityRequest::Active),
            &request("smolrunner"),
            &lima_adapter(),
            &complete_executor(6, 6 * 1024 * 1024 * 1024),
            &clock(),
        )
        .expect_err("unreviewed Lima profile");
    assert_eq!(
        error.kind,
        PersonalWorkerMacObservationErrorKind::LimaEvidence
    );
    assert_eq!(error.code, "unsupported_lima_profile");
}

#[test]
fn config_request_mismatch_fails_before_clock_or_commands() {
    let executor = complete_executor(4, INTERACTIVE_MEMORY);
    let error = adapter()
        .observe(
            &config(AvailabilityRequest::Active),
            &request("other"),
            &lima_adapter(),
            &executor,
            &FakeClock::new([]),
        )
        .expect_err("config/request mismatch");

    assert_eq!(error.code, "config_lima_instance_mismatch");
    assert!(executor.seen().is_empty());
}

#[test]
fn malformed_duplicate_overflowed_memory_and_cpu_evidence_are_refused() {
    for output in [
        "Mach Virtual Memory Statistics: (page size of 16384 bytes)\nPages free: 1.\nPages inactive: 1.\n",
        "Mach Virtual Memory Statistics: (page size of 16384 bytes)\nPages free: 1.\nPages free: 2.\nPages inactive: 1.\nPages speculative: 1.\n",
        "Mach Virtual Memory Statistics: (page size of 65536 bytes)\nPages free: 18446744073709551615.\nPages inactive: 1.\nPages speculative: 1.\n",
    ] {
        let command = vm_stat_command();
        let executor = complete_executor(4, INTERACTIVE_MEMORY).replacing(command, output);
        let error = adapter()
            .observe(
                &config(AvailabilityRequest::Active),
                &request("smolrunner"),
                &lima_adapter(),
                &executor,
                &clock(),
            )
            .expect_err("invalid memory evidence");
        assert_eq!(error.code, "malformed_available_memory");
    }

    for output in ["0\n", "1025\n", "10\n11\n", "010\n"] {
        let command = logical_cpu_command();
        let executor = complete_executor(4, INTERACTIVE_MEMORY).replacing(command, output);
        let error = adapter()
            .observe(
                &config(AvailabilityRequest::Active),
                &request("smolrunner"),
                &lima_adapter(),
                &executor,
                &clock(),
            )
            .expect_err("invalid CPU evidence");
        assert_eq!(error.code, "malformed_logical_cpu_count");
    }
}

#[test]
fn command_identity_output_and_timeout_failures_are_bounded_and_private() {
    let command = vm_stat_command();
    let executor = complete_executor(4, INTERACTIVE_MEMORY).replacing_record(
        command,
        ExecutionRecord {
            argv: vec!["/usr/bin/vm_stat".to_owned(), "--unsafe".to_owned()],
            environment_keys: Vec::new(),
            status: Some(0),
            success: true,
            stdout: "/Users/operator/private".to_owned(),
            stderr: String::new(),
        },
    );
    let error = adapter()
        .observe(
            &config(AvailabilityRequest::Active),
            &request("smolrunner"),
            &lima_adapter(),
            &executor,
            &clock(),
        )
        .expect_err("command drift");
    assert_eq!(error.code, "command_identity_mismatch");
    assert!(!format!("{error:?}").contains("/Users/operator"));
    assert!(
        !serde_json::to_string(&error)
            .expect("error JSON")
            .contains("/Users/operator")
    );

    let command = vm_stat_command();
    let executor = complete_executor(4, INTERACTIVE_MEMORY).replacing_record(
        command.clone(),
        ExecutionRecord {
            argv: command.displayed_argv(),
            environment_keys: Vec::new(),
            status: Some(0),
            success: true,
            stdout: "x".repeat(65_537),
            stderr: String::new(),
        },
    );
    let error = adapter()
        .observe(
            &config(AvailabilityRequest::Active),
            &request("smolrunner"),
            &lima_adapter(),
            &executor,
            &clock(),
        )
        .expect_err("oversized direct output");
    assert_eq!(error.code, "unbounded_command_output");

    assert!(PersonalWorkerMacObservationAdapter::new(30_000, Duration::ZERO).is_err());
    assert!(PersonalWorkerMacObservationAdapter::new(30_000, Duration::from_secs(31)).is_err());
}

#[test]
fn partial_low_level_host_evidence_stays_unknown_instead_of_becoming_absent() {
    let mut executor = complete_executor(4, INTERACTIVE_MEMORY);
    executor
        .receipts
        .get_mut()
        .remove(&power_command().displayed_argv());
    let observation = adapter()
        .observe(
            &config(AvailabilityRequest::Active),
            &request("smolrunner"),
            &lima_adapter(),
            &executor,
            &clock(),
        )
        .expect("partial host observation remains representable");
    assert_eq!(
        observation.report().host_resources.power.source,
        HostPowerSource::Unknown
    );
    assert!(!observation.report().host_resources.problems.is_empty());
}

#[test]
fn outer_and_lima_timing_must_be_fresh_monotonic_and_coherent() {
    let stale = adapter()
        .observe(
            &config(AvailabilityRequest::Active),
            &request("smolrunner"),
            &lima_adapter(),
            &complete_executor(4, INTERACTIVE_MEMORY),
            &FakeClock::new([100_000, 101_000, 102_000, 140_001]),
        )
        .expect_err("stale outer observation");
    assert_eq!(stale.code, "stale_observation");

    let incoherent = adapter()
        .observe(
            &config(AvailabilityRequest::Active),
            &request("smolrunner"),
            &lima_adapter(),
            &complete_executor(4, INTERACTIVE_MEMORY),
            &FakeClock::new([100_000, 99_000, 99_000, 103_000]),
        )
        .expect_err("Lima timing outside outer observation");
    assert_eq!(incoherent.code, "lima_timing_mismatch");
}

#[test]
fn lima_failures_retain_private_evidence_behind_bounded_redaction() {
    let list = list_command("smolrunner");
    let executor = complete_executor(4, INTERACTIVE_MEMORY)
        .replacing(list, "/Users/operator/private malformed Lima output\n");
    let error = adapter()
        .observe(
            &config(AvailabilityRequest::Active),
            &request("smolrunner"),
            &lima_adapter(),
            &executor,
            &clock(),
        )
        .expect_err("malformed Lima observation");

    assert_eq!(error.code, "lima_observation_failed");
    assert!(error.lima_code.is_some());
    assert!(error.lima_phase.is_some());
    assert!(error.private_lima_failure().is_some());
    assert!(!format!("{error:?}").contains("/Users/operator"));
    assert!(
        !serde_json::to_string(&error)
            .expect("public B02 error JSON")
            .contains("/Users/operator")
    );
}

#[test]
fn source_has_read_only_fixed_command_authority_only() {
    assert_eq!(vm_stat_command().displayed_argv(), vec!["/usr/bin/vm_stat"]);
    assert_eq!(
        logical_cpu_command().displayed_argv(),
        vec!["/usr/sbin/sysctl", "-n", "hw.logicalcpu"]
    );

    let source =
        fs::read_to_string("src/personal_worker_mac_observation.rs").expect("B02 module source");
    for forbidden in [
        "Command::new",
        "std::fs",
        "std::env",
        "sh -c",
        "limactl start",
        "limactl stop",
        "limactl edit",
        "octocrab",
        "reqwest",
        "unsafe {",
    ] {
        assert!(
            !source.contains(forbidden),
            "forbidden B02 authority: {forbidden}"
        );
    }
}
