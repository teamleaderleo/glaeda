use std::fmt;

use serde::Serialize;

use crate::execution_admission::{EpochMillis, ExecutionRequestId, HostCapacityObservation};
use crate::lima_lifecycle::{
    LimaInstanceIdentity, LimaLifecycleObservation, LimaLifecyclePolicy, LimaLifecycleState,
    LimaProfileGeneration, LimaResourceProfile,
};
use crate::mac_availability::{
    AvailabilityDisposition, AvailabilityRequest, HostPowerSource, JobActivity,
    MacAvailabilityObservation, MemoryPressure, ObservationFreshness, VmPowerState,
    plan_availability_transition,
};
use crate::personal_worker_host_broker::{
    HostBrokerAction, HostBrokerObservationTarget, HostBrokerReducerError, HostBrokerReducerInput,
    HostBrokerReducerPolicy, HostBrokerRunnerObservation, HostBrokerRunnerState,
    HostBrokerStateRevision,
};
use crate::personal_worker_queue::{
    PersonalWorkerProfile, PersonalWorkerQueueDecision, PersonalWorkerQueueEntryState,
    PersonalWorkerQueueGeneration, PersonalWorkerQueueVisibility, PersonalWorkerSelection,
};
use crate::personal_worker_store::PersonalWorkerStoreRevision;

pub const PERSONAL_WORKER_TICK_SCHEMA_VERSION: u8 = 1;
pub const MAX_PERSONAL_WORKER_TICK_OBSERVATION_AGE_MILLIS: u64 = 300_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerTickObservationTarget {
    Queue,
    Lima,
    Runner,
    Availability,
    Capacity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerTickBlocker {
    AvailabilityOff,
    AvailabilityAutoUnsupported,
    AvailabilityBlocked,
    CapacityInsufficient,
    ConcurrencyLimit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum PersonalWorkerTickJobStep {
    CancelPendingDownscale {
        target: PersonalWorkerProfile,
    },
    Reserve {
        selection: PersonalWorkerSelection,
        capacity: HostCapacityObservation,
    },
    Execute {
        request_id: ExecutionRequestId,
        // Queue visibility proves the reserved CPU and memory envelope only. This tick remains a
        // scheduling veto, not process-launch authority: B05 must reopen the exact store revision
        // and queue generation, then validate the sealed admission including its PID limit.
        capacity: HostCapacityObservation,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum PersonalWorkerTickDrainStep {
    Release { request_id: ExecutionRequestId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum PersonalWorkerTickAction {
    Satisfied,
    Blocked {
        blocker: PersonalWorkerTickBlocker,
    },
    Observe {
        target: PersonalWorkerTickObservationTarget,
    },
    ChangeProfile {
        identity: LimaInstanceIdentity,
        from_profile: LimaResourceProfile,
        to_profile: LimaResourceProfile,
        current_generation: LimaProfileGeneration,
        next_generation: LimaProfileGeneration,
    },
    StartVm {
        identity: LimaInstanceIdentity,
        profile: LimaResourceProfile,
        profile_generation: LimaProfileGeneration,
        capacity: HostCapacityObservation,
    },
    RunJob {
        job: PersonalWorkerTickJobStep,
    },
    Drain {
        drain: PersonalWorkerTickDrainStep,
    },
    StopVm {
        identity: LimaInstanceIdentity,
        current_profile: LimaResourceProfile,
        profile_generation: LimaProfileGeneration,
        target_after_stop: PersonalWorkerProfile,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonalWorkerTickInput<'a> {
    pub store_revision: PersonalWorkerStoreRevision,
    pub decision_at: EpochMillis,
    pub queue: &'a PersonalWorkerQueueDecision,
    pub lifecycle_policy: &'a LimaLifecyclePolicy,
    pub lifecycle: Option<&'a LimaLifecycleObservation>,
    pub runner: Option<&'a HostBrokerRunnerObservation>,
    pub capacity: Option<HostCapacityObservation>,
    pub availability_request: AvailabilityRequest,
    pub availability: Option<MacAvailabilityObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerTickPlan {
    schema_version: u8,
    store_revision: PersonalWorkerStoreRevision,
    queue_generation: PersonalWorkerQueueGeneration,
    decision_at: EpochMillis,
    action: PersonalWorkerTickAction,
}

impl PersonalWorkerTickPlan {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn store_revision(&self) -> PersonalWorkerStoreRevision {
        self.store_revision
    }

    #[must_use]
    pub const fn queue_generation(&self) -> PersonalWorkerQueueGeneration {
        self.queue_generation
    }

    #[must_use]
    pub const fn decision_at(&self) -> EpochMillis {
        self.decision_at
    }

    #[must_use]
    pub const fn action(&self) -> &PersonalWorkerTickAction {
        &self.action
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonalWorkerTickPolicy {
    host_broker: HostBrokerReducerPolicy,
    max_runner_age_millis: u64,
    max_capacity_age_millis: u64,
}

impl PersonalWorkerTickPolicy {
    /// Define bounded freshness windows for one pure worker tick.
    ///
    /// # Errors
    ///
    /// Returns a bounded error when any window is zero or exceeds the reviewed maximum.
    pub fn new(
        max_queue_age_millis: u64,
        max_runner_age_millis: u64,
        max_capacity_age_millis: u64,
    ) -> Result<Self, PersonalWorkerTickError> {
        let host_broker = HostBrokerReducerPolicy::new(max_queue_age_millis, max_runner_age_millis)
            .map_err(PersonalWorkerTickError::from_host_broker)?;
        if !(1..=MAX_PERSONAL_WORKER_TICK_OBSERVATION_AGE_MILLIS).contains(&max_capacity_age_millis)
        {
            return Err(PersonalWorkerTickError::new(
                PersonalWorkerTickErrorKind::Policy,
                "policy.max_capacity_age_millis",
                "invalid_capacity_freshness_window",
                "capacity freshness must be positive and within the reviewed maximum",
            ));
        }
        Ok(Self {
            host_broker,
            max_runner_age_millis,
            max_capacity_age_millis,
        })
    }

    /// Compose accepted queue, availability, lifecycle, capacity, readiness, and time evidence.
    ///
    /// This method is pure. It reads no clock, process, filesystem, network, credential, durable
    /// store, Lima state, or runner state and performs no mutation. The returned plan authorizes at
    /// most one bounded next action against the exact input revision and generation.
    ///
    /// # Errors
    ///
    /// Returns a bounded static error for contradictory identities, impossible cross-source state,
    /// future evidence, or invalid accepted foundation evidence.
    pub fn plan(
        &self,
        input: PersonalWorkerTickInput<'_>,
    ) -> Result<PersonalWorkerTickPlan, PersonalWorkerTickError> {
        self.validate_supplied_time(&input)?;
        let state_revision = HostBrokerStateRevision::new(input.store_revision.get())
            .map_err(PersonalWorkerTickError::from_host_broker)?;
        let host = self
            .host_broker
            .reduce(HostBrokerReducerInput {
                state_revision,
                decision_at: input.decision_at,
                queue: input.queue,
                lifecycle_policy: input.lifecycle_policy,
                lifecycle: input.lifecycle,
                runner: input.runner,
                previous: None,
            })
            .map_err(PersonalWorkerTickError::from_host_broker)?;

        if let HostBrokerAction::Observe { target } = host.action() {
            if *target == HostBrokerObservationTarget::Runner {
                validate_available_cross_source_evidence(&input)?;
            }
            return Ok(tick_plan(
                &input,
                PersonalWorkerTickAction::Observe {
                    target: map_observation_target(*target),
                },
            ));
        }
        if let HostBrokerAction::CancelDownscale { target } = host.action() {
            return Ok(tick_plan(
                &input,
                PersonalWorkerTickAction::RunJob {
                    job: PersonalWorkerTickJobStep::CancelPendingDownscale { target: *target },
                },
            ));
        }
        if let HostBrokerAction::Release { request_id } = host.action() {
            return Ok(tick_plan(
                &input,
                PersonalWorkerTickAction::Drain {
                    drain: PersonalWorkerTickDrainStep::Release {
                        request_id: request_id.clone(),
                    },
                },
            ));
        }
        if concurrency_exceeded(input.queue) {
            return Ok(tick_plan(
                &input,
                PersonalWorkerTickAction::Blocked {
                    blocker: PersonalWorkerTickBlocker::ConcurrencyLimit,
                },
            ));
        }

        let lifecycle = input.lifecycle.ok_or_else(|| {
            PersonalWorkerTickError::new(
                PersonalWorkerTickErrorKind::LifecycleEvidence,
                "lifecycle",
                "missing_lifecycle_after_broker_plan",
                "a consequential broker plan requires exact Lima lifecycle evidence",
            )
        })?;
        if let Some(action) = availability_gate(&input, lifecycle)? {
            return Ok(tick_plan(&input, action));
        }

        let action = match host.action() {
            HostBrokerAction::Observe { .. } => unreachable!("observation returned above"),
            HostBrokerAction::Start {
                identity,
                profile,
                profile_generation,
            } => match self.validated_profile_capacity(&input, *profile)? {
                CapacityEvidence::MissingOrStale => PersonalWorkerTickAction::Observe {
                    target: PersonalWorkerTickObservationTarget::Capacity,
                },
                CapacityEvidence::Insufficient => PersonalWorkerTickAction::Blocked {
                    blocker: PersonalWorkerTickBlocker::CapacityInsufficient,
                },
                CapacityEvidence::Fresh(capacity) => PersonalWorkerTickAction::StartVm {
                    identity: identity.clone(),
                    profile: *profile,
                    profile_generation: *profile_generation,
                    capacity,
                },
            },
            HostBrokerAction::Stop {
                identity,
                current_profile,
                profile_generation,
                target_after_stop,
            } => {
                return self.plan_stop(
                    &input,
                    identity,
                    *current_profile,
                    *profile_generation,
                    *target_after_stop,
                );
            }
            HostBrokerAction::ChangeProfile {
                identity,
                from_profile,
                to_profile,
                current_generation,
                next_generation,
            } => PersonalWorkerTickAction::ChangeProfile {
                identity: identity.clone(),
                from_profile: *from_profile,
                to_profile: *to_profile,
                current_generation: *current_generation,
                next_generation: *next_generation,
            },
            HostBrokerAction::WaitForRunner { .. } => PersonalWorkerTickAction::Observe {
                target: PersonalWorkerTickObservationTarget::Runner,
            },
            HostBrokerAction::Reserve { selection } => {
                if queue_has_active_work(input.queue) || input.queue.selected.len() != 1 {
                    PersonalWorkerTickAction::Blocked {
                        blocker: PersonalWorkerTickBlocker::ConcurrencyLimit,
                    }
                } else {
                    match self.validated_capacity(&input, selection)? {
                        CapacityEvidence::MissingOrStale => PersonalWorkerTickAction::Observe {
                            target: PersonalWorkerTickObservationTarget::Capacity,
                        },
                        CapacityEvidence::Insufficient => PersonalWorkerTickAction::Blocked {
                            blocker: PersonalWorkerTickBlocker::CapacityInsufficient,
                        },
                        CapacityEvidence::Fresh(capacity) => PersonalWorkerTickAction::RunJob {
                            job: PersonalWorkerTickJobStep::Reserve {
                                selection: selection.clone(),
                                capacity,
                            },
                        },
                    }
                }
            }
            HostBrokerAction::Release { .. } | HostBrokerAction::CancelDownscale { .. } => {
                unreachable!("protective durable step returned above")
            }
            HostBrokerAction::NoOp => self.no_op_action(&input, lifecycle)?,
        };
        Ok(tick_plan(&input, action))
    }

    fn plan_stop(
        &self,
        input: &PersonalWorkerTickInput<'_>,
        identity: &LimaInstanceIdentity,
        current_profile: LimaResourceProfile,
        profile_generation: LimaProfileGeneration,
        target_after_stop: PersonalWorkerProfile,
    ) -> Result<PersonalWorkerTickPlan, PersonalWorkerTickError> {
        let runner = match validated_runner_for_stop(self, input, identity, profile_generation)? {
            RunnerEvidence::MissingOrStale => {
                return Ok(tick_plan(
                    input,
                    PersonalWorkerTickAction::Observe {
                        target: PersonalWorkerTickObservationTarget::Runner,
                    },
                ));
            }
            RunnerEvidence::Fresh(runner) => runner,
        };
        let action = match runner.state() {
            HostBrokerRunnerState::Offline => PersonalWorkerTickAction::StopVm {
                identity: identity.clone(),
                current_profile,
                profile_generation,
                target_after_stop,
            },
            HostBrokerRunnerState::Starting => PersonalWorkerTickAction::Observe {
                target: PersonalWorkerTickObservationTarget::Runner,
            },
            HostBrokerRunnerState::IdleReady => PersonalWorkerTickAction::StopVm {
                identity: identity.clone(),
                current_profile,
                profile_generation,
                target_after_stop,
            },
            HostBrokerRunnerState::Busy { .. } | HostBrokerRunnerState::Draining { .. } => {
                return Err(PersonalWorkerTickError::new(
                    PersonalWorkerTickErrorKind::RunnerEvidence,
                    "runner.state",
                    "runner_active_without_queue_work",
                    "runner activity cannot authorize stop without matching active queue evidence",
                ));
            }
        };
        Ok(tick_plan(input, action))
    }

    fn validated_capacity(
        &self,
        input: &PersonalWorkerTickInput<'_>,
        selection: &PersonalWorkerSelection,
    ) -> Result<CapacityEvidence, PersonalWorkerTickError> {
        let Some(capacity) = input.capacity else {
            return Ok(CapacityEvidence::MissingOrStale);
        };
        if observation_is_stale(
            capacity.observed_at,
            input.decision_at,
            self.max_capacity_age_millis,
            PersonalWorkerTickErrorKind::CapacityEvidence,
            "capacity.observed_at",
        )? {
            return Ok(CapacityEvidence::MissingOrStale);
        }
        if !selection.reserved_limits.fits_within(capacity.capacity) {
            return Ok(CapacityEvidence::Insufficient);
        }
        Ok(CapacityEvidence::Fresh(capacity))
    }

    fn validated_profile_capacity(
        &self,
        input: &PersonalWorkerTickInput<'_>,
        profile: LimaResourceProfile,
    ) -> Result<CapacityEvidence, PersonalWorkerTickError> {
        let Some(capacity) = input.capacity else {
            return Ok(CapacityEvidence::MissingOrStale);
        };
        if observation_is_stale(
            capacity.observed_at,
            input.decision_at,
            self.max_capacity_age_millis,
            PersonalWorkerTickErrorKind::CapacityEvidence,
            "capacity.observed_at",
        )? {
            return Ok(CapacityEvidence::MissingOrStale);
        }
        let envelope = profile.envelope();
        let required_cpu_millis = u32::from(envelope.vcpus) * 1_000;
        if required_cpu_millis > capacity.capacity.cpu_millis
            || envelope.memory_bytes > capacity.capacity.memory_bytes
        {
            return Ok(CapacityEvidence::Insufficient);
        }
        Ok(CapacityEvidence::Fresh(capacity))
    }

    fn validated_execution_capacity(
        &self,
        input: &PersonalWorkerTickInput<'_>,
        entry: &PersonalWorkerQueueVisibility,
    ) -> Result<CapacityEvidence, PersonalWorkerTickError> {
        let Some(capacity) = input.capacity else {
            return Ok(CapacityEvidence::MissingOrStale);
        };
        if observation_is_stale(
            capacity.observed_at,
            input.decision_at,
            self.max_capacity_age_millis,
            PersonalWorkerTickErrorKind::CapacityEvidence,
            "capacity.observed_at",
        )? {
            return Ok(CapacityEvidence::MissingOrStale);
        }
        let (Some(reserved_cpu_millis), Some(reserved_memory_bytes)) =
            (entry.reserved_cpu_millis, entry.reserved_memory_bytes)
        else {
            return Err(PersonalWorkerTickError::new(
                PersonalWorkerTickErrorKind::HostBrokerEvidence,
                "queue.visibility.reserved_resources",
                "missing_reserved_resources",
                "reserved or starting work requires exact reserved resource evidence",
            ));
        };
        if reserved_cpu_millis > capacity.capacity.cpu_millis
            || reserved_memory_bytes > capacity.capacity.memory_bytes
        {
            return Ok(CapacityEvidence::Insufficient);
        }
        Ok(CapacityEvidence::Fresh(capacity))
    }

    fn validate_supplied_time(
        &self,
        input: &PersonalWorkerTickInput<'_>,
    ) -> Result<(), PersonalWorkerTickError> {
        if let Some(lifecycle) = input.lifecycle {
            let _ = observation_is_stale(
                lifecycle.observed_at(),
                input.decision_at,
                input.lifecycle_policy.max_observation_age_millis(),
                PersonalWorkerTickErrorKind::LifecycleEvidence,
                "lifecycle.observed_at",
            )?;
        }
        if let Some(runner) = input.runner {
            let _ = observation_is_stale(
                runner.observed_at(),
                input.decision_at,
                self.max_runner_age_millis,
                PersonalWorkerTickErrorKind::RunnerEvidence,
                "runner.observed_at",
            )?;
        }
        if let Some(capacity) = input.capacity {
            let _ = observation_is_stale(
                capacity.observed_at,
                input.decision_at,
                self.max_capacity_age_millis,
                PersonalWorkerTickErrorKind::CapacityEvidence,
                "capacity.observed_at",
            )?;
        }
        Ok(())
    }

    fn no_op_action(
        &self,
        input: &PersonalWorkerTickInput<'_>,
        lifecycle: &LimaLifecycleObservation,
    ) -> Result<PersonalWorkerTickAction, PersonalWorkerTickError> {
        if lifecycle.state() == LimaLifecycleState::Stopping {
            return Ok(PersonalWorkerTickAction::Observe {
                target: PersonalWorkerTickObservationTarget::Lima,
            });
        }
        if let Some(runner) = input.runner
            && matches!(
                runner.state(),
                HostBrokerRunnerState::Busy { .. } | HostBrokerRunnerState::Draining { .. }
            )
        {
            return Ok(PersonalWorkerTickAction::Observe {
                target: PersonalWorkerTickObservationTarget::Runner,
            });
        }
        if input.queue.visibility.iter().any(|entry| {
            matches!(
                entry.state,
                PersonalWorkerQueueEntryState::Starting | PersonalWorkerQueueEntryState::Running
            )
        }) {
            return Ok(PersonalWorkerTickAction::Observe {
                target: PersonalWorkerTickObservationTarget::Runner,
            });
        }
        if let Some(entry) = single_execution_candidate(input.queue)? {
            let capacity = match self.validated_execution_capacity(input, entry)? {
                CapacityEvidence::MissingOrStale => {
                    return Ok(PersonalWorkerTickAction::Observe {
                        target: PersonalWorkerTickObservationTarget::Capacity,
                    });
                }
                CapacityEvidence::Insufficient => {
                    return Ok(PersonalWorkerTickAction::Blocked {
                        blocker: PersonalWorkerTickBlocker::CapacityInsufficient,
                    });
                }
                CapacityEvidence::Fresh(capacity) => capacity,
            };
            return Ok(PersonalWorkerTickAction::RunJob {
                job: PersonalWorkerTickJobStep::Execute {
                    request_id: entry.request_id.clone(),
                    capacity,
                },
            });
        }
        Ok(PersonalWorkerTickAction::Satisfied)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerTickErrorKind {
    Policy,
    HostBrokerEvidence,
    AvailabilityEvidence,
    LifecycleEvidence,
    RunnerEvidence,
    CapacityEvidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerTickError {
    kind: PersonalWorkerTickErrorKind,
    field: &'static str,
    code: &'static str,
    message: &'static str,
}

impl PersonalWorkerTickError {
    const fn new(
        kind: PersonalWorkerTickErrorKind,
        field: &'static str,
        code: &'static str,
        message: &'static str,
    ) -> Self {
        Self {
            kind,
            field,
            code,
            message,
        }
    }

    const fn from_host_broker(error: HostBrokerReducerError) -> Self {
        Self::new(
            PersonalWorkerTickErrorKind::HostBrokerEvidence,
            error.field,
            error.code,
            error.message,
        )
    }

    #[must_use]
    pub const fn kind(self) -> PersonalWorkerTickErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn field(self) -> &'static str {
        self.field
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Display for PersonalWorkerTickError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for PersonalWorkerTickError {}

enum CapacityEvidence {
    MissingOrStale,
    Insufficient,
    Fresh(HostCapacityObservation),
}

enum RunnerEvidence<'a> {
    MissingOrStale,
    Fresh(&'a HostBrokerRunnerObservation),
}

fn availability_gate(
    input: &PersonalWorkerTickInput<'_>,
    lifecycle: &LimaLifecycleObservation,
) -> Result<Option<PersonalWorkerTickAction>, PersonalWorkerTickError> {
    let Some(availability) = input.availability else {
        return Ok(Some(PersonalWorkerTickAction::Observe {
            target: PersonalWorkerTickObservationTarget::Availability,
        }));
    };
    if availability.freshness == ObservationFreshness::Stale
        || availability.job_activity == JobActivity::Unknown
        || availability.vm_power == VmPowerState::Unknown
    {
        return Ok(Some(PersonalWorkerTickAction::Observe {
            target: PersonalWorkerTickObservationTarget::Availability,
        }));
    }

    validate_availability_coherence(input.queue, lifecycle, availability)?;
    if input.availability_request == AvailabilityRequest::Off && queue_has_work(input.queue) {
        return Ok(Some(PersonalWorkerTickAction::Blocked {
            blocker: PersonalWorkerTickBlocker::AvailabilityOff,
        }));
    }
    if queue_has_work(input.queue) {
        match availability.memory_pressure {
            MemoryPressure::Unknown => {
                return Ok(Some(PersonalWorkerTickAction::Observe {
                    target: PersonalWorkerTickObservationTarget::Availability,
                }));
            }
            MemoryPressure::Elevated | MemoryPressure::Critical => {
                return Ok(Some(PersonalWorkerTickAction::Blocked {
                    blocker: PersonalWorkerTickBlocker::AvailabilityBlocked,
                }));
            }
            MemoryPressure::Normal => {}
        }
        if input.availability_request == AvailabilityRequest::Away {
            match availability.host_power {
                HostPowerSource::Unknown => {
                    return Ok(Some(PersonalWorkerTickAction::Observe {
                        target: PersonalWorkerTickObservationTarget::Availability,
                    }));
                }
                HostPowerSource::Battery => {
                    return Ok(Some(PersonalWorkerTickAction::Blocked {
                        blocker: PersonalWorkerTickBlocker::AvailabilityBlocked,
                    }));
                }
                HostPowerSource::Ac => {}
            }
        }
    }

    let availability_plan = plan_availability_transition(availability, input.availability_request);
    let action = match availability_plan.disposition {
        AvailabilityDisposition::NoChange => None,
        // W03 does not execute the availability plan's free-standing transition sequence. A ready
        // active/away/off transition only removes the availability veto; the exact host-broker
        // action below remains the sole lifecycle authority for this tick.
        AvailabilityDisposition::Ready => None,
        AvailabilityDisposition::ManualPolicyRequired => Some(PersonalWorkerTickAction::Blocked {
            blocker: PersonalWorkerTickBlocker::AvailabilityAutoUnsupported,
        }),
        AvailabilityDisposition::Blocked => Some(PersonalWorkerTickAction::Blocked {
            blocker: PersonalWorkerTickBlocker::AvailabilityBlocked,
        }),
    };
    Ok(action)
}

fn validate_available_cross_source_evidence(
    input: &PersonalWorkerTickInput<'_>,
) -> Result<(), PersonalWorkerTickError> {
    let (Some(lifecycle), Some(availability)) = (input.lifecycle, input.availability) else {
        return Ok(());
    };
    if availability.freshness == ObservationFreshness::Fresh
        && availability.job_activity != JobActivity::Unknown
        && availability.vm_power != VmPowerState::Unknown
    {
        validate_availability_coherence(input.queue, lifecycle, availability)?;
    }
    Ok(())
}

fn validate_availability_coherence(
    queue: &PersonalWorkerQueueDecision,
    lifecycle: &LimaLifecycleObservation,
    availability: MacAvailabilityObservation,
) -> Result<(), PersonalWorkerTickError> {
    let expected_job_activity = if queue_has_active_work(queue) {
        JobActivity::Active
    } else {
        JobActivity::Idle
    };
    if availability.job_activity != expected_job_activity {
        return Err(PersonalWorkerTickError::new(
            PersonalWorkerTickErrorKind::AvailabilityEvidence,
            "availability.job_activity",
            "availability_queue_activity_mismatch",
            "availability job activity must match exact queue activity evidence",
        ));
    }
    let expected_vm_power = match lifecycle.state() {
        LimaLifecycleState::Stopped => VmPowerState::Stopped,
        LimaLifecycleState::Starting
        | LimaLifecycleState::Running
        | LimaLifecycleState::Draining
        | LimaLifecycleState::Stopping => VmPowerState::Running,
        LimaLifecycleState::Unavailable => {
            return Err(PersonalWorkerTickError::new(
                PersonalWorkerTickErrorKind::LifecycleEvidence,
                "lifecycle.state",
                "unavailable_lifecycle_in_availability_gate",
                "unavailable Lima evidence cannot authorize a worker tick",
            ));
        }
    };
    if availability.vm_power != expected_vm_power {
        return Err(PersonalWorkerTickError::new(
            PersonalWorkerTickErrorKind::AvailabilityEvidence,
            "availability.vm_power",
            "availability_lifecycle_power_mismatch",
            "availability VM power must match exact Lima lifecycle evidence",
        ));
    }
    Ok(())
}

fn validated_runner_for_stop<'a>(
    policy: &PersonalWorkerTickPolicy,
    input: &'a PersonalWorkerTickInput<'_>,
    identity: &LimaInstanceIdentity,
    profile_generation: LimaProfileGeneration,
) -> Result<RunnerEvidence<'a>, PersonalWorkerTickError> {
    let Some(runner) = input.runner else {
        return Ok(RunnerEvidence::MissingOrStale);
    };
    if observation_is_stale(
        runner.observed_at(),
        input.decision_at,
        policy.max_runner_age_millis,
        PersonalWorkerTickErrorKind::RunnerEvidence,
        "runner.observed_at",
    )? {
        return Ok(RunnerEvidence::MissingOrStale);
    }
    if runner.instance_id() != identity.instance_id() {
        return Err(PersonalWorkerTickError::new(
            PersonalWorkerTickErrorKind::RunnerEvidence,
            "runner.instance_id",
            "runner_instance_identity_mismatch",
            "runner readiness must bind the exact Lima instance identity",
        ));
    }
    if runner.profile_generation() != profile_generation {
        return Err(PersonalWorkerTickError::new(
            PersonalWorkerTickErrorKind::RunnerEvidence,
            "runner.profile_generation",
            "runner_profile_generation_mismatch",
            "runner readiness must bind the exact Lima profile generation",
        ));
    }
    Ok(RunnerEvidence::Fresh(runner))
}

fn observation_is_stale(
    observed_at: EpochMillis,
    decision_at: EpochMillis,
    max_age_millis: u64,
    error_kind: PersonalWorkerTickErrorKind,
    field: &'static str,
) -> Result<bool, PersonalWorkerTickError> {
    let age = decision_at
        .get()
        .checked_sub(observed_at.get())
        .ok_or_else(|| {
            PersonalWorkerTickError::new(
                error_kind,
                field,
                "future_observation",
                "worker tick evidence cannot be newer than the decision time",
            )
        })?;
    Ok(age > max_age_millis)
}

fn single_execution_candidate(
    queue: &PersonalWorkerQueueDecision,
) -> Result<Option<&PersonalWorkerQueueVisibility>, PersonalWorkerTickError> {
    let mut candidates = queue
        .visibility
        .iter()
        .filter(|entry| entry.state == PersonalWorkerQueueEntryState::Reserved);
    let candidate = candidates.next();
    if candidates.next().is_some() {
        return Err(PersonalWorkerTickError::new(
            PersonalWorkerTickErrorKind::HostBrokerEvidence,
            "queue.visibility",
            "multiple_execution_candidates",
            "one worker tick cannot select more than one reserved request",
        ));
    }
    Ok(candidate)
}

fn queue_has_active_work(queue: &PersonalWorkerQueueDecision) -> bool {
    queue.visibility.iter().any(|entry| {
        matches!(
            entry.state,
            PersonalWorkerQueueEntryState::Reserved
                | PersonalWorkerQueueEntryState::Starting
                | PersonalWorkerQueueEntryState::Running
                | PersonalWorkerQueueEntryState::Draining
        )
    })
}

fn queue_has_work(queue: &PersonalWorkerQueueDecision) -> bool {
    !queue.selected.is_empty()
        || queue
            .visibility
            .iter()
            .any(|entry| entry.state != PersonalWorkerQueueEntryState::Cancelled)
}

fn concurrency_exceeded(queue: &PersonalWorkerQueueDecision) -> bool {
    let active_count = queue
        .visibility
        .iter()
        .filter(|entry| {
            matches!(
                entry.state,
                PersonalWorkerQueueEntryState::Reserved
                    | PersonalWorkerQueueEntryState::Starting
                    | PersonalWorkerQueueEntryState::Running
                    | PersonalWorkerQueueEntryState::Draining
            )
        })
        .take(2)
        .count();
    queue.selected.len() > 1 || active_count > 1
}

const fn map_observation_target(
    target: HostBrokerObservationTarget,
) -> PersonalWorkerTickObservationTarget {
    match target {
        HostBrokerObservationTarget::Queue => PersonalWorkerTickObservationTarget::Queue,
        HostBrokerObservationTarget::Lima => PersonalWorkerTickObservationTarget::Lima,
        HostBrokerObservationTarget::Runner => PersonalWorkerTickObservationTarget::Runner,
    }
}

fn tick_plan(
    input: &PersonalWorkerTickInput<'_>,
    action: PersonalWorkerTickAction,
) -> PersonalWorkerTickPlan {
    PersonalWorkerTickPlan {
        schema_version: PERSONAL_WORKER_TICK_SCHEMA_VERSION,
        store_revision: input.store_revision,
        queue_generation: input.queue.generation,
        decision_at: input.decision_at,
        action,
    }
}
