use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::lima_lifecycle::LimaResourceProfile;
use crate::lima_observation::{
    LimaInstanceObservation, LimaInstanceObservationReport, LimaObservationAdapter,
    LimaObservationClock, LimaObservationFailure, LimaObservationFreshness, LimaObservationPhase,
    LimaObservationRefusalCode, LimaObservationRequest, LimaObservationRequestIdentity,
    LimaObservationSourceIdentity,
};
use crate::mac_availability::{AvailabilityRequest, ObservationFreshness};
use crate::macos_resource_observation::{
    MacOsResourceObservation, MacOsResourceReport, observe_macos_resources,
};
use crate::operator_config::{OperatorConfig, OperatorConfigIdentity};
use crate::process::{CommandExecutor, CommandSpec, ExecutionRecord, TimedCommandExecutor};

pub const PERSONAL_WORKER_MAC_OBSERVATION_SCHEMA_VERSION: u8 = 1;
pub const MAX_PERSONAL_WORKER_MAC_OBSERVATION_AGE_MILLIS: u64 = 300_000;
pub const MAX_PERSONAL_WORKER_MAC_COMMAND_TIMEOUT_MILLIS: u64 = 30_000;

const MAX_COMMAND_OUTPUT_BYTES: usize = 65_536;
const MAX_LOGICAL_CPUS: u16 = 1_024;
const MIN_VM_PAGE_BYTES: u64 = 4_096;
const MAX_VM_PAGE_BYTES: u64 = 65_536;
const VM_STAT_PROGRAM: &str = "/usr/bin/vm_stat";
const SYSCTL_PROGRAM: &str = "/usr/sbin/sysctl";
const VM_STAT_HEADER_PREFIX: &str = "Mach Virtual Memory Statistics: (page size of ";
const VM_STAT_HEADER_SUFFIX: &str = " bytes)";
const REDACTED_PRIVATE_EVIDENCE: &str = "<private-personal-worker-mac-observation-evidence>";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerMacObservationTiming {
    pub started_at_millis: u64,
    pub observed_at_millis: u64,
    pub expires_at_millis: u64,
    pub duration_millis: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MacHostHeadroomEvidence {
    pub available_memory_bytes: u64,
    pub logical_cpu_count: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerMacObservationReport {
    pub schema_version: u8,
    pub config_identity: OperatorConfigIdentity,
    pub requested_availability: AvailabilityRequest,
    pub timing: PersonalWorkerMacObservationTiming,
    pub host_headroom: MacHostHeadroomEvidence,
    pub host_resources: MacOsResourceReport,
    pub lima: LimaInstanceObservationReport,
    pub lima_profile: LimaResourceProfile,
}

pub struct PersonalWorkerMacObservation {
    report: PersonalWorkerMacObservationReport,
    lima_source_identity: LimaObservationSourceIdentity,
    lima_request_identity: LimaObservationRequestIdentity,
    private_evidence: PrivateEvidence,
}

impl PersonalWorkerMacObservation {
    #[must_use]
    pub const fn report(&self) -> &PersonalWorkerMacObservationReport {
        &self.report
    }

    #[must_use]
    pub fn into_report(self) -> PersonalWorkerMacObservationReport {
        self.report
    }

    #[must_use]
    pub const fn lima_source_identity(&self) -> &LimaObservationSourceIdentity {
        &self.lima_source_identity
    }

    #[must_use]
    pub const fn lima_request_identity(&self) -> &LimaObservationRequestIdentity {
        &self.lima_request_identity
    }
}

impl fmt::Debug for PersonalWorkerMacObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersonalWorkerMacObservation")
            .field("report", &self.report)
            .field("private_evidence", &REDACTED_PRIVATE_EVIDENCE)
            .field(
                "captured_source_count",
                &self.private_evidence.source_count(),
            )
            .finish()
    }
}

struct PrivateEvidence {
    vm_stat: ExecutionRecord,
    logical_cpu: ExecutionRecord,
    host_resources: MacOsResourceObservation,
    lima: LimaInstanceObservation,
}

impl PrivateEvidence {
    const fn source_count(&self) -> usize {
        let _ = (
            &self.vm_stat,
            &self.logical_cpu,
            &self.host_resources,
            &self.lima,
        );
        4
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerMacObservationErrorKind {
    Policy,
    ConfigIdentity,
    Clock,
    Command,
    HostEvidence,
    LimaEvidence,
}

#[derive(Serialize)]
pub struct PersonalWorkerMacObservationError {
    pub kind: PersonalWorkerMacObservationErrorKind,
    pub field: &'static str,
    pub code: &'static str,
    pub message: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lima_code: Option<LimaObservationRefusalCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lima_phase: Option<LimaObservationPhase>,
    #[serde(skip)]
    private_lima_failure: Option<Box<LimaObservationFailure>>,
}

impl PersonalWorkerMacObservationError {
    const fn new(
        kind: PersonalWorkerMacObservationErrorKind,
        field: &'static str,
        code: &'static str,
        message: &'static str,
    ) -> Self {
        Self {
            kind,
            field,
            code,
            message,
            lima_code: None,
            lima_phase: None,
            private_lima_failure: None,
        }
    }

    fn from_lima(error: LimaObservationFailure) -> Self {
        Self {
            kind: PersonalWorkerMacObservationErrorKind::LimaEvidence,
            field: "lima",
            code: "lima_observation_failed",
            message: "the exact Lima observation could not be established",
            lima_code: Some(error.code),
            lima_phase: Some(error.phase),
            private_lima_failure: Some(Box::new(error)),
        }
    }

    #[must_use]
    pub fn private_lima_failure(&self) -> Option<&LimaObservationFailure> {
        self.private_lima_failure.as_deref()
    }
}

impl fmt::Debug for PersonalWorkerMacObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersonalWorkerMacObservationError")
            .field("kind", &self.kind)
            .field("field", &self.field)
            .field("code", &self.code)
            .field("message", &self.message)
            .field("lima_code", &self.lima_code)
            .field("lima_phase", &self.lima_phase)
            .field("private_lima_failure", &REDACTED_PRIVATE_EVIDENCE)
            .finish()
    }
}

impl fmt::Display for PersonalWorkerMacObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for PersonalWorkerMacObservationError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonalWorkerMacObservationAdapter {
    freshness_window_millis: u64,
    command_timeout: Duration,
}

impl PersonalWorkerMacObservationAdapter {
    /// Construct the bounded personal-worker Mac observation adapter.
    ///
    /// # Errors
    ///
    /// Returns a static public error unless both the freshness window and command timeout are
    /// positive and remain within their reviewed maxima.
    pub fn new(
        freshness_window_millis: u64,
        command_timeout: Duration,
    ) -> Result<Self, PersonalWorkerMacObservationError> {
        if !(1..=MAX_PERSONAL_WORKER_MAC_OBSERVATION_AGE_MILLIS).contains(&freshness_window_millis)
        {
            return Err(PersonalWorkerMacObservationError::new(
                PersonalWorkerMacObservationErrorKind::Policy,
                "freshness_window_millis",
                "invalid_freshness_window",
                "the Mac observation freshness window is outside the reviewed range",
            ));
        }
        if command_timeout.is_zero()
            || command_timeout
                > Duration::from_millis(MAX_PERSONAL_WORKER_MAC_COMMAND_TIMEOUT_MILLIS)
        {
            return Err(PersonalWorkerMacObservationError::new(
                PersonalWorkerMacObservationErrorKind::Policy,
                "command_timeout",
                "invalid_command_timeout",
                "the Mac observation command timeout is outside the reviewed range",
            ));
        }
        Ok(Self {
            freshness_window_millis,
            command_timeout,
        })
    }

    /// Observe exact accepted operator, Mac host, and Lima instance evidence without mutation.
    ///
    /// All direct and delegated subprocesses pass through the same reviewed timeout. The adapter
    /// has no profile decision, lifecycle mutation, queue, runner, persistence, credential, or
    /// cache authority.
    ///
    /// # Errors
    ///
    /// Returns a bounded public error for config/request drift, clock failure, command failure,
    /// malformed or oversized evidence, unsupported Lima profiles, or stale/cross-window evidence.
    pub fn observe(
        &self,
        config: &OperatorConfig,
        lima_request: &LimaObservationRequest,
        lima_adapter: &LimaObservationAdapter,
        executor: &impl TimedCommandExecutor,
        clock: &impl PersonalWorkerMacObservationClock,
    ) -> Result<PersonalWorkerMacObservation, PersonalWorkerMacObservationError> {
        if config.lima_instance() != lima_request.instance() {
            return Err(PersonalWorkerMacObservationError::new(
                PersonalWorkerMacObservationErrorKind::ConfigIdentity,
                "lima_request.instance",
                "config_lima_instance_mismatch",
                "the Lima observation request must bind the configured instance",
            ));
        }

        let started_at_millis = read_clock(clock, "timing.started_at_millis")?;
        let bounded = TimeoutExecutor {
            executor,
            timeout: self.command_timeout,
        };
        let host_resources = observe_macos_resources(
            &bounded,
            started_at_millis,
            started_at_millis,
            self.freshness_window_millis,
        )
        .map_err(|_| {
            PersonalWorkerMacObservationError::new(
                PersonalWorkerMacObservationErrorKind::HostEvidence,
                "host_resources",
                "host_resource_observation_failed",
                "the bounded macOS resource observation could not be established",
            )
        })?;

        let vm_stat = execute_exact(&bounded, &vm_stat_command(), "host.available_memory")?;
        let available_memory_bytes = parse_available_memory(&vm_stat.stdout).ok_or_else(|| {
            PersonalWorkerMacObservationError::new(
                PersonalWorkerMacObservationErrorKind::HostEvidence,
                "host.available_memory",
                "malformed_available_memory",
                "the macOS available-memory evidence was malformed or inconsistent",
            )
        })?;

        let logical_cpu = execute_exact(&bounded, &logical_cpu_command(), "host.logical_cpu")?;
        let logical_cpu_count = parse_logical_cpu_count(&logical_cpu.stdout).ok_or_else(|| {
            PersonalWorkerMacObservationError::new(
                PersonalWorkerMacObservationErrorKind::HostEvidence,
                "host.logical_cpu",
                "malformed_logical_cpu_count",
                "the macOS logical CPU evidence was malformed or outside the reviewed range",
            )
        })?;

        let lima_clock = LimaClockBridge { clock };
        let lima = lima_adapter
            .observe(lima_request, &bounded, &lima_clock)
            .map_err(PersonalWorkerMacObservationError::from_lima)?;
        let lima_profile = exact_lima_profile(lima.report())?;

        let observed_at_millis = read_clock(clock, "timing.observed_at_millis")?;
        let duration_millis = observed_at_millis
            .checked_sub(started_at_millis)
            .ok_or_else(|| {
                PersonalWorkerMacObservationError::new(
                    PersonalWorkerMacObservationErrorKind::Clock,
                    "timing.observed_at_millis",
                    "clock_reversal",
                    "the Mac observation clock moved backwards",
                )
            })?;
        if duration_millis > self.freshness_window_millis {
            return Err(PersonalWorkerMacObservationError::new(
                PersonalWorkerMacObservationErrorKind::Clock,
                "timing.duration_millis",
                "stale_observation",
                "the Mac observation exceeded its reviewed freshness window",
            ));
        }
        let expires_at_millis = observed_at_millis
            .checked_add(self.freshness_window_millis)
            .ok_or_else(|| {
                PersonalWorkerMacObservationError::new(
                    PersonalWorkerMacObservationErrorKind::Clock,
                    "timing.expires_at_millis",
                    "observation_expiry_overflow",
                    "the Mac observation expiry could not be represented",
                )
            })?;
        validate_lima_timing(lima.report(), started_at_millis, observed_at_millis)?;

        let mut host_report = host_resources.report().clone();
        host_report.observed_at_millis = observed_at_millis;
        host_report.freshness = ObservationFreshness::Fresh;
        let report = PersonalWorkerMacObservationReport {
            schema_version: PERSONAL_WORKER_MAC_OBSERVATION_SCHEMA_VERSION,
            config_identity: config.identity().clone(),
            requested_availability: config.availability(),
            timing: PersonalWorkerMacObservationTiming {
                started_at_millis,
                observed_at_millis,
                expires_at_millis,
                duration_millis,
            },
            host_headroom: MacHostHeadroomEvidence {
                available_memory_bytes,
                logical_cpu_count,
            },
            host_resources: host_report,
            lima: lima.report().clone(),
            lima_profile,
        };

        Ok(PersonalWorkerMacObservation {
            report,
            lima_source_identity: lima_request.source_identity(),
            lima_request_identity: lima_request.request_identity().clone(),
            private_evidence: PrivateEvidence {
                vm_stat,
                logical_cpu,
                host_resources,
                lima,
            },
        })
    }
}

#[must_use]
pub fn vm_stat_command() -> CommandSpec {
    CommandSpec::new(VM_STAT_PROGRAM)
}

#[must_use]
pub fn logical_cpu_command() -> CommandSpec {
    CommandSpec::new(SYSCTL_PROGRAM)
        .argument("-n")
        .argument("hw.logicalcpu")
}

pub trait PersonalWorkerMacObservationClock {
    /// Return the current Unix timestamp in milliseconds.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when a trustworthy clock value is unavailable.
    fn unix_millis(&self) -> io::Result<u64>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemPersonalWorkerMacObservationClock;

impl PersonalWorkerMacObservationClock for SystemPersonalWorkerMacObservationClock {
    fn unix_millis(&self) -> io::Result<u64> {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| u64::try_from(duration.as_millis()))
            .map_err(|_| io::Error::other("system clock precedes the Unix epoch"))?
            .map_err(|_| io::Error::other("system clock exceeds the supported range"))
    }
}

struct LimaClockBridge<'a, C> {
    clock: &'a C,
}

impl<C: PersonalWorkerMacObservationClock> LimaObservationClock for LimaClockBridge<'_, C> {
    fn unix_seconds(&self) -> io::Result<u64> {
        self.clock.unix_millis().map(|millis| millis / 1_000)
    }
}

struct TimeoutExecutor<'a, E> {
    executor: &'a E,
    timeout: Duration,
}

impl<E: TimedCommandExecutor> CommandExecutor for TimeoutExecutor<'_, E> {
    fn execute(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord> {
        self.executor.execute_with_timeout(spec, self.timeout)
    }
}

fn read_clock(
    clock: &impl PersonalWorkerMacObservationClock,
    field: &'static str,
) -> Result<u64, PersonalWorkerMacObservationError> {
    let value = clock.unix_millis().map_err(|_| {
        PersonalWorkerMacObservationError::new(
            PersonalWorkerMacObservationErrorKind::Clock,
            field,
            "clock_failure",
            "the Mac observation clock could not be read",
        )
    })?;
    if value == 0 {
        return Err(PersonalWorkerMacObservationError::new(
            PersonalWorkerMacObservationErrorKind::Clock,
            field,
            "invalid_observation_time",
            "the Mac observation time must be greater than zero",
        ));
    }
    Ok(value)
}

fn execute_exact(
    executor: &impl CommandExecutor,
    command: &CommandSpec,
    field: &'static str,
) -> Result<ExecutionRecord, PersonalWorkerMacObservationError> {
    let record = executor.execute(command).map_err(|_| {
        PersonalWorkerMacObservationError::new(
            PersonalWorkerMacObservationErrorKind::Command,
            field,
            "command_failed",
            "the reviewed Mac observation command could not be executed",
        )
    })?;
    if record.argv != command.displayed_argv()
        || record.environment_keys != command.environment.keys().cloned().collect::<Vec<_>>()
    {
        return Err(PersonalWorkerMacObservationError::new(
            PersonalWorkerMacObservationErrorKind::Command,
            field,
            "command_identity_mismatch",
            "the Mac subprocess record did not match the reviewed command identity",
        ));
    }
    if record.stdout.len() > MAX_COMMAND_OUTPUT_BYTES
        || record.stderr.len() > MAX_COMMAND_OUTPUT_BYTES
    {
        return Err(PersonalWorkerMacObservationError::new(
            PersonalWorkerMacObservationErrorKind::Command,
            field,
            "unbounded_command_output",
            "the Mac subprocess output exceeded the reviewed bound",
        ));
    }
    if record.status != Some(0)
        || !record.success
        || !record.stderr.is_empty()
        || record.stdout.contains('\0')
    {
        return Err(PersonalWorkerMacObservationError::new(
            PersonalWorkerMacObservationErrorKind::Command,
            field,
            "command_failed",
            "the reviewed Mac observation command did not complete cleanly",
        ));
    }
    Ok(record)
}

fn parse_available_memory(output: &str) -> Option<u64> {
    let mut lines = output.lines().filter(|line| !line.trim().is_empty());
    let header = lines.next()?.trim();
    let page_size = header
        .strip_prefix(VM_STAT_HEADER_PREFIX)?
        .strip_suffix(VM_STAT_HEADER_SUFFIX)?;
    let page_size = parse_canonical_u64(page_size)?;
    if !(MIN_VM_PAGE_BYTES..=MAX_VM_PAGE_BYTES).contains(&page_size) || !page_size.is_power_of_two()
    {
        return None;
    }

    let mut counts = BTreeMap::new();
    for line in lines {
        let (name, value) = line.split_once(':')?;
        let value = value.trim().strip_suffix('.')?;
        let count = parse_canonical_u64(value)?;
        if counts.insert(name.trim(), count).is_some() {
            return None;
        }
    }
    // Apple's vm_stat prints `Pages free` with speculative pages already subtracted, so each
    // reclaimable category is added exactly once. Purgeable pages are intentionally excluded
    // because they can overlap other page classes.
    let available_pages = counts
        .get("Pages free")?
        .checked_add(*counts.get("Pages inactive")?)?
        .checked_add(*counts.get("Pages speculative")?)?;
    available_pages.checked_mul(page_size)
}

fn parse_logical_cpu_count(output: &str) -> Option<u16> {
    let lines = output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    if lines.len() != 1 {
        return None;
    }
    let count = u16::try_from(parse_canonical_u64(lines[0].trim())?).ok()?;
    (1..=MAX_LOGICAL_CPUS).contains(&count).then_some(count)
}

fn parse_canonical_u64(value: &str) -> Option<u64> {
    if value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return None;
    }
    value.parse().ok()
}

fn exact_lima_profile(
    report: &LimaInstanceObservationReport,
) -> Result<LimaResourceProfile, PersonalWorkerMacObservationError> {
    for profile in [LimaResourceProfile::Interactive, LimaResourceProfile::Work] {
        let envelope = profile.envelope();
        if report.configured.cpus == envelope.vcpus
            && report.configured.memory_bytes == envelope.memory_bytes
        {
            return Ok(profile);
        }
    }
    Err(PersonalWorkerMacObservationError::new(
        PersonalWorkerMacObservationErrorKind::LimaEvidence,
        "lima.configured",
        "unsupported_lima_profile",
        "the configured Lima resources do not match a reviewed worker profile",
    ))
}

fn validate_lima_timing(
    report: &LimaInstanceObservationReport,
    outer_started_at_millis: u64,
    outer_observed_at_millis: u64,
) -> Result<(), PersonalWorkerMacObservationError> {
    let outer_started_seconds = outer_started_at_millis / 1_000;
    let outer_observed_seconds = outer_observed_at_millis / 1_000;
    if report.timing.started_at_unix_seconds < outer_started_seconds
        || report.timing.observed_at_unix_seconds > outer_observed_seconds
        || report.timing.freshness_at(outer_observed_seconds) != LimaObservationFreshness::Fresh
    {
        return Err(PersonalWorkerMacObservationError::new(
            PersonalWorkerMacObservationErrorKind::LimaEvidence,
            "lima.timing",
            "lima_timing_mismatch",
            "the Lima observation timing does not fit the enclosing Mac observation window",
        ));
    }
    Ok(())
}
