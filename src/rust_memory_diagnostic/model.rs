use std::fmt;

use serde::Serialize;

use crate::artifact::Sha256Digest;
use crate::execution_admission::EpochMillis;
use crate::rust_verification_envelope::{
    RustRuntimeConcurrency, RustVerificationEnvelope, RustVerificationSourceIdentity,
};
use crate::rust_verification_envelope_digest::digest_rust_verification_envelope;
use crate::verification_profile::{RepositoryCommandIdentity, VerificationProfileId};

pub const RUST_MEMORY_DIAGNOSTIC_SCHEMA_VERSION: u8 = 1;
pub const MAX_RUST_DIAGNOSTIC_OBSERVATION_LAG_MILLIS: u64 = 3_600_000;
pub const MAX_RUST_DIAGNOSTIC_MEMORY_BYTES: u64 = 1_u64 << 50;
pub const MAX_RUST_MEMORY_EVENT_COUNTER: u64 = 1_000_000_000_000;
const MAX_IDENTIFIER_BYTES: usize = 96;

macro_rules! identifier_type {
    ($name:ident, $field:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Parse one bounded opaque diagnostic identity.
            ///
            /// # Errors
            ///
            /// Returns an error for empty, oversized, non-ASCII, or path-shaped values.
            pub fn parse(value: &str) -> Result<Self, RustMemoryDiagnosticError> {
                validate_identifier($field, value)?;
                Ok(Self(value.to_owned()))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

identifier_type!(RustVerificationAttemptId, "identity.attempt_id");
identifier_type!(RustProcessGroupId, "identity.process_group_id");

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct RustProcessGroupGeneration(u64);

impl RustProcessGroupGeneration {
    /// Define one positive process-group generation.
    ///
    /// # Errors
    ///
    /// Returns an error for generation zero.
    pub fn new(value: u64) -> Result<Self, RustMemoryDiagnosticError> {
        if value == 0 {
            return Err(RustMemoryDiagnosticError::new(
                "identity.process_group_generation",
                "invalid_process_group_generation",
                "process-group generation must be greater than zero",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustProcessGroupIdentity {
    pub group_id: RustProcessGroupId,
    pub generation: RustProcessGroupGeneration,
}

impl RustProcessGroupIdentity {
    #[must_use]
    pub const fn new(group_id: RustProcessGroupId, generation: RustProcessGroupGeneration) -> Self {
        Self {
            group_id,
            generation,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustMemoryDiagnosticIdentity {
    verification_profile_id: VerificationProfileId,
    source: RustVerificationSourceIdentity,
    command: RepositoryCommandIdentity,
    envelope_digest: Sha256Digest,
    attempt_id: RustVerificationAttemptId,
    process_group: RustProcessGroupIdentity,
}

impl RustMemoryDiagnosticIdentity {
    fn from_envelope(
        envelope: &RustVerificationEnvelope,
        envelope_digest: Sha256Digest,
        attempt_id: RustVerificationAttemptId,
        process_group: RustProcessGroupIdentity,
    ) -> Self {
        Self {
            verification_profile_id: envelope.profile_id().clone(),
            source: envelope.source().clone(),
            command: envelope.command().clone(),
            envelope_digest,
            attempt_id,
            process_group,
        }
    }

    #[must_use]
    pub const fn verification_profile_id(&self) -> &VerificationProfileId {
        &self.verification_profile_id
    }

    #[must_use]
    pub const fn source(&self) -> &RustVerificationSourceIdentity {
        &self.source
    }

    #[must_use]
    pub const fn command(&self) -> &RepositoryCommandIdentity {
        &self.command
    }

    #[must_use]
    pub const fn envelope_digest(&self) -> &Sha256Digest {
        &self.envelope_digest
    }

    #[must_use]
    pub const fn attempt_id(&self) -> &RustVerificationAttemptId {
        &self.attempt_id
    }

    #[must_use]
    pub const fn process_group(&self) -> &RustProcessGroupIdentity {
        &self.process_group
    }

    #[must_use]
    pub fn observation_binding(&self) -> RustObservationBinding {
        RustObservationBinding {
            attempt_id: self.attempt_id.clone(),
            process_group: self.process_group.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustObservationBinding {
    pub attempt_id: RustVerificationAttemptId,
    pub process_group: RustProcessGroupIdentity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RustMemoryAuthoritySnapshot {
    minimum_guest_available_memory_bytes: u64,
    minimum_guest_available_swap_bytes: u64,
    reserved_memory_bytes: u64,
    cargo_build_jobs: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime_test_threads: Option<u16>,
    maximum_execution_millis: u64,
}

impl RustMemoryAuthoritySnapshot {
    #[must_use]
    fn from_envelope(envelope: &RustVerificationEnvelope) -> Self {
        let resources = envelope.resources();
        let runtime_test_threads = match &resources.concurrency.runtime {
            RustRuntimeConcurrency::NotApplicable => None,
            RustRuntimeConcurrency::Libtest { test_threads, .. }
            | RustRuntimeConcurrency::Nextest { test_threads, .. } => Some(*test_threads),
        };
        Self {
            minimum_guest_available_memory_bytes: resources.minimum_guest_available_memory_bytes,
            minimum_guest_available_swap_bytes: resources.minimum_guest_available_swap_bytes,
            reserved_memory_bytes: resources.reserved_resources.memory_bytes,
            cargo_build_jobs: resources.concurrency.cargo_build_jobs,
            runtime_test_threads,
            maximum_execution_millis: resources.maximum_execution_millis,
        }
    }

    #[must_use]
    pub const fn minimum_guest_available_memory_bytes(&self) -> u64 {
        self.minimum_guest_available_memory_bytes
    }

    #[must_use]
    pub const fn minimum_guest_available_swap_bytes(&self) -> u64 {
        self.minimum_guest_available_swap_bytes
    }

    #[must_use]
    pub const fn reserved_memory_bytes(&self) -> u64 {
        self.reserved_memory_bytes
    }

    #[must_use]
    pub const fn cargo_build_jobs(&self) -> u16 {
        self.cargo_build_jobs
    }

    #[must_use]
    pub const fn runtime_test_threads(&self) -> Option<u16> {
        self.runtime_test_threads
    }

    #[must_use]
    pub const fn maximum_execution_millis(&self) -> u64 {
        self.maximum_execution_millis
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustMemoryDiagnosticEnvelopeBinding {
    identity: RustMemoryDiagnosticIdentity,
    authority: RustMemoryAuthoritySnapshot,
}

impl RustMemoryDiagnosticEnvelopeBinding {
    /// Bind one diagnostic identity and authority snapshot to the same exact envelope.
    ///
    /// The envelope digest is derived canonically inside this constructor. Callers supply only the
    /// attempt and process-group identities, so a digest from another envelope cannot label the
    /// retained profile, source, command, resource, concurrency, or duration authority.
    ///
    /// # Errors
    ///
    /// Returns a bounded diagnostic error when the exact envelope cannot be canonically digested.
    pub fn from_envelope(
        envelope: &RustVerificationEnvelope,
        attempt_id: RustVerificationAttemptId,
        process_group: RustProcessGroupIdentity,
    ) -> Result<Self, RustMemoryDiagnosticError> {
        let envelope_digest = digest_rust_verification_envelope(envelope).map_err(|_| {
            RustMemoryDiagnosticError::new(
                "identity.envelope_digest",
                "envelope_digest_unavailable",
                "exact Rust verification envelope could not be canonically digested",
            )
        })?;
        Ok(Self {
            identity: RustMemoryDiagnosticIdentity::from_envelope(
                envelope,
                envelope_digest,
                attempt_id,
                process_group,
            ),
            authority: RustMemoryAuthoritySnapshot::from_envelope(envelope),
        })
    }

    #[must_use]
    pub const fn identity(&self) -> &RustMemoryDiagnosticIdentity {
        &self.identity
    }

    #[must_use]
    pub const fn authority(&self) -> &RustMemoryAuthoritySnapshot {
        &self.authority
    }

    #[must_use]
    pub fn observation_binding(&self) -> RustObservationBinding {
        self.identity.observation_binding()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RustDiagnosticTiming {
    pub classified_at: EpochMillis,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt_started_at: Option<EpochMillis>,
    pub maximum_preflight_to_start_millis: u64,
    pub maximum_terminal_to_classification_millis: u64,
}

impl RustDiagnosticTiming {
    /// Define caller-supplied attempt timing and freshness bounds.
    ///
    /// # Errors
    ///
    /// Returns an error for zero or excessive observation-lag bounds, or a future start time.
    pub fn new(
        classified_at: EpochMillis,
        attempt_started_at: Option<EpochMillis>,
        maximum_preflight_to_start_millis: u64,
        maximum_terminal_to_classification_millis: u64,
    ) -> Result<Self, RustMemoryDiagnosticError> {
        for (field, value) in [
            (
                "timing.maximum_preflight_to_start_millis",
                maximum_preflight_to_start_millis,
            ),
            (
                "timing.maximum_terminal_to_classification_millis",
                maximum_terminal_to_classification_millis,
            ),
        ] {
            if !(1..=MAX_RUST_DIAGNOSTIC_OBSERVATION_LAG_MILLIS).contains(&value) {
                return Err(RustMemoryDiagnosticError::new(
                    field,
                    "invalid_observation_lag",
                    "observation lag must remain within the fixed positive bound",
                ));
            }
        }
        if attempt_started_at.is_some_and(|started| started > classified_at) {
            return Err(RustMemoryDiagnosticError::new(
                "timing.attempt_started_at",
                "future_attempt_start",
                "attempt start may not be later than classification time",
            ));
        }
        Ok(Self {
            classified_at,
            attempt_started_at,
            maximum_preflight_to_start_millis,
            maximum_terminal_to_classification_millis,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustPreflightMemoryObservation {
    pub binding: RustObservationBinding,
    pub observed_at: EpochMillis,
    pub available_memory_bytes: u64,
    pub available_swap_bytes: u64,
}

impl RustPreflightMemoryObservation {
    /// Record one bounded guest-memory preflight observation.
    ///
    /// # Errors
    ///
    /// Returns an error for memory or swap observations above the fixed bound.
    pub fn new(
        binding: RustObservationBinding,
        observed_at: EpochMillis,
        available_memory_bytes: u64,
        available_swap_bytes: u64,
    ) -> Result<Self, RustMemoryDiagnosticError> {
        validate_memory_bytes("preflight.available_memory_bytes", available_memory_bytes)?;
        validate_memory_bytes("preflight.available_swap_bytes", available_swap_bytes)?;
        Ok(Self {
            binding,
            observed_at,
            available_memory_bytes,
            available_swap_bytes,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RustExecutionPhase {
    NotStarted,
    Compile,
    Link,
    Test,
    Cleanup,
    Completed,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct RustSignal(u8);

impl RustSignal {
    /// Define one representable Unix-style signal number.
    ///
    /// # Errors
    ///
    /// Returns an error outside the reviewed 1 through 64 range.
    pub fn new(value: u8) -> Result<Self, RustMemoryDiagnosticError> {
        if !(1..=64).contains(&value) {
            return Err(RustMemoryDiagnosticError::new(
                "terminal.signal",
                "invalid_signal",
                "signal must be within the reviewed 1 through 64 range",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RustProcessTermination {
    NotStarted,
    Exited { code: u8 },
    Signaled { signal: RustSignal },
    Timeout,
    RunnerLost,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustTerminalObservation {
    pub binding: RustObservationBinding,
    pub observed_at: EpochMillis,
    pub phase: RustExecutionPhase,
    pub termination: RustProcessTermination,
}

impl RustTerminalObservation {
    #[must_use]
    pub const fn new(
        binding: RustObservationBinding,
        observed_at: EpochMillis,
        phase: RustExecutionPhase,
        termination: RustProcessTermination,
    ) -> Self {
        Self {
            binding,
            observed_at,
            phase,
            termination,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RustMemoryEventCounters {
    pub low: u64,
    pub high: u64,
    pub max: u64,
    pub oom: u64,
    pub oom_kill: u64,
}

impl RustMemoryEventCounters {
    /// Record one complete bounded memory-events counter set.
    ///
    /// # Errors
    ///
    /// Returns an error when any counter exceeds the fixed public bound.
    pub fn new(
        low: u64,
        high: u64,
        max: u64,
        oom: u64,
        oom_kill: u64,
    ) -> Result<Self, RustMemoryDiagnosticError> {
        for (field, value) in [
            ("cgroup.events.low", low),
            ("cgroup.events.high", high),
            ("cgroup.events.max", max),
            ("cgroup.events.oom", oom),
            ("cgroup.events.oom_kill", oom_kill),
        ] {
            if value > MAX_RUST_MEMORY_EVENT_COUNTER {
                return Err(RustMemoryDiagnosticError::new(
                    field,
                    "memory_event_counter_overflow",
                    "memory-event counters must remain within the fixed public bound",
                ));
            }
        }
        Ok(Self {
            low,
            high,
            max,
            oom,
            oom_kill,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustCgroupMemoryObservation {
    pub binding: RustObservationBinding,
    pub observed_at: EpochMillis,
    pub memory_limit_bytes: u64,
    pub memory_current_bytes: u64,
    pub memory_peak_bytes: u64,
    pub events: RustMemoryEventCounters,
}

impl RustCgroupMemoryObservation {
    /// Record one complete bounded dedicated-process-group memory observation.
    ///
    /// # Errors
    ///
    /// Returns an error for zero/excessive limits or impossible current/peak relationships.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        binding: RustObservationBinding,
        observed_at: EpochMillis,
        memory_limit_bytes: u64,
        memory_current_bytes: u64,
        memory_peak_bytes: u64,
        events: RustMemoryEventCounters,
    ) -> Result<Self, RustMemoryDiagnosticError> {
        if memory_limit_bytes == 0 || memory_limit_bytes > MAX_RUST_DIAGNOSTIC_MEMORY_BYTES {
            return Err(RustMemoryDiagnosticError::new(
                "cgroup.memory_limit_bytes",
                "invalid_memory_limit",
                "dedicated process-group memory limit must remain within the fixed positive bound",
            ));
        }
        validate_memory_bytes("cgroup.memory_current_bytes", memory_current_bytes)?;
        validate_memory_bytes("cgroup.memory_peak_bytes", memory_peak_bytes)?;
        if memory_current_bytes > memory_limit_bytes
            || memory_peak_bytes < memory_current_bytes
            || memory_peak_bytes > memory_limit_bytes
        {
            return Err(RustMemoryDiagnosticError::new(
                "cgroup.memory_usage",
                "invalid_memory_usage_relationship",
                "current and peak memory must be ordered within the exact process-group limit",
            ));
        }
        Ok(Self {
            binding,
            observed_at,
            memory_limit_bytes,
            memory_current_bytes,
            memory_peak_bytes,
            events,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RustCgroupMemoryEvidence {
    NotCreated,
    Complete {
        before: RustCgroupMemoryObservation,
        after: RustCgroupMemoryObservation,
    },
    UnavailableAfterRunnerLoss {
        before: RustCgroupMemoryObservation,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustMemoryDiagnosticInput {
    binding: RustMemoryDiagnosticEnvelopeBinding,
    timing: RustDiagnosticTiming,
    preflight: RustPreflightMemoryObservation,
    terminal: RustTerminalObservation,
    cgroup: RustCgroupMemoryEvidence,
}

impl RustMemoryDiagnosticInput {
    #[must_use]
    pub const fn new(
        binding: RustMemoryDiagnosticEnvelopeBinding,
        timing: RustDiagnosticTiming,
        preflight: RustPreflightMemoryObservation,
        terminal: RustTerminalObservation,
        cgroup: RustCgroupMemoryEvidence,
    ) -> Self {
        Self {
            binding,
            timing,
            preflight,
            terminal,
            cgroup,
        }
    }

    #[must_use]
    pub const fn envelope_binding(&self) -> &RustMemoryDiagnosticEnvelopeBinding {
        &self.binding
    }

    #[must_use]
    pub fn observation_binding(&self) -> RustObservationBinding {
        self.binding.observation_binding()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RustMemoryDiagnosticClassification {
    Succeeded,
    CompileFailed,
    LinkFailed,
    TestFailed,
    MemoryPressureRefused,
    MemoryExhausted,
    Timeout,
    RunnerLost,
    Inconclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RustMemoryEventDelta {
    pub low: u64,
    pub high: u64,
    pub max: u64,
    pub oom: u64,
    pub oom_kill: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RustCgroupMemorySummary {
    NotCreated,
    Complete {
        memory_limit_bytes: u64,
        memory_peak_bytes: u64,
        events: RustMemoryEventDelta,
    },
    UnavailableAfterRunnerLoss {
        memory_limit_bytes: u64,
        memory_peak_bytes: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustMemoryDiagnosticReport {
    pub schema_version: u8,
    pub identity: RustMemoryDiagnosticIdentity,
    pub authority: RustMemoryAuthoritySnapshot,
    pub classification: RustMemoryDiagnosticClassification,
    pub phase: RustExecutionPhase,
    pub termination: RustProcessTermination,
    pub preflight_available_memory_bytes: u64,
    pub preflight_available_swap_bytes: u64,
    pub cgroup: RustCgroupMemorySummary,
    pub classified_at: EpochMillis,
}
