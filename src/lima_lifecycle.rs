use std::fmt;

use serde::Serialize;

use crate::artifact::Sha256Digest;
use crate::execution_admission::{EpochMillis, ReservationId};

pub const LIMA_LIFECYCLE_SCHEMA_VERSION: u8 = 1;
pub const MAX_LIMA_OBSERVATION_AGE_MILLIS: u64 = 300_000;
pub const INTERACTIVE_AFTER_IDLE_MILLIS: u64 = 10 * 60 * 1000;
pub const STOP_AFTER_IDLE_MILLIS: u64 = 30 * 60 * 1000;
pub const INTERACTIVE_VCPUS: u16 = 4;
pub const INTERACTIVE_MEMORY_BYTES: u64 = 3 * 1024 * 1024 * 1024;
pub const WORK_VCPUS: u16 = 8;
pub const WORK_MEMORY_BYTES: u64 = 10 * 1024 * 1024 * 1024;

const MAX_IDENTIFIER_LEN: usize = 96;
const MAX_VCPUS: u16 = 256;
const MAX_MEMORY_BYTES: u64 = 1_u64 << 50;

macro_rules! identifier_type {
    ($name:ident, $field:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Parse one bounded opaque lifecycle identifier.
            ///
            /// # Errors
            ///
            /// Returns an error for empty, oversized, non-ASCII, or path-shaped values.
            pub fn parse(value: &str) -> Result<Self, LimaLifecycleError> {
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

identifier_type!(LimaInstanceId, "identity.instance_id");
identifier_type!(LimaCacheDiskId, "identity.cache_disk.disk_id");

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct LimaProfileGeneration(u64);

impl LimaProfileGeneration {
    /// Define one nonzero monotonic profile generation.
    ///
    /// # Errors
    ///
    /// Returns an error for generation zero.
    pub fn new(value: u64) -> Result<Self, LimaLifecycleError> {
        if value == 0 {
            return Err(LimaLifecycleError::new(
                "profile_generation",
                "invalid_profile_generation",
                "profile generation must be greater than zero",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    fn checked_next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LimaResourceProfile {
    Interactive,
    Work,
}

impl LimaResourceProfile {
    #[must_use]
    pub const fn envelope(self) -> LimaResourceEnvelope {
        match self {
            Self::Interactive => LimaResourceEnvelope {
                vcpus: INTERACTIVE_VCPUS,
                memory_bytes: INTERACTIVE_MEMORY_BYTES,
            },
            Self::Work => LimaResourceEnvelope {
                vcpus: WORK_VCPUS,
                memory_bytes: WORK_MEMORY_BYTES,
            },
        }
    }

    #[must_use]
    pub const fn idle_deadline_offset_millis(self) -> u64 {
        match self {
            Self::Work => INTERACTIVE_AFTER_IDLE_MILLIS,
            Self::Interactive => STOP_AFTER_IDLE_MILLIS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LimaResourceEnvelope {
    pub vcpus: u16,
    pub memory_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LimaObservedResources {
    vcpus: u16,
    memory_bytes: u64,
}

impl LimaObservedResources {
    /// Record one bounded observed CPU and memory envelope.
    ///
    /// # Errors
    ///
    /// Returns an error for zero or excessive values.
    pub fn new(vcpus: u16, memory_bytes: u64) -> Result<Self, LimaLifecycleError> {
        if !(1..=MAX_VCPUS).contains(&vcpus) {
            return Err(LimaLifecycleError::new(
                "observed_resources.vcpus",
                "invalid_vcpu_observation",
                "observed vCPU count must be within the bounded positive range",
            ));
        }
        if !(1..=MAX_MEMORY_BYTES).contains(&memory_bytes) {
            return Err(LimaLifecycleError::new(
                "observed_resources.memory_bytes",
                "invalid_memory_observation",
                "observed memory bytes must be within the bounded positive range",
            ));
        }
        Ok(Self {
            vcpus,
            memory_bytes,
        })
    }

    #[must_use]
    pub const fn vcpus(self) -> u16 {
        self.vcpus
    }

    #[must_use]
    pub const fn memory_bytes(self) -> u64 {
        self.memory_bytes
    }

    #[must_use]
    pub const fn for_profile(profile: LimaResourceProfile) -> Self {
        let envelope = profile.envelope();
        Self {
            vcpus: envelope.vcpus,
            memory_bytes: envelope.memory_bytes,
        }
    }

    const fn matches_profile(self, profile: LimaResourceProfile) -> bool {
        let envelope = profile.envelope();
        self.vcpus == envelope.vcpus && self.memory_bytes == envelope.memory_bytes
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LimaCacheDiskIdentity {
    disk_id: LimaCacheDiskId,
    identity_digest: Sha256Digest,
}

impl LimaCacheDiskIdentity {
    #[must_use]
    pub const fn new(disk_id: LimaCacheDiskId, identity_digest: Sha256Digest) -> Self {
        Self {
            disk_id,
            identity_digest,
        }
    }

    #[must_use]
    pub const fn disk_id(&self) -> &LimaCacheDiskId {
        &self.disk_id
    }

    #[must_use]
    pub const fn identity_digest(&self) -> &Sha256Digest {
        &self.identity_digest
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LimaInstanceIdentity {
    instance_id: LimaInstanceId,
    cache_disk: LimaCacheDiskIdentity,
}

impl LimaInstanceIdentity {
    #[must_use]
    pub const fn new(instance_id: LimaInstanceId, cache_disk: LimaCacheDiskIdentity) -> Self {
        Self {
            instance_id,
            cache_disk,
        }
    }

    #[must_use]
    pub const fn instance_id(&self) -> &LimaInstanceId {
        &self.instance_id
    }

    #[must_use]
    pub const fn cache_disk(&self) -> &LimaCacheDiskIdentity {
        &self.cache_disk
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LimaLifecycleState {
    Stopped,
    Starting,
    Running,
    Draining,
    Stopping,
    Unavailable,
}

impl LimaLifecycleState {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Unavailable)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LimaLifecycleTarget {
    Work,
    Interactive,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LimaDrainAcknowledgement {
    NotRequired,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GracefulStopAcknowledgement {
    acknowledged_at: EpochMillis,
    profile_generation: LimaProfileGeneration,
    cache_disk: LimaCacheDiskIdentity,
    drain: LimaDrainAcknowledgement,
}

impl GracefulStopAcknowledgement {
    #[must_use]
    pub const fn new(
        acknowledged_at: EpochMillis,
        profile_generation: LimaProfileGeneration,
        cache_disk: LimaCacheDiskIdentity,
        drain: LimaDrainAcknowledgement,
    ) -> Self {
        Self {
            acknowledged_at,
            profile_generation,
            cache_disk,
            drain,
        }
    }

    #[must_use]
    pub const fn acknowledged_at(&self) -> EpochMillis {
        self.acknowledged_at
    }

    #[must_use]
    pub const fn profile_generation(&self) -> LimaProfileGeneration {
        self.profile_generation
    }

    #[must_use]
    pub const fn cache_disk(&self) -> &LimaCacheDiskIdentity {
        &self.cache_disk
    }

    #[must_use]
    pub const fn drain(&self) -> LimaDrainAcknowledgement {
        self.drain
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimaLifecycleObservationDefinition {
    pub identity: LimaInstanceIdentity,
    pub state: LimaLifecycleState,
    pub profile: LimaResourceProfile,
    pub profile_generation: LimaProfileGeneration,
    pub observed_resources: LimaObservedResources,
    pub observed_at: EpochMillis,
    pub active_reservation_id: Option<ReservationId>,
    pub last_activity_at: EpochMillis,
    pub idle_deadline: EpochMillis,
    pub graceful_stop_acknowledgement: Option<GracefulStopAcknowledgement>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LimaLifecycleObservation {
    schema_version: u8,
    identity: LimaInstanceIdentity,
    state: LimaLifecycleState,
    profile: LimaResourceProfile,
    profile_generation: LimaProfileGeneration,
    observed_resources: LimaObservedResources,
    observed_at: EpochMillis,
    #[serde(skip_serializing_if = "Option::is_none")]
    active_reservation_id: Option<ReservationId>,
    last_activity_at: EpochMillis,
    idle_deadline: EpochMillis,
    #[serde(skip_serializing_if = "Option::is_none")]
    graceful_stop_acknowledgement: Option<GracefulStopAcknowledgement>,
}

impl LimaLifecycleObservation {
    /// Validate one complete already-observed Lima lifecycle record.
    ///
    /// # Errors
    ///
    /// Returns an error for resource-profile drift, invalid activity/cooldown evidence, reservation
    /// evidence in an impossible state, or stale graceful-stop evidence.
    pub fn new(definition: LimaLifecycleObservationDefinition) -> Result<Self, LimaLifecycleError> {
        validate_observation_shape(&definition)?;
        Ok(Self {
            schema_version: LIMA_LIFECYCLE_SCHEMA_VERSION,
            identity: definition.identity,
            state: definition.state,
            profile: definition.profile,
            profile_generation: definition.profile_generation,
            observed_resources: definition.observed_resources,
            observed_at: definition.observed_at,
            active_reservation_id: definition.active_reservation_id,
            last_activity_at: definition.last_activity_at,
            idle_deadline: definition.idle_deadline,
            graceful_stop_acknowledgement: definition.graceful_stop_acknowledgement,
        })
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn identity(&self) -> &LimaInstanceIdentity {
        &self.identity
    }

    #[must_use]
    pub const fn state(&self) -> LimaLifecycleState {
        self.state
    }

    #[must_use]
    pub const fn profile(&self) -> LimaResourceProfile {
        self.profile
    }

    #[must_use]
    pub const fn profile_generation(&self) -> LimaProfileGeneration {
        self.profile_generation
    }

    #[must_use]
    pub const fn observed_resources(&self) -> LimaObservedResources {
        self.observed_resources
    }

    #[must_use]
    pub const fn observed_at(&self) -> EpochMillis {
        self.observed_at
    }

    #[must_use]
    pub fn active_reservation_id(&self) -> Option<&ReservationId> {
        self.active_reservation_id.as_ref()
    }

    #[must_use]
    pub const fn last_activity_at(&self) -> EpochMillis {
        self.last_activity_at
    }

    #[must_use]
    pub const fn idle_deadline(&self) -> EpochMillis {
        self.idle_deadline
    }

    #[must_use]
    pub fn graceful_stop_acknowledgement(&self) -> Option<&GracefulStopAcknowledgement> {
        self.graceful_stop_acknowledgement.as_ref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LimaLifecyclePolicy {
    schema_version: u8,
    max_observation_age_millis: u64,
}

impl LimaLifecyclePolicy {
    /// Define one bounded freshness policy for pure lifecycle decisions.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero or excessive freshness window.
    pub fn new(max_observation_age_millis: u64) -> Result<Self, LimaLifecycleError> {
        if !(1..=MAX_LIMA_OBSERVATION_AGE_MILLIS).contains(&max_observation_age_millis) {
            return Err(LimaLifecycleError::new(
                "policy.max_observation_age_millis",
                "invalid_freshness_window",
                "freshness window must be positive and within the reviewed maximum",
            ));
        }
        Ok(Self {
            schema_version: LIMA_LIFECYCLE_SCHEMA_VERSION,
            max_observation_age_millis,
        })
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn max_observation_age_millis(&self) -> u64 {
        self.max_observation_age_millis
    }

    /// Select the exact reviewed steady-state target from trusted lifecycle evidence.
    ///
    /// Active work always selects `work`. With no active reservation, the worker remains on
    /// `work` for the first ten idle minutes, selects `interactive` until thirty total idle
    /// minutes, and then selects `stopped`. Time is caller-supplied; this method reads no clock.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for stale or future lifecycle evidence.
    pub fn desired_target(
        &self,
        observation: &LimaLifecycleObservation,
        decision_at: EpochMillis,
    ) -> Result<LimaLifecycleTarget, LimaLifecycleError> {
        validate_freshness(
            "observation.observed_at",
            observation.observed_at,
            decision_at,
            self.max_observation_age_millis,
        )?;
        if observation.active_reservation_id.is_some() {
            return Ok(LimaLifecycleTarget::Work);
        }
        let idle_millis = decision_at
            .get()
            .checked_sub(observation.last_activity_at.get())
            .ok_or_else(|| {
                LimaLifecycleError::new(
                    "last_activity_at",
                    "future_last_activity",
                    "last activity cannot be newer than the lifecycle decision",
                )
            })?;
        if idle_millis >= STOP_AFTER_IDLE_MILLIS {
            Ok(LimaLifecycleTarget::Stopped)
        } else if idle_millis >= INTERACTIVE_AFTER_IDLE_MILLIS {
            Ok(LimaLifecycleTarget::Interactive)
        } else {
            Ok(LimaLifecycleTarget::Work)
        }
    }

    /// Validate one exact lifecycle transition without executing it.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for stale or reversing observations, identity or cache drift,
    /// invalid state movement, active-work profile changes or stop, generation drift, downscale
    /// without a completed graceful drain, stop before cooldown, or terminal reversal.
    pub fn plan_transition(
        &self,
        current: &LimaLifecycleObservation,
        next: LimaLifecycleObservation,
        decision_at: EpochMillis,
    ) -> Result<LimaLifecycleTransition, LimaLifecycleError> {
        if current.state.is_terminal() {
            return Err(LimaLifecycleError::new(
                "state",
                "terminal_state_reversal",
                "unavailable lifecycle state cannot transition",
            ));
        }

        validate_freshness(
            "current.observed_at",
            current.observed_at,
            decision_at,
            self.max_observation_age_millis,
        )?;
        validate_freshness(
            "next.observed_at",
            next.observed_at,
            decision_at,
            self.max_observation_age_millis,
        )?;
        if next.observed_at < current.observed_at {
            return Err(LimaLifecycleError::new(
                "next.observed_at",
                "observation_time_reversal",
                "lifecycle observation time cannot move backwards",
            ));
        }

        if next.identity.instance_id() != current.identity.instance_id() {
            return Err(LimaLifecycleError::new(
                "identity.instance_id",
                "instance_identity_drift",
                "Lima instance identity must remain exact across transitions",
            ));
        }
        if next.identity.cache_disk() != current.identity.cache_disk() {
            return Err(LimaLifecycleError::new(
                "identity.cache_disk",
                "cache_identity_drift",
                "persistent cache-disk identity must remain exact across transitions",
            ));
        }

        validate_activity_progress(current, &next)?;
        validate_profile_progress(current, &next)?;
        validate_reservation_progress(current, &next)?;

        if next.state == LimaLifecycleState::Stopping {
            if current.active_reservation_id.is_some() {
                return Err(LimaLifecycleError::new(
                    "active_reservation_id",
                    "stop_while_active",
                    "Lima worker cannot stop while a job or reservation remains active",
                ));
            }
            if decision_at < current.idle_deadline {
                return Err(LimaLifecycleError::new(
                    "idle_deadline",
                    "stop_before_cooldown",
                    "Lima worker cannot stop before the explicit idle deadline",
                ));
            }
        }

        validate_transition_pair(current.state, next.state)?;
        validate_stop_acknowledgement_progress(current, &next)?;

        Ok(LimaLifecycleTransition {
            schema_version: LIMA_LIFECYCLE_SCHEMA_VERSION,
            identity: current.identity.clone(),
            from: current.state,
            to: next.state,
            from_profile: current.profile,
            to_profile: next.profile,
            profile_generation: next.profile_generation,
            observed_at: next.observed_at,
            resulting_observation: next,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LimaLifecycleTransition {
    schema_version: u8,
    identity: LimaInstanceIdentity,
    from: LimaLifecycleState,
    to: LimaLifecycleState,
    from_profile: LimaResourceProfile,
    to_profile: LimaResourceProfile,
    profile_generation: LimaProfileGeneration,
    observed_at: EpochMillis,
    resulting_observation: LimaLifecycleObservation,
}

impl LimaLifecycleTransition {
    #[must_use]
    pub const fn from(&self) -> LimaLifecycleState {
        self.from
    }

    #[must_use]
    pub const fn to(&self) -> LimaLifecycleState {
        self.to
    }

    #[must_use]
    pub const fn from_profile(&self) -> LimaResourceProfile {
        self.from_profile
    }

    #[must_use]
    pub const fn to_profile(&self) -> LimaResourceProfile {
        self.to_profile
    }

    #[must_use]
    pub const fn profile_generation(&self) -> LimaProfileGeneration {
        self.profile_generation
    }

    #[must_use]
    pub const fn observed_at(&self) -> EpochMillis {
        self.observed_at
    }

    #[must_use]
    pub const fn identity(&self) -> &LimaInstanceIdentity {
        &self.identity
    }

    #[must_use]
    pub const fn resulting_observation(&self) -> &LimaLifecycleObservation {
        &self.resulting_observation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LimaLifecycleError {
    pub field: &'static str,
    pub code: &'static str,
    pub message: &'static str,
}

impl LimaLifecycleError {
    const fn new(field: &'static str, code: &'static str, message: &'static str) -> Self {
        Self {
            field,
            code,
            message,
        }
    }
}

impl fmt::Display for LimaLifecycleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for LimaLifecycleError {}

fn validate_identifier(field: &'static str, value: &str) -> Result<(), LimaLifecycleError> {
    let mut bytes = value.bytes();
    let first_is_safe = bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    let remaining_are_safe = bytes.all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
    });
    if value.len() > MAX_IDENTIFIER_LEN || !first_is_safe || !remaining_are_safe {
        return Err(LimaLifecycleError::new(
            field,
            "invalid_identifier",
            "identifier must be bounded lowercase ASCII without path or log content",
        ));
    }
    Ok(())
}

fn validate_observation_shape(
    definition: &LimaLifecycleObservationDefinition,
) -> Result<(), LimaLifecycleError> {
    if !definition
        .observed_resources
        .matches_profile(definition.profile)
    {
        return Err(LimaLifecycleError::new(
            "observed_resources",
            "profile_resource_mismatch",
            "observed CPU and memory must exactly match the reviewed Lima profile",
        ));
    }
    if definition.last_activity_at > definition.observed_at {
        return Err(LimaLifecycleError::new(
            "last_activity_at",
            "future_last_activity",
            "last activity cannot be newer than the lifecycle observation",
        ));
    }
    let expected_idle_deadline = definition
        .last_activity_at
        .get()
        .checked_add(definition.profile.idle_deadline_offset_millis())
        .ok_or_else(|| {
            LimaLifecycleError::new(
                "idle_deadline",
                "invalid_idle_deadline",
                "reviewed idle deadline exceeds the supported time range",
            )
        })?;
    if definition.idle_deadline.get() != expected_idle_deadline {
        return Err(LimaLifecycleError::new(
            "idle_deadline",
            "idle_deadline_policy_mismatch",
            "work requires a ten-minute idle deadline and interactive requires thirty total idle minutes",
        ));
    }

    let reservation_allowed = matches!(
        definition.state,
        LimaLifecycleState::Running
            | LimaLifecycleState::Draining
            | LimaLifecycleState::Unavailable
    );
    if definition.active_reservation_id.is_some() && !reservation_allowed {
        return Err(LimaLifecycleError::new(
            "active_reservation_id",
            "reservation_in_invalid_state",
            "active reservation identity is valid only while running, draining, or unavailable",
        ));
    }
    if definition.active_reservation_id.is_some() && definition.profile != LimaResourceProfile::Work
    {
        return Err(LimaLifecycleError::new(
            "profile",
            "active_reservation_requires_work",
            "active jobs and reservations require the exact work profile",
        ));
    }

    let acknowledgement_allowed = matches!(
        definition.state,
        LimaLifecycleState::Stopping | LimaLifecycleState::Stopped
    );
    if definition.graceful_stop_acknowledgement.is_some() && !acknowledgement_allowed {
        return Err(LimaLifecycleError::new(
            "graceful_stop_acknowledgement",
            "acknowledgement_in_invalid_state",
            "graceful-stop acknowledgement is valid only while stopping or stopped",
        ));
    }
    if let Some(acknowledgement) = &definition.graceful_stop_acknowledgement {
        if acknowledgement.profile_generation != definition.profile_generation {
            return Err(LimaLifecycleError::new(
                "graceful_stop_acknowledgement.profile_generation",
                "acknowledgement_generation_drift",
                "graceful-stop acknowledgement must bind the exact profile generation",
            ));
        }
        if acknowledgement.cache_disk() != definition.identity.cache_disk() {
            return Err(LimaLifecycleError::new(
                "graceful_stop_acknowledgement.cache_disk",
                "acknowledgement_cache_drift",
                "graceful-stop acknowledgement must bind the exact cache-disk identity",
            ));
        }
        if acknowledgement.acknowledged_at > definition.observed_at
            || acknowledgement.acknowledged_at < definition.last_activity_at
        {
            return Err(LimaLifecycleError::new(
                "graceful_stop_acknowledgement.acknowledged_at",
                "invalid_acknowledgement_time",
                "graceful-stop acknowledgement must follow last activity and not exceed observation time",
            ));
        }
    }

    Ok(())
}

fn validate_freshness(
    field: &'static str,
    observed_at: EpochMillis,
    decision_at: EpochMillis,
    max_age_millis: u64,
) -> Result<(), LimaLifecycleError> {
    let age = decision_at
        .get()
        .checked_sub(observed_at.get())
        .ok_or_else(|| {
            LimaLifecycleError::new(
                field,
                "future_observation",
                "lifecycle observation cannot be newer than the decision time",
            )
        })?;
    if age > max_age_millis {
        return Err(LimaLifecycleError::new(
            field,
            "stale_observation",
            "lifecycle observation is older than the reviewed freshness window",
        ));
    }
    Ok(())
}

fn validate_activity_progress(
    current: &LimaLifecycleObservation,
    next: &LimaLifecycleObservation,
) -> Result<(), LimaLifecycleError> {
    if next.last_activity_at < current.last_activity_at {
        return Err(LimaLifecycleError::new(
            "last_activity_at",
            "activity_time_reversal",
            "last activity time cannot move backwards",
        ));
    }
    if next.last_activity_at == current.last_activity_at
        && next.profile == current.profile
        && next.idle_deadline != current.idle_deadline
    {
        return Err(LimaLifecycleError::new(
            "idle_deadline",
            "idle_deadline_drift",
            "idle deadline may change only when last activity advances",
        ));
    }
    Ok(())
}

fn validate_profile_progress(
    current: &LimaLifecycleObservation,
    next: &LimaLifecycleObservation,
) -> Result<(), LimaLifecycleError> {
    if next.profile == current.profile {
        if next.profile_generation != current.profile_generation {
            return Err(LimaLifecycleError::new(
                "profile_generation",
                "profile_generation_drift",
                "unchanged Lima profile must retain the exact generation",
            ));
        }
        return Ok(());
    }

    if current.active_reservation_id.is_some() || next.active_reservation_id.is_some() {
        return Err(LimaLifecycleError::new(
            "active_reservation_id",
            "profile_change_while_active",
            "Lima profile cannot change while a job or reservation remains active",
        ));
    }
    if current.state != LimaLifecycleState::Stopped || next.state != LimaLifecycleState::Starting {
        return Err(LimaLifecycleError::new(
            "profile",
            "profile_change_outside_stopped_transition",
            "Lima profile may change only from stopped into starting",
        ));
    }
    if current.profile_generation.checked_next() != Some(next.profile_generation) {
        return Err(LimaLifecycleError::new(
            "profile_generation",
            "profile_generation_drift",
            "profile change must advance generation by exactly one",
        ));
    }
    if current.profile == LimaResourceProfile::Work
        && next.profile == LimaResourceProfile::Interactive
        && current
            .graceful_stop_acknowledgement
            .as_ref()
            .is_none_or(|acknowledgement| {
                acknowledgement.drain() != LimaDrainAcknowledgement::Completed
            })
    {
        return Err(LimaLifecycleError::new(
            "graceful_stop_acknowledgement",
            "downscale_without_drain",
            "work-to-interactive downscale requires completed drain and graceful-stop evidence",
        ));
    }

    Ok(())
}

fn validate_reservation_progress(
    current: &LimaLifecycleObservation,
    next: &LimaLifecycleObservation,
) -> Result<(), LimaLifecycleError> {
    if let (Some(current_id), Some(next_id)) = (
        current.active_reservation_id.as_ref(),
        next.active_reservation_id.as_ref(),
    ) && current_id != next_id
    {
        return Err(LimaLifecycleError::new(
            "active_reservation_id",
            "reservation_identity_drift",
            "active reservation identity cannot change without an idle observation",
        ));
    }

    if current.state == LimaLifecycleState::Running
        && next.state == LimaLifecycleState::Draining
        && current.active_reservation_id.is_some()
        && next.active_reservation_id.as_ref() != current.active_reservation_id.as_ref()
    {
        return Err(LimaLifecycleError::new(
            "active_reservation_id",
            "reservation_drain_identity_drift",
            "initial draining observation must retain the active reservation identity",
        ));
    }

    if current.state == LimaLifecycleState::Draining
        && next.state == LimaLifecycleState::Draining
        && current.active_reservation_id.is_none()
        && next.active_reservation_id.is_some()
    {
        return Err(LimaLifecycleError::new(
            "active_reservation_id",
            "reservation_started_while_draining",
            "draining state cannot acquire a new reservation",
        ));
    }

    Ok(())
}

fn validate_transition_pair(
    current: LimaLifecycleState,
    next: LimaLifecycleState,
) -> Result<(), LimaLifecycleError> {
    let valid = match current {
        LimaLifecycleState::Stopped => matches!(
            next,
            LimaLifecycleState::Stopped
                | LimaLifecycleState::Starting
                | LimaLifecycleState::Unavailable
        ),
        LimaLifecycleState::Starting => matches!(
            next,
            LimaLifecycleState::Starting
                | LimaLifecycleState::Running
                | LimaLifecycleState::Unavailable
        ),
        LimaLifecycleState::Running => matches!(
            next,
            LimaLifecycleState::Running
                | LimaLifecycleState::Draining
                | LimaLifecycleState::Stopping
                | LimaLifecycleState::Unavailable
        ),
        LimaLifecycleState::Draining => matches!(
            next,
            LimaLifecycleState::Draining
                | LimaLifecycleState::Running
                | LimaLifecycleState::Stopping
                | LimaLifecycleState::Unavailable
        ),
        LimaLifecycleState::Stopping => matches!(
            next,
            LimaLifecycleState::Stopping
                | LimaLifecycleState::Stopped
                | LimaLifecycleState::Unavailable
        ),
        LimaLifecycleState::Unavailable => false,
    };
    if !valid {
        return Err(LimaLifecycleError::new(
            "state",
            "invalid_lifecycle_transition",
            "requested Lima lifecycle state transition is not permitted",
        ));
    }
    Ok(())
}

fn validate_stop_acknowledgement_progress(
    current: &LimaLifecycleObservation,
    next: &LimaLifecycleObservation,
) -> Result<(), LimaLifecycleError> {
    if next.state == LimaLifecycleState::Stopped && next.graceful_stop_acknowledgement.is_none() {
        return Err(LimaLifecycleError::new(
            "graceful_stop_acknowledgement",
            "graceful_stop_acknowledgement_missing",
            "stopped transition requires exact graceful-stop acknowledgement",
        ));
    }

    if let (Some(current_ack), Some(next_ack)) = (
        current.graceful_stop_acknowledgement.as_ref(),
        next.graceful_stop_acknowledgement.as_ref(),
    ) && current_ack != next_ack
    {
        return Err(LimaLifecycleError::new(
            "graceful_stop_acknowledgement",
            "graceful_stop_acknowledgement_drift",
            "existing graceful-stop acknowledgement must remain exact",
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::Sha256Digest;

    const PRIVATE_PATH: &str = "/Users/operator/private/lima/smolrunner";
    const FRESHNESS_MILLIS: u64 = 30_000;

    fn epoch(value: u64) -> EpochMillis {
        EpochMillis::new(value).expect("epoch")
    }

    fn generation(value: u64) -> LimaProfileGeneration {
        LimaProfileGeneration::new(value).expect("generation")
    }

    fn digest(byte: &str) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", byte.repeat(64))).expect("digest")
    }

    fn cache_disk() -> LimaCacheDiskIdentity {
        LimaCacheDiskIdentity::new(
            LimaCacheDiskId::parse("smolrunner-cache").expect("disk ID"),
            digest("a"),
        )
    }

    fn identity() -> LimaInstanceIdentity {
        LimaInstanceIdentity::new(
            LimaInstanceId::parse("smolrunner").expect("instance ID"),
            cache_disk(),
        )
    }

    fn acknowledgement(
        profile_generation: LimaProfileGeneration,
        acknowledged_at: u64,
    ) -> GracefulStopAcknowledgement {
        GracefulStopAcknowledgement::new(
            epoch(acknowledged_at),
            profile_generation,
            cache_disk(),
            LimaDrainAcknowledgement::Completed,
        )
    }

    fn definition(
        state: LimaLifecycleState,
        profile: LimaResourceProfile,
        profile_generation: u64,
        observed_at: u64,
        last_activity_at: u64,
        _idle_deadline: u64,
    ) -> LimaLifecycleObservationDefinition {
        LimaLifecycleObservationDefinition {
            identity: identity(),
            state,
            profile,
            profile_generation: generation(profile_generation),
            observed_resources: LimaObservedResources::for_profile(profile),
            observed_at: epoch(observed_at),
            active_reservation_id: None,
            last_activity_at: epoch(last_activity_at),
            idle_deadline: epoch(last_activity_at + profile.idle_deadline_offset_millis()),
            graceful_stop_acknowledgement: None,
        }
    }

    fn observation(
        state: LimaLifecycleState,
        profile: LimaResourceProfile,
        profile_generation: u64,
        observed_at: u64,
        last_activity_at: u64,
        idle_deadline: u64,
    ) -> LimaLifecycleObservation {
        LimaLifecycleObservation::new(definition(
            state,
            profile,
            profile_generation,
            observed_at,
            last_activity_at,
            idle_deadline,
        ))
        .expect("observation")
    }

    fn policy() -> LimaLifecyclePolicy {
        LimaLifecyclePolicy::new(FRESHNESS_MILLIS).expect("policy")
    }

    #[test]
    fn reviewed_profiles_are_exact() {
        assert_eq!(
            LimaResourceProfile::Interactive.envelope(),
            LimaResourceEnvelope {
                vcpus: 4,
                memory_bytes: 3 * 1024 * 1024 * 1024,
            }
        );
        assert_eq!(
            LimaResourceProfile::Work.envelope(),
            LimaResourceEnvelope {
                vcpus: 8,
                memory_bytes: 10 * 1024 * 1024 * 1024,
            }
        );
    }

    #[test]
    fn agreed_idle_policy_selects_work_interactive_and_stopped() {
        let last_activity = 1_000;
        let before_interactive = observation(
            LimaLifecycleState::Running,
            LimaResourceProfile::Work,
            1,
            last_activity + INTERACTIVE_AFTER_IDLE_MILLIS - 1,
            last_activity,
            0,
        );
        assert_eq!(
            policy()
                .desired_target(&before_interactive, before_interactive.observed_at())
                .expect("work target"),
            LimaLifecycleTarget::Work
        );

        let interactive = observation(
            LimaLifecycleState::Running,
            LimaResourceProfile::Work,
            1,
            last_activity + INTERACTIVE_AFTER_IDLE_MILLIS,
            last_activity,
            0,
        );
        assert_eq!(
            policy()
                .desired_target(&interactive, interactive.observed_at())
                .expect("interactive target"),
            LimaLifecycleTarget::Interactive
        );

        let stopped = observation(
            LimaLifecycleState::Running,
            LimaResourceProfile::Interactive,
            2,
            last_activity + STOP_AFTER_IDLE_MILLIS,
            last_activity,
            0,
        );
        assert_eq!(
            policy()
                .desired_target(&stopped, stopped.observed_at())
                .expect("stopped target"),
            LimaLifecycleTarget::Stopped
        );

        let mut active_definition = definition(
            LimaLifecycleState::Running,
            LimaResourceProfile::Work,
            1,
            last_activity + STOP_AFTER_IDLE_MILLIS,
            last_activity,
            0,
        );
        active_definition.active_reservation_id =
            Some(ReservationId::parse("reservation-active").expect("reservation"));
        let active = LimaLifecycleObservation::new(active_definition).expect("active work");
        assert_eq!(
            policy()
                .desired_target(&active, active.observed_at())
                .expect("active target"),
            LimaLifecycleTarget::Work
        );
    }

    #[test]
    fn active_reservation_and_idle_deadline_must_match_policy() {
        let mut active_interactive = definition(
            LimaLifecycleState::Running,
            LimaResourceProfile::Interactive,
            1,
            10_000,
            9_000,
            0,
        );
        active_interactive.active_reservation_id =
            Some(ReservationId::parse("reservation-a").expect("reservation"));
        assert_eq!(
            LimaLifecycleObservation::new(active_interactive)
                .expect_err("interactive active reservation")
                .code,
            "active_reservation_requires_work"
        );

        let mut deadline_drift = definition(
            LimaLifecycleState::Running,
            LimaResourceProfile::Work,
            1,
            20_000,
            19_000,
            0,
        );
        deadline_drift.idle_deadline = epoch(deadline_drift.idle_deadline.get() + 1);
        assert_eq!(
            LimaLifecycleObservation::new(deadline_drift)
                .expect_err("deadline drift")
                .code,
            "idle_deadline_policy_mismatch"
        );
    }

    #[test]
    fn exact_stop_edit_start_profile_transition_is_accepted() {
        let mut stopped_definition = definition(
            LimaLifecycleState::Stopped,
            LimaResourceProfile::Work,
            1,
            10_000,
            8_000,
            9_000,
        );
        stopped_definition.graceful_stop_acknowledgement =
            Some(acknowledgement(generation(1), 9_500));
        let stopped = LimaLifecycleObservation::new(stopped_definition).expect("stopped");

        let starting = observation(
            LimaLifecycleState::Starting,
            LimaResourceProfile::Interactive,
            2,
            10_100,
            8_000,
            9_000,
        );
        let transition = policy()
            .plan_transition(&stopped, starting, epoch(10_100))
            .expect("profile transition");
        assert_eq!(transition.from(), LimaLifecycleState::Stopped);
        assert_eq!(transition.to(), LimaLifecycleState::Starting);
        assert_eq!(transition.from_profile(), LimaResourceProfile::Work);
        assert_eq!(transition.to_profile(), LimaResourceProfile::Interactive);
        assert_eq!(transition.profile_generation(), generation(2));

        let running = observation(
            LimaLifecycleState::Running,
            LimaResourceProfile::Interactive,
            2,
            10_200,
            10_200,
            11_200,
        );
        policy()
            .plan_transition(transition.resulting_observation(), running, epoch(10_200))
            .expect("running");
    }

    #[test]
    fn idle_worker_stops_only_with_graceful_acknowledgement() {
        let last_activity = 18_000;
        let stop_at = last_activity + STOP_AFTER_IDLE_MILLIS;
        let running = observation(
            LimaLifecycleState::Running,
            LimaResourceProfile::Interactive,
            4,
            stop_at,
            last_activity,
            0,
        );
        let stopping = observation(
            LimaLifecycleState::Stopping,
            LimaResourceProfile::Interactive,
            4,
            stop_at + 100,
            last_activity,
            0,
        );
        let transition = policy()
            .plan_transition(&running, stopping, epoch(stop_at + 100))
            .expect("stopping");
        assert_eq!(transition.to(), LimaLifecycleState::Stopping);

        let mut stopped_definition = definition(
            LimaLifecycleState::Stopped,
            LimaResourceProfile::Interactive,
            4,
            stop_at + 200,
            last_activity,
            0,
        );
        stopped_definition.graceful_stop_acknowledgement =
            Some(acknowledgement(generation(4), stop_at + 150));
        let stopped = LimaLifecycleObservation::new(stopped_definition).expect("stopped");
        let transition = policy()
            .plan_transition(
                transition.resulting_observation(),
                stopped,
                epoch(stop_at + 200),
            )
            .expect("stopped transition");
        assert_eq!(transition.to(), LimaLifecycleState::Stopped);
        assert_eq!(transition.identity().cache_disk(), &cache_disk());
    }

    #[test]
    fn active_job_refuses_stop_and_profile_change() {
        let mut running_definition = definition(
            LimaLifecycleState::Running,
            LimaResourceProfile::Work,
            1,
            30_000,
            29_000,
            29_500,
        );
        running_definition.active_reservation_id =
            Some(ReservationId::parse("reservation-a").expect("reservation"));
        let running = LimaLifecycleObservation::new(running_definition).expect("running");

        let stopping = observation(
            LimaLifecycleState::Stopping,
            LimaResourceProfile::Work,
            1,
            30_100,
            29_000,
            29_500,
        );
        assert_eq!(
            policy()
                .plan_transition(&running, stopping, epoch(30_100))
                .expect_err("active stop")
                .code,
            "stop_while_active"
        );

        let starting = observation(
            LimaLifecycleState::Starting,
            LimaResourceProfile::Interactive,
            2,
            30_100,
            29_000,
            29_500,
        );
        assert_eq!(
            policy()
                .plan_transition(&running, starting, epoch(30_100))
                .expect_err("active profile change")
                .code,
            "profile_change_while_active"
        );
    }

    #[test]
    fn stale_observation_is_rejected() {
        let current = observation(
            LimaLifecycleState::Running,
            LimaResourceProfile::Interactive,
            1,
            40_000,
            39_000,
            45_000,
        );
        let next = observation(
            LimaLifecycleState::Running,
            LimaResourceProfile::Interactive,
            1,
            40_100,
            39_000,
            45_000,
        );
        assert_eq!(
            LimaLifecyclePolicy::new(100)
                .expect("policy")
                .plan_transition(&current, next, epoch(40_500))
                .expect_err("stale")
                .code,
            "stale_observation"
        );
    }

    #[test]
    fn generation_and_cache_drift_are_rejected() {
        let current = observation(
            LimaLifecycleState::Running,
            LimaResourceProfile::Work,
            2,
            50_000,
            49_000,
            55_000,
        );

        let generation_drift = observation(
            LimaLifecycleState::Running,
            LimaResourceProfile::Work,
            3,
            50_100,
            49_000,
            55_000,
        );
        assert_eq!(
            policy()
                .plan_transition(&current, generation_drift, epoch(50_100))
                .expect_err("generation drift")
                .code,
            "profile_generation_drift"
        );

        let mut cache_drift_definition = definition(
            LimaLifecycleState::Running,
            LimaResourceProfile::Work,
            2,
            50_100,
            49_000,
            55_000,
        );
        cache_drift_definition.identity = LimaInstanceIdentity::new(
            LimaInstanceId::parse("smolrunner").expect("instance"),
            LimaCacheDiskIdentity::new(
                LimaCacheDiskId::parse("smolrunner-cache").expect("disk"),
                digest("b"),
            ),
        );
        let cache_drift =
            LimaLifecycleObservation::new(cache_drift_definition).expect("observation");
        assert_eq!(
            policy()
                .plan_transition(&current, cache_drift, epoch(50_100))
                .expect_err("cache drift")
                .code,
            "cache_identity_drift"
        );
    }

    #[test]
    fn downscale_requires_completed_drain() {
        let stopped = observation(
            LimaLifecycleState::Stopped,
            LimaResourceProfile::Work,
            7,
            60_000,
            58_000,
            59_000,
        );
        let starting = observation(
            LimaLifecycleState::Starting,
            LimaResourceProfile::Interactive,
            8,
            60_100,
            58_000,
            59_000,
        );
        assert_eq!(
            policy()
                .plan_transition(&stopped, starting, epoch(60_100))
                .expect_err("undrained downscale")
                .code,
            "downscale_without_drain"
        );
    }

    #[test]
    fn stop_before_cooldown_is_rejected() {
        let running = observation(
            LimaLifecycleState::Running,
            LimaResourceProfile::Interactive,
            1,
            70_000,
            69_000,
            71_000,
        );
        let stopping = observation(
            LimaLifecycleState::Stopping,
            LimaResourceProfile::Interactive,
            1,
            70_100,
            69_000,
            71_000,
        );
        assert_eq!(
            policy()
                .plan_transition(&running, stopping, epoch(70_100))
                .expect_err("cooldown")
                .code,
            "stop_before_cooldown"
        );
    }

    #[test]
    fn terminal_state_cannot_reverse() {
        let unavailable = observation(
            LimaLifecycleState::Unavailable,
            LimaResourceProfile::Work,
            1,
            80_000,
            79_000,
            81_000,
        );
        let stopped = observation(
            LimaLifecycleState::Stopped,
            LimaResourceProfile::Work,
            1,
            80_100,
            79_000,
            81_000,
        );
        assert_eq!(
            policy()
                .plan_transition(&unavailable, stopped, epoch(80_100))
                .expect_err("terminal reversal")
                .code,
            "terminal_state_reversal"
        );
    }

    #[test]
    fn mismatched_resources_and_acknowledgement_identity_are_rejected() {
        let mut resources = definition(
            LimaLifecycleState::Running,
            LimaResourceProfile::Interactive,
            1,
            90_000,
            89_000,
            91_000,
        );
        resources.observed_resources =
            LimaObservedResources::new(WORK_VCPUS, WORK_MEMORY_BYTES).expect("resources");
        assert_eq!(
            LimaLifecycleObservation::new(resources)
                .expect_err("resource mismatch")
                .code,
            "profile_resource_mismatch"
        );

        let mut acknowledgement_drift = definition(
            LimaLifecycleState::Stopped,
            LimaResourceProfile::Work,
            2,
            90_000,
            89_000,
            91_000,
        );
        acknowledgement_drift.graceful_stop_acknowledgement =
            Some(GracefulStopAcknowledgement::new(
                epoch(89_500),
                generation(1),
                cache_disk(),
                LimaDrainAcknowledgement::Completed,
            ));
        assert_eq!(
            LimaLifecycleObservation::new(acknowledgement_drift)
                .expect_err("acknowledgement generation")
                .code,
            "acknowledgement_generation_drift"
        );
    }

    #[test]
    fn public_output_is_bounded_and_path_free() {
        assert_eq!(
            LimaInstanceId::parse(PRIVATE_PATH)
                .expect_err("path-shaped identifier")
                .code,
            "invalid_identifier"
        );

        let current = observation(
            LimaLifecycleState::Running,
            LimaResourceProfile::Interactive,
            1,
            100_000,
            99_000,
            105_000,
        );
        let next = observation(
            LimaLifecycleState::Running,
            LimaResourceProfile::Interactive,
            1,
            100_100,
            99_000,
            105_000,
        );
        let transition = policy()
            .plan_transition(&current, next, epoch(100_100))
            .expect("transition");
        let debug = format!("{transition:?}");
        let json = serde_json::to_string(&transition).expect("JSON");
        for output in [debug, json] {
            assert!(!output.contains(PRIVATE_PATH));
            assert!(!output.contains("limactl"));
            assert!(!output.contains("credential"));
            assert!(!output.contains("stdout"));
            assert!(!output.contains("stderr"));
        }
    }
}
