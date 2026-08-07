use std::fmt;

use serde::Serialize;

use crate::mac_availability::{
    ACTIVE_PROFILE, AvailabilityActionKind, AvailabilityDisposition, AvailabilityRequest,
    EffectiveAvailabilityMode, HostPowerSource, JobActivity, MacAvailabilityObservation,
    MacAvailabilityPlan, MacVmProfile, MemoryPressure, ObservationFreshness, VmPowerState,
    plan_availability_transition,
};
use crate::personal_worker_queue::{
    PERSONAL_WORKER_INTERACTIVE_COOLDOWN_MILLIS, PERSONAL_WORKER_STOPPED_COOLDOWN_MILLIS,
    PersonalWorkerJobClass,
};

pub const MAC_AUTO_AVAILABILITY_SCHEMA_VERSION: u8 = 1;
pub const INITIAL_AUTO_MINIMUM_MODE_DWELL_MILLIS: u64 = 5 * 60 * 1_000;
pub const INITIAL_AUTO_MAX_SWAP_GROWTH_BYTES: u64 = 512 * 1_024 * 1_024;
pub const INITIAL_AUTO_BATTERY_FLOOR_PERCENT: u8 = 30;
pub const INITIAL_AUTO_REQUIRED_HEALTHY_OBSERVATIONS: u8 = 3;
pub const INITIAL_AUTO_MAX_LOCAL_QUEUE_DEPTH: u16 = 2;
pub const WORK_PROFILE: MacVmProfile = MacVmProfile {
    cpus: 8,
    memory_mib: 10 * 1024,
    max_concurrent_jobs: 1,
};
const MAX_AUTO_POLICY_MILLIS: u64 = 7 * 24 * 60 * 60 * 1_000;
const MAX_AUTO_QUEUE_DEPTH: u16 = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorActivityState {
    Active,
    Idle,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MacJobTrust {
    Trusted,
    Untrusted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MacQueuedJob {
    pub class: PersonalWorkerJobClass,
    pub trust: MacJobTrust,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MacAdmissionClass {
    None,
    Light,
    Work,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DesiredLimaProfile {
    Stopped,
    Interactive,
    Work,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalDispatchDecision {
    RunLocalNow,
    QueueLocal,
    OverflowRecommended,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MacAutoReasonKind {
    ActiveJob,
    BatteryConservative,
    BatteryFloor,
    CriticalMemoryPressure,
    ElevatedMemoryPressure,
    HealthyObservationDebt,
    IdleDwellIncomplete,
    LocalQueueSaturated,
    ModeDwellIncomplete,
    OperatorActive,
    OperatorHold,
    StaleActivityObservation,
    StaleQueueObservation,
    StaleResourceObservation,
    SwapBaselineUnavailable,
    SwapGrowthExceeded,
    UnknownJobActivity,
    UnknownMemoryPressure,
    UnknownOperatorActivity,
    UnknownPowerSource,
    UntrustedJob,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MacAutoReason {
    pub kind: MacAutoReasonKind,
    pub message: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MacAutoAvailabilityPolicy {
    pub idle_before_work_millis: u64,
    pub idle_before_stop_millis: u64,
    pub minimum_mode_dwell_millis: u64,
    pub max_swap_growth_bytes: u64,
    pub battery_floor_percent: u8,
    pub required_healthy_observations: u8,
    pub max_local_queue_depth: u16,
}

impl MacAutoAvailabilityPolicy {
    /// Construct one bounded automatic Mac admission policy.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for zero or excessive dwell windows, invalid idle ordering,
    /// an impossible battery floor, zero health evidence, or an unsupported local queue cap.
    pub fn new(
        idle_before_work_millis: u64,
        idle_before_stop_millis: u64,
        minimum_mode_dwell_millis: u64,
        max_swap_growth_bytes: u64,
        battery_floor_percent: u8,
        required_healthy_observations: u8,
        max_local_queue_depth: u16,
    ) -> Result<Self, MacAutoAvailabilityError> {
        if idle_before_work_millis == 0
            || idle_before_stop_millis == 0
            || minimum_mode_dwell_millis == 0
            || idle_before_work_millis > MAX_AUTO_POLICY_MILLIS
            || idle_before_stop_millis > MAX_AUTO_POLICY_MILLIS
            || minimum_mode_dwell_millis > MAX_AUTO_POLICY_MILLIS
        {
            return Err(MacAutoAvailabilityError::new(
                "policy",
                "invalid_time_window",
                "automatic availability time windows must remain within the bounded positive range",
            ));
        }
        if idle_before_stop_millis <= idle_before_work_millis {
            return Err(MacAutoAvailabilityError::new(
                "policy.idle_before_stop_millis",
                "invalid_idle_order",
                "the stopped idle threshold must be later than the work idle threshold",
            ));
        }
        if battery_floor_percent > 100 {
            return Err(MacAutoAvailabilityError::new(
                "policy.battery_floor_percent",
                "invalid_battery_floor",
                "battery floor must be a percentage from zero through one hundred",
            ));
        }
        if required_healthy_observations == 0 {
            return Err(MacAutoAvailabilityError::new(
                "policy.required_healthy_observations",
                "invalid_health_streak",
                "automatic work admission requires at least one healthy observation",
            ));
        }
        if max_local_queue_depth == 0 || max_local_queue_depth > MAX_AUTO_QUEUE_DEPTH {
            return Err(MacAutoAvailabilityError::new(
                "policy.max_local_queue_depth",
                "invalid_local_queue_depth",
                "local queue depth must remain within the bounded positive queue range",
            ));
        }
        Ok(Self {
            idle_before_work_millis,
            idle_before_stop_millis,
            minimum_mode_dwell_millis,
            max_swap_growth_bytes,
            battery_floor_percent,
            required_healthy_observations,
            max_local_queue_depth,
        })
    }

    /// Initial conservative policy for the compute-relief lane.
    ///
    /// The values are explicit hypotheses for physical Mac measurement, not correctness limits.
    #[must_use]
    pub fn initial() -> Self {
        Self::new(
            PERSONAL_WORKER_INTERACTIVE_COOLDOWN_MILLIS,
            PERSONAL_WORKER_STOPPED_COOLDOWN_MILLIS,
            INITIAL_AUTO_MINIMUM_MODE_DWELL_MILLIS,
            INITIAL_AUTO_MAX_SWAP_GROWTH_BYTES,
            INITIAL_AUTO_BATTERY_FLOOR_PERCENT,
            INITIAL_AUTO_REQUIRED_HEALTHY_OBSERVATIONS,
            INITIAL_AUTO_MAX_LOCAL_QUEUE_DEPTH,
        )
        .expect("initial automatic Mac policy is valid")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacAutoAvailabilityObservation {
    pub requested_mode: AvailabilityRequest,
    pub effective_mode: EffectiveAvailabilityMode,
    pub vm_power: VmPowerState,
    pub job_activity: JobActivity,
    pub resource_freshness: ObservationFreshness,
    pub activity_freshness: ObservationFreshness,
    pub queue_freshness: ObservationFreshness,
    pub host_power: HostPowerSource,
    pub battery_percent: Option<u8>,
    pub memory_pressure: MemoryPressure,
    pub swap_used_bytes: u64,
    pub previous_swap_used_bytes: Option<u64>,
    pub operator_activity: OperatorActivityState,
    pub operator_idle_millis: Option<u64>,
    pub operator_hold: bool,
    pub queued_jobs: u16,
    pub next_job: Option<MacQueuedJob>,
    pub healthy_observation_streak: u8,
    pub last_transition_at_millis: u64,
    pub decision_at_millis: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MacAutoAvailabilityPlan {
    pub schema_version: u8,
    pub requested_mode: AvailabilityRequest,
    pub effective_mode: EffectiveAvailabilityMode,
    pub resolved_mode: EffectiveAvailabilityMode,
    pub desired_profile: DesiredLimaProfile,
    pub desired_vm_profile: Option<MacVmProfile>,
    pub admission: MacAdmissionClass,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dispatch: Option<LocalDispatchDecision>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_action: Option<AvailabilityActionKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub swap_growth_bytes: Option<u64>,
    pub transition: MacAvailabilityPlan,
    pub reasons: Vec<MacAutoReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MacAutoAvailabilityError {
    pub field: &'static str,
    pub code: &'static str,
    pub message: &'static str,
}

impl MacAutoAvailabilityError {
    const fn new(field: &'static str, code: &'static str, message: &'static str) -> Self {
        Self {
            field,
            code,
            message,
        }
    }
}

impl fmt::Display for MacAutoAvailabilityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for MacAutoAvailabilityError {}

/// Reduce bounded operator, resource, queue, and current-mode evidence into one local compute plan.
///
/// This function reads no clock, process, filesystem, application list, browser state, network,
/// credential, queue store, or Lima state. It performs no mutation. The caller owns every
/// observation and timestamp.
///
/// # Errors
///
/// Returns a bounded error for time reversal, inconsistent queue summary, impossible battery
/// percentage, or incomplete idle evidence.
pub fn plan_mac_auto_availability(
    policy: MacAutoAvailabilityPolicy,
    observation: MacAutoAvailabilityObservation,
) -> Result<MacAutoAvailabilityPlan, MacAutoAvailabilityError> {
    validate_observation(observation)?;

    let swap_growth_bytes = observation
        .previous_swap_used_bytes
        .map(|previous| observation.swap_used_bytes.saturating_sub(previous));
    let mut reasons = Vec::new();
    classify_common_reasons(policy, observation, swap_growth_bytes, &mut reasons);

    let resolved_mode = match observation.requested_mode {
        AvailabilityRequest::Active => EffectiveAvailabilityMode::Active,
        AvailabilityRequest::Away => EffectiveAvailabilityMode::Away,
        AvailabilityRequest::Off => EffectiveAvailabilityMode::Off,
        AvailabilityRequest::Auto => {
            resolve_auto_mode(policy, observation, swap_growth_bytes, &mut reasons)
        }
    };

    let explicit_request = request_for_effective_mode(resolved_mode);
    let mut transition = plan_availability_transition(
        MacAvailabilityObservation {
            effective_mode: observation.effective_mode,
            vm_power: observation.vm_power,
            job_activity: observation.job_activity,
            freshness: observation.resource_freshness,
            host_power: observation.host_power,
            memory_pressure: observation.memory_pressure,
            operator_hold: observation.operator_hold,
        },
        explicit_request,
    );
    transition.target_profile = vm_profile(resolved_mode);

    let admission = desired_admission(policy, observation, resolved_mode, swap_growth_bytes);
    let dispatch = observation.next_job.map(|job| {
        decide_dispatch(
            policy,
            observation,
            resolved_mode,
            admission,
            &transition,
            job,
            &mut reasons,
        )
    });
    let next_action = if matches!(
        transition.disposition,
        AvailabilityDisposition::Ready | AvailabilityDisposition::NoChange
    ) {
        transition.actions.first().map(|action| action.kind)
    } else {
        None
    };

    Ok(MacAutoAvailabilityPlan {
        schema_version: MAC_AUTO_AVAILABILITY_SCHEMA_VERSION,
        requested_mode: observation.requested_mode,
        effective_mode: observation.effective_mode,
        resolved_mode,
        desired_profile: desired_profile(resolved_mode),
        desired_vm_profile: vm_profile(resolved_mode),
        admission,
        dispatch,
        next_action,
        swap_growth_bytes,
        transition,
        reasons,
    })
}

#[must_use]
pub fn render_human(plan: &MacAutoAvailabilityPlan) -> String {
    let mut output = format!(
        "Mac compute policy: {:?} -> {:?}\nDesired profile: {:?}\nAdmission: {:?}\n",
        plan.effective_mode, plan.resolved_mode, plan.desired_profile, plan.admission
    );
    if let Some(dispatch) = plan.dispatch {
        output.push_str(&format!("Dispatch: {dispatch:?}\n"));
    }
    if let Some(action) = plan.next_action {
        output.push_str(&format!("Next action: {action:?}\n"));
    }
    if !plan.reasons.is_empty() {
        output.push_str("Reasons:\n");
        for reason in &plan.reasons {
            output.push_str(&format!("- {}\n", reason.message));
        }
    }
    output
}

fn validate_observation(
    observation: MacAutoAvailabilityObservation,
) -> Result<(), MacAutoAvailabilityError> {
    if observation.decision_at_millis < observation.last_transition_at_millis {
        return Err(MacAutoAvailabilityError::new(
            "decision_at_millis",
            "transition_time_reversal",
            "decision time cannot precede the last effective-mode transition",
        ));
    }
    if observation.battery_percent.is_some_and(|percent| percent > 100) {
        return Err(MacAutoAvailabilityError::new(
            "battery_percent",
            "invalid_battery_percent",
            "battery percentage must be from zero through one hundred",
        ));
    }
    if observation.queued_jobs > MAX_AUTO_QUEUE_DEPTH {
        return Err(MacAutoAvailabilityError::new(
            "queued_jobs",
            "queue_depth_exceeded",
            "automatic availability queue depth exceeds the bounded personal-worker queue",
        ));
    }
    if (observation.queued_jobs == 0) != observation.next_job.is_none() {
        return Err(MacAutoAvailabilityError::new(
            "next_job",
            "inconsistent_queue_summary",
            "next queued job presence must agree with the bounded queue depth summary",
        ));
    }
    match observation.operator_activity {
        OperatorActivityState::Idle if observation.operator_idle_millis.is_none() => {
            return Err(MacAutoAvailabilityError::new(
                "operator_idle_millis",
                "missing_idle_duration",
                "idle operator evidence requires an exact bounded idle duration",
            ));
        }
        OperatorActivityState::Active
            if observation.operator_idle_millis.is_some_and(|idle| idle > 0) =>
        {
            return Err(MacAutoAvailabilityError::new(
                "operator_idle_millis",
                "active_idle_conflict",
                "active operator evidence cannot carry a positive idle duration",
            ));
        }
        OperatorActivityState::Active
        | OperatorActivityState::Idle
        | OperatorActivityState::Unknown => {}
    }
    Ok(())
}

fn classify_common_reasons(
    policy: MacAutoAvailabilityPolicy,
    observation: MacAutoAvailabilityObservation,
    swap_growth_bytes: Option<u64>,
    reasons: &mut Vec<MacAutoReason>,
) {
    if observation.operator_hold {
        push_reason(
            reasons,
            MacAutoReasonKind::OperatorHold,
            "operator hold disables new local admission and automatic profile movement",
        );
    }
    if observation.resource_freshness == ObservationFreshness::Stale {
        push_reason(
            reasons,
            MacAutoReasonKind::StaleResourceObservation,
            "resource evidence is stale",
        );
    }
    if observation.activity_freshness == ObservationFreshness::Stale {
        push_reason(
            reasons,
            MacAutoReasonKind::StaleActivityObservation,
            "operator activity evidence is stale",
        );
    }
    if observation.queue_freshness == ObservationFreshness::Stale {
        push_reason(
            reasons,
            MacAutoReasonKind::StaleQueueObservation,
            "queue evidence is stale",
        );
    }
    match observation.memory_pressure {
        MemoryPressure::Normal => {}
        MemoryPressure::Elevated => push_reason(
            reasons,
            MacAutoReasonKind::ElevatedMemoryPressure,
            "elevated macOS memory pressure suppresses new local work",
        ),
        MemoryPressure::Critical => push_reason(
            reasons,
            MacAutoReasonKind::CriticalMemoryPressure,
            "critical macOS memory pressure requests a drained local stop",
        ),
        MemoryPressure::Unknown => push_reason(
            reasons,
            MacAutoReasonKind::UnknownMemoryPressure,
            "memory pressure is unknown",
        ),
    }
    match observation.host_power {
        HostPowerSource::Ac => {}
        HostPowerSource::Battery => push_reason(
            reasons,
            MacAutoReasonKind::BatteryConservative,
            "battery power suppresses automatic work-profile admission",
        ),
        HostPowerSource::Unknown => push_reason(
            reasons,
            MacAutoReasonKind::UnknownPowerSource,
            "host power source is unknown",
        ),
    }
    if observation.host_power == HostPowerSource::Battery
        && observation
            .battery_percent
            .is_some_and(|percent| percent <= policy.battery_floor_percent)
    {
        push_reason(
            reasons,
            MacAutoReasonKind::BatteryFloor,
            "battery floor requests a drained local stop",
        );
    }
    match observation.operator_activity {
        OperatorActivityState::Active => push_reason(
            reasons,
            MacAutoReasonKind::OperatorActive,
            "operator activity keeps automatic compute in the interactive envelope",
        ),
        OperatorActivityState::Idle => {}
        OperatorActivityState::Unknown => push_reason(
            reasons,
            MacAutoReasonKind::UnknownOperatorActivity,
            "operator activity is unknown",
        ),
    }
    match observation.job_activity {
        JobActivity::Idle => {}
        JobActivity::Active => push_reason(
            reasons,
            MacAutoReasonKind::ActiveJob,
            "an active job drains before any profile reduction",
        ),
        JobActivity::Unknown => push_reason(
            reasons,
            MacAutoReasonKind::UnknownJobActivity,
            "runner job activity is unknown",
        ),
    }
    match swap_growth_bytes {
        Some(growth) if growth > policy.max_swap_growth_bytes => push_reason(
            reasons,
            MacAutoReasonKind::SwapGrowthExceeded,
            "recent swap growth exceeds the local admission policy",
        ),
        Some(_) => {}
        None => push_reason(
            reasons,
            MacAutoReasonKind::SwapBaselineUnavailable,
            "swap growth cannot be classified without a comparison sample",
        ),
    }
}

fn resolve_auto_mode(
    policy: MacAutoAvailabilityPolicy,
    observation: MacAutoAvailabilityObservation,
    swap_growth_bytes: Option<u64>,
    reasons: &mut Vec<MacAutoReason>,
) -> EffectiveAvailabilityMode {
    if observation.operator_hold || essential_auto_evidence_missing(observation) {
        return observation.effective_mode;
    }

    if observation.memory_pressure == MemoryPressure::Critical {
        return EffectiveAvailabilityMode::Off;
    }

    if observation.host_power == HostPowerSource::Battery {
        if observation
            .battery_percent
            .is_some_and(|percent| percent <= policy.battery_floor_percent)
        {
            return EffectiveAvailabilityMode::Off;
        }
        return EffectiveAvailabilityMode::Active;
    }

    if observation.operator_activity == OperatorActivityState::Active {
        return EffectiveAvailabilityMode::Active;
    }

    let idle_millis = observation.operator_idle_millis.unwrap_or(0);
    if observation.queued_jobs == 0 && idle_millis >= policy.idle_before_stop_millis {
        return EffectiveAvailabilityMode::Off;
    }

    if observation.queued_jobs > 0 {
        if idle_millis < policy.idle_before_work_millis {
            push_reason(
                reasons,
                MacAutoReasonKind::IdleDwellIncomplete,
                "operator idle duration has not reached the automatic work threshold",
            );
            return EffectiveAvailabilityMode::Active;
        }
        let mode_age = observation
            .decision_at_millis
            .saturating_sub(observation.last_transition_at_millis);
        if observation.effective_mode != EffectiveAvailabilityMode::Away
            && mode_age < policy.minimum_mode_dwell_millis
        {
            push_reason(
                reasons,
                MacAutoReasonKind::ModeDwellIncomplete,
                "minimum effective-mode dwell has not elapsed before work-profile entry",
            );
            return EffectiveAvailabilityMode::Active;
        }
        if observation.healthy_observation_streak < policy.required_healthy_observations {
            push_reason(
                reasons,
                MacAutoReasonKind::HealthyObservationDebt,
                "additional consecutive healthy observations are required before heavy local admission",
            );
            return EffectiveAvailabilityMode::Active;
        }
        if observation.memory_pressure != MemoryPressure::Normal
            || swap_growth_bytes.is_none_or(|growth| growth > policy.max_swap_growth_bytes)
        {
            return EffectiveAvailabilityMode::Active;
        }
        return EffectiveAvailabilityMode::Away;
    }

    EffectiveAvailabilityMode::Active
}

fn essential_auto_evidence_missing(observation: MacAutoAvailabilityObservation) -> bool {
    observation.resource_freshness == ObservationFreshness::Stale
        || observation.activity_freshness == ObservationFreshness::Stale
        || observation.queue_freshness == ObservationFreshness::Stale
        || observation.operator_activity == OperatorActivityState::Unknown
        || observation.memory_pressure == MemoryPressure::Unknown
        || observation.host_power == HostPowerSource::Unknown
        || observation.job_activity == JobActivity::Unknown
}

fn desired_admission(
    policy: MacAutoAvailabilityPolicy,
    observation: MacAutoAvailabilityObservation,
    resolved_mode: EffectiveAvailabilityMode,
    swap_growth_bytes: Option<u64>,
) -> MacAdmissionClass {
    if observation.operator_hold
        || observation.resource_freshness == ObservationFreshness::Stale
        || observation.activity_freshness == ObservationFreshness::Stale
        || observation.queue_freshness == ObservationFreshness::Stale
        || observation.job_activity == JobActivity::Unknown
        || observation.memory_pressure != MemoryPressure::Normal
        || swap_growth_bytes.is_none_or(|growth| growth > policy.max_swap_growth_bytes)
    {
        return MacAdmissionClass::None;
    }

    match resolved_mode {
        EffectiveAvailabilityMode::Off => MacAdmissionClass::None,
        EffectiveAvailabilityMode::Active => MacAdmissionClass::Light,
        EffectiveAvailabilityMode::Away => {
            if observation.host_power == HostPowerSource::Ac {
                MacAdmissionClass::Work
            } else {
                MacAdmissionClass::None
            }
        }
    }
}

fn decide_dispatch(
    policy: MacAutoAvailabilityPolicy,
    observation: MacAutoAvailabilityObservation,
    resolved_mode: EffectiveAvailabilityMode,
    admission: MacAdmissionClass,
    transition: &MacAvailabilityPlan,
    job: MacQueuedJob,
    reasons: &mut Vec<MacAutoReason>,
) -> LocalDispatchDecision {
    if job.trust == MacJobTrust::Untrusted {
        push_reason(
            reasons,
            MacAutoReasonKind::UntrustedJob,
            "untrusted work is excluded from the personal Mac worker",
        );
        return LocalDispatchDecision::OverflowRecommended;
    }

    if observation.queued_jobs > policy.max_local_queue_depth {
        push_reason(
            reasons,
            MacAutoReasonKind::LocalQueueSaturated,
            "local queue depth exceeds the preferred opportunistic backlog",
        );
        return LocalDispatchDecision::OverflowRecommended;
    }

    if observation.operator_hold || essential_dispatch_evidence_missing(observation) {
        return LocalDispatchDecision::OverflowRecommended;
    }

    if observation.job_activity == JobActivity::Active {
        return LocalDispatchDecision::QueueLocal;
    }

    if transition.disposition == AvailabilityDisposition::Blocked {
        return LocalDispatchDecision::OverflowRecommended;
    }

    let current_admission = current_admission(policy, observation);
    if transition.disposition == AvailabilityDisposition::NoChange
        && admission_allows(current_admission, job.class)
    {
        return LocalDispatchDecision::RunLocalNow;
    }

    if transition.disposition == AvailabilityDisposition::Ready
        && admission_allows(admission, job.class)
    {
        return LocalDispatchDecision::QueueLocal;
    }

    if observation.requested_mode == AvailabilityRequest::Auto
        && resolved_mode == EffectiveAvailabilityMode::Active
        && observation.operator_activity == OperatorActivityState::Idle
        && observation.host_power == HostPowerSource::Ac
        && observation.memory_pressure == MemoryPressure::Normal
        && observation
            .operator_idle_millis
            .is_some_and(|idle| idle < policy.idle_before_work_millis)
    {
        return LocalDispatchDecision::QueueLocal;
    }

    LocalDispatchDecision::OverflowRecommended
}

fn essential_dispatch_evidence_missing(observation: MacAutoAvailabilityObservation) -> bool {
    observation.resource_freshness == ObservationFreshness::Stale
        || observation.activity_freshness == ObservationFreshness::Stale
        || observation.queue_freshness == ObservationFreshness::Stale
        || observation.job_activity == JobActivity::Unknown
}

fn current_admission(
    policy: MacAutoAvailabilityPolicy,
    observation: MacAutoAvailabilityObservation,
) -> MacAdmissionClass {
    if observation.memory_pressure != MemoryPressure::Normal
        || observation.operator_hold
        || observation.resource_freshness == ObservationFreshness::Stale
        || observation
            .previous_swap_used_bytes
            .map(|previous| observation.swap_used_bytes.saturating_sub(previous))
            .is_none_or(|growth| growth > policy.max_swap_growth_bytes)
    {
        return MacAdmissionClass::None;
    }
    match observation.effective_mode {
        EffectiveAvailabilityMode::Off => MacAdmissionClass::None,
        EffectiveAvailabilityMode::Active => MacAdmissionClass::Light,
        EffectiveAvailabilityMode::Away => {
            if observation.host_power == HostPowerSource::Ac {
                MacAdmissionClass::Work
            } else {
                MacAdmissionClass::None
            }
        }
    }
}

const fn admission_allows(admission: MacAdmissionClass, job_class: PersonalWorkerJobClass) -> bool {
    matches!(
        (admission, job_class),
        (MacAdmissionClass::Light, PersonalWorkerJobClass::Light)
            | (MacAdmissionClass::Work, PersonalWorkerJobClass::Light)
            | (MacAdmissionClass::Work, PersonalWorkerJobClass::Heavy)
    )
}

const fn request_for_effective_mode(mode: EffectiveAvailabilityMode) -> AvailabilityRequest {
    match mode {
        EffectiveAvailabilityMode::Active => AvailabilityRequest::Active,
        EffectiveAvailabilityMode::Away => AvailabilityRequest::Away,
        EffectiveAvailabilityMode::Off => AvailabilityRequest::Off,
    }
}

const fn desired_profile(mode: EffectiveAvailabilityMode) -> DesiredLimaProfile {
    match mode {
        EffectiveAvailabilityMode::Active => DesiredLimaProfile::Interactive,
        EffectiveAvailabilityMode::Away => DesiredLimaProfile::Work,
        EffectiveAvailabilityMode::Off => DesiredLimaProfile::Stopped,
    }
}

const fn vm_profile(mode: EffectiveAvailabilityMode) -> Option<MacVmProfile> {
    match mode {
        EffectiveAvailabilityMode::Active => Some(ACTIVE_PROFILE),
        EffectiveAvailabilityMode::Away => Some(WORK_PROFILE),
        EffectiveAvailabilityMode::Off => None,
    }
}

fn push_reason(reasons: &mut Vec<MacAutoReason>, kind: MacAutoReasonKind, message: &'static str) {
    if reasons.iter().all(|reason| reason.kind != kind) {
        reasons.push(MacAutoReason { kind, message });
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DesiredLimaProfile, LocalDispatchDecision, MacAdmissionClass, MacAutoAvailabilityObservation,
        MacAutoAvailabilityPolicy, MacJobTrust, MacQueuedJob, OperatorActivityState,
        plan_mac_auto_availability,
    };
    use crate::mac_availability::{
        AvailabilityActionKind, AvailabilityDisposition, AvailabilityRequest,
        EffectiveAvailabilityMode, HostPowerSource, JobActivity, MemoryPressure,
        ObservationFreshness, VmPowerState,
    };
    use crate::personal_worker_queue::PersonalWorkerJobClass;

    const MINUTE: u64 = 60 * 1_000;
    const GIB: u64 = 1_024 * 1_024 * 1_024;

    fn policy() -> MacAutoAvailabilityPolicy {
        MacAutoAvailabilityPolicy::new(10 * MINUTE, 30 * MINUTE, 5 * MINUTE, GIB / 2, 30, 3, 2)
            .expect("test policy")
    }

    fn observation(effective_mode: EffectiveAvailabilityMode) -> MacAutoAvailabilityObservation {
        MacAutoAvailabilityObservation {
            requested_mode: AvailabilityRequest::Auto,
            effective_mode,
            vm_power: if effective_mode == EffectiveAvailabilityMode::Off {
                VmPowerState::Stopped
            } else {
                VmPowerState::Running
            },
            job_activity: JobActivity::Idle,
            resource_freshness: ObservationFreshness::Fresh,
            activity_freshness: ObservationFreshness::Fresh,
            queue_freshness: ObservationFreshness::Fresh,
            host_power: HostPowerSource::Ac,
            battery_percent: Some(100),
            memory_pressure: MemoryPressure::Normal,
            swap_used_bytes: GIB,
            previous_swap_used_bytes: Some(GIB),
            operator_activity: OperatorActivityState::Active,
            operator_idle_millis: Some(0),
            operator_hold: false,
            queued_jobs: 1,
            next_job: Some(MacQueuedJob {
                class: PersonalWorkerJobClass::Light,
                trust: MacJobTrust::Trusted,
            }),
            healthy_observation_streak: 3,
            last_transition_at_millis: 10 * MINUTE,
            decision_at_millis: 20 * MINUTE,
        }
    }

    #[test]
    fn active_operator_keeps_interactive_and_runs_one_light_job() {
        let plan = plan_mac_auto_availability(policy(), observation(EffectiveAvailabilityMode::Active))
            .expect("plan");

        assert_eq!(plan.resolved_mode, EffectiveAvailabilityMode::Active);
        assert_eq!(plan.desired_profile, DesiredLimaProfile::Interactive);
        assert_eq!(plan.admission, MacAdmissionClass::Light);
        assert_eq!(plan.dispatch, Some(LocalDispatchDecision::RunLocalNow));
        assert_eq!(plan.transition.disposition, AvailabilityDisposition::NoChange);
        assert_eq!(plan.desired_vm_profile.expect("profile").memory_mib, 3 * 1024);
    }

    #[test]
    fn sustained_idle_on_ac_enters_work_and_queues_until_transition() {
        let mut facts = observation(EffectiveAvailabilityMode::Active);
        facts.operator_activity = OperatorActivityState::Idle;
        facts.operator_idle_millis = Some(12 * MINUTE);
        facts.next_job = Some(MacQueuedJob {
            class: PersonalWorkerJobClass::Heavy,
            trust: MacJobTrust::Trusted,
        });

        let plan = plan_mac_auto_availability(policy(), facts).expect("plan");

        assert_eq!(plan.resolved_mode, EffectiveAvailabilityMode::Away);
        assert_eq!(plan.desired_profile, DesiredLimaProfile::Work);
        assert_eq!(plan.admission, MacAdmissionClass::Work);
        assert_eq!(plan.dispatch, Some(LocalDispatchDecision::QueueLocal));
        assert_eq!(plan.next_action, Some(AvailabilityActionKind::DrainRunner));
        assert_eq!(plan.desired_vm_profile.expect("profile").memory_mib, 10 * 1024);
        assert_eq!(plan.transition.target_profile.expect("profile").memory_mib, 10 * 1024);
    }

    #[test]
    fn idle_work_profile_runs_heavy_job_immediately() {
        let mut facts = observation(EffectiveAvailabilityMode::Away);
        facts.operator_activity = OperatorActivityState::Idle;
        facts.operator_idle_millis = Some(20 * MINUTE);
        facts.next_job = Some(MacQueuedJob {
            class: PersonalWorkerJobClass::Heavy,
            trust: MacJobTrust::Trusted,
        });

        let plan = plan_mac_auto_availability(policy(), facts).expect("plan");

        assert_eq!(plan.resolved_mode, EffectiveAvailabilityMode::Away);
        assert_eq!(plan.admission, MacAdmissionClass::Work);
        assert_eq!(plan.dispatch, Some(LocalDispatchDecision::RunLocalNow));
    }

    #[test]
    fn resumed_activity_during_heavy_job_blocks_new_heavy_admission() {
        let mut facts = observation(EffectiveAvailabilityMode::Away);
        facts.job_activity = JobActivity::Active;
        facts.next_job = Some(MacQueuedJob {
            class: PersonalWorkerJobClass::Heavy,
            trust: MacJobTrust::Trusted,
        });

        let plan = plan_mac_auto_availability(policy(), facts).expect("plan");

        assert_eq!(plan.resolved_mode, EffectiveAvailabilityMode::Active);
        assert_eq!(plan.admission, MacAdmissionClass::Light);
        assert_eq!(plan.dispatch, Some(LocalDispatchDecision::QueueLocal));
        assert_eq!(plan.transition.disposition, AvailabilityDisposition::Blocked);
        assert!(plan.next_action.is_none());
    }

    #[test]
    fn critical_memory_pressure_targets_stopped_after_drain() {
        let mut facts = observation(EffectiveAvailabilityMode::Away);
        facts.memory_pressure = MemoryPressure::Critical;
        facts.operator_activity = OperatorActivityState::Idle;
        facts.operator_idle_millis = Some(20 * MINUTE);

        let plan = plan_mac_auto_availability(policy(), facts).expect("plan");

        assert_eq!(plan.resolved_mode, EffectiveAvailabilityMode::Off);
        assert_eq!(plan.desired_profile, DesiredLimaProfile::Stopped);
        assert_eq!(plan.admission, MacAdmissionClass::None);
        assert_eq!(plan.next_action, Some(AvailabilityActionKind::DrainRunner));
    }

    #[test]
    fn elevated_memory_pressure_suppresses_new_local_jobs() {
        let mut facts = observation(EffectiveAvailabilityMode::Active);
        facts.memory_pressure = MemoryPressure::Elevated;

        let plan = plan_mac_auto_availability(policy(), facts).expect("plan");

        assert_eq!(plan.resolved_mode, EffectiveAvailabilityMode::Active);
        assert_eq!(plan.admission, MacAdmissionClass::None);
        assert_eq!(plan.dispatch, Some(LocalDispatchDecision::OverflowRecommended));
    }

    #[test]
    fn swap_growth_prevents_work_upscale() {
        let mut facts = observation(EffectiveAvailabilityMode::Active);
        facts.operator_activity = OperatorActivityState::Idle;
        facts.operator_idle_millis = Some(20 * MINUTE);
        facts.swap_used_bytes = 2 * GIB;
        facts.previous_swap_used_bytes = Some(GIB);
        facts.next_job = Some(MacQueuedJob {
            class: PersonalWorkerJobClass::Heavy,
            trust: MacJobTrust::Trusted,
        });

        let plan = plan_mac_auto_availability(policy(), facts).expect("plan");

        assert_eq!(plan.resolved_mode, EffectiveAvailabilityMode::Active);
        assert_eq!(plan.admission, MacAdmissionClass::None);
        assert_eq!(plan.dispatch, Some(LocalDispatchDecision::OverflowRecommended));
    }

    #[test]
    fn battery_never_auto_upscales_and_floor_targets_off() {
        let mut facts = observation(EffectiveAvailabilityMode::Active);
        facts.host_power = HostPowerSource::Battery;
        facts.battery_percent = Some(80);
        facts.operator_activity = OperatorActivityState::Idle;
        facts.operator_idle_millis = Some(20 * MINUTE);
        facts.next_job = Some(MacQueuedJob {
            class: PersonalWorkerJobClass::Heavy,
            trust: MacJobTrust::Trusted,
        });

        let plan = plan_mac_auto_availability(policy(), facts).expect("plan");
        assert_eq!(plan.resolved_mode, EffectiveAvailabilityMode::Active);

        facts.battery_percent = Some(20);
        let floor_plan = plan_mac_auto_availability(policy(), facts).expect("floor plan");
        assert_eq!(floor_plan.resolved_mode, EffectiveAvailabilityMode::Off);
    }

    #[test]
    fn long_idle_empty_queue_targets_stopped() {
        let mut facts = observation(EffectiveAvailabilityMode::Active);
        facts.operator_activity = OperatorActivityState::Idle;
        facts.operator_idle_millis = Some(35 * MINUTE);
        facts.queued_jobs = 0;
        facts.next_job = None;

        let plan = plan_mac_auto_availability(policy(), facts).expect("plan");

        assert_eq!(plan.resolved_mode, EffectiveAvailabilityMode::Off);
        assert_eq!(plan.desired_profile, DesiredLimaProfile::Stopped);
    }

    #[test]
    fn work_entry_waits_for_idle_and_mode_dwell() {
        let mut facts = observation(EffectiveAvailabilityMode::Active);
        facts.operator_activity = OperatorActivityState::Idle;
        facts.operator_idle_millis = Some(8 * MINUTE);
        facts.next_job = Some(MacQueuedJob {
            class: PersonalWorkerJobClass::Heavy,
            trust: MacJobTrust::Trusted,
        });

        let idle_plan = plan_mac_auto_availability(policy(), facts).expect("idle plan");
        assert_eq!(idle_plan.resolved_mode, EffectiveAvailabilityMode::Active);
        assert_eq!(idle_plan.dispatch, Some(LocalDispatchDecision::QueueLocal));

        facts.operator_idle_millis = Some(12 * MINUTE);
        facts.last_transition_at_millis = 18 * MINUTE;
        facts.decision_at_millis = 20 * MINUTE;
        let dwell_plan = plan_mac_auto_availability(policy(), facts).expect("dwell plan");
        assert_eq!(dwell_plan.resolved_mode, EffectiveAvailabilityMode::Active);
    }

    #[test]
    fn healthy_streak_is_required_before_reenabling_work() {
        let mut facts = observation(EffectiveAvailabilityMode::Active);
        facts.operator_activity = OperatorActivityState::Idle;
        facts.operator_idle_millis = Some(20 * MINUTE);
        facts.healthy_observation_streak = 2;
        facts.next_job = Some(MacQueuedJob {
            class: PersonalWorkerJobClass::Heavy,
            trust: MacJobTrust::Trusted,
        });

        let plan = plan_mac_auto_availability(policy(), facts).expect("plan");

        assert_eq!(plan.resolved_mode, EffectiveAvailabilityMode::Active);
        assert_eq!(plan.dispatch, Some(LocalDispatchDecision::OverflowRecommended));
    }

    #[test]
    fn stale_activity_evidence_fails_closed_to_current_mode_and_overflow() {
        let mut facts = observation(EffectiveAvailabilityMode::Active);
        facts.activity_freshness = ObservationFreshness::Stale;
        facts.operator_activity = OperatorActivityState::Idle;
        facts.operator_idle_millis = Some(20 * MINUTE);
        facts.next_job = Some(MacQueuedJob {
            class: PersonalWorkerJobClass::Heavy,
            trust: MacJobTrust::Trusted,
        });

        let plan = plan_mac_auto_availability(policy(), facts).expect("plan");

        assert_eq!(plan.resolved_mode, EffectiveAvailabilityMode::Active);
        assert_eq!(plan.admission, MacAdmissionClass::None);
        assert_eq!(plan.dispatch, Some(LocalDispatchDecision::OverflowRecommended));
    }

    #[test]
    fn explicit_away_overrides_activity_but_keeps_resource_vetoes() {
        let mut facts = observation(EffectiveAvailabilityMode::Active);
        facts.requested_mode = AvailabilityRequest::Away;
        facts.next_job = Some(MacQueuedJob {
            class: PersonalWorkerJobClass::Heavy,
            trust: MacJobTrust::Trusted,
        });

        let plan = plan_mac_auto_availability(policy(), facts).expect("plan");
        assert_eq!(plan.resolved_mode, EffectiveAvailabilityMode::Away);
        assert_eq!(plan.transition.disposition, AvailabilityDisposition::Ready);

        facts.memory_pressure = MemoryPressure::Elevated;
        let blocked = plan_mac_auto_availability(policy(), facts).expect("blocked plan");
        assert_eq!(blocked.resolved_mode, EffectiveAvailabilityMode::Away);
        assert_eq!(blocked.transition.disposition, AvailabilityDisposition::Blocked);
        assert_eq!(blocked.admission, MacAdmissionClass::None);
    }

    #[test]
    fn operator_hold_disables_local_admission_without_inventing_a_transition() {
        let mut facts = observation(EffectiveAvailabilityMode::Active);
        facts.operator_hold = true;

        let plan = plan_mac_auto_availability(policy(), facts).expect("plan");

        assert_eq!(plan.resolved_mode, EffectiveAvailabilityMode::Active);
        assert_eq!(plan.admission, MacAdmissionClass::None);
        assert_eq!(plan.dispatch, Some(LocalDispatchDecision::OverflowRecommended));
        assert!(plan.next_action.is_none());
    }

    #[test]
    fn untrusted_job_is_never_selected_for_personal_mac_execution() {
        let mut facts = observation(EffectiveAvailabilityMode::Active);
        facts.next_job = Some(MacQueuedJob {
            class: PersonalWorkerJobClass::Light,
            trust: MacJobTrust::Untrusted,
        });

        let plan = plan_mac_auto_availability(policy(), facts).expect("plan");

        assert_eq!(plan.dispatch, Some(LocalDispatchDecision::OverflowRecommended));
    }

    #[test]
    fn queue_saturation_recommends_overflow() {
        let mut facts = observation(EffectiveAvailabilityMode::Away);
        facts.operator_activity = OperatorActivityState::Idle;
        facts.operator_idle_millis = Some(20 * MINUTE);
        facts.queued_jobs = 3;
        facts.next_job = Some(MacQueuedJob {
            class: PersonalWorkerJobClass::Heavy,
            trust: MacJobTrust::Trusted,
        });

        let plan = plan_mac_auto_availability(policy(), facts).expect("plan");

        assert_eq!(plan.dispatch, Some(LocalDispatchDecision::OverflowRecommended));
    }

    #[test]
    fn invalid_queue_summary_is_rejected() {
        let mut facts = observation(EffectiveAvailabilityMode::Active);
        facts.queued_jobs = 0;

        let error = plan_mac_auto_availability(policy(), facts).expect_err("invalid summary");
        assert_eq!(error.code, "inconsistent_queue_summary");
    }

    #[test]
    fn json_contract_is_bounded_and_contains_no_application_identity() {
        let plan = plan_mac_auto_availability(policy(), observation(EffectiveAvailabilityMode::Active))
            .expect("plan");
        let json = serde_json::to_string(&plan).expect("serialize");

        assert!(json.contains("\"schema_version\":1"));
        assert!(json.contains("\"desired_profile\":\"interactive\""));
        assert!(!json.contains("Safari"));
        assert!(!json.contains("Chrome"));
        assert!(!json.contains("/Users/"));
    }
}
