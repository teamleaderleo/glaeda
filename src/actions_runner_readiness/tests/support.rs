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
                guest_machine_id_digest: Sha256Digest::parse(&format!("sha256:{}", "a".repeat(64)))
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
