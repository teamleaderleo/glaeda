use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::Serialize;

use crate::mac_availability::{HostPowerSource, MemoryPressure, ObservationFreshness};
use crate::process::{CommandExecutor, CommandSpec, ExecutionRecord};

pub const MACOS_RESOURCE_OBSERVATION_SCHEMA_VERSION: u8 = 1;
pub const DEFAULT_FRESHNESS_WINDOW_MILLIS: u64 = 30_000;
const MAX_SMALL_OUTPUT_BYTES: usize = 65_536;
const MAX_PROCESS_OUTPUT_BYTES: usize = 524_288;
const MAX_PROCESS_ROWS: usize = 4_096;
const MAX_PUBLIC_LIMA_PROCESSES: usize = 64;
const MAX_CPU_BASIS_POINTS: u32 = 10_000_000;
const MAX_RSS_KIB: u64 = 1_u64 << 40;
const SWAP_ROUNDING_TOLERANCE_BYTES: u64 = 65_536;

const SYSCTL_PROGRAM: &str = "/usr/sbin/sysctl";
const PMSET_PROGRAM: &str = "/usr/bin/pmset";
const PS_PROGRAM: &str = "/bin/ps";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationCompleteness {
    Complete,
    Partial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BatteryChargeState {
    Charging,
    Charged,
    Discharging,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MacPowerObservation {
    pub source: HostPowerSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub battery_percent: Option<u8>,
    pub charge_state: BatteryChargeState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SwapObservation {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub free_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encrypted: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LimaProcessRole {
    Controller,
    HostAgent,
    VirtualMachine,
    Network,
    FileSharing,
    Auxiliary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LimaProcessObservation {
    pub pid: u32,
    pub parent_pid: u32,
    pub role: LimaProcessRole,
    pub cpu_basis_points: u32,
    pub rss_bytes: u64,
    pub elapsed_seconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MacOsResourceProblemKind {
    StaleObservation,
    MemoryPressureUnavailable,
    SwapUnavailable,
    PowerUnavailable,
    BatteryDetailsUnavailable,
    LimaProcessObservationUnavailable,
    LimaProcessListTruncated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MacOsResourceReport {
    pub schema_version: u8,
    pub observed_at_millis: u64,
    pub freshness: ObservationFreshness,
    pub completeness: ObservationCompleteness,
    pub memory_pressure: MemoryPressure,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub swap: Option<SwapObservation>,
    pub power: MacPowerObservation,
    pub lima_processes: Vec<LimaProcessObservation>,
    pub problems: Vec<MacOsResourceProblemKind>,
}

pub struct MacOsResourceObservation {
    report: MacOsResourceReport,
    private_evidence: PrivateEvidence,
}

impl MacOsResourceObservation {
    #[must_use]
    pub const fn report(&self) -> &MacOsResourceReport {
        &self.report
    }

    #[must_use]
    pub fn into_report(self) -> MacOsResourceReport {
        self.report
    }
}

impl fmt::Debug for MacOsResourceObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MacOsResourceObservation")
            .field("report", &self.report)
            .field("private_evidence", &"[REDACTED]")
            .field(
                "captured_sources",
                &self.private_evidence.captured_source_count(),
            )
            .finish()
    }
}

#[derive(Default)]
struct PrivateEvidence {
    memory_pressure: Option<ExecutionRecord>,
    swap: Option<ExecutionRecord>,
    power: Option<ExecutionRecord>,
    processes: Option<ExecutionRecord>,
}

impl PrivateEvidence {
    fn captured_source_count(&self) -> usize {
        [
            self.memory_pressure.as_ref(),
            self.swap.as_ref(),
            self.power.as_ref(),
            self.processes.as_ref(),
        ]
        .into_iter()
        .flatten()
        .count()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MacOsResourceObservationError {
    pub field: &'static str,
    pub code: &'static str,
    pub message: &'static str,
}

impl MacOsResourceObservationError {
    const fn new(field: &'static str, code: &'static str, message: &'static str) -> Self {
        Self {
            field,
            code,
            message,
        }
    }
}

impl fmt::Display for MacOsResourceObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for MacOsResourceObservationError {}

#[must_use]
pub fn memory_pressure_command() -> CommandSpec {
    CommandSpec::new(SYSCTL_PROGRAM)
        .argument("-n")
        .argument("kern.memorystatus_vm_pressure_level")
}

#[must_use]
pub fn swap_command() -> CommandSpec {
    CommandSpec::new(SYSCTL_PROGRAM)
        .argument("-n")
        .argument("vm.swapusage")
}

#[must_use]
pub fn power_command() -> CommandSpec {
    CommandSpec::new(PMSET_PROGRAM)
        .argument("-g")
        .argument("batt")
}

#[must_use]
pub fn lima_process_command() -> CommandSpec {
    CommandSpec::new(PS_PROGRAM)
        .argument("-axo")
        .argument("pid=,ppid=,%cpu=,rss=,etime=,comm=")
}

/// Observe bounded macOS host resource evidence without changing host or Lima state.
///
/// The caller supplies the observation completion time and comparison time so tests and future
/// status adapters do not depend on an ambient clock. Individual command failures are represented
/// as typed partial evidence rather than promoted to absence.
///
/// # Errors
///
/// Returns an error only when the supplied time boundary is invalid. Command, output, or parser
/// failures are retained as fixed public problem kinds with private raw evidence.
pub fn observe_macos_resources(
    executor: &impl CommandExecutor,
    observed_at_millis: u64,
    now_millis: u64,
    freshness_window_millis: u64,
) -> Result<MacOsResourceObservation, MacOsResourceObservationError> {
    if observed_at_millis == 0 {
        return Err(MacOsResourceObservationError::new(
            "observed_at_millis",
            "invalid_observation_time",
            "observation time must be greater than zero",
        ));
    }
    if freshness_window_millis == 0 {
        return Err(MacOsResourceObservationError::new(
            "freshness_window_millis",
            "invalid_freshness_window",
            "freshness window must be greater than zero",
        ));
    }
    let age = now_millis.checked_sub(observed_at_millis).ok_or_else(|| {
        MacOsResourceObservationError::new(
            "now_millis",
            "observation_time_reversal",
            "comparison time cannot precede the observation time",
        )
    })?;

    let memory_command = memory_pressure_command();
    let swap_command = swap_command();
    let power_command = power_command();
    let process_command = lima_process_command();

    let memory_receipt = executor.execute(&memory_command).ok();
    let swap_receipt = executor.execute(&swap_command).ok();
    let power_receipt = executor.execute(&power_command).ok();
    let process_receipt = executor.execute(&process_command).ok();

    let mut problems = BTreeSet::new();
    let freshness = if age <= freshness_window_millis {
        ObservationFreshness::Fresh
    } else {
        problems.insert(MacOsResourceProblemKind::StaleObservation);
        ObservationFreshness::Stale
    };

    let memory_pressure = memory_receipt
        .as_ref()
        .and_then(|receipt| parse_memory_pressure_receipt(&memory_command, receipt))
        .unwrap_or_else(|| {
            problems.insert(MacOsResourceProblemKind::MemoryPressureUnavailable);
            MemoryPressure::Unknown
        });
    if memory_pressure == MemoryPressure::Unknown {
        problems.insert(MacOsResourceProblemKind::MemoryPressureUnavailable);
    }

    let swap = swap_receipt
        .as_ref()
        .and_then(|receipt| parse_swap_receipt(&swap_command, receipt));
    if swap.is_none() {
        problems.insert(MacOsResourceProblemKind::SwapUnavailable);
    }

    let power = power_receipt
        .as_ref()
        .and_then(|receipt| parse_power_receipt(&power_command, receipt))
        .unwrap_or_else(|| {
            problems.insert(MacOsResourceProblemKind::PowerUnavailable);
            MacPowerObservation {
                source: HostPowerSource::Unknown,
                battery_percent: None,
                charge_state: BatteryChargeState::Unknown,
            }
        });
    if power.source == HostPowerSource::Unknown {
        problems.insert(MacOsResourceProblemKind::PowerUnavailable);
    }
    if power.source == HostPowerSource::Battery && power.battery_percent.is_none() {
        problems.insert(MacOsResourceProblemKind::BatteryDetailsUnavailable);
    }

    let mut lima_processes = process_receipt
        .as_ref()
        .and_then(|receipt| parse_process_receipt(&process_command, receipt))
        .unwrap_or_else(|| {
            problems.insert(MacOsResourceProblemKind::LimaProcessObservationUnavailable);
            Vec::new()
        });
    if lima_processes.len() > MAX_PUBLIC_LIMA_PROCESSES {
        lima_processes.truncate(MAX_PUBLIC_LIMA_PROCESSES);
        problems.insert(MacOsResourceProblemKind::LimaProcessListTruncated);
    }

    let problems = problems.into_iter().collect::<Vec<_>>();
    let completeness = if problems.is_empty() {
        ObservationCompleteness::Complete
    } else {
        ObservationCompleteness::Partial
    };

    Ok(MacOsResourceObservation {
        report: MacOsResourceReport {
            schema_version: MACOS_RESOURCE_OBSERVATION_SCHEMA_VERSION,
            observed_at_millis,
            freshness,
            completeness,
            memory_pressure,
            swap,
            power,
            lima_processes,
            problems,
        },
        private_evidence: PrivateEvidence {
            memory_pressure: memory_receipt,
            swap: swap_receipt,
            power: power_receipt,
            processes: process_receipt,
        },
    })
}

fn parse_memory_pressure_receipt(
    command: &CommandSpec,
    receipt: &ExecutionRecord,
) -> Option<MemoryPressure> {
    if !valid_receipt(command, receipt, MAX_SMALL_OUTPUT_BYTES)
        || !receipt.success
        || receipt.status != Some(0)
        || !receipt.stderr.is_empty()
    {
        return None;
    }
    parse_memory_pressure_output(&receipt.stdout)
}

fn parse_memory_pressure_output(input: &str) -> Option<MemoryPressure> {
    let lines = input
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    if lines.len() != 1 {
        return None;
    }
    match lines[0].trim() {
        "1" => Some(MemoryPressure::Normal),
        "2" => Some(MemoryPressure::Elevated),
        "4" => Some(MemoryPressure::Critical),
        _ => None,
    }
}

fn parse_swap_receipt(command: &CommandSpec, receipt: &ExecutionRecord) -> Option<SwapObservation> {
    if !valid_receipt(command, receipt, MAX_SMALL_OUTPUT_BYTES)
        || !receipt.success
        || receipt.status != Some(0)
        || !receipt.stderr.is_empty()
    {
        return None;
    }

    let total_bytes = parse_named_quantity(&receipt.stdout, "total")?;
    let used_bytes = parse_named_quantity(&receipt.stdout, "used")?;
    let free_bytes = parse_named_quantity(&receipt.stdout, "free")?;
    let combined = used_bytes.checked_add(free_bytes)?;
    if total_bytes.abs_diff(combined) > SWAP_ROUNDING_TOLERANCE_BYTES {
        return None;
    }

    let lower = receipt.stdout.to_ascii_lowercase();
    let encrypted = if lower.contains("(encrypted)") {
        Some(true)
    } else if lower.contains("(unencrypted)") {
        Some(false)
    } else {
        None
    };

    Some(SwapObservation {
        total_bytes,
        used_bytes,
        free_bytes,
        encrypted,
    })
}

fn parse_named_quantity(input: &str, name: &str) -> Option<u64> {
    let marker = format!("{name} =");
    let positions = input.match_indices(&marker).collect::<Vec<_>>();
    if positions.len() != 1 {
        return None;
    }
    let value = input[positions[0].0 + marker.len()..]
        .split_whitespace()
        .next()?;
    parse_rounded_bytes(value)
}

fn parse_rounded_bytes(value: &str) -> Option<u64> {
    let suffix = value.chars().last()?;
    let multiplier = match suffix.to_ascii_uppercase() {
        'B' => 1_u128,
        'K' => 1_u128 << 10,
        'M' => 1_u128 << 20,
        'G' => 1_u128 << 30,
        'T' => 1_u128 << 40,
        _ => return None,
    };
    let numeric = &value[..value.len().checked_sub(suffix.len_utf8())?];
    let (numerator, denominator) = parse_decimal_ratio(numeric)?;
    let rounded = numerator
        .checked_mul(multiplier)?
        .checked_add(denominator / 2)?
        / denominator;
    u64::try_from(rounded).ok()
}

fn parse_power_receipt(
    command: &CommandSpec,
    receipt: &ExecutionRecord,
) -> Option<MacPowerObservation> {
    if !valid_receipt(command, receipt, MAX_SMALL_OUTPUT_BYTES)
        || !receipt.success
        || receipt.status != Some(0)
        || !receipt.stderr.is_empty()
    {
        return None;
    }

    let first_line = receipt
        .stdout
        .lines()
        .find(|line| !line.trim().is_empty())?;
    let source = if first_line.contains("'AC Power'") {
        HostPowerSource::Ac
    } else if first_line.contains("'Battery Power'") {
        HostPowerSource::Battery
    } else {
        HostPowerSource::Unknown
    };

    let battery_percent = parse_unique_battery_percent(&receipt.stdout);
    let lower = receipt.stdout.to_ascii_lowercase();
    let charge_state = if lower.contains("; discharging;") {
        BatteryChargeState::Discharging
    } else if lower.contains("; charging;") || lower.contains("; finishing charge;") {
        BatteryChargeState::Charging
    } else if lower.contains("; charged;") {
        BatteryChargeState::Charged
    } else {
        BatteryChargeState::Unknown
    };

    Some(MacPowerObservation {
        source,
        battery_percent,
        charge_state,
    })
}

fn parse_unique_battery_percent(input: &str) -> Option<u8> {
    let mut parsed = None;
    for (index, character) in input.char_indices() {
        if character != '%' {
            continue;
        }
        let prefix = &input[..index];
        let digits = prefix
            .chars()
            .rev()
            .take_while(|character| character.is_ascii_digit())
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>();
        if digits.is_empty() {
            continue;
        }
        let value = digits.parse::<u8>().ok()?;
        if value > 100 {
            return None;
        }
        set_unique(&mut parsed, value)?;
    }
    parsed
}

fn parse_process_receipt(
    command: &CommandSpec,
    receipt: &ExecutionRecord,
) -> Option<Vec<LimaProcessObservation>> {
    if !valid_receipt(command, receipt, MAX_PROCESS_OUTPUT_BYTES)
        || !receipt.success
        || receipt.status != Some(0)
        || !receipt.stderr.is_empty()
    {
        return None;
    }

    let lines = receipt
        .stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    if lines.len() > MAX_PROCESS_ROWS {
        return None;
    }

    let mut processes = BTreeMap::new();
    for line in lines {
        let parsed = ParsedProcess::parse(line)?;
        if processes.insert(parsed.pid, parsed).is_some() {
            return None;
        }
    }

    let mut selected = processes
        .values()
        .filter(|process| root_role(&process.command).is_some())
        .map(|process| process.pid)
        .collect::<BTreeSet<_>>();

    loop {
        let before = selected.len();
        for process in processes.values() {
            if selected.contains(&process.parent_pid) {
                selected.insert(process.pid);
            }
        }
        if selected.len() == before {
            break;
        }
    }

    let mut observations = selected
        .into_iter()
        .filter_map(|pid| processes.get(&pid))
        .map(|process| LimaProcessObservation {
            pid: process.pid,
            parent_pid: process.parent_pid,
            role: process_role(&process.command),
            cpu_basis_points: process.cpu_basis_points,
            rss_bytes: process.rss_bytes,
            elapsed_seconds: process.elapsed_seconds,
        })
        .collect::<Vec<_>>();
    observations.sort_by_key(|observation| observation.pid);
    Some(observations)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedProcess {
    pid: u32,
    parent_pid: u32,
    cpu_basis_points: u32,
    rss_bytes: u64,
    elapsed_seconds: u64,
    command: String,
}

impl ParsedProcess {
    fn parse(line: &str) -> Option<Self> {
        let mut remainder = line;
        let pid = take_field(&mut remainder)?.parse::<u32>().ok()?;
        let parent_pid = take_field(&mut remainder)?.parse::<u32>().ok()?;
        let cpu_basis_points = parse_decimal_scaled(take_field(&mut remainder)?, 100)?;
        if cpu_basis_points > u64::from(MAX_CPU_BASIS_POINTS) {
            return None;
        }
        let rss_kib = take_field(&mut remainder)?.parse::<u64>().ok()?;
        if rss_kib > MAX_RSS_KIB {
            return None;
        }
        let elapsed_seconds = parse_elapsed(take_field(&mut remainder)?)?;
        let command = remainder.trim().to_owned();
        if pid == 0 || command.is_empty() || command.contains('\0') {
            return None;
        }

        Some(Self {
            pid,
            parent_pid,
            cpu_basis_points: u32::try_from(cpu_basis_points).ok()?,
            rss_bytes: rss_kib.checked_mul(1_024)?,
            elapsed_seconds,
            command,
        })
    }
}

fn root_role(command: &str) -> Option<LimaProcessRole> {
    let basename = command_basename(command).to_ascii_lowercase();
    if basename == "limactl" {
        Some(LimaProcessRole::Controller)
    } else if basename == "lima" || basename.starts_with("lima-") {
        Some(LimaProcessRole::HostAgent)
    } else {
        None
    }
}

fn process_role(command: &str) -> LimaProcessRole {
    if let Some(role) = root_role(command) {
        return role;
    }
    let basename = command_basename(command).to_ascii_lowercase();
    if basename == "vfkit"
        || basename.starts_with("qemu-system-")
        || basename == "virtualization.virtualmachine"
    {
        LimaProcessRole::VirtualMachine
    } else if basename == "socket_vmnet" || basename == "vde_switch" {
        LimaProcessRole::Network
    } else if basename == "virtiofsd" {
        LimaProcessRole::FileSharing
    } else {
        LimaProcessRole::Auxiliary
    }
}

fn command_basename(command: &str) -> &str {
    command.rsplit('/').next().unwrap_or(command)
}

fn parse_elapsed(value: &str) -> Option<u64> {
    let (days, clock) = if let Some((days, clock)) = value.split_once('-') {
        (days.parse::<u64>().ok()?, clock)
    } else {
        (0, value)
    };
    let parts = clock.split(':').collect::<Vec<_>>();
    let (hours, minutes, seconds) = match parts.as_slice() {
        [minutes, seconds] => (
            0,
            minutes.parse::<u64>().ok()?,
            seconds.parse::<u64>().ok()?,
        ),
        [hours, minutes, seconds] => (
            hours.parse::<u64>().ok()?,
            minutes.parse::<u64>().ok()?,
            seconds.parse::<u64>().ok()?,
        ),
        _ => return None,
    };
    if hours >= 24 || minutes >= 60 || seconds >= 60 {
        return None;
    }
    days.checked_mul(86_400)?
        .checked_add(hours.checked_mul(3_600)?)?
        .checked_add(minutes.checked_mul(60)?)?
        .checked_add(seconds)
}

fn take_field<'a>(input: &mut &'a str) -> Option<&'a str> {
    *input = input.trim_start();
    let end = input.find(char::is_whitespace).unwrap_or(input.len());
    if end == 0 {
        return None;
    }
    let field = &input[..end];
    *input = &input[end..];
    Some(field)
}

fn parse_decimal_scaled(value: &str, scale: u64) -> Option<u64> {
    let (numerator, denominator) = parse_decimal_ratio(value)?;
    let scaled = numerator.checked_mul(u128::from(scale))? / denominator;
    u64::try_from(scaled).ok()
}

fn parse_decimal_ratio(value: &str) -> Option<(u128, u128)> {
    if value.is_empty() || value.starts_with('+') || value.starts_with('-') {
        return None;
    }
    let mut split = value.split('.');
    let whole = split.next()?;
    let fraction = split.next();
    if split.next().is_some()
        || whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let whole = whole.parse::<u128>().ok()?;
    let Some(fraction) = fraction else {
        return Some((whole, 1));
    };
    if fraction.is_empty()
        || fraction.len() > 6
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let denominator = 10_u128.checked_pow(u32::try_from(fraction.len()).ok()?)?;
    let fraction = fraction.parse::<u128>().ok()?;
    Some((
        whole.checked_mul(denominator)?.checked_add(fraction)?,
        denominator,
    ))
}

fn valid_receipt(command: &CommandSpec, receipt: &ExecutionRecord, max_bytes: usize) -> bool {
    receipt.argv == command.displayed_argv()
        && receipt.environment_keys.is_empty()
        && receipt.stdout.len() <= max_bytes
        && receipt.stderr.len() <= max_bytes
        && !receipt.stdout.contains('\0')
        && !receipt.stderr.contains('\0')
}

fn set_unique<T: Copy + PartialEq>(slot: &mut Option<T>, value: T) -> Option<()> {
    match slot {
        Some(existing) if *existing != value => None,
        Some(_) => Some(()),
        None => {
            *slot = Some(value);
            Some(())
        }
    }
}

#[cfg(test)]
mod tests;
