//! Host yield: a fleet Mac's own work comes before a disposable Linux worker's next job.
//!
//! While any reason holds, the coordinator advertises zero Scale Set capacity, so the worker takes
//! no new job and starts no new clone. A clone that already has its job keeps it and finishes; this
//! boundary never stops, kills, or resizes anything. The facts are the ones the cmux runner hook
//! already keeps on the Mac (`scripts/glaeda-cmux-runner-hook`), read without taking any of them:
//!
//! - a nightly job runs: a `Runner.Worker` whose runner install is the nightly slot or the nightly
//!   signing runner (its `glaeda-hooks/job-started.sh` carries `--nightly-runner` or
//!   `--trusted-next-signing`, the hook's `runner_scope`), or whose path names `nightly`;
//! - a macOS runner job waits for capacity: a fresh `capacity/admit.want-<pid>` marker (`ADMIT_WANT`)
//!   whose pid lives and that was rewritten within `ADMIT_FRESH_S` (180 s); a nightly job freeing
//!   disk leaves one too (`NIGHTLY_DISK_WANT`);
//! - the disk ledger is low: a `capacity/disk-low-<pid>` marker of a live pid (`DISK_LOW`: a job
//!   admitted under the runner disk floor still runs);
//! - macOS memory pressure is above normal (`kern.memorystatus_vm_pressure_level` > 1);
//! - another build holds the fleet host lock exclusively (`host.lock`, probed with one non-blocking
//!   shared flock as the hook's `host_held` does) or the host is reserved (`reservation.json`,
//!   `glaeda-reservation/v1`, active while now < until).
//!
//! A missing fleet directory, ledger, lock, or reservation yields nothing from that source, so a
//! Mac without the runner hook still runs workers. Any other read failure yields: an unknown Mac is
//! treated as a busy one.

use std::cell::Cell;
use std::fs;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rustix::fs::{FlockOperation, Mode, OFlags};
use rustix::process::Pid;
use serde::Serialize;

use crate::process::{CommandSpec, TimedCommandExecutor};

pub(crate) const DEFAULT_FLEET_DIR: &str = "/Users/Shared/cmux-build-fleet";
const FLEET_DIR_ENV: &str = "GLAEDA_FLEET_DIR";
const CAPACITY_DIR: &str = "capacity";
const ADMIT_WANT: &str = "admit.want-";
const ADMIT_FRESH_SECONDS: u64 = 180;
const DISK_LOW: &str = "disk-low-";
const HOST_LOCK: &str = "host.lock";
const RESERVATION: &str = "reservation.json";
const RESERVATION_SCHEMA: &str = "glaeda-reservation/v1";
const RUNNER_HOOK_SCRIPT: &str = "glaeda-hooks/job-started.sh";
const NIGHTLY_RUNNER_FLAGS: [&str; 2] = ["--nightly-runner", "--trusted-next-signing"];
const RUNNER_WORKER: &str = "Runner.Worker";
const MAX_SMALL_FILE_BYTES: u64 = 64 * 1024;
const MAX_LEDGER_ENTRIES: usize = 4_096;
const MAX_PROCESS_ROWS: usize = 16_384;
const MAX_COUNT: u16 = 999;
const SYSCTL_PROGRAM: &str = "/usr/sbin/sysctl";
const PS_PROGRAM: &str = "/bin/ps";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
/// One observation answers for this long: two `ps`/`sysctl` runs per idle second would be the
/// worker's own load on the Mac it yields to.
const OBSERVATION_REUSE: Duration = Duration::from_secs(5);

/// Why the worker takes no new job right now. Nightly jobs come first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub(crate) enum HostYieldReason {
    NightlyJobRunning { jobs: u16 },
    RunnerJobWaiting { jobs: u16 },
    DiskLow { jobs: u16 },
    MemoryPressure { level: u8 },
    HostLockHeld,
    HostReserved,
    Unobservable { source: HostYieldSource },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HostYieldSource {
    Processes,
    CapacityLedger,
    MemoryPressure,
    HostLock,
    Reservation,
}

/// One reading of the Mac. `None` is a source that could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct HostYieldFacts {
    pub(crate) nightly_jobs: Option<u16>,
    pub(crate) admission_waiters: Option<u16>,
    pub(crate) disk_low_jobs: Option<u16>,
    pub(crate) memory_pressure_level: Option<u8>,
    pub(crate) host_lock_held: Option<bool>,
    pub(crate) reservation_active: Option<bool>,
}

impl HostYieldFacts {
    #[cfg(test)]
    pub(crate) const fn quiet() -> Self {
        Self {
            nightly_jobs: Some(0),
            admission_waiters: Some(0),
            disk_low_jobs: Some(0),
            memory_pressure_level: Some(1),
            host_lock_held: Some(false),
            reservation_active: Some(false),
        }
    }
}

/// The reason the Mac's own work holds new workers off, or `None`. Pure.
///
/// A held reason outranks an unreadable source, so the receipt names the real cause when there is
/// one; an unreadable source still yields.
pub(crate) fn host_yield_reason(facts: &HostYieldFacts) -> Option<HostYieldReason> {
    if let Some(jobs @ 1..) = facts.nightly_jobs {
        return Some(HostYieldReason::NightlyJobRunning { jobs });
    }
    if let Some(jobs @ 1..) = facts.admission_waiters {
        return Some(HostYieldReason::RunnerJobWaiting { jobs });
    }
    if let Some(jobs @ 1..) = facts.disk_low_jobs {
        return Some(HostYieldReason::DiskLow { jobs });
    }
    if let Some(level @ 2..) = facts.memory_pressure_level {
        return Some(HostYieldReason::MemoryPressure { level });
    }
    if facts.host_lock_held == Some(true) {
        return Some(HostYieldReason::HostLockHeld);
    }
    if facts.reservation_active == Some(true) {
        return Some(HostYieldReason::HostReserved);
    }
    let unreadable = [
        (facts.nightly_jobs.is_none(), HostYieldSource::Processes),
        (
            facts.admission_waiters.is_none() || facts.disk_low_jobs.is_none(),
            HostYieldSource::CapacityLedger,
        ),
        (
            facts.memory_pressure_level.is_none(),
            HostYieldSource::MemoryPressure,
        ),
        (facts.host_lock_held.is_none(), HostYieldSource::HostLock),
        (
            facts.reservation_active.is_none(),
            HostYieldSource::Reservation,
        ),
    ];
    unreadable
        .into_iter()
        .find(|(missing, _)| *missing)
        .map(|(_, source)| HostYieldReason::Unobservable { source })
}

pub(crate) trait DisposableHostYieldSource {
    /// Why no new worker may start now, or `None`. Never mutates host state.
    fn host_yield(&self) -> Option<HostYieldReason>;
}

/// No host work to yield to (native Linux workers and tests).
pub(crate) struct NoHostYield;

impl DisposableHostYieldSource for NoHostYield {
    fn host_yield(&self) -> Option<HostYieldReason> {
        None
    }
}

/// Read-only observation of a fleet Mac's runner-hook ledger, processes and memory pressure.
pub(crate) struct MacHostYield<E> {
    fleet_dir: PathBuf,
    executor: E,
    last: Cell<Option<(Instant, Option<HostYieldReason>)>>,
}

impl<E: TimedCommandExecutor> MacHostYield<E> {
    pub(crate) fn new(fleet_dir: PathBuf, executor: E) -> Self {
        Self {
            fleet_dir,
            executor,
            last: Cell::new(None),
        }
    }

    /// The fleet directory the runner hook uses (`GLAEDA_FLEET_DIR`, else the shared default).
    pub(crate) fn from_environment(executor: E) -> Self {
        let fleet_dir = std::env::var_os(FLEET_DIR_ENV)
            .filter(|value| !value.is_empty())
            .map_or_else(|| PathBuf::from(DEFAULT_FLEET_DIR), PathBuf::from);
        Self::new(fleet_dir, executor)
    }

    pub(crate) fn observe(&self, now_unix_seconds: u64) -> HostYieldFacts {
        let capacity = self.fleet_dir.join(CAPACITY_DIR);
        let (admission_waiters, disk_low_jobs) =
            match observe_capacity_ledger(&capacity, now_unix_seconds) {
                Some((waiters, disk_low)) => (Some(waiters), Some(disk_low)),
                None => (None, None),
            };
        HostYieldFacts {
            nightly_jobs: self.observe_nightly_jobs(),
            admission_waiters,
            disk_low_jobs,
            memory_pressure_level: self.observe_memory_pressure(),
            host_lock_held: observe_host_lock(&self.fleet_dir.join(HOST_LOCK)),
            reservation_active: observe_reservation(
                &self.fleet_dir.join(RESERVATION),
                now_unix_seconds,
            ),
        }
    }

    fn observe_nightly_jobs(&self) -> Option<u16> {
        let spec = CommandSpec::new(PS_PROGRAM)
            .argument("-axo")
            .argument("comm=");
        let record = self
            .executor
            .execute_with_timeout(&spec, COMMAND_TIMEOUT)
            .ok()?;
        if !record.success {
            return None;
        }
        count_nightly_workers(&record.stdout, |runner| {
            read_small(&runner.join(RUNNER_HOOK_SCRIPT)).ok()
        })
    }

    fn observe_memory_pressure(&self) -> Option<u8> {
        let spec = CommandSpec::new(SYSCTL_PROGRAM)
            .argument("-n")
            .argument("kern.memorystatus_vm_pressure_level");
        let record = self
            .executor
            .execute_with_timeout(&spec, COMMAND_TIMEOUT)
            .ok()?;
        if !record.success {
            return None;
        }
        record.stdout.trim().parse().ok()
    }
}

impl<E: TimedCommandExecutor> DisposableHostYieldSource for MacHostYield<E> {
    fn host_yield(&self) -> Option<HostYieldReason> {
        if let Some((at, reason)) = self.last.get()
            && at.elapsed() < OBSERVATION_REUSE
        {
            return reason;
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        let reason = host_yield_reason(&self.observe(now));
        self.last.set(Some((Instant::now(), reason)));
        reason
    }
}

/// Running nightly jobs in a `ps -axo comm=` listing. `hook_script` reads one runner install's
/// job-started hook (`None` when it has none, as the hook's `runner_scope` reads it). Pure apart
/// from that reader.
pub(crate) fn count_nightly_workers(
    listing: &str,
    hook_script: impl Fn(&Path) -> Option<String>,
) -> Option<u16> {
    let mut jobs = 0_u16;
    for (row, line) in listing.lines().enumerate() {
        if row >= MAX_PROCESS_ROWS {
            return None;
        }
        let command = line.trim();
        let Some(bin) = command
            .strip_suffix(RUNNER_WORKER)
            .and_then(|rest| rest.strip_suffix('/'))
        else {
            continue;
        };
        let bin = Path::new(bin);
        let runner = bin
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| *name == "bin" || name.starts_with("bin."))
            .and_then(|_| bin.parent());
        let nightly = command.contains("nightly")
            || runner
                .filter(|runner| {
                    runner
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with("actions-runner"))
                })
                .and_then(&hook_script)
                .is_some_and(|text| {
                    text.split_whitespace()
                        .any(|word| NIGHTLY_RUNNER_FLAGS.contains(&word))
                });
        if nightly {
            jobs = jobs.saturating_add(1).min(MAX_COUNT);
        }
    }
    Some(jobs)
}

/// (fresh admission waiters, live disk-low markers) in the capacity ledger; (0, 0) when the Mac
/// has no ledger, `None` when it cannot be read.
fn observe_capacity_ledger(capacity: &Path, now_unix_seconds: u64) -> Option<(u16, u16)> {
    let entries = match fs::read_dir(capacity) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Some((0, 0)),
        Err(_) => return None,
    };
    let mut waiters = 0_u16;
    let mut disk_low = 0_u16;
    for (index, entry) in entries.enumerate() {
        if index >= MAX_LEDGER_ENTRIES {
            return None;
        }
        let Ok(entry) = entry else {
            return None;
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if let Some(pid) = name.strip_prefix(ADMIT_WANT).and_then(ledger_pid) {
            // The hook rewrites its marker every poll; one older than ADMIT_FRESH_S, or one whose
            // waiter is gone, is a waiter that was killed (the hook removes it; we only skip it).
            let fresh = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
                .is_some_and(|modified| {
                    now_unix_seconds.saturating_sub(modified.as_secs()) < ADMIT_FRESH_SECONDS
                });
            if fresh && pid_alive(pid) {
                waiters = waiters.saturating_add(1).min(MAX_COUNT);
            }
        } else if let Some(pid) = name.strip_prefix(DISK_LOW).and_then(ledger_pid)
            && pid_alive(pid)
        {
            disk_low = disk_low.saturating_add(1).min(MAX_COUNT);
        }
    }
    Some((waiters, disk_low))
}

/// A marker's pid: digits only (a `.tmp` being written, `disk-low.lock` and the like are not markers).
fn ledger_pid(suffix: &str) -> Option<i32> {
    if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    suffix.parse().ok().filter(|pid| *pid > 1)
}

fn pid_alive(pid: i32) -> bool {
    let Some(pid) = Pid::from_raw(pid) else {
        return false;
    };
    match rustix::process::test_kill_process(pid) {
        Ok(()) => true,
        Err(error) => error != rustix::io::Errno::SRCH,
    }
}

/// Whether another build holds the fleet host lock exclusively. Runner jobs hold it shared, so one
/// non-blocking shared probe tells the two apart; closing the descriptor drops the probe's lock.
fn observe_host_lock(path: &Path) -> Option<bool> {
    let flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK;
    let fd = match rustix::fs::open(path, flags, Mode::empty()) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT) => return Some(false),
        Err(_) => return None,
    };
    match rustix::fs::flock(&fd, FlockOperation::NonBlockingLockShared) {
        Ok(()) => Some(false),
        Err(rustix::io::Errno::WOULDBLOCK) => Some(true),
        Err(_) => None,
    }
}

/// Whether a `glaeda-reservation/v1` marker holds the host now; an invalid one counts as unreadable
/// (the hook refuses on it too).
fn observe_reservation(path: &Path, now_unix_seconds: u64) -> Option<bool> {
    match read_small(path) {
        Ok(text) => reservation_active(&text, now_unix_seconds),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Some(false),
        Err(_) => None,
    }
}

/// `Some(now < until)` for a valid marker, `None` for an invalid one. Pure.
pub(crate) fn reservation_active(text: &str, now_unix_seconds: u64) -> Option<bool> {
    let document: serde_json::Value = serde_json::from_str(text).ok()?;
    let object = document.as_object()?;
    if object.get("schema")?.as_str()? != RESERVATION_SCHEMA
        || !object.get("owner")?.is_string()
        || !object.get("purpose")?.is_string()
    {
        return None;
    }
    let since = object.get("since")?;
    let until = object.get("until")?;
    if !(since.is_i64() || since.is_u64()) || !(until.is_i64() || until.is_u64()) {
        return None;
    }
    let now = i128::from(now_unix_seconds);
    let until = until
        .as_i64()
        .map(i128::from)
        .or_else(|| until.as_u64().map(i128::from))?;
    Some(now < until)
}

fn read_small(path: &Path) -> io::Result<String> {
    let mut text = String::new();
    fs::File::open(path)?
        .take(MAX_SMALL_FILE_BYTES + 1)
        .read_to_string(&mut text)?;
    if text.len() as u64 > MAX_SMALL_FILE_BYTES {
        return Err(io::Error::other("file exceeds the bound"));
    }
    Ok(text)
}

#[cfg(test)]
mod tests;
