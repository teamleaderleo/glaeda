use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io;

use super::*;

#[derive(Default)]
struct FakeExecutor {
    receipts: RefCell<BTreeMap<Vec<String>, ExecutionRecord>>,
}

impl FakeExecutor {
    fn with(mut self, command: CommandSpec, stdout: impl Into<String>) -> Self {
        let receipt = ExecutionRecord {
            argv: command.displayed_argv(),
            environment_keys: Vec::new(),
            status: Some(0),
            success: true,
            stdout: stdout.into(),
            stderr: String::new(),
        };
        self.receipts
            .get_mut()
            .insert(command.displayed_argv(), receipt);
        self
    }

    fn with_receipt(mut self, command: CommandSpec, receipt: ExecutionRecord) -> Self {
        self.receipts
            .get_mut()
            .insert(command.displayed_argv(), receipt);
        self
    }
}

impl CommandExecutor for FakeExecutor {
    fn execute(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord> {
        self.receipts
            .borrow_mut()
            .remove(&spec.displayed_argv())
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "fixture command missing"))
    }
}

fn complete_executor() -> FakeExecutor {
    FakeExecutor::default()
        .with(
            memory_pressure_command(),
            "1\n",
        )
        .with(
            swap_command(),
            "total = 4096.00M  used = 1024.00M  free = 3072.00M  (encrypted)\n",
        )
        .with(
            power_command(),
            "Now drawing from 'AC Power'\n -InternalBattery-0 (id=1)\t100%; charged; 0:00 remaining present: true\n",
        )
        .with(
            lima_process_command(),
            concat!(
                "  50   1  0.2  900 00:10 /usr/local/bin/qemu-system-aarch64\n",
                " 100   1  1.25 2048 01:02 /opt/homebrew/bin/limactl\n",
                " 101 100 20.5 4096 01:01 /opt/homebrew/bin/vfkit\n",
                " 102 101  0.1  512 00:59 /opt/homebrew/bin/socket_vmnet\n",
                " 200   1  3.0 1024 00:30 /Applications/Other.app/worker\n",
            ),
        )
}

#[test]
fn commands_use_exact_absolute_programs_and_fixed_arguments() {
    assert_eq!(
        memory_pressure_command().displayed_argv(),
        vec![
            "/usr/sbin/sysctl",
            "-n",
            "kern.memorystatus_vm_pressure_level"
        ]
    );
    assert_eq!(
        swap_command().displayed_argv(),
        vec!["/usr/sbin/sysctl", "-n", "vm.swapusage"]
    );
    assert_eq!(
        power_command().displayed_argv(),
        vec!["/usr/bin/pmset", "-g", "batt"]
    );
    assert_eq!(
        lima_process_command().displayed_argv(),
        vec!["/bin/ps", "-axo", "pid=,ppid=,%cpu=,rss=,etime=,comm="]
    );
    for command in [
        memory_pressure_command(),
        swap_command(),
        power_command(),
        lima_process_command(),
    ] {
        assert!(command.environment.is_empty());
        assert_ne!(command.program, std::path::PathBuf::from("/bin/sh"));
        assert_ne!(command.program, std::path::PathBuf::from("/bin/bash"));
    }
}

#[test]
fn complete_observation_reuses_availability_vocabulary_and_filters_processes() {
    let observation = observe_macos_resources(&complete_executor(), 1_000, 1_500, 30_000)
        .expect("complete fixture must observe");
    let report = observation.report();

    assert_eq!(report.schema_version, 1);
    assert_eq!(report.freshness, ObservationFreshness::Fresh);
    assert_eq!(report.completeness, ObservationCompleteness::Complete);
    assert_eq!(report.memory_pressure, MemoryPressure::Normal);
    assert_eq!(
        report.swap,
        Some(SwapObservation {
            total_bytes: 4 * 1024 * 1024 * 1024,
            used_bytes: 1024 * 1024 * 1024,
            free_bytes: 3 * 1024 * 1024 * 1024,
            encrypted: Some(true),
        })
    );
    assert_eq!(report.power.source, HostPowerSource::Ac);
    assert_eq!(report.power.battery_percent, Some(100));
    assert_eq!(report.power.charge_state, BatteryChargeState::Charged);
    assert!(report.problems.is_empty());

    assert_eq!(
        report
            .lima_processes
            .iter()
            .map(|process| (process.pid, process.role))
            .collect::<Vec<_>>(),
        vec![
            (100, LimaProcessRole::Controller),
            (101, LimaProcessRole::VirtualMachine),
            (102, LimaProcessRole::Network),
        ]
    );
    assert_eq!(report.lima_processes[0].cpu_basis_points, 125);
    assert_eq!(report.lima_processes[0].rss_bytes, 2 * 1024 * 1024);
    assert_eq!(report.lima_processes[0].elapsed_seconds, 62);
    assert!(
        !report
            .lima_processes
            .iter()
            .any(|process| process.pid == 50)
    );
}

#[test]
fn battery_source_and_discharge_details_are_preserved() {
    let executor = complete_executor().with(
        power_command(),
        "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=1)\t67%; discharging; 2:41 remaining present: true\n",
    );
    let report = observe_macos_resources(&executor, 1_000, 1_001, 30_000)
        .expect("battery fixture must observe")
        .into_report();

    assert_eq!(report.power.source, HostPowerSource::Battery);
    assert_eq!(report.power.battery_percent, Some(67));
    assert_eq!(report.power.charge_state, BatteryChargeState::Discharging);
    assert!(
        !report
            .problems
            .contains(&MacOsResourceProblemKind::BatteryDetailsUnavailable)
    );
}

#[test]
fn unknown_memory_pressure_level_is_partial() {
    let executor = complete_executor().with(
        memory_pressure_command(),
        "3
",
    );
    let report = observe_macos_resources(&executor, 1_000, 1_001, 30_000)
        .expect("unknown pressure fixture must observe")
        .into_report();

    assert_eq!(report.memory_pressure, MemoryPressure::Unknown);
    assert_eq!(report.completeness, ObservationCompleteness::Partial);
    assert!(
        report
            .problems
            .contains(&MacOsResourceProblemKind::MemoryPressureUnavailable)
    );
}

#[test]
fn malformed_or_inconsistent_swap_evidence_is_unknown_not_zero() {
    let executor = complete_executor().with(
        swap_command(),
        "total = 4096.00M  used = 1024.00M  free = 1024.00M  (encrypted)\n",
    );
    let report = observe_macos_resources(&executor, 1_000, 1_001, 30_000)
        .expect("invalid swap fixture must remain representable")
        .into_report();

    assert_eq!(report.swap, None);
    assert!(
        report
            .problems
            .contains(&MacOsResourceProblemKind::SwapUnavailable)
    );
}

#[test]
fn duplicate_process_ids_make_process_evidence_unavailable() {
    let executor = complete_executor().with(
        lima_process_command(),
        concat!(
            " 100 1 1.0 1024 00:01 /opt/homebrew/bin/limactl\n",
            " 100 1 2.0 2048 00:02 /opt/homebrew/bin/lima\n",
        ),
    );
    let report = observe_macos_resources(&executor, 1_000, 1_001, 30_000)
        .expect("duplicate process fixture must remain representable")
        .into_report();

    assert!(report.lima_processes.is_empty());
    assert!(
        report
            .problems
            .contains(&MacOsResourceProblemKind::LimaProcessObservationUnavailable)
    );
}

#[test]
fn only_lima_roots_and_their_descendants_are_exposed() {
    let processes = parse_process_receipt(
        &lima_process_command(),
        &ExecutionRecord {
            argv: lima_process_command().displayed_argv(),
            environment_keys: Vec::new(),
            status: Some(0),
            success: true,
            stdout: concat!(
                " 10 1 9.0 9999 00:10 /usr/local/bin/qemu-system-aarch64\n",
                " 20 1 0.1 1000 00:10 /opt/homebrew/bin/lima-hostagent\n",
                " 21 20 3.0 2000 00:09 /usr/local/bin/qemu-system-aarch64\n",
                " 22 21 0.0 100 00:08 /private/path/unknown-helper\n",
            )
            .to_owned(),
            stderr: String::new(),
        },
    )
    .expect("valid process output");

    assert_eq!(
        processes
            .iter()
            .map(|process| (process.pid, process.role))
            .collect::<Vec<_>>(),
        vec![
            (20, LimaProcessRole::HostAgent),
            (21, LimaProcessRole::VirtualMachine),
            (22, LimaProcessRole::Auxiliary),
        ]
    );
}

#[test]
fn oversized_public_process_list_is_deterministically_truncated() {
    let mut output = String::from(" 1 0 0.0 10 00:01 /opt/homebrew/bin/limactl\n");
    for pid in 2..=80 {
        output.push_str(&format!(
            " {pid} 1 0.1 10 00:01 /private/lima/helper-{pid}\n"
        ));
    }
    let executor = complete_executor().with(lima_process_command(), output);
    let report = observe_macos_resources(&executor, 1_000, 1_001, 30_000)
        .expect("bounded process fixture must observe")
        .into_report();

    assert_eq!(report.lima_processes.len(), MAX_PUBLIC_LIMA_PROCESSES);
    assert!(
        report
            .problems
            .contains(&MacOsResourceProblemKind::LimaProcessListTruncated)
    );
}

#[test]
fn command_shape_drift_and_private_output_are_refused() {
    let command = swap_command();
    let executor = complete_executor().with_receipt(
        command.clone(),
        ExecutionRecord {
            argv: vec!["/usr/sbin/sysctl".to_owned(), "vm.swapusage".to_owned()],
            environment_keys: Vec::new(),
            status: Some(0),
            success: true,
            stdout: "total = 1.00G used = 0.00G free = 1.00G /Users/alice/private".to_owned(),
            stderr: String::new(),
        },
    );
    let observation = observe_macos_resources(&executor, 1_000, 1_001, 30_000)
        .expect("drift remains a typed partial observation");

    assert_eq!(observation.report().swap, None);
    assert!(
        observation
            .report()
            .problems
            .contains(&MacOsResourceProblemKind::SwapUnavailable)
    );
    let json = serde_json::to_string(observation.report()).expect("serialize public report");
    let debug = format!("{observation:?}");
    for private in [
        "/Users/alice/private",
        "/opt/homebrew/bin/limactl",
        "/opt/homebrew/bin/vfkit",
        "socket_vmnet",
    ] {
        assert!(!json.contains(private));
        assert!(!debug.contains(private));
    }
    assert!(debug.contains("[REDACTED]"));
}

#[test]
fn stale_and_partial_evidence_are_explicit() {
    let executor = FakeExecutor::default();
    let report = observe_macos_resources(&executor, 1_000, 40_001, 30_000)
        .expect("missing commands become partial evidence")
        .into_report();

    assert_eq!(report.freshness, ObservationFreshness::Stale);
    assert_eq!(report.completeness, ObservationCompleteness::Partial);
    assert_eq!(report.memory_pressure, MemoryPressure::Unknown);
    assert_eq!(report.power.source, HostPowerSource::Unknown);
    assert_eq!(report.swap, None);
    assert!(report.lima_processes.is_empty());
    for problem in [
        MacOsResourceProblemKind::StaleObservation,
        MacOsResourceProblemKind::MemoryPressureUnavailable,
        MacOsResourceProblemKind::SwapUnavailable,
        MacOsResourceProblemKind::PowerUnavailable,
        MacOsResourceProblemKind::LimaProcessObservationUnavailable,
    ] {
        assert!(report.problems.contains(&problem));
    }
}

#[test]
fn invalid_time_boundaries_fail_closed() {
    let executor = FakeExecutor::default();
    assert_eq!(
        observe_macos_resources(&executor, 0, 1, 30_000)
            .expect_err("zero observation time must fail")
            .code,
        "invalid_observation_time"
    );
    assert_eq!(
        observe_macos_resources(&executor, 1, 1, 0)
            .expect_err("zero freshness window must fail")
            .code,
        "invalid_freshness_window"
    );
    assert_eq!(
        observe_macos_resources(&executor, 2, 1, 30_000)
            .expect_err("time reversal must fail")
            .code,
        "observation_time_reversal"
    );
}

#[test]
fn parsers_reject_duplicate_or_noncanonical_evidence() {
    assert_eq!(
        parse_memory_pressure_output(
            "1
"
        ),
        Some(MemoryPressure::Normal)
    );
    assert_eq!(
        parse_memory_pressure_output(
            "2
"
        ),
        Some(MemoryPressure::Elevated)
    );
    assert_eq!(
        parse_memory_pressure_output(
            "4
"
        ),
        Some(MemoryPressure::Critical)
    );
    assert_eq!(
        parse_memory_pressure_output(
            "1
4
"
        ),
        None
    );
    assert_eq!(
        parse_memory_pressure_output(
            "normal
"
        ),
        None
    );
    assert_eq!(parse_rounded_bytes("-1.0G"), None);
    assert_eq!(parse_rounded_bytes("1.1234567G"), None);
    assert_eq!(parse_elapsed("00:60"), None);
    assert_eq!(parse_elapsed("1-24:00:00"), None);
}
