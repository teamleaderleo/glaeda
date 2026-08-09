use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::execution_admission::EpochMillis;
use crate::lima_lifecycle::{
    LimaCacheDiskId, LimaCacheDiskIdentity, LimaInstanceId, LimaInstanceIdentity,
    LimaLifecycleObservation, LimaLifecycleState, LimaObservedResources, LimaProfileGeneration,
    LimaResourceProfile,
};
use crate::lima_observation::{
    LimaArchitecture, LimaFilesystemObjectIdentity, LimaGuestObservation, LimaInstanceName,
    LimaObservationRequest, LimaPersistentIdentity, LimaRuntimeState, LimaVmType,
};
use crate::operator_config::{
    OPERATOR_CONFIG_SCHEMA_VERSION, OperatorConfig, OperatorConfigIdentity,
};
use crate::personal_worker_mac_observation::PersonalWorkerMacObservation;
use crate::personal_worker_queue::{PersonalWorkerProfile, PersonalWorkerQueueGeneration};
use crate::personal_worker_store::PersonalWorkerStoreRevision;
use crate::personal_worker_tick::{PersonalWorkerTickAction, PersonalWorkerTickPlan};

pub const PERSONAL_WORKER_LIMA_AUTHORITY_SCHEMA_VERSION: u8 = 1;
pub const MAX_PERSONAL_WORKER_LIMA_AUTHORITY_BYTES: usize = 16_384;
const PERSONAL_WORKER_LIMA_AUTHORITY_DOCUMENT_TYPE: &str =
    "smolrunner.personal_worker_lima_authority";
const PERSONAL_WORKER_LIMA_ENROLLMENT_CONFIRMATION_PREFIX: &str =
    "personal-worker-lima-enrollment-v1.sha256:";
const MAX_AUTHORITY_GENERATION: u64 = 1_000_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct PersonalWorkerLimaAuthorityGeneration(u64);

impl PersonalWorkerLimaAuthorityGeneration {
    /// Construct one bounded positive authority generation.
    ///
    /// # Errors
    ///
    /// Returns an error for zero or a value beyond the reviewed bound.
    pub fn new(value: u64) -> Result<Self, PersonalWorkerLimaAuthorityError> {
        bounded_generation("authority_generation", value).map(Self)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    fn next(self) -> Result<Self, PersonalWorkerLimaAuthorityError> {
        let next = self.0.checked_add(1).ok_or_else(|| {
            PersonalWorkerLimaAuthorityError::invalid("Lima authority generation cannot advance")
        })?;
        Self::new(next)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct PersonalWorkerLimaAttemptGeneration(u64);

impl PersonalWorkerLimaAttemptGeneration {
    /// Construct one bounded positive lifecycle-attempt generation.
    ///
    /// # Errors
    ///
    /// Returns an error for zero or a value beyond the reviewed bound.
    pub fn new(value: u64) -> Result<Self, PersonalWorkerLimaAuthorityError> {
        bounded_generation("attempt.generation", value).map(Self)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerLimaAction {
    Start,
    StopToStopped,
    StopToInteractive,
    StopToWork,
}

impl PersonalWorkerLimaAction {
    const fn target_profile(self, before: LimaResourceProfile) -> LimaResourceProfile {
        match self {
            Self::Start | Self::StopToStopped => before,
            Self::StopToInteractive => LimaResourceProfile::Interactive,
            Self::StopToWork => LimaResourceProfile::Work,
        }
    }

    const fn target_state(self) -> LimaLifecycleState {
        match self {
            Self::StopToStopped => LimaLifecycleState::Stopped,
            Self::StopToInteractive | Self::StopToWork => LimaLifecycleState::Running,
            Self::Start => LimaLifecycleState::Running,
        }
    }

    fn target_generation(
        self,
        before: LimaProfileGeneration,
    ) -> Result<LimaProfileGeneration, PersonalWorkerLimaAuthorityError> {
        match self {
            Self::Start | Self::StopToStopped => Ok(before),
            Self::StopToInteractive | Self::StopToWork => {
                let next = before.get().checked_add(1).ok_or_else(|| {
                    PersonalWorkerLimaAuthorityError::invalid(
                        "attempt.after_profile_generation cannot advance",
                    )
                })?;
                LimaProfileGeneration::new(next).map_err(|_| {
                    PersonalWorkerLimaAuthorityError::invalid(
                        "attempt.after_profile_generation cannot advance",
                    )
                })
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerLimaAttemptPhase {
    Prepared,
    StopStarted,
    StopCompleted,
    EditStarted,
    EditCompleted,
    StartStarted,
    StartCompleted,
    VerifyStarted,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerLimaAttempt {
    generation: PersonalWorkerLimaAttemptGeneration,
    store_revision: PersonalWorkerStoreRevision,
    queue_generation: PersonalWorkerQueueGeneration,
    decision_at: EpochMillis,
    action: PersonalWorkerLimaAction,
    identity: LimaInstanceIdentity,
    before_state: LimaLifecycleState,
    before_profile: LimaResourceProfile,
    before_profile_generation: LimaProfileGeneration,
    before_resources: LimaObservedResources,
    after_state: LimaLifecycleState,
    after_profile: LimaResourceProfile,
    after_profile_generation: LimaProfileGeneration,
    phase: PersonalWorkerLimaAttemptPhase,
    checkpoint_at: EpochMillis,
    #[serde(skip_serializing_if = "Option::is_none")]
    completed_at: Option<EpochMillis>,
}

pub struct PersonalWorkerLimaAttemptInput<'a> {
    pub config: &'a OperatorConfig,
    pub mac: &'a PersonalWorkerMacObservation,
    pub request: &'a LimaObservationRequest,
    pub lifecycle: &'a LimaLifecycleObservation,
    pub tick: &'a PersonalWorkerTickPlan,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerLimaEnrollmentConfirmation {
    schema_version: u8,
    value: String,
}

impl PersonalWorkerLimaEnrollmentConfirmation {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }
}

impl PersonalWorkerLimaAttempt {
    #[must_use]
    pub const fn generation(&self) -> PersonalWorkerLimaAttemptGeneration {
        self.generation
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
    pub const fn action(&self) -> PersonalWorkerLimaAction {
        self.action
    }

    #[must_use]
    pub const fn phase(&self) -> PersonalWorkerLimaAttemptPhase {
        self.phase
    }

    #[must_use]
    pub const fn after_profile(&self) -> LimaResourceProfile {
        self.after_profile
    }

    #[must_use]
    pub const fn completed_at(&self) -> Option<EpochMillis> {
        self.completed_at
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct PersonalWorkerLimaAuthorityDocument {
    authority_generation: PersonalWorkerLimaAuthorityGeneration,
    config_identity: EnrolledConfigIdentity,
    request_identity_digest: Sha256Digest,
    lima_instance: LimaInstanceName,
    identity: LimaInstanceIdentity,
    expected_vm_type: LimaVmType,
    expected_architecture: LimaArchitecture,
    persistent_identity: LimaPersistentIdentity,
    attempt: Option<PersonalWorkerLimaAttempt>,
}

impl fmt::Debug for PersonalWorkerLimaAuthorityDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersonalWorkerLimaAuthorityDocument")
            .field("authority_generation", &self.authority_generation)
            .field("config_identity", &self.config_identity)
            .field("request_identity_digest", &self.request_identity_digest)
            .field("lima_instance", &self.lima_instance)
            .field("identity", &self.identity)
            .field("expected_vm_type", &self.expected_vm_type)
            .field("expected_architecture", &self.expected_architecture)
            .field("persistent_identity", &self.persistent_identity)
            .field("attempt", &self.attempt)
            .finish()
    }
}

impl PersonalWorkerLimaAuthorityDocument {
    /// Enroll one exact running guest from sealed B02 evidence after explicit operator approval.
    ///
    /// The persistent guest identity is always derived from the sealed observation. The caller can
    /// name the logical broker identity being approved, but cannot supply machine, root, or cache
    /// identity evidence.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal unless the exact config/request/B02 evidence is running,
    /// internally coherent, and explicitly approved. The later durable enrollment service owns
    /// the injected-clock freshness check immediately before publication.
    pub fn enroll(
        config: &OperatorConfig,
        mac: &PersonalWorkerMacObservation,
        request: &LimaObservationRequest,
        approved_identity: LimaInstanceIdentity,
        supplied_confirmation: Option<&str>,
    ) -> Result<Self, PersonalWorkerLimaAuthorityError> {
        let candidate = enrollment_candidate(config, mac, request, approved_identity)?;
        let confirmation = confirmation_for_document(&candidate)?;
        let Some(supplied_confirmation) = supplied_confirmation else {
            return Err(PersonalWorkerLimaAuthorityError::new(
                PersonalWorkerLimaAuthorityErrorKind::ConfirmationRequired,
                "explicit operator confirmation is required for Lima enrollment",
            ));
        };
        if supplied_confirmation != confirmation.value() {
            return Err(PersonalWorkerLimaAuthorityError::conflict(
                "the supplied Lima enrollment confirmation does not match the exact candidate",
            ));
        }
        Ok(candidate)
    }

    #[must_use]
    pub const fn authority_generation(&self) -> PersonalWorkerLimaAuthorityGeneration {
        self.authority_generation
    }

    #[must_use]
    pub const fn identity(&self) -> &LimaInstanceIdentity {
        &self.identity
    }

    #[must_use]
    pub const fn persistent_identity(&self) -> &LimaPersistentIdentity {
        &self.persistent_identity
    }

    #[must_use]
    pub const fn attempt(&self) -> Option<&PersonalWorkerLimaAttempt> {
        self.attempt.as_ref()
    }

    /// Start one exact attempt in the durable `Prepared` phase.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal for enrollment drift, active lifecycle work, an existing attempt,
    /// an impossible pre-state, or exhausted generations.
    pub fn begin_attempt(
        &self,
        input: PersonalWorkerLimaAttemptInput<'_>,
    ) -> Result<Self, PersonalWorkerLimaAuthorityError> {
        if self.attempt.is_some() {
            return Err(PersonalWorkerLimaAuthorityError::new(
                PersonalWorkerLimaAuthorityErrorKind::RecoveryRequired,
                "an existing Lima lifecycle attempt requires recovery",
            ));
        }
        self.validate_enrollment(input.config, input.mac, input.request)?;
        let action = action_from_tick(self, input.lifecycle, input.tick)?;
        validate_pre_lifecycle(self, input.lifecycle, action, input.tick.decision_at())?;
        validate_pre_report(self, input.mac, input.lifecycle, input.tick.decision_at())?;
        let after_profile = action.target_profile(input.lifecycle.profile());
        let after_profile_generation =
            action.target_generation(input.lifecycle.profile_generation())?;
        let attempt = PersonalWorkerLimaAttempt {
            generation: PersonalWorkerLimaAttemptGeneration::new(self.authority_generation.get())?,
            store_revision: input.tick.store_revision(),
            queue_generation: input.tick.queue_generation(),
            decision_at: input.tick.decision_at(),
            action,
            identity: self.identity.clone(),
            before_state: input.lifecycle.state(),
            before_profile: input.lifecycle.profile(),
            before_profile_generation: input.lifecycle.profile_generation(),
            before_resources: input.lifecycle.observed_resources(),
            after_state: action.target_state(),
            after_profile,
            after_profile_generation,
            phase: PersonalWorkerLimaAttemptPhase::Prepared,
            checkpoint_at: input.tick.decision_at(),
            completed_at: None,
        };
        let mut next = self.clone();
        next.authority_generation = self.authority_generation.next()?;
        next.attempt = Some(attempt);
        Ok(next)
    }

    /// Advance an existing attempt through one exact command checkpoint.
    ///
    /// `Completed` is intentionally unavailable here; completion requires fresh sealed B02 and
    /// lifecycle verification through [`Self::complete_attempt`].
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal for a missing/stale attempt, invalid phase edge, time reversal,
    /// or exhausted authority generation.
    pub fn checkpoint(
        &self,
        attempt_generation: PersonalWorkerLimaAttemptGeneration,
        next_phase: PersonalWorkerLimaAttemptPhase,
        checkpoint_at: EpochMillis,
    ) -> Result<Self, PersonalWorkerLimaAuthorityError> {
        let current = self.exact_attempt(attempt_generation)?;
        if next_phase == PersonalWorkerLimaAttemptPhase::Completed
            || !valid_phase_edge(
                current.action,
                current.before_state,
                current.phase,
                next_phase,
            )
        {
            return Err(PersonalWorkerLimaAuthorityError::invalid(
                "the Lima lifecycle checkpoint does not follow the exact action phase graph",
            ));
        }
        if checkpoint_at < current.checkpoint_at {
            return Err(PersonalWorkerLimaAuthorityError::invalid(
                "the Lima lifecycle checkpoint time cannot move backwards",
            ));
        }
        let mut next = self.clone();
        next.authority_generation = self.authority_generation.next()?;
        let attempt = next.attempt.as_mut().expect("exact attempt exists");
        attempt.phase = next_phase;
        attempt.checkpoint_at = checkpoint_at;
        Ok(next)
    }

    /// Complete verification using the same enrollment plus fresh sealed B02/lifecycle evidence.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal unless the attempt is at `VerifyStarted` and exact post-state,
    /// profile, resources, generation, and running persistent identity evidence all match.
    pub fn complete_attempt(
        &self,
        attempt_generation: PersonalWorkerLimaAttemptGeneration,
        config: &OperatorConfig,
        mac: &PersonalWorkerMacObservation,
        request: &LimaObservationRequest,
        lifecycle: &LimaLifecycleObservation,
        completed_at: EpochMillis,
    ) -> Result<Self, PersonalWorkerLimaAuthorityError> {
        let current = self.exact_attempt(attempt_generation)?;
        if current.phase != PersonalWorkerLimaAttemptPhase::VerifyStarted {
            return Err(PersonalWorkerLimaAuthorityError::invalid(
                "the Lima lifecycle attempt is not ready for completion verification",
            ));
        }
        self.validate_enrollment(config, mac, request)?;
        if lifecycle.identity() != &self.identity
            || lifecycle.state() != current.after_state
            || lifecycle.profile() != current.after_profile
            || lifecycle.profile_generation() != current.after_profile_generation
            || lifecycle.observed_resources()
                != LimaObservedResources::for_profile(current.after_profile)
            || lifecycle.active_reservation_id().is_some()
        {
            return Err(PersonalWorkerLimaAuthorityError::conflict(
                "post-lifecycle evidence does not match the exact attempted successor",
            ));
        }
        validate_post_report(self, mac, current)?;
        if lifecycle.observed_at() < current.checkpoint_at
            || mac.report().timing.started_at_millis < current.checkpoint_at.get()
            || completed_at < current.checkpoint_at
            || completed_at < lifecycle.observed_at()
            || completed_at.get() < mac.report().timing.observed_at_millis
            || completed_at.get() > mac.report().timing.expires_at_millis
        {
            return Err(PersonalWorkerLimaAuthorityError::invalid(
                "completion time cannot precede verified lifecycle evidence",
            ));
        }
        let mut next = self.clone();
        next.authority_generation = self.authority_generation.next()?;
        let attempt = next.attempt.as_mut().expect("exact attempt exists");
        attempt.phase = PersonalWorkerLimaAttemptPhase::Completed;
        attempt.checkpoint_at = completed_at;
        attempt.completed_at = Some(completed_at);
        Ok(next)
    }

    fn validate_enrollment(
        &self,
        config: &OperatorConfig,
        mac: &PersonalWorkerMacObservation,
        request: &LimaObservationRequest,
    ) -> Result<(), PersonalWorkerLimaAuthorityError> {
        validate_exact_source(config, mac, request)?;
        if !self.config_identity.matches(config.identity())
            || self.request_identity_digest != *request.request_identity().digest()
            || self.lima_instance != *request.instance()
            || self.identity.instance_id().as_str() != request.instance().as_str()
            || self.expected_vm_type != request.expected_vm_type()
            || self.expected_architecture != request.expected_architecture()
        {
            return Err(PersonalWorkerLimaAuthorityError::conflict(
                "current config or complete Lima request differs from durable enrollment",
            ));
        }
        Ok(())
    }

    fn exact_attempt(
        &self,
        generation: PersonalWorkerLimaAttemptGeneration,
    ) -> Result<&PersonalWorkerLimaAttempt, PersonalWorkerLimaAuthorityError> {
        let attempt = self.attempt.as_ref().ok_or_else(|| {
            PersonalWorkerLimaAuthorityError::invalid("no Lima lifecycle attempt exists")
        })?;
        if attempt.generation != generation {
            return Err(PersonalWorkerLimaAuthorityError::conflict(
                "Lima lifecycle attempt generation does not match",
            ));
        }
        Ok(attempt)
    }
}

/// Derive the exact path-private challenge an operator must supply to approve enrollment.
///
/// # Errors
///
/// Returns a bounded refusal unless sealed B02 evidence proves the exact candidate is running and
/// coherent with the accepted config and complete observation request.
pub fn personal_worker_lima_enrollment_confirmation(
    config: &OperatorConfig,
    mac: &PersonalWorkerMacObservation,
    request: &LimaObservationRequest,
    approved_identity: LimaInstanceIdentity,
) -> Result<PersonalWorkerLimaEnrollmentConfirmation, PersonalWorkerLimaAuthorityError> {
    let candidate = enrollment_candidate(config, mac, request, approved_identity)?;
    confirmation_for_document(&candidate)
}

fn enrollment_candidate(
    config: &OperatorConfig,
    mac: &PersonalWorkerMacObservation,
    request: &LimaObservationRequest,
    approved_identity: LimaInstanceIdentity,
) -> Result<PersonalWorkerLimaAuthorityDocument, PersonalWorkerLimaAuthorityError> {
    validate_exact_source(config, mac, request)?;
    let report = mac.report();
    if report.lima.configured.runtime_state != LimaRuntimeState::Running {
        return Err(PersonalWorkerLimaAuthorityError::invalid(
            "Lima enrollment requires sealed running guest evidence",
        ));
    }
    let LimaGuestObservation::Observed(guest) = &report.lima.guest else {
        return Err(PersonalWorkerLimaAuthorityError::invalid(
            "Lima enrollment requires an observed running guest",
        ));
    };
    if approved_identity.instance_id().as_str() != request.instance().as_str() {
        return Err(PersonalWorkerLimaAuthorityError::conflict(
            "the approved broker instance does not match the sealed physical Lima instance",
        ));
    }
    if guest.resources.architecture != request.expected_architecture() {
        return Err(PersonalWorkerLimaAuthorityError::invalid(
            "sealed Lima guest architecture does not match the enrolled request",
        ));
    }
    Ok(PersonalWorkerLimaAuthorityDocument {
        authority_generation: PersonalWorkerLimaAuthorityGeneration::new(1)?,
        config_identity: EnrolledConfigIdentity::from_config(config.identity()),
        request_identity_digest: request.request_identity().digest().clone(),
        lima_instance: request.instance().clone(),
        identity: approved_identity,
        expected_vm_type: request.expected_vm_type(),
        expected_architecture: request.expected_architecture(),
        persistent_identity: guest.persistent_identity.clone(),
        attempt: None,
    })
}

fn confirmation_for_document(
    document: &PersonalWorkerLimaAuthorityDocument,
) -> Result<PersonalWorkerLimaEnrollmentConfirmation, PersonalWorkerLimaAuthorityError> {
    let bytes = encode_personal_worker_lima_authority(document)?;
    let digest = Sha256::digest(bytes);
    Ok(PersonalWorkerLimaEnrollmentConfirmation {
        schema_version: PERSONAL_WORKER_LIMA_AUTHORITY_SCHEMA_VERSION,
        value: format!("{PERSONAL_WORKER_LIMA_ENROLLMENT_CONFIRMATION_PREFIX}{digest:x}"),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EnrolledConfigIdentity {
    schema_version: u8,
    digest: Sha256Digest,
}

impl EnrolledConfigIdentity {
    fn from_config(identity: &OperatorConfigIdentity) -> Self {
        Self {
            schema_version: identity.schema_version(),
            digest: identity.digest().clone(),
        }
    }

    fn matches(&self, identity: &OperatorConfigIdentity) -> bool {
        self.schema_version == identity.schema_version() && self.digest == *identity.digest()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerLimaAuthorityErrorKind {
    ConfirmationRequired,
    InvalidInput,
    Conflict,
    RecoveryRequired,
    VersionIncompatible,
    CorruptDocument,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerLimaAuthorityError {
    pub kind: PersonalWorkerLimaAuthorityErrorKind,
    pub code: &'static str,
    pub message: &'static str,
}

impl PersonalWorkerLimaAuthorityError {
    const fn new(kind: PersonalWorkerLimaAuthorityErrorKind, message: &'static str) -> Self {
        Self {
            kind,
            code: match kind {
                PersonalWorkerLimaAuthorityErrorKind::ConfirmationRequired => {
                    "confirmation_required"
                }
                PersonalWorkerLimaAuthorityErrorKind::InvalidInput => "invalid_input",
                PersonalWorkerLimaAuthorityErrorKind::Conflict => "authority_conflict",
                PersonalWorkerLimaAuthorityErrorKind::RecoveryRequired => "recovery_required",
                PersonalWorkerLimaAuthorityErrorKind::VersionIncompatible => {
                    "durable_state_version_incompatible"
                }
                PersonalWorkerLimaAuthorityErrorKind::CorruptDocument => "corrupt_document",
            },
            message,
        }
    }

    const fn invalid(message: &'static str) -> Self {
        Self::new(PersonalWorkerLimaAuthorityErrorKind::InvalidInput, message)
    }

    const fn conflict(message: &'static str) -> Self {
        Self::new(PersonalWorkerLimaAuthorityErrorKind::Conflict, message)
    }
}

impl fmt::Debug for PersonalWorkerLimaAuthorityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersonalWorkerLimaAuthorityError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for PersonalWorkerLimaAuthorityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for PersonalWorkerLimaAuthorityError {}

fn validate_exact_source(
    config: &OperatorConfig,
    mac: &PersonalWorkerMacObservation,
    request: &LimaObservationRequest,
) -> Result<(), PersonalWorkerLimaAuthorityError> {
    if mac.report().config_identity != *config.identity()
        || mac.report().lima.instance != *request.instance()
        || mac.lima_source_identity() != &request.source_identity()
        || mac.lima_request_identity() != request.request_identity()
        || config.lima_instance() != request.instance()
    {
        return Err(PersonalWorkerLimaAuthorityError::conflict(
            "sealed Mac evidence does not bind the exact config and complete Lima request",
        ));
    }
    if mac.report().lima.configured.vm_type != request.expected_vm_type()
        || mac.report().lima.configured.architecture != request.expected_architecture()
    {
        return Err(PersonalWorkerLimaAuthorityError::conflict(
            "sealed Lima configuration does not match the exact request",
        ));
    }
    Ok(())
}

fn validate_pre_lifecycle(
    authority: &PersonalWorkerLimaAuthorityDocument,
    lifecycle: &LimaLifecycleObservation,
    action: PersonalWorkerLimaAction,
    decision_at: EpochMillis,
) -> Result<(), PersonalWorkerLimaAuthorityError> {
    if lifecycle.identity() != &authority.identity || lifecycle.active_reservation_id().is_some() {
        return Err(PersonalWorkerLimaAuthorityError::conflict(
            "lifecycle identity or active-work evidence conflicts with durable enrollment",
        ));
    }
    if decision_at < lifecycle.observed_at()
        || lifecycle.observed_resources() != LimaObservedResources::for_profile(lifecycle.profile())
    {
        return Err(PersonalWorkerLimaAuthorityError::invalid(
            "lifecycle pre-observation timing or resources are invalid",
        ));
    }
    let state_ok = match action {
        // A stopped B02 report cannot prove that the currently named instance still owns the
        // enrolled machine/root/cache. Start remains blocked until stopped-host identity is
        // independently re-observable.
        PersonalWorkerLimaAction::Start => false,
        PersonalWorkerLimaAction::StopToStopped
        | PersonalWorkerLimaAction::StopToInteractive
        | PersonalWorkerLimaAction::StopToWork => lifecycle.state() == LimaLifecycleState::Running,
    };
    let profile_ok = match action {
        PersonalWorkerLimaAction::StopToInteractive => {
            lifecycle.profile() == LimaResourceProfile::Work
        }
        PersonalWorkerLimaAction::StopToWork => {
            lifecycle.profile() == LimaResourceProfile::Interactive
        }
        PersonalWorkerLimaAction::Start | PersonalWorkerLimaAction::StopToStopped => true,
    };
    if !state_ok || !profile_ok {
        return Err(PersonalWorkerLimaAuthorityError::invalid(
            "lifecycle action does not match the exact pre-state and profile",
        ));
    }
    Ok(())
}

fn action_from_tick(
    authority: &PersonalWorkerLimaAuthorityDocument,
    lifecycle: &LimaLifecycleObservation,
    tick: &PersonalWorkerTickPlan,
) -> Result<PersonalWorkerLimaAction, PersonalWorkerLimaAuthorityError> {
    let (identity, action, profile_ok, generation_ok) = match tick.action() {
        PersonalWorkerTickAction::StartVm {
            identity,
            profile,
            profile_generation,
            ..
        } => (
            identity,
            PersonalWorkerLimaAction::Start,
            *profile == lifecycle.profile(),
            *profile_generation == lifecycle.profile_generation(),
        ),
        PersonalWorkerTickAction::StopVm {
            identity,
            current_profile,
            profile_generation,
            target_after_stop,
        } => {
            let action = match target_after_stop {
                PersonalWorkerProfile::Stopped => PersonalWorkerLimaAction::StopToStopped,
                PersonalWorkerProfile::Interactive => PersonalWorkerLimaAction::StopToInteractive,
                PersonalWorkerProfile::Work => PersonalWorkerLimaAction::StopToWork,
            };
            (
                identity,
                action,
                *current_profile == lifecycle.profile(),
                *profile_generation == lifecycle.profile_generation(),
            )
        }
        PersonalWorkerTickAction::ChangeProfile { .. } => {
            return Err(PersonalWorkerLimaAuthorityError::invalid(
                "a stopped profile edit lacks current immutable ownership evidence",
            ));
        }
        _ => {
            return Err(PersonalWorkerLimaAuthorityError::invalid(
                "the sealed personal-worker tick does not contain a lifecycle action",
            ));
        }
    };
    if identity != &authority.identity
        || identity != lifecycle.identity()
        || !profile_ok
        || !generation_ok
    {
        return Err(PersonalWorkerLimaAuthorityError::conflict(
            "the sealed personal-worker tick does not match enrolled lifecycle evidence",
        ));
    }
    Ok(action)
}

fn validate_pre_report(
    authority: &PersonalWorkerLimaAuthorityDocument,
    mac: &PersonalWorkerMacObservation,
    lifecycle: &LimaLifecycleObservation,
    decision_at: EpochMillis,
) -> Result<(), PersonalWorkerLimaAuthorityError> {
    let report = &mac.report().lima;
    if mac.report().timing.observed_at_millis > decision_at.get()
        || mac.report().timing.expires_at_millis < decision_at.get()
    {
        return Err(PersonalWorkerLimaAuthorityError::conflict(
            "sealed pre-observation is not fresh at the exact tick decision",
        ));
    }
    let expected_runtime = if lifecycle.state() == LimaLifecycleState::Stopped {
        LimaRuntimeState::Stopped
    } else {
        LimaRuntimeState::Running
    };
    let envelope = lifecycle.profile().envelope();
    if report.configured.runtime_state != expected_runtime
        || report.configured.cpus != envelope.vcpus
        || report.configured.memory_bytes != envelope.memory_bytes
    {
        return Err(PersonalWorkerLimaAuthorityError::conflict(
            "sealed pre-observation does not match the lifecycle pre-state and profile",
        ));
    }
    match (&report.guest, expected_runtime) {
        (LimaGuestObservation::Observed(guest), LimaRuntimeState::Running)
            if guest.persistent_identity == authority.persistent_identity
                && guest.resources.architecture == authority.expected_architecture =>
        {
            Ok(())
        }
        (LimaGuestObservation::NotRunning { runtime_state }, LimaRuntimeState::Stopped)
            if *runtime_state == LimaRuntimeState::Stopped =>
        {
            Ok(())
        }
        _ => Err(PersonalWorkerLimaAuthorityError::conflict(
            "sealed pre-observation guest identity or state conflicts with enrollment",
        )),
    }
}

fn validate_post_report(
    authority: &PersonalWorkerLimaAuthorityDocument,
    mac: &PersonalWorkerMacObservation,
    attempt: &PersonalWorkerLimaAttempt,
) -> Result<(), PersonalWorkerLimaAuthorityError> {
    let report = &mac.report().lima;
    let expected_runtime = if attempt.after_state == LimaLifecycleState::Stopped {
        LimaRuntimeState::Stopped
    } else {
        LimaRuntimeState::Running
    };
    let envelope = attempt.after_profile.envelope();
    if report.configured.runtime_state != expected_runtime
        || report.configured.cpus != envelope.vcpus
        || report.configured.memory_bytes != envelope.memory_bytes
    {
        return Err(PersonalWorkerLimaAuthorityError::conflict(
            "sealed post-observation does not match the attempted state and profile",
        ));
    }
    match (&report.guest, expected_runtime) {
        (LimaGuestObservation::Observed(guest), LimaRuntimeState::Running)
            if guest.persistent_identity == authority.persistent_identity
                && guest.resources.architecture == authority.expected_architecture =>
        {
            Ok(())
        }
        (LimaGuestObservation::NotRunning { runtime_state }, LimaRuntimeState::Stopped)
            if *runtime_state == LimaRuntimeState::Stopped =>
        {
            Ok(())
        }
        _ => Err(PersonalWorkerLimaAuthorityError::conflict(
            "sealed post-observation guest identity or state conflicts with enrollment",
        )),
    }
}

const fn valid_phase_edge(
    action: PersonalWorkerLimaAction,
    before_state: LimaLifecycleState,
    current: PersonalWorkerLimaAttemptPhase,
    next: PersonalWorkerLimaAttemptPhase,
) -> bool {
    use PersonalWorkerLimaAttemptPhase as Phase;
    match action {
        PersonalWorkerLimaAction::Start => matches!(
            (current, next),
            (Phase::Prepared, Phase::StartStarted)
                | (Phase::StartStarted, Phase::StartCompleted)
                | (Phase::StartCompleted, Phase::VerifyStarted)
        ),
        PersonalWorkerLimaAction::StopToStopped => matches!(
            (current, next),
            (Phase::Prepared, Phase::StopStarted)
                | (Phase::StopStarted, Phase::StopCompleted)
                | (Phase::StopCompleted, Phase::VerifyStarted)
        ),
        PersonalWorkerLimaAction::StopToInteractive | PersonalWorkerLimaAction::StopToWork => {
            if matches!(before_state, LimaLifecycleState::Running) {
                matches!(
                    (current, next),
                    (Phase::Prepared, Phase::StopStarted)
                        | (Phase::StopStarted, Phase::StopCompleted)
                        | (Phase::StopCompleted, Phase::EditStarted)
                        | (Phase::EditStarted, Phase::EditCompleted)
                        | (Phase::EditCompleted, Phase::StartStarted)
                        | (Phase::StartStarted, Phase::StartCompleted)
                        | (Phase::StartCompleted, Phase::VerifyStarted)
                )
            } else {
                false
            }
        }
    }
}

fn bounded_generation(
    field: &'static str,
    value: u64,
) -> Result<u64, PersonalWorkerLimaAuthorityError> {
    if !(1..=MAX_AUTHORITY_GENERATION).contains(&value) {
        return Err(PersonalWorkerLimaAuthorityError::invalid(match field {
            "authority_generation" => "authority generation is outside the bounded range",
            _ => "attempt generation is outside the bounded range",
        }));
    }
    Ok(value)
}

#[derive(Serialize)]
struct AuthorityWire<'a> {
    document_type: &'static str,
    schema_version: u8,
    authority_generation: u64,
    config_identity: ConfigIdentityWire<'a>,
    request_identity_digest: &'a str,
    lima_instance: &'a str,
    identity: InstanceIdentityWire<'a>,
    expected_vm_type: &'static str,
    expected_architecture: &'static str,
    persistent_identity: PersistentIdentityWire<'a>,
    attempt: Option<AttemptWire<'a>>,
}

#[derive(Serialize)]
struct ConfigIdentityWire<'a> {
    schema_version: u8,
    digest: &'a str,
}

#[derive(Serialize)]
struct InstanceIdentityWire<'a> {
    instance_id: &'a str,
    cache_disk_id: &'a str,
    cache_disk_identity_digest: &'a str,
}

#[derive(Serialize)]
struct PersistentIdentityWire<'a> {
    guest_machine_id_digest: &'a str,
    root_device_id: u64,
    root_inode: u64,
    cache_device_id: u64,
    cache_inode: u64,
}

#[derive(Serialize)]
struct AttemptWire<'a> {
    generation: u64,
    store_revision: u64,
    queue_generation: u64,
    decision_at: u64,
    action: &'static str,
    identity: InstanceIdentityWire<'a>,
    before_state: &'static str,
    before_profile: &'static str,
    before_profile_generation: u64,
    before_vcpus: u16,
    before_memory_bytes: u64,
    after_state: &'static str,
    after_profile: &'static str,
    after_profile_generation: u64,
    phase: &'static str,
    checkpoint_at: u64,
    completed_at: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAuthority {
    document_type: String,
    schema_version: u8,
    authority_generation: u64,
    config_identity: RawConfigIdentity,
    request_identity_digest: String,
    lima_instance: String,
    identity: RawInstanceIdentity,
    expected_vm_type: String,
    expected_architecture: String,
    persistent_identity: RawPersistentIdentity,
    attempt: Option<RawAttempt>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfigIdentity {
    schema_version: u8,
    digest: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawInstanceIdentity {
    instance_id: String,
    cache_disk_id: String,
    cache_disk_identity_digest: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPersistentIdentity {
    guest_machine_id_digest: String,
    root_device_id: u64,
    root_inode: u64,
    cache_device_id: u64,
    cache_inode: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAttempt {
    generation: u64,
    store_revision: u64,
    queue_generation: u64,
    decision_at: u64,
    action: String,
    identity: RawInstanceIdentity,
    before_state: String,
    before_profile: String,
    before_profile_generation: u64,
    before_vcpus: u16,
    before_memory_bytes: u64,
    after_state: String,
    after_profile: String,
    after_profile_generation: u64,
    phase: String,
    checkpoint_at: u64,
    completed_at: Option<u64>,
}

/// Encode one canonical bounded lifecycle-authority document.
///
/// # Errors
///
/// Returns a bounded error when the document cannot be represented within the fixed limit.
pub fn encode_personal_worker_lima_authority(
    document: &PersonalWorkerLimaAuthorityDocument,
) -> Result<Vec<u8>, PersonalWorkerLimaAuthorityError> {
    let wire = authority_wire(document);
    let bytes = serde_json::to_vec(&wire).map_err(|_| {
        PersonalWorkerLimaAuthorityError::new(
            PersonalWorkerLimaAuthorityErrorKind::CorruptDocument,
            "Lima authority document could not be encoded",
        )
    })?;
    if bytes.len() > MAX_PERSONAL_WORKER_LIMA_AUTHORITY_BYTES {
        return Err(PersonalWorkerLimaAuthorityError::new(
            PersonalWorkerLimaAuthorityErrorKind::CorruptDocument,
            "Lima authority document exceeds the bounded size",
        ));
    }
    Ok(bytes)
}

/// Decode one exact canonical lifecycle-authority document with strict field/version refusal.
///
/// # Errors
///
/// Returns a bounded version error for other schemas and corruption for malformed, noncanonical,
/// unknown-field, internally inconsistent, or oversized documents.
pub fn decode_personal_worker_lima_authority(
    bytes: &[u8],
) -> Result<PersonalWorkerLimaAuthorityDocument, PersonalWorkerLimaAuthorityError> {
    if bytes.len() > MAX_PERSONAL_WORKER_LIMA_AUTHORITY_BYTES {
        return Err(corrupt_document());
    }
    let raw: RawAuthority = serde_json::from_slice(bytes).map_err(|_| corrupt_document())?;
    if raw.document_type != PERSONAL_WORKER_LIMA_AUTHORITY_DOCUMENT_TYPE {
        return Err(corrupt_document());
    }
    if raw.schema_version != PERSONAL_WORKER_LIMA_AUTHORITY_SCHEMA_VERSION {
        return Err(PersonalWorkerLimaAuthorityError::new(
            PersonalWorkerLimaAuthorityErrorKind::VersionIncompatible,
            "Lima authority document requires an explicit supported migration",
        ));
    }
    let document = parse_raw_authority(raw).map_err(|_| corrupt_document())?;
    let canonical = encode_personal_worker_lima_authority(&document)?;
    if canonical != bytes {
        return Err(corrupt_document());
    }
    Ok(document)
}

fn authority_wire(document: &PersonalWorkerLimaAuthorityDocument) -> AuthorityWire<'_> {
    AuthorityWire {
        document_type: PERSONAL_WORKER_LIMA_AUTHORITY_DOCUMENT_TYPE,
        schema_version: PERSONAL_WORKER_LIMA_AUTHORITY_SCHEMA_VERSION,
        authority_generation: document.authority_generation.get(),
        config_identity: ConfigIdentityWire {
            schema_version: document.config_identity.schema_version,
            digest: document.config_identity.digest.as_str(),
        },
        request_identity_digest: document.request_identity_digest.as_str(),
        lima_instance: document.lima_instance.as_str(),
        identity: instance_wire(&document.identity),
        expected_vm_type: vm_type_name(document.expected_vm_type),
        expected_architecture: architecture_name(document.expected_architecture),
        persistent_identity: persistent_wire(&document.persistent_identity),
        attempt: document.attempt.as_ref().map(attempt_wire),
    }
}

fn instance_wire(identity: &LimaInstanceIdentity) -> InstanceIdentityWire<'_> {
    InstanceIdentityWire {
        instance_id: identity.instance_id().as_str(),
        cache_disk_id: identity.cache_disk().disk_id().as_str(),
        cache_disk_identity_digest: identity.cache_disk().identity_digest().as_str(),
    }
}

fn persistent_wire(identity: &LimaPersistentIdentity) -> PersistentIdentityWire<'_> {
    PersistentIdentityWire {
        guest_machine_id_digest: identity.guest_machine_id_digest.as_str(),
        root_device_id: identity.root_filesystem.device_id,
        root_inode: identity.root_filesystem.inode,
        cache_device_id: identity.cache_directory.device_id,
        cache_inode: identity.cache_directory.inode,
    }
}

fn attempt_wire(attempt: &PersonalWorkerLimaAttempt) -> AttemptWire<'_> {
    AttemptWire {
        generation: attempt.generation.get(),
        store_revision: attempt.store_revision.get(),
        queue_generation: attempt.queue_generation.get(),
        decision_at: attempt.decision_at.get(),
        action: action_name(attempt.action),
        identity: instance_wire(&attempt.identity),
        before_state: state_name(attempt.before_state),
        before_profile: profile_name(attempt.before_profile),
        before_profile_generation: attempt.before_profile_generation.get(),
        before_vcpus: attempt.before_resources.vcpus(),
        before_memory_bytes: attempt.before_resources.memory_bytes(),
        after_state: state_name(attempt.after_state),
        after_profile: profile_name(attempt.after_profile),
        after_profile_generation: attempt.after_profile_generation.get(),
        phase: phase_name(attempt.phase),
        checkpoint_at: attempt.checkpoint_at.get(),
        completed_at: attempt.completed_at.map(EpochMillis::get),
    }
}

fn parse_raw_authority(
    raw: RawAuthority,
) -> Result<PersonalWorkerLimaAuthorityDocument, PersonalWorkerLimaAuthorityError> {
    let identity = parse_instance(raw.identity)?;
    let attempt = raw.attempt.map(parse_attempt).transpose()?;
    let document = PersonalWorkerLimaAuthorityDocument {
        authority_generation: PersonalWorkerLimaAuthorityGeneration::new(raw.authority_generation)?,
        config_identity: EnrolledConfigIdentity {
            schema_version: raw.config_identity.schema_version,
            digest: parse_digest(&raw.config_identity.digest)?,
        },
        request_identity_digest: parse_digest(&raw.request_identity_digest)?,
        lima_instance: LimaInstanceName::parse(&raw.lima_instance)
            .map_err(|_| corrupt_document())?,
        identity,
        expected_vm_type: parse_vm_type(&raw.expected_vm_type)?,
        expected_architecture: parse_architecture(&raw.expected_architecture)?,
        persistent_identity: LimaPersistentIdentity {
            guest_machine_id_digest: parse_digest(
                &raw.persistent_identity.guest_machine_id_digest,
            )?,
            root_filesystem: LimaFilesystemObjectIdentity {
                device_id: raw.persistent_identity.root_device_id,
                inode: raw.persistent_identity.root_inode,
            },
            cache_directory: LimaFilesystemObjectIdentity {
                device_id: raw.persistent_identity.cache_device_id,
                inode: raw.persistent_identity.cache_inode,
            },
        },
        attempt,
    };
    validate_document_shape(&document)?;
    Ok(document)
}

fn parse_attempt(
    raw: RawAttempt,
) -> Result<PersonalWorkerLimaAttempt, PersonalWorkerLimaAuthorityError> {
    Ok(PersonalWorkerLimaAttempt {
        generation: PersonalWorkerLimaAttemptGeneration::new(raw.generation)?,
        store_revision: PersonalWorkerStoreRevision::new(raw.store_revision)
            .map_err(|_| corrupt_document())?,
        queue_generation: PersonalWorkerQueueGeneration::new(raw.queue_generation)
            .map_err(|_| corrupt_document())?,
        decision_at: parse_time(raw.decision_at)?,
        action: parse_action(&raw.action)?,
        identity: parse_instance(raw.identity)?,
        before_state: parse_state(&raw.before_state)?,
        before_profile: parse_profile(&raw.before_profile)?,
        before_profile_generation: parse_profile_generation(raw.before_profile_generation)?,
        before_resources: LimaObservedResources::new(raw.before_vcpus, raw.before_memory_bytes)
            .map_err(|_| corrupt_document())?,
        after_state: parse_state(&raw.after_state)?,
        after_profile: parse_profile(&raw.after_profile)?,
        after_profile_generation: parse_profile_generation(raw.after_profile_generation)?,
        phase: parse_phase(&raw.phase)?,
        checkpoint_at: parse_time(raw.checkpoint_at)?,
        completed_at: raw.completed_at.map(parse_time).transpose()?,
    })
}

fn validate_document_shape(
    document: &PersonalWorkerLimaAuthorityDocument,
) -> Result<(), PersonalWorkerLimaAuthorityError> {
    if document.config_identity.schema_version != OPERATOR_CONFIG_SCHEMA_VERSION
        || document.identity.instance_id().as_str() != document.lima_instance.as_str()
        || document.persistent_identity.root_filesystem.device_id == 0
        || document.persistent_identity.root_filesystem.inode == 0
        || document.persistent_identity.cache_directory.device_id == 0
        || document.persistent_identity.cache_directory.inode == 0
    {
        return Err(corrupt_document());
    }
    if let Some(attempt) = &document.attempt {
        let completed_shape = match attempt.phase {
            PersonalWorkerLimaAttemptPhase::Completed => {
                attempt.completed_at == Some(attempt.checkpoint_at)
            }
            _ => attempt.completed_at.is_none(),
        };
        if attempt.identity != document.identity
            || !authority_generation_matches_attempt(document.authority_generation, attempt)
            || attempt.checkpoint_at < attempt.decision_at
            || !completed_shape
            || attempt.before_resources
                != LimaObservedResources::for_profile(attempt.before_profile)
            || attempt.after_profile != attempt.action.target_profile(attempt.before_profile)
            || attempt.after_state != attempt.action.target_state()
            || attempt.after_profile_generation
                != attempt
                    .action
                    .target_generation(attempt.before_profile_generation)?
            || !valid_attempt_endpoints(attempt)
            || !phase_is_reachable(attempt.action, attempt.before_state, attempt.phase)
        {
            return Err(corrupt_document());
        }
    }
    Ok(())
}

fn authority_generation_matches_attempt(
    authority_generation: PersonalWorkerLimaAuthorityGeneration,
    attempt: &PersonalWorkerLimaAttempt,
) -> bool {
    let delta = match (attempt.action, attempt.phase) {
        (PersonalWorkerLimaAction::StopToStopped, PersonalWorkerLimaAttemptPhase::Prepared) => 1,
        (PersonalWorkerLimaAction::StopToStopped, PersonalWorkerLimaAttemptPhase::StopStarted) => 2,
        (
            PersonalWorkerLimaAction::StopToStopped,
            PersonalWorkerLimaAttemptPhase::StopCompleted,
        ) => 3,
        (
            PersonalWorkerLimaAction::StopToStopped,
            PersonalWorkerLimaAttemptPhase::VerifyStarted,
        ) => 4,
        (PersonalWorkerLimaAction::StopToStopped, PersonalWorkerLimaAttemptPhase::Completed) => 5,
        (
            PersonalWorkerLimaAction::StopToInteractive | PersonalWorkerLimaAction::StopToWork,
            PersonalWorkerLimaAttemptPhase::Prepared,
        ) => 1,
        (
            PersonalWorkerLimaAction::StopToInteractive | PersonalWorkerLimaAction::StopToWork,
            PersonalWorkerLimaAttemptPhase::StopStarted,
        ) => 2,
        (
            PersonalWorkerLimaAction::StopToInteractive | PersonalWorkerLimaAction::StopToWork,
            PersonalWorkerLimaAttemptPhase::StopCompleted,
        ) => 3,
        (
            PersonalWorkerLimaAction::StopToInteractive | PersonalWorkerLimaAction::StopToWork,
            PersonalWorkerLimaAttemptPhase::EditStarted,
        ) => 4,
        (
            PersonalWorkerLimaAction::StopToInteractive | PersonalWorkerLimaAction::StopToWork,
            PersonalWorkerLimaAttemptPhase::EditCompleted,
        ) => 5,
        (
            PersonalWorkerLimaAction::StopToInteractive | PersonalWorkerLimaAction::StopToWork,
            PersonalWorkerLimaAttemptPhase::StartStarted,
        ) => 6,
        (
            PersonalWorkerLimaAction::StopToInteractive | PersonalWorkerLimaAction::StopToWork,
            PersonalWorkerLimaAttemptPhase::StartCompleted,
        ) => 7,
        (
            PersonalWorkerLimaAction::StopToInteractive | PersonalWorkerLimaAction::StopToWork,
            PersonalWorkerLimaAttemptPhase::VerifyStarted,
        ) => 8,
        (
            PersonalWorkerLimaAction::StopToInteractive | PersonalWorkerLimaAction::StopToWork,
            PersonalWorkerLimaAttemptPhase::Completed,
        ) => 9,
        _ => return false,
    };
    attempt
        .generation
        .get()
        .checked_add(delta)
        .is_some_and(|expected| expected == authority_generation.get())
}

fn valid_attempt_endpoints(attempt: &PersonalWorkerLimaAttempt) -> bool {
    match attempt.action {
        // Schema v1 cannot prove immutable ownership while an instance is stopped, so no Start
        // attempt is a valid persisted producer state yet.
        PersonalWorkerLimaAction::Start => false,
        PersonalWorkerLimaAction::StopToStopped => {
            attempt.before_state == LimaLifecycleState::Running
        }
        PersonalWorkerLimaAction::StopToInteractive => {
            matches!(attempt.before_state, LimaLifecycleState::Running)
                && attempt.before_profile == LimaResourceProfile::Work
        }
        PersonalWorkerLimaAction::StopToWork => {
            matches!(attempt.before_state, LimaLifecycleState::Running)
                && attempt.before_profile == LimaResourceProfile::Interactive
        }
    }
}

const fn phase_is_reachable(
    action: PersonalWorkerLimaAction,
    before_state: LimaLifecycleState,
    phase: PersonalWorkerLimaAttemptPhase,
) -> bool {
    use PersonalWorkerLimaAttemptPhase as Phase;
    match action {
        PersonalWorkerLimaAction::Start => false,
        PersonalWorkerLimaAction::StopToStopped => matches!(
            phase,
            Phase::Prepared
                | Phase::StopStarted
                | Phase::StopCompleted
                | Phase::VerifyStarted
                | Phase::Completed
        ),
        PersonalWorkerLimaAction::StopToInteractive | PersonalWorkerLimaAction::StopToWork => {
            matches!(before_state, LimaLifecycleState::Running)
        }
    }
}

fn parse_instance(
    raw: RawInstanceIdentity,
) -> Result<LimaInstanceIdentity, PersonalWorkerLimaAuthorityError> {
    let instance_id = LimaInstanceId::parse(&raw.instance_id).map_err(|_| corrupt_document())?;
    let disk_id = LimaCacheDiskId::parse(&raw.cache_disk_id).map_err(|_| corrupt_document())?;
    Ok(LimaInstanceIdentity::new(
        instance_id,
        LimaCacheDiskIdentity::new(disk_id, parse_digest(&raw.cache_disk_identity_digest)?),
    ))
}

fn parse_digest(value: &str) -> Result<Sha256Digest, PersonalWorkerLimaAuthorityError> {
    Sha256Digest::parse(value).map_err(|_| corrupt_document())
}

fn parse_time(value: u64) -> Result<EpochMillis, PersonalWorkerLimaAuthorityError> {
    EpochMillis::new(value).map_err(|_| corrupt_document())
}

fn parse_profile_generation(
    value: u64,
) -> Result<LimaProfileGeneration, PersonalWorkerLimaAuthorityError> {
    LimaProfileGeneration::new(value).map_err(|_| corrupt_document())
}

macro_rules! name_parser {
    ($parse:ident, $name:ident, $type:ty, {$($text:literal => $variant:path),+ $(,)?}) => {
        fn $parse(value: &str) -> Result<$type, PersonalWorkerLimaAuthorityError> {
            match value {
                $($text => Ok($variant),)+
                _ => Err(corrupt_document()),
            }
        }

        const fn $name(value: $type) -> &'static str {
            match value {
                $($variant => $text,)+
            }
        }
    };
}

name_parser!(parse_vm_type, vm_type_name, LimaVmType, {
    "vz" => LimaVmType::Vz,
    "qemu" => LimaVmType::Qemu,
});
name_parser!(parse_architecture, architecture_name, LimaArchitecture, {
    "aarch64" => LimaArchitecture::Aarch64,
    "x86_64" => LimaArchitecture::X86_64,
});
name_parser!(parse_profile, profile_name, LimaResourceProfile, {
    "interactive" => LimaResourceProfile::Interactive,
    "work" => LimaResourceProfile::Work,
});
name_parser!(parse_action, action_name, PersonalWorkerLimaAction, {
    "start" => PersonalWorkerLimaAction::Start,
    "stop_to_stopped" => PersonalWorkerLimaAction::StopToStopped,
    "stop_to_interactive" => PersonalWorkerLimaAction::StopToInteractive,
    "stop_to_work" => PersonalWorkerLimaAction::StopToWork,
});
name_parser!(parse_state, state_name, LimaLifecycleState, {
    "stopped" => LimaLifecycleState::Stopped,
    "starting" => LimaLifecycleState::Starting,
    "running" => LimaLifecycleState::Running,
    "draining" => LimaLifecycleState::Draining,
    "stopping" => LimaLifecycleState::Stopping,
    "unavailable" => LimaLifecycleState::Unavailable,
});
name_parser!(parse_phase, phase_name, PersonalWorkerLimaAttemptPhase, {
    "prepared" => PersonalWorkerLimaAttemptPhase::Prepared,
    "stop_started" => PersonalWorkerLimaAttemptPhase::StopStarted,
    "stop_completed" => PersonalWorkerLimaAttemptPhase::StopCompleted,
    "edit_started" => PersonalWorkerLimaAttemptPhase::EditStarted,
    "edit_completed" => PersonalWorkerLimaAttemptPhase::EditCompleted,
    "start_started" => PersonalWorkerLimaAttemptPhase::StartStarted,
    "start_completed" => PersonalWorkerLimaAttemptPhase::StartCompleted,
    "verify_started" => PersonalWorkerLimaAttemptPhase::VerifyStarted,
    "completed" => PersonalWorkerLimaAttemptPhase::Completed,
});

const fn corrupt_document() -> PersonalWorkerLimaAuthorityError {
    PersonalWorkerLimaAuthorityError::new(
        PersonalWorkerLimaAuthorityErrorKind::CorruptDocument,
        "Lima authority document is corrupt or noncanonical",
    )
}
