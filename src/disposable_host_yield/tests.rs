use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super::*;
use crate::process::{CommandExecutor, ExecutionRecord};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

struct TempRoot(PathBuf);

impl TempRoot {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "glaeda-host-yield-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn capacity(&self) -> PathBuf {
        let path = self.0.join(CAPACITY_DIR);
        fs::create_dir_all(&path).unwrap();
        path
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Answers `ps` and `sysctl` from fixtures; a command without one fails.
struct FakeExecutor(BTreeMap<&'static str, (bool, &'static str)>);

impl FakeExecutor {
    fn quiet() -> Self {
        Self(BTreeMap::from([
            (PS_PROGRAM, (true, "/sbin/launchd\n/usr/bin/login\n")),
            (SYSCTL_PROGRAM, (true, "1\n")),
        ]))
    }

    fn with(mut self, program: &'static str, success: bool, stdout: &'static str) -> Self {
        self.0.insert(program, (success, stdout));
        self
    }

    fn without(mut self, program: &'static str) -> Self {
        self.0.remove(program);
        self
    }
}

impl CommandExecutor for FakeExecutor {
    fn execute(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord> {
        let program = spec.program.to_str().unwrap();
        let (success, stdout) = self
            .0
            .get(program)
            .copied()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no fixture"))?;
        Ok(ExecutionRecord {
            argv: spec.displayed_argv(),
            environment_keys: Vec::new(),
            status: Some(i32::from(!success)),
            success,
            stdout: stdout.to_owned(),
            stderr: String::new(),
        })
    }
}

impl TimedCommandExecutor for FakeExecutor {
    fn execute_with_timeout(&self, spec: &CommandSpec, _: Duration) -> io::Result<ExecutionRecord> {
        self.execute(spec)
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn own_pid() -> u32 {
    std::process::id()
}

/// A pid no live process has (pids are below 2^22 on Linux and 99999 on macOS).
const DEAD_PID: u32 = 2_000_000_000;

#[test]
fn quiet_host_yields_nothing() {
    assert_eq!(host_yield_reason(&HostYieldFacts::quiet()), None);
}

#[test]
fn each_held_fact_yields_and_nightly_comes_first() {
    let quiet = HostYieldFacts::quiet();
    let cases = [
        (
            HostYieldFacts {
                nightly_jobs: Some(1),
                ..quiet
            },
            HostYieldReason::NightlyJobRunning { jobs: 1 },
        ),
        (
            HostYieldFacts {
                admission_waiters: Some(2),
                ..quiet
            },
            HostYieldReason::RunnerJobWaiting { jobs: 2 },
        ),
        (
            HostYieldFacts {
                disk_low_jobs: Some(1),
                ..quiet
            },
            HostYieldReason::DiskLow { jobs: 1 },
        ),
        (
            HostYieldFacts {
                memory_pressure_level: Some(2),
                ..quiet
            },
            HostYieldReason::MemoryPressure { level: 2 },
        ),
        (
            HostYieldFacts {
                memory_pressure_level: Some(4),
                ..quiet
            },
            HostYieldReason::MemoryPressure { level: 4 },
        ),
        (
            HostYieldFacts {
                host_lock_held: Some(true),
                ..quiet
            },
            HostYieldReason::HostLockHeld,
        ),
        (
            HostYieldFacts {
                reservation_active: Some(true),
                ..quiet
            },
            HostYieldReason::HostReserved,
        ),
    ];
    for (facts, reason) in cases {
        assert_eq!(host_yield_reason(&facts), Some(reason), "{facts:?}");
    }
    let everything = HostYieldFacts {
        nightly_jobs: Some(1),
        admission_waiters: Some(3),
        disk_low_jobs: Some(1),
        memory_pressure_level: Some(4),
        host_lock_held: Some(true),
        reservation_active: Some(true),
    };
    assert_eq!(
        host_yield_reason(&everything),
        Some(HostYieldReason::NightlyJobRunning { jobs: 1 })
    );
}

#[test]
fn an_unreadable_source_yields_but_a_held_reason_is_named_first() {
    let quiet = HostYieldFacts::quiet();
    assert_eq!(
        host_yield_reason(&HostYieldFacts {
            memory_pressure_level: None,
            ..quiet
        }),
        Some(HostYieldReason::Unobservable {
            source: HostYieldSource::MemoryPressure
        })
    );
    assert_eq!(
        host_yield_reason(&HostYieldFacts::default()),
        Some(HostYieldReason::Unobservable {
            source: HostYieldSource::Processes
        })
    );
    assert_eq!(
        host_yield_reason(&HostYieldFacts {
            nightly_jobs: None,
            host_lock_held: Some(true),
            ..quiet
        }),
        Some(HostYieldReason::HostLockHeld)
    );
}

#[test]
fn reasons_serialize_as_bounded_tagged_receipts() {
    assert_eq!(
        serde_json::to_string(&HostYieldReason::NightlyJobRunning { jobs: 1 }).unwrap(),
        r#"{"reason":"nightly_job_running","jobs":1}"#
    );
    assert_eq!(
        serde_json::to_string(&HostYieldReason::Unobservable {
            source: HostYieldSource::CapacityLedger
        })
        .unwrap(),
        r#"{"reason":"unobservable","source":"capacity_ledger"}"#
    );
}

#[test]
fn nightly_workers_are_the_nightly_slots_and_nightly_paths() {
    let listing = concat!(
        "/sbin/launchd\n",
        "/Users/cmux/actions-runner-glaeda-1/bin/Runner.Listener\n",
        "/Users/cmux/actions-runner-glaeda-2/bin/Runner.Worker\n",
        "/Users/cmux/actions-runner-glaeda-4/bin.2.330.0/Runner.Worker\n",
        "/Users/cmux/actions-runner-glaeda-5/bin/Runner.Worker\n",
        "/Users/cmux/actions-runner-nightly/bin/Runner.Worker\n",
        "/tmp/not-a-runner/bin/Runner.Worker\n",
    );
    let hooks = |runner: &Path| match runner.file_name()?.to_str()? {
        "actions-runner-glaeda-2" => {
            Some("exec hook --instance 2 --canonical-roots 2\n".to_owned())
        }
        "actions-runner-glaeda-4" => Some("exec hook --instance 4 --nightly-runner\n".to_owned()),
        "actions-runner-glaeda-5" => Some("exec hook --trusted-next-signing --x\n".to_owned()),
        _ => None,
    };
    assert_eq!(count_nightly_workers(listing, hooks), Some(3));
    assert_eq!(
        count_nightly_workers("/x/actions-runner-glaeda-2/bin/Runner.Worker\n", |_| None),
        Some(0)
    );
    // A flag word only counts whole: --nightly-runner-x is not the nightly slot.
    assert_eq!(
        count_nightly_workers("/x/actions-runner-glaeda-2/bin/Runner.Worker\n", |_| {
            Some("--nightly-runner-x".to_owned())
        }),
        Some(0)
    );
}

#[test]
fn reservation_is_active_only_while_valid_and_before_until() {
    let marker = |until: &str| {
        format!(
            r#"{{"schema":"glaeda-reservation/v1","owner":"o","purpose":"p","since":1,"until":{until}}}"#
        )
    };
    assert_eq!(reservation_active(&marker("200"), 100), Some(true));
    assert_eq!(reservation_active(&marker("100"), 100), Some(false));
    assert_eq!(reservation_active(&marker("true"), 100), None);
    assert_eq!(reservation_active(&marker("1.5"), 100), None);
    assert_eq!(reservation_active("{}", 100), None);
    assert_eq!(reservation_active("not json", 100), None);
    assert_eq!(
        reservation_active(
            r#"{"schema":"other/v1","owner":"o","purpose":"p","since":1,"until":200}"#,
            100
        ),
        None
    );
}

#[test]
fn a_mac_without_the_runner_hook_yields_nothing() {
    let root = TempRoot::new();
    let missing = root.0.join("no-fleet-dir");
    let source = MacHostYield::new(missing, FakeExecutor::quiet());
    assert_eq!(source.observe(now()), HostYieldFacts::quiet());
    assert_eq!(source.host_yield(), None);
}

#[test]
fn ledger_counts_fresh_live_waiters_and_live_disk_low_markers() {
    let root = TempRoot::new();
    let capacity = root.capacity();
    let mine = own_pid();
    fs::write(capacity.join(format!("{ADMIT_WANT}{mine}")), "{}").unwrap();
    fs::write(capacity.join(format!("{ADMIT_WANT}{DEAD_PID}")), "{}").unwrap();
    fs::write(capacity.join(format!("{ADMIT_WANT}{mine}.tmp")), "{}").unwrap();
    fs::write(capacity.join(format!("{DISK_LOW}{mine}")), "id\n").unwrap();
    fs::write(capacity.join(format!("{DISK_LOW}{DEAD_PID}")), "id\n").unwrap();
    fs::write(capacity.join("disk-low.lock"), "").unwrap();
    fs::write(capacity.join("units.token"), "").unwrap();

    assert_eq!(observe_capacity_ledger(&capacity, now()), Some((1, 1)));
    // A waiter that has not rewritten its marker for ADMIT_FRESH_S is gone.
    assert_eq!(
        observe_capacity_ledger(&capacity, now() + ADMIT_FRESH_SECONDS + 5),
        Some((0, 1))
    );

    let source = MacHostYield::new(root.0.clone(), FakeExecutor::quiet());
    assert_eq!(
        source.host_yield(),
        Some(HostYieldReason::RunnerJobWaiting { jobs: 1 })
    );
}

#[test]
fn an_unreadable_ledger_yields() {
    let root = TempRoot::new();
    fs::write(root.0.join(CAPACITY_DIR), "not a directory").unwrap();
    let source = MacHostYield::new(root.0.clone(), FakeExecutor::quiet());
    assert_eq!(
        source.host_yield(),
        Some(HostYieldReason::Unobservable {
            source: HostYieldSource::CapacityLedger
        })
    );
}

#[test]
fn host_lock_counts_only_an_exclusive_holder() {
    let root = TempRoot::new();
    let path = root.0.join(HOST_LOCK);
    assert_eq!(observe_host_lock(&path), Some(false));
    fs::write(&path, "").unwrap();
    assert_eq!(observe_host_lock(&path), Some(false));

    let shared = fs::File::open(&path).unwrap();
    rustix::fs::flock(&shared, FlockOperation::LockShared).unwrap();
    assert_eq!(
        observe_host_lock(&path),
        Some(false),
        "runner jobs hold it shared"
    );
    drop(shared);

    let exclusive = fs::File::open(&path).unwrap();
    rustix::fs::flock(&exclusive, FlockOperation::LockExclusive).unwrap();
    assert_eq!(observe_host_lock(&path), Some(true));
    let source = MacHostYield::new(root.0.clone(), FakeExecutor::quiet());
    assert_eq!(source.host_yield(), Some(HostYieldReason::HostLockHeld));
    drop(exclusive);
    assert_eq!(observe_host_lock(&path), Some(false));

    let alias = root.0.join("alias.lock");
    symlink(&path, &alias).unwrap();
    assert_eq!(
        observe_host_lock(&alias),
        None,
        "a symlinked lock is not read"
    );
}

#[test]
fn reservation_file_missing_active_and_invalid() {
    let root = TempRoot::new();
    let path = root.0.join(RESERVATION);
    assert_eq!(observe_reservation(&path, 100), Some(false));
    fs::write(
        &path,
        r#"{"schema":"glaeda-reservation/v1","owner":"o","purpose":"p","since":1,"until":200}"#,
    )
    .unwrap();
    assert_eq!(observe_reservation(&path, 100), Some(true));
    assert_eq!(observe_reservation(&path, 300), Some(false));
    fs::write(&path, "garbage").unwrap();
    assert_eq!(observe_reservation(&path, 100), None);
}

#[test]
fn commands_feed_nightly_and_memory_pressure() {
    let root = TempRoot::new();
    let nightly = FakeExecutor::quiet().with(
        PS_PROGRAM,
        true,
        "/Users/cmux/actions-runner-nightly-1/bin/Runner.Worker\n",
    );
    assert_eq!(
        MacHostYield::new(root.0.clone(), nightly).host_yield(),
        Some(HostYieldReason::NightlyJobRunning { jobs: 1 })
    );

    let pressure = FakeExecutor::quiet().with(SYSCTL_PROGRAM, true, "4\n");
    assert_eq!(
        MacHostYield::new(root.0.clone(), pressure).host_yield(),
        Some(HostYieldReason::MemoryPressure { level: 4 })
    );

    let no_ps = FakeExecutor::quiet().without(PS_PROGRAM);
    assert_eq!(
        MacHostYield::new(root.0.clone(), no_ps).host_yield(),
        Some(HostYieldReason::Unobservable {
            source: HostYieldSource::Processes
        })
    );

    let failed_sysctl = FakeExecutor::quiet().with(SYSCTL_PROGRAM, false, "");
    assert_eq!(
        MacHostYield::new(root.0.clone(), failed_sysctl).host_yield(),
        Some(HostYieldReason::Unobservable {
            source: HostYieldSource::MemoryPressure
        })
    );
}

#[test]
fn one_observation_answers_for_the_reuse_window() {
    let root = TempRoot::new();
    let source = MacHostYield::new(root.0.clone(), FakeExecutor::quiet());
    assert_eq!(source.host_yield(), None);
    let capacity = root.capacity();
    fs::write(capacity.join(format!("{ADMIT_WANT}{}", own_pid())), "{}").unwrap();
    assert_eq!(source.host_yield(), None, "reused within OBSERVATION_REUSE");
    source.last.set(None);
    assert_eq!(
        source.host_yield(),
        Some(HostYieldReason::RunnerJobWaiting { jobs: 1 })
    );
}
