//! Low-fixed-cost measurement front door for ultra-trusted same-worktree commands.
//!
//! This deliberately owns only direct native execution. Cross-worktree cache views and source
//! composition remain in `scripts/hot-run` until their existing semantics are migrated intact.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{
    DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
};
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::Parser;
#[cfg(target_os = "linux")]
use rustix::thread::sched_getaffinity;
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};

const MAX_OBSERVATION_BYTES: u64 = 64 * 1024;
const SHA256_PREFIX: &str = "sha256:";
const GIT_OVERRIDE_NAMES: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
];
const PRESSURE_KINDS: &[&str] = &["cpu", "memory", "io"];

#[derive(Debug, Parser)]
#[command(
    name = "glaeda-hot-run",
    about = "Measure an ultra-trusted command directly in one native Git worktree"
)]
struct Cli {
    /// Resident Git worktree root. It must be the task's physical worktree root.
    #[arg(long)]
    resident: PathBuf,
    /// Task working directory within the resident Git worktree.
    #[arg(long)]
    task: PathBuf,
    /// Record one resident-relative cache path as direct native state.
    #[arg(long, action = clap::ArgAction::Append)]
    cache: Vec<String>,
    /// Atomically write one bounded schema-v6 developer observation.
    #[arg(long)]
    measurement: Option<PathBuf>,
    /// Caller-owned exact-work comparison digest recorded only in a measurement.
    #[arg(long)]
    comparison_key: Option<String>,
    /// Bounded public identity for the resolved runtime executable.
    #[arg(long)]
    runtime_id: Option<String>,
    /// Optional expected digest for the resolved runtime executable.
    #[arg(long)]
    runtime_sha256: Option<String>,
    /// Canonical toolchain bin directory placed first in descendant PATH.
    #[arg(long)]
    runtime_bin: Option<PathBuf>,
    /// Absolute executable or PATH-resolved command followed by its arguments.
    #[arg(last = true, required = true)]
    command: Vec<OsString>,
}

#[derive(Debug)]
struct NativeCache {
    path: String,
}

#[derive(Debug)]
struct RuntimeDeclaration {
    id: String,
    expected_program_sha256: Option<String>,
}

#[derive(Debug)]
struct RuntimeBinBinding {
    path: PathBuf,
    identity_sha256: String,
}

#[derive(Debug)]
struct RuntimeContract {
    id: String,
    program_sha256: String,
    runtime_bin_binding_sha256: Option<String>,
}

#[derive(Debug)]
struct CommandResult {
    elapsed: Duration,
    user_cpu_seconds: Option<f64>,
    system_cpu_seconds: Option<f64>,
    max_rss_kib: Option<u64>,
    resource_accounting: &'static str,
    exit_code: i32,
    signal: Option<i32>,
    completion_reason: &'static str,
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(255)),
        Err(error) => {
            eprintln!("glaeda-hot-run error: {error}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: Cli) -> Result<i32, String> {
    if !cfg!(target_os = "linux") {
        return Err("native hot-run execution currently requires Linux".into());
    }
    if cli.comparison_key.is_some() && cli.measurement.is_none() {
        return Err("--comparison-key requires --measurement".into());
    }
    if let Some(key) = cli.comparison_key.as_deref() {
        validate_comparison_key(key)?;
    }
    let runtime_declaration =
        parse_runtime_contract(cli.runtime_id.as_deref(), cli.runtime_sha256.as_deref())?;
    let runtime_bin = observe_runtime_bin(
        cli.runtime_bin.as_deref(),
        runtime_declaration
            .as_ref()
            .map(|declaration| declaration.id.as_str()),
    )?;
    for name in GIT_OVERRIDE_NAMES {
        if env::var_os(name).is_some() {
            return Err(format!("Git environment override is unsupported: {name}"));
        }
    }

    let caches = cli
        .cache
        .iter()
        .map(|value| parse_native_cache(value))
        .collect::<Result<Vec<_>, _>>()?;
    validate_native_caches(&caches)?;
    let resident = cli
        .resident
        .canonicalize()
        .map_err(|error| format!("resident worktree is unavailable: {error}"))?;
    let task_cwd = cli
        .task
        .canonicalize()
        .map_err(|error| format!("task working directory is unavailable: {error}"))?;
    if !resident.is_dir() || !task_cwd.is_dir() {
        return Err("resident and task must be directories".into());
    }
    let git = resolve_program(OsStr::new("git"), &task_cwd, None)?;
    let task_root = observe_git_root(&git, &task_cwd)?;
    if task_root != resident {
        return Err("resident must be the task's physical Git worktree root".into());
    }
    let bound_path = runtime_bin.as_ref().map(runtime_environment_path);
    let command = resolve_command(&cli.command, &task_cwd, bound_path.as_deref())?;
    let runtime_contract = verify_runtime_contract(
        runtime_declaration.as_ref(),
        Path::new(&command[0]),
        runtime_bin.as_ref(),
    )?;

    let machine_before = cli.measurement.as_ref().map(|_| observe_machine());
    let result = execute_command(
        &command,
        &task_cwd,
        bound_path.as_deref(),
        cli.measurement.is_some(),
    )?;
    if let Some(destination) = cli.measurement.as_ref() {
        let machine_after = observe_machine();
        write_measurement(
            destination,
            &result,
            &caches,
            runtime_contract.as_ref(),
            cli.comparison_key.as_deref(),
            machine_before.expect("measurement observation exists"),
            machine_after,
        )?;
    }
    Ok(result.exit_code)
}

fn validate_comparison_key(value: &str) -> Result<(), String> {
    let Some(digest) = value.strip_prefix(SHA256_PREFIX) else {
        return Err("comparison key must be canonical SHA-256".into());
    };
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("comparison key must be canonical SHA-256".into());
    }
    Ok(())
}

fn parse_runtime_contract(
    runtime_id: Option<&str>,
    program_sha256: Option<&str>,
) -> Result<Option<RuntimeDeclaration>, String> {
    let Some(runtime_id) = runtime_id else {
        if program_sha256.is_some() {
            return Err("runtime executable digest requires a runtime ID".into());
        }
        return Ok(None);
    };
    let valid_id = !runtime_id.is_empty()
        && runtime_id.len() <= 96
        && runtime_id.is_ascii()
        && runtime_id
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && runtime_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        && !runtime_id.contains("..");
    if !valid_id {
        return Err("runtime ID must be bounded safe ASCII".into());
    }
    if let Some(digest) = program_sha256 {
        validate_sha256(digest)
            .map_err(|_| "runtime executable digest must be canonical SHA-256".to_owned())?;
    }
    Ok(Some(RuntimeDeclaration {
        id: runtime_id.to_owned(),
        expected_program_sha256: program_sha256.map(str::to_owned),
    }))
}

fn validate_sha256(value: &str) -> Result<(), ()> {
    let digest = value.strip_prefix(SHA256_PREFIX).ok_or(())?;
    if digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(())
    }
}

fn observe_runtime_bin(
    runtime_bin: Option<&Path>,
    runtime_id: Option<&str>,
) -> Result<Option<RuntimeBinBinding>, String> {
    let Some(path) = runtime_bin else {
        return Ok(None);
    };
    if runtime_id.is_none() {
        return Err("runtime bin binding requires a runtime ID".into());
    }
    if !path.is_absolute() {
        return Err("runtime bin binding must be an absolute canonical path".into());
    }
    let details =
        fs::symlink_metadata(path).map_err(|_| "runtime bin binding is unavailable".to_owned())?;
    let resolved = path
        .canonicalize()
        .map_err(|_| "runtime bin binding is unavailable".to_owned())?;
    if details.file_type().is_symlink() || !details.is_dir() {
        return Err("runtime bin binding is not a plain directory".into());
    }
    if resolved != path {
        return Err("runtime bin binding contains a symbolic-link component".into());
    }
    Ok(Some(RuntimeBinBinding {
        path: path.to_owned(),
        identity_sha256: runtime_bin_identity(path, &details),
    }))
}

fn runtime_bin_identity(path: &Path, details: &fs::Metadata) -> String {
    let mut digest = Sha256::new();
    digest.update(b"glaeda-hot-run-runtime-bin-v1");
    digest.update(b"\0");
    digest.update(path.as_os_str().as_bytes());
    let mtime_ns = i128::from(details.mtime()) * 1_000_000_000 + i128::from(details.mtime_nsec());
    let ctime_ns = i128::from(details.ctime()) * 1_000_000_000 + i128::from(details.ctime_nsec());
    for value in [
        u128::from(details.dev()),
        u128::from(details.ino()),
        u128::from(details.uid()),
        u128::from(details.gid()),
        u128::from(details.mode() & 0o7777),
        u128::from(details.nlink()),
        u128::from(details.size()),
    ] {
        digest.update(b"\0");
        digest.update(value.to_string().as_bytes());
    }
    for value in [mtime_ns, ctime_ns] {
        digest.update(b"\0");
        digest.update(value.to_string().as_bytes());
    }
    format!("sha256:{:x}", digest.finalize())
}

fn revalidate_runtime_bin(binding: &RuntimeBinBinding) -> Result<(), String> {
    let current = observe_runtime_bin(Some(&binding.path), Some("revalidate"))?
        .expect("runtime bin observation exists");
    if current.identity_sha256 != binding.identity_sha256 {
        return Err("runtime bin binding changed during preflight".into());
    }
    Ok(())
}

fn runtime_environment_path(binding: &RuntimeBinBinding) -> OsString {
    let mut path = binding.path.as_os_str().to_os_string();
    if let Some(inherited) = env::var_os("PATH").filter(|value| !value.is_empty()) {
        path.push(OsStr::new(":"));
        path.push(inherited);
    }
    path
}

fn verify_runtime_contract(
    declaration: Option<&RuntimeDeclaration>,
    program: &Path,
    runtime_bin: Option<&RuntimeBinBinding>,
) -> Result<Option<RuntimeContract>, String> {
    let Some(declaration) = declaration else {
        return Ok(None);
    };
    if let Some(binding) = runtime_bin {
        if program.parent() != Some(binding.path.as_path()) {
            return Err("runtime executable is outside the bound runtime bin".into());
        }
        revalidate_runtime_bin(binding)?;
    }
    let observed = sha256_file(program)?;
    if declaration
        .expected_program_sha256
        .as_ref()
        .is_some_and(|expected| expected != &observed)
    {
        return Err("runtime executable content does not match declared digest".into());
    }
    if let Some(binding) = runtime_bin {
        revalidate_runtime_bin(binding)?;
    }
    Ok(Some(RuntimeContract {
        id: declaration.id.clone(),
        program_sha256: observed,
        runtime_bin_binding_sha256: runtime_bin.map(|binding| binding.identity_sha256.clone()),
    }))
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut executable =
        File::open(path).map_err(|error| format!("cannot read runtime executable: {error}"))?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = executable
            .read(&mut buffer)
            .map_err(|error| format!("cannot read runtime executable: {error}"))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("sha256:{:x}", digest.finalize()))
}

fn parse_native_cache(value: &str) -> Result<NativeCache, String> {
    let Some(path) = value.strip_suffix(":native") else {
        return Err("native front door accepts only PATH:native cache declarations".into());
    };
    if path.is_empty() || path.contains('\0') || path.contains("//") {
        return Err("cache path must be a nonempty normalized relative path".into());
    }
    let parsed = Path::new(path);
    if parsed.is_absolute()
        || parsed
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("cache path must be a nonempty normalized relative path".into());
    }
    Ok(NativeCache {
        path: path.to_owned(),
    })
}

fn validate_native_caches(caches: &[NativeCache]) -> Result<(), String> {
    let paths = caches
        .iter()
        .map(|cache| Path::new(&cache.path))
        .collect::<Vec<_>>();
    if paths.iter().copied().collect::<BTreeSet<_>>().len() != paths.len() {
        return Err("cache paths must be unique".into());
    }
    for (index, left) in paths.iter().enumerate() {
        for right in &paths[index + 1..] {
            if left.starts_with(right) || right.starts_with(left) {
                return Err("cache paths must not overlap".into());
            }
        }
    }
    Ok(())
}

fn resolve_command(
    command: &[OsString],
    cwd: &Path,
    search_path: Option<&OsStr>,
) -> Result<Vec<OsString>, String> {
    let requested = command
        .first()
        .ok_or_else(|| "a command is required after --".to_owned())?;
    let program = resolve_program(requested, cwd, search_path)?;
    Ok(std::iter::once(program.into_os_string())
        .chain(command.iter().skip(1).cloned())
        .collect())
}

fn resolve_program(
    requested: &OsStr,
    cwd: &Path,
    search_path: Option<&OsStr>,
) -> Result<PathBuf, String> {
    let requested_path = Path::new(requested);
    let candidate = if requested_path.components().count() > 1 || requested_path.is_absolute() {
        if requested_path.is_absolute() {
            requested_path.to_owned()
        } else {
            cwd.join(requested_path)
        }
    } else {
        let inherited_path;
        let path = match search_path {
            Some(path) => path,
            None => {
                inherited_path = env::var_os("PATH").unwrap_or_default();
                &inherited_path
            }
        };
        env::split_paths(path)
            .map(|directory| directory.join(requested_path))
            .find(|path| is_executable_file(path))
            .ok_or_else(|| format!("command is unavailable: {}", requested.to_string_lossy()))?
    };
    let absolute = if candidate.is_absolute() {
        candidate
    } else {
        cwd.join(candidate)
    };
    if !is_executable_file(&absolute) {
        return Err(format!(
            "command is not executable: {}",
            requested.to_string_lossy()
        ));
    }
    Ok(absolute)
}

fn is_executable_file(path: &Path) -> bool {
    path.metadata()
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn observe_git_root(git: &Path, task: &Path) -> Result<PathBuf, String> {
    let output = Command::new(git)
        .args([
            OsStr::new("-C"),
            task.as_os_str(),
            OsStr::new("rev-parse"),
            OsStr::new("--path-format=absolute"),
            OsStr::new("--show-toplevel"),
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|error| format!("cannot inspect task Git worktree: {error}"))?;
    if !output.status.success() || output.stdout.len() > 4096 {
        return Err("task is not a bounded Git worktree".into());
    }
    let raw = std::str::from_utf8(&output.stdout)
        .map_err(|_| "Git worktree root is not UTF-8".to_owned())?
        .trim_end_matches(['\r', '\n']);
    if raw.is_empty() || raw.contains('\n') || raw.contains('\r') {
        return Err("Git returned an invalid worktree root".into());
    }
    Path::new(raw)
        .canonicalize()
        .map_err(|error| format!("Git worktree root is unavailable: {error}"))
}

fn execute_command(
    command: &[OsString],
    cwd: &Path,
    environment_path: Option<&OsStr>,
    measured: bool,
) -> Result<CommandResult, String> {
    let mut time_report = None;
    let arguments = if measured {
        let gnu_time = resolve_program(OsStr::new("time"), cwd, None)?;
        let report = unique_temporary_path("glaeda-hot-run-time")?;
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&report)
            .map_err(|error| format!("cannot create resource observation: {error}"))?;
        let wrapped = [
            vec![
                gnu_time.into_os_string(),
                OsString::from("--quiet"),
                OsString::from("--format"),
                OsString::from("%U\n%S\n%M\n%x"),
                OsString::from("--output"),
                report.clone().into_os_string(),
            ],
            command.to_vec(),
        ]
        .concat();
        time_report = Some(report);
        wrapped
    } else {
        command.to_vec()
    };

    let started = Instant::now();
    let mut process = Command::new(&arguments[0]);
    process.args(&arguments[1..]).current_dir(cwd);
    if let Some(path) = environment_path {
        process.env("PATH", path);
    }
    let status = process.status().map_err(|error| {
        if let Some(path) = time_report.as_ref() {
            let _ = fs::remove_file(path);
        }
        format!("cannot launch command: {error}")
    })?;
    let elapsed = started.elapsed();

    let timed_usage = time_report
        .as_ref()
        .and_then(|path| parse_time_report(path).ok());
    if let Some(path) = time_report.as_ref() {
        let _ = fs::remove_file(path);
    }
    let raw_code = status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
    let time_exit = timed_usage.as_ref().map(|usage| usage.3);
    let (exit_code, signal, completion_reason) = if let Some(signal) = status.signal() {
        (128 + signal, Some(signal), "signaled")
    } else if time_exit == Some(0)
        && (128..=255).contains(&raw_code)
        && valid_signal(raw_code - 128)
    {
        (raw_code, Some(raw_code - 128), "signaled")
    } else {
        (raw_code, None, "exited")
    };
    let (user_cpu_seconds, system_cpu_seconds, max_rss_kib, resource_accounting) = match timed_usage
    {
        Some((user, system, rss, _)) => {
            (Some(user), Some(system), Some(rss), "gnu_time_command_tree")
        }
        None if measured => (None, None, None, "unavailable_for_measured_command"),
        None => (None, None, None, "not_measured"),
    };
    Ok(CommandResult {
        elapsed,
        user_cpu_seconds,
        system_cpu_seconds,
        max_rss_kib,
        resource_accounting,
        exit_code,
        signal,
        completion_reason,
    })
}

fn valid_signal(signal: i32) -> bool {
    (1..=31).contains(&signal) || (34..=64).contains(&signal)
}

fn parse_time_report(path: &Path) -> Result<(f64, f64, u64, i32), String> {
    let raw = read_bounded(path)?;
    let fields = raw.lines().collect::<Vec<_>>();
    if fields.len() != 4 {
        return Err("resource observation is incomplete".into());
    }
    let user = fields[0]
        .parse::<f64>()
        .map_err(|_| "resource user CPU is invalid".to_owned())?;
    let system = fields[1]
        .parse::<f64>()
        .map_err(|_| "resource system CPU is invalid".to_owned())?;
    let rss = fields[2]
        .parse::<u64>()
        .map_err(|_| "resource peak RSS is invalid".to_owned())?;
    let exit = fields[3]
        .parse::<i32>()
        .map_err(|_| "resource exit status is invalid".to_owned())?;
    if !user.is_finite() || user < 0.0 || !system.is_finite() || system < 0.0 {
        return Err("resource CPU observation is out of range".into());
    }
    Ok((user, system, rss, exit))
}

fn observe_machine() -> Value {
    let started = Instant::now();
    let online = optional_online_cpu_count();
    let allowed = allowed_cpu_count();
    let load = optional_observation(Path::new("/proc/loadavg"), parse_load_average);
    let memory = optional_observation(Path::new("/proc/meminfo"), parse_meminfo);
    let mut pressure = Map::new();
    for kind in PRESSURE_KINDS {
        pressure.insert(
            (*kind).to_owned(),
            optional_observation(&Path::new("/proc/pressure").join(kind), parse_pressure)
                .unwrap_or(Value::Null),
        );
    }
    let present = usize::from(online.is_some())
        + usize::from(allowed.is_some())
        + usize::from(load.is_some())
        + usize::from(memory.is_some())
        + pressure.values().filter(|value| !value.is_null()).count();
    let status = if present == 7 {
        "observed"
    } else if present > 0 {
        "partial"
    } else {
        "unavailable"
    };
    json!({
        "status": status,
        "observation_elapsed_seconds": round_seconds(started.elapsed()),
        "online_logical_cpus": online,
        "allowed_logical_cpus": allowed,
        "load_average": load,
        "memory": memory,
        "pressure": pressure,
    })
}

fn optional_online_cpu_count() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        read_bounded(Path::new("/sys/devices/system/cpu/online"))
            .ok()
            .and_then(|raw| parse_cpu_list(&raw).ok())
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

fn allowed_cpu_count() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        sched_getaffinity(None)
            .ok()
            .map(|set| u64::from(set.count()))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg(any(target_os = "linux", test))]
fn parse_cpu_list(raw: &str) -> Result<u64, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("online CPU observation is empty".into());
    }
    let mut count = 0_u64;
    let mut prior_end = None;
    for span in raw.split(',') {
        let (start, end) = match span.split_once('-') {
            Some((start, end)) => (
                start
                    .parse::<u64>()
                    .map_err(|_| "online CPU observation is invalid".to_owned())?,
                end.parse::<u64>()
                    .map_err(|_| "online CPU observation is invalid".to_owned())?,
            ),
            None => {
                let value = span
                    .parse::<u64>()
                    .map_err(|_| "online CPU observation is invalid".to_owned())?;
                (value, value)
            }
        };
        if end < start || prior_end.is_some_and(|prior| start <= prior) {
            return Err("online CPU observation is overlapping or unordered".into());
        }
        count = count
            .checked_add(end - start + 1)
            .ok_or_else(|| "online CPU observation is out of range".to_owned())?;
        prior_end = Some(end);
    }
    Ok(count)
}

fn optional_observation(path: &Path, parser: fn(&str) -> Result<Value, String>) -> Option<Value> {
    read_bounded(path).ok().and_then(|raw| parser(&raw).ok())
}

fn read_bounded(path: &Path) -> Result<String, String> {
    let mut source = File::open(path).map_err(|error| error.to_string())?;
    let mut raw = String::new();
    std::io::Read::by_ref(&mut source)
        .take(MAX_OBSERVATION_BYTES + 1)
        .read_to_string(&mut raw)
        .map_err(|error| error.to_string())?;
    if raw.len() as u64 > MAX_OBSERVATION_BYTES {
        return Err("observation exceeds the bounded input size".into());
    }
    if !raw.is_ascii() {
        return Err("observation is not ASCII".into());
    }
    Ok(raw)
}

fn parse_nonnegative_finite(raw: &str) -> Result<f64, String> {
    let value = raw
        .parse::<f64>()
        .map_err(|_| "value is not numeric".to_owned())?;
    if !value.is_finite() || value < 0.0 {
        return Err("value is not finite and nonnegative".into());
    }
    Ok(value)
}

fn parse_load_average(raw: &str) -> Result<Value, String> {
    let fields = raw.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 3 {
        return Err("load-average observation is incomplete".into());
    }
    Ok(json!({
        "one_minute": parse_nonnegative_finite(fields[0])?,
        "five_minutes": parse_nonnegative_finite(fields[1])?,
        "fifteen_minutes": parse_nonnegative_finite(fields[2])?,
    }))
}

fn parse_meminfo(raw: &str) -> Result<Value, String> {
    let mut values = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for line in raw.lines() {
        let Some((key, remainder)) = line.split_once(':') else {
            return Err("memory observation contains malformed fields".into());
        };
        if !seen.insert(key) {
            return Err("memory observation contains duplicate fields".into());
        }
        let fields = remainder.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 2 || fields[1] != "kB" {
            continue;
        }
        let kib = fields[0]
            .parse::<u64>()
            .map_err(|_| "memory observation value is out of range".to_owned())?;
        values.insert(
            key,
            kib.checked_mul(1024)
                .ok_or_else(|| "memory observation value is out of range".to_owned())?,
        );
    }
    let available = *values
        .get("MemAvailable")
        .ok_or_else(|| "memory observation is incomplete".to_owned())?;
    let swap_total = *values
        .get("SwapTotal")
        .ok_or_else(|| "memory observation is incomplete".to_owned())?;
    let swap_free = *values
        .get("SwapFree")
        .ok_or_else(|| "memory observation is incomplete".to_owned())?;
    let swap_used = swap_total
        .checked_sub(swap_free)
        .ok_or_else(|| "memory observation has inconsistent swap totals".to_owned())?;
    Ok(json!({
        "available_bytes": available,
        "swap_total_bytes": swap_total,
        "swap_used_bytes": swap_used,
    }))
}

fn parse_pressure(raw: &str) -> Result<Value, String> {
    let mut result = Map::new();
    for line in raw.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 5 || !matches!(fields[0], "some" | "full") {
            return Err("pressure observation has an unsupported shape".into());
        }
        if result.contains_key(fields[0]) {
            return Err("pressure observation contains duplicate classes".into());
        }
        let mut pairs = BTreeMap::new();
        for field in &fields[1..] {
            let Some((key, value)) = field.split_once('=') else {
                return Err("pressure observation contains malformed fields".into());
            };
            if pairs.insert(key, value).is_some() {
                return Err("pressure observation contains duplicate fields".into());
            }
        }
        if pairs.keys().copied().collect::<BTreeSet<_>>()
            != BTreeSet::from(["avg10", "avg60", "avg300", "total"])
        {
            return Err("pressure observation is incomplete".into());
        }
        let total = pairs["total"]
            .parse::<u64>()
            .map_err(|_| "pressure total is not a nonnegative integer".to_owned())?;
        result.insert(
            fields[0].to_owned(),
            json!({
                "avg10": parse_nonnegative_finite(pairs["avg10"] )?,
                "avg60": parse_nonnegative_finite(pairs["avg60"] )?,
                "avg300": parse_nonnegative_finite(pairs["avg300"] )?,
                "total_microseconds": total,
            }),
        );
    }
    if !result.contains_key("some") {
        return Err("pressure observation does not contain the some class".into());
    }
    Ok(Value::Object(result))
}

fn nested_i128(value: &Value, keys: &[&str]) -> Option<i128> {
    let mut current = value;
    for key in keys {
        current = current.get(*key)?;
    }
    current.as_u64().map(i128::from)
}

fn pressure_interval(before: &Value, after: &Value, elapsed: Duration) -> Value {
    let elapsed_seconds = elapsed.as_secs_f64();
    let mut pressure = Map::new();
    for kind in PRESSURE_KINDS {
        let mut classes = Map::new();
        for class in ["some", "full"] {
            let keys = ["pressure", *kind, class, "total_microseconds"];
            let before_total = nested_i128(before, &keys);
            let after_total = nested_i128(after, &keys);
            let delta = before_total
                .zip(after_total)
                .and_then(|(before, after)| (after >= before).then_some(after - before));
            let fraction = delta.and_then(|value| {
                (elapsed_seconds > 0.0)
                    .then_some(round_to(value as f64 / (elapsed_seconds * 1_000_000.0), 9))
            });
            classes.insert(
                class.to_owned(),
                json!({
                    "total_microseconds_delta": delta,
                    "stall_fraction_of_command_elapsed": fraction,
                }),
            );
        }
        pressure.insert((*kind).to_owned(), Value::Object(classes));
    }
    let available_delta = signed_delta(before, after, &["memory", "available_bytes"]);
    let swap_delta = signed_delta(before, after, &["memory", "swap_used_bytes"]);
    json!({
        "duration_basis": "command_elapsed",
        "elapsed_seconds": round_seconds(elapsed),
        "memory": {
            "available_bytes_delta": available_delta,
            "swap_used_bytes_delta": swap_delta,
        },
        "pressure": pressure,
    })
}

fn signed_delta(before: &Value, after: &Value, keys: &[&str]) -> Option<i128> {
    nested_i128(after, keys)
        .zip(nested_i128(before, keys))
        .map(|(after, before)| after - before)
}

fn write_measurement(
    destination: &Path,
    result: &CommandResult,
    caches: &[NativeCache],
    runtime: Option<&RuntimeContract>,
    comparison_key: Option<&str>,
    machine_before: Value,
    machine_after: Value,
) -> Result<(), String> {
    let destination = absolute_path(destination)?;
    let parent = destination
        .parent()
        .ok_or_else(|| "measurement destination has no parent".to_owned())?;
    let mut builder = DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder
        .create(parent)
        .map_err(|error| format!("cannot create measurement parent: {error}"))?;
    let temporary = unique_sibling_path(&destination)?;
    let report = json!({
        "schema_version": 6,
        "document_type": "glaeda-hot-run-measurement",
        "authority": "developer_observation_only",
        "comparison_key": comparison_key,
        "cross_worktree": false,
        "resource_profile": Value::Null,
        "machine_observation": {
            "scope": "host_aggregate",
            "before": machine_before,
            "after": machine_after,
            "interval": pressure_interval(&machine_before, &machine_after, result.elapsed),
        },
        "timeout_seconds": Value::Null,
        "cache_views": caches.iter().map(|cache| json!({"path": cache.path, "mode": "native"})).collect::<Vec<_>>(),
        "state_preparation": [],
        "source_preparation": Value::Null,
        "runtime": runtime.map(runtime_report).unwrap_or(Value::Null),
        "elapsed_seconds": round_seconds(result.elapsed),
        "preparation_elapsed_seconds": 0.0,
        "command_plus_preparation_elapsed_seconds": round_seconds(result.elapsed),
        "user_cpu_seconds": result.user_cpu_seconds.map(|value| round_to(value, 6)),
        "system_cpu_seconds": result.system_cpu_seconds.map(|value| round_to(value, 6)),
        "max_rss_kib": result.max_rss_kib,
        "resource_accounting": result.resource_accounting,
        "exit_code": result.exit_code,
        "signal": result.signal,
        "completion_reason": result.completion_reason,
    });
    let write_result = (|| -> Result<(), String> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|error| format!("cannot create measurement temporary file: {error}"))?;
        serde_json::to_writer(&mut file, &report)
            .map_err(|error| format!("cannot encode measurement: {error}"))?;
        file.write_all(b"\n")
            .map_err(|error| format!("cannot finish measurement: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("cannot persist measurement: {error}"))?;
        fs::rename(&temporary, &destination)
            .map_err(|error| format!("cannot publish measurement: {error}"))?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

fn runtime_report(runtime: &RuntimeContract) -> Value {
    let mut report = Map::from_iter([
        ("id".to_owned(), Value::String(runtime.id.clone())),
        (
            "program_sha256".to_owned(),
            Value::String(runtime.program_sha256.clone()),
        ),
    ]);
    if let Some(binding) = runtime.runtime_bin_binding_sha256.as_ref() {
        report.insert(
            "descendant_path".to_owned(),
            Value::String("runtime_bin_first".into()),
        );
        report.insert(
            "runtime_bin_binding_sha256".to_owned(),
            Value::String(binding.clone()),
        );
    }
    Value::Object(report)
}

fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        env::current_dir()
            .map(|cwd| cwd.join(path))
            .map_err(|error| format!("cannot resolve measurement destination: {error}"))
    }
}

fn unique_temporary_path(prefix: &str) -> Result<PathBuf, String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch".to_owned())?
        .as_nanos();
    Ok(env::temp_dir().join(format!(".{prefix}-{}-{nonce}", std::process::id())))
}

fn unique_sibling_path(destination: &Path) -> Result<PathBuf, String> {
    let parent = destination
        .parent()
        .ok_or_else(|| "measurement destination has no parent".to_owned())?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch".to_owned())?
        .as_nanos();
    Ok(parent.join(format!(
        ".glaeda-hot-run-measurement-{}-{nonce}",
        std::process::id()
    )))
}

fn round_seconds(duration: Duration) -> f64 {
    round_to(duration.as_secs_f64(), 6)
}

fn round_to(value: f64, places: i32) -> f64 {
    let factor = 10_f64.powi(places);
    (value * factor).round() / factor
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn test_directory(label: &str) -> PathBuf {
        let path = unique_temporary_path(label).unwrap();
        fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn comparison_keys_are_canonical() {
        assert!(validate_comparison_key(&format!("sha256:{}", "a".repeat(64))).is_ok());
        assert!(validate_comparison_key(&format!("sha256:{}", "A".repeat(64))).is_err());
        assert!(validate_comparison_key("sha256:1234").is_err());
    }

    #[test]
    fn runtime_declarations_are_bounded_and_digest_bound() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let declaration = parse_runtime_contract(Some("node-v26.8.1"), Some(&digest))
            .unwrap()
            .unwrap();
        assert_eq!(declaration.id, "node-v26.8.1");
        assert_eq!(
            declaration.expected_program_sha256.as_deref(),
            Some(digest.as_str())
        );
        assert!(parse_runtime_contract(None, Some(&digest)).is_err());
        for invalid in ["", ".node", "node..current", "node/current"] {
            assert!(parse_runtime_contract(Some(invalid), None).is_err());
        }
        assert!(parse_runtime_contract(Some(&"a".repeat(97)), None).is_err());
    }

    #[test]
    fn native_cache_paths_are_normalized_and_relative() {
        assert_eq!(parse_native_cache("target:native").unwrap().path, "target");
        for value in [
            "target:overlay",
            "/target:native",
            "../target:native",
            "a//b:native",
        ] {
            assert!(parse_native_cache(value).is_err(), "accepted {value}");
        }
        let duplicate = [
            parse_native_cache("target:native").unwrap(),
            parse_native_cache("target:native").unwrap(),
        ];
        assert!(validate_native_caches(&duplicate).is_err());
        let overlap = [
            parse_native_cache("build:native").unwrap(),
            parse_native_cache("build/output:native").unwrap(),
        ];
        assert!(validate_native_caches(&overlap).is_err());
    }

    #[test]
    fn machine_parsers_preserve_missing_and_strict_values() {
        assert_eq!(parse_cpu_list("0-7,16,20-22\n").unwrap(), 12);
        assert!(parse_cpu_list("0-7,7-8").is_err());
        assert_eq!(
            parse_load_average("1.25 2.5 3.75 1/2 3").unwrap(),
            json!({"one_minute": 1.25, "five_minutes": 2.5, "fifteen_minutes": 3.75})
        );
        assert_eq!(
            parse_meminfo("MemAvailable: 1024 kB\nSwapTotal: 512 kB\nSwapFree: 128 kB\n").unwrap(),
            json!({"available_bytes": 1048576, "swap_total_bytes": 524288, "swap_used_bytes": 393216})
        );
        assert!(parse_meminfo("MemAvailable: 1 kB\nSwapTotal: 1 kB\n").is_err());
        assert!(parse_load_average("nan 1 1").is_err());
        let pressure = parse_pressure(
            "some avg10=0.25 avg60=1.50 avg300=2.75 total=12345\nfull avg10=0 avg60=0 avg300=0 total=0\n",
        )
        .unwrap();
        assert_eq!(pressure["some"]["total_microseconds"], 12345);
    }

    #[test]
    fn pressure_interval_rejects_counter_reset() {
        let before = json!({"memory": {"available_bytes": 1000, "swap_used_bytes": 200}, "pressure": {"cpu": {"some": {"total_microseconds": 1000}}}});
        let after = json!({"memory": {"available_bytes": 900, "swap_used_bytes": 230}, "pressure": {"cpu": {"some": {"total_microseconds": 1500}}}});
        let interval = pressure_interval(&before, &after, Duration::from_secs(2));
        assert_eq!(interval["memory"]["available_bytes_delta"], -100);
        assert_eq!(
            interval["pressure"]["cpu"]["some"]["total_microseconds_delta"],
            500
        );
        assert_eq!(
            interval["pressure"]["io"]["some"]["total_microseconds_delta"],
            Value::Null
        );
    }

    #[test]
    fn direct_measurement_preserves_exit_and_schema() {
        let fixture = test_directory("glaeda-hot-run-test");
        let measurement = fixture.join("measurement.json");
        let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let code = run(Cli {
            resident: repository.clone(),
            task: repository.clone(),
            cache: vec!["target:native".into()],
            measurement: Some(measurement.clone()),
            comparison_key: Some(format!("sha256:{}", "a".repeat(64))),
            runtime_id: None,
            runtime_sha256: None,
            runtime_bin: None,
            command: vec![
                OsString::from("/bin/sh"),
                OsString::from("-c"),
                OsString::from("exit 17"),
            ],
        })
        .unwrap();
        assert_eq!(code, 17);
        let report: Value = serde_json::from_reader(File::open(&measurement).unwrap()).unwrap();
        assert_eq!(report["schema_version"], 6);
        assert_eq!(report["document_type"], "glaeda-hot-run-measurement");
        assert_eq!(report["authority"], "developer_observation_only");
        assert_eq!(report["cross_worktree"], false);
        assert_eq!(report["cache_views"][0]["path"], "target");
        assert_eq!(report["resource_accounting"], "gnu_time_command_tree");
        assert_eq!(report["exit_code"], 17);
        assert_eq!(report["signal"], Value::Null);
        assert_eq!(report["completion_reason"], "exited");
        assert_eq!(
            fs::metadata(&measurement).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_file(measurement).unwrap();
        fs::remove_dir(fixture).unwrap();
    }

    #[test]
    fn measured_signal_is_distinct_from_same_numeric_exit() {
        let fixture = test_directory("glaeda-hot-run-signal-test");
        let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for (name, shell, reason, expected_signal) in [
            ("exit", "exit 143", "exited", Value::Null),
            ("signal", "kill -TERM $$", "signaled", json!(15)),
        ] {
            let measurement = fixture.join(format!("{name}.json"));
            let code = run(Cli {
                resident: repository.clone(),
                task: repository.clone(),
                cache: Vec::new(),
                measurement: Some(measurement.clone()),
                comparison_key: None,
                runtime_id: None,
                runtime_sha256: None,
                runtime_bin: None,
                command: vec![
                    OsString::from("/bin/sh"),
                    OsString::from("-c"),
                    OsString::from(shell),
                ],
            })
            .unwrap();
            assert_eq!(code, 143);
            let report: Value = serde_json::from_reader(File::open(&measurement).unwrap()).unwrap();
            assert_eq!(report["completion_reason"], reason);
            assert_eq!(report["signal"], expected_signal);
            fs::remove_file(measurement).unwrap();
        }
        fs::remove_dir(fixture).unwrap();
    }

    #[test]
    fn runtime_bin_binds_program_digest_and_descendant_path() {
        let fixture = test_directory("glaeda-hot-run-runtime-test");
        let runtime_bin = fixture.join("bin");
        fs::create_dir(&runtime_bin).unwrap();
        let program = runtime_bin.join("runtime-tool");
        let descendant = runtime_bin.join("runtime-descendant");
        fs::write(&program, b"#!/bin/sh\nexec runtime-descendant\n").unwrap();
        fs::write(&descendant, b"#!/bin/sh\nexit 17\n").unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&descendant, fs::Permissions::from_mode(0o700)).unwrap();
        let program_digest = sha256_file(&program).unwrap();
        let measurement = fixture.join("measurement.json");
        let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let wrong_measurement = fixture.join("wrong-measurement.json");
        let error = run(Cli {
            resident: repository.clone(),
            task: repository.clone(),
            cache: Vec::new(),
            measurement: Some(wrong_measurement.clone()),
            comparison_key: None,
            runtime_id: Some("fixture-runtime-v1".into()),
            runtime_sha256: Some(format!("sha256:{}", "0".repeat(64))),
            runtime_bin: Some(runtime_bin.clone()),
            command: vec![OsString::from("runtime-tool")],
        })
        .unwrap_err();
        assert_eq!(
            error,
            "runtime executable content does not match declared digest"
        );
        assert!(!wrong_measurement.exists());

        let code = run(Cli {
            resident: repository.clone(),
            task: repository.clone(),
            cache: Vec::new(),
            measurement: Some(measurement.clone()),
            comparison_key: None,
            runtime_id: Some("fixture-runtime-v1".into()),
            runtime_sha256: Some(program_digest.clone()),
            runtime_bin: Some(runtime_bin.clone()),
            command: vec![OsString::from("runtime-tool")],
        })
        .unwrap();
        assert_eq!(code, 17);
        let report_text = fs::read_to_string(&measurement).unwrap();
        let report: Value = serde_json::from_str(&report_text).unwrap();
        assert_eq!(report["runtime"]["id"], "fixture-runtime-v1");
        assert_eq!(report["runtime"]["program_sha256"], program_digest);
        assert_eq!(report["runtime"]["descendant_path"], "runtime_bin_first");
        assert!(
            report["runtime"]["runtime_bin_binding_sha256"]
                .as_str()
                .is_some_and(|value| validate_sha256(value).is_ok())
        );
        assert!(!report_text.contains(fixture.to_str().unwrap()));

        let outside_error = run(Cli {
            resident: repository.clone(),
            task: repository,
            cache: Vec::new(),
            measurement: None,
            comparison_key: None,
            runtime_id: Some("fixture-runtime-v1".into()),
            runtime_sha256: None,
            runtime_bin: Some(runtime_bin.clone()),
            command: vec![OsString::from("/bin/true")],
        })
        .unwrap_err();
        assert_eq!(
            outside_error,
            "runtime executable is outside the bound runtime bin"
        );

        let alias = fixture.join("bin-alias");
        symlink(&runtime_bin, &alias).unwrap();
        assert!(observe_runtime_bin(Some(&alias), Some("fixture")).is_err());

        fs::remove_file(alias).unwrap();
        fs::remove_file(measurement).unwrap();
        let binding = observe_runtime_bin(Some(&runtime_bin), Some("fixture"))
            .unwrap()
            .unwrap();
        let moved = fixture.join("old-bin");
        fs::rename(&runtime_bin, &moved).unwrap();
        fs::create_dir(&runtime_bin).unwrap();
        assert_eq!(
            revalidate_runtime_bin(&binding).unwrap_err(),
            "runtime bin binding changed during preflight"
        );
        fs::remove_dir(runtime_bin).unwrap();
        fs::remove_file(moved.join("runtime-descendant")).unwrap();
        fs::remove_file(moved.join("runtime-tool")).unwrap();
        fs::remove_dir(moved).unwrap();
        fs::remove_dir(fixture).unwrap();
    }
}
