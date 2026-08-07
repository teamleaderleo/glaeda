use std::fmt;

use serde::Serialize;

pub use crate::mac_auto_availability::{
    DesiredLimaProfile, LocalDispatchDecision, MacAdmissionClass, MacAutoAvailabilityPlan,
    MacAutoAvailabilityPolicy, MacAutoReason, MacAutoReasonKind, MacJobTrust, OperatorActivityState,
    WORK_PROFILE,
};
use crate::mac_auto_availability::{
    MacAutoAvailabilityError, MacAutoAvailabilityObservation, MacQueuedJob,
    plan_mac_auto_availability,
};
use crate::mac_availability::{
    AvailabilityRequest, EffectiveAvailabilityMode, HostPowerSource, JobActivity, MemoryPressure,
    ObservationFreshness, VmPowerState,
};
use crate::personal_worker_queue::PersonalWorkerJobClass;

pub const MAC_AUTO_CONTROLLER_SCHEMA_VERSION: u8 = 1;
pub const INITIAL_LOCAL_JOB_CPU_MILLIS: u32 = 2_000;
pub const INITIAL_LOCAL_JOB_MEMORY_BYTES: u64 = 2 * 1_024 * 1_024 * 1_024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MacWorkloadClass {
    Light,
    Work,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MacLocalJobRequest {
    pub class: MacWorkloadClass,
    pub trust: MacJobTrust,
    pub requested_cpu_millis: u32,
    pub requested_memory_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacAutoControllerObservation {
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
    pub next_job: Option<MacLocalJobRequest>,
    pub healthy_observation_streak: u8,
    pub last_transition_at_millis: u64,
    pub decision_at_millis: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MacAutoControllerReasonKind {
    AutoEvidenceIncomplete,
    LocalResourceRequestExceeded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MacAutoControllerReason {
    pub kind: MacAutoControllerReasonKind,
    pub message: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MacAutoControllerPlan {
    pub schema_version: u8,
    pub policy: MacAutoAvailabilityPlan,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_resource_fit: Option<bool>,
    pub controller_reasons: Vec<MacAutoControllerReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MacAutoControllerError {
    pub field: &'static str,
    pub code: &'static str,
    pub message: &'static str,
}

impl MacAutoControllerError {
    const fn new(field: &'static str, code: &'static str, message: &'static str) -> Self {
        Self {
            field,
            code,
            message,
        }
    }
}

impl From<MacAutoAvailabilityError> for MacAutoControllerError {
    fn from(value: MacAutoAvailabilityError) -> Self {
        Self::new(value.field, value.code, value.message)
    }
}

impl fmt::Display for MacAutoControllerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for MacAutoControllerError {}

/// Plan automatic personal-Mac admission with the exact initial R0 execution cap.
///
/// The lower-level availability reducer remains reusable policy machinery. This facade is the
/// #288 integration surface: it additionally proves that the next job fits the current local
/// container envelope and disables automatic admission when required host evidence is incomplete.
///
/// # Errors
///
/// Returns a bounded error for zero resource requests or any invalid lower-level observation.
pub fn plan_mac_auto_controller(
    policy: MacAutoAvailabilityPolicy,
    observation: MacAutoControllerObservation,
) -> Result<MacAutoControllerPlan, MacAutoControllerError> {
    validate_job_request(observation.next_job)?;

    let core_next_job = observation.next_job.map(|job| MacQueuedJob {
        class: match job.class {
            MacWorkloadClass::Light => PersonalWorkerJobClass::Light,
            MacWorkloadClass::Work => PersonalWorkerJobClass::Heavy,
        },
        trust: job.trust,
    });

    let mut policy_plan = plan_mac_auto_availability(
        policy,
        MacAutoAvailabilityObservation {
            requested_mode: observation.requested_mode,
            effective_mode: observation.effective_mode,
            vm_power: observation.vm_power,
            job_activity: observation.job_activity,
            resource_freshness: observation.resource_freshness,
            activity_freshness: observation.activity_freshness,
            queue_freshness: observation.queue_freshness,
            host_power: observation.host_power,
            battery_percent: observation.battery_percent,
            memory_pressure: observation.memory_pressure,
            swap_used_bytes: observation.swap_used_bytes,
            previous_swap_used_bytes: observation.previous_swap_used_bytes,
            operator_activity: observation.operator_activity,
            operator_idle_millis: observation.operator_idle_millis,
            operator_hold: observation.operator_hold,
            queued_jobs: observation.queued_jobs,
            next_job: core_next_job,
            healthy_observation_streak: observation.healthy_observation_streak,
            last_transition_at_millis: observation.last_transition_at_millis,
            decision_at_millis: observation.decision_at_millis,
        },
    )?;

    let local_resource_fit = observation.next_job.map(local_job_fits_initial_envelope);
    let mut controller_reasons = Vec::new();

    if observation.requested_mode == AvailabilityRequest::Auto
        && auto_evidence_incomplete(observation)
    {
        policy_plan.admission = MacAdmissionClass::None;
        policy_plan.dispatch = observation
            .next_job
            .map(|_| LocalDispatchDecision::OverflowRecommended);
        policy_plan.next_action = None;
        controller_reasons.push(MacAutoControllerReason {
            kind: MacAutoControllerReasonKind::AutoEvidenceIncomplete,
            message: "automatic local admission requires fresh known activity, power, memory, queue, and job evidence",
        });
    }

    if matches!(local_resource_fit, Some(false)) {
        policy_plan.dispatch = Some(LocalDispatchDecision::OverflowRecommended);
        controller_reasons.push(MacAutoControllerReason {
            kind: MacAutoControllerReasonKind::LocalResourceRequestExceeded,
            message: "next job exceeds the initial 2 CPU / 2 GiB local execution envelope",
        });
    }

    Ok(MacAutoControllerPlan {
        schema_version: MAC_AUTO_CONTROLLER_SCHEMA_VERSION,
        policy: policy_plan,
        local_resource_fit,
        controller_reasons,
    })
}

fn validate_job_request(
    next_job: Option<MacLocalJobRequest>,
) -> Result<(), MacAutoControllerError> {
    let Some(job) = next_job else {
        return Ok(());
    };
    if job.requested_cpu_millis == 0 {
        return Err(MacAutoControllerError::new(
            "next_job.requested_cpu_millis",
            "invalid_cpu_request",
            "local job CPU request must be greater than zero",
        ));
    }
    if job.requested_memory_bytes == 0 {
        return Err(MacAutoControllerError::new(
            "next_job.requested_memory_bytes",
            "invalid_memory_request",
            "local job memory request must be greater than zero",
        ));
    }
    Ok(())
}

const fn local_job_fits_initial_envelope(job: MacLocalJobRequest) -> bool {
    job.requested_cpu_millis <= INITIAL_LOCAL_JOB_CPU_MILLIS
        && job.requested_memory_bytes <= INITIAL_LOCAL_JOB_MEMORY_BYTES
}

fn auto_evidence_incomplete(observation: MacAutoControllerObservation) -> bool {
    observation.resource_freshness == ObservationFreshness::Stale
        || observation.activity_freshness == ObservationFreshness::Stale
        || observation.queue_freshness == ObservationFreshness::Stale
        || observation.operator_activity == OperatorActivityState::Unknown
        || observation.host_power == HostPowerSource::Unknown
        || observation.memory_pressure == MemoryPressure::Unknown
        || observation.job_activity == JobActivity::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: u64 = 60 * 1_000;
    const GIB: u64 = 1_024 * 1_024 * 1_024;

    fn observation() -> MacAutoControllerObservation {
        MacAutoControllerObservation {
            requested_mode: AvailabilityRequest::Auto,
            effective_mode: EffectiveAvailabilityMode::Active,
            vm_power: VmPowerState::Running,
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
            next_job: Some(MacLocalJobRequest {
                class: MacWorkloadClass::Light,
                trust: MacJobTrust::Trusted,
                requested_cpu_millis: INITIAL_LOCAL_JOB_CPU_MILLIS,
                requested_memory_bytes: INITIAL_LOCAL_JOB_MEMORY_BYTES,
            }),
            healthy_observation_streak: 3,
            last_transition_at_millis: 10 * MINUTE,
            decision_at_millis: 20 * MINUTE,
        }
    }

    #[test]
    fn fitting_light_job_runs_locally_while_operator_is_active() {
        let plan = plan_mac_auto_controller(MacAutoAvailabilityPolicy::initial(), observation())
            .expect("plan");

        assert_eq!(plan.local_resource_fit, Some(true));
        assert_eq!(
            plan.policy.dispatch,
            Some(LocalDispatchDecision::RunLocalNow)
        );
    }

    #[test]
    fn semantic_work_job_waits_for_idle_work_mode() {
        let mut facts = observation();
        facts.next_job = Some(MacLocalJobRequest {
            class: MacWorkloadClass::Work,
            trust: MacJobTrust::Trusted,
            requested_cpu_millis: INITIAL_LOCAL_JOB_CPU_MILLIS,
            requested_memory_bytes: INITIAL_LOCAL_JOB_MEMORY_BYTES,
        });

        let active = plan_mac_auto_controller(MacAutoAvailabilityPolicy::initial(), facts)
            .expect("active plan");
        assert_eq!(
            active.policy.dispatch,
            Some(LocalDispatchDecision::OverflowRecommended)
        );

        facts.operator_activity = OperatorActivityState::Idle;
        facts.operator_idle_millis = Some(20 * MINUTE);
        let idle = plan_mac_auto_controller(MacAutoAvailabilityPolicy::initial(), facts)
            .expect("idle plan");
        assert_eq!(idle.policy.resolved_mode, EffectiveAvailabilityMode::Away);
        assert_eq!(idle.policy.dispatch, Some(LocalDispatchDecision::QueueLocal));
    }

    #[test]
    fn oversized_job_overflows_even_when_work_mode_is_available() {
        let mut facts = observation();
        facts.effective_mode = EffectiveAvailabilityMode::Away;
        facts.operator_activity = OperatorActivityState::Idle;
        facts.operator_idle_millis = Some(20 * MINUTE);
        facts.next_job = Some(MacLocalJobRequest {
            class: MacWorkloadClass::Work,
            trust: MacJobTrust::Trusted,
            requested_cpu_millis: INITIAL_LOCAL_JOB_CPU_MILLIS + 1,
            requested_memory_bytes: INITIAL_LOCAL_JOB_MEMORY_BYTES,
        });

        let plan = plan_mac_auto_controller(MacAutoAvailabilityPolicy::initial(), facts)
            .expect("plan");

        assert_eq!(plan.local_resource_fit, Some(false));
        assert_eq!(
            plan.policy.dispatch,
            Some(LocalDispatchDecision::OverflowRecommended)
        );
        assert!(plan.controller_reasons.iter().any(|reason| {
            reason.kind == MacAutoControllerReasonKind::LocalResourceRequestExceeded
        }));
    }

    #[test]
    fn unknown_activity_fails_closed_in_auto_mode() {
        let mut facts = observation();
        facts.operator_activity = OperatorActivityState::Unknown;
        facts.operator_idle_millis = None;

        let plan = plan_mac_auto_controller(MacAutoAvailabilityPolicy::initial(), facts)
            .expect("plan");

        assert_eq!(plan.policy.admission, MacAdmissionClass::None);
        assert_eq!(
            plan.policy.dispatch,
            Some(LocalDispatchDecision::OverflowRecommended)
        );
        assert!(plan.policy.next_action.is_none());
    }

    #[test]
    fn unknown_power_fails_closed_in_auto_mode() {
        let mut facts = observation();
        facts.host_power = HostPowerSource::Unknown;

        let plan = plan_mac_auto_controller(MacAutoAvailabilityPolicy::initial(), facts)
            .expect("plan");

        assert_eq!(plan.policy.admission, MacAdmissionClass::None);
        assert_eq!(
            plan.policy.dispatch,
            Some(LocalDispatchDecision::OverflowRecommended)
        );
    }

    #[test]
    fn explicit_active_can_ignore_unknown_activity_but_keeps_resource_cap() {
        let mut facts = observation();
        facts.requested_mode = AvailabilityRequest::Active;
        facts.operator_activity = OperatorActivityState::Unknown;
        facts.operator_idle_millis = None;

        let plan = plan_mac_auto_controller(MacAutoAvailabilityPolicy::initial(), facts)
            .expect("plan");

        assert_eq!(plan.policy.admission, MacAdmissionClass::Light);
        assert_eq!(plan.local_resource_fit, Some(true));
    }

    #[test]
    fn zero_resource_request_is_rejected() {
        let mut facts = observation();
        facts.next_job = Some(MacLocalJobRequest {
            class: MacWorkloadClass::Light,
            trust: MacJobTrust::Trusted,
            requested_cpu_millis: 0,
            requested_memory_bytes: GIB,
        });

        let error = plan_mac_auto_controller(MacAutoAvailabilityPolicy::initial(), facts)
            .expect_err("invalid request");
        assert_eq!(error.code, "invalid_cpu_request");
    }
}
