use std::fmt;

use serde::{Deserialize, Serialize};

use crate::disposable_worker_reconciler::{
    CapacityClaimId, DisposableAttemptId, DisposableAttemptPhase, DisposableVmId,
    DisposableVmIdentity,
};
use crate::execution_admission::EpochMillis;
use crate::github_scale_set_protocol::{
    ScaleSetJobId, ScaleSetJobResult, ScaleSetRunnerId, ScaleSetRunnerName, ScaleSetRunnerReference,
};

pub const DISPOSABLE_ATTEMPT_STATE_SCHEMA_VERSION: u8 = 6;
pub const MAX_DISPOSABLE_ATTEMPT_STATE_BYTES: usize = 16_384;
const MAX_DISPOSABLE_ATTEMPT_REVISION: u64 = 1_000_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct DisposableAttemptRevision(u64);

impl DisposableAttemptRevision {
    /// Construct one bounded positive durable-attempt revision.
    ///
    /// # Errors
    ///
    /// Returns an error for zero or a value beyond the reviewed revision space.
    pub fn new(value: u64) -> Result<Self, DisposableAttemptStateError> {
        if !(1..=MAX_DISPOSABLE_ATTEMPT_REVISION).contains(&value) {
            return Err(DisposableAttemptStateError::new(
                "revision",
                "invalid_revision",
                "disposable attempt revision is outside the bounded positive range",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    fn next(self) -> Result<Self, DisposableAttemptStateError> {
        let value = self.0.checked_add(1).ok_or_else(|| {
            DisposableAttemptStateError::new(
                "revision",
                "revision_conflict",
                "disposable attempt revision cannot advance",
            )
        })?;
        Self::new(value).map_err(|_| {
            DisposableAttemptStateError::new(
                "revision",
                "revision_conflict",
                "disposable attempt revision cannot advance",
            )
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisposableAttemptState {
    schema_version: u8,
    revision: DisposableAttemptRevision,
    attempt_id: DisposableAttemptId,
    capacity_claim_id: CapacityClaimId,
    vm_id: DisposableVmId,
    vm_identity: Option<DisposableVmIdentity>,
    runner_name: ScaleSetRunnerName,
    runner_id: Option<ScaleSetRunnerId>,
    runner_start_started: bool,
    phase: DisposableAttemptPhase,
    github_job_id: Option<ScaleSetJobId>,
    result: Option<ScaleSetJobResult>,
    not_after: EpochMillis,
}

impl DisposableAttemptState {
    /// Construct one reserved attempt before any external worker or runner mutation.
    #[must_use]
    pub fn reserved(
        attempt_id: DisposableAttemptId,
        capacity_claim_id: CapacityClaimId,
        vm_id: DisposableVmId,
        runner_name: ScaleSetRunnerName,
        not_after: EpochMillis,
    ) -> Self {
        Self {
            schema_version: DISPOSABLE_ATTEMPT_STATE_SCHEMA_VERSION,
            revision: DisposableAttemptRevision(1),
            attempt_id,
            capacity_claim_id,
            vm_id,
            vm_identity: None,
            runner_name,
            runner_id: None,
            runner_start_started: false,
            phase: DisposableAttemptPhase::Reserved,
            github_job_id: None,
            result: None,
            not_after,
        }
    }

    /// Construct one reserved attempt already bound to the exact Scale Set job offered for it.
    ///
    /// Binding the job at reservation time lets an assigned job that is cancelled before runner
    /// startup release capacity without inventing VM cleanup authority. The claim and job remain
    /// immutable for the lifetime of the attempt.
    #[must_use]
    pub(crate) fn reserved_for_job(
        attempt_id: DisposableAttemptId,
        capacity_claim_id: CapacityClaimId,
        vm_id: DisposableVmId,
        runner_name: ScaleSetRunnerName,
        job_id: ScaleSetJobId,
        not_after: EpochMillis,
    ) -> Self {
        let mut state =
            Self::reserved(attempt_id, capacity_claim_id, vm_id, runner_name, not_after);
        state.github_job_id = Some(job_id);
        state
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn revision(&self) -> DisposableAttemptRevision {
        self.revision
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

    /// Return the exact observed host identity bound to this clone, when one has been persisted.
    #[must_use]
    pub const fn vm_identity(&self) -> Option<&DisposableVmIdentity> {
        self.vm_identity.as_ref()
    }

    #[must_use]
    pub const fn runner_name(&self) -> &ScaleSetRunnerName {
        &self.runner_name
    }

    #[must_use]
    pub const fn runner_id(&self) -> Option<ScaleSetRunnerId> {
        self.runner_id
    }

    #[must_use]
    pub const fn runner_start_started(&self) -> bool {
        self.runner_start_started
    }

    #[must_use]
    pub const fn phase(&self) -> DisposableAttemptPhase {
        self.phase
    }

    #[must_use]
    pub const fn github_job_id(&self) -> Option<&ScaleSetJobId> {
        self.github_job_id.as_ref()
    }

    #[must_use]
    pub const fn result(&self) -> Option<&ScaleSetJobResult> {
        self.result.as_ref()
    }

    #[must_use]
    pub const fn not_after(&self) -> EpochMillis {
        self.not_after
    }

    pub(crate) fn is_exact_successor_of(&self, current: &Self) -> bool {
        let matches = |candidate: Result<Self, DisposableAttemptStateError>| {
            candidate.as_ref().is_ok_and(|candidate| candidate == self)
        };
        if matches(current.authorize_clone())
            || matches(current.record_clone_started())
            || matches(current.begin_unprovisioned_release())
            || matches(current.complete_unprovisioned())
            || matches(current.begin_registration())
            || matches(current.record_runner_start_started())
            || matches(current.begin_cleanup())
            || matches(current.advance_cleanup(self.phase))
        {
            return true;
        }

        let runner = self
            .runner_id
            .map(|id| ScaleSetRunnerReference::new(id, self.runner_name.clone()));
        if let Some(runner) = runner.as_ref()
            && (matches(current.record_registration(runner))
                || matches(current.record_runner_ready(runner)))
        {
            return true;
        }
        if let Some(job_id) = self.github_job_id.as_ref() {
            if matches(current.record_assigned(job_id.clone())) {
                return true;
            }
            if let Some(runner) = runner.as_ref()
                && matches(current.record_running(runner, job_id.clone()))
            {
                return true;
            }
            if let Some(result) = self.result.as_ref()
                && (matches(current.record_terminal(None, job_id.clone(), result.clone()))
                    || runner.as_ref().is_some_and(|runner| {
                        matches(current.record_terminal(
                            Some(runner),
                            job_id.clone(),
                            result.clone(),
                        ))
                    }))
            {
                return true;
            }
        }
        false
    }

    pub fn begin_provisioning(&self) -> Result<Self, DisposableAttemptStateError> {
        Err(invalid_transition(
            "legacy provisioning cannot advance a current-schema attempt",
        ))
    }

    /// Persist the observed-absent decision that permits one attempt-bound clone operation.
    pub fn authorize_clone(&self) -> Result<Self, DisposableAttemptStateError> {
        self.advance_phase(
            DisposableAttemptPhase::Reserved,
            DisposableAttemptPhase::CloneAuthorized,
        )
    }

    /// Persist the no-replay checkpoint immediately before the first clone command.
    pub fn record_clone_started(&self) -> Result<Self, DisposableAttemptStateError> {
        self.advance_phase(
            DisposableAttemptPhase::CloneAuthorized,
            DisposableAttemptPhase::CloneStarted,
        )
    }

    /// Bind the first VM identity after the fixed clone command succeeds.
    ///
    /// This transition is intentionally crate-private and is excluded from the generic catalog
    /// action vocabulary. Only the same-lock clone runtime may use it.
    pub(crate) fn bind_vm_identity_after_clone(
        &self,
        identity: DisposableVmIdentity,
    ) -> Result<Self, DisposableAttemptStateError> {
        if self.phase != DisposableAttemptPhase::CloneStarted
            || self.revision.get() != 3
            || self.vm_identity.is_some()
        {
            return Err(invalid_transition(
                "VM identity can bind only to the first clone-started checkpoint",
            ));
        }
        let mut next = self.clone();
        next.revision = self.revision.next()?;
        next.vm_identity = Some(identity);
        next.validate()?;
        Ok(next)
    }

    /// Persist cancellation/expiry before releasing capacity for an unprovisioned attempt.
    pub fn begin_unprovisioned_release(&self) -> Result<Self, DisposableAttemptStateError> {
        match self.phase {
            DisposableAttemptPhase::Reserved | DisposableAttemptPhase::CloneAuthorized => self
                .advance_with(
                    DisposableAttemptPhase::UnprovisionedReleasing,
                    None,
                    self.github_job_id.clone(),
                    self.result.clone(),
                ),
            _ => Err(invalid_transition(
                "only an attempt that has not started cloning may release without VM cleanup",
            )),
        }
    }

    /// Complete an attempt that released capacity before any VM clone was started.
    ///
    /// This path deliberately skips VM and runner cleanup. The attempt may have proved VM absence,
    /// but it has not crossed the clone-start checkpoint and owns no external object to delete.
    pub fn complete_unprovisioned(&self) -> Result<Self, DisposableAttemptStateError> {
        match self.phase {
            DisposableAttemptPhase::Reserved
            | DisposableAttemptPhase::CloneAuthorized
            | DisposableAttemptPhase::UnprovisionedReleasing => self.advance_with(
                DisposableAttemptPhase::Complete,
                None,
                self.github_job_id.clone(),
                self.result.clone(),
            ),
            _ => Err(invalid_transition(
                "only an unprovisioned attempt may complete without cleanup",
            )),
        }
    }

    pub fn begin_registration(&self) -> Result<Self, DisposableAttemptStateError> {
        self.advance_phase(
            DisposableAttemptPhase::CloneStarted,
            DisposableAttemptPhase::Registering,
        )
    }

    /// Record the exact GitHub registration returned or rediscovered after JIT creation.
    ///
    /// This deliberately stays in the current phase: an existing registration does not prove that
    /// the guest listener is alive or that the one-time JIT configuration remains usable. During
    /// cleanup, binding a rediscovered service ID is the durable prerequisite for exact deletion.
    pub fn record_registration(
        &self,
        runner: &ScaleSetRunnerReference,
    ) -> Result<Self, DisposableAttemptStateError> {
        if !matches!(
            self.phase,
            DisposableAttemptPhase::Registering
                | DisposableAttemptPhase::Assigned
                | DisposableAttemptPhase::Destroying
                | DisposableAttemptPhase::Deregistering
                | DisposableAttemptPhase::Releasing
        ) {
            return Err(invalid_transition(
                "runner registration can only be recorded while registering or cleaning up",
            ));
        }
        let runner_id = self.validate_runner(runner)?;
        if self.runner_id == Some(runner_id) {
            return Ok(self.clone());
        }
        self.advance_with(
            self.phase,
            Some(runner_id),
            self.github_job_id.clone(),
            self.result.clone(),
        )
    }

    /// Consume the exact registered runner's one-time JIT launch authority before execution.
    pub fn record_runner_start_started(&self) -> Result<Self, DisposableAttemptStateError> {
        if !matches!(
            self.phase,
            DisposableAttemptPhase::Registering | DisposableAttemptPhase::Assigned
        ) || self.runner_id.is_none()
        {
            return Err(invalid_transition(
                "runner start requires one exact durable registration",
            ));
        }
        if self.runner_start_started {
            return Ok(self.clone());
        }
        let mut next =
            self.advance_with(self.phase, self.runner_id, self.github_job_id.clone(), None)?;
        next.runner_start_started = true;
        next.validate()?;
        Ok(next)
    }

    /// Record independent evidence that the exact registered runner listener is alive and ready.
    pub fn record_runner_ready(
        &self,
        runner: &ScaleSetRunnerReference,
    ) -> Result<Self, DisposableAttemptStateError> {
        let runner_id = self.validate_runner(runner)?;
        if !self.runner_start_started {
            return Err(invalid_transition(
                "runner readiness requires the durable runner-start checkpoint",
            ));
        }
        match self.phase {
            DisposableAttemptPhase::Registering => self.advance_with(
                DisposableAttemptPhase::Waiting,
                Some(runner_id),
                self.github_job_id.clone(),
                None,
            ),
            DisposableAttemptPhase::Assigned if self.runner_id.is_none() => self.advance_with(
                DisposableAttemptPhase::Assigned,
                Some(runner_id),
                self.github_job_id.clone(),
                None,
            ),
            DisposableAttemptPhase::Waiting
            | DisposableAttemptPhase::Assigned
            | DisposableAttemptPhase::Running
            | DisposableAttemptPhase::Terminal
                if self.runner_id == Some(runner_id) =>
            {
                Ok(self.clone())
            }
            _ => Err(invalid_transition(
                "runner readiness cannot advance the current attempt phase",
            )),
        }
    }

    /// Record the Scale Set job identity without treating assignment as runner/job binding.
    pub fn record_assigned(
        &self,
        job_id: ScaleSetJobId,
    ) -> Result<Self, DisposableAttemptStateError> {
        self.validate_job(&job_id)?;
        match self.phase {
            DisposableAttemptPhase::Reserved
            | DisposableAttemptPhase::CloneAuthorized
            | DisposableAttemptPhase::UnprovisionedReleasing
                if self.github_job_id.as_ref() == Some(&job_id) =>
            {
                Ok(self.clone())
            }
            DisposableAttemptPhase::Registering | DisposableAttemptPhase::Waiting => self
                .advance_with(
                    DisposableAttemptPhase::Assigned,
                    self.runner_id,
                    Some(job_id),
                    None,
                ),
            DisposableAttemptPhase::Assigned
            | DisposableAttemptPhase::Running
            | DisposableAttemptPhase::Terminal
                if self.github_job_id.as_ref() == Some(&job_id) =>
            {
                Ok(self.clone())
            }
            _ => Err(invalid_transition(
                "job assignment cannot advance the current attempt phase",
            )),
        }
    }

    /// Bind the actual job to the exact runner only when a started observation identifies both.
    pub fn record_running(
        &self,
        runner: &ScaleSetRunnerReference,
        job_id: ScaleSetJobId,
    ) -> Result<Self, DisposableAttemptStateError> {
        let runner_id = self.validate_runner(runner)?;
        if !self.runner_start_started {
            return Err(invalid_transition(
                "job start requires the durable runner-start checkpoint",
            ));
        }
        self.validate_job(&job_id)?;
        match self.phase {
            DisposableAttemptPhase::Registering
            | DisposableAttemptPhase::Waiting
            | DisposableAttemptPhase::Assigned => self.advance_with(
                DisposableAttemptPhase::Running,
                Some(runner_id),
                Some(job_id),
                None,
            ),
            DisposableAttemptPhase::Running | DisposableAttemptPhase::Terminal
                if self.runner_id == Some(runner_id)
                    && self.github_job_id.as_ref() == Some(&job_id) =>
            {
                Ok(self.clone())
            }
            DisposableAttemptPhase::Destroying
            | DisposableAttemptPhase::Deregistering
            | DisposableAttemptPhase::Releasing
            | DisposableAttemptPhase::Complete => {
                if self.github_job_id.is_none() && !self.has_clone_started_history() {
                    return Err(invalid_transition(
                        "late job evidence requires a durable clone-start checkpoint",
                    ));
                }
                if self.result.is_some() {
                    if self.runner_id == Some(runner_id)
                        && self.github_job_id.as_ref() == Some(&job_id)
                    {
                        return Ok(self.clone());
                    }
                    return Err(identity_drift(
                        "late job start conflicts with terminal cleanup evidence",
                    ));
                }
                if self.runner_id == Some(runner_id) && self.github_job_id.as_ref() == Some(&job_id)
                {
                    return Ok(self.clone());
                }
                self.advance_with(self.phase, Some(runner_id), Some(job_id), None)
            }
            _ => Err(invalid_transition(
                "job start cannot advance the current attempt phase",
            )),
        }
    }

    /// Record terminal demand for the exact job.
    ///
    /// Ordinarily the attempt must already have crossed the durable clone-start checkpoint.
    /// Before that checkpoint, the sole accepted terminal shape is an exact runnerless cancellation
    /// for the job prebound when Scale Set capacity was reserved; it releases capacity without
    /// creating VM cleanup authority. After clone start, `runner` may be absent only when another
    /// exact event has already bound the job. Together these conditions prevent a late service
    /// event from manufacturing cleanup authority or being attributed to arbitrary capacity.
    pub fn record_terminal(
        &self,
        runner: Option<&ScaleSetRunnerReference>,
        job_id: ScaleSetJobId,
        result: ScaleSetJobResult,
    ) -> Result<Self, DisposableAttemptStateError> {
        let observed_runner_id = runner
            .map(|value| self.validate_runner(value))
            .transpose()?;
        self.validate_job(&job_id)?;
        if matches!(
            self.phase,
            DisposableAttemptPhase::Reserved
                | DisposableAttemptPhase::CloneAuthorized
                | DisposableAttemptPhase::UnprovisionedReleasing
        ) {
            if self.github_job_id.is_none() {
                return Err(invalid_transition(
                    "terminal evidence cannot advance the current attempt phase",
                ));
            }
            if observed_runner_id.is_some()
                || self.github_job_id.as_ref() != Some(&job_id)
                || result.as_str() != "canceled"
            {
                return Err(identity_drift(
                    "pre-clone completion must be the exact runnerless canceled job",
                ));
            }
            if self.result.as_ref() == Some(&result) {
                return Ok(self.clone());
            }
            if self.result.is_some() {
                return Err(identity_drift(
                    "terminal evidence conflicts with the bound disposable attempt",
                ));
            }
            return self.advance_with(
                DisposableAttemptPhase::UnprovisionedReleasing,
                None,
                Some(job_id),
                Some(result),
            );
        }
        if observed_runner_id.is_none() && self.github_job_id.is_none() {
            return Err(identity_drift(
                "runnerless terminal evidence requires an already-bound job identity",
            ));
        }
        if !self.runner_start_started
            && (observed_runner_id.is_some() || result.as_str() != "canceled")
        {
            return Err(invalid_transition(
                "pre-start terminal evidence must be runnerless cancellation",
            ));
        }
        if let Some(current_result) = self.result.as_ref() {
            if self.github_job_id.as_ref() == Some(&job_id)
                && current_result == &result
                && observed_runner_id.is_none_or(|id| self.runner_id == Some(id))
            {
                return Ok(self.clone());
            }
            return Err(identity_drift(
                "terminal evidence conflicts with the bound disposable attempt",
            ));
        }
        if self.phase == DisposableAttemptPhase::Terminal {
            return Err(identity_drift(
                "terminal attempt is missing its validated result",
            ));
        }
        let cleanup = matches!(
            self.phase,
            DisposableAttemptPhase::Destroying
                | DisposableAttemptPhase::Deregistering
                | DisposableAttemptPhase::Releasing
                | DisposableAttemptPhase::Complete
        );
        if cleanup && self.github_job_id.is_none() && !self.has_clone_started_history() {
            return Err(invalid_transition(
                "late terminal evidence requires a durable clone-start checkpoint",
            ));
        }
        if !cleanup
            && !matches!(
                self.phase,
                DisposableAttemptPhase::CloneStarted
                    | DisposableAttemptPhase::Registering
                    | DisposableAttemptPhase::Waiting
                    | DisposableAttemptPhase::Assigned
                    | DisposableAttemptPhase::Running
            )
        {
            return Err(invalid_transition(
                "terminal evidence cannot advance the current attempt phase",
            ));
        }
        let runner_id = match (self.runner_id, observed_runner_id) {
            (Some(current), Some(observed)) if current != observed => {
                return Err(identity_drift(
                    "terminal runner identity differs from the bound runner",
                ));
            }
            (Some(current), _) => Some(current),
            (None, observed) => observed,
        };
        self.advance_with(
            if cleanup {
                self.phase
            } else {
                DisposableAttemptPhase::Terminal
            },
            runner_id,
            Some(job_id),
            Some(result),
        )
    }

    pub fn begin_cleanup(&self) -> Result<Self, DisposableAttemptStateError> {
        match self.phase {
            DisposableAttemptPhase::Reserved
            | DisposableAttemptPhase::CloneAuthorized
            | DisposableAttemptPhase::UnprovisionedReleasing
            | DisposableAttemptPhase::Provisioning => Err(invalid_transition(
                "cleanup requires durable clone or later external-object authority",
            )),
            DisposableAttemptPhase::CloneStarted
            | DisposableAttemptPhase::Registering
            | DisposableAttemptPhase::Waiting
            | DisposableAttemptPhase::Assigned
            | DisposableAttemptPhase::Running
            | DisposableAttemptPhase::Terminal => self.advance_with(
                DisposableAttemptPhase::Destroying,
                self.runner_id,
                self.github_job_id.clone(),
                self.result.clone(),
            ),
            DisposableAttemptPhase::Destroying
            | DisposableAttemptPhase::Deregistering
            | DisposableAttemptPhase::Releasing
            | DisposableAttemptPhase::Complete => Ok(self.clone()),
        }
    }

    pub fn advance_cleanup(
        &self,
        phase: DisposableAttemptPhase,
    ) -> Result<Self, DisposableAttemptStateError> {
        if self.phase == phase {
            return Ok(self.clone());
        }
        let valid = matches!(
            (self.phase, phase),
            (
                DisposableAttemptPhase::Destroying,
                DisposableAttemptPhase::Deregistering
            ) | (
                DisposableAttemptPhase::Deregistering,
                DisposableAttemptPhase::Releasing
            ) | (
                DisposableAttemptPhase::Releasing,
                DisposableAttemptPhase::Complete
            )
        );
        if !valid {
            return Err(invalid_transition(
                "cleanup phase does not follow the durable attempt order",
            ));
        }
        self.advance_with(
            phase,
            self.runner_id,
            self.github_job_id.clone(),
            self.result.clone(),
        )
    }

    fn advance_phase(
        &self,
        expected: DisposableAttemptPhase,
        phase: DisposableAttemptPhase,
    ) -> Result<Self, DisposableAttemptStateError> {
        if self.phase != expected {
            return Err(invalid_transition(
                "attempt phase does not match the required predecessor",
            ));
        }
        self.advance_with(
            phase,
            self.runner_id,
            self.github_job_id.clone(),
            self.result.clone(),
        )
    }

    fn has_clone_started_history(&self) -> bool {
        let revision = self.revision.get();
        match self.phase {
            DisposableAttemptPhase::CloneStarted
            | DisposableAttemptPhase::Registering
            | DisposableAttemptPhase::Waiting
            | DisposableAttemptPhase::Assigned
            | DisposableAttemptPhase::Running
            | DisposableAttemptPhase::Terminal => true,
            DisposableAttemptPhase::Destroying => revision >= 5,
            DisposableAttemptPhase::Deregistering => revision >= 6,
            DisposableAttemptPhase::Releasing => revision >= 7,
            DisposableAttemptPhase::Complete => revision >= 8,
            DisposableAttemptPhase::Reserved
            | DisposableAttemptPhase::UnprovisionedReleasing
            | DisposableAttemptPhase::Provisioning
            | DisposableAttemptPhase::CloneAuthorized => false,
        }
    }

    fn advance_with(
        &self,
        phase: DisposableAttemptPhase,
        runner_id: Option<ScaleSetRunnerId>,
        github_job_id: Option<ScaleSetJobId>,
        result: Option<ScaleSetJobResult>,
    ) -> Result<Self, DisposableAttemptStateError> {
        let next = Self {
            schema_version: self.schema_version,
            revision: self.revision.next()?,
            attempt_id: self.attempt_id.clone(),
            capacity_claim_id: self.capacity_claim_id.clone(),
            vm_id: self.vm_id.clone(),
            vm_identity: self.vm_identity.clone(),
            runner_name: self.runner_name.clone(),
            runner_id,
            runner_start_started: self.runner_start_started,
            phase,
            github_job_id,
            result,
            not_after: self.not_after,
        };
        next.validate()?;
        Ok(next)
    }

    fn validate_runner(
        &self,
        runner: &ScaleSetRunnerReference,
    ) -> Result<ScaleSetRunnerId, DisposableAttemptStateError> {
        if runner.name != self.runner_name {
            return Err(identity_drift(
                "observed runner name differs from the prechosen durable runner name",
            ));
        }
        if self.runner_id.is_some_and(|current| current != runner.id) {
            return Err(identity_drift(
                "observed runner ID differs from the durable runner ID",
            ));
        }
        Ok(runner.id)
    }

    fn validate_job(&self, job_id: &ScaleSetJobId) -> Result<(), DisposableAttemptStateError> {
        if self
            .github_job_id
            .as_ref()
            .is_some_and(|current| current != job_id)
        {
            return Err(identity_drift(
                "observed Scale Set job ID differs from the durable job ID",
            ));
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), DisposableAttemptStateError> {
        if self.schema_version != DISPOSABLE_ATTEMPT_STATE_SCHEMA_VERSION {
            return Err(DisposableAttemptStateError::new(
                "schema_version",
                "version_incompatible",
                "disposable attempt schema version is unsupported",
            ));
        }
        if self.result.is_some() && self.github_job_id.is_none() {
            return Err(invalid_document(
                "terminal result requires an exact Scale Set job identity",
            ));
        }
        if self.runner_start_started && (self.vm_identity.is_none() || self.runner_id.is_none()) {
            return Err(invalid_document(
                "runner-start checkpoint requires exact VM and runner identities",
            ));
        }
        if self
            .result
            .as_ref()
            .is_some_and(|result| !self.runner_start_started && result.as_str() != "canceled")
        {
            return Err(invalid_document(
                "pre-start terminal evidence must be cancellation",
            ));
        }
        let revision = self.revision.get();
        let revision_shape_valid = match self.phase {
            DisposableAttemptPhase::Reserved => revision == 1,
            DisposableAttemptPhase::UnprovisionedReleasing => matches!(revision, 2..=4),
            DisposableAttemptPhase::CloneAuthorized => revision == 2,
            DisposableAttemptPhase::CloneStarted => matches!(revision, 3 | 4),
            DisposableAttemptPhase::Provisioning => false,
            DisposableAttemptPhase::Registering => revision >= 5,
            DisposableAttemptPhase::Waiting
            | DisposableAttemptPhase::Assigned
            | DisposableAttemptPhase::Running => revision >= 6,
            DisposableAttemptPhase::Terminal => revision >= 5,
            DisposableAttemptPhase::Destroying => revision >= 5,
            DisposableAttemptPhase::Deregistering => revision >= 6,
            DisposableAttemptPhase::Releasing => revision >= 7,
            DisposableAttemptPhase::Complete => matches!(revision, 2..=5) || revision >= 8,
        };
        if !revision_shape_valid {
            return Err(invalid_document(
                "attempt phase conflicts with its durable revision history",
            ));
        }
        match self.phase {
            DisposableAttemptPhase::Reserved | DisposableAttemptPhase::CloneAuthorized => {
                if self.vm_identity.is_some() || self.runner_id.is_some() || self.result.is_some() {
                    return Err(invalid_document(
                        "pre-clone attempt cannot carry VM, runner, or terminal evidence",
                    ));
                }
            }
            DisposableAttemptPhase::UnprovisionedReleasing => {
                if self.vm_identity.is_some()
                    || self.runner_id.is_some()
                    || !valid_unprovisioned_result(
                        self.github_job_id.as_ref(),
                        self.result.as_ref(),
                    )
                {
                    return Err(invalid_document(
                        "unprovisioned release carries invalid external evidence",
                    ));
                }
            }
            DisposableAttemptPhase::CloneStarted => {
                if (revision == 3) != self.vm_identity.is_none()
                    || self.runner_id.is_some()
                    || self.result.is_some()
                {
                    return Err(invalid_document(
                        "clone-started attempt has inconsistent VM, runner, or job evidence",
                    ));
                }
            }
            DisposableAttemptPhase::Provisioning => {
                return Err(invalid_document(
                    "legacy provisioning phase is unsupported by the current schema",
                ));
            }
            DisposableAttemptPhase::Registering => {
                if self.vm_identity.is_none() || self.result.is_some() {
                    return Err(invalid_document(
                        "registering attempt requires VM identity and no terminal evidence",
                    ));
                }
            }
            DisposableAttemptPhase::Waiting => {
                if self.vm_identity.is_none()
                    || self.runner_id.is_none()
                    || !self.runner_start_started
                    || self.result.is_some()
                {
                    return Err(invalid_document(
                        "waiting attempt requires exact VM and runner identities without a result",
                    ));
                }
            }
            DisposableAttemptPhase::Assigned => {
                if self.vm_identity.is_none()
                    || self.github_job_id.is_none()
                    || self.result.is_some()
                {
                    return Err(invalid_document(
                        "assigned attempt requires exact VM and job identities without terminal result",
                    ));
                }
            }
            DisposableAttemptPhase::Running => {
                if self.vm_identity.is_none()
                    || self.runner_id.is_none()
                    || self.github_job_id.is_none()
                    || !self.runner_start_started
                    || self.result.is_some()
                {
                    return Err(invalid_document(
                        "running attempt requires exact VM, runner, and job identities",
                    ));
                }
            }
            DisposableAttemptPhase::Terminal => {
                if self.vm_identity.is_none()
                    || self.github_job_id.is_none()
                    || self.result.is_none()
                {
                    return Err(invalid_document(
                        "terminal attempt requires exact VM and job identities and result",
                    ));
                }
            }
            DisposableAttemptPhase::Destroying
            | DisposableAttemptPhase::Deregistering
            | DisposableAttemptPhase::Releasing => {
                if self.vm_identity.is_none() {
                    return Err(invalid_document(
                        "provisioned cleanup requires exact durable VM identity",
                    ));
                }
            }
            DisposableAttemptPhase::Complete => {}
        }
        if self.phase == DisposableAttemptPhase::Complete {
            if self.vm_identity.is_none() {
                if revision > 5
                    || self.runner_id.is_some()
                    || (revision == 5 && self.result.is_none())
                    || !valid_unprovisioned_result(
                        self.github_job_id.as_ref(),
                        self.result.as_ref(),
                    )
                {
                    return Err(invalid_document(
                        "unprovisioned completion carries invalid external evidence",
                    ));
                }
            } else if revision < 8 {
                return Err(invalid_document(
                    "provisioned completion conflicts with its cleanup history",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct AttemptWire<'a> {
    schema_version: u8,
    revision: u64,
    attempt_id: &'a str,
    capacity_claim_id: &'a str,
    vm_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    vm_identity_digest: Option<&'a str>,
    runner_name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    runner_id: Option<u64>,
    runner_start_started: bool,
    phase: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    github_job_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<&'a str>,
    not_after: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnedAttemptWire {
    schema_version: u8,
    revision: u64,
    attempt_id: String,
    capacity_claim_id: String,
    vm_id: String,
    vm_identity_digest: Option<String>,
    runner_name: String,
    runner_id: Option<u64>,
    runner_start_started: bool,
    phase: String,
    github_job_id: Option<String>,
    result: Option<String>,
    not_after: u64,
}

/// Encode one bounded canonical durable attempt document.
///
/// # Errors
///
/// Returns an error if the state is invalid or its canonical JSON exceeds the reviewed bound.
pub fn encode_disposable_attempt_state(
    state: &DisposableAttemptState,
) -> Result<Vec<u8>, DisposableAttemptStateError> {
    state.validate()?;
    let wire = AttemptWire {
        schema_version: state.schema_version,
        revision: state.revision.get(),
        attempt_id: state.attempt_id.as_str(),
        capacity_claim_id: state.capacity_claim_id.as_str(),
        vm_id: state.vm_id.as_str(),
        vm_identity_digest: state.vm_identity.as_ref().map(DisposableVmIdentity::as_str),
        runner_name: state.runner_name.as_str(),
        runner_id: state.runner_id.map(ScaleSetRunnerId::get),
        runner_start_started: state.runner_start_started,
        phase: phase_name(state.phase),
        github_job_id: state.github_job_id.as_ref().map(ScaleSetJobId::as_str),
        result: state.result.as_ref().map(ScaleSetJobResult::as_str),
        not_after: state.not_after.get(),
    };
    let encoded =
        serde_json::to_vec(&wire).map_err(|_| invalid_document("attempt cannot encode"))?;
    if encoded.len() > MAX_DISPOSABLE_ATTEMPT_STATE_BYTES {
        return Err(DisposableAttemptStateError::new(
            "document",
            "document_too_large",
            "disposable attempt document exceeds the reviewed byte bound",
        ));
    }
    Ok(encoded)
}

/// Decode and fully validate one bounded durable attempt document.
///
/// # Errors
///
/// Returns an error for malformed, oversized, future-version, or internally inconsistent state.
pub fn decode_disposable_attempt_state(
    bytes: &[u8],
) -> Result<DisposableAttemptState, DisposableAttemptStateError> {
    if bytes.len() > MAX_DISPOSABLE_ATTEMPT_STATE_BYTES {
        return Err(DisposableAttemptStateError::new(
            "document",
            "document_too_large",
            "disposable attempt document exceeds the reviewed byte bound",
        ));
    }
    let wire: OwnedAttemptWire =
        serde_json::from_slice(bytes).map_err(|_| invalid_document("attempt JSON is invalid"))?;
    if wire.schema_version != DISPOSABLE_ATTEMPT_STATE_SCHEMA_VERSION {
        return Err(DisposableAttemptStateError::new(
            "schema_version",
            "version_incompatible",
            "disposable attempt schema version is unsupported",
        ));
    }
    let state = DisposableAttemptState {
        schema_version: wire.schema_version,
        revision: DisposableAttemptRevision::new(wire.revision)?,
        attempt_id: DisposableAttemptId::parse(&wire.attempt_id)
            .map_err(|_| invalid_document("attempt ID is invalid"))?,
        capacity_claim_id: CapacityClaimId::parse(&wire.capacity_claim_id)
            .map_err(|_| invalid_document("capacity claim ID is invalid"))?,
        vm_id: DisposableVmId::parse(&wire.vm_id)
            .map_err(|_| invalid_document("VM ID is invalid"))?,
        vm_identity: wire
            .vm_identity_digest
            .map(|value| DisposableVmIdentity::parse(&value))
            .transpose()
            .map_err(|_| invalid_document("VM identity digest is invalid"))?,
        runner_name: ScaleSetRunnerName::parse(&wire.runner_name)
            .map_err(|_| invalid_document("runner name is invalid"))?,
        runner_id: wire
            .runner_id
            .map(ScaleSetRunnerId::new)
            .transpose()
            .map_err(|_| invalid_document("runner ID is invalid"))?,
        runner_start_started: wire.runner_start_started,
        phase: parse_phase(&wire.phase)?,
        github_job_id: wire
            .github_job_id
            .map(|value| ScaleSetJobId::parse(&value))
            .transpose()
            .map_err(|_| invalid_document("Scale Set job ID is invalid"))?,
        result: wire
            .result
            .map(|value| ScaleSetJobResult::parse(&value))
            .transpose()
            .map_err(|_| invalid_document("Scale Set job result is invalid"))?,
        not_after: EpochMillis::new(wire.not_after)
            .map_err(|_| invalid_document("attempt deadline is invalid"))?,
    };
    state.validate()?;
    Ok(state)
}

const fn phase_name(phase: DisposableAttemptPhase) -> &'static str {
    match phase {
        DisposableAttemptPhase::Reserved => "reserved",
        DisposableAttemptPhase::UnprovisionedReleasing => "unprovisioned_releasing",
        DisposableAttemptPhase::Provisioning => "provisioning",
        DisposableAttemptPhase::CloneAuthorized => "clone_authorized",
        DisposableAttemptPhase::CloneStarted => "clone_started",
        DisposableAttemptPhase::Registering => "registering",
        DisposableAttemptPhase::Waiting => "waiting",
        DisposableAttemptPhase::Assigned => "assigned",
        DisposableAttemptPhase::Running => "running",
        DisposableAttemptPhase::Terminal => "terminal",
        DisposableAttemptPhase::Destroying => "destroying",
        DisposableAttemptPhase::Deregistering => "deregistering",
        DisposableAttemptPhase::Releasing => "releasing",
        DisposableAttemptPhase::Complete => "complete",
    }
}

fn parse_phase(value: &str) -> Result<DisposableAttemptPhase, DisposableAttemptStateError> {
    match value {
        "reserved" => Ok(DisposableAttemptPhase::Reserved),
        "unprovisioned_releasing" => Ok(DisposableAttemptPhase::UnprovisionedReleasing),
        "provisioning" => Ok(DisposableAttemptPhase::Provisioning),
        "clone_authorized" => Ok(DisposableAttemptPhase::CloneAuthorized),
        "clone_started" => Ok(DisposableAttemptPhase::CloneStarted),
        "registering" => Ok(DisposableAttemptPhase::Registering),
        "waiting" => Ok(DisposableAttemptPhase::Waiting),
        "assigned" => Ok(DisposableAttemptPhase::Assigned),
        "running" => Ok(DisposableAttemptPhase::Running),
        "terminal" => Ok(DisposableAttemptPhase::Terminal),
        "destroying" => Ok(DisposableAttemptPhase::Destroying),
        "deregistering" => Ok(DisposableAttemptPhase::Deregistering),
        "releasing" => Ok(DisposableAttemptPhase::Releasing),
        "complete" => Ok(DisposableAttemptPhase::Complete),
        _ => Err(invalid_document("attempt phase is invalid")),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DisposableAttemptStateError {
    field: &'static str,
    code: &'static str,
    message: &'static str,
}

impl DisposableAttemptStateError {
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

impl fmt::Display for DisposableAttemptStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl std::error::Error for DisposableAttemptStateError {}

fn valid_unprovisioned_result(
    job_id: Option<&ScaleSetJobId>,
    result: Option<&ScaleSetJobResult>,
) -> bool {
    result.is_none()
        || (job_id.is_some() && result.is_some_and(|value| value.as_str() == "canceled"))
}

const fn invalid_document(message: &'static str) -> DisposableAttemptStateError {
    DisposableAttemptStateError::new("document", "invalid_document", message)
}

const fn invalid_transition(message: &'static str) -> DisposableAttemptStateError {
    DisposableAttemptStateError::new("phase", "invalid_transition", message)
}

const fn identity_drift(message: &'static str) -> DisposableAttemptStateError {
    DisposableAttemptStateError::new("identity", "identity_drift", message)
}
