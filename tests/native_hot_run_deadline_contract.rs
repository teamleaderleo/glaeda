#![cfg(target_os = "linux")]

use std::fs;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rustix::io::Errno;
use rustix::process::{Pid, Signal, kill_process, kill_process_group, test_kill_process};
use serde_json::Value;

struct RunningFixture {
    directory: std::path::PathBuf,
    leader_file: std::path::PathBuf,
    wrapper: Option<Child>,
}

impl Drop for RunningFixture {
    fn drop(&mut self) {
        if let Ok(raw) = fs::read_to_string(&self.leader_file)
            && let Ok(raw) = raw.trim().parse::<i32>()
            && let Some(pid) = Pid::from_raw(raw)
        {
            let _ = kill_process_group(pid, Signal::KILL);
        }
        if let Some(wrapper) = self.wrapper.as_mut() {
            let _ = wrapper.kill();
            let _ = wrapper.wait();
        }
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn wait_for_file(path: &std::path::Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while fs::read_to_string(path).is_err() {
        assert!(Instant::now() < deadline, "timed out waiting for {path:?}");
        thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_exit(child: &mut Child, timeout: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(Instant::now() < deadline, "native hot-run did not exit");
        thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_process_absence(pid: Pid, timeout: Duration, label: &str) {
    let deadline = Instant::now() + timeout;
    loop {
        match test_kill_process(pid) {
            Err(Errno::SRCH) => return,
            Ok(()) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            result => panic!("{label} remained live: {result:?}"),
        }
    }
}

fn heavy_user_scope_is_available() -> bool {
    Command::new("/usr/bin/systemd-run")
        .args([
            "--user",
            "--scope",
            "--quiet",
            "--collect",
            "--expand-environment=no",
            "--property",
            "CPUQuota=1200%",
            "--property",
            "MemoryHigh=8G",
            "--property",
            "MemoryMax=12G",
            "--property",
            "TasksMax=1024",
            "/bin/true",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[test]
fn sigint_is_forwarded_to_the_owned_process_group_and_receipted() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "glaeda-native-hot-run-sigint-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&directory).unwrap();
    let leader_file = directory.join("leader.pid");
    let descendant_file = directory.join("descendant.pid");
    let measurement = directory.join("measurement.json");
    let shell = format!(
        "echo $$ > {}; sleep 60 & echo $! > {}; wait",
        leader_file.display(),
        descendant_file.display()
    );
    let repository = env!("CARGO_MANIFEST_DIR");
    let wrapper = Command::new(env!("CARGO_BIN_EXE_glaeda-hot-run"))
        .args([
            "--resident",
            repository,
            "--task",
            repository,
            "--measurement",
            measurement.to_str().unwrap(),
            "--timeout",
            "30",
            "--",
            "/bin/sh",
            "-c",
            &shell,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut fixture = RunningFixture {
        directory,
        leader_file,
        wrapper: Some(wrapper),
    };
    wait_for_file(&descendant_file, Duration::from_secs(3));

    let wrapper_pid = Pid::from_child(fixture.wrapper.as_ref().unwrap());
    kill_process(wrapper_pid, Signal::INT).unwrap();
    let status = wait_for_exit(fixture.wrapper.as_mut().unwrap(), Duration::from_secs(5));
    assert_eq!(status.code(), Some(130));
    fixture.wrapper = None;

    let descendant_pid = fs::read_to_string(&descendant_file)
        .unwrap()
        .trim()
        .parse::<i32>()
        .unwrap();
    let descendant_pid = Pid::from_raw(descendant_pid).unwrap();
    wait_for_process_absence(
        descendant_pid,
        Duration::from_secs(2),
        "interrupted descendant",
    );

    let report: Value = serde_json::from_reader(fs::File::open(&measurement).unwrap()).unwrap();
    assert_eq!(report["timeout_seconds"], 30.0);
    assert_eq!(report["exit_code"], 130);
    assert_eq!(report["signal"], signal_hook::consts::signal::SIGKILL);
    assert_eq!(report["completion_reason"], "operator_interrupt");
    assert_eq!(
        report["resource_accounting"],
        "unavailable_after_forced_termination"
    );
}

#[test]
fn profiled_deadline_terminates_the_scoped_process_group_and_receipts_it() {
    if !heavy_user_scope_is_available() {
        return;
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "glaeda-native-hot-run-profile-deadline-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&directory).unwrap();
    let group_file = directory.join("group.pid");
    let descendant_file = directory.join("descendant.pid");
    let measurement = directory.join("measurement.json");
    let shell = format!(
        "/usr/bin/awk '{{ print $5 }}' /proc/self/stat > {}; \
         sleep 60 & echo $! > {}; wait",
        group_file.display(),
        descendant_file.display()
    );
    let repository = env!("CARGO_MANIFEST_DIR");
    let wrapper = Command::new(env!("CARGO_BIN_EXE_glaeda-hot-run"))
        .args([
            "--resident",
            repository,
            "--task",
            repository,
            "--resource-profile",
            "big-red-heavy",
            "--measurement",
            measurement.to_str().unwrap(),
            "--timeout",
            "0.2",
            "--",
            "/bin/sh",
            "-c",
            &shell,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut fixture = RunningFixture {
        directory,
        leader_file: group_file,
        wrapper: Some(wrapper),
    };
    wait_for_file(&descendant_file, Duration::from_secs(3));

    let status = wait_for_exit(fixture.wrapper.as_mut().unwrap(), Duration::from_secs(5));
    assert_eq!(status.code(), Some(124));
    fixture.wrapper = None;

    let descendant_pid = fs::read_to_string(&descendant_file)
        .unwrap()
        .trim()
        .parse::<i32>()
        .unwrap();
    wait_for_process_absence(
        Pid::from_raw(descendant_pid).unwrap(),
        Duration::from_secs(2),
        "profiled deadline descendant",
    );

    let report: Value = serde_json::from_reader(fs::File::open(&measurement).unwrap()).unwrap();
    assert_eq!(report["resource_profile"], "big-red-heavy");
    assert_eq!(report["timeout_seconds"], 0.2);
    assert_eq!(report["exit_code"], 124);
    assert_eq!(report["signal"], signal_hook::consts::signal::SIGTERM);
    assert_eq!(report["completion_reason"], "deadline_exceeded");
    assert_eq!(
        report["resource_accounting"],
        "unavailable_after_forced_termination"
    );
}
