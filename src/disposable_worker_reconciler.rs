use std::fmt;

use serde::Serialize;

use crate::disposable_attempt_catalog::DisposableAttemptCatalogAction;
use crate::disposable_attempt_state::DisposableAttemptState;
use crate::execution_admission::EpochMillis;
use crate::github_scale_set_protocol::{ScaleSetJobEvent, ScaleSetRunnerReference};

pub const DISPOSABLE_WORKER_RECONCILER_SCHEMA_VERSION: u8 = 1;
const MAX_IDENTIFIER_LEN: usize = 96;
const MAX_WORKERS: u16 = 64;
const MAX_CPU_MILLIS: u32 = 1_000_000;
const MAX_BYTES: u64 = 1_u64 << 50;

macro_rules! identifier {
    ($name:ident, $field:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn parse(value: &str) -> Result<Self, DisposableWorkerReconcilerError> {
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

identifier!(DisposableAttemptId, "attempt_id");
identifier!(CapacityClaimId, "capacity_claim_id");
identifier!(DisposableVmId, "vm_id");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DisposableWorkerResources {
    cpu_millis: u32,
    memory_bytes: u64,
    disk_bytes: u64,
}

impl DisposableWorkerResources {
    pub fn new(
        cpu_millis: u32,
        memory_bytes: u64,
        disk_bytes: u64,
    ) -> Result<Self, DisposableWorkerReconcilerError> {
        if !(1..=MAX_CPU_MILLIS).contains(&cpu_millis) {
            return Err(DisposableWorkerReconcilerError::new(
                "resources.cpu_millis",
                "invalid_cpu_limit",
                "CPU must be within the bounded positive range",
            ));
        }
        if !(1..=MAX_BYTES).contains(&memory_bytes) {
            return Err(DisposableWorkerReconcilerError::new(
                "resources.memory_bytes",
                "invalid_memory_limit",
                "memory must be within the bounded positive range",
            ));
        }
        if !(1..=MAX_BYTES).contains(&disk_bytes) {
            return Err(DisposableWorkerReconcilerError::new(
                "resources.disk_bytes",
                "invalid_disk_limit",
                "disk must be within the bounded positive range",
            ));
        }
        Ok(Self {
            cpu_millis,
            memory_bytes,
            disk_bytes,
        })
    }

    #[must_use]
    pub const fn cpu_millis(self) -> u32 {
        self.cpu_millis
    }

    #[must_use]
    pub const fn memory_bytes(self) -> u64 {
        self.memory_bytes
    }

    #[must_use]
    pub const fn disk_bytes(self) -> u64 {
        self.disk_bytes
    }

    fn worker_capacity_within(self, request: Self) -> u64 {
        u64::from(self.cpu_millis / request.cpu_millis)
            .min(self.memory_bytes / request.memory_bytes)
            .min(self.disk_bytes / request.disk_bytes)
    }

    fn fits_within(self, limit: Self) -> bool {
        self.cpu_millis <= limit.cpu_millis
            && self.memory_bytes <= limit.memory_bytes
            && self.disk_bytes <= limit.disk_bytes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DisposableHostBudget {
    max_workers: u16,
    total: DisposableWorkerResources,
}

impl DisposableHostBudget {
    pub fn new(
        max_workers: u16,
        total: DisposableWorkerResources,
    ) -> Result<Self, DisposableWorkerReconcilerError> {
        if !(1..=MAX_WORKERS).contains(&max_workers) {
            return Err(DisposableWorkerReconcilerError::new(
                "budget.max_workers",
                "invalid_worker_limit",
                "worker limit must be within the bounded positive range",
            ));
        }
        Ok(Self { max_workers, total })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DisposableHostUsage {
    workers: u16,
    resources: DisposableWorkerResources,
}

impl DisposableHostUsage {
    #[must_use]
    pub const fn zero() -> Self {
        Self {
            workers: 0,
            resources: DisposableWorkerResources {
                cpu_millis: 0,
                memory_bytes: 0,
                disk_bytes: 0,
            },
        }
    }

    pub fn new(
        workers: u16,
        resources: DisposableWorkerResources,
    ) -> Result<Self, DisposableWorkerReconcilerError> {
        if workers > MAX_WORKERS {
            return Err(DisposableWorkerReconcilerError::new(
                "usage.workers",
                "invalid_worker_usage",
                "observed worker usage exceeds the implementation bound",
            ));
        }
        Ok(Self { workers, resources })
    }

    #[must_use]
    pub const fn workers(self) -> u16 {
        self.workers
    }

    #[must_use]
    pub const fn resources(self) -> DisposableWorkerResources {
        self.resources
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ScaleSetDemand {
    assigned_jobs: u16,
    running_jobs: u16,
    observed_at: EpochMillis,
    expires_at: EpochMillis,
}

impl ScaleSetDemand {
    pub fn new(
        assigned_jobs: u16,
        running_jobs: u16,
        observed_at: EpochMillis,
        expires_at: EpochMillis,
    ) -> Result<Self, DisposableWorkerReconcilerError> {
        if assigned_jobs > MAX_WORKERS || running_jobs > assigned_jobs {
            return Err(DisposableWorkerReconcilerError::new(
                "demand",
                "invalid_scale_set_demand",
                "scale-set demand must be bounded and running jobs cannot exceed assigned jobs",
            ));
        }
        if expires_at < observed_at {
            return Err(DisposableWorkerReconcilerError::new(
                "demand.expires_at",
                "invalid_demand_window",
                "scale-set demand expiry cannot precede its observation",
            ));
        }
        Ok(Self {
            assigned_jobs,
            running_jobs,
            observed_at,
            expires_at,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DisposableCapacityPlan {
    schema_version: u8,
    advertised_max_capacity: u16,
    desired_workers: u16,
    additional_workers: u16,
}

impl DisposableCapacityPlan {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn advertised_max_capacity(&self) -> u16 {
        self.advertised_max_capacity
    }

    #[must_use]
    pub const fn desired_workers(&self) -> u16 {
        self.desired_workers
    }

    #[must_use]
    pub const fn additional_workers(&self) -> u16 {
        self.additional_workers
    }
}

pub fn plan_capacity(
    now: EpochMillis,
    demand: ScaleSetDemand,
    budget: DisposableHostBudget,
    usage: DisposableHostUsage,
    worker: DisposableWorkerResources,
) -> Result<DisposableCapacityPlan, DisposableWorkerReconcilerError> {
    if now < demand.observed_at || now > demand.expires_at {
        return Err(DisposableWorkerReconcilerError::new(
            "demand",
            "stale_scale_set_demand",
            "scale-set demand must be current at the capacity decision",
        ));
    }
    if usage.workers > budget.max_workers || !usage.resources.fits_within(budget.total) {
        return Err(DisposableWorkerReconcilerError::new(
            "usage",
            "host_budget_exceeded",
            "current worker usage already exceeds the configured host budget",
        ));
    }

    let resource_capacity = budget.total.worker_capacity_within(worker);
    let advertised_max_capacity = budget
        .max_workers
        .min(u16::try_from(resource_capacity).unwrap_or(u16::MAX));
    let desired_workers = demand.assigned_jobs.min(advertised_max_capacity);

    let remaining = DisposableWorkerResources {
        cpu_millis: budget.total.cpu_millis - usage.resources.cpu_millis,
        memory_bytes: budget.total.memory_bytes - usage.resources.memory_bytes,
        disk_bytes: budget.total.disk_bytes - usage.resources.disk_bytes,
    };
    let resource_slots =
        u16::try_from(remaining.worker_capacity_within(worker)).unwrap_or(u16::MAX);
    let worker_slots = budget.max_workers - usage.workers;
    let demand_slots = desired_workers.saturating_sub(usage.workers);
    let additional_workers = demand_slots.min(worker_slots).min(resource_slots);

    Ok(DisposableCapacityPlan {
        schema_version: DISPOSABLE_WORKER_RECONCILER_SCHEMA_VERSION,
        advertised_max_capacity,
        desired_workers,
        additional_workers,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableAttemptPhase {
    Reserved,
    Provisioning,
    Registering,
    Waiting,
    Assigned,
    Running,
    Terminal,
    Destroying,
    Deregistering,
    Releasing,
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ExactObjectObservation {
    Unknown,
    Absent,
    Matching,
    Conflicting,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ScaleSetRunnerObservation {
    Unknown,
    Absent,
    /// The exact service registration exists, but a bounded launch/recovery check proved that no
    /// usable listener is becoming ready from the one-time JIT configuration.
    RegistrationOnly {
        runner: ScaleSetRunnerReference,
    },
    IdleReady {
        runner: ScaleSetRunnerReference,
    },
    Conflicting,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisposableWorkerReconcileInput<'a> {
    pub now: EpochMillis,
    pub attempt: &'a DisposableAttemptState,
    pub vm: ExactObjectObservation,
    pub runner: ScaleSetRunnerObservation,
    pub job_event: Option<ScaleSetJobEvent>,
    pub capacity_reserved: bool,
    pub cancellation_requested: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableWorkerObservationTarget {
    Vm,
    Runner,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum DisposableWorkerAction {
    Persist {
        transition: DisposableAttemptCatalogAction,
    },
    Observe {
        target: DisposableWorkerObservationTarget,
    },
    ProvisionVm,
    GenerateJitAndStartRunner,
    Wait,
    DestroyVm,
    DeleteRunner,
    ReleaseCapacity,
    NoOp,
    Blocked {
        code: &'static str,
    },
}

pub fn reconcile_attempt(
    input: DisposableWorkerReconcileInput<'_>,
) -> Result<DisposableWorkerAction, DisposableWorkerReconcilerError> {
    let cleanup = input.cancellation_requested || input.now > input.attempt.not_after();

    if matches!(input.vm, ExactObjectObservation::Conflicting) {
        return Ok(DisposableWorkerAction::Blocked {
            code: "conflicting_vm_identity",
        });
    }
    if matches!(input.runner, ScaleSetRunnerObservation::Conflicting) {
        return Ok(DisposableWorkerAction::Blocked {
            code: "conflicting_runner_identity",
        });
    }
    validate_runner_observation(input.attempt, &input.runner)?;
    if let Some(event) = input.job_event.as_ref()
        && let Some(action) = plan_job_event(input.attempt, event)?
    {
        return Ok(action);
    }

    use DisposableAttemptPhase as Phase;
    use DisposableWorkerAction as Action;
    if !input.capacity_reserved
        && matches!(
            input.attempt.phase(),
            Phase::Reserved
                | Phase::Provisioning
                | Phase::Registering
                | Phase::Waiting
                | Phase::Assigned
                | Phase::Running
                | Phase::Terminal
        )
    {
        return Ok(persist(DisposableAttemptCatalogAction::BeginCleanup));
    }
    Ok(match input.attempt.phase() {
        Phase::Reserved if cleanup => persist(DisposableAttemptCatalogAction::BeginCleanup),
        Phase::Reserved => persist(DisposableAttemptCatalogAction::BeginProvisioning),
        Phase::Provisioning if cleanup => persist(DisposableAttemptCatalogAction::BeginCleanup),
        Phase::Provisioning => match input.vm {
            ExactObjectObservation::Unknown => Action::Observe {
                target: DisposableWorkerObservationTarget::Vm,
            },
            ExactObjectObservation::Absent => Action::ProvisionVm,
            ExactObjectObservation::Matching => {
                persist(DisposableAttemptCatalogAction::BeginRegistration)
            }
            ExactObjectObservation::Conflicting => unreachable!(),
        },
        Phase::Registering if cleanup => persist(DisposableAttemptCatalogAction::BeginCleanup),
        Phase::Registering if matches!(input.vm, ExactObjectObservation::Unknown) => {
            Action::Observe {
                target: DisposableWorkerObservationTarget::Vm,
            }
        }
        Phase::Registering if matches!(input.vm, ExactObjectObservation::Absent) => {
            persist(DisposableAttemptCatalogAction::BeginCleanup)
        }
        Phase::Registering => match &input.runner {
            ScaleSetRunnerObservation::Unknown => Action::Observe {
                target: DisposableWorkerObservationTarget::Runner,
            },
            ScaleSetRunnerObservation::Absent => Action::GenerateJitAndStartRunner,
            ScaleSetRunnerObservation::RegistrationOnly { .. }
                if input.attempt.runner_id().is_none() =>
            {
                Action::DeleteRunner
            }
            ScaleSetRunnerObservation::RegistrationOnly { .. } => {
                persist(DisposableAttemptCatalogAction::BeginCleanup)
            }
            ScaleSetRunnerObservation::IdleReady { runner } => persist(validate_transition(
                input.attempt,
                DisposableAttemptCatalogAction::RecordRunnerReady(runner.clone()),
            )?),
            ScaleSetRunnerObservation::Conflicting => unreachable!(),
        },
        Phase::Waiting | Phase::Assigned | Phase::Running if cleanup => {
            persist(DisposableAttemptCatalogAction::BeginCleanup)
        }
        Phase::Waiting | Phase::Assigned | Phase::Running
            if matches!(input.vm, ExactObjectObservation::Unknown) =>
        {
            Action::Observe {
                target: DisposableWorkerObservationTarget::Vm,
            }
        }
        Phase::Waiting | Phase::Assigned | Phase::Running
            if matches!(input.vm, ExactObjectObservation::Absent) =>
        {
            persist(DisposableAttemptCatalogAction::BeginCleanup)
        }
        Phase::Waiting | Phase::Assigned | Phase::Running => match &input.runner {
            ScaleSetRunnerObservation::Unknown => Action::Observe {
                target: DisposableWorkerObservationTarget::Runner,
            },
            ScaleSetRunnerObservation::Absent
            | ScaleSetRunnerObservation::RegistrationOnly { .. } => {
                persist(DisposableAttemptCatalogAction::BeginCleanup)
            }
            ScaleSetRunnerObservation::IdleReady { runner }
                if input.attempt.runner_id().is_none() =>
            {
                persist(validate_transition(
                    input.attempt,
                    DisposableAttemptCatalogAction::RecordRunnerReady(runner.clone()),
                )?)
            }
            ScaleSetRunnerObservation::IdleReady { .. } => Action::Wait,
            ScaleSetRunnerObservation::Conflicting => unreachable!(),
        },
        Phase::Terminal => persist(DisposableAttemptCatalogAction::BeginCleanup),
        Phase::Destroying => match input.vm {
            ExactObjectObservation::Unknown => Action::Observe {
                target: DisposableWorkerObservationTarget::Vm,
            },
            ExactObjectObservation::Matching => Action::DestroyVm,
            ExactObjectObservation::Absent => persist(
                DisposableAttemptCatalogAction::AdvanceCleanup(Phase::Deregistering),
            ),
            ExactObjectObservation::Conflicting => unreachable!(),
        },
        Phase::Deregistering if matches!(input.vm, ExactObjectObservation::Unknown) => {
            Action::Observe {
                target: DisposableWorkerObservationTarget::Vm,
            }
        }
        Phase::Deregistering if matches!(input.vm, ExactObjectObservation::Matching) => {
            Action::DestroyVm
        }
        Phase::Deregistering => match &input.runner {
            ScaleSetRunnerObservation::Unknown => Action::Observe {
                target: DisposableWorkerObservationTarget::Runner,
            },
            ScaleSetRunnerObservation::Absent => persist(
                DisposableAttemptCatalogAction::AdvanceCleanup(Phase::Releasing),
            ),
            ScaleSetRunnerObservation::RegistrationOnly { .. }
            | ScaleSetRunnerObservation::IdleReady { .. } => Action::DeleteRunner,
            ScaleSetRunnerObservation::Conflicting => unreachable!(),
        },
        Phase::Releasing if matches!(input.vm, ExactObjectObservation::Unknown) => {
            Action::Observe {
                target: DisposableWorkerObservationTarget::Vm,
            }
        }
        Phase::Releasing if matches!(input.vm, ExactObjectObservation::Matching) => {
            Action::DestroyVm
        }
        Phase::Releasing if matches!(input.runner, ScaleSetRunnerObservation::Unknown) => {
            Action::Observe {
                target: DisposableWorkerObservationTarget::Runner,
            }
        }
        Phase::Releasing if !matches!(input.runner, ScaleSetRunnerObservation::Absent) => {
            Action::DeleteRunner
        }
        Phase::Releasing if input.capacity_reserved => Action::ReleaseCapacity,
        Phase::Releasing => persist(DisposableAttemptCatalogAction::AdvanceCleanup(
            Phase::Complete,
        )),
        Phase::Complete if input.capacity_reserved => Action::Blocked {
            code: "completed_attempt_retains_capacity",
        },
        Phase::Complete
            if matches!(input.vm, ExactObjectObservation::Unknown)
                || matches!(input.runner, ScaleSetRunnerObservation::Unknown) =>
        {
            Action::Blocked {
                code: "completed_attempt_external_state_unknown",
            }
        }
        Phase::Complete
            if !matches!(input.vm, ExactObjectObservation::Absent)
                || !matches!(input.runner, ScaleSetRunnerObservation::Absent) =>
        {
            Action::Blocked {
                code: "completed_attempt_retains_external_state",
            }
        }
        Phase::Complete => Action::NoOp,
    })
}

fn persist(transition: DisposableAttemptCatalogAction) -> DisposableWorkerAction {
    DisposableWorkerAction::Persist { transition }
}

fn validate_transition(
    attempt: &DisposableAttemptState,
    transition: DisposableAttemptCatalogAction,
) -> Result<DisposableAttemptCatalogAction, DisposableWorkerReconcilerError> {
    let result = match &transition {
        DisposableAttemptCatalogAction::BeginProvisioning => attempt.begin_provisioning(),
        DisposableAttemptCatalogAction::BeginRegistration => attempt.begin_registration(),
        DisposableAttemptCatalogAction::RecordRegistration(runner) => {
            attempt.record_registration(runner)
        }
        DisposableAttemptCatalogAction::RecordRunnerReady(runner) => {
            attempt.record_runner_ready(runner)
        }
        DisposableAttemptCatalogAction::RecordAssigned(job_id) => {
            attempt.record_assigned(job_id.clone())
        }
        DisposableAttemptCatalogAction::RecordRunning { runner, job_id } => {
            attempt.record_running(runner, job_id.clone())
        }
        DisposableAttemptCatalogAction::RecordTerminal {
            runner,
            job_id,
            result,
        } => attempt.record_terminal(runner.as_ref(), job_id.clone(), result.clone()),
        DisposableAttemptCatalogAction::BeginCleanup => attempt.begin_cleanup(),
        DisposableAttemptCatalogAction::AdvanceCleanup(phase) => attempt.advance_cleanup(*phase),
    };
    result.map_err(|error| {
        let code = if error.code() == "identity_drift" {
            "github_job_identity_drift"
        } else {
            "invalid_attempt_transition"
        };
        DisposableWorkerReconcilerError::new(
            "attempt",
            code,
            "observation cannot advance the durable disposable attempt",
        )
    })?;
    Ok(transition)
}

fn validate_runner_observation(
    attempt: &DisposableAttemptState,
    observation: &ScaleSetRunnerObservation,
) -> Result<(), DisposableWorkerReconcilerError> {
    let runner = match observation {
        ScaleSetRunnerObservation::RegistrationOnly { runner }
        | ScaleSetRunnerObservation::IdleReady { runner } => runner,
        ScaleSetRunnerObservation::Unknown
        | ScaleSetRunnerObservation::Absent
        | ScaleSetRunnerObservation::Conflicting => return Ok(()),
    };
    if runner.name != *attempt.runner_name()
        || attempt.runner_id().is_some_and(|id| id != runner.id)
    {
        return Err(DisposableWorkerReconcilerError::new(
            "runner",
            "runner_identity_drift",
            "runner observation differs from the durable runner identity",
        ));
    }
    Ok(())
}

fn plan_job_event(
    attempt: &DisposableAttemptState,
    event: &ScaleSetJobEvent,
) -> Result<Option<DisposableWorkerAction>, DisposableWorkerReconcilerError> {
    if matches!(
        attempt.phase(),
        DisposableAttemptPhase::Destroying
            | DisposableAttemptPhase::Deregistering
            | DisposableAttemptPhase::Releasing
            | DisposableAttemptPhase::Complete
    ) {
        validate_late_job_event_identity(attempt, event)?;
        return Ok(None);
    }
    let transition = match event {
        ScaleSetJobEvent::Started { runner, job_id } => {
            DisposableAttemptCatalogAction::RecordRunning {
                runner: runner.clone(),
                job_id: job_id.clone(),
            }
        }
        ScaleSetJobEvent::Completed {
            runner,
            job_id,
            result,
        } => DisposableAttemptCatalogAction::RecordTerminal {
            runner: runner.clone(),
            job_id: job_id.clone(),
            result: result.clone(),
        },
    };
    let before = attempt.revision();
    let transition = validate_transition(attempt, transition)?;
    let after = match &transition {
        DisposableAttemptCatalogAction::RecordRunning { runner, job_id } => {
            attempt.record_running(runner, job_id.clone())
        }
        DisposableAttemptCatalogAction::RecordTerminal {
            runner,
            job_id,
            result,
        } => attempt.record_terminal(runner.as_ref(), job_id.clone(), result.clone()),
        _ => unreachable!(),
    }
    .map_err(|_| {
        DisposableWorkerReconcilerError::new(
            "job_event",
            "invalid_job_event",
            "job event cannot advance the durable attempt",
        )
    })?;
    Ok((after.revision() != before).then(|| persist(transition)))
}

fn validate_late_job_event_identity(
    attempt: &DisposableAttemptState,
    event: &ScaleSetJobEvent,
) -> Result<(), DisposableWorkerReconcilerError> {
    let runner = event.runner();
    let job_id = event.job_id();
    let runner_matches = runner.is_none_or(|runner| {
        runner.name == *attempt.runner_name()
            && attempt.runner_id().is_none_or(|id| id == runner.id)
    });
    let job_matches = attempt
        .github_job_id()
        .is_none_or(|current| current == job_id);
    if runner_matches && job_matches {
        Ok(())
    } else {
        Err(DisposableWorkerReconcilerError::new(
            "job_event",
            "github_job_identity_drift",
            "late job event conflicts with cleanup-bound durable identity",
        ))
    }
}

fn validate_identifier(
    field: &'static str,
    value: &str,
) -> Result<(), DisposableWorkerReconcilerError> {
    if value.is_empty()
        || value.len() > MAX_IDENTIFIER_LEN
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || value.starts_with('-')
        || value.ends_with('-')
    {
        return Err(DisposableWorkerReconcilerError::new(
            field,
            "invalid_identifier",
            "identifier must be bounded lowercase ASCII with interior hyphens only",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DisposableWorkerReconcilerError {
    field: &'static str,
    code: &'static str,
    message: &'static str,
}

impl DisposableWorkerReconcilerError {
    const fn new(field: &'static str, code: &'static str, message: &'static str) -> Self {
        Self {
            field,
            code,
            message,
        }
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for DisposableWorkerReconcilerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl std::error::Error for DisposableWorkerReconcilerError {}
