use std::fmt;

use serde::Serialize;

use crate::execution_admission::EpochMillis;

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
identifier!(ScaleSetRunnerId, "runner_id");

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct GitHubJobId(u64);

impl GitHubJobId {
    pub fn new(value: u64) -> Result<Self, DisposableWorkerReconcilerError> {
        if value == 0 {
            return Err(DisposableWorkerReconcilerError::new(
                "github_job_id",
                "invalid_github_job_id",
                "GitHub job identity must be greater than zero",
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
#[serde(rename_all = "snake_case")]
pub enum GitHubJobConclusion {
    Success,
    Failure,
    Cancelled,
    TimedOut,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DisposableAttempt {
    schema_version: u8,
    attempt_id: DisposableAttemptId,
    capacity_claim_id: CapacityClaimId,
    vm_id: DisposableVmId,
    runner_id: ScaleSetRunnerId,
    phase: DisposableAttemptPhase,
    github_job_id: Option<GitHubJobId>,
    conclusion: Option<GitHubJobConclusion>,
    not_after: EpochMillis,
}

impl DisposableAttempt {
    #[must_use]
    pub fn reserved(
        attempt_id: DisposableAttemptId,
        capacity_claim_id: CapacityClaimId,
        vm_id: DisposableVmId,
        runner_id: ScaleSetRunnerId,
        not_after: EpochMillis,
    ) -> Self {
        Self {
            schema_version: DISPOSABLE_WORKER_RECONCILER_SCHEMA_VERSION,
            attempt_id,
            capacity_claim_id,
            vm_id,
            runner_id,
            phase: DisposableAttemptPhase::Reserved,
            github_job_id: None,
            conclusion: None,
            not_after,
        }
    }

    #[must_use]
    pub const fn phase(&self) -> DisposableAttemptPhase {
        self.phase
    }

    #[must_use]
    pub const fn attempt_id(&self) -> &DisposableAttemptId {
        &self.attempt_id
    }

    #[must_use]
    pub const fn capacity_claim_id(&self) -> &CapacityClaimId {
        &self.capacity_claim_id
    }

    #[must_use]
    pub const fn vm_id(&self) -> &DisposableVmId {
        &self.vm_id
    }

    #[must_use]
    pub const fn runner_id(&self) -> &ScaleSetRunnerId {
        &self.runner_id
    }

    #[must_use]
    pub const fn github_job_id(&self) -> Option<GitHubJobId> {
        self.github_job_id
    }

    #[must_use]
    pub const fn conclusion(&self) -> Option<GitHubJobConclusion> {
        self.conclusion
    }

    #[must_use]
    pub const fn not_after(&self) -> EpochMillis {
        self.not_after
    }

    pub fn checkpoint(
        &self,
        action: &DisposableWorkerAction,
    ) -> Result<Self, DisposableWorkerReconcilerError> {
        let (phase, job, conclusion) = match action {
            DisposableWorkerAction::Checkpoint { phase } => {
                (*phase, self.github_job_id, self.conclusion)
            }
            DisposableWorkerAction::RecordAssigned { github_job_id } => {
                (DisposableAttemptPhase::Assigned, Some(*github_job_id), None)
            }
            DisposableWorkerAction::RecordRunning { github_job_id } => {
                (DisposableAttemptPhase::Running, Some(*github_job_id), None)
            }
            DisposableWorkerAction::RecordTerminal {
                github_job_id,
                conclusion,
            } => (
                DisposableAttemptPhase::Terminal,
                Some(*github_job_id),
                Some(*conclusion),
            ),
            _ => {
                return Err(DisposableWorkerReconcilerError::new(
                    "action",
                    "action_not_checkpointable",
                    "only durable checkpoint actions can advance an attempt",
                ));
            }
        };
        validate_transition(self.phase, phase)?;
        if let (Some(current), Some(next)) = (self.github_job_id, job)
            && current != next
        {
            return Err(DisposableWorkerReconcilerError::new(
                "github_job_id",
                "github_job_identity_drift",
                "an attempt cannot change its assigned GitHub job",
            ));
        }
        let mut next = self.clone();
        next.phase = phase;
        next.github_job_id = job;
        next.conclusion = conclusion;
        Ok(next)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ExactObjectObservation {
    Unknown,
    Absent,
    Matching,
    Conflicting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ScaleSetRunnerObservation {
    Unknown,
    Absent,
    Idle,
    Assigned {
        github_job_id: GitHubJobId,
    },
    Running {
        github_job_id: GitHubJobId,
    },
    Terminal {
        github_job_id: GitHubJobId,
        conclusion: GitHubJobConclusion,
    },
    Conflicting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisposableWorkerReconcileInput<'a> {
    pub now: EpochMillis,
    pub attempt: &'a DisposableAttempt,
    pub vm: ExactObjectObservation,
    pub runner: ScaleSetRunnerObservation,
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
    Checkpoint {
        phase: DisposableAttemptPhase,
    },
    Observe {
        target: DisposableWorkerObservationTarget,
    },
    ProvisionVm,
    GenerateJitAndStartRunner,
    RecordAssigned {
        github_job_id: GitHubJobId,
    },
    RecordRunning {
        github_job_id: GitHubJobId,
    },
    RecordTerminal {
        github_job_id: GitHubJobId,
        conclusion: GitHubJobConclusion,
    },
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
    validate_attempt(input.attempt)?;
    let cleanup = input.cancellation_requested || input.now > input.attempt.not_after;

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

    use DisposableAttemptPhase as Phase;
    use DisposableWorkerAction as Action;
    if !input.capacity_reserved
        && matches!(
            input.attempt.phase,
            Phase::Reserved
                | Phase::Provisioning
                | Phase::Registering
                | Phase::Waiting
                | Phase::Assigned
                | Phase::Running
                | Phase::Terminal
        )
    {
        return Ok(Action::Checkpoint {
            phase: Phase::Destroying,
        });
    }
    Ok(match input.attempt.phase {
        Phase::Reserved if cleanup => Action::Checkpoint {
            phase: Phase::Destroying,
        },
        Phase::Reserved => Action::Checkpoint {
            phase: Phase::Provisioning,
        },
        Phase::Provisioning if cleanup => Action::Checkpoint {
            phase: Phase::Destroying,
        },
        Phase::Provisioning => match input.vm {
            ExactObjectObservation::Unknown => Action::Observe {
                target: DisposableWorkerObservationTarget::Vm,
            },
            ExactObjectObservation::Absent => Action::ProvisionVm,
            ExactObjectObservation::Matching => Action::Checkpoint {
                phase: Phase::Registering,
            },
            ExactObjectObservation::Conflicting => unreachable!(),
        },
        Phase::Registering if cleanup => Action::Checkpoint {
            phase: Phase::Destroying,
        },
        Phase::Registering if matches!(input.vm, ExactObjectObservation::Unknown) => {
            Action::Observe {
                target: DisposableWorkerObservationTarget::Vm,
            }
        }
        Phase::Registering if matches!(input.vm, ExactObjectObservation::Absent) => {
            Action::Checkpoint {
                phase: Phase::Deregistering,
            }
        }
        Phase::Registering => match input.runner {
            ScaleSetRunnerObservation::Unknown => Action::Observe {
                target: DisposableWorkerObservationTarget::Runner,
            },
            ScaleSetRunnerObservation::Absent => Action::GenerateJitAndStartRunner,
            ScaleSetRunnerObservation::Idle => Action::Checkpoint {
                phase: Phase::Waiting,
            },
            state => record_job_state(input.attempt.phase, input.attempt.github_job_id, state)?,
        },
        Phase::Waiting | Phase::Assigned | Phase::Running if cleanup => Action::Checkpoint {
            phase: Phase::Destroying,
        },
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
            Action::Checkpoint {
                phase: Phase::Deregistering,
            }
        }
        Phase::Waiting | Phase::Assigned | Phase::Running => match input.runner {
            ScaleSetRunnerObservation::Unknown => Action::Observe {
                target: DisposableWorkerObservationTarget::Runner,
            },
            ScaleSetRunnerObservation::Absent => Action::Checkpoint {
                phase: Phase::Destroying,
            },
            ScaleSetRunnerObservation::Idle => Action::Wait,
            state => record_job_state(input.attempt.phase, input.attempt.github_job_id, state)?,
        },
        Phase::Terminal => Action::Checkpoint {
            phase: Phase::Destroying,
        },
        Phase::Destroying => match input.vm {
            ExactObjectObservation::Unknown => Action::Observe {
                target: DisposableWorkerObservationTarget::Vm,
            },
            ExactObjectObservation::Matching => Action::DestroyVm,
            ExactObjectObservation::Absent => Action::Checkpoint {
                phase: Phase::Deregistering,
            },
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
        Phase::Deregistering => match input.runner {
            ScaleSetRunnerObservation::Unknown => Action::Observe {
                target: DisposableWorkerObservationTarget::Runner,
            },
            ScaleSetRunnerObservation::Absent => Action::Checkpoint {
                phase: Phase::Releasing,
            },
            ScaleSetRunnerObservation::Idle
            | ScaleSetRunnerObservation::Assigned { .. }
            | ScaleSetRunnerObservation::Running { .. }
            | ScaleSetRunnerObservation::Terminal { .. } => Action::DeleteRunner,
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
        Phase::Releasing => Action::Checkpoint {
            phase: Phase::Complete,
        },
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

fn record_job_state(
    phase: DisposableAttemptPhase,
    expected: Option<GitHubJobId>,
    state: ScaleSetRunnerObservation,
) -> Result<DisposableWorkerAction, DisposableWorkerReconcilerError> {
    let action = match state {
        ScaleSetRunnerObservation::Assigned { github_job_id } => {
            DisposableWorkerAction::RecordAssigned { github_job_id }
        }
        ScaleSetRunnerObservation::Running { github_job_id } => {
            DisposableWorkerAction::RecordRunning { github_job_id }
        }
        ScaleSetRunnerObservation::Terminal {
            github_job_id,
            conclusion,
        } => DisposableWorkerAction::RecordTerminal {
            github_job_id,
            conclusion,
        },
        ScaleSetRunnerObservation::Conflicting => DisposableWorkerAction::Blocked {
            code: "conflicting_runner_identity",
        },
        ScaleSetRunnerObservation::Unknown
        | ScaleSetRunnerObservation::Absent
        | ScaleSetRunnerObservation::Idle => {
            return Err(DisposableWorkerReconcilerError::new(
                "runner",
                "runner_state_not_recordable",
                "runner state does not contain an assignable GitHub job",
            ));
        }
    };
    let observed = match action {
        DisposableWorkerAction::RecordAssigned { github_job_id }
        | DisposableWorkerAction::RecordRunning { github_job_id }
        | DisposableWorkerAction::RecordTerminal { github_job_id, .. } => Some(github_job_id),
        _ => None,
    };
    if let (Some(expected), Some(observed)) = (expected, observed)
        && expected != observed
    {
        return Err(DisposableWorkerReconcilerError::new(
            "github_job_id",
            "github_job_identity_drift",
            "runner observation changed the attempt's assigned GitHub job",
        ));
    }
    if matches!(
        (&action, phase),
        (
            DisposableWorkerAction::RecordAssigned { .. },
            DisposableAttemptPhase::Assigned | DisposableAttemptPhase::Running
        ) | (
            DisposableWorkerAction::RecordRunning { .. },
            DisposableAttemptPhase::Running
        )
    ) {
        return Ok(DisposableWorkerAction::Wait);
    }
    Ok(action)
}

fn validate_attempt(attempt: &DisposableAttempt) -> Result<(), DisposableWorkerReconcilerError> {
    if attempt.schema_version != DISPOSABLE_WORKER_RECONCILER_SCHEMA_VERSION {
        return Err(DisposableWorkerReconcilerError::new(
            "schema_version",
            "unsupported_schema_version",
            "disposable worker attempt schema is unsupported",
        ));
    }
    use DisposableAttemptPhase as Phase;
    let valid_shape = match attempt.phase {
        Phase::Reserved | Phase::Provisioning | Phase::Registering | Phase::Waiting => {
            attempt.github_job_id.is_none() && attempt.conclusion.is_none()
        }
        Phase::Assigned | Phase::Running => {
            attempt.github_job_id.is_some() && attempt.conclusion.is_none()
        }
        Phase::Terminal => attempt.github_job_id.is_some() && attempt.conclusion.is_some(),
        Phase::Destroying | Phase::Deregistering | Phase::Releasing | Phase::Complete => {
            attempt.conclusion.is_none() || attempt.github_job_id.is_some()
        }
    };
    if !valid_shape {
        return Err(DisposableWorkerReconcilerError::new(
            "attempt.phase",
            "invalid_attempt_shape",
            "attempt phase does not match its GitHub job evidence",
        ));
    }
    Ok(())
}

fn validate_transition(
    from: DisposableAttemptPhase,
    to: DisposableAttemptPhase,
) -> Result<(), DisposableWorkerReconcilerError> {
    use DisposableAttemptPhase as Phase;
    let valid = matches!(
        (from, to),
        (Phase::Reserved, Phase::Provisioning | Phase::Destroying)
            | (Phase::Provisioning, Phase::Registering | Phase::Destroying)
            | (
                Phase::Registering,
                Phase::Waiting
                    | Phase::Assigned
                    | Phase::Running
                    | Phase::Terminal
                    | Phase::Destroying
                    | Phase::Deregistering
            )
            | (
                Phase::Waiting,
                Phase::Assigned
                    | Phase::Running
                    | Phase::Terminal
                    | Phase::Destroying
                    | Phase::Deregistering
            )
            | (
                Phase::Assigned,
                Phase::Running | Phase::Terminal | Phase::Destroying | Phase::Deregistering
            )
            | (
                Phase::Running,
                Phase::Terminal | Phase::Destroying | Phase::Deregistering
            )
            | (Phase::Terminal, Phase::Destroying)
            | (Phase::Destroying, Phase::Deregistering)
            | (Phase::Deregistering, Phase::Releasing)
            | (Phase::Releasing, Phase::Complete)
    );
    if !valid {
        return Err(DisposableWorkerReconcilerError::new(
            "attempt.phase",
            "invalid_phase_transition",
            "disposable worker phase transition is not monotonic",
        ));
    }
    Ok(())
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
