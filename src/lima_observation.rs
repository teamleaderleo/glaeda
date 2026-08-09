use std::collections::BTreeSet;
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde::de::{self, Deserialize, Deserializer, IgnoredAny, MapAccess, Visitor};
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::process::{CommandExecutor, CommandSpec, ExecutionRecord};

pub const LIMA_OBSERVATION_SCHEMA_VERSION: u8 = 1;
pub const MAX_LIMA_OBSERVATION_OUTPUT_BYTES: usize = 65_536;
const MAX_INSTANCE_NAME_BYTES: usize = 64;
const MAX_PATH_BYTES: usize = 1_024;
const MAX_GUEST_MEMORY_FIXED_DEFICIT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_REVIEWED_CPUS: u16 = 1_024;
const GUEST_UNAME: &str = "/usr/bin/uname";
const GUEST_GETCONF: &str = "/usr/bin/getconf";
const GUEST_SHA256SUM: &str = "/usr/bin/sha256sum";
const GUEST_STAT: &str = "/usr/bin/stat";
const GUEST_MACHINE_ID: &str = "/etc/machine-id";
const REDACTED_PRIVATE_EVIDENCE: &str = "<private-lima-command-evidence>";
const LIMA_OBSERVATION_REQUEST_IDENTITY_DOCUMENT_TYPE: &str =
    "smolrunner-lima-observation-request-identity";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LimaObservationRefusalCode {
    InvalidInput,
    ClockFailure,
    StaleObservation,
    CommandFailed,
    CommandIdentityMismatch,
    UnboundedOutput,
    MissingInstanceEvidence,
    DuplicateInstanceEvidence,
    MalformedInstanceEvidence,
    AliasedEvidence,
    InstanceMismatch,
    InstanceDirectoryMismatch,
    InstanceDrift,
    VmTypeMismatch,
    ArchitectureMismatch,
    MissingGuestEvidence,
    MalformedGuestEvidence,
    GuestArchitectureMismatch,
    GuestCpuMismatch,
    GuestMemoryMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LimaObservationPhase {
    InputValidation,
    InstanceObservation,
    GuestArchitecture,
    GuestCpu,
    GuestMemory,
    GuestMachineIdentity,
    GuestRootIdentity,
    GuestCacheIdentity,
    Freshness,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ObservationProblem {
    code: LimaObservationRefusalCode,
    phase: LimaObservationPhase,
    message: &'static str,
}

impl ObservationProblem {
    const fn new(
        code: LimaObservationRefusalCode,
        phase: LimaObservationPhase,
        message: &'static str,
    ) -> Self {
        Self {
            code,
            phase,
            message,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct LimaObservationFailure {
    pub code: LimaObservationRefusalCode,
    pub phase: LimaObservationPhase,
    pub public_message: &'static str,
    #[serde(skip)]
    private_evidence: LimaObservationPrivateEvidence,
}

impl LimaObservationFailure {
    fn from_problem(
        problem: ObservationProblem,
        private_evidence: LimaObservationPrivateEvidence,
    ) -> Self {
        Self {
            code: problem.code,
            phase: problem.phase,
            public_message: problem.message,
            private_evidence,
        }
    }

    #[must_use]
    pub const fn private_evidence(&self) -> &LimaObservationPrivateEvidence {
        &self.private_evidence
    }
}

impl fmt::Debug for LimaObservationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LimaObservationFailure")
            .field("code", &self.code)
            .field("phase", &self.phase)
            .field("public_message", &self.public_message)
            .field("private_evidence", &REDACTED_PRIVATE_EVIDENCE)
            .finish()
    }
}

impl fmt::Display for LimaObservationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.public_message)
    }
}

impl std::error::Error for LimaObservationFailure {}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct LimaInstanceName(String);

impl LimaInstanceName {
    /// Validate one exact non-option Lima instance name.
    ///
    /// # Errors
    ///
    /// Returns a bounded error unless the name is a simple ASCII Lima identifier.
    pub fn parse(value: &str) -> Result<Self, LimaObservationFailure> {
        if !valid_instance_name(value) {
            return Err(input_failure(
                "the Lima instance name must be one bounded non-option ASCII identifier",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LimaRuntimeState {
    Uninitialized,
    Installing,
    Broken,
    Stopped,
    Running,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LimaVmType {
    Vz,
    Qemu,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LimaArchitecture {
    Aarch64,
    X86_64,
}

/// Opaque equality-only identity for one exact private Lima source.
///
/// This value intentionally has no serialization or path accessor. It lets higher-level adapters
/// prove that independently constructed requests target the same validated instance and private
/// Lima home without disclosing that home.
#[derive(Clone, PartialEq, Eq)]
pub struct LimaObservationSourceIdentity {
    instance: LimaInstanceName,
    lima_home: PathBuf,
}

/// Opaque canonical digest for every semantic field in one validated Lima observation request.
///
/// Unlike [`LimaObservationSourceIdentity`], this identity also binds the expected VM type,
/// architecture, guest cache path, and freshness window. It exposes only a SHA-256 digest and
/// never the private paths used to derive it.
#[derive(Clone, PartialEq, Eq)]
pub struct LimaObservationRequestIdentity {
    digest: Sha256Digest,
}

impl LimaObservationRequestIdentity {
    #[must_use]
    pub const fn digest(&self) -> &Sha256Digest {
        &self.digest
    }
}

impl fmt::Debug for LimaObservationRequestIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LimaObservationRequestIdentity")
            .field("digest", &self.digest)
            .finish()
    }
}

impl LimaObservationSourceIdentity {
    pub(crate) const fn from_validated(instance: LimaInstanceName, lima_home: PathBuf) -> Self {
        Self {
            instance,
            lima_home,
        }
    }
}

impl fmt::Debug for LimaObservationSourceIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LimaObservationSourceIdentity")
            .field("instance", &self.instance)
            .field("lima_home", &"<private-lima-home>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct LimaObservationRequest {
    instance: LimaInstanceName,
    lima_home: PathBuf,
    expected_instance_directory: PathBuf,
    expected_vm_type: LimaVmType,
    expected_architecture: LimaArchitecture,
    guest_cache_path: PathBuf,
    max_age_seconds: u64,
    request_identity: LimaObservationRequestIdentity,
}

impl LimaObservationRequest {
    /// Build one exact observation request for a reviewed persistent Lima instance.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for aliased or non-absolute private paths, a mismatched instance
    /// directory, or a zero freshness window.
    pub fn new(
        instance: LimaInstanceName,
        lima_home: impl Into<PathBuf>,
        expected_vm_type: LimaVmType,
        expected_architecture: LimaArchitecture,
        guest_cache_path: impl Into<PathBuf>,
        max_age_seconds: u64,
    ) -> Result<Self, LimaObservationFailure> {
        let lima_home = validate_private_absolute_path(lima_home.into(), false)?;
        let guest_cache_path = validate_private_absolute_path(guest_cache_path.into(), false)?;
        if max_age_seconds == 0 {
            return Err(input_failure(
                "the Lima observation freshness window must be nonzero",
            ));
        }
        let expected_instance_directory = lima_home.join(instance.as_str());
        let request_identity = digest_observation_request(
            &instance,
            &lima_home,
            expected_vm_type,
            expected_architecture,
            &guest_cache_path,
            max_age_seconds,
        )?;
        Ok(Self {
            instance,
            lima_home,
            expected_instance_directory,
            expected_vm_type,
            expected_architecture,
            guest_cache_path,
            max_age_seconds,
            request_identity,
        })
    }

    #[must_use]
    pub const fn instance(&self) -> &LimaInstanceName {
        &self.instance
    }

    #[must_use]
    pub const fn expected_vm_type(&self) -> LimaVmType {
        self.expected_vm_type
    }

    #[must_use]
    pub const fn expected_architecture(&self) -> LimaArchitecture {
        self.expected_architecture
    }

    #[must_use]
    pub const fn max_age_seconds(&self) -> u64 {
        self.max_age_seconds
    }

    #[must_use]
    pub fn source_identity(&self) -> LimaObservationSourceIdentity {
        LimaObservationSourceIdentity::from_validated(self.instance.clone(), self.lima_home.clone())
    }

    #[must_use]
    pub const fn request_identity(&self) -> &LimaObservationRequestIdentity {
        &self.request_identity
    }
}

impl fmt::Debug for LimaObservationRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LimaObservationRequest")
            .field("instance", &self.instance)
            .field("lima_home", &"<private-lima-home>")
            .field(
                "expected_instance_directory",
                &"<private-instance-directory>",
            )
            .field("expected_vm_type", &self.expected_vm_type)
            .field("expected_architecture", &self.expected_architecture)
            .field("guest_cache_path", &"<private-guest-cache-path>")
            .field("max_age_seconds", &self.max_age_seconds)
            .finish()
    }
}

#[derive(Serialize)]
struct LimaObservationRequestIdentityDocument<'a> {
    document_type: &'static str,
    schema_version: u8,
    instance: &'a str,
    lima_home: &'a str,
    expected_vm_type: LimaVmType,
    expected_architecture: LimaArchitecture,
    guest_cache_path: &'a str,
    max_age_seconds: u64,
}

fn digest_observation_request(
    instance: &LimaInstanceName,
    lima_home: &Path,
    expected_vm_type: LimaVmType,
    expected_architecture: LimaArchitecture,
    guest_cache_path: &Path,
    max_age_seconds: u64,
) -> Result<LimaObservationRequestIdentity, LimaObservationFailure> {
    let document = LimaObservationRequestIdentityDocument {
        document_type: LIMA_OBSERVATION_REQUEST_IDENTITY_DOCUMENT_TYPE,
        schema_version: LIMA_OBSERVATION_SCHEMA_VERSION,
        instance: instance.as_str(),
        lima_home: lima_home
            .to_str()
            .expect("validated Lima home remains exact UTF-8"),
        expected_vm_type,
        expected_architecture,
        guest_cache_path: guest_cache_path
            .to_str()
            .expect("validated guest cache path remains exact UTF-8"),
        max_age_seconds,
    };
    let bytes = serde_json::to_vec(&document)
        .map_err(|_| input_failure("the Lima observation request identity could not be encoded"))?;
    let digest = Sha256::digest(bytes);
    let digest = Sha256Digest::parse(&format!("sha256:{digest:x}")).map_err(|_| {
        input_failure("the Lima observation request identity could not be represented")
    })?;
    Ok(LimaObservationRequestIdentity { digest })
}

#[derive(Clone, PartialEq, Eq)]
pub struct LimaObservationAdapter {
    limactl_program: PathBuf,
}

impl LimaObservationAdapter {
    /// Construct a narrow direct `limactl` observation adapter.
    ///
    /// # Errors
    ///
    /// Returns a bounded error unless the reviewed `limactl` program path is absolute and
    /// unaliased.
    pub fn new(limactl_program: impl Into<PathBuf>) -> Result<Self, LimaObservationFailure> {
        Ok(Self {
            limactl_program: validate_private_absolute_path(limactl_program.into(), false)?,
        })
    }

    /// Observe one configured Lima instance and, when running, its exact guest resource identity.
    ///
    /// Every subprocess is one fixed direct `limactl` invocation. The process environment contains
    /// only `LIMA_HOME`, `LANG`, and `LC_ALL`; no shell, generic guest command, profile decision,
    /// lifecycle mutation, credential, timer, runner control, or cache mutation is available.
    ///
    /// # Errors
    ///
    /// Returns a bounded public refusal carrying private raw command evidence separately.
    pub fn observe(
        &self,
        request: &LimaObservationRequest,
        executor: &impl CommandExecutor,
        clock: &impl LimaObservationClock,
    ) -> Result<LimaInstanceObservation, LimaObservationFailure> {
        let mut evidence = LimaObservationPrivateEvidence::default();
        let result = self.observe_inner(request, executor, clock, &mut evidence);
        match result {
            Ok(public) => Ok(LimaInstanceObservation {
                public,
                private_evidence: evidence,
            }),
            Err(problem) => Err(LimaObservationFailure::from_problem(problem, evidence)),
        }
    }

    fn observe_inner(
        &self,
        request: &LimaObservationRequest,
        executor: &impl CommandExecutor,
        clock: &impl LimaObservationClock,
        evidence: &mut LimaObservationPrivateEvidence,
    ) -> Result<LimaInstanceObservationReport, ObservationProblem> {
        let started_at_unix_seconds = clock.unix_seconds().map_err(|_| {
            ObservationProblem::new(
                LimaObservationRefusalCode::ClockFailure,
                LimaObservationPhase::Freshness,
                "the Lima observation start time could not be recorded",
            )
        })?;

        let list_output = self.run_command(
            executor,
            evidence,
            LimaObservationPhase::InstanceObservation,
            self.list_command(request),
        )?;
        let raw_instance = parse_instance_output(&list_output)?;
        let configured = validate_instance_evidence(request, raw_instance)?;

        let guest = if configured.runtime_state == LimaRuntimeState::Running {
            LimaGuestObservation::Observed(self.observe_guest(
                request,
                executor,
                evidence,
                &configured,
            )?)
        } else {
            LimaGuestObservation::NotRunning {
                runtime_state: configured.runtime_state,
            }
        };

        let final_list_output = self.run_command(
            executor,
            evidence,
            LimaObservationPhase::InstanceObservation,
            self.list_command(request),
        )?;
        let final_raw_instance = parse_instance_output(&final_list_output)?;
        let final_configured = validate_instance_evidence(request, final_raw_instance)?;
        if final_configured != configured {
            return Err(ObservationProblem::new(
                LimaObservationRefusalCode::InstanceDrift,
                LimaObservationPhase::InstanceObservation,
                "the Lima instance runtime or configured resources changed during observation",
            ));
        }

        let observed_at_unix_seconds = clock.unix_seconds().map_err(|_| {
            ObservationProblem::new(
                LimaObservationRefusalCode::ClockFailure,
                LimaObservationPhase::Freshness,
                "the Lima observation completion time could not be recorded",
            )
        })?;
        let duration_seconds = observed_at_unix_seconds
            .checked_sub(started_at_unix_seconds)
            .ok_or_else(|| {
                ObservationProblem::new(
                    LimaObservationRefusalCode::ClockFailure,
                    LimaObservationPhase::Freshness,
                    "the Lima observation clock moved backwards",
                )
            })?;
        if duration_seconds > request.max_age_seconds {
            return Err(ObservationProblem::new(
                LimaObservationRefusalCode::StaleObservation,
                LimaObservationPhase::Freshness,
                "the Lima observation exceeded its reviewed freshness window",
            ));
        }
        let expires_at_unix_seconds = observed_at_unix_seconds
            .checked_add(request.max_age_seconds)
            .ok_or_else(|| {
                ObservationProblem::new(
                    LimaObservationRefusalCode::ClockFailure,
                    LimaObservationPhase::Freshness,
                    "the Lima observation expiry time could not be represented",
                )
            })?;

        Ok(LimaInstanceObservationReport {
            schema_version: LIMA_OBSERVATION_SCHEMA_VERSION,
            instance: request.instance.clone(),
            configured,
            guest,
            timing: LimaObservationTiming {
                started_at_unix_seconds,
                observed_at_unix_seconds,
                expires_at_unix_seconds,
                duration_seconds,
                freshness: LimaObservationFreshness::Fresh,
            },
        })
    }

    fn observe_guest(
        &self,
        request: &LimaObservationRequest,
        executor: &impl CommandExecutor,
        evidence: &mut LimaObservationPrivateEvidence,
        configured: &LimaConfiguredInstance,
    ) -> Result<LimaObservedGuest, ObservationProblem> {
        let architecture = parse_guest_architecture(&self.run_command(
            executor,
            evidence,
            LimaObservationPhase::GuestArchitecture,
            self.guest_command(request, GUEST_UNAME, ["-m"], None),
        )?)?;
        if architecture != configured.architecture {
            return Err(ObservationProblem::new(
                LimaObservationRefusalCode::GuestArchitectureMismatch,
                LimaObservationPhase::GuestArchitecture,
                "the running guest architecture differs from the configured Lima architecture",
            ));
        }

        let cpus = parse_canonical_u64_line(
            &self.run_command(
                executor,
                evidence,
                LimaObservationPhase::GuestCpu,
                self.guest_command(request, GUEST_GETCONF, ["_NPROCESSORS_ONLN"], None),
            )?,
            LimaObservationPhase::GuestCpu,
        )?;
        let cpus = u16::try_from(cpus).map_err(|_| {
            malformed_guest(
                LimaObservationPhase::GuestCpu,
                "the running guest CPU count is outside the reviewed range",
            )
        })?;
        if cpus == 0 || cpus > MAX_REVIEWED_CPUS {
            return Err(malformed_guest(
                LimaObservationPhase::GuestCpu,
                "the running guest CPU count is outside the reviewed range",
            ));
        }
        if cpus != configured.cpus {
            return Err(ObservationProblem::new(
                LimaObservationRefusalCode::GuestCpuMismatch,
                LimaObservationPhase::GuestCpu,
                "the running guest CPU count differs from the configured Lima envelope",
            ));
        }

        let page_size = parse_canonical_u64_line(
            &self.run_command(
                executor,
                evidence,
                LimaObservationPhase::GuestMemory,
                self.guest_command(request, GUEST_GETCONF, ["PAGE_SIZE"], None),
            )?,
            LimaObservationPhase::GuestMemory,
        )?;
        let physical_pages = parse_canonical_u64_line(
            &self.run_command(
                executor,
                evidence,
                LimaObservationPhase::GuestMemory,
                self.guest_command(request, GUEST_GETCONF, ["_PHYS_PAGES"], None),
            )?,
            LimaObservationPhase::GuestMemory,
        )?;
        let memory_bytes = page_size.checked_mul(physical_pages).ok_or_else(|| {
            malformed_guest(
                LimaObservationPhase::GuestMemory,
                "the running guest memory observation overflowed",
            )
        })?;
        validate_guest_memory(configured.memory_bytes, memory_bytes)?;

        let machine_id_digest = parse_machine_id_digest(&self.run_command(
            executor,
            evidence,
            LimaObservationPhase::GuestMachineIdentity,
            self.guest_command(request, GUEST_SHA256SUM, ["--", GUEST_MACHINE_ID], None),
        )?)?;
        let root_filesystem = parse_filesystem_identity(
            &self.run_command(
                executor,
                evidence,
                LimaObservationPhase::GuestRootIdentity,
                self.guest_command(request, GUEST_STAT, ["-Lc", "%d:%i", "--", "/"], None),
            )?,
            LimaObservationPhase::GuestRootIdentity,
        )?;
        let cache_directory = parse_filesystem_identity(
            &self.run_command(
                executor,
                evidence,
                LimaObservationPhase::GuestCacheIdentity,
                self.guest_command(
                    request,
                    GUEST_STAT,
                    ["-Lc", "%d:%i", "--"],
                    Some(&request.guest_cache_path),
                ),
            )?,
            LimaObservationPhase::GuestCacheIdentity,
        )?;

        Ok(LimaObservedGuest {
            resources: LimaGuestResources {
                architecture,
                cpus,
                memory_bytes,
            },
            persistent_identity: LimaPersistentIdentity {
                guest_machine_id_digest: machine_id_digest,
                root_filesystem,
                cache_directory,
            },
        })
    }

    fn list_command(&self, request: &LimaObservationRequest) -> CommandSpec {
        self.base_command(request)
            .argument("--tty=false")
            .argument("list")
            .argument("--format=json")
            .argument("--all-fields")
            .argument(request.instance.as_str())
    }

    fn guest_command<const N: usize>(
        &self,
        request: &LimaObservationRequest,
        guest_program: &str,
        arguments: [&str; N],
        private_path: Option<&Path>,
    ) -> CommandSpec {
        let mut command = self
            .base_command(request)
            .argument("--tty=false")
            .argument("shell")
            .argument(request.instance.as_str())
            .argument("--")
            .argument(guest_program);
        for argument in arguments {
            command = command.argument(argument);
        }
        if let Some(path) = private_path {
            command = command.secret_argument(exact_private_path(path));
        }
        command
    }

    fn base_command(&self, request: &LimaObservationRequest) -> CommandSpec {
        CommandSpec::new(&self.limactl_program)
            .environment("LIMA_HOME", exact_private_path(&request.lima_home))
            .environment("LANG", "C")
            .environment("LC_ALL", "C")
    }

    fn run_command(
        &self,
        executor: &impl CommandExecutor,
        evidence: &mut LimaObservationPrivateEvidence,
        phase: LimaObservationPhase,
        command: CommandSpec,
    ) -> Result<String, ObservationProblem> {
        let record = executor.execute(&command).map_err(|_| {
            ObservationProblem::new(
                LimaObservationRefusalCode::CommandFailed,
                phase,
                "the reviewed Lima observation command could not be executed",
            )
        })?;
        evidence.commands.push(LimaPrivateCommandEvidence {
            phase,
            record: record.clone(),
        });

        if record.argv != command.displayed_argv()
            || record.environment_keys != command.environment.keys().cloned().collect::<Vec<_>>()
        {
            return Err(ObservationProblem::new(
                LimaObservationRefusalCode::CommandIdentityMismatch,
                phase,
                "the Lima subprocess record does not match the reviewed command identity",
            ));
        }
        if record.stdout.len() > MAX_LIMA_OBSERVATION_OUTPUT_BYTES
            || record.stderr.len() > MAX_LIMA_OBSERVATION_OUTPUT_BYTES
        {
            return Err(ObservationProblem::new(
                LimaObservationRefusalCode::UnboundedOutput,
                phase,
                "the Lima subprocess output exceeded the reviewed observation bound",
            ));
        }
        if record.status != Some(0) || !record.success || !record.stderr.is_empty() {
            return Err(ObservationProblem::new(
                LimaObservationRefusalCode::CommandFailed,
                phase,
                "the reviewed Lima observation command did not complete cleanly",
            ));
        }
        if record.stdout.contains('\0') {
            let code = if phase == LimaObservationPhase::InstanceObservation {
                LimaObservationRefusalCode::MalformedInstanceEvidence
            } else {
                LimaObservationRefusalCode::MalformedGuestEvidence
            };
            return Err(ObservationProblem::new(
                code,
                phase,
                "the Lima observation command returned malformed evidence",
            ));
        }
        Ok(record.stdout)
    }
}

impl fmt::Debug for LimaObservationAdapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LimaObservationAdapter")
            .field("limactl_program", &"<reviewed-absolute-limactl>")
            .finish()
    }
}

pub trait LimaObservationClock {
    /// Return the current Unix timestamp in whole seconds.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when a trustworthy clock value is unavailable.
    fn unix_seconds(&self) -> io::Result<u64>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemLimaObservationClock;

impl LimaObservationClock for SystemLimaObservationClock {
    fn unix_seconds(&self) -> io::Result<u64> {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .map_err(|_| io::Error::other("system clock precedes the Unix epoch"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LimaObservationFreshness {
    Fresh,
    Stale,
    Future,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LimaObservationTiming {
    pub started_at_unix_seconds: u64,
    pub observed_at_unix_seconds: u64,
    pub expires_at_unix_seconds: u64,
    pub duration_seconds: u64,
    pub freshness: LimaObservationFreshness,
}

impl LimaObservationTiming {
    #[must_use]
    pub const fn freshness_at(&self, unix_seconds: u64) -> LimaObservationFreshness {
        if unix_seconds < self.observed_at_unix_seconds {
            LimaObservationFreshness::Future
        } else if unix_seconds <= self.expires_at_unix_seconds {
            LimaObservationFreshness::Fresh
        } else {
            LimaObservationFreshness::Stale
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LimaConfiguredInstance {
    pub runtime_state: LimaRuntimeState,
    pub vm_type: LimaVmType,
    pub architecture: LimaArchitecture,
    pub cpus: u16,
    pub memory_bytes: u64,
    pub primary_disk_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LimaGuestResources {
    pub architecture: LimaArchitecture,
    pub cpus: u16,
    pub memory_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LimaFilesystemObjectIdentity {
    pub device_id: u64,
    pub inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LimaPersistentIdentity {
    pub guest_machine_id_digest: Sha256Digest,
    pub root_filesystem: LimaFilesystemObjectIdentity,
    pub cache_directory: LimaFilesystemObjectIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LimaObservedGuest {
    pub resources: LimaGuestResources,
    pub persistent_identity: LimaPersistentIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", content = "evidence", rename_all = "snake_case")]
pub enum LimaGuestObservation {
    NotRunning { runtime_state: LimaRuntimeState },
    Observed(LimaObservedGuest),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LimaInstanceObservationReport {
    pub schema_version: u8,
    pub instance: LimaInstanceName,
    pub configured: LimaConfiguredInstance,
    pub guest: LimaGuestObservation,
    pub timing: LimaObservationTiming,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct LimaInstanceObservation {
    #[serde(flatten)]
    public: LimaInstanceObservationReport,
    #[serde(skip)]
    private_evidence: LimaObservationPrivateEvidence,
}

impl LimaInstanceObservation {
    #[must_use]
    pub const fn report(&self) -> &LimaInstanceObservationReport {
        &self.public
    }

    #[must_use]
    pub const fn private_evidence(&self) -> &LimaObservationPrivateEvidence {
        &self.private_evidence
    }
}

impl fmt::Debug for LimaInstanceObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LimaInstanceObservation")
            .field("public", &self.public)
            .field("private_evidence", &REDACTED_PRIVATE_EVIDENCE)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Default)]
pub struct LimaObservationPrivateEvidence {
    commands: Vec<LimaPrivateCommandEvidence>,
}

impl LimaObservationPrivateEvidence {
    #[must_use]
    pub fn commands(&self) -> &[LimaPrivateCommandEvidence] {
        &self.commands
    }
}

impl fmt::Debug for LimaObservationPrivateEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LimaObservationPrivateEvidence")
            .field("command_count", &self.commands.len())
            .field("raw_output", &REDACTED_PRIVATE_EVIDENCE)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct LimaPrivateCommandEvidence {
    phase: LimaObservationPhase,
    record: ExecutionRecord,
}

impl LimaPrivateCommandEvidence {
    #[must_use]
    pub const fn phase(&self) -> LimaObservationPhase {
        self.phase
    }

    #[must_use]
    pub const fn record(&self) -> &ExecutionRecord {
        &self.record
    }
}

impl fmt::Debug for LimaPrivateCommandEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LimaPrivateCommandEvidence")
            .field("phase", &self.phase)
            .field("record", &REDACTED_PRIVATE_EVIDENCE)
            .finish()
    }
}

#[derive(Debug)]
struct RawInstanceEvidence {
    name: String,
    status: String,
    directory: String,
    vm_type: String,
    architecture: String,
    cpus: i64,
    memory_bytes: i64,
    disk_bytes: i64,
    errors: Vec<String>,
}

impl<'de> Deserialize<'de> for RawInstanceEvidence {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct RawInstanceVisitor;

        impl<'de> Visitor<'de> for RawInstanceVisitor {
            type Value = RawInstanceEvidence;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("one Lima instance JSON object")
            }

            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut seen = BTreeSet::new();
                let mut name = None;
                let mut status = None;
                let mut directory = None;
                let mut vm_type = None;
                let mut architecture = None;
                let mut cpus = None;
                let mut memory_bytes = None;
                let mut disk_bytes = None;
                let mut errors = None;

                while let Some(key) = map.next_key::<String>()? {
                    if !seen.insert(key.clone()) {
                        return Err(de::Error::custom("duplicate Lima instance JSON field"));
                    }
                    match key.as_str() {
                        "name" => name = Some(map.next_value()?),
                        "status" => status = Some(map.next_value()?),
                        "dir" => directory = Some(map.next_value()?),
                        "vmType" => vm_type = Some(map.next_value()?),
                        "arch" => architecture = Some(map.next_value()?),
                        "cpus" => cpus = Some(map.next_value()?),
                        "memory" => memory_bytes = Some(map.next_value()?),
                        "disk" => disk_bytes = Some(map.next_value()?),
                        "errors" => errors = Some(map.next_value()?),
                        _ => {
                            let _: IgnoredAny = map.next_value()?;
                        }
                    }
                }

                Ok(RawInstanceEvidence {
                    name: name.ok_or_else(|| de::Error::missing_field("name"))?,
                    status: status.ok_or_else(|| de::Error::missing_field("status"))?,
                    directory: directory.ok_or_else(|| de::Error::missing_field("dir"))?,
                    vm_type: vm_type.ok_or_else(|| de::Error::missing_field("vmType"))?,
                    architecture: architecture.ok_or_else(|| de::Error::missing_field("arch"))?,
                    cpus: cpus.ok_or_else(|| de::Error::missing_field("cpus"))?,
                    memory_bytes: memory_bytes.ok_or_else(|| de::Error::missing_field("memory"))?,
                    disk_bytes: disk_bytes.ok_or_else(|| de::Error::missing_field("disk"))?,
                    errors: errors.unwrap_or_default(),
                })
            }
        }

        deserializer.deserialize_map(RawInstanceVisitor)
    }
}

fn parse_instance_output(output: &str) -> Result<RawInstanceEvidence, ObservationProblem> {
    if output.is_empty() || output == "\n" {
        return Err(ObservationProblem::new(
            LimaObservationRefusalCode::MissingInstanceEvidence,
            LimaObservationPhase::InstanceObservation,
            "limactl did not return the requested instance evidence",
        ));
    }
    let Some(line) = output.strip_suffix('\n') else {
        return Err(malformed_instance(
            "the Lima instance JSON evidence is not one complete line",
        ));
    };
    if line.is_empty() {
        return Err(ObservationProblem::new(
            LimaObservationRefusalCode::MissingInstanceEvidence,
            LimaObservationPhase::InstanceObservation,
            "limactl did not return the requested instance evidence",
        ));
    }
    if line.contains('\n') {
        return Err(ObservationProblem::new(
            LimaObservationRefusalCode::DuplicateInstanceEvidence,
            LimaObservationPhase::InstanceObservation,
            "limactl returned more than one instance observation",
        ));
    }
    if line.contains('\r') {
        return Err(malformed_instance(
            "the Lima instance JSON evidence contains an invalid line ending",
        ));
    }

    let mut deserializer = serde_json::Deserializer::from_str(line);
    let evidence = RawInstanceEvidence::deserialize(&mut deserializer).map_err(|error| {
        if error
            .to_string()
            .contains("duplicate Lima instance JSON field")
        {
            ObservationProblem::new(
                LimaObservationRefusalCode::DuplicateInstanceEvidence,
                LimaObservationPhase::InstanceObservation,
                "the Lima instance JSON evidence contains duplicate fields",
            )
        } else {
            malformed_instance("the Lima instance JSON evidence is malformed")
        }
    })?;
    deserializer
        .end()
        .map_err(|_| malformed_instance("the Lima instance JSON evidence has trailing data"))?;
    Ok(evidence)
}

fn validate_instance_evidence(
    request: &LimaObservationRequest,
    raw: RawInstanceEvidence,
) -> Result<LimaConfiguredInstance, ObservationProblem> {
    if !raw.errors.is_empty() {
        return Err(malformed_instance(
            "limactl reported incomplete or broken instance evidence",
        ));
    }
    let observed_name = LimaInstanceName::parse(&raw.name)
        .map_err(|_| malformed_instance("the observed Lima instance name is invalid"))?;
    if observed_name != request.instance {
        return Err(ObservationProblem::new(
            LimaObservationRefusalCode::InstanceMismatch,
            LimaObservationPhase::InstanceObservation,
            "limactl returned a different instance identity",
        ));
    }
    let observed_directory = validate_observed_absolute_path(&raw.directory)?;
    if observed_directory != request.expected_instance_directory {
        return Err(ObservationProblem::new(
            LimaObservationRefusalCode::InstanceDirectoryMismatch,
            LimaObservationPhase::InstanceObservation,
            "the Lima instance directory differs from the reviewed persistent identity",
        ));
    }

    let runtime_state = parse_runtime_state(&raw.status)?;
    let vm_type = parse_vm_type(&raw.vm_type)?;
    if vm_type != request.expected_vm_type {
        return Err(ObservationProblem::new(
            LimaObservationRefusalCode::VmTypeMismatch,
            LimaObservationPhase::InstanceObservation,
            "the configured Lima VM type differs from the reviewed instance identity",
        ));
    }
    if matches!(raw.architecture.as_str(), "arm64" | "amd64") {
        return Err(ObservationProblem::new(
            LimaObservationRefusalCode::AliasedEvidence,
            LimaObservationPhase::InstanceObservation,
            "the configured Lima architecture uses an unreviewed alias",
        ));
    }
    let architecture = parse_architecture(&raw.architecture).map_err(|_| {
        malformed_instance("the configured Lima architecture is not supported by this observer")
    })?;
    if architecture != request.expected_architecture {
        return Err(ObservationProblem::new(
            LimaObservationRefusalCode::ArchitectureMismatch,
            LimaObservationPhase::InstanceObservation,
            "the configured Lima architecture differs from the reviewed instance identity",
        ));
    }

    let cpus = u16::try_from(raw.cpus).map_err(|_| {
        malformed_instance("the configured Lima CPU envelope is outside the reviewed range")
    })?;
    if cpus == 0 || cpus > MAX_REVIEWED_CPUS {
        return Err(malformed_instance(
            "the configured Lima CPU envelope is outside the reviewed range",
        ));
    }
    let memory_bytes = u64::try_from(raw.memory_bytes)
        .map_err(|_| malformed_instance("the configured Lima memory envelope must be positive"))?;
    let primary_disk_bytes = u64::try_from(raw.disk_bytes)
        .map_err(|_| malformed_instance("the configured Lima disk envelope must be positive"))?;
    if memory_bytes == 0 || primary_disk_bytes == 0 {
        return Err(malformed_instance(
            "the configured Lima resource envelope must be complete and positive",
        ));
    }

    Ok(LimaConfiguredInstance {
        runtime_state,
        vm_type,
        architecture,
        cpus,
        memory_bytes,
        primary_disk_bytes,
    })
}

fn parse_runtime_state(value: &str) -> Result<LimaRuntimeState, ObservationProblem> {
    match value {
        "Uninitialized" => Ok(LimaRuntimeState::Uninitialized),
        "Installing" => Ok(LimaRuntimeState::Installing),
        "Broken" => Ok(LimaRuntimeState::Broken),
        "Stopped" => Ok(LimaRuntimeState::Stopped),
        "Running" => Ok(LimaRuntimeState::Running),
        _ => Err(malformed_instance(
            "the Lima runtime state is not one reviewed exact state",
        )),
    }
}

fn parse_vm_type(value: &str) -> Result<LimaVmType, ObservationProblem> {
    match value {
        "vz" => Ok(LimaVmType::Vz),
        "qemu" => Ok(LimaVmType::Qemu),
        _ => Err(malformed_instance(
            "the configured Lima VM type is not supported by this observer",
        )),
    }
}

fn parse_architecture(value: &str) -> Result<LimaArchitecture, ObservationProblem> {
    match value {
        "aarch64" => Ok(LimaArchitecture::Aarch64),
        "x86_64" => Ok(LimaArchitecture::X86_64),
        _ => Err(ObservationProblem::new(
            LimaObservationRefusalCode::ArchitectureMismatch,
            LimaObservationPhase::InstanceObservation,
            "the Lima architecture is not one reviewed exact architecture",
        )),
    }
}

fn parse_guest_architecture(output: &str) -> Result<LimaArchitecture, ObservationProblem> {
    let value = parse_single_guest_line(output, LimaObservationPhase::GuestArchitecture)?;
    if value == "arm64" || value == "amd64" {
        return Err(ObservationProblem::new(
            LimaObservationRefusalCode::AliasedEvidence,
            LimaObservationPhase::GuestArchitecture,
            "the running guest architecture uses an unreviewed alias",
        ));
    }
    parse_architecture(value).map_err(|_| {
        malformed_guest(
            LimaObservationPhase::GuestArchitecture,
            "the running guest architecture is not one reviewed exact architecture",
        )
    })
}

fn parse_canonical_u64_line(
    output: &str,
    phase: LimaObservationPhase,
) -> Result<u64, ObservationProblem> {
    let value = parse_single_guest_line(output, phase)?;
    parse_canonical_u64(value).ok_or_else(|| {
        malformed_guest(phase, "the running guest numeric evidence is not canonical")
    })
}

fn parse_machine_id_digest(output: &str) -> Result<Sha256Digest, ObservationProblem> {
    let value = parse_single_guest_line(output, LimaObservationPhase::GuestMachineIdentity)?;
    let Some((digest, path)) = value.split_once("  ") else {
        return Err(malformed_guest(
            LimaObservationPhase::GuestMachineIdentity,
            "the guest machine identity digest is malformed",
        ));
    };
    if path != GUEST_MACHINE_ID || digest.len() != 64 || !digest.bytes().all(is_lower_hex) {
        return Err(malformed_guest(
            LimaObservationPhase::GuestMachineIdentity,
            "the guest machine identity digest is malformed",
        ));
    }
    Sha256Digest::parse(&format!("sha256:{digest}")).map_err(|_| {
        malformed_guest(
            LimaObservationPhase::GuestMachineIdentity,
            "the guest machine identity digest is malformed",
        )
    })
}

fn parse_filesystem_identity(
    output: &str,
    phase: LimaObservationPhase,
) -> Result<LimaFilesystemObjectIdentity, ObservationProblem> {
    let value = parse_single_guest_line(output, phase)?;
    let Some((device, inode)) = value.split_once(':') else {
        return Err(malformed_guest(
            phase,
            "the guest filesystem identity is malformed",
        ));
    };
    if inode.contains(':') {
        return Err(malformed_guest(
            phase,
            "the guest filesystem identity is malformed",
        ));
    }
    let device_id = parse_canonical_u64(device).ok_or_else(|| {
        malformed_guest(phase, "the guest filesystem device identity is malformed")
    })?;
    let inode = parse_canonical_u64(inode).ok_or_else(|| {
        malformed_guest(phase, "the guest filesystem inode identity is malformed")
    })?;
    if inode == 0 {
        return Err(malformed_guest(
            phase,
            "the guest filesystem inode identity is missing",
        ));
    }
    Ok(LimaFilesystemObjectIdentity { device_id, inode })
}

fn parse_single_guest_line(
    output: &str,
    phase: LimaObservationPhase,
) -> Result<&str, ObservationProblem> {
    if output.is_empty() || output == "\n" {
        return Err(ObservationProblem::new(
            LimaObservationRefusalCode::MissingGuestEvidence,
            phase,
            "the running guest did not return the required observation evidence",
        ));
    }
    let Some(value) = output.strip_suffix('\n') else {
        return Err(malformed_guest(
            phase,
            "the running guest evidence is not one complete line",
        ));
    };
    if value.is_empty()
        || value
            .chars()
            .any(|character| matches!(character, '\n' | '\r' | '\0'))
    {
        return Err(malformed_guest(
            phase,
            "the running guest evidence is not one canonical line",
        ));
    }
    Ok(value)
}

fn validate_guest_memory(
    configured_bytes: u64,
    observed_bytes: u64,
) -> Result<(), ObservationProblem> {
    if observed_bytes == 0 || observed_bytes > configured_bytes {
        return Err(ObservationProblem::new(
            LimaObservationRefusalCode::GuestMemoryMismatch,
            LimaObservationPhase::GuestMemory,
            "the running guest memory differs from the configured Lima envelope",
        ));
    }
    let allowed_deficit = configured_bytes / 20 + MAX_GUEST_MEMORY_FIXED_DEFICIT_BYTES;
    if configured_bytes - observed_bytes > allowed_deficit {
        return Err(ObservationProblem::new(
            LimaObservationRefusalCode::GuestMemoryMismatch,
            LimaObservationPhase::GuestMemory,
            "the running guest memory differs from the configured Lima envelope",
        ));
    }
    Ok(())
}

fn validate_private_absolute_path(
    path: PathBuf,
    allow_root: bool,
) -> Result<PathBuf, LimaObservationFailure> {
    if !valid_absolute_path(&path, allow_root) {
        return Err(input_failure(
            "reviewed Lima paths must be bounded canonical absolute paths without aliases",
        ));
    }
    Ok(path)
}

fn validate_observed_absolute_path(value: &str) -> Result<PathBuf, ObservationProblem> {
    let path = PathBuf::from(value);
    if !valid_absolute_path(&path, false) {
        return Err(ObservationProblem::new(
            LimaObservationRefusalCode::AliasedEvidence,
            LimaObservationPhase::InstanceObservation,
            "the observed Lima instance directory is aliased or non-canonical",
        ));
    }
    Ok(path)
}

fn valid_absolute_path(path: &Path, allow_root: bool) -> bool {
    let Some(value) = path.to_str() else {
        return false;
    };
    if !path.is_absolute()
        || value.len() > MAX_PATH_BYTES
        || value.bytes().any(|byte| byte.is_ascii_control())
        || value.ends_with('/') && value != "/"
        || value.get(1..).is_some_and(|rest| rest.contains("//"))
        || value
            .split('/')
            .any(|component| matches!(component, "." | ".."))
        || !allow_root && value == "/"
    {
        return false;
    }
    path.components()
        .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

fn exact_private_path(path: &Path) -> &str {
    path.to_str()
        .expect("reviewed private Lima paths are validated UTF-8")
}

fn valid_instance_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_INSTANCE_NAME_BYTES
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn parse_canonical_u64(value: &str) -> Option<u64> {
    if value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || value.len() > 1 && value.starts_with('0')
    {
        return None;
    }
    value.parse().ok()
}

const fn is_lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')
}

fn input_failure(message: &'static str) -> LimaObservationFailure {
    LimaObservationFailure::from_problem(
        ObservationProblem::new(
            LimaObservationRefusalCode::InvalidInput,
            LimaObservationPhase::InputValidation,
            message,
        ),
        LimaObservationPrivateEvidence::default(),
    )
}

const fn malformed_instance(message: &'static str) -> ObservationProblem {
    ObservationProblem::new(
        LimaObservationRefusalCode::MalformedInstanceEvidence,
        LimaObservationPhase::InstanceObservation,
        message,
    )
}

const fn malformed_guest(phase: LimaObservationPhase, message: &'static str) -> ObservationProblem {
    ObservationProblem::new(
        LimaObservationRefusalCode::MalformedGuestEvidence,
        phase,
        message,
    )
}

#[cfg(test)]
mod tests;
