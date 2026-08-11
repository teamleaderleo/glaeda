//! Durable control state for one controller-owned disposable-worker source template.
//!
//! This module is deliberately pure. It owns no filesystem, clock, process, Lima, credential, or
//! network authority. Lima remains responsible for VM and provisioning semantics; this document
//! only makes every crash-ambiguous source-template mutation explicit before a future same-lock
//! executor is allowed to run it.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::artifact::Sha256Digest;
use crate::disposable_prepared_template::DisposablePreparedTemplateIdentity;
use crate::lima_observation::LimaInstanceName;

pub const DISPOSABLE_TEMPLATE_GENERATION_SCHEMA_VERSION: u8 = 1;
pub const MAX_DISPOSABLE_TEMPLATE_GENERATION_BYTES: usize = 16_384;
const MAX_GENERATION_ID_BYTES: usize = 64;
const MAX_TRANSITIONS: usize = 8;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct DisposableTemplateGenerationId(String);

impl DisposableTemplateGenerationId {
    /// Parse one bounded durable generation identifier.
    ///
    /// # Errors
    ///
    /// Returns an error unless the value is a lowercase state-safe identifier.
    pub fn parse(value: &str) -> Result<Self, DisposableTemplateGenerationError> {
        let valid = !value.is_empty()
            && value.len() <= MAX_GENERATION_ID_BYTES
            && !value.starts_with('-')
            && !value.ends_with('-')
            && !value.contains("--")
            && value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        if !valid {
            return Err(generation_error(
                DisposableTemplateGenerationErrorKind::InvalidDocument,
                "template generation identifier is invalid",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for DisposableTemplateGenerationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Equality-only identity of the private `LIMA_HOME` and fixed source instance.
///
/// The future source-request constructor derives this from validated private inputs. It has no
/// path accessor and its raw parser remains crate-private for strict durable decoding.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DisposableTemplateSourceIdentity(Sha256Digest);

impl DisposableTemplateSourceIdentity {
    pub(crate) fn from_runtime_digest(digest: Sha256Digest) -> Self {
        Self(digest)
    }

    pub(crate) fn parse(value: &str) -> Result<Self, DisposableTemplateGenerationError> {
        Sha256Digest::parse(value).map(Self).map_err(|_| {
            generation_error(
                DisposableTemplateGenerationErrorKind::InvalidDocument,
                "template source identity is invalid",
            )
        })
    }

    #[must_use]
    pub(crate) fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for DisposableTemplateSourceIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DisposableTemplateSourceIdentity(<private>)")
    }
}

/// Equality-only identity of the exact observed Lima source object.
///
/// The future private observer derives this from retained host evidence. The value is persisted
/// before any destructive command and has no public raw constructor or accessor.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DisposableTemplateObjectIdentity(Sha256Digest);

impl DisposableTemplateObjectIdentity {
    pub(crate) fn from_host_digest(digest: Sha256Digest) -> Self {
        Self(digest)
    }

    pub(crate) fn parse(value: &str) -> Result<Self, DisposableTemplateGenerationError> {
        Sha256Digest::parse(value)
            .map(Self)
            .map_err(|_| invalid_document())
    }

    #[must_use]
    pub(crate) fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for DisposableTemplateObjectIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DisposableTemplateObjectIdentity(<private>)")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableTemplateGenerationPhase {
    Pending,
    CreateAuthorized,
    CreateStarted,
    Verified,
    StopAuthorized,
    StopStarted,
    DiscardAuthorized,
    DiscardStarted,
    Ready,
    Discarded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableTemplateGenerationAction {
    AuthorizeCreate,
    RecordCreateStarted,
    RecordVerified,
    AuthorizeStop,
    RecordStopStarted,
    AuthorizeDiscard,
    RecordDiscardStarted,
    RecordReady,
    RecordDiscarded,
}

#[derive(Clone, PartialEq, Eq)]
pub struct DisposableTemplateGenerationDocument {
    generation_id: DisposableTemplateGenerationId,
    revision: u64,
    prepared_template_identity: DisposablePreparedTemplateIdentity,
    source_identity: DisposableTemplateSourceIdentity,
    source_instance: LimaInstanceName,
    owned_object_identity: Option<DisposableTemplateObjectIdentity>,
    phase: DisposableTemplateGenerationPhase,
    history: Vec<DisposableTemplateGenerationAction>,
}

impl fmt::Debug for DisposableTemplateGenerationDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableTemplateGenerationDocument")
            .field("generation_id", &self.generation_id)
            .field("revision", &self.revision)
            .field("source_instance", &self.source_instance)
            .field("phase", &self.phase)
            .field("history_length", &self.history.len())
            .finish()
    }
}

impl DisposableTemplateGenerationDocument {
    pub(crate) fn initial(
        generation_id: DisposableTemplateGenerationId,
        prepared_template_identity: DisposablePreparedTemplateIdentity,
        source_identity: DisposableTemplateSourceIdentity,
        source_instance: LimaInstanceName,
    ) -> Self {
        Self {
            generation_id,
            revision: 1,
            prepared_template_identity,
            source_identity,
            source_instance,
            owned_object_identity: None,
            phase: DisposableTemplateGenerationPhase::Pending,
            history: Vec::new(),
        }
    }

    pub(crate) fn runtime_initial(
        generation_id: DisposableTemplateGenerationId,
        prepared_template_identity: DisposablePreparedTemplateIdentity,
        source_identity: DisposableTemplateSourceIdentity,
        source_instance: LimaInstanceName,
    ) -> Self {
        Self::initial(
            generation_id,
            prepared_template_identity,
            source_identity,
            source_instance,
        )
    }

    #[must_use]
    pub const fn generation_id(&self) -> &DisposableTemplateGenerationId {
        &self.generation_id
    }

    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    #[must_use]
    pub const fn prepared_template_identity(&self) -> &DisposablePreparedTemplateIdentity {
        &self.prepared_template_identity
    }

    #[must_use]
    pub const fn source_identity(&self) -> &DisposableTemplateSourceIdentity {
        &self.source_identity
    }

    #[must_use]
    pub const fn source_instance(&self) -> &LimaInstanceName {
        &self.source_instance
    }

    #[must_use]
    pub const fn owned_object_identity(&self) -> Option<&DisposableTemplateObjectIdentity> {
        self.owned_object_identity.as_ref()
    }

    #[must_use]
    pub const fn phase(&self) -> DisposableTemplateGenerationPhase {
        self.phase
    }

    /// Apply one exact durable transition.
    ///
    /// # Errors
    ///
    /// Returns a stale-revision, invalid-transition, or exhausted-history refusal without change.
    pub(crate) fn transition(
        &self,
        expected_revision: u64,
        action: DisposableTemplateGenerationAction,
        observed_object_identity: Option<DisposableTemplateObjectIdentity>,
    ) -> Result<Self, DisposableTemplateGenerationError> {
        if expected_revision != self.revision {
            return Err(generation_error(
                DisposableTemplateGenerationErrorKind::StaleRevision,
                "template generation revision is stale",
            ));
        }
        let next_phase = next_phase(self.phase, action).ok_or_else(|| {
            generation_error(
                DisposableTemplateGenerationErrorKind::InvalidTransition,
                "template generation transition is invalid",
            )
        })?;
        if self.history.len() >= MAX_TRANSITIONS {
            return Err(generation_error(
                DisposableTemplateGenerationErrorKind::RevisionExhausted,
                "template generation history is exhausted",
            ));
        }
        let revision = self.revision.checked_add(1).ok_or_else(|| {
            generation_error(
                DisposableTemplateGenerationErrorKind::RevisionExhausted,
                "template generation revision is exhausted",
            )
        })?;
        let mut next = self.clone();
        next.revision = revision;
        next.phase = next_phase;
        match (&self.owned_object_identity, observed_object_identity) {
            (Some(expected), Some(observed)) if expected == &observed => {}
            (Some(_), None) => {}
            (None, Some(observed))
                if matches!(
                    action,
                    DisposableTemplateGenerationAction::RecordVerified
                        | DisposableTemplateGenerationAction::AuthorizeDiscard
                ) =>
            {
                next.owned_object_identity = Some(observed);
            }
            (None, None) if action == DisposableTemplateGenerationAction::AuthorizeDiscard => {}
            (None, None) if action != DisposableTemplateGenerationAction::RecordVerified => {}
            _ => return Err(invalid_document()),
        }
        next.history.push(action);
        Ok(next)
    }

    /// Validate that this document is exactly one legal successor of `prior`.
    ///
    /// This is the future durable-store CAS boundary: a revision increment alone never permits
    /// rebinding a generation, prepared template, private source, or instance identity.
    ///
    /// # Errors
    ///
    /// Returns an invalid-transition refusal unless every immutable field and the complete history
    /// prefix are unchanged and the final action derives this exact phase and revision.
    pub fn validate_successor_of(
        &self,
        prior: &Self,
    ) -> Result<(), DisposableTemplateGenerationError> {
        let valid = self.generation_id == prior.generation_id
            && self.prepared_template_identity == prior.prepared_template_identity
            && self.source_identity == prior.source_identity
            && self.source_instance == prior.source_instance
            && object_identity_successor_is_valid(self, prior)
            && self.revision == prior.revision.checked_add(1).unwrap_or(0)
            && self.history.len() == prior.history.len() + 1
            && self.history.starts_with(&prior.history)
            && self
                .history
                .last()
                .and_then(|action| next_phase(prior.phase, *action))
                == Some(self.phase);
        if !valid {
            return Err(generation_error(
                DisposableTemplateGenerationErrorKind::InvalidTransition,
                "template generation successor is invalid",
            ));
        }
        Ok(())
    }
}

fn object_identity_successor_is_valid(
    next: &DisposableTemplateGenerationDocument,
    prior: &DisposableTemplateGenerationDocument,
) -> bool {
    match (&prior.owned_object_identity, &next.owned_object_identity) {
        (Some(expected), Some(actual)) => expected == actual,
        (Some(_), None) => false,
        (None, Some(_)) => matches!(
            next.history.last(),
            Some(
                DisposableTemplateGenerationAction::RecordVerified
                    | DisposableTemplateGenerationAction::AuthorizeDiscard
            )
        ),
        (None, None) => !matches!(
            next.history.last(),
            Some(DisposableTemplateGenerationAction::RecordVerified)
        ),
    }
}

const fn next_phase(
    phase: DisposableTemplateGenerationPhase,
    action: DisposableTemplateGenerationAction,
) -> Option<DisposableTemplateGenerationPhase> {
    use DisposableTemplateGenerationAction as Action;
    use DisposableTemplateGenerationPhase as Phase;
    match (phase, action) {
        (Phase::Pending, Action::AuthorizeCreate) => Some(Phase::CreateAuthorized),
        (Phase::CreateAuthorized, Action::RecordCreateStarted) => Some(Phase::CreateStarted),
        (Phase::CreateStarted, Action::RecordVerified) => Some(Phase::Verified),
        (Phase::CreateStarted | Phase::Verified, Action::AuthorizeDiscard) => {
            Some(Phase::DiscardAuthorized)
        }
        (Phase::Verified, Action::AuthorizeStop) => Some(Phase::StopAuthorized),
        (Phase::StopAuthorized, Action::RecordStopStarted) => Some(Phase::StopStarted),
        (Phase::Verified | Phase::StopAuthorized | Phase::StopStarted, Action::RecordReady) => {
            Some(Phase::Ready)
        }
        (Phase::DiscardAuthorized, Action::RecordDiscardStarted) => Some(Phase::DiscardStarted),
        (Phase::DiscardAuthorized | Phase::DiscardStarted, Action::RecordDiscarded) => {
            Some(Phase::Discarded)
        }
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisposableTemplateObservedState {
    Unknown,
    Absent,
    OwnedIncomplete,
    ReadyRunning,
    ReadyStopped,
    Conflicting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisposableTemplatePriorOperationState {
    NoPriorOperation,
    InFlight,
    Quiescent,
}

/// Sealed current observation consumed by the pure reconciler.
///
/// Its fields and constructors remain private. Public callers cannot forge source absence,
/// ownership, readiness, or prepared-template identity merely because they can name the type.
pub struct DisposableTemplateObservation {
    generation_id: DisposableTemplateGenerationId,
    document_revision: u64,
    source_identity: DisposableTemplateSourceIdentity,
    object_identity: Option<DisposableTemplateObjectIdentity>,
    prepared_template_identity: Option<DisposablePreparedTemplateIdentity>,
    prior_operation: DisposableTemplatePriorOperationState,
    state: DisposableTemplateObservedState,
}

#[cfg(test)]
pub(crate) fn test_disposable_template_observation(
    document: &DisposableTemplateGenerationDocument,
    source_identity: DisposableTemplateSourceIdentity,
    object_identity: Option<DisposableTemplateObjectIdentity>,
    prepared_template_identity: Option<DisposablePreparedTemplateIdentity>,
    prior_operation: DisposableTemplatePriorOperationState,
    state: DisposableTemplateObservedState,
) -> DisposableTemplateObservation {
    DisposableTemplateObservation {
        generation_id: document.generation_id.clone(),
        document_revision: document.revision,
        source_identity,
        object_identity,
        prepared_template_identity,
        prior_operation,
        state,
    }
}

pub(crate) fn runtime_disposable_template_observation(
    document: &DisposableTemplateGenerationDocument,
    source_identity: DisposableTemplateSourceIdentity,
    object_identity: Option<DisposableTemplateObjectIdentity>,
    prepared_template_identity: Option<DisposablePreparedTemplateIdentity>,
    prior_operation: DisposableTemplatePriorOperationState,
    state: DisposableTemplateObservedState,
) -> DisposableTemplateObservation {
    DisposableTemplateObservation {
        generation_id: document.generation_id.clone(),
        document_revision: document.revision,
        source_identity,
        object_identity,
        prepared_template_identity,
        prior_operation,
        state,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableTemplateGenerationRefusal {
    ObservationRequired,
    StaleObservation,
    ExistingSourceProtected,
    SourceIdentityDrift,
    ObjectIdentityDrift,
    PreparedTemplateDrift,
    RecoveryRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DisposableTemplateGenerationDisposition {
    Persist {
        action: DisposableTemplateGenerationAction,
    },
    CreateCandidate,
    StopCandidate,
    DiscardCandidate,
    Satisfied,
    RebuildRequired,
    Refuse {
        reason: DisposableTemplateGenerationRefusal,
    },
}

/// Opaque exact-state-bound advisory decision.
///
/// This value is never executable authority. A future same-lock service must obtain a second fresh
/// sealed observation and privately compare the complete retained observation immediately before
/// it may checkpoint and execute. Public callers can inspect the bounded disposition but cannot
/// construct, clone, serialize, or extract its private bindings.
pub struct DisposableTemplateGenerationPlan {
    generation_id: DisposableTemplateGenerationId,
    expected_revision: u64,
    prepared_template_identity: DisposablePreparedTemplateIdentity,
    source_identity: DisposableTemplateSourceIdentity,
    source_instance: LimaInstanceName,
    observed_object_identity: Option<DisposableTemplateObjectIdentity>,
    observed_prepared_template_identity: Option<DisposablePreparedTemplateIdentity>,
    observed_state: DisposableTemplateObservedState,
    prior_operation: DisposableTemplatePriorOperationState,
    disposition: DisposableTemplateGenerationDisposition,
}

impl DisposableTemplateGenerationPlan {
    #[must_use]
    pub const fn disposition(&self) -> DisposableTemplateGenerationDisposition {
        self.disposition
    }

    pub(crate) fn is_bound_to_document(
        &self,
        document: &DisposableTemplateGenerationDocument,
    ) -> bool {
        self.generation_id == document.generation_id
            && self.expected_revision == document.revision
            && self.prepared_template_identity == document.prepared_template_identity
            && self.source_identity == document.source_identity
            && self.source_instance == document.source_instance
    }

    pub(crate) fn confirmed_persist_successor(
        self,
        document: &DisposableTemplateGenerationDocument,
        confirmation: DisposableTemplateObservation,
    ) -> Result<DisposableTemplateGenerationDocument, DisposableTemplateGenerationError> {
        let DisposableTemplateGenerationDisposition::Persist { action } = self.disposition else {
            return Err(generation_error(
                DisposableTemplateGenerationErrorKind::InvalidTransition,
                "template generation candidate requires the runtime execution service",
            ));
        };
        if !self.confirmation_matches(document, &confirmation) {
            return Err(generation_error(
                DisposableTemplateGenerationErrorKind::StaleRevision,
                "template generation confirmation no longer matches the advisory decision",
            ));
        }
        document.transition(self.expected_revision, action, confirmation.object_identity)
    }

    pub(crate) fn confirmed_runtime_successor(
        self,
        document: &DisposableTemplateGenerationDocument,
        confirmation: DisposableTemplateObservation,
    ) -> Result<DisposableTemplateGenerationDocument, DisposableTemplateGenerationError> {
        let action = match self.disposition {
            DisposableTemplateGenerationDisposition::CreateCandidate => {
                DisposableTemplateGenerationAction::RecordCreateStarted
            }
            DisposableTemplateGenerationDisposition::StopCandidate => {
                DisposableTemplateGenerationAction::RecordStopStarted
            }
            DisposableTemplateGenerationDisposition::DiscardCandidate => {
                DisposableTemplateGenerationAction::RecordDiscardStarted
            }
            _ => {
                return Err(generation_error(
                    DisposableTemplateGenerationErrorKind::InvalidTransition,
                    "template generation plan is not runtime command authority",
                ));
            }
        };
        if !self.confirmation_matches(document, &confirmation) {
            return Err(generation_error(
                DisposableTemplateGenerationErrorKind::StaleRevision,
                "template generation confirmation no longer matches the runtime decision",
            ));
        }
        document.transition(self.expected_revision, action, confirmation.object_identity)
    }

    fn confirmation_matches(
        &self,
        document: &DisposableTemplateGenerationDocument,
        confirmation: &DisposableTemplateObservation,
    ) -> bool {
        self.is_bound_to_document(document)
            && confirmation.generation_id == self.generation_id
            && confirmation.document_revision == self.expected_revision
            && confirmation.source_identity == self.source_identity
            && confirmation.object_identity == self.observed_object_identity
            && confirmation.prepared_template_identity == self.observed_prepared_template_identity
            && confirmation.state == self.observed_state
            && confirmation.prior_operation == self.prior_operation
    }
}

impl fmt::Debug for DisposableTemplateGenerationPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableTemplateGenerationPlan")
            .field("generation_id", &self.generation_id)
            .field("expected_revision", &self.expected_revision)
            .field("source_instance", &self.source_instance)
            .field("disposition", &self.disposition)
            .finish()
    }
}

/// Select one recovery-safe next step from current durable state and one sealed observation.
pub fn reconcile_disposable_template_generation(
    document: &DisposableTemplateGenerationDocument,
    observation: &DisposableTemplateObservation,
) -> DisposableTemplateGenerationPlan {
    use DisposableTemplateGenerationAction as Action;
    use DisposableTemplateGenerationDisposition as Disposition;
    use DisposableTemplateGenerationPhase as Phase;
    use DisposableTemplateGenerationRefusal as Refusal;
    use DisposableTemplateObservedState as Observed;

    if observation.source_identity != document.source_identity {
        return plan(
            document,
            observation,
            Disposition::Refuse {
                reason: Refusal::SourceIdentityDrift,
            },
        );
    }
    if observation.generation_id != document.generation_id
        || observation.document_revision != document.revision
    {
        return plan(
            document,
            observation,
            Disposition::Refuse {
                reason: Refusal::StaleObservation,
            },
        );
    }
    if observation.state == Observed::Unknown {
        return plan(
            document,
            observation,
            Disposition::Refuse {
                reason: Refusal::ObservationRequired,
            },
        );
    }
    if let (Some(expected), Some(actual)) = (
        document.owned_object_identity.as_ref(),
        observation.object_identity.as_ref(),
    ) && expected != actual
    {
        return plan(
            document,
            observation,
            Disposition::Refuse {
                reason: Refusal::ObjectIdentityDrift,
            },
        );
    }
    let ready_for_document = matches!(
        observation.state,
        Observed::ReadyRunning | Observed::ReadyStopped
    ) && observation.prepared_template_identity.as_ref()
        == Some(&document.prepared_template_identity);
    if matches!(
        observation.state,
        Observed::ReadyRunning | Observed::ReadyStopped
    ) && !ready_for_document
    {
        let disposition = match document.phase {
            Phase::CreateStarted
                if observation.prior_operation
                    == DisposableTemplatePriorOperationState::Quiescent =>
            {
                Disposition::Persist {
                    action: Action::AuthorizeDiscard,
                }
            }
            Phase::Verified => Disposition::Persist {
                action: Action::AuthorizeDiscard,
            },
            Phase::DiscardAuthorized
                if observation.prior_operation
                    == DisposableTemplatePriorOperationState::NoPriorOperation =>
            {
                Disposition::DiscardCandidate
            }
            _ => Disposition::Refuse {
                reason: Refusal::PreparedTemplateDrift,
            },
        };
        return plan(document, observation, disposition);
    }

    let disposition = match (document.phase, observation.state) {
        (Phase::Pending, Observed::Absent) => Disposition::Persist {
            action: Action::AuthorizeCreate,
        },
        (Phase::Pending, _) => Disposition::Refuse {
            reason: Refusal::ExistingSourceProtected,
        },

        (Phase::CreateAuthorized, Observed::Absent)
            if observation.prior_operation
                == DisposableTemplatePriorOperationState::NoPriorOperation =>
        {
            Disposition::CreateCandidate
        }
        (Phase::CreateAuthorized, _) => Disposition::Refuse {
            reason: Refusal::RecoveryRequired,
        },

        (Phase::CreateStarted, Observed::Absent)
            if observation.prior_operation == DisposableTemplatePriorOperationState::Quiescent =>
        {
            Disposition::Persist {
                action: Action::AuthorizeDiscard,
            }
        }
        (Phase::CreateStarted, Observed::OwnedIncomplete)
            if observation.prior_operation == DisposableTemplatePriorOperationState::Quiescent =>
        {
            Disposition::Persist {
                action: Action::AuthorizeDiscard,
            }
        }
        (Phase::CreateStarted, Observed::ReadyRunning | Observed::ReadyStopped)
            if ready_for_document
                && observation.prior_operation
                    == DisposableTemplatePriorOperationState::Quiescent =>
        {
            Disposition::Persist {
                action: Action::RecordVerified,
            }
        }
        (Phase::CreateStarted, _) => Disposition::Refuse {
            reason: Refusal::RecoveryRequired,
        },

        (Phase::Verified, Observed::ReadyRunning) if ready_for_document => Disposition::Persist {
            action: Action::AuthorizeStop,
        },
        (Phase::Verified, Observed::ReadyStopped) if ready_for_document => Disposition::Persist {
            action: Action::RecordReady,
        },
        (Phase::Verified, Observed::OwnedIncomplete) => Disposition::Persist {
            action: Action::AuthorizeDiscard,
        },
        (Phase::Verified, _) => Disposition::Refuse {
            reason: Refusal::RecoveryRequired,
        },

        (Phase::StopAuthorized, Observed::ReadyRunning)
            if ready_for_document
                && observation.prior_operation
                    == DisposableTemplatePriorOperationState::NoPriorOperation =>
        {
            Disposition::StopCandidate
        }
        (Phase::StopAuthorized, Observed::ReadyStopped) if ready_for_document => {
            Disposition::Persist {
                action: Action::RecordReady,
            }
        }
        (Phase::StopAuthorized, _) => Disposition::Refuse {
            reason: Refusal::RecoveryRequired,
        },

        (Phase::StopStarted, Observed::ReadyStopped)
            if ready_for_document
                && observation.prior_operation
                    == DisposableTemplatePriorOperationState::Quiescent =>
        {
            Disposition::Persist {
                action: Action::RecordReady,
            }
        }
        (Phase::StopStarted, _) => Disposition::Refuse {
            reason: Refusal::RecoveryRequired,
        },

        (Phase::DiscardAuthorized, Observed::Absent) => Disposition::Persist {
            action: Action::RecordDiscarded,
        },
        (
            Phase::DiscardAuthorized,
            Observed::OwnedIncomplete | Observed::ReadyRunning | Observed::ReadyStopped,
        ) if observation.prior_operation
            == DisposableTemplatePriorOperationState::NoPriorOperation =>
        {
            Disposition::DiscardCandidate
        }
        (Phase::DiscardAuthorized, _) => Disposition::Refuse {
            reason: Refusal::RecoveryRequired,
        },

        (Phase::DiscardStarted, Observed::Absent)
            if observation.prior_operation == DisposableTemplatePriorOperationState::Quiescent =>
        {
            Disposition::Persist {
                action: Action::RecordDiscarded,
            }
        }
        (Phase::DiscardStarted, _) => Disposition::Refuse {
            reason: Refusal::RecoveryRequired,
        },

        (Phase::Ready, Observed::ReadyStopped) if ready_for_document => Disposition::Satisfied,
        (Phase::Ready, _) => Disposition::Refuse {
            reason: Refusal::RecoveryRequired,
        },
        (Phase::Discarded, Observed::Absent) => Disposition::RebuildRequired,
        (Phase::Discarded, _) => Disposition::Refuse {
            reason: Refusal::RecoveryRequired,
        },
    };
    plan(document, observation, disposition)
}

fn plan(
    document: &DisposableTemplateGenerationDocument,
    observation: &DisposableTemplateObservation,
    disposition: DisposableTemplateGenerationDisposition,
) -> DisposableTemplateGenerationPlan {
    DisposableTemplateGenerationPlan {
        generation_id: document.generation_id.clone(),
        expected_revision: document.revision,
        prepared_template_identity: document.prepared_template_identity.clone(),
        source_identity: document.source_identity.clone(),
        source_instance: document.source_instance.clone(),
        observed_object_identity: observation.object_identity.clone(),
        observed_prepared_template_identity: observation.prepared_template_identity.clone(),
        observed_state: observation.state,
        prior_operation: observation.prior_operation,
        disposition,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableTemplateGenerationErrorKind {
    VersionIncompatible,
    InvalidDocument,
    NonCanonical,
    StaleRevision,
    InvalidTransition,
    RevisionExhausted,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct DisposableTemplateGenerationError {
    kind: DisposableTemplateGenerationErrorKind,
    code: &'static str,
    message: &'static str,
}

impl DisposableTemplateGenerationError {
    #[must_use]
    pub const fn kind(&self) -> DisposableTemplateGenerationErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for DisposableTemplateGenerationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableTemplateGenerationError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for DisposableTemplateGenerationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for DisposableTemplateGenerationError {}

#[derive(Deserialize)]
struct VersionWire {
    schema_version: u8,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerationWire {
    schema_version: u8,
    generation_id: String,
    revision: u64,
    prepared_template_identity: String,
    source_identity: String,
    source_instance: String,
    owned_object_identity: Option<String>,
    phase: DisposableTemplateGenerationPhase,
    history: Vec<DisposableTemplateGenerationAction>,
}

/// Encode one generation document into its unique durable JSON bytes.
///
/// # Errors
///
/// Returns a bounded error if the already-validated document cannot be serialized.
pub fn encode_disposable_template_generation(
    document: &DisposableTemplateGenerationDocument,
) -> Result<Vec<u8>, DisposableTemplateGenerationError> {
    let wire = GenerationWire {
        schema_version: DISPOSABLE_TEMPLATE_GENERATION_SCHEMA_VERSION,
        generation_id: document.generation_id.as_str().to_owned(),
        revision: document.revision,
        prepared_template_identity: document.prepared_template_identity.as_str().to_owned(),
        source_identity: document.source_identity.as_str().to_owned(),
        source_instance: document.source_instance.as_str().to_owned(),
        owned_object_identity: document
            .owned_object_identity
            .as_ref()
            .map(|identity| identity.as_str().to_owned()),
        phase: document.phase,
        history: document.history.clone(),
    };
    let mut bytes = serde_json::to_vec_pretty(&wire).map_err(|_| invalid_document())?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Strictly decode one canonical generation document and replay its complete bounded history.
///
/// # Errors
///
/// Returns a bounded error for oversize, incompatible, invalid, impossible, or noncanonical bytes.
pub fn decode_disposable_template_generation(
    bytes: &[u8],
) -> Result<DisposableTemplateGenerationDocument, DisposableTemplateGenerationError> {
    if bytes.len() > MAX_DISPOSABLE_TEMPLATE_GENERATION_BYTES {
        return Err(invalid_document());
    }
    let version: VersionWire = serde_json::from_slice(bytes).map_err(|_| invalid_document())?;
    if version.schema_version != DISPOSABLE_TEMPLATE_GENERATION_SCHEMA_VERSION {
        return Err(generation_error(
            DisposableTemplateGenerationErrorKind::VersionIncompatible,
            "template generation schema version is unsupported",
        ));
    }
    let wire: GenerationWire = serde_json::from_slice(bytes).map_err(|_| invalid_document())?;
    if wire.history.len() > MAX_TRANSITIONS || wire.revision == 0 {
        return Err(invalid_document());
    }
    let generation_id = DisposableTemplateGenerationId::parse(&wire.generation_id)?;
    let prepared_template_identity =
        DisposablePreparedTemplateIdentity::parse(&wire.prepared_template_identity)
            .map_err(|_| invalid_document())?;
    let source_identity = DisposableTemplateSourceIdentity::parse(&wire.source_identity)?;
    let source_instance =
        LimaInstanceName::parse(&wire.source_instance).map_err(|_| invalid_document())?;
    let owned_object_identity = wire
        .owned_object_identity
        .as_deref()
        .map(DisposableTemplateObjectIdentity::parse)
        .transpose()?;
    let mut document = DisposableTemplateGenerationDocument::initial(
        generation_id,
        prepared_template_identity,
        source_identity,
        source_instance,
    );
    for action in wire.history {
        let binds_object = document.owned_object_identity.is_none()
            && matches!(
                action,
                DisposableTemplateGenerationAction::RecordVerified
                    | DisposableTemplateGenerationAction::AuthorizeDiscard
            );
        document = document
            .transition(
                document.revision,
                action,
                binds_object
                    .then(|| owned_object_identity.clone())
                    .flatten(),
            )
            .map_err(|_| invalid_document())?;
    }
    if document.revision != wire.revision
        || document.phase != wire.phase
        || document.owned_object_identity != owned_object_identity
    {
        return Err(invalid_document());
    }
    if encode_disposable_template_generation(&document)? != bytes {
        return Err(generation_error(
            DisposableTemplateGenerationErrorKind::NonCanonical,
            "template generation document is not canonically encoded",
        ));
    }
    Ok(document)
}

const fn invalid_document() -> DisposableTemplateGenerationError {
    generation_error(
        DisposableTemplateGenerationErrorKind::InvalidDocument,
        "template generation document is invalid",
    )
}

const fn generation_error(
    kind: DisposableTemplateGenerationErrorKind,
    message: &'static str,
) -> DisposableTemplateGenerationError {
    let code = match kind {
        DisposableTemplateGenerationErrorKind::VersionIncompatible => {
            "template_generation_version_incompatible"
        }
        DisposableTemplateGenerationErrorKind::InvalidDocument => "template_generation_invalid",
        DisposableTemplateGenerationErrorKind::NonCanonical => "template_generation_noncanonical",
        DisposableTemplateGenerationErrorKind::StaleRevision => {
            "template_generation_stale_revision"
        }
        DisposableTemplateGenerationErrorKind::InvalidTransition => {
            "template_generation_invalid_transition"
        }
        DisposableTemplateGenerationErrorKind::RevisionExhausted => {
            "template_generation_revision_exhausted"
        }
    };
    DisposableTemplateGenerationError {
        kind,
        code,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disposable_prepared_template::current_disposable_prepared_template;

    fn template_identity() -> DisposablePreparedTemplateIdentity {
        current_disposable_prepared_template()
            .unwrap()
            .identity()
            .unwrap()
    }

    fn source_identity(byte: &str) -> DisposableTemplateSourceIdentity {
        DisposableTemplateSourceIdentity::parse(&format!("sha256:{}", byte.repeat(64))).unwrap()
    }

    fn initial() -> DisposableTemplateGenerationDocument {
        DisposableTemplateGenerationDocument::initial(
            DisposableTemplateGenerationId::parse("template-generation-1").unwrap(),
            template_identity(),
            source_identity("a"),
            LimaInstanceName::parse("smolrunner-template").unwrap(),
        )
    }

    fn object_identity(byte: &str) -> DisposableTemplateObjectIdentity {
        DisposableTemplateObjectIdentity::parse(&format!("sha256:{}", byte.repeat(64))).unwrap()
    }

    fn observation(
        document: &DisposableTemplateGenerationDocument,
        state: DisposableTemplateObservedState,
        prior_operation: DisposableTemplatePriorOperationState,
    ) -> DisposableTemplateObservation {
        DisposableTemplateObservation {
            generation_id: document.generation_id.clone(),
            document_revision: document.revision,
            source_identity: source_identity("a"),
            object_identity: (!matches!(state, DisposableTemplateObservedState::Absent))
                .then(|| object_identity("b")),
            prepared_template_identity: matches!(
                state,
                DisposableTemplateObservedState::ReadyRunning
                    | DisposableTemplateObservedState::ReadyStopped
            )
            .then(template_identity),
            prior_operation,
            state,
        }
    }

    fn persist(
        document: &DisposableTemplateGenerationDocument,
        action: DisposableTemplateGenerationAction,
    ) -> DisposableTemplateGenerationDocument {
        let observed_object_identity = matches!(
            action,
            DisposableTemplateGenerationAction::RecordVerified
                | DisposableTemplateGenerationAction::AuthorizeDiscard
        )
        .then(|| object_identity("b"));
        document
            .transition(document.revision(), action, observed_object_identity)
            .unwrap()
    }

    #[test]
    fn canonical_codec_replays_exact_history_and_refuses_impossible_shapes() {
        let initial = initial();
        let authorized = persist(
            &initial,
            DisposableTemplateGenerationAction::AuthorizeCreate,
        );
        let started = persist(
            &authorized,
            DisposableTemplateGenerationAction::RecordCreateStarted,
        );
        let bytes = encode_disposable_template_generation(&started).unwrap();
        assert_eq!(
            decode_disposable_template_generation(&bytes).unwrap(),
            started
        );

        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value["revision"] = serde_json::json!(99);
        assert_eq!(
            decode_disposable_template_generation(&serde_json::to_vec_pretty(&value).unwrap())
                .unwrap_err()
                .kind(),
            DisposableTemplateGenerationErrorKind::InvalidDocument
        );
        value = serde_json::from_slice(&bytes).unwrap();
        value["phase"] = serde_json::json!("ready");
        assert_eq!(
            decode_disposable_template_generation(&serde_json::to_vec_pretty(&value).unwrap())
                .unwrap_err()
                .kind(),
            DisposableTemplateGenerationErrorKind::InvalidDocument
        );
    }

    #[test]
    fn successor_validation_binds_every_identity_and_exact_history() {
        let initial = initial();
        let authorized = persist(
            &initial,
            DisposableTemplateGenerationAction::AuthorizeCreate,
        );
        authorized.validate_successor_of(&initial).unwrap();

        let mut rebound = authorized.clone();
        rebound.source_identity = source_identity("b");
        assert_eq!(
            rebound.validate_successor_of(&initial).unwrap_err().kind(),
            DisposableTemplateGenerationErrorKind::InvalidTransition
        );

        let mut skipped = authorized.clone();
        skipped.revision += 1;
        assert_eq!(
            skipped.validate_successor_of(&initial).unwrap_err().kind(),
            DisposableTemplateGenerationErrorKind::InvalidTransition
        );

        let started = persist(
            &authorized,
            DisposableTemplateGenerationAction::RecordCreateStarted,
        );
        let verified = persist(&started, DisposableTemplateGenerationAction::RecordVerified);
        assert_eq!(
            decode_disposable_template_generation(
                &encode_disposable_template_generation(&verified).unwrap()
            )
            .unwrap(),
            verified
        );
        let mut object_rebound =
            persist(&verified, DisposableTemplateGenerationAction::AuthorizeStop);
        object_rebound.owned_object_identity = Some(object_identity("c"));
        assert_eq!(
            object_rebound
                .validate_successor_of(&verified)
                .unwrap_err()
                .kind(),
            DisposableTemplateGenerationErrorKind::InvalidTransition
        );
    }

    #[test]
    fn version_precedes_current_fields_and_noncanonical_bytes_fail_closed() {
        let bytes = encode_disposable_template_generation(&initial()).unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value["schema_version"] = serde_json::json!(2);
        value.as_object_mut().unwrap().remove("source_identity");
        assert_eq!(
            decode_disposable_template_generation(&serde_json::to_vec(&value).unwrap())
                .unwrap_err()
                .kind(),
            DisposableTemplateGenerationErrorKind::VersionIncompatible
        );
        assert_eq!(
            decode_disposable_template_generation(
                &serde_json::to_vec(&serde_json::from_slice::<serde_json::Value>(&bytes).unwrap())
                    .unwrap()
            )
            .unwrap_err()
            .kind(),
            DisposableTemplateGenerationErrorKind::NonCanonical
        );
    }

    #[test]
    fn existing_source_is_protected_until_absence_is_durably_authorized() {
        let initial = initial();
        assert_eq!(
            reconcile_disposable_template_generation(
                &initial,
                &observation(
                    &initial,
                    DisposableTemplateObservedState::OwnedIncomplete,
                    DisposableTemplatePriorOperationState::NoPriorOperation
                )
            )
            .disposition(),
            DisposableTemplateGenerationDisposition::Refuse {
                reason: DisposableTemplateGenerationRefusal::ExistingSourceProtected
            }
        );
        let absent = observation(
            &initial,
            DisposableTemplateObservedState::Absent,
            DisposableTemplatePriorOperationState::NoPriorOperation,
        );
        let authorize = reconcile_disposable_template_generation(&initial, &absent);
        assert_eq!(
            authorize.disposition(),
            DisposableTemplateGenerationDisposition::Persist {
                action: DisposableTemplateGenerationAction::AuthorizeCreate
            }
        );
        let authorized = persist(
            &initial,
            DisposableTemplateGenerationAction::AuthorizeCreate,
        );
        assert_eq!(
            reconcile_disposable_template_generation(&authorized, &absent).disposition(),
            DisposableTemplateGenerationDisposition::Refuse {
                reason: DisposableTemplateGenerationRefusal::StaleObservation
            },
            "one sealed absence observation cannot advance more than one durable revision"
        );
        assert_eq!(
            reconcile_disposable_template_generation(
                &authorized,
                &observation(
                    &authorized,
                    DisposableTemplateObservedState::OwnedIncomplete,
                    DisposableTemplatePriorOperationState::NoPriorOperation
                )
            )
            .disposition(),
            DisposableTemplateGenerationDisposition::Refuse {
                reason: DisposableTemplateGenerationRefusal::RecoveryRequired
            }
        );
    }

    #[test]
    fn create_stop_and_ready_require_their_durable_started_checkpoints() {
        let pending = initial();
        let create_authorized = persist(
            &pending,
            DisposableTemplateGenerationAction::AuthorizeCreate,
        );
        assert_eq!(
            reconcile_disposable_template_generation(
                &create_authorized,
                &observation(
                    &create_authorized,
                    DisposableTemplateObservedState::Absent,
                    DisposableTemplatePriorOperationState::NoPriorOperation
                )
            )
            .disposition(),
            DisposableTemplateGenerationDisposition::CreateCandidate
        );
        let create_started = persist(
            &create_authorized,
            DisposableTemplateGenerationAction::RecordCreateStarted,
        );
        assert_eq!(
            reconcile_disposable_template_generation(
                &create_started,
                &observation(
                    &create_started,
                    DisposableTemplateObservedState::Absent,
                    DisposableTemplatePriorOperationState::InFlight
                )
            )
            .disposition(),
            DisposableTemplateGenerationDisposition::Refuse {
                reason: DisposableTemplateGenerationRefusal::RecoveryRequired
            },
            "a started create is never replayed while its prior operation may still exist"
        );
        let verified = persist(
            &create_started,
            DisposableTemplateGenerationAction::RecordVerified,
        );
        assert_eq!(
            reconcile_disposable_template_generation(
                &verified,
                &observation(
                    &verified,
                    DisposableTemplateObservedState::ReadyRunning,
                    DisposableTemplatePriorOperationState::NoPriorOperation
                )
            )
            .disposition(),
            DisposableTemplateGenerationDisposition::Persist {
                action: DisposableTemplateGenerationAction::AuthorizeStop
            }
        );
        let stop_authorized = persist(&verified, DisposableTemplateGenerationAction::AuthorizeStop);
        assert_eq!(
            reconcile_disposable_template_generation(
                &stop_authorized,
                &observation(
                    &stop_authorized,
                    DisposableTemplateObservedState::ReadyRunning,
                    DisposableTemplatePriorOperationState::NoPriorOperation
                )
            )
            .disposition(),
            DisposableTemplateGenerationDisposition::StopCandidate
        );
        let stop_started = persist(
            &stop_authorized,
            DisposableTemplateGenerationAction::RecordStopStarted,
        );
        assert_eq!(
            reconcile_disposable_template_generation(
                &stop_started,
                &observation(
                    &stop_started,
                    DisposableTemplateObservedState::ReadyRunning,
                    DisposableTemplatePriorOperationState::Quiescent
                )
            )
            .disposition(),
            DisposableTemplateGenerationDisposition::Refuse {
                reason: DisposableTemplateGenerationRefusal::RecoveryRequired
            }
        );
        let ready = persist(
            &stop_started,
            DisposableTemplateGenerationAction::RecordReady,
        );
        assert_eq!(
            reconcile_disposable_template_generation(
                &ready,
                &observation(
                    &ready,
                    DisposableTemplateObservedState::ReadyStopped,
                    DisposableTemplatePriorOperationState::NoPriorOperation
                )
            )
            .disposition(),
            DisposableTemplateGenerationDisposition::Satisfied
        );
    }

    #[test]
    fn incomplete_generation_is_discarded_before_a_new_generation_is_requested() {
        let create_started = persist(
            &persist(
                &initial(),
                DisposableTemplateGenerationAction::AuthorizeCreate,
            ),
            DisposableTemplateGenerationAction::RecordCreateStarted,
        );
        assert_eq!(
            reconcile_disposable_template_generation(
                &create_started,
                &observation(
                    &create_started,
                    DisposableTemplateObservedState::OwnedIncomplete,
                    DisposableTemplatePriorOperationState::Quiescent
                )
            )
            .disposition(),
            DisposableTemplateGenerationDisposition::Persist {
                action: DisposableTemplateGenerationAction::AuthorizeDiscard
            }
        );
        let discard_authorized = persist(
            &create_started,
            DisposableTemplateGenerationAction::AuthorizeDiscard,
        );
        assert_eq!(
            reconcile_disposable_template_generation(
                &discard_authorized,
                &observation(
                    &discard_authorized,
                    DisposableTemplateObservedState::OwnedIncomplete,
                    DisposableTemplatePriorOperationState::NoPriorOperation
                )
            )
            .disposition(),
            DisposableTemplateGenerationDisposition::DiscardCandidate
        );
        let discard_started = persist(
            &discard_authorized,
            DisposableTemplateGenerationAction::RecordDiscardStarted,
        );
        assert_eq!(
            reconcile_disposable_template_generation(
                &discard_started,
                &observation(
                    &discard_started,
                    DisposableTemplateObservedState::ReadyRunning,
                    DisposableTemplatePriorOperationState::Quiescent
                )
            )
            .disposition(),
            DisposableTemplateGenerationDisposition::Refuse {
                reason: DisposableTemplateGenerationRefusal::RecoveryRequired
            },
            "a completed destructive command is observed rather than blindly replayed"
        );
        let discarded = persist(
            &discard_started,
            DisposableTemplateGenerationAction::RecordDiscarded,
        );
        assert_eq!(
            reconcile_disposable_template_generation(
                &discarded,
                &observation(
                    &discarded,
                    DisposableTemplateObservedState::Absent,
                    DisposableTemplatePriorOperationState::NoPriorOperation
                )
            )
            .disposition(),
            DisposableTemplateGenerationDisposition::RebuildRequired
        );
    }

    #[test]
    fn source_drift_refuses_and_owned_template_drift_is_destroy_and_rebuild_debt() {
        let started = persist(
            &persist(
                &initial(),
                DisposableTemplateGenerationAction::AuthorizeCreate,
            ),
            DisposableTemplateGenerationAction::RecordCreateStarted,
        );
        let wrong_source = DisposableTemplateObservation {
            generation_id: started.generation_id.clone(),
            document_revision: started.revision,
            source_identity: source_identity("b"),
            object_identity: None,
            prepared_template_identity: None,
            prior_operation: DisposableTemplatePriorOperationState::Quiescent,
            state: DisposableTemplateObservedState::Absent,
        };
        assert_eq!(
            reconcile_disposable_template_generation(&started, &wrong_source).disposition(),
            DisposableTemplateGenerationDisposition::Refuse {
                reason: DisposableTemplateGenerationRefusal::SourceIdentityDrift
            }
        );
        let wrong_template = DisposableTemplateObservation {
            generation_id: started.generation_id.clone(),
            document_revision: started.revision,
            source_identity: source_identity("a"),
            object_identity: Some(object_identity("b")),
            prepared_template_identity: Some(
                DisposablePreparedTemplateIdentity::parse(&format!("sha256:{}", "c".repeat(64)))
                    .unwrap(),
            ),
            prior_operation: DisposableTemplatePriorOperationState::Quiescent,
            state: DisposableTemplateObservedState::ReadyRunning,
        };
        assert_eq!(
            reconcile_disposable_template_generation(&started, &wrong_template).disposition(),
            DisposableTemplateGenerationDisposition::Persist {
                action: DisposableTemplateGenerationAction::AuthorizeDiscard
            }
        );
        assert!(!format!("{started:?}").contains("sha256:"));
    }
}
