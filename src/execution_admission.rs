use std::fmt;

use serde::Serialize;

use crate::verification_profile::VerificationProfileId;

pub const EXECUTION_ADMISSION_SCHEMA_VERSION: u8 = 1;
pub const MAX_RESERVATION_LIFETIME_MILLIS: u64 = 86_400_000;
const MAX_IDENTIFIER_LEN: usize = 96;
const MAX_CPU_MILLIS: u32 = 1_000_000;
const MAX_MEMORY_BYTES: u64 = 1_u64 << 50;
const MAX_PIDS: u32 = 1_000_000;
const MAX_QUEUE_POSITION: u32 = 1_000_000;

macro_rules! identifier_type {
    ($name:ident, $field:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn parse(value: &str) -> Result<Self, ExecutionAdmissionError> {
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

identifier_type!(ExecutionRequestId, "identity.request_id");
identifier_type!(RunnerProfileId, "identity.runner_profile_id");
identifier_type!(ReservationId, "reservation.id");

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct EpochMillis(u64);

impl EpochMillis {
    pub fn new(value: u64) -> Result<Self, ExecutionAdmissionError> {
        if value == 0 {
            return Err(ExecutionAdmissionError::new(
                "observed_at",
                "invalid_observation_time",
                "observation time must be greater than zero",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct ReservationGeneration(u64);

impl ReservationGeneration {
    pub fn new(value: u64) -> Result<Self, ExecutionAdmissionError> {
        if value == 0 {
            return Err(ExecutionAdmissionError::new(
                "reservation.generation",
                "invalid_reservation_generation",
                "reservation generation must be greater than zero",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ExecutionResourceLimits {
    pub cpu_millis: u32,
    pub memory_bytes: u64,
    pub pids: u32,
}

impl ExecutionResourceLimits {
    pub fn new(
        cpu_millis: u32,
        memory_bytes: u64,
        pids: u32,
    ) -> Result<Self, ExecutionAdmissionError> {
        if !(1..=MAX_CPU_MILLIS).contains(&cpu_millis) {
            return Err(ExecutionAdmissionError::new(
                "resources.cpu_millis",
                "invalid_cpu_limit",
                "CPU limit must be within the bounded positive range",
            ));
        }
        if !(1..=MAX_MEMORY_BYTES).contains(&memory_bytes) {
            return Err(ExecutionAdmissionError::new(
                "resources.memory_bytes",
                "invalid_memory_limit",
                "memory limit must be within the bounded positive range",
            ));
        }
        if !(1..=MAX_PIDS).contains(&pids) {
            return Err(ExecutionAdmissionError::new(
                "resources.pids",
                "invalid_pid_limit",
                "PID limit must be within the bounded positive range",
            ));
        }
        Ok(Self {
            cpu_millis,
            memory_bytes,
            pids,
        })
    }

    #[must_use]
    pub const fn fits_within(self, capacity: Self) -> bool {
        self.cpu_millis <= capacity.cpu_millis
            && self.memory_bytes <= capacity.memory_bytes
            && self.pids <= capacity.pids
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct HostCapacityObservation {
    pub observed_at: EpochMillis,
    pub capacity: ExecutionResourceLimits,
}

impl HostCapacityObservation {
    #[must_use]
    pub const fn new(observed_at: EpochMillis, capacity: ExecutionResourceLimits) -> Self {
        Self {
            observed_at,
            capacity,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecutionAdmissionIdentity {
    pub request_id: ExecutionRequestId,
    pub verification_profile_id: VerificationProfileId,
    pub runner_profile_id: RunnerProfileId,
}

impl ExecutionAdmissionIdentity {
    #[must_use]
    pub const fn new(
        request_id: ExecutionRequestId,
        verification_profile_id: VerificationProfileId,
        runner_profile_id: RunnerProfileId,
    ) -> Self {
        Self {
            request_id,
            verification_profile_id,
            runner_profile_id,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionAdmissionState {
    Requested,
    Admitted,
    Queued,
    Reserved,
    Starting,
    Running,
    Draining,
    Unavailable,
}

impl ExecutionAdmissionState {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Unavailable)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "knowledge", rename_all = "snake_case")]
pub enum QueuePosition {
    Known { position: u32 },
    Unknown,
}

impl QueuePosition {
    pub fn known(position: u32) -> Result<Self, ExecutionAdmissionError> {
        if !(1..=MAX_QUEUE_POSITION).contains(&position) {
            return Err(ExecutionAdmissionError::new(
                "queue_position.position",
                "invalid_queue_position",
                "known queue position must be within the bounded positive range",
            ));
        }
        Ok(Self::Known { position })
    }

    #[must_use]
    pub const fn unknown() -> Self {
        Self::Unknown
    }

    const fn is_valid(self) -> bool {
        match self {
            Self::Known { position } => position > 0 && position <= MAX_QUEUE_POSITION,
            Self::Unknown => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum FallbackProfileEligibility {
    Eligible { runner_profile_id: RunnerProfileId },
    Ineligible,
}

impl FallbackProfileEligibility {
    #[must_use]
    pub const fn ineligible() -> Self {
        Self::Ineligible
    }

    #[must_use]
    pub const fn eligible(runner_profile_id: RunnerProfileId) -> Self {
        Self::Eligible { runner_profile_id }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DrainAcknowledgement {
    Cancellation,
    Drain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    AdmissionRejected,
    CapacityUnavailable,
    HostUnavailable,
    ReservationExpired,
    Cancelled,
    Drained,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReservationEvidence {
    pub id: ReservationId,
    pub generation: ReservationGeneration,
    pub reserved_at: EpochMillis,
    pub expires_at: EpochMillis,
}

impl ReservationEvidence {
    #[must_use]
    pub const fn new(
        id: ReservationId,
        generation: ReservationGeneration,
        reserved_at: EpochMillis,
        expires_at: EpochMillis,
    ) -> Self {
        Self {
            id,
            generation,
            reserved_at,
            expires_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionAdmissionInput {
    pub identity: ExecutionAdmissionIdentity,
    pub state: ExecutionAdmissionState,
    pub observed_at: EpochMillis,
    pub requested_limits: ExecutionResourceLimits,
    pub host_capacity: Option<HostCapacityObservation>,
    pub applied_limits: Option<ExecutionResourceLimits>,
    pub queue_position: Option<QueuePosition>,
    pub reservation: Option<ReservationEvidence>,
    pub acknowledgement: Option<DrainAcknowledgement>,
    pub fallback_eligibility: FallbackProfileEligibility,
    pub unavailable_reason: Option<UnavailableReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecutionAdmissionRecord {
    schema_version: u8,
    identity: ExecutionAdmissionIdentity,
    state: ExecutionAdmissionState,
    observed_at: EpochMillis,
    requested_limits: ExecutionResourceLimits,
    #[serde(skip_serializing_if = "Option::is_none")]
    host_capacity: Option<HostCapacityObservation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    applied_limits: Option<ExecutionResourceLimits>,
    #[serde(skip_serializing_if = "Option::is_none")]
    queue_position: Option<QueuePosition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reservation: Option<ReservationEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    acknowledgement: Option<DrainAcknowledgement>,
    fallback_eligibility: FallbackProfileEligibility,
    #[serde(skip_serializing_if = "Option::is_none")]
    unavailable_reason: Option<UnavailableReason>,
}

impl ExecutionAdmissionRecord {
    pub fn from_input(input: ExecutionAdmissionInput) -> Result<Self, ExecutionAdmissionError> {
        validate_fallback(&input.identity, &input.fallback_eligibility)?;
        validate_state_evidence(&input)?;
        Ok(Self {
            schema_version: EXECUTION_ADMISSION_SCHEMA_VERSION,
            identity: input.identity,
            state: input.state,
            observed_at: input.observed_at,
            requested_limits: input.requested_limits,
            host_capacity: input.host_capacity,
            applied_limits: input.applied_limits,
            queue_position: input.queue_position,
            reservation: input.reservation,
            acknowledgement: input.acknowledgement,
            fallback_eligibility: input.fallback_eligibility,
            unavailable_reason: input.unavailable_reason,
        })
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn identity(&self) -> &ExecutionAdmissionIdentity {
        &self.identity
    }

    #[must_use]
    pub const fn state(&self) -> ExecutionAdmissionState {
        self.state
    }

    #[must_use]
    pub const fn observed_at(&self) -> EpochMillis {
        self.observed_at
    }

    #[must_use]
    pub const fn requested_limits(&self) -> ExecutionResourceLimits {
        self.requested_limits
    }

    #[must_use]
    pub const fn host_capacity(&self) -> Option<HostCapacityObservation> {
        self.host_capacity
    }

    #[must_use]
    pub const fn applied_limits(&self) -> Option<ExecutionResourceLimits> {
        self.applied_limits
    }

    #[must_use]
    pub const fn queue_position(&self) -> Option<QueuePosition> {
        self.queue_position
    }

    #[must_use]
    pub fn reservation(&self) -> Option<&ReservationEvidence> {
        self.reservation.as_ref()
    }

    #[must_use]
    pub const fn acknowledgement(&self) -> Option<DrainAcknowledgement> {
        self.acknowledgement
    }

    #[must_use]
    pub const fn fallback_eligibility(&self) -> &FallbackProfileEligibility {
        &self.fallback_eligibility
    }

    #[must_use]
    pub const fn unavailable_reason(&self) -> Option<UnavailableReason> {
        self.unavailable_reason
    }

    pub fn plan_transition(
        &self,
        next: Self,
    ) -> Result<ExecutionAdmissionTransition, ExecutionAdmissionError> {
        if self.state.is_terminal() {
            return Err(ExecutionAdmissionError::new(
                "state",
                "terminal_state_reversal",
                "terminal admission state cannot transition",
            ));
        }
        if next.observed_at < self.observed_at {
            return Err(ExecutionAdmissionError::new(
                "observed_at",
                "observation_time_reversal",
                "admission observation time cannot move backwards",
            ));
        }
        if next.identity != self.identity {
            return Err(ExecutionAdmissionError::new(
                "identity",
                "admission_identity_drift",
                "request, verification-profile, and runner-profile identity must remain exact",
            ));
        }
        if next.requested_limits != self.requested_limits {
            return Err(ExecutionAdmissionError::new(
                "requested_limits",
                "requested_capacity_drift",
                "requested resource limits must remain exact for one admission request",
            ));
        }
        if next.fallback_eligibility != self.fallback_eligibility {
            return Err(ExecutionAdmissionError::new(
                "fallback_eligibility",
                "fallback_eligibility_drift",
                "fallback-profile eligibility must remain exact for one admission request",
            ));
        }
        validate_capacity_observation_progress(self.host_capacity, next.host_capacity)?;
        validate_transition_pair(self.state, next.state)?;
        validate_reservation_progress(self, &next)?;
        validate_acknowledgement_progress(self, &next)?;
        validate_terminal_transition_reason(self, &next)?;
        Ok(ExecutionAdmissionTransition {
            schema_version: EXECUTION_ADMISSION_SCHEMA_VERSION,
            identity: self.identity.clone(),
            from: self.state,
            to: next.state,
            observed_at: next.observed_at,
            resulting_record: next,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecutionAdmissionTransition {
    schema_version: u8,
    identity: ExecutionAdmissionIdentity,
    from: ExecutionAdmissionState,
    to: ExecutionAdmissionState,
    observed_at: EpochMillis,
    resulting_record: ExecutionAdmissionRecord,
}

impl ExecutionAdmissionTransition {
    #[must_use]
    pub const fn from(&self) -> ExecutionAdmissionState {
        self.from
    }

    #[must_use]
    pub const fn to(&self) -> ExecutionAdmissionState {
        self.to
    }

    #[must_use]
    pub const fn observed_at(&self) -> EpochMillis {
        self.observed_at
    }

    #[must_use]
    pub const fn identity(&self) -> &ExecutionAdmissionIdentity {
        &self.identity
    }

    #[must_use]
    pub const fn resulting_record(&self) -> &ExecutionAdmissionRecord {
        &self.resulting_record
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ExecutionAdmissionError {
    pub field: &'static str,
    pub code: &'static str,
    pub message: &'static str,
}

impl ExecutionAdmissionError {
    const fn new(field: &'static str, code: &'static str, message: &'static str) -> Self {
        Self {
            field,
            code,
            message,
        }
    }
}

impl fmt::Display for ExecutionAdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ExecutionAdmissionError {}

fn validate_identifier(field: &'static str, value: &str) -> Result<(), ExecutionAdmissionError> {
    let mut bytes = value.bytes();
    let first_is_safe = bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    let remaining_are_safe = bytes.all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
    });
    if value.len() > MAX_IDENTIFIER_LEN || !first_is_safe || !remaining_are_safe {
        return Err(ExecutionAdmissionError::new(
            field,
            "invalid_identifier",
            "identifier must be bounded lowercase ASCII without path or log content",
        ));
    }
    Ok(())
}

fn validate_fallback(
    identity: &ExecutionAdmissionIdentity,
    fallback: &FallbackProfileEligibility,
) -> Result<(), ExecutionAdmissionError> {
    if let FallbackProfileEligibility::Eligible { runner_profile_id } = fallback
        && runner_profile_id == &identity.runner_profile_id
    {
        return Err(ExecutionAdmissionError::new(
            "fallback_eligibility.runner_profile_id",
            "fallback_profile_matches_primary",
            "eligible fallback runner profile must differ from the primary runner profile",
        ));
    }
    Ok(())
}

fn validate_state_evidence(input: &ExecutionAdmissionInput) -> Result<(), ExecutionAdmissionError> {
    let requires_capacity = input.state != ExecutionAdmissionState::Requested;
    if requires_capacity != input.host_capacity.is_some() {
        return Err(ExecutionAdmissionError::new(
            "host_capacity",
            "invalid_host_capacity_evidence",
            "every non-requested state requires one host-capacity observation",
        ));
    }
    if let Some(host) = input.host_capacity {
        if host.observed_at > input.observed_at {
            return Err(ExecutionAdmissionError::new(
                "host_capacity.observed_at",
                "future_host_capacity_observation",
                "host-capacity observation cannot be newer than the admission observation",
            ));
        }
        let active = matches!(
            input.state,
            ExecutionAdmissionState::Admitted
                | ExecutionAdmissionState::Queued
                | ExecutionAdmissionState::Reserved
                | ExecutionAdmissionState::Starting
                | ExecutionAdmissionState::Running
                | ExecutionAdmissionState::Draining
        );
        if active && !input.requested_limits.fits_within(host.capacity) {
            return Err(ExecutionAdmissionError::new(
                "requested_limits",
                "requested_capacity_unavailable",
                "active execution requests must fit within observed host capacity",
            ));
        }
        if let Some(applied) = input.applied_limits {
            if !applied.fits_within(input.requested_limits) {
                return Err(ExecutionAdmissionError::new(
                    "applied_limits",
                    "applied_capacity_widening",
                    "applied resource limits must fit within requested limits",
                ));
            }
            if active && !applied.fits_within(host.capacity) {
                return Err(ExecutionAdmissionError::new(
                    "applied_limits",
                    "applied_capacity_unavailable",
                    "active applied limits must fit within observed host capacity",
                ));
            }
        }
    }

    let queued = input.state == ExecutionAdmissionState::Queued;
    if queued != input.queue_position.is_some()
        || input
            .queue_position
            .is_some_and(|position| !position.is_valid())
    {
        return Err(ExecutionAdmissionError::new(
            "queue_position",
            "invalid_queue_evidence",
            "queued state requires one bounded known or explicit unknown queue position",
        ));
    }

    let reservation_required = matches!(
        input.state,
        ExecutionAdmissionState::Reserved
            | ExecutionAdmissionState::Starting
            | ExecutionAdmissionState::Running
            | ExecutionAdmissionState::Draining
    );
    if reservation_required && (input.reservation.is_none() || input.applied_limits.is_none()) {
        return Err(ExecutionAdmissionError::new(
            "reservation",
            "reservation_evidence_missing",
            "reserved execution states require reservation and applied-limit evidence",
        ));
    }
    if input.reservation.is_some() != input.applied_limits.is_some() {
        return Err(ExecutionAdmissionError::new(
            "reservation",
            "reservation_limit_evidence_mismatch",
            "reservation and applied-limit evidence must appear together",
        ));
    }
    if matches!(
        input.state,
        ExecutionAdmissionState::Requested
            | ExecutionAdmissionState::Admitted
            | ExecutionAdmissionState::Queued
    ) && input.reservation.is_some()
    {
        return Err(ExecutionAdmissionError::new(
            "reservation",
            "premature_reservation_evidence",
            "pre-reservation states cannot claim reservation evidence",
        ));
    }

    validate_reservation(input)?;
    validate_acknowledgement(input)?;
    validate_unavailable_reason(input)?;
    Ok(())
}

fn validate_reservation(input: &ExecutionAdmissionInput) -> Result<(), ExecutionAdmissionError> {
    let Some(reservation) = &input.reservation else {
        return Ok(());
    };
    let lifetime = reservation
        .expires_at
        .get()
        .checked_sub(reservation.reserved_at.get())
        .ok_or_else(|| {
            ExecutionAdmissionError::new(
                "reservation.expires_at",
                "invalid_reservation_expiry",
                "reservation expiry must follow reservation start",
            )
        })?;
    if lifetime == 0 || lifetime > MAX_RESERVATION_LIFETIME_MILLIS {
        return Err(ExecutionAdmissionError::new(
            "reservation.expires_at",
            "invalid_reservation_lifetime",
            "reservation lifetime must be positive and within the bounded maximum",
        ));
    }
    if reservation.reserved_at > input.observed_at {
        return Err(ExecutionAdmissionError::new(
            "reservation.reserved_at",
            "future_reservation_start",
            "reservation start cannot be newer than the admission observation",
        ));
    }
    let expired = reservation.expires_at <= input.observed_at;
    let terminal_expiry = input.state == ExecutionAdmissionState::Unavailable
        && input.unavailable_reason == Some(UnavailableReason::ReservationExpired);
    if expired != terminal_expiry {
        return Err(ExecutionAdmissionError::new(
            "reservation.expires_at",
            "reservation_expired",
            "expired reservations may appear only in a terminal reservation-expired observation",
        ));
    }
    Ok(())
}

fn validate_acknowledgement(
    input: &ExecutionAdmissionInput,
) -> Result<(), ExecutionAdmissionError> {
    match input.state {
        ExecutionAdmissionState::Draining if input.acknowledgement.is_none() => {
            Err(ExecutionAdmissionError::new(
                "acknowledgement",
                "drain_acknowledgement_missing",
                "draining state requires cancellation or drain acknowledgement",
            ))
        }
        ExecutionAdmissionState::Draining | ExecutionAdmissionState::Unavailable => Ok(()),
        _ if input.acknowledgement.is_some() => Err(ExecutionAdmissionError::new(
            "acknowledgement",
            "unexpected_acknowledgement",
            "acknowledgement is valid only for draining or unavailable state",
        )),
        _ => Ok(()),
    }
}

fn validate_unavailable_reason(
    input: &ExecutionAdmissionInput,
) -> Result<(), ExecutionAdmissionError> {
    if input.state != ExecutionAdmissionState::Unavailable {
        if input.unavailable_reason.is_some() {
            return Err(ExecutionAdmissionError::new(
                "unavailable_reason",
                "unexpected_unavailable_reason",
                "unavailable reason is valid only for unavailable state",
            ));
        }
        return Ok(());
    }

    let reason = input.unavailable_reason.ok_or_else(|| {
        ExecutionAdmissionError::new(
            "unavailable_reason",
            "unavailable_reason_missing",
            "unavailable state requires one bounded reason",
        )
    })?;
    match reason {
        UnavailableReason::CapacityUnavailable => {
            if input.acknowledgement.is_some() {
                return Err(ExecutionAdmissionError::new(
                    "acknowledgement",
                    "unexpected_acknowledgement",
                    "capacity unavailability cannot claim cancellation or drain acknowledgement",
                ));
            }
            let host = input
                .host_capacity
                .expect("unavailable state requires host-capacity evidence");
            let request_fits = input.requested_limits.fits_within(host.capacity);
            let applied_fits = input
                .applied_limits
                .is_none_or(|applied| applied.fits_within(host.capacity));
            if request_fits && applied_fits {
                return Err(ExecutionAdmissionError::new(
                    "host_capacity.capacity",
                    "capacity_unavailability_unproven",
                    "capacity-unavailable state requires an observed resource shortfall",
                ));
            }
        }
        UnavailableReason::AdmissionRejected => {
            if input.reservation.is_some() || input.acknowledgement.is_some() {
                return Err(ExecutionAdmissionError::new(
                    "reservation",
                    "unexpected_reservation_evidence",
                    "admission rejection cannot claim reservation or acknowledgement evidence",
                ));
            }
        }
        UnavailableReason::ReservationExpired => {
            if input.reservation.is_none() || input.acknowledgement.is_some() {
                return Err(ExecutionAdmissionError::new(
                    "reservation",
                    "expired_reservation_evidence_missing",
                    "reservation-expired state requires exact reservation evidence without acknowledgement",
                ));
            }
        }
        UnavailableReason::Cancelled => {
            if input.reservation.is_none()
                || input.acknowledgement != Some(DrainAcknowledgement::Cancellation)
            {
                return Err(ExecutionAdmissionError::new(
                    "acknowledgement",
                    "cancellation_acknowledgement_missing",
                    "cancelled state requires reservation and cancellation acknowledgement",
                ));
            }
        }
        UnavailableReason::Drained => {
            if input.reservation.is_none()
                || input.acknowledgement != Some(DrainAcknowledgement::Drain)
            {
                return Err(ExecutionAdmissionError::new(
                    "acknowledgement",
                    "drain_acknowledgement_missing",
                    "drained state requires reservation and drain acknowledgement",
                ));
            }
        }
        UnavailableReason::HostUnavailable => {
            if input.acknowledgement.is_some() {
                return Err(ExecutionAdmissionError::new(
                    "acknowledgement",
                    "unexpected_acknowledgement",
                    "host unavailability cannot claim cancellation or drain acknowledgement",
                ));
            }
        }
    }
    Ok(())
}

fn validate_capacity_observation_progress(
    previous: Option<HostCapacityObservation>,
    next: Option<HostCapacityObservation>,
) -> Result<(), ExecutionAdmissionError> {
    if let (Some(previous), Some(next)) = (previous, next)
        && next.observed_at < previous.observed_at
    {
        return Err(ExecutionAdmissionError::new(
            "host_capacity.observed_at",
            "stale_host_capacity_observation",
            "host-capacity observation time cannot move backwards",
        ));
    }
    Ok(())
}

fn validate_transition_pair(
    from: ExecutionAdmissionState,
    to: ExecutionAdmissionState,
) -> Result<(), ExecutionAdmissionError> {
    use ExecutionAdmissionState::{
        Admitted, Draining, Queued, Requested, Reserved, Running, Starting, Unavailable,
    };
    let same_active_state = from == to && !from.is_terminal();
    let forward = matches!(
        (from, to),
        (Requested, Admitted | Unavailable)
            | (Admitted, Queued | Reserved | Unavailable)
            | (Queued, Reserved | Unavailable)
            | (Reserved, Starting | Draining | Unavailable)
            | (Starting, Running | Draining | Unavailable)
            | (Running, Draining | Unavailable)
            | (Draining, Unavailable)
    );
    if !same_active_state && !forward {
        return Err(ExecutionAdmissionError::new(
            "state",
            "invalid_admission_transition",
            "state transition is outside the execution-admission contract",
        ));
    }
    Ok(())
}

fn validate_reservation_progress(
    previous: &ExecutionAdmissionRecord,
    next: &ExecutionAdmissionRecord,
) -> Result<(), ExecutionAdmissionError> {
    match (&previous.reservation, &next.reservation) {
        (None, Some(_)) if next.state != ExecutionAdmissionState::Reserved => {
            Err(ExecutionAdmissionError::new(
                "reservation",
                "reservation_state_mismatch",
                "first reservation evidence may appear only when entering reserved state",
            ))
        }
        (None, Some(next_reservation)) => {
            let host_observed_at = next
                .host_capacity
                .expect("reserved state requires host-capacity evidence")
                .observed_at;
            if next_reservation.reserved_at < host_observed_at {
                return Err(ExecutionAdmissionError::new(
                    "reservation.reserved_at",
                    "reservation_precedes_capacity_observation",
                    "reservation start must not precede the capacity observation that admitted it",
                ));
            }
            Ok(())
        }
        (None, None) => Ok(()),
        (Some(_), None) => Err(ExecutionAdmissionError::new(
            "reservation",
            "reservation_identity_lost",
            "post-reservation states must retain exact reservation evidence",
        )),
        (Some(previous_reservation), Some(next_reservation)) => {
            if next_reservation.id != previous_reservation.id {
                return Err(ExecutionAdmissionError::new(
                    "reservation.id",
                    "reservation_identity_drift",
                    "reservation identity must remain exact after reservation",
                ));
            }
            if next_reservation.generation < previous_reservation.generation {
                return Err(ExecutionAdmissionError::new(
                    "reservation.generation",
                    "stale_reservation_generation",
                    "reservation generation is older than the admitted generation",
                ));
            }
            if next_reservation.generation > previous_reservation.generation {
                return Err(ExecutionAdmissionError::new(
                    "reservation.generation",
                    "reservation_generation_drift",
                    "reservation generation cannot change within one admitted execution",
                ));
            }
            if next_reservation.reserved_at != previous_reservation.reserved_at {
                return Err(ExecutionAdmissionError::new(
                    "reservation.reserved_at",
                    "reservation_start_drift",
                    "reservation start cannot change within one admitted execution",
                ));
            }
            if next_reservation.expires_at != previous_reservation.expires_at {
                return Err(ExecutionAdmissionError::new(
                    "reservation.expires_at",
                    "reservation_expiry_drift",
                    "reservation expiry cannot change within one admitted execution",
                ));
            }
            if next.applied_limits != previous.applied_limits {
                return Err(ExecutionAdmissionError::new(
                    "applied_limits",
                    "applied_capacity_drift",
                    "applied resource limits must remain exact after reservation",
                ));
            }
            Ok(())
        }
    }
}

fn validate_acknowledgement_progress(
    previous: &ExecutionAdmissionRecord,
    next: &ExecutionAdmissionRecord,
) -> Result<(), ExecutionAdmissionError> {
    if previous.state == ExecutionAdmissionState::Draining
        && next.acknowledgement != previous.acknowledgement
    {
        return Err(ExecutionAdmissionError::new(
            "acknowledgement",
            "drain_acknowledgement_drift",
            "draining acknowledgement must remain exact through terminal observation",
        ));
    }
    Ok(())
}

fn validate_terminal_transition_reason(
    previous: &ExecutionAdmissionRecord,
    next: &ExecutionAdmissionRecord,
) -> Result<(), ExecutionAdmissionError> {
    if next.state == ExecutionAdmissionState::Unavailable
        && matches!(
            next.unavailable_reason,
            Some(UnavailableReason::Cancelled | UnavailableReason::Drained)
        )
        && previous.state != ExecutionAdmissionState::Draining
    {
        return Err(ExecutionAdmissionError::new(
            "state",
            "draining_state_missing",
            "cancelled or drained terminal state requires an explicit draining observation",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::to_string;

    use super::{
        DrainAcknowledgement, EpochMillis, ExecutionAdmissionIdentity, ExecutionAdmissionInput,
        ExecutionAdmissionRecord, ExecutionAdmissionState, ExecutionRequestId,
        ExecutionResourceLimits, FallbackProfileEligibility, HostCapacityObservation,
        QueuePosition, ReservationEvidence, ReservationGeneration, ReservationId, RunnerProfileId,
        UnavailableReason,
    };
    use crate::verification_profile::VerificationProfileId;

    fn time(value: u64) -> EpochMillis {
        EpochMillis::new(value).expect("valid test time")
    }

    fn limits(cpu_millis: u32, memory_gib: u64, pids: u32) -> ExecutionResourceLimits {
        ExecutionResourceLimits::new(cpu_millis, memory_gib * 1024 * 1024 * 1024, pids)
            .expect("valid test limits")
    }

    fn identity(request: &str) -> ExecutionAdmissionIdentity {
        ExecutionAdmissionIdentity::new(
            ExecutionRequestId::parse(request).expect("valid request ID"),
            VerificationProfileId::parse("quarry.pre-ready").expect("valid verification profile"),
            RunnerProfileId::parse("operator-machine").expect("valid runner profile"),
        )
    }

    fn fallback() -> FallbackProfileEligibility {
        FallbackProfileEligibility::eligible(
            RunnerProfileId::parse("shared-vps").expect("valid fallback profile"),
        )
    }

    fn capacity(observed_at: u64) -> HostCapacityObservation {
        HostCapacityObservation::new(time(observed_at), limits(4_000, 8, 4_096))
    }

    fn reservation(generation: u64, reserved_at: u64, expires_at: u64) -> ReservationEvidence {
        ReservationEvidence::new(
            ReservationId::parse("reservation-a").expect("valid reservation ID"),
            ReservationGeneration::new(generation).expect("valid generation"),
            time(reserved_at),
            time(expires_at),
        )
    }

    fn record(input: ExecutionAdmissionInput) -> ExecutionAdmissionRecord {
        ExecutionAdmissionRecord::from_input(input).expect("valid admission record")
    }

    fn requested(observed_at: u64) -> ExecutionAdmissionRecord {
        record(ExecutionAdmissionInput {
            identity: identity("request-a"),
            state: ExecutionAdmissionState::Requested,
            observed_at: time(observed_at),
            requested_limits: limits(3_500, 6, 2_048),
            host_capacity: None,
            applied_limits: None,
            queue_position: None,
            reservation: None,
            acknowledgement: None,
            fallback_eligibility: fallback(),
            unavailable_reason: None,
        })
    }

    fn observed(
        state: ExecutionAdmissionState,
        observed_at: u64,
        host_observed_at: u64,
        queue_position: Option<QueuePosition>,
        reservation: Option<ReservationEvidence>,
        applied_limits: Option<ExecutionResourceLimits>,
        acknowledgement: Option<DrainAcknowledgement>,
        unavailable_reason: Option<UnavailableReason>,
    ) -> ExecutionAdmissionRecord {
        record(ExecutionAdmissionInput {
            identity: identity("request-a"),
            state,
            observed_at: time(observed_at),
            requested_limits: limits(3_500, 6, 2_048),
            host_capacity: Some(capacity(host_observed_at)),
            applied_limits,
            queue_position,
            reservation,
            acknowledgement,
            fallback_eligibility: fallback(),
            unavailable_reason,
        })
    }

    #[test]
    fn happy_path_reaches_terminal_state_through_draining() {
        let applied = limits(3_000, 6, 1_024);
        let reservation = reservation(7, 150, 10_000);
        let admitted = observed(
            ExecutionAdmissionState::Admitted,
            120,
            110,
            None,
            None,
            None,
            None,
            None,
        );
        requested(100)
            .plan_transition(admitted.clone())
            .expect("admit");
        let reserved = observed(
            ExecutionAdmissionState::Reserved,
            150,
            140,
            None,
            Some(reservation.clone()),
            Some(applied),
            None,
            None,
        );
        admitted.plan_transition(reserved.clone()).expect("reserve");
        let starting = observed(
            ExecutionAdmissionState::Starting,
            160,
            155,
            None,
            Some(reservation.clone()),
            Some(applied),
            None,
            None,
        );
        reserved.plan_transition(starting.clone()).expect("start");
        let running = observed(
            ExecutionAdmissionState::Running,
            170,
            160,
            None,
            Some(reservation.clone()),
            Some(applied),
            None,
            None,
        );
        starting.plan_transition(running.clone()).expect("run");
        let draining = observed(
            ExecutionAdmissionState::Draining,
            180,
            175,
            None,
            Some(reservation.clone()),
            Some(applied),
            Some(DrainAcknowledgement::Drain),
            None,
        );
        running
            .plan_transition(draining.clone())
            .expect("begin drain");
        assert!(!draining.state().is_terminal());
        let drained = observed(
            ExecutionAdmissionState::Unavailable,
            190,
            185,
            None,
            Some(reservation),
            Some(applied),
            Some(DrainAcknowledgement::Drain),
            Some(UnavailableReason::Drained),
        );
        assert!(
            draining
                .plan_transition(drained)
                .expect("finish drain")
                .resulting_record()
                .state()
                .is_terminal()
        );
    }

    #[test]
    fn active_states_accept_monotonic_heartbeats() {
        let applied = limits(3_000, 6, 1_024);
        let reservation = reservation(7, 150, 10_000);
        for state in [
            ExecutionAdmissionState::Admitted,
            ExecutionAdmissionState::Queued,
            ExecutionAdmissionState::Reserved,
            ExecutionAdmissionState::Starting,
            ExecutionAdmissionState::Running,
            ExecutionAdmissionState::Draining,
        ] {
            let queue = (state == ExecutionAdmissionState::Queued).then(QueuePosition::unknown);
            let reserved = matches!(
                state,
                ExecutionAdmissionState::Reserved
                    | ExecutionAdmissionState::Starting
                    | ExecutionAdmissionState::Running
                    | ExecutionAdmissionState::Draining
            );
            let acknowledgement = (state == ExecutionAdmissionState::Draining)
                .then_some(DrainAcknowledgement::Cancellation);
            let current = observed(
                state,
                200,
                190,
                queue,
                reserved.then(|| reservation.clone()),
                reserved.then_some(applied),
                acknowledgement,
                None,
            );
            let next = observed(
                state,
                220,
                210,
                queue,
                reserved.then(|| reservation.clone()),
                reserved.then_some(applied),
                acknowledgement,
                None,
            );
            current.plan_transition(next).expect("heartbeat");
        }
    }

    #[test]
    fn reservation_lifetime_is_stable_across_capacity_refresh() {
        let applied = limits(3_000, 6, 1_024);
        let reservation = reservation(7, 150, 10_000);
        let current = observed(
            ExecutionAdmissionState::Running,
            200,
            190,
            None,
            Some(reservation.clone()),
            Some(applied),
            None,
            None,
        );
        let heartbeat = observed(
            ExecutionAdmissionState::Running,
            9_000,
            8_900,
            None,
            Some(reservation),
            Some(applied),
            None,
            None,
        );
        current.plan_transition(heartbeat).expect("late heartbeat");
    }

    #[test]
    fn cancelled_terminal_state_requires_explicit_matching_draining_state() {
        let applied = limits(3_000, 6, 1_024);
        let reservation = reservation(7, 150, 10_000);
        let running = observed(
            ExecutionAdmissionState::Running,
            170,
            160,
            None,
            Some(reservation.clone()),
            Some(applied),
            None,
            None,
        );
        let cancelled = observed(
            ExecutionAdmissionState::Unavailable,
            180,
            175,
            None,
            Some(reservation.clone()),
            Some(applied),
            Some(DrainAcknowledgement::Cancellation),
            Some(UnavailableReason::Cancelled),
        );
        assert_eq!(
            running
                .plan_transition(cancelled)
                .expect_err("must drain first")
                .code,
            "draining_state_missing"
        );

        let draining = observed(
            ExecutionAdmissionState::Draining,
            175,
            170,
            None,
            Some(reservation.clone()),
            Some(applied),
            Some(DrainAcknowledgement::Cancellation),
            None,
        );
        let wrong = observed(
            ExecutionAdmissionState::Unavailable,
            180,
            175,
            None,
            Some(reservation),
            Some(applied),
            Some(DrainAcknowledgement::Drain),
            Some(UnavailableReason::Drained),
        );
        assert_eq!(
            draining
                .plan_transition(wrong)
                .expect_err("acknowledgement drift must fail")
                .code,
            "drain_acknowledgement_drift"
        );
    }

    #[test]
    fn stale_capacity_and_reservation_generation_are_rejected() {
        let applied = limits(3_000, 6, 1_024);
        let current = observed(
            ExecutionAdmissionState::Reserved,
            200,
            190,
            None,
            Some(reservation(7, 150, 10_000)),
            Some(applied),
            None,
            None,
        );
        let stale_capacity = observed(
            ExecutionAdmissionState::Starting,
            210,
            180,
            None,
            Some(reservation(7, 150, 10_000)),
            Some(applied),
            None,
            None,
        );
        assert_eq!(
            current
                .plan_transition(stale_capacity)
                .expect_err("stale capacity must fail")
                .code,
            "stale_host_capacity_observation"
        );
        let changed_generation = observed(
            ExecutionAdmissionState::Starting,
            210,
            200,
            None,
            Some(reservation(8, 150, 10_000)),
            Some(applied),
            None,
            None,
        );
        assert_eq!(
            current
                .plan_transition(changed_generation)
                .expect_err("generation drift must fail")
                .code,
            "reservation_generation_drift"
        );
    }

    #[test]
    fn queue_evidence_is_known_or_explicitly_unknown() {
        let missing = ExecutionAdmissionRecord::from_input(ExecutionAdmissionInput {
            identity: identity("request-a"),
            state: ExecutionAdmissionState::Queued,
            observed_at: time(120),
            requested_limits: limits(3_500, 6, 2_048),
            host_capacity: Some(capacity(110)),
            applied_limits: None,
            queue_position: None,
            reservation: None,
            acknowledgement: None,
            fallback_eligibility: fallback(),
            unavailable_reason: None,
        })
        .expect_err("missing queue evidence must fail");
        assert_eq!(missing.code, "invalid_queue_evidence");
        assert!(QueuePosition::known(1).is_ok());
        assert!(QueuePosition::known(0).is_err());
    }

    #[test]
    fn expired_reservation_can_only_terminate_explicitly() {
        let applied = limits(3_000, 6, 1_024);
        let invalid = ExecutionAdmissionRecord::from_input(ExecutionAdmissionInput {
            identity: identity("request-a"),
            state: ExecutionAdmissionState::Running,
            observed_at: time(500),
            requested_limits: limits(3_500, 6, 2_048),
            host_capacity: Some(capacity(490)),
            applied_limits: Some(applied),
            queue_position: None,
            reservation: Some(reservation(7, 150, 500)),
            acknowledgement: None,
            fallback_eligibility: fallback(),
            unavailable_reason: None,
        })
        .expect_err("expired reservation must fail");
        assert_eq!(invalid.code, "reservation_expired");

        let expired = observed(
            ExecutionAdmissionState::Unavailable,
            500,
            490,
            None,
            Some(reservation(7, 150, 500)),
            Some(applied),
            None,
            Some(UnavailableReason::ReservationExpired),
        );
        assert_eq!(
            expired.unavailable_reason(),
            Some(UnavailableReason::ReservationExpired)
        );
    }

    #[test]
    fn identity_and_fallback_drift_are_rejected() {
        let current = requested(100);
        let changed = ExecutionAdmissionRecord::from_input(ExecutionAdmissionInput {
            identity: identity("request-b"),
            state: ExecutionAdmissionState::Admitted,
            observed_at: time(120),
            requested_limits: limits(3_500, 6, 2_048),
            host_capacity: Some(capacity(110)),
            applied_limits: None,
            queue_position: None,
            reservation: None,
            acknowledgement: None,
            fallback_eligibility: fallback(),
            unavailable_reason: None,
        })
        .expect("valid changed record");
        assert_eq!(
            current
                .plan_transition(changed)
                .expect_err("identity drift must fail")
                .code,
            "admission_identity_drift"
        );

        let invalid_fallback = ExecutionAdmissionRecord::from_input(ExecutionAdmissionInput {
            identity: identity("request-a"),
            state: ExecutionAdmissionState::Requested,
            observed_at: time(100),
            requested_limits: limits(3_500, 6, 2_048),
            host_capacity: None,
            applied_limits: None,
            queue_position: None,
            reservation: None,
            acknowledgement: None,
            fallback_eligibility: FallbackProfileEligibility::eligible(
                RunnerProfileId::parse("operator-machine").expect("valid runner profile"),
            ),
            unavailable_reason: None,
        })
        .expect_err("primary runner cannot be its own fallback");
        assert_eq!(invalid_fallback.code, "fallback_profile_matches_primary");
    }

    #[test]
    fn capacity_unavailable_requires_observed_shortfall() {
        let invalid = ExecutionAdmissionRecord::from_input(ExecutionAdmissionInput {
            identity: identity("request-a"),
            state: ExecutionAdmissionState::Unavailable,
            observed_at: time(150),
            requested_limits: limits(3_500, 6, 2_048),
            host_capacity: Some(capacity(140)),
            applied_limits: None,
            queue_position: None,
            reservation: None,
            acknowledgement: None,
            fallback_eligibility: fallback(),
            unavailable_reason: Some(UnavailableReason::CapacityUnavailable),
        })
        .expect_err("capacity claim without shortfall must fail");
        assert_eq!(invalid.code, "capacity_unavailability_unproven");

        let shortfall = HostCapacityObservation::new(time(140), limits(2_000, 4, 1_024));
        let valid = ExecutionAdmissionRecord::from_input(ExecutionAdmissionInput {
            identity: identity("request-a"),
            state: ExecutionAdmissionState::Unavailable,
            observed_at: time(150),
            requested_limits: limits(3_500, 6, 2_048),
            host_capacity: Some(shortfall),
            applied_limits: None,
            queue_position: None,
            reservation: None,
            acknowledgement: None,
            fallback_eligibility: fallback(),
            unavailable_reason: Some(UnavailableReason::CapacityUnavailable),
        });
        assert!(valid.is_ok());
    }

    #[test]
    fn only_unavailable_is_terminal() {
        assert!(!ExecutionAdmissionState::Draining.is_terminal());
        assert!(ExecutionAdmissionState::Unavailable.is_terminal());
        let unavailable = observed(
            ExecutionAdmissionState::Unavailable,
            180,
            170,
            None,
            None,
            None,
            None,
            Some(UnavailableReason::HostUnavailable),
        );
        let admitted = observed(
            ExecutionAdmissionState::Admitted,
            190,
            180,
            None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(
            unavailable
                .plan_transition(admitted)
                .expect_err("terminal reversal must fail")
                .code,
            "terminal_state_reversal"
        );
    }

    #[test]
    fn public_output_and_errors_exclude_private_values() {
        let record = observed(
            ExecutionAdmissionState::Queued,
            130,
            120,
            Some(QueuePosition::unknown()),
            None,
            None,
            None,
            None,
        );
        let public = to_string(&record).expect("serialize record");
        assert!(!public.contains("/home/"));
        assert!(!public.contains("token"));
        assert!(!public.contains("environment"));
        assert!(!public.contains("stdout"));
        assert!(!public.contains("stderr"));

        let error = ReservationId::parse("/home/private/token=secret")
            .expect_err("path-shaped value must fail");
        let rendered = format!("{error:?} {error}");
        assert!(!rendered.contains("/home/private"));
        assert!(!rendered.contains("token=secret"));
    }
}
