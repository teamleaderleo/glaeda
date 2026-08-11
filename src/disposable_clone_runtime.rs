//! Same-lock execution boundary for one disposable Lima clone.
//!
//! The injected transaction tests choose only an already-reserved attempt. Current durable state,
//! live capacity/cancellation evidence, time, prepared-source readiness, target absence, the fixed
//! command, its bounded process cleanup, and post-clone identity remain inside the canonical store
//! lock. The Scale Set coordinator supplies one live zero-capacity poll at the final admission
//! barrier; a message is persisted instead of checkpointing, while idle authority is consumed by
//! the immediately following durable checkpoint and fixed command.

use std::fmt;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;

use crate::artifact::Sha256Digest;
use crate::disposable_attempt_catalog::{
    DisposableAttemptCatalogDocument, DisposableAttemptCatalogRevision,
    DisposableAttemptReservation,
};
use crate::disposable_attempt_state::DisposableAttemptRevision;
use crate::disposable_lima_worker::{
    DisposableLimaWorkerAdapter, DisposableLimaWorkerCommandKind, DisposableLimaWorkerCommandPlan,
};
use crate::disposable_prepared_template::{
    DisposablePreparedTemplateIdentity, current_disposable_prepared_template,
};
use crate::disposable_template_generation::DisposableTemplateGenerationDocument;
use crate::disposable_template_runtime::DisposableTemplateRuntime;
use crate::disposable_worker_reconciler::{
    CapacityClaimId, DisposableAttemptId, DisposableAttemptPhase, DisposableVmId,
    DisposableVmIdentity, DisposableVmObservation, DisposableWorkerAction,
    DisposableWorkerReconcileInput, DisposableWorkerResources, ScaleSetRunnerObservation,
    reconcile_attempt,
};
use crate::execution_admission::EpochMillis;
use crate::github_scale_set_bridge::{ScaleSetBridgeIdentity, ScaleSetBridgePoll};
use crate::lima_host_identity::LimaHostIdentityAdapter;
use crate::lima_observation::{
    LimaInstanceName, LimaObservationAdapter, LimaObservationClock, LimaObservationRefusalCode,
    LimaObservationRequest,
};
use crate::process::{CommandExecutor, CommandSpec, ExecutionRecord, TimedCommandExecutor};
#[cfg(test)]
use crate::unix_personal_worker_store::UnixPersonalWorkerStore;

pub const DISPOSABLE_CLONE_RUNTIME_SCHEMA_VERSION: u8 = 1;
const OBSERVATION_TIMEOUT: Duration = Duration::from_secs(30);
const CLONE_ADMISSION_MAX_MILLIS: u64 = 30_000;

/// One live capacity/cancellation observation bound to the exact durable reservation.
pub struct DisposableCloneAdmissionObservation {
    catalog_revision: DisposableAttemptCatalogRevision,
    attempt_id: DisposableAttemptId,
    attempt_revision: DisposableAttemptRevision,
    capacity_claim_id: CapacityClaimId,
    vm_id: DisposableVmId,
    resources: DisposableWorkerResources,
    prepared_template_identity: DisposablePreparedTemplateIdentity,
    observed_at: EpochMillis,
    expires_at: EpochMillis,
    capacity_reserved: bool,
    cancellation_requested: bool,
}

impl DisposableCloneAdmissionObservation {
    pub(crate) fn new(
        catalog: &DisposableAttemptCatalogDocument,
        reservation: &DisposableAttemptReservation,
        observed_at: EpochMillis,
        expires_at: EpochMillis,
        capacity_reserved: bool,
        cancellation_requested: bool,
    ) -> Self {
        Self {
            catalog_revision: catalog.revision(),
            attempt_id: reservation.attempt().attempt_id().clone(),
            attempt_revision: reservation.attempt().revision(),
            capacity_claim_id: reservation.attempt().capacity_claim_id().clone(),
            vm_id: reservation.attempt().vm_id().clone(),
            resources: reservation.resources(),
            prepared_template_identity: reservation.prepared_template_identity().clone(),
            observed_at,
            expires_at,
            capacity_reserved,
            cancellation_requested,
        }
    }

    fn validate_for(
        &self,
        catalog: &DisposableAttemptCatalogDocument,
        reservation: &DisposableAttemptReservation,
        now: EpochMillis,
    ) -> Result<(), DisposableCloneRuntimeError> {
        if self.catalog_revision != catalog.revision()
            || &self.attempt_id != reservation.attempt().attempt_id()
            || self.attempt_revision != reservation.attempt().revision()
            || &self.capacity_claim_id != reservation.attempt().capacity_claim_id()
            || &self.vm_id != reservation.attempt().vm_id()
            || self.resources != reservation.resources()
            || &self.prepared_template_identity != reservation.prepared_template_identity()
        {
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_admission_identity_drift",
            ));
        }
        if self.observed_at > now
            || now > self.expires_at
            || self
                .expires_at
                .get()
                .checked_sub(self.observed_at.get())
                .is_none_or(|window| window > CLONE_ADMISSION_MAX_MILLIS)
        {
            return Err(observation("clone_admission_stale"));
        }
        if !self.capacity_reserved {
            return Err(DisposableCloneRuntimeError::recovery("clone_capacity_lost"));
        }
        if self.cancellation_requested {
            return Err(DisposableCloneRuntimeError::recovery("clone_cancelled"));
        }
        Ok(())
    }
}

/// Live source for current capacity ownership and cancellation state.
///
/// The clone transaction invokes the sealed production implementation while holding the canonical
/// store lock after host preflight and before the durable start checkpoint. Any returned message is
/// durably published before control returns; only a bounded idle result can reach the checkpoint.
pub(crate) mod admission_seal {
    pub trait Sealed {}
}

pub(crate) struct PendingCloneScaleSetMessage {
    pub(crate) source_identity: ScaleSetBridgeIdentity,
    pub(crate) response: ScaleSetBridgePoll,
    pub(crate) observed_at: EpochMillis,
    pub(crate) not_after: EpochMillis,
}

pub(crate) trait DisposableCloneAdmissionSource: admission_seal::Sealed {
    fn scale_set_source_identity(&self) -> Option<&ScaleSetBridgeIdentity> {
        None
    }

    fn observe(
        &self,
        catalog: &DisposableAttemptCatalogDocument,
        reservation: &DisposableAttemptReservation,
    ) -> Result<DisposableCloneAdmissionObservation, DisposableCloneRuntimeError>;

    fn take_pending_scale_set_message(&self) -> Option<PendingCloneScaleSetMessage> {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableCloneRuntimeErrorKind {
    InvalidConfiguration,
    DurableState,
    Observation,
    Command,
    RecoveryRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DisposableCloneRuntimeError {
    kind: DisposableCloneRuntimeErrorKind,
    code: &'static str,
    message: &'static str,
}

impl DisposableCloneRuntimeError {
    #[must_use]
    pub const fn kind(&self) -> DisposableCloneRuntimeErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }

    pub(crate) const fn durable(code: &'static str) -> Self {
        runtime_error(
            DisposableCloneRuntimeErrorKind::DurableState,
            code,
            "durable disposable-clone state is unavailable or inconsistent",
        )
    }

    pub(crate) const fn recovery(code: &'static str) -> Self {
        runtime_error(
            DisposableCloneRuntimeErrorKind::RecoveryRequired,
            code,
            "the disposable clone requires recovery before it can advance",
        )
    }

    pub(crate) const fn observation(code: &'static str) -> Self {
        observation(code)
    }
}

impl fmt::Display for DisposableCloneRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for DisposableCloneRuntimeError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DisposableCloneRuntimeReceipt {
    schema_version: u8,
    attempt_id: String,
    catalog_revision: u64,
    attempt_revision: u64,
    command_identity: Sha256Digest,
}

pub(crate) enum DisposableCloneTransactionOutcome {
    CloneAuthorized { attempt_id: String },
    Completed(DisposableCloneRuntimeReceipt),
    ScaleSetMessagePersisted { message_id: u32 },
}

impl DisposableCloneRuntimeReceipt {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub fn attempt_id(&self) -> &str {
        &self.attempt_id
    }

    #[must_use]
    pub const fn catalog_revision(&self) -> u64 {
        self.catalog_revision
    }

    #[must_use]
    pub const fn attempt_revision(&self) -> u64 {
        self.attempt_revision
    }

    #[must_use]
    pub const fn command_identity(&self) -> &Sha256Digest {
        &self.command_identity
    }
}

/// Fixed production runtime for one controller-owned prepared source template.
pub struct DisposableCloneRuntime {
    #[cfg(test)]
    state_root: PathBuf,
    limactl_program: PathBuf,
    template_runtime: DisposableTemplateRuntime,
    worker: DisposableLimaWorkerAdapter,
}

impl fmt::Debug for DisposableCloneRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableCloneRuntime")
            .field("state_root", &"<private-state-root>")
            .field("limactl_program", &"<private-program-path>")
            .field("worker", &self.worker)
            .finish()
    }
}

impl DisposableCloneRuntime {
    /// Construct the fixed runtime around the checked-in prepared-template declaration.
    pub fn new(
        state_root: impl Into<PathBuf>,
        limactl_program: impl Into<PathBuf>,
        lima_home: impl Into<PathBuf>,
        source_instance: LimaInstanceName,
    ) -> Result<Self, DisposableCloneRuntimeError> {
        let state_root = state_root.into();
        let limactl_program = limactl_program.into();
        let lima_home = lima_home.into();
        let prepared = current_disposable_prepared_template()
            .map_err(|_| invalid_configuration("clone_prepared_template_invalid"))?;
        let template_runtime = DisposableTemplateRuntime::new(
            state_root.clone(),
            limactl_program.clone(),
            lima_home.clone(),
            source_instance.clone(),
        )
        .map_err(|_| invalid_configuration("clone_template_runtime_invalid"))?;
        let worker = DisposableLimaWorkerAdapter::new(
            limactl_program.clone(),
            lima_home,
            source_instance,
            &prepared,
        )
        .map_err(|_| invalid_configuration("clone_worker_runtime_invalid"))?;
        Ok(Self {
            #[cfg(test)]
            state_root,
            limactl_program,
            template_runtime,
            worker,
        })
    }

    /// Exercise the future transaction with injected admission, process, and clock sources.
    #[cfg(test)]
    pub(crate) fn clone_once_with(
        &self,
        attempt_id: &DisposableAttemptId,
        admission: &impl DisposableCloneAdmissionSource,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<DisposableCloneRuntimeReceipt, DisposableCloneRuntimeError> {
        let mut store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(&self.state_root)
                .map_err(|_| DisposableCloneRuntimeError::durable("clone_catalog_unavailable"))?;
        match store
            .execute_disposable_clone_transaction(self, attempt_id, admission, executor, clock)?
        {
            DisposableCloneTransactionOutcome::Completed(receipt) => Ok(receipt),
            DisposableCloneTransactionOutcome::CloneAuthorized { .. } => Err(
                DisposableCloneRuntimeError::recovery("clone_authorization_only"),
            ),
            DisposableCloneTransactionOutcome::ScaleSetMessagePersisted { .. } => Err(
                DisposableCloneRuntimeError::recovery("clone_scale_set_message_persisted"),
            ),
        }
    }

    pub(crate) fn authorize_locked(
        &self,
        catalog: &DisposableAttemptCatalogDocument,
        attempt_id: &DisposableAttemptId,
        admission: &impl DisposableCloneAdmissionSource,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<(), DisposableCloneRuntimeError> {
        let reservation = catalog
            .find_active(attempt_id)
            .ok_or_else(|| DisposableCloneRuntimeError::durable("clone_attempt_missing"))?;
        if reservation.attempt().phase() != DisposableAttemptPhase::Reserved {
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_authorization_phase_mismatch",
            ));
        }
        let prepared_template_identity = current_disposable_prepared_template()
            .and_then(|manifest| manifest.identity())
            .map_err(|_| invalid_configuration("clone_prepared_template_invalid"))?;
        if reservation.prepared_template_identity() != &prepared_template_identity {
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_prepared_template_drift",
            ));
        }
        let target_request = self
            .worker
            .target_observation_request(reservation)
            .map_err(|_| invalid_configuration("clone_target_request_invalid"))?;
        self.verify_limactl(executor)?;
        self.confirm_target_absent(&target_request, executor, clock)?;

        let admission_observation = admission.observe(catalog, reservation)?;
        let now = clock
            .epoch_millis()
            .map_err(|_| observation("clone_clock_unavailable"))?;
        admission_observation.validate_for(catalog, reservation, now)?;

        self.confirm_target_absent(&target_request, executor, clock)?;
        self.verify_limactl(executor)?;
        let authorized_at = clock
            .epoch_millis()
            .map_err(|_| observation("clone_clock_unavailable"))?;
        admission_observation.validate_for(catalog, reservation, authorized_at)?;
        if authorized_at > reservation.attempt().not_after() {
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_expired_before_authorization",
            ));
        }
        let action = reconcile_attempt(DisposableWorkerReconcileInput {
            now: authorized_at,
            attempt: reservation.attempt(),
            vm: DisposableVmObservation::Absent,
            vm_identity: None,
            runner: ScaleSetRunnerObservation::Absent,
            job_event: None,
            capacity_reserved: admission_observation.capacity_reserved,
            cancellation_requested: admission_observation.cancellation_requested,
        })
        .map_err(|_| DisposableCloneRuntimeError::recovery("clone_reconcile_failed"))?;
        if action != (DisposableWorkerAction::Persist {
            transition:
                crate::disposable_attempt_catalog::DisposableAttemptCatalogAction::AuthorizeClone,
        }) {
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_authorization_refused",
            ));
        }
        Ok(())
    }

    pub(crate) fn prepare_locked(
        &self,
        catalog: &DisposableAttemptCatalogDocument,
        generation: &DisposableTemplateGenerationDocument,
        attempt_id: &DisposableAttemptId,
        admission: &impl DisposableCloneAdmissionSource,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<PreparedClone, DisposableCloneRuntimeError> {
        let reservation = catalog
            .find_active(attempt_id)
            .ok_or_else(|| DisposableCloneRuntimeError::durable("clone_attempt_missing"))?;
        let source = self
            .template_runtime
            .confirm_stopped_clone_source(generation, executor, clock)
            .map_err(|_| observation("clone_source_not_ready"))?;
        let target_request = self
            .worker
            .target_observation_request(reservation)
            .map_err(|_| invalid_configuration("clone_target_request_invalid"))?;
        self.verify_limactl(executor)?;
        self.confirm_target_absent(&target_request, executor, clock)?;
        source
            .confirm_current()
            .map_err(|_| observation("clone_source_drift"))?;
        let fresh_source = self
            .template_runtime
            .confirm_stopped_clone_source(generation, executor, clock)
            .map_err(|_| observation("clone_source_drift"))?;
        self.confirm_target_absent(&target_request, executor, clock)?;
        fresh_source
            .confirm_current()
            .map_err(|_| observation("clone_source_drift"))?;
        let admission_observation = admission.observe(catalog, reservation)?;
        let now = clock
            .epoch_millis()
            .map_err(|_| observation("clone_clock_unavailable"))?;
        admission_observation.validate_for(catalog, reservation, now)?;
        self.confirm_target_absent(&target_request, executor, clock)?;
        source
            .confirm_current()
            .map_err(|_| observation("clone_source_drift"))?;
        fresh_source
            .confirm_current()
            .map_err(|_| observation("clone_source_drift"))?;
        self.verify_limactl(executor)?;
        let command_now = clock
            .epoch_millis()
            .map_err(|_| observation("clone_clock_unavailable"))?;
        admission_observation.validate_for(catalog, reservation, command_now)?;
        if command_now > reservation.attempt().not_after() {
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_expired_before_command",
            ));
        }
        let action = reconcile_attempt(DisposableWorkerReconcileInput {
            now: command_now,
            attempt: reservation.attempt(),
            vm: DisposableVmObservation::Absent,
            vm_identity: None,
            runner: ScaleSetRunnerObservation::Absent,
            job_event: None,
            capacity_reserved: admission_observation.capacity_reserved,
            cancellation_requested: admission_observation.cancellation_requested,
        })
        .map_err(|_| DisposableCloneRuntimeError::recovery("clone_reconcile_failed"))?;
        if action != DisposableWorkerAction::CheckpointAndCloneVm {
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_no_longer_authorized",
            ));
        }
        let plan = self
            .worker
            .plan(command_now, reservation, &action)
            .map_err(|_| DisposableCloneRuntimeError::recovery("clone_plan_refused"))?;
        if plan.kind() != DisposableLimaWorkerCommandKind::Clone {
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_plan_kind_mismatch",
            ));
        }
        Ok(PreparedClone {
            plan,
            target_request,
            generation: generation.clone(),
            admission_expires_at: admission_observation.expires_at,
        })
    }

    pub(crate) fn execute_locked(
        &self,
        started: &DisposableAttemptCatalogDocument,
        attempt_id: &DisposableAttemptId,
        prepared: &PreparedClone,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<DisposableVmIdentity, DisposableCloneRuntimeError> {
        let reservation = started
            .find_active(attempt_id)
            .ok_or_else(|| DisposableCloneRuntimeError::durable("clone_attempt_missing"))?;
        let attempt = reservation.attempt();
        if attempt.phase() != DisposableAttemptPhase::CloneStarted
            || attempt.revision().get() != prepared.plan.attempt_revision() + 1
            || attempt.attempt_id().as_str() != prepared.plan.attempt_id()
            || attempt.vm_id().as_str() != prepared.plan.vm_id()
            || reservation.prepared_template_identity()
                != prepared.plan.prepared_template_identity()
            || attempt.vm_identity().is_some()
        {
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_started_checkpoint_mismatch",
            ));
        }
        let now = clock
            .epoch_millis()
            .map_err(|_| observation("clone_clock_unavailable"))?;
        if now > attempt.not_after() || now > prepared.admission_expires_at {
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_expired_before_command",
            ));
        }

        let record = executor
            .execute_with_timeout(prepared.plan.command(), prepared.plan.timeout())
            .map_err(|_| command("clone_command_failed"))?;
        validate_record(prepared.plan.command(), &record)?;
        let host = LimaHostIdentityAdapter
            .observe(&prepared.target_request)
            .map_err(|_| observation("clone_host_identity_unavailable"))?;
        if host.root_disk_bytes() != reservation.resources().disk_bytes() {
            return Err(observation("clone_host_disk_mismatch"));
        }
        host.confirm(&prepared.target_request)
            .map_err(|_| observation("clone_host_identity_drift"))?;
        self.template_runtime
            .confirm_stopped_clone_source(&prepared.generation, executor, clock)
            .map_err(|_| observation("clone_source_drift"))?;
        host.confirm(&prepared.target_request)
            .map_err(|_| observation("clone_host_identity_drift"))?;
        Ok(DisposableVmIdentity::from_host_identity(host.identity()))
    }

    pub(crate) fn receipt(
        &self,
        bound: &DisposableAttemptCatalogDocument,
        attempt_id: &DisposableAttemptId,
        plan: &DisposableLimaWorkerCommandPlan,
    ) -> Result<DisposableCloneRuntimeReceipt, DisposableCloneRuntimeError> {
        let attempt = bound
            .find_active(attempt_id)
            .ok_or_else(|| DisposableCloneRuntimeError::durable("clone_attempt_missing"))?
            .attempt();
        if attempt.phase() != DisposableAttemptPhase::CloneStarted
            || attempt.vm_identity().is_none()
        {
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_identity_not_bound",
            ));
        }
        Ok(DisposableCloneRuntimeReceipt {
            schema_version: DISPOSABLE_CLONE_RUNTIME_SCHEMA_VERSION,
            attempt_id: attempt_id.as_str().to_owned(),
            catalog_revision: bound.revision().get(),
            attempt_revision: attempt.revision().get(),
            command_identity: plan.command_identity().clone(),
        })
    }

    /// Reconfirm the exact running clone and its realized isolation policy before JIT handoff.
    pub(crate) fn confirm_ready_worker(
        &self,
        reservation: &DisposableAttemptReservation,
        executor: &impl TimedCommandExecutor,
        clock: &impl CloneRuntimeClock,
    ) -> Result<ConfirmedDisposableWorker, DisposableCloneRuntimeError> {
        let attempt = reservation.attempt();
        if !matches!(
            attempt.phase(),
            DisposableAttemptPhase::Registering | DisposableAttemptPhase::Assigned
        ) {
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_worker_phase_mismatch",
            ));
        }
        let expected_identity = attempt.vm_identity().ok_or_else(|| {
            DisposableCloneRuntimeError::recovery("clone_worker_identity_missing")
        })?;
        let prepared_template_identity = current_disposable_prepared_template()
            .and_then(|manifest| manifest.identity())
            .map_err(|_| invalid_configuration("clone_prepared_template_invalid"))?;
        if reservation.prepared_template_identity() != &prepared_template_identity {
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_prepared_template_drift",
            ));
        }
        let request = self
            .worker
            .target_observation_request(reservation)
            .map_err(|_| invalid_configuration("clone_target_request_invalid"))?;
        let host = self
            .template_runtime
            .confirm_running_clone_target(&request, reservation.resources(), executor, clock)
            .map_err(|_| observation("clone_worker_not_ready"))?;
        if &DisposableVmIdentity::from_host_identity(host.identity()) != expected_identity {
            return Err(DisposableCloneRuntimeError::recovery(
                "clone_worker_identity_drift",
            ));
        }
        host.confirm(&request)
            .map_err(|_| observation("clone_worker_identity_drift"))?;
        Ok(ConfirmedDisposableWorker { host, request })
    }

    fn verify_limactl(
        &self,
        executor: &impl TimedCommandExecutor,
    ) -> Result<(), DisposableCloneRuntimeError> {
        let (command, expected) = self.worker.version_command();
        let record = executor
            .execute_with_timeout(&command, OBSERVATION_TIMEOUT)
            .map_err(|_| observation("clone_lima_version_unavailable"))?;
        if record.argv != command.displayed_argv()
            || record.environment_keys != command.environment.keys().cloned().collect::<Vec<_>>()
            || record.status != Some(0)
            || !record.success
            || record.stdout != expected
            || !record.stderr.is_empty()
        {
            return Err(observation("clone_lima_version_mismatch"));
        }
        Ok(())
    }

    fn confirm_target_absent(
        &self,
        request: &LimaObservationRequest,
        executor: &impl TimedCommandExecutor,
        clock: &impl LimaObservationClock,
    ) -> Result<(), DisposableCloneRuntimeError> {
        let adapter = LimaObservationAdapter::new(self.limactl_program.clone())
            .map_err(|_| invalid_configuration("clone_observer_invalid"))?;
        let bounded = BoundedExecutor { executor };
        match adapter.observe(request, &bounded, clock) {
            Err(error) if error.code == LimaObservationRefusalCode::MissingInstanceEvidence => {
                Ok(())
            }
            Ok(_) => Err(DisposableCloneRuntimeError::recovery(
                "clone_target_no_longer_absent",
            )),
            Err(_) => Err(observation("clone_target_absence_unavailable")),
        }
    }
}

pub(crate) struct PreparedClone {
    pub(crate) plan: DisposableLimaWorkerCommandPlan,
    target_request: LimaObservationRequest,
    generation: DisposableTemplateGenerationDocument,
    admission_expires_at: EpochMillis,
}

/// Descriptor-retaining proof of the exact disposable target observed ready for JIT handoff.
pub(crate) struct ConfirmedDisposableWorker {
    host: crate::lima_host_identity::LimaHostIdentityObservation,
    request: LimaObservationRequest,
}

impl ConfirmedDisposableWorker {
    pub(crate) fn confirm_current(&self) -> Result<(), DisposableCloneRuntimeError> {
        self.host
            .confirm(&self.request)
            .map_err(|_| observation("clone_worker_identity_drift"))
    }
}

pub(crate) trait CloneRuntimeClock: LimaObservationClock {
    fn epoch_millis(&self) -> io::Result<EpochMillis>;
}

struct BoundedExecutor<'a, E> {
    executor: &'a E,
}

impl<E: TimedCommandExecutor> CommandExecutor for BoundedExecutor<'_, E> {
    fn execute(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord> {
        self.executor
            .execute_with_timeout(spec, OBSERVATION_TIMEOUT)
    }
}

fn validate_record(
    command_spec: &CommandSpec,
    record: &ExecutionRecord,
) -> Result<(), DisposableCloneRuntimeError> {
    if record.argv != command_spec.displayed_argv()
        || record.environment_keys != command_spec.environment.keys().cloned().collect::<Vec<_>>()
        || record.status != Some(0)
        || !record.success
    {
        return Err(command("clone_command_record_mismatch"));
    }
    Ok(())
}

const fn runtime_error(
    kind: DisposableCloneRuntimeErrorKind,
    code: &'static str,
    message: &'static str,
) -> DisposableCloneRuntimeError {
    DisposableCloneRuntimeError {
        kind,
        code,
        message,
    }
}

const fn invalid_configuration(code: &'static str) -> DisposableCloneRuntimeError {
    runtime_error(
        DisposableCloneRuntimeErrorKind::InvalidConfiguration,
        code,
        "the fixed disposable-clone runtime configuration is invalid",
    )
}

pub(crate) const fn observation(code: &'static str) -> DisposableCloneRuntimeError {
    runtime_error(
        DisposableCloneRuntimeErrorKind::Observation,
        code,
        "the disposable-clone host evidence is unavailable or changed",
    )
}

const fn command(code: &'static str) -> DisposableCloneRuntimeError {
    runtime_error(
        DisposableCloneRuntimeErrorKind::Command,
        code,
        "the fixed disposable-clone command failed or returned inconsistent evidence",
    )
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::disposable_attempt_catalog::{
        DisposableAttemptCatalog, DisposableAttemptCatalogAction, DisposableAttemptReservation,
        MemoryDisposableAttemptCatalogStore,
    };
    use crate::disposable_attempt_state::DisposableAttemptState;
    use crate::disposable_prepared_template::current_disposable_prepared_template;
    use crate::disposable_runner_runtime::{
        DisposableRunnerRegistrationSource, DisposableRunnerRuntime, DisposableRunnerRuntimeError,
    };
    use crate::disposable_template_generation::{
        DisposableTemplateGenerationAction, DisposableTemplateObjectIdentity,
        encode_disposable_template_generation,
    };
    use crate::disposable_worker_reconciler::{
        CapacityClaimId, DisposableVmId, DisposableWorkerResources,
    };
    use crate::github_scale_set_bridge::{
        EncodedJitConfig, ScaleSetBridgeError, ScaleSetBridgeEvent, ScaleSetBridgeIdentity,
        ScaleSetBridgeJobEvidence, ScaleSetBridgePoll, ScaleSetJitReceipt, ScaleSetRunnerLookup,
        ScaleSetStatistics,
    };
    use crate::github_scale_set_protocol::{
        ScaleSetJobId, ScaleSetRunnerId, ScaleSetRunnerName, ScaleSetRunnerReference,
    };
    use crate::github_scale_set_service::{LiveScaleSetCloneAdmission, ScaleSetBridgeSession};
    use crate::lima_observation::{LimaArchitecture, LimaVmType};
    use crate::unix_personal_worker_store::{DisposableRunnerTransactionOutcome, STORE_DIRECTORY};

    #[allow(dead_code)]
    mod lima_host_identity_support {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/lima_host_identity_support/mod.rs"
        ));
    }
    use lima_host_identity_support::LimaHostIdentityFixture;

    const SOURCE: &str = "smolrunner-source";
    const TARGET: &str = "smol-clone-1";
    const GIB: u64 = 1 << 30;
    const SOURCE_DISK: u64 = 20 * GIB;
    const TARGET_DISK: u64 = 80 * GIB;
    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(label: &str) -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "smolrunner-clone-runtime-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct FixedClock;

    impl LimaObservationClock for FixedClock {
        fn unix_seconds(&self) -> io::Result<u64> {
            Ok(1_900_000_000)
        }
    }

    impl CloneRuntimeClock for FixedClock {
        fn epoch_millis(&self) -> io::Result<EpochMillis> {
            EpochMillis::new(1_900_000_000_000).map_err(io::Error::other)
        }
    }

    struct AdmissionOrderedClock {
        observed: Rc<Cell<bool>>,
    }

    impl LimaObservationClock for AdmissionOrderedClock {
        fn unix_seconds(&self) -> io::Result<u64> {
            Ok(1_900_000_000)
        }
    }

    impl CloneRuntimeClock for AdmissionOrderedClock {
        fn epoch_millis(&self) -> io::Result<EpochMillis> {
            let value = if self.observed.get() {
                1_900_000_000_001
            } else {
                1_900_000_000_000
            };
            EpochMillis::new(value).map_err(io::Error::other)
        }
    }

    struct PostPollClockFailure {
        calls: Cell<u8>,
    }

    impl LimaObservationClock for PostPollClockFailure {
        fn unix_seconds(&self) -> io::Result<u64> {
            Ok(1_900_000_000)
        }
    }

    impl CloneRuntimeClock for PostPollClockFailure {
        fn epoch_millis(&self) -> io::Result<EpochMillis> {
            let call = self.calls.get();
            self.calls.set(call.saturating_add(1));
            if call == 0 {
                EpochMillis::new(1_900_000_000_000).map_err(io::Error::other)
            } else {
                Err(io::Error::other("injected post-poll clock failure"))
            }
        }
    }

    struct SequencedClock {
        calls: Cell<u8>,
        final_millis: u64,
    }

    impl LimaObservationClock for SequencedClock {
        fn unix_seconds(&self) -> io::Result<u64> {
            Ok(1_900_000_000)
        }
    }

    impl CloneRuntimeClock for SequencedClock {
        fn epoch_millis(&self) -> io::Result<EpochMillis> {
            let call = self.calls.get();
            self.calls.set(call.saturating_add(1));
            let value = if call < 1 {
                1_900_000_000_000
            } else {
                self.final_millis
            };
            EpochMillis::new(value).map_err(io::Error::other)
        }
    }

    struct FakeExecutor<'a> {
        host: &'a LimaHostIdentityFixture,
        calls: RefCell<Vec<Vec<String>>>,
        fail_clone: bool,
        fail_runner: bool,
        target_ready: bool,
        rewrite_target_during_final_source: bool,
        target_rewritten: Cell<bool>,
        version_calls: Cell<u8>,
        drift_version_on: Option<u8>,
    }

    impl FakeExecutor<'_> {
        fn record(spec: &CommandSpec, stdout: String) -> ExecutionRecord {
            ExecutionRecord {
                argv: spec.displayed_argv(),
                environment_keys: spec.environment.keys().cloned().collect(),
                status: Some(0),
                success: true,
                stdout,
                stderr: String::new(),
            }
        }

        fn source_config() -> serde_json::Value {
            serde_json::json!({
                "vmType": "vz",
                "arch": "aarch64",
                "plain": true,
                "mounts": [],
                "networks": [],
                "portForwards": [],
                "propagateProxyEnv": false,
                "hostResolver": { "enabled": false },
                "containerd": { "system": false, "user": false }
            })
        }

        fn source_json(&self) -> String {
            serde_json::json!({
                "name": SOURCE,
                "status": "Stopped",
                "dir": self.host.lima_home().join(SOURCE),
                "vmType": "vz",
                "arch": "aarch64",
                "cpus": 2,
                "memory": 2 * GIB,
                "disk": SOURCE_DISK,
                "errors": [],
                "config": Self::source_config()
            })
            .to_string()
                + "\n"
        }

        fn target_config() -> serde_json::Value {
            let mut config = Self::source_config();
            let object = config.as_object_mut().unwrap();
            object.insert("cpus".to_owned(), serde_json::json!(4));
            object.insert("memory".to_owned(), serde_json::json!("8GiB"));
            object.insert("disk".to_owned(), serde_json::json!("80GiB"));
            config
        }

        fn target_json(&self) -> String {
            serde_json::json!({
                "name": TARGET,
                "status": "Running",
                "dir": self.host.lima_home().join(TARGET),
                "vmType": "vz",
                "arch": "aarch64",
                "cpus": 4,
                "memory": 8 * GIB,
                "disk": TARGET_DISK,
                "errors": [],
                "config": Self::target_config()
            })
            .to_string()
                + "\n"
        }

        fn clone_count(&self) -> usize {
            self.calls
                .borrow()
                .iter()
                .filter(|argv| argv.iter().any(|value| value == "clone"))
                .count()
        }

        fn runner_launch_count(&self) -> usize {
            self.calls
                .borrow()
                .iter()
                .filter(|argv| {
                    argv.iter()
                        .any(|value| value == "/opt/smolrunner/bin/smolrunner-jit-launcher")
                        && !argv.iter().any(|value| value == "/usr/bin/sha256sum")
                })
                .count()
        }
    }

    impl CommandExecutor for FakeExecutor<'_> {
        fn execute(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord> {
            self.execute_with_timeout(spec, OBSERVATION_TIMEOUT)
        }
    }

    impl TimedCommandExecutor for FakeExecutor<'_> {
        fn execute_with_timeout(
            &self,
            spec: &CommandSpec,
            _timeout: Duration,
        ) -> io::Result<ExecutionRecord> {
            let argv = spec.displayed_argv();
            self.calls.borrow_mut().push(argv.clone());
            if argv.iter().any(|value| value == "clone") {
                if self.fail_clone {
                    return Err(io::Error::other("injected clone failure"));
                }
                self.host.add_instance(TARGET, TARGET_DISK);
                return Ok(Self::record(spec, "clone complete\n".to_owned()));
            }
            if argv.len() == 3 && argv.last().is_some_and(|value| value == "--version") {
                let call = self.version_calls.get().saturating_add(1);
                self.version_calls.set(call);
                let stdout = if self.drift_version_on == Some(call) {
                    "limactl version 2.3.0\n"
                } else {
                    "limactl version 2.2.0\n"
                };
                return Ok(Self::record(spec, stdout.to_owned()));
            }
            if argv.iter().any(|value| value == "validate") {
                return Ok(Self::record(
                    spec,
                    serde_json::to_string(&Self::source_config()).unwrap() + "\n",
                ));
            }
            if argv.iter().any(|value| value == "list") {
                let target = argv.last().map(String::as_str).unwrap_or_default();
                let stdout = if target == SOURCE {
                    if self.rewrite_target_during_final_source
                        && self.clone_count() > 0
                        && !self.target_rewritten.replace(true)
                    {
                        self.host.rewrite_disk_identity(TARGET, 0x88);
                    }
                    self.source_json()
                } else if target == TARGET && !self.host.lima_home().join(TARGET).exists() {
                    "\n".to_owned()
                } else if target == TARGET && self.target_ready {
                    self.target_json()
                } else {
                    return Err(io::Error::other("unexpected present target observation"));
                };
                return Ok(Self::record(spec, stdout));
            }
            if self.target_ready && argv.iter().any(|value| value == TARGET) {
                let runner_launch = argv
                    .iter()
                    .any(|value| value == "/opt/smolrunner/bin/smolrunner-jit-launcher")
                    && !argv.iter().any(|value| value == "/usr/bin/sha256sum");
                if runner_launch && self.fail_runner {
                    return Err(io::Error::other("injected runner failure"));
                }
                let stdout = if argv.iter().any(|value| value == "/usr/bin/uname") {
                    "aarch64\n".to_owned()
                } else if argv.iter().any(|value| value == "_NPROCESSORS_ONLN") {
                    "4\n".to_owned()
                } else if argv.iter().any(|value| value == "PAGE_SIZE") {
                    "4096\n".to_owned()
                } else if argv.iter().any(|value| value == "_PHYS_PAGES") {
                    "2097152\n".to_owned()
                } else if argv.iter().any(|value| value == "/etc/machine-id") {
                    format!("{}  /etc/machine-id\n", "aa".repeat(32))
                } else if argv.last().is_some_and(|value| value == "/") {
                    "1:2\n".to_owned()
                } else if argv.last().is_some_and(|value| value == "[REDACTED]") {
                    "3:4\n".to_owned()
                } else if argv
                    .last()
                    .is_some_and(|value| value == "/etc/smolrunner/prepared-template.json")
                {
                    "a330312e82193d07907ebd7ef291d5d5851f9106234ab58e72d662acc6bbb3b2  /etc/smolrunner/prepared-template.json\n".to_owned()
                } else if argv
                    .last()
                    .is_some_and(|value| value == "/opt/smolrunner/bin/smolrunner-jit-launcher")
                {
                    "9b7cc857f2de1181f64bb067e4d4870e0bcb679d597ec047d885395ac6160996  /opt/smolrunner/bin/smolrunner-jit-launcher\n".to_owned()
                } else if argv.last().is_some_and(|value| {
                    value == "/etc/apt/apt.conf.d/99-smolrunner-no-automatic-updates"
                }) {
                    "b10384a904cdd14d18af31a7754a19ca0c67c237f3ca7bd239f4cf64102ffedb  /etc/apt/apt.conf.d/99-smolrunner-no-automatic-updates\n".to_owned()
                } else if argv
                    .iter()
                    .any(|value| value == "/opt/smolrunner/actions-runner/bin/Runner.Listener")
                {
                    "2.336.0\n".to_owned()
                } else if argv.iter().any(|value| value == "/usr/bin/id") {
                    "smolrunner-runner\n".to_owned()
                } else if argv.iter().any(|value| value == "/usr/bin/systemctl") {
                    return Ok(ExecutionRecord {
                        argv: spec.displayed_argv(),
                        environment_keys: spec.environment.keys().cloned().collect(),
                        status: Some(1),
                        success: false,
                        stdout: "masked\n".to_owned(),
                        stderr: String::new(),
                    });
                } else {
                    return Err(io::Error::other("unexpected target guest command"));
                };
                return Ok(Self::record(spec, stdout));
            }
            Err(io::Error::other("unexpected fake command"))
        }
    }

    fn runtime(root: &TempRoot, host: &LimaHostIdentityFixture) -> DisposableCloneRuntime {
        DisposableCloneRuntime::new(
            root.path(),
            "/opt/homebrew/bin/limactl",
            host.lima_home(),
            LimaInstanceName::parse(SOURCE).unwrap(),
        )
        .unwrap()
    }

    fn executor(host: &LimaHostIdentityFixture, fail_clone: bool) -> FakeExecutor<'_> {
        FakeExecutor {
            host,
            calls: RefCell::new(Vec::new()),
            fail_clone,
            fail_runner: false,
            target_ready: false,
            rewrite_target_during_final_source: false,
            target_rewritten: Cell::new(false),
            version_calls: Cell::new(0),
            drift_version_on: None,
        }
    }

    #[test]
    fn running_clone_must_match_exact_resources_policy_and_host_identity() {
        let root = TempRoot::new("ready-worker");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-ready-worker",
            SOURCE,
            SOURCE_DISK,
        );
        host.add_instance(TARGET, TARGET_DISK);
        let runtime = runtime(&root, &host);
        let attempt_id = DisposableAttemptId::parse("attempt-ready-worker").unwrap();
        let attempt = DisposableAttemptState::reserved(
            attempt_id.clone(),
            CapacityClaimId::parse("claim-ready-worker").unwrap(),
            DisposableVmId::parse(TARGET).unwrap(),
            ScaleSetRunnerName::parse(TARGET).unwrap(),
            EpochMillis::new(1_900_000_600_000).unwrap(),
        );
        let resources = DisposableWorkerResources::new(4_000, 8 * GIB, TARGET_DISK).unwrap();
        let reservation = DisposableAttemptReservation::new(
            attempt,
            resources,
            current_disposable_prepared_template()
                .unwrap()
                .identity()
                .unwrap(),
        )
        .unwrap();
        let mut catalog =
            DisposableAttemptCatalog::new(MemoryDisposableAttemptCatalogStore::default());
        let (empty, _) = catalog.initialize().unwrap();
        let (reserved, _) = catalog.reserve(empty.revision(), reservation).unwrap();
        let authorized = reserved
            .replace_attempt(
                &attempt_id,
                reserved
                    .find_active(&attempt_id)
                    .unwrap()
                    .attempt()
                    .revision(),
                DisposableAttemptCatalogAction::AuthorizeClone,
            )
            .unwrap();
        let started = authorized
            .replace_attempt(
                &attempt_id,
                authorized
                    .find_active(&attempt_id)
                    .unwrap()
                    .attempt()
                    .revision(),
                DisposableAttemptCatalogAction::RecordCloneStarted,
            )
            .unwrap();
        let target_request = runtime
            .worker
            .target_observation_request(started.find_active(&attempt_id).unwrap())
            .unwrap();
        let target_host = LimaHostIdentityAdapter.observe(&target_request).unwrap();
        let bound = started
            .bind_vm_identity_after_clone(
                &attempt_id,
                started
                    .find_active(&attempt_id)
                    .unwrap()
                    .attempt()
                    .revision(),
                DisposableVmIdentity::from_host_identity(target_host.identity()),
            )
            .unwrap();
        let registering = bound
            .replace_attempt(
                &attempt_id,
                bound.find_active(&attempt_id).unwrap().attempt().revision(),
                DisposableAttemptCatalogAction::BeginRegistration,
            )
            .unwrap();
        let mut executor = executor(&host, false);
        executor.target_ready = true;

        let confirmed = runtime
            .confirm_ready_worker(
                registering.find_active(&attempt_id).unwrap(),
                &executor,
                &FixedClock,
            )
            .unwrap_or_else(|error| panic!("{error:?}; calls={:?}", executor.calls.borrow()));
        confirmed.confirm_current().unwrap();

        assert!(executor.calls.borrow().iter().any(|argv| {
            argv.iter().any(|value| value == TARGET)
                && argv.iter().any(|value| value == "/usr/bin/sha256sum")
        }));
    }

    #[test]
    fn runner_transaction_persists_registration_and_start_before_one_command() {
        let root = TempRoot::new("runner-transaction");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-runner-transaction",
            SOURCE,
            SOURCE_DISK,
        );
        let clone_runtime = runtime(&root, &host);
        let mut executor = executor(&host, false);
        let attempt_id =
            install_running_registering_attempt(&root, &host, &clone_runtime, &mut executor);
        let mut store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        let source_identity =
            ScaleSetBridgeIdentity::parse(&format!("sha256:{}", "66".repeat(32))).unwrap();
        store.initialize_scale_set_inbox(&source_identity).unwrap();
        let runner_runtime =
            DisposableRunnerRuntime::new("/opt/homebrew/bin/limactl", host.lima_home()).unwrap();
        let mut registration = FakeRegistration {
            source_identity,
            observed: ScaleSetRunnerLookup::Absent,
            observe_calls: 0,
            jit_calls: 0,
        };

        let outcome = store
            .execute_disposable_runner_transaction(
                &runner_runtime,
                &clone_runtime,
                &attempt_id,
                &mut registration,
                &executor,
                &FixedClock,
            )
            .unwrap();
        assert!(matches!(
            outcome,
            DisposableRunnerTransactionOutcome::CommandCompleted(_)
        ));
        assert_eq!(registration.observe_calls, 1);
        assert_eq!(registration.jit_calls, 1);
        assert_eq!(executor.runner_launch_count(), 1);
        let durable = durable_attempt(&root, &attempt_id);
        assert_eq!(durable.runner_id().map(ScaleSetRunnerId::get), Some(77));
        assert!(durable.runner_start_started());

        let error = store
            .execute_disposable_runner_transaction(
                &runner_runtime,
                &clone_runtime,
                &attempt_id,
                &mut registration,
                &executor,
                &FixedClock,
            )
            .unwrap_err();
        assert_eq!(error.code(), "runner_launch_state_invalid");
        assert_eq!(registration.observe_calls, 1);
        assert_eq!(registration.jit_calls, 1);
        assert_eq!(executor.runner_launch_count(), 1);
    }

    #[test]
    fn failed_runner_command_leaves_started_checkpoint_and_never_replays_jit() {
        let root = TempRoot::new("runner-command-failure");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-runner-command-failure",
            SOURCE,
            SOURCE_DISK,
        );
        let clone_runtime = runtime(&root, &host);
        let mut executor = executor(&host, false);
        let attempt_id =
            install_running_registering_attempt(&root, &host, &clone_runtime, &mut executor);
        executor.fail_runner = true;
        let source_identity =
            ScaleSetBridgeIdentity::parse(&format!("sha256:{}", "67".repeat(32))).unwrap();
        let mut store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        store.initialize_scale_set_inbox(&source_identity).unwrap();
        let runner_runtime =
            DisposableRunnerRuntime::new("/opt/homebrew/bin/limactl", host.lima_home()).unwrap();
        let mut registration = FakeRegistration {
            source_identity,
            observed: ScaleSetRunnerLookup::Absent,
            observe_calls: 0,
            jit_calls: 0,
        };

        let error = store
            .execute_disposable_runner_transaction(
                &runner_runtime,
                &clone_runtime,
                &attempt_id,
                &mut registration,
                &executor,
                &FixedClock,
            )
            .unwrap_err();
        assert_eq!(error.code(), "runner_command_failed");
        assert!(durable_attempt(&root, &attempt_id).runner_start_started());
        assert_eq!(registration.jit_calls, 1);
        assert_eq!(executor.runner_launch_count(), 1);

        let recovery = store
            .execute_disposable_runner_transaction(
                &runner_runtime,
                &clone_runtime,
                &attempt_id,
                &mut registration,
                &executor,
                &FixedClock,
            )
            .unwrap_err();
        assert_eq!(recovery.code(), "runner_launch_state_invalid");
        assert_eq!(registration.jit_calls, 1);
        assert_eq!(executor.runner_launch_count(), 1);
    }

    #[test]
    fn discovered_registration_is_persisted_without_jit_or_runner_replay() {
        let root = TempRoot::new("runner-registration-recovery");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-runner-registration-recovery",
            SOURCE,
            SOURCE_DISK,
        );
        let clone_runtime = runtime(&root, &host);
        let mut executor = executor(&host, false);
        let attempt_id =
            install_running_registering_attempt(&root, &host, &clone_runtime, &mut executor);
        let source_identity =
            ScaleSetBridgeIdentity::parse(&format!("sha256:{}", "68".repeat(32))).unwrap();
        let mut store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        store.initialize_scale_set_inbox(&source_identity).unwrap();
        let runner_runtime =
            DisposableRunnerRuntime::new("/opt/homebrew/bin/limactl", host.lima_home()).unwrap();
        let existing = ScaleSetRunnerReference::new(
            ScaleSetRunnerId::new(78).unwrap(),
            ScaleSetRunnerName::parse(TARGET).unwrap(),
        );
        let mut registration = FakeRegistration {
            source_identity,
            observed: ScaleSetRunnerLookup::Present(existing),
            observe_calls: 0,
            jit_calls: 0,
        };

        let outcome = store
            .execute_disposable_runner_transaction(
                &runner_runtime,
                &clone_runtime,
                &attempt_id,
                &mut registration,
                &executor,
                &FixedClock,
            )
            .unwrap();

        assert!(matches!(
            outcome,
            DisposableRunnerTransactionOutcome::RegistrationRecovered { .. }
        ));
        let durable = durable_attempt(&root, &attempt_id);
        assert_eq!(durable.runner_id().map(ScaleSetRunnerId::get), Some(78));
        assert!(!durable.runner_start_started());
        assert_eq!(registration.observe_calls, 1);
        assert_eq!(registration.jit_calls, 0);
        assert_eq!(executor.runner_launch_count(), 0);
    }

    struct FakeAdmission {
        calls: Cell<u8>,
        lose_capacity_on: Option<u8>,
        cancel_on: Option<u8>,
    }

    impl FakeAdmission {
        fn available() -> Self {
            Self {
                calls: Cell::new(0),
                lose_capacity_on: None,
                cancel_on: None,
            }
        }
    }

    impl admission_seal::Sealed for FakeAdmission {}

    impl DisposableCloneAdmissionSource for FakeAdmission {
        fn observe(
            &self,
            catalog: &DisposableAttemptCatalogDocument,
            reservation: &DisposableAttemptReservation,
        ) -> Result<DisposableCloneAdmissionObservation, DisposableCloneRuntimeError> {
            let call = self.calls.get().saturating_add(1);
            self.calls.set(call);
            Ok(DisposableCloneAdmissionObservation::new(
                catalog,
                reservation,
                EpochMillis::new(1_900_000_000_000).unwrap(),
                EpochMillis::new(1_900_000_030_000).unwrap(),
                self.lose_capacity_on != Some(call),
                self.cancel_on == Some(call),
            ))
        }
    }

    struct FakeRegistration {
        source_identity: ScaleSetBridgeIdentity,
        observed: ScaleSetRunnerLookup,
        observe_calls: u8,
        jit_calls: u8,
    }

    impl DisposableRunnerRegistrationSource for FakeRegistration {
        fn scale_set_source_identity(&self) -> &ScaleSetBridgeIdentity {
            &self.source_identity
        }

        fn observe_runner(
            &mut self,
            _runner_name: &ScaleSetRunnerName,
        ) -> Result<ScaleSetRunnerLookup, DisposableRunnerRuntimeError> {
            self.observe_calls = self.observe_calls.saturating_add(1);
            Ok(self.observed.clone())
        }

        fn generate_jit(
            &mut self,
            runner_name: &ScaleSetRunnerName,
        ) -> Result<ScaleSetJitReceipt, DisposableRunnerRuntimeError> {
            self.jit_calls = self.jit_calls.saturating_add(1);
            Ok(ScaleSetJitReceipt {
                runner: ScaleSetRunnerReference::new(
                    ScaleSetRunnerId::new(77).unwrap(),
                    runner_name.clone(),
                ),
                config: EncodedJitConfig::for_test("eyJ0b2tlbiI6InNlY3JldCJ9"),
            })
        }
    }

    struct OrderedAdmission {
        observed: Rc<Cell<bool>>,
    }

    impl admission_seal::Sealed for OrderedAdmission {}

    impl DisposableCloneAdmissionSource for OrderedAdmission {
        fn observe(
            &self,
            catalog: &DisposableAttemptCatalogDocument,
            reservation: &DisposableAttemptReservation,
        ) -> Result<DisposableCloneAdmissionObservation, DisposableCloneRuntimeError> {
            self.observed.set(true);
            Ok(DisposableCloneAdmissionObservation::new(
                catalog,
                reservation,
                EpochMillis::new(1_900_000_000_001).unwrap(),
                EpochMillis::new(1_900_000_030_001).unwrap(),
                true,
                false,
            ))
        }
    }

    struct MessageBridge {
        response: RefCell<Option<ScaleSetBridgePoll>>,
        capacities: RefCell<Vec<u16>>,
    }

    impl ScaleSetBridgeSession for MessageBridge {
        fn poll(
            &mut self,
            available_capacity: u16,
        ) -> Result<ScaleSetBridgePoll, ScaleSetBridgeError> {
            self.capacities.borrow_mut().push(available_capacity);
            Ok(self.response.borrow_mut().take().unwrap())
        }

        fn ack(&mut self, _message_id: u32) -> Result<Vec<u64>, ScaleSetBridgeError> {
            unreachable!("clone admission never acknowledges a message")
        }
    }

    fn install_ready_generation(
        root: &TempRoot,
        host: &LimaHostIdentityFixture,
        runtime: &DisposableCloneRuntime,
    ) {
        let source_request = LimaObservationRequest::new(
            LimaInstanceName::parse(SOURCE).unwrap(),
            host.lima_home(),
            LimaVmType::Vz,
            LimaArchitecture::Aarch64,
            "/var/lib/smolrunner-runner/work",
            30,
        )
        .unwrap();
        let source_host = LimaHostIdentityAdapter.observe(&source_request).unwrap();
        let object = DisposableTemplateObjectIdentity::from_host_digest(
            source_host.identity().digest().clone(),
        );
        let initial = runtime.template_runtime.initial_document();
        let ready = initial
            .transition(1, DisposableTemplateGenerationAction::AuthorizeCreate, None)
            .unwrap()
            .transition(
                2,
                DisposableTemplateGenerationAction::RecordCreateStarted,
                None,
            )
            .unwrap()
            .transition(
                3,
                DisposableTemplateGenerationAction::RecordVerified,
                Some(object),
            )
            .unwrap()
            .transition(4, DisposableTemplateGenerationAction::AuthorizeStop, None)
            .unwrap()
            .transition(
                5,
                DisposableTemplateGenerationAction::RecordStopStarted,
                None,
            )
            .unwrap()
            .transition(6, DisposableTemplateGenerationAction::RecordReady, None)
            .unwrap();
        let mut store =
            UnixPersonalWorkerStore::open_or_create_disposable_template_generation(root.path())
                .unwrap();
        store
            .create_disposable_template_generation(&initial)
            .unwrap();
        drop(store);
        fs::write(
            root.path()
                .join(STORE_DIRECTORY)
                .join("disposable-template-generation.json"),
            encode_disposable_template_generation(&ready).unwrap(),
        )
        .unwrap();
    }

    fn install_reserved_attempt(root: &TempRoot) -> DisposableAttemptId {
        let store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        let mut catalog = DisposableAttemptCatalog::new(store);
        let (empty, _) = catalog.initialize().unwrap();
        let attempt_id = DisposableAttemptId::parse("attempt-clone-1").unwrap();
        let attempt = DisposableAttemptState::reserved(
            attempt_id.clone(),
            CapacityClaimId::parse("claim-clone-1").unwrap(),
            DisposableVmId::parse(TARGET).unwrap(),
            ScaleSetRunnerName::parse("smol-clone-1").unwrap(),
            EpochMillis::new(1_900_000_600_000).unwrap(),
        );
        let reservation = DisposableAttemptReservation::new(
            attempt,
            DisposableWorkerResources::new(4_000, 8 * GIB, TARGET_DISK).unwrap(),
            current_disposable_prepared_template()
                .unwrap()
                .identity()
                .unwrap(),
        )
        .unwrap();
        catalog.reserve(empty.revision(), reservation).unwrap();
        attempt_id
    }

    fn install_authorized_attempt(root: &TempRoot) -> DisposableAttemptId {
        let attempt_id = install_reserved_attempt(root);
        let store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        let mut catalog = DisposableAttemptCatalog::new(store);
        let reserved = catalog.load().unwrap();
        catalog
            .transition(
                reserved.revision(),
                &attempt_id,
                reserved
                    .find_active(&attempt_id)
                    .unwrap()
                    .attempt()
                    .revision(),
                DisposableAttemptCatalogAction::AuthorizeClone,
            )
            .unwrap();
        attempt_id
    }

    fn durable_attempt(
        root: &TempRoot,
        attempt_id: &DisposableAttemptId,
    ) -> crate::disposable_attempt_state::DisposableAttemptState {
        let store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        DisposableAttemptCatalog::new(store)
            .load()
            .unwrap()
            .find_active(attempt_id)
            .unwrap()
            .attempt()
            .clone()
    }

    fn install_running_registering_attempt(
        root: &TempRoot,
        host: &LimaHostIdentityFixture,
        runtime: &DisposableCloneRuntime,
        executor: &mut FakeExecutor<'_>,
    ) -> DisposableAttemptId {
        install_ready_generation(root, host, runtime);
        let attempt_id = install_authorized_attempt(root);
        runtime
            .clone_once_with(
                &attempt_id,
                &FakeAdmission::available(),
                executor,
                &FixedClock,
            )
            .unwrap();
        executor.target_ready = true;
        let store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();
        let mut catalog = DisposableAttemptCatalog::new(store);
        let cloned = catalog.load().unwrap();
        catalog
            .transition(
                cloned.revision(),
                &attempt_id,
                cloned
                    .find_active(&attempt_id)
                    .unwrap()
                    .attempt()
                    .revision(),
                DisposableAttemptCatalogAction::BeginRegistration,
            )
            .unwrap();
        attempt_id
    }

    #[test]
    fn reserved_attempt_is_authorized_only_after_fresh_absence_and_admission() {
        let root = TempRoot::new("authorize-reserved");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-authorize-reserved",
            SOURCE,
            SOURCE_DISK,
        );
        let runtime = runtime(&root, &host);
        let attempt_id = install_reserved_attempt(&root);
        let executor = executor(&host, false);
        let admission = FakeAdmission::available();
        let mut store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(root.path()).unwrap();

        let outcome = store
            .authorize_disposable_clone_transaction(
                &runtime,
                &attempt_id,
                &admission,
                &executor,
                &FixedClock,
            )
            .unwrap();

        assert!(matches!(
            outcome,
            DisposableCloneTransactionOutcome::CloneAuthorized { ref attempt_id }
                if attempt_id == "attempt-clone-1"
        ));
        assert_eq!(admission.calls.get(), 1);
        assert_eq!(executor.clone_count(), 0);
        let attempt = durable_attempt(&root, &attempt_id);
        assert_eq!(attempt.phase(), DisposableAttemptPhase::CloneAuthorized);
        assert!(attempt.vm_identity().is_none());
    }

    #[test]
    fn live_message_preempts_reserved_attempt_authorization() {
        let root = TempRoot::new("authorize-message");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-authorize-message",
            SOURCE,
            SOURCE_DISK,
        );
        let runtime = runtime(&root, &host);
        let attempt_id = install_reserved_attempt(&root);
        let identity =
            ScaleSetBridgeIdentity::parse(&format!("sha256:{}", "45".repeat(32))).unwrap();
        let mut store =
            UnixPersonalWorkerStore::open_or_recover_scale_set_inbox(root.path()).unwrap();
        store.initialize_scale_set_inbox(&identity).unwrap();
        let executor = executor(&host, false);
        let mut bridge = MessageBridge {
            response: RefCell::new(Some(ScaleSetBridgePoll::Message {
                message_id: 8,
                statistics: ScaleSetStatistics {
                    available_jobs: 0,
                    acquired_jobs: 1,
                    assigned_jobs: 1,
                    running_jobs: 0,
                    registered_runners: 0,
                    busy_runners: 0,
                    idle_runners: 0,
                },
                events: vec![ScaleSetBridgeEvent::Assigned(ScaleSetBridgeJobEvidence {
                    runner_request_id: 41,
                    repository: "project".to_owned(),
                    owner: "example".to_owned(),
                    job_id: ScaleSetJobId::parse("job-1").unwrap(),
                    workflow_run_id: 99,
                    request_labels: vec!["smolrunner".to_owned()],
                })],
            })),
            capacities: RefCell::new(Vec::new()),
        };
        let clock = PostPollClockFailure {
            calls: Cell::new(0),
        };
        let admission = LiveScaleSetCloneAdmission::new(&mut bridge, &identity, &clock);

        let outcome = store
            .authorize_disposable_clone_transaction(
                &runtime,
                &attempt_id,
                &admission,
                &executor,
                &clock,
            )
            .unwrap();

        assert!(matches!(
            outcome,
            DisposableCloneTransactionOutcome::ScaleSetMessagePersisted { message_id: 8 }
        ));
        drop(admission);
        assert_eq!(bridge.capacities.into_inner(), [0]);
        assert_eq!(executor.clone_count(), 0);
        let (inbox, catalog) = store.load_scale_set_control_state(&identity).unwrap();
        assert_eq!(
            catalog.find_active(&attempt_id).unwrap().attempt().phase(),
            DisposableAttemptPhase::Reserved
        );
        assert_eq!(inbox.pending().unwrap().message_id(), 8);
    }

    #[test]
    fn live_message_is_persisted_before_clone_admission_returns() {
        let root = TempRoot::new("scale-set-message");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-scale-set-message",
            SOURCE,
            SOURCE_DISK,
        );
        let runtime = runtime(&root, &host);
        install_ready_generation(&root, &host, &runtime);
        let attempt_id = install_authorized_attempt(&root);
        let identity =
            ScaleSetBridgeIdentity::parse(&format!("sha256:{}", "44".repeat(32))).unwrap();
        let mut store =
            UnixPersonalWorkerStore::open_or_recover_scale_set_inbox(root.path()).unwrap();
        store.initialize_scale_set_inbox(&identity).unwrap();
        drop(store);
        let executor = executor(&host, false);
        let mut bridge = MessageBridge {
            response: RefCell::new(Some(ScaleSetBridgePoll::Message {
                message_id: 7,
                statistics: ScaleSetStatistics {
                    available_jobs: 0,
                    acquired_jobs: 1,
                    assigned_jobs: 1,
                    running_jobs: 0,
                    registered_runners: 0,
                    busy_runners: 0,
                    idle_runners: 0,
                },
                events: vec![ScaleSetBridgeEvent::Assigned(ScaleSetBridgeJobEvidence {
                    runner_request_id: 41,
                    repository: "project".to_owned(),
                    owner: "example".to_owned(),
                    job_id: ScaleSetJobId::parse("job-1").unwrap(),
                    workflow_run_id: 99,
                    request_labels: vec!["smolrunner".to_owned()],
                })],
            })),
            capacities: RefCell::new(Vec::new()),
        };
        let clock = PostPollClockFailure {
            calls: Cell::new(0),
        };
        let admission = LiveScaleSetCloneAdmission::new(&mut bridge, &identity, &clock);

        let error = runtime
            .clone_once_with(&attempt_id, &admission, &executor, &clock)
            .unwrap_err();

        assert_eq!(error.code(), "clone_scale_set_message_persisted");
        assert_eq!(executor.clone_count(), 0);
        drop(admission);
        assert_eq!(bridge.capacities.into_inner(), [0]);
        assert_eq!(clock.calls.get(), 2);
        let mut store =
            UnixPersonalWorkerStore::open_or_recover_scale_set_inbox(root.path()).unwrap();
        let (inbox, catalog) = store.load_scale_set_control_state(&identity).unwrap();
        assert_eq!(
            catalog.find_active(&attempt_id).unwrap().attempt().phase(),
            DisposableAttemptPhase::CloneAuthorized
        );
        assert_eq!(inbox.pending().unwrap().message_id(), 7);
    }

    #[test]
    fn successful_clone_checkpoints_once_and_binds_the_observed_host_identity() {
        let root = TempRoot::new("success");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-success",
            SOURCE,
            SOURCE_DISK,
        );
        let runtime = runtime(&root, &host);
        install_ready_generation(&root, &host, &runtime);
        let attempt_id = install_authorized_attempt(&root);
        let executor = executor(&host, false);
        let admission = FakeAdmission::available();

        let receipt = runtime
            .clone_once_with(&attempt_id, &admission, &executor, &FixedClock)
            .unwrap();
        assert_eq!(receipt.schema_version(), 1);
        assert_eq!(receipt.attempt_revision(), 4);
        assert_eq!(admission.calls.get(), 1);
        assert_eq!(executor.clone_count(), 1);
        let attempt = durable_attempt(&root, &attempt_id);
        assert_eq!(attempt.phase(), DisposableAttemptPhase::CloneStarted);
        assert!(attempt.vm_identity().is_some());
    }

    #[test]
    fn admission_sample_precedes_its_validation_clock() {
        let root = TempRoot::new("admission-clock-order");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-admission-clock-order",
            SOURCE,
            SOURCE_DISK,
        );
        let runtime = runtime(&root, &host);
        install_ready_generation(&root, &host, &runtime);
        let attempt_id = install_authorized_attempt(&root);
        let executor = executor(&host, false);
        let observed = Rc::new(Cell::new(false));
        let admission = OrderedAdmission {
            observed: Rc::clone(&observed),
        };
        let clock = AdmissionOrderedClock { observed };

        let receipt = runtime
            .clone_once_with(&attempt_id, &admission, &executor, &clock)
            .unwrap();

        assert_eq!(receipt.attempt_revision(), 4);
        assert_eq!(executor.clone_count(), 1);
    }

    #[test]
    fn failed_clone_leaves_unbound_started_debt_and_never_replays() {
        let root = TempRoot::new("failure");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-failure",
            SOURCE,
            SOURCE_DISK,
        );
        let runtime = runtime(&root, &host);
        install_ready_generation(&root, &host, &runtime);
        let attempt_id = install_authorized_attempt(&root);
        let executor = executor(&host, true);
        let admission = FakeAdmission::available();

        let error = runtime
            .clone_once_with(&attempt_id, &admission, &executor, &FixedClock)
            .unwrap_err();
        assert_eq!(error.kind(), DisposableCloneRuntimeErrorKind::Command);
        let attempt = durable_attempt(&root, &attempt_id);
        assert_eq!(attempt.phase(), DisposableAttemptPhase::CloneStarted);
        assert!(attempt.vm_identity().is_none());

        let current = durable_attempt(&root, &attempt_id);
        assert_eq!(current.phase(), DisposableAttemptPhase::CloneStarted);
        let retry = runtime
            .clone_once_with(&attempt_id, &admission, &executor, &FixedClock)
            .unwrap_err();
        assert_eq!(
            retry.kind(),
            DisposableCloneRuntimeErrorKind::RecoveryRequired
        );
        assert_eq!(executor.clone_count(), 1);
    }

    #[test]
    fn expiry_during_final_preflight_blocks_before_checkpoint() {
        let root = TempRoot::new("deadline");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-deadline",
            SOURCE,
            SOURCE_DISK,
        );
        let runtime = runtime(&root, &host);
        install_ready_generation(&root, &host, &runtime);
        let attempt_id = install_authorized_attempt(&root);
        let executor = executor(&host, false);
        let admission = FakeAdmission::available();
        let clock = SequencedClock {
            calls: Cell::new(0),
            final_millis: 1_900_000_700_000,
        };

        let error = runtime
            .clone_once_with(&attempt_id, &admission, &executor, &clock)
            .unwrap_err();
        assert_eq!(error.kind(), DisposableCloneRuntimeErrorKind::Observation);
        assert_eq!(error.code(), "clone_admission_stale");
        assert_eq!(executor.clone_count(), 0);
        let attempt = durable_attempt(&root, &attempt_id);
        assert_eq!(attempt.phase(), DisposableAttemptPhase::CloneAuthorized);
        assert!(attempt.vm_identity().is_none());
    }

    #[test]
    fn lost_capacity_before_checkpoint_blocks_without_consuming_clone_authority() {
        let root = TempRoot::new("capacity-lost");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-capacity-lost",
            SOURCE,
            SOURCE_DISK,
        );
        let runtime = runtime(&root, &host);
        install_ready_generation(&root, &host, &runtime);
        let attempt_id = install_authorized_attempt(&root);
        let executor = executor(&host, false);
        let admission = FakeAdmission {
            calls: Cell::new(0),
            lose_capacity_on: Some(1),
            cancel_on: None,
        };

        let error = runtime
            .clone_once_with(&attempt_id, &admission, &executor, &FixedClock)
            .unwrap_err();
        assert_eq!(error.code(), "clone_capacity_lost");
        assert_eq!(executor.clone_count(), 0);
        let attempt = durable_attempt(&root, &attempt_id);
        assert_eq!(attempt.phase(), DisposableAttemptPhase::CloneAuthorized);
    }

    #[test]
    fn cancellation_at_final_admission_barrier_blocks_before_checkpoint() {
        let root = TempRoot::new("cancelled");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-cancelled",
            SOURCE,
            SOURCE_DISK,
        );
        let runtime = runtime(&root, &host);
        install_ready_generation(&root, &host, &runtime);
        let attempt_id = install_authorized_attempt(&root);
        let executor = executor(&host, false);
        let admission = FakeAdmission {
            calls: Cell::new(0),
            lose_capacity_on: None,
            cancel_on: Some(1),
        };

        let error = runtime
            .clone_once_with(&attempt_id, &admission, &executor, &FixedClock)
            .unwrap_err();
        assert_eq!(error.code(), "clone_cancelled");
        assert_eq!(executor.clone_count(), 0);
        let attempt = durable_attempt(&root, &attempt_id);
        assert_eq!(attempt.phase(), DisposableAttemptPhase::CloneAuthorized);
        assert!(attempt.vm_identity().is_none());
    }

    #[test]
    fn target_drift_during_final_source_observation_is_never_bound() {
        let root = TempRoot::new("target-drift");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-target-drift",
            SOURCE,
            SOURCE_DISK,
        );
        let runtime = runtime(&root, &host);
        install_ready_generation(&root, &host, &runtime);
        let attempt_id = install_authorized_attempt(&root);
        let mut executor = executor(&host, false);
        executor.rewrite_target_during_final_source = true;
        let admission = FakeAdmission::available();

        let error = runtime
            .clone_once_with(&attempt_id, &admission, &executor, &FixedClock)
            .unwrap_err();
        assert_eq!(error.code(), "clone_host_identity_drift");
        assert_eq!(executor.clone_count(), 1);
        let attempt = durable_attempt(&root, &attempt_id);
        assert_eq!(attempt.phase(), DisposableAttemptPhase::CloneStarted);
        assert!(attempt.vm_identity().is_none());
    }

    #[test]
    fn final_lima_version_drift_blocks_immediately_before_clone() {
        let root = TempRoot::new("version-drift");
        let host = LimaHostIdentityFixture::new_with_disk_bytes(
            "clone-runtime-version-drift",
            SOURCE,
            SOURCE_DISK,
        );
        let runtime = runtime(&root, &host);
        install_ready_generation(&root, &host, &runtime);
        let attempt_id = install_authorized_attempt(&root);
        let mut executor = executor(&host, false);
        executor.drift_version_on = Some(6);
        let admission = FakeAdmission::available();

        let error = runtime
            .clone_once_with(&attempt_id, &admission, &executor, &FixedClock)
            .unwrap_err();
        assert_eq!(error.code(), "clone_lima_version_mismatch");
        assert_eq!(executor.version_calls.get(), 6);
        assert_eq!(executor.clone_count(), 0);
        let attempt = durable_attempt(&root, &attempt_id);
        assert_eq!(attempt.phase(), DisposableAttemptPhase::CloneAuthorized);
        assert!(attempt.vm_identity().is_none());
    }
}
