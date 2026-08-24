//! Pure crash/replay state for one exact project-disk attachment transaction.
//!
//! This module carries the exact P1 attach plan through a durable P4 transaction without executing
//! Lima, starting a guest, mutating a filesystem, or minting the #589 correlation proof. After the
//! external attach may have started, automatic replay is forbidden until fresh observation proves
//! either the exact planned attachment or separately proves the prior process quiescent and the disk
//! detached again.

use std::fmt;

use serde::Serialize;

use crate::artifact::Sha256Digest;
use crate::project_catalog::ProjectIdentity;
use crate::project_disk_filesystem::{
    ProjectDiskFilesystemBinding, ProjectDiskFilesystemFormatProfileGeneration,
    ProjectDiskFilesystemGeneration, ProjectDiskFilesystemKind,
};
use crate::project_disk_lease::{
    ProjectDiskAttachPlan, ProjectDiskAttachmentGeneration, ProjectDiskAttachmentLease,
    ProjectDiskGeneration, ProjectDiskId, ProjectDiskLeaseError, ProjectDiskLeaseRecord,
    ProjectDiskLeaseState, ProjectDiskLockObservation, ProjectDiskObservation,
    ProjectDiskPhysicalObservation, ProjectDiskRecoverability, ProjectDiskRevision,
    ProjectDiskUseObservation, ResidentSandboxGeneration, ResidentSandboxId,
};

pub const PROJECT_DISK_ATTACHMENT_STATE_SCHEMA_VERSION: u8 = 1;
const ZERO_DIGEST: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000000";

macro_rules! positive_generation_type {
    ($name:ident, $code:literal, $message:literal) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(u64);

        impl $name {
            pub fn new(value: u64) -> Result<Self, ProjectDiskAttachmentStateError> {
                if value == 0 {
                    return Err(error(
                        ProjectDiskAttachmentStateErrorKind::InvalidGeneration,
                        $code,
                        $message,
                    ));
                }
                Ok(Self(value))
            }

            #[must_use]
            pub const fn get(self) -> u64 {
                self.0
            }
        }
    };
}

positive_generation_type!(
    ProjectDiskAttachmentRevision,
    "project_disk_attachment_revision_invalid",
    "project disk attachment revision must be greater than zero"
);
positive_generation_type!(
    ProjectDiskAttachTransactionGeneration,
    "project_disk_attach_transaction_generation_invalid",
    "project disk attach transaction generation must be greater than zero"
);

impl ProjectDiskAttachmentRevision {
    fn next(self) -> Result<Self, ProjectDiskAttachmentStateError> {
        Self::new(self.0.checked_add(1).ok_or_else(generation_exhausted)?)
    }
}

impl ProjectDiskAttachTransactionGeneration {
    fn next(self) -> Result<Self, ProjectDiskAttachmentStateError> {
        Self::new(self.0.checked_add(1).ok_or_else(generation_exhausted)?)
    }
}

/// Sealed P4 authority that the exact filesystem generation completed formatting and formatter
/// cleanup and is currently detached.
///
/// Production construction remains crate-private until the format-state/P2 adapter is composed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiskFormattedDetachedAuthority {
    filesystem: ProjectDiskFilesystemBinding,
    format_outcome_digest: Sha256Digest,
}

impl ProjectDiskFormattedDetachedAuthority {
    #[cfg(test)]
    fn for_test(
        filesystem: ProjectDiskFilesystemBinding,
        format_outcome_digest: Sha256Digest,
    ) -> Result<Self, ProjectDiskAttachmentStateError> {
        if format_outcome_digest.as_str() == ZERO_DIGEST {
            return Err(invalid_input());
        }
        Ok(Self {
            filesystem,
            format_outcome_digest,
        })
    }

    #[must_use]
    pub const fn filesystem(&self) -> &ProjectDiskFilesystemBinding {
        &self.filesystem
    }

    #[must_use]
    pub const fn format_outcome_digest(&self) -> &Sha256Digest {
        &self.format_outcome_digest
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDiskAttachRecoveryAssessment {
    BlockedPriorAttachMayCommit {
        transaction_generation: ProjectDiskAttachTransactionGeneration,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDiskAttachQuarantineReason {
    ConflictingPhysicalState,
    WrongResidentAttachment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiskAttachmentReceipt {
    transaction_generation: ProjectDiskAttachTransactionGeneration,
    disk_revision: ProjectDiskRevision,
    attachment_generation: ProjectDiskAttachmentGeneration,
    sandbox_id: ResidentSandboxId,
    sandbox_generation: ResidentSandboxGeneration,
    filesystem_generation: ProjectDiskFilesystemGeneration,
    format_profile_generation: ProjectDiskFilesystemFormatProfileGeneration,
    filesystem_kind: ProjectDiskFilesystemKind,
    correlation_digest: Sha256Digest,
}

impl ProjectDiskAttachmentReceipt {
    #[must_use]
    pub const fn transaction_generation(&self) -> ProjectDiskAttachTransactionGeneration {
        self.transaction_generation
    }

    #[must_use]
    pub const fn disk_revision(&self) -> ProjectDiskRevision {
        self.disk_revision
    }

    #[must_use]
    pub const fn attachment_generation(&self) -> ProjectDiskAttachmentGeneration {
        self.attachment_generation
    }

    #[must_use]
    pub const fn sandbox_id(&self) -> &ResidentSandboxId {
        &self.sandbox_id
    }

    #[must_use]
    pub const fn sandbox_generation(&self) -> ResidentSandboxGeneration {
        self.sandbox_generation
    }

    #[must_use]
    pub const fn filesystem_generation(&self) -> ProjectDiskFilesystemGeneration {
        self.filesystem_generation
    }

    #[must_use]
    pub const fn correlation_digest(&self) -> &Sha256Digest {
        &self.correlation_digest
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProjectDiskAttachmentState {
    FormattedDetached,
    AttachAuthorized {
        transaction_generation: ProjectDiskAttachTransactionGeneration,
        expected_disk_revision: ProjectDiskRevision,
        attachment: ProjectDiskAttachmentLease,
    },
    AttachStarted {
        transaction_generation: ProjectDiskAttachTransactionGeneration,
        expected_disk_revision: ProjectDiskRevision,
        attachment: ProjectDiskAttachmentLease,
    },
    AttachRecoveryRequired {
        transaction_generation: ProjectDiskAttachTransactionGeneration,
        expected_disk_revision: ProjectDiskRevision,
        attachment: ProjectDiskAttachmentLease,
    },
    AttachmentObserved {
        receipt: ProjectDiskAttachmentReceipt,
    },
    Quarantined {
        transaction_generation: ProjectDiskAttachTransactionGeneration,
        reason: ProjectDiskAttachQuarantineReason,
    },
}

/// P4 transaction record for one formatted project-disk filesystem generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiskAttachmentRecord {
    schema_version: u8,
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    filesystem: ProjectDiskFilesystemBinding,
    format_outcome_digest: Sha256Digest,
    revision: ProjectDiskAttachmentRevision,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_transaction_generation: Option<ProjectDiskAttachTransactionGeneration>,
    state: ProjectDiskAttachmentState,
}

impl ProjectDiskAttachmentRecord {
    /// Start P4 attachment tracking only from sealed formatted-detached authority.
    #[must_use]
    pub fn from_formatted_detached(authority: ProjectDiskFormattedDetachedAuthority) -> Self {
        let filesystem = authority.filesystem;
        Self {
            schema_version: PROJECT_DISK_ATTACHMENT_STATE_SCHEMA_VERSION,
            project: filesystem.project().clone(),
            disk_id: filesystem.disk_id().clone(),
            disk_generation: filesystem.disk_generation(),
            filesystem,
            format_outcome_digest: authority.format_outcome_digest,
            revision: ProjectDiskAttachmentRevision(1),
            last_transaction_generation: None,
            state: ProjectDiskAttachmentState::FormattedDetached,
        }
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn project(&self) -> &ProjectIdentity {
        &self.project
    }

    #[must_use]
    pub const fn disk_id(&self) -> &ProjectDiskId {
        &self.disk_id
    }

    #[must_use]
    pub const fn disk_generation(&self) -> ProjectDiskGeneration {
        self.disk_generation
    }

    #[must_use]
    pub const fn filesystem(&self) -> &ProjectDiskFilesystemBinding {
        &self.filesystem
    }

    #[must_use]
    pub const fn revision(&self) -> ProjectDiskAttachmentRevision {
        self.revision
    }

    #[must_use]
    pub const fn state(&self) -> &ProjectDiskAttachmentState {
        &self.state
    }

    /// Plan one exact P4 attachment around the P1 plan and fresh sealed detached preconditions.
    pub fn plan_attach(
        &self,
        current: &ProjectDiskLeaseRecord,
        plan: &ProjectDiskAttachPlan,
        precondition: &ProjectDiskAttachPreconditionProof,
    ) -> Result<ProjectDiskAttachmentIntent, ProjectDiskAttachmentStateError> {
        if !matches!(self.state, ProjectDiskAttachmentState::FormattedDetached) {
            return Err(invalid_state(
                "project_disk_p4_attach_requires_formatted_detached",
                "project disk attachment requires formatted-detached state",
            ));
        }
        self.confirm_current_detached_plan(current, plan)?;
        precondition.confirm(self, current, plan)?;
        Ok(ProjectDiskAttachmentIntent {
            project: self.project.clone(),
            disk_id: self.disk_id.clone(),
            disk_generation: self.disk_generation,
            expected_record_revision: self.revision,
            expected_disk_revision: current.revision(),
            transaction_generation: self.next_transaction_generation()?,
            attachment: plan.attachment().clone(),
        })
    }

    pub fn record_attach_authorized(
        &self,
        current: &ProjectDiskLeaseRecord,
        plan: &ProjectDiskAttachPlan,
        intent: &ProjectDiskAttachmentIntent,
    ) -> Result<Self, ProjectDiskAttachmentStateError> {
        if !matches!(self.state, ProjectDiskAttachmentState::FormattedDetached) {
            return Err(invalid_state(
                "project_disk_p4_attach_requires_formatted_detached",
                "project disk attachment requires formatted-detached state",
            ));
        }
        self.confirm_current_detached_plan(current, plan)?;
        intent.confirm(self, current, plan)?;
        self.successor(
            ProjectDiskAttachmentState::AttachAuthorized {
                transaction_generation: intent.transaction_generation,
                expected_disk_revision: intent.expected_disk_revision,
                attachment: intent.attachment.clone(),
            },
            Some(intent.transaction_generation),
        )
    }

    /// Produce the exact no-replay start plan for the active authorized attachment.
    pub fn plan_attach_start(
        &self,
    ) -> Result<ProjectDiskAttachmentStartPlan, ProjectDiskAttachmentStateError> {
        let ProjectDiskAttachmentState::AttachAuthorized {
            transaction_generation,
            expected_disk_revision,
            attachment,
        } = &self.state
        else {
            return Err(invalid_state(
                "project_disk_p4_attach_start_requires_authorized",
                "project disk attach start requires an authorized transaction",
            ));
        };
        Ok(ProjectDiskAttachmentStartPlan {
            project: self.project.clone(),
            disk_id: self.disk_id.clone(),
            disk_generation: self.disk_generation,
            attachment_record_revision: self.revision,
            transaction_generation: *transaction_generation,
            expected_disk_revision: *expected_disk_revision,
            attachment: attachment.clone(),
            filesystem_generation: self.filesystem.filesystem_generation(),
            format_profile_generation: self.filesystem.format_profile_generation(),
            filesystem_kind: self.filesystem.kind(),
            format_outcome_digest: self.format_outcome_digest.clone(),
        })
    }

    /// Cross the attach no-replay boundary immediately before the external attach/start executor.
    pub fn record_attach_started(
        &self,
        start: &ProjectDiskAttachmentStartPlan,
    ) -> Result<Self, ProjectDiskAttachmentStateError> {
        let ProjectDiskAttachmentState::AttachAuthorized {
            transaction_generation,
            expected_disk_revision,
            attachment,
        } = &self.state
        else {
            return Err(invalid_state(
                "project_disk_p4_attach_start_requires_authorized",
                "project disk attach start requires an authorized transaction",
            ));
        };
        start.confirm(self, *transaction_generation, *expected_disk_revision, attachment)?;
        self.successor(
            ProjectDiskAttachmentState::AttachStarted {
                transaction_generation: *transaction_generation,
                expected_disk_revision: *expected_disk_revision,
                attachment: attachment.clone(),
            },
            self.last_transaction_generation,
        )
    }

    /// Mark an ambiguous attach/start outcome. No method in this state retries the external action.
    pub fn require_attach_recovery(&self) -> Result<Self, ProjectDiskAttachmentStateError> {
        let ProjectDiskAttachmentState::AttachStarted {
            transaction_generation,
            expected_disk_revision,
            attachment,
        } = &self.state
        else {
            return Err(started_required());
        };
        self.successor(
            ProjectDiskAttachmentState::AttachRecoveryRequired {
                transaction_generation: *transaction_generation,
                expected_disk_revision: *expected_disk_revision,
                attachment: attachment.clone(),
            },
            self.last_transaction_generation,
        )
    }

    /// Accept the exact planned attachment from either the uninterrupted or reconciliation path.
    ///
    /// The sealed success proof requires host physical/resident evidence and guest filesystem +
    /// whole-block-device evidence for the exact P1 attachment generation.
    pub fn record_attachment_observed(
        &self,
        proof: &ProjectDiskAttachmentSuccessProof,
    ) -> Result<Self, ProjectDiskAttachmentStateError> {
        let (transaction_generation, expected_disk_revision, attachment) = match &self.state {
            ProjectDiskAttachmentState::AttachStarted {
                transaction_generation,
                expected_disk_revision,
                attachment,
            }
            | ProjectDiskAttachmentState::AttachRecoveryRequired {
                transaction_generation,
                expected_disk_revision,
                attachment,
            } => (
                *transaction_generation,
                *expected_disk_revision,
                attachment,
            ),
            _ => return Err(started_required()),
        };
        proof.confirm(
            self,
            transaction_generation,
            expected_disk_revision,
            attachment,
        )?;
        let receipt = ProjectDiskAttachmentReceipt {
            transaction_generation,
            disk_revision: expected_disk_revision,
            attachment_generation: attachment.generation(),
            sandbox_id: attachment.sandbox_id().clone(),
            sandbox_generation: attachment.sandbox_generation(),
            filesystem_generation: self.filesystem.filesystem_generation(),
            format_profile_generation: self.filesystem.format_profile_generation(),
            filesystem_kind: self.filesystem.kind(),
            correlation_digest: proof.correlation_digest.clone(),
        };
        self.successor(
            ProjectDiskAttachmentState::AttachmentObserved { receipt },
            self.last_transaction_generation,
        )
    }

    /// Publish the P1 attached successor only from the exact sealed P4 success proof.
    ///
    /// This is pure state composition. The returned P1 record is the ordinary revision-advanced
    /// successor from `ProjectDiskLeaseRecord::record_attach_success`.
    pub fn accept_p1_attach_success(
        &self,
        current: &ProjectDiskLeaseRecord,
        plan: &ProjectDiskAttachPlan,
        proof: &ProjectDiskAttachmentSuccessProof,
    ) -> Result<ProjectDiskLeaseRecord, ProjectDiskAttachmentStateError> {
        let ProjectDiskAttachmentState::AttachmentObserved { receipt } = &self.state else {
            return Err(invalid_state(
                "project_disk_p4_attach_observation_required",
                "P1 attach publication requires exact P4 attachment observation",
            ));
        };
        self.confirm_current_detached_plan(current, plan)?;
        if receipt.disk_revision != current.revision()
            || receipt.attachment_generation != plan.attachment().generation()
            || receipt.sandbox_id != *plan.attachment().sandbox_id()
            || receipt.sandbox_generation != plan.attachment().sandbox_generation()
            || receipt.correlation_digest != proof.correlation_digest
        {
            return Err(evidence_mismatch());
        }
        proof.confirm(
            self,
            receipt.transaction_generation,
            receipt.disk_revision,
            plan.attachment(),
        )?;
        current
            .record_attach_success(plan, proof.p1_post_observation())
            .map_err(map_p1_error)
    }

    /// Fresh detached/unused evidence after `AttachStarted` remains blocked while the prior external
    /// attach process may still commit.
    pub fn assess_started_detached(
        &self,
        detached: &ProjectDiskAttachDetachedProof,
    ) -> Result<ProjectDiskAttachRecoveryAssessment, ProjectDiskAttachmentStateError> {
        let (transaction_generation, expected_disk_revision, attachment) = match &self.state {
            ProjectDiskAttachmentState::AttachStarted {
                transaction_generation,
                expected_disk_revision,
                attachment,
            }
            | ProjectDiskAttachmentState::AttachRecoveryRequired {
                transaction_generation,
                expected_disk_revision,
                attachment,
            } => (
                *transaction_generation,
                *expected_disk_revision,
                attachment,
            ),
            _ => return Err(started_required()),
        };
        detached.confirm(
            self,
            transaction_generation,
            expected_disk_revision,
            attachment,
        )?;
        Ok(ProjectDiskAttachRecoveryAssessment::BlockedPriorAttachMayCommit {
            transaction_generation,
        })
    }

    /// Reopen formatted-detached state only after a separate proof says the prior process cannot
    /// still commit and fresh evidence proves the disk detached/unused. The next transaction is N+1.
    pub fn record_prior_attach_quiescent_detached(
        &self,
        quiescent: &ProjectDiskPriorAttachQuiescentProof,
        detached: &ProjectDiskAttachDetachedProof,
    ) -> Result<Self, ProjectDiskAttachmentStateError> {
        let (transaction_generation, expected_disk_revision, attachment) = match &self.state {
            ProjectDiskAttachmentState::AttachStarted {
                transaction_generation,
                expected_disk_revision,
                attachment,
            }
            | ProjectDiskAttachmentState::AttachRecoveryRequired {
                transaction_generation,
                expected_disk_revision,
                attachment,
            } => (
                *transaction_generation,
                *expected_disk_revision,
                attachment,
            ),
            _ => return Err(started_required()),
        };
        quiescent.confirm(self, transaction_generation, attachment)?;
        detached.confirm(
            self,
            transaction_generation,
            expected_disk_revision,
            attachment,
        )?;
        self.successor(
            ProjectDiskAttachmentState::FormattedDetached,
            self.last_transaction_generation,
        )
    }

    pub fn quarantine_started_conflict(
        &self,
        reason: ProjectDiskAttachQuarantineReason,
    ) -> Result<Self, ProjectDiskAttachmentStateError> {
        let transaction_generation = match &self.state {
            ProjectDiskAttachmentState::AttachStarted {
                transaction_generation,
                ..
            }
            | ProjectDiskAttachmentState::AttachRecoveryRequired {
                transaction_generation,
                ..
            } => *transaction_generation,
            _ => return Err(started_required()),
        };
        self.successor(
            ProjectDiskAttachmentState::Quarantined {
                transaction_generation,
                reason,
            },
            self.last_transaction_generation,
        )
    }

    fn confirm_current_detached_plan(
        &self,
        current: &ProjectDiskLeaseRecord,
        plan: &ProjectDiskAttachPlan,
    ) -> Result<(), ProjectDiskAttachmentStateError> {
        if current.project() != &self.project
            || current.disk_id() != &self.disk_id
            || current.disk_generation() != self.disk_generation
            || current.revision() != plan.identity().expected_revision()
            || !matches!(current.state(), ProjectDiskLeaseState::Detached)
            || !self.filesystem.matches_project_disk(current)
        {
            return Err(plan_mismatch());
        }
        Ok(())
    }

    fn next_transaction_generation(
        &self,
    ) -> Result<ProjectDiskAttachTransactionGeneration, ProjectDiskAttachmentStateError> {
        match self.last_transaction_generation {
            Some(generation) => generation.next(),
            None => ProjectDiskAttachTransactionGeneration::new(1),
        }
    }

    fn successor(
        &self,
        state: ProjectDiskAttachmentState,
        last_transaction_generation: Option<ProjectDiskAttachTransactionGeneration>,
    ) -> Result<Self, ProjectDiskAttachmentStateError> {
        Ok(Self {
            schema_version: self.schema_version,
            project: self.project.clone(),
            disk_id: self.disk_id.clone(),
            disk_generation: self.disk_generation,
            filesystem: self.filesystem.clone(),
            format_outcome_digest: self.format_outcome_digest.clone(),
            revision: self.revision.next()?,
            last_transaction_generation,
            state,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDiskAttachmentIntent {
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    expected_record_revision: ProjectDiskAttachmentRevision,
    expected_disk_revision: ProjectDiskRevision,
    transaction_generation: ProjectDiskAttachTransactionGeneration,
    attachment: ProjectDiskAttachmentLease,
}

impl ProjectDiskAttachmentIntent {
    #[must_use]
    pub const fn transaction_generation(&self) -> ProjectDiskAttachTransactionGeneration {
        self.transaction_generation
    }

    #[must_use]
    pub const fn attachment(&self) -> &ProjectDiskAttachmentLease {
        &self.attachment
    }

    fn confirm(
        &self,
        record: &ProjectDiskAttachmentRecord,
        current: &ProjectDiskLeaseRecord,
        plan: &ProjectDiskAttachPlan,
    ) -> Result<(), ProjectDiskAttachmentStateError> {
        if self.project != record.project
            || self.disk_id != record.disk_id
            || self.disk_generation != record.disk_generation
            || self.expected_record_revision != record.revision
            || self.expected_disk_revision != current.revision()
            || self.transaction_generation != record.next_transaction_generation()?
            || self.attachment != *plan.attachment()
        {
            return Err(plan_mismatch());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDiskAttachmentStartPlan {
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    attachment_record_revision: ProjectDiskAttachmentRevision,
    transaction_generation: ProjectDiskAttachTransactionGeneration,
    expected_disk_revision: ProjectDiskRevision,
    attachment: ProjectDiskAttachmentLease,
    filesystem_generation: ProjectDiskFilesystemGeneration,
    format_profile_generation: ProjectDiskFilesystemFormatProfileGeneration,
    filesystem_kind: ProjectDiskFilesystemKind,
    format_outcome_digest: Sha256Digest,
}

impl ProjectDiskAttachmentStartPlan {
    #[must_use]
    pub const fn transaction_generation(&self) -> ProjectDiskAttachTransactionGeneration {
        self.transaction_generation
    }

    #[must_use]
    pub const fn expected_disk_revision(&self) -> ProjectDiskRevision {
        self.expected_disk_revision
    }

    #[must_use]
    pub const fn attachment(&self) -> &ProjectDiskAttachmentLease {
        &self.attachment
    }

    fn confirm(
        &self,
        record: &ProjectDiskAttachmentRecord,
        transaction_generation: ProjectDiskAttachTransactionGeneration,
        expected_disk_revision: ProjectDiskRevision,
        attachment: &ProjectDiskAttachmentLease,
    ) -> Result<(), ProjectDiskAttachmentStateError> {
        if self.project != record.project
            || self.disk_id != record.disk_id
            || self.disk_generation != record.disk_generation
            || self.attachment_record_revision != record.revision
            || self.transaction_generation != transaction_generation
            || self.expected_disk_revision != expected_disk_revision
            || &self.attachment != attachment
            || self.filesystem_generation != record.filesystem.filesystem_generation()
            || self.format_profile_generation != record.filesystem.format_profile_generation()
            || self.filesystem_kind != record.filesystem.kind()
            || self.format_outcome_digest != record.format_outcome_digest
        {
            return Err(plan_mismatch());
        }
        Ok(())
    }
}

/// Sealed fresh host precondition for the exact P1 plan on the formatted-detached filesystem.
pub struct ProjectDiskAttachPreconditionProof {
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    disk_revision: ProjectDiskRevision,
    attachment: ProjectDiskAttachmentLease,
    filesystem_generation: ProjectDiskFilesystemGeneration,
    format_profile_generation: ProjectDiskFilesystemFormatProfileGeneration,
    filesystem_kind: ProjectDiskFilesystemKind,
    format_outcome_digest: Sha256Digest,
    physical_exact_detached_unused: bool,
}

impl ProjectDiskAttachPreconditionProof {
    fn confirm(
        &self,
        record: &ProjectDiskAttachmentRecord,
        current: &ProjectDiskLeaseRecord,
        plan: &ProjectDiskAttachPlan,
    ) -> Result<(), ProjectDiskAttachmentStateError> {
        if self.project != record.project
            || self.disk_id != record.disk_id
            || self.disk_generation != record.disk_generation
            || self.disk_revision != current.revision()
            || self.attachment != *plan.attachment()
            || self.filesystem_generation != record.filesystem.filesystem_generation()
            || self.format_profile_generation != record.filesystem.format_profile_generation()
            || self.filesystem_kind != record.filesystem.kind()
            || self.format_outcome_digest != record.format_outcome_digest
            || !self.physical_exact_detached_unused
        {
            return Err(evidence_mismatch());
        }
        Ok(())
    }

    #[cfg(test)]
    fn for_test(
        record: &ProjectDiskAttachmentRecord,
        current: &ProjectDiskLeaseRecord,
        plan: &ProjectDiskAttachPlan,
    ) -> Self {
        Self {
            project: record.project.clone(),
            disk_id: record.disk_id.clone(),
            disk_generation: record.disk_generation,
            disk_revision: current.revision(),
            attachment: plan.attachment().clone(),
            filesystem_generation: record.filesystem.filesystem_generation(),
            format_profile_generation: record.filesystem.format_profile_generation(),
            filesystem_kind: record.filesystem.kind(),
            format_outcome_digest: record.format_outcome_digest.clone(),
            physical_exact_detached_unused: true,
        }
    }
}

/// Sealed fresh correlation for the exact planned P1 attachment.
///
/// Production construction remains absent. The future P2/guest composition must require exact
/// physical disk identity, exact resident host identity, guest filesystem generation, mountinfo,
/// and whole-block-device correlation before constructing this value.
pub struct ProjectDiskAttachmentSuccessProof {
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    disk_revision: ProjectDiskRevision,
    attachment: ProjectDiskAttachmentLease,
    filesystem_generation: ProjectDiskFilesystemGeneration,
    format_profile_generation: ProjectDiskFilesystemFormatProfileGeneration,
    filesystem_kind: ProjectDiskFilesystemKind,
    host_physical_identity_bound: bool,
    resident_host_identity_bound: bool,
    guest_filesystem_device_bound: bool,
    whole_block_device_bound: bool,
    correlation_digest: Sha256Digest,
}

impl ProjectDiskAttachmentSuccessProof {
    fn confirm(
        &self,
        record: &ProjectDiskAttachmentRecord,
        _transaction_generation: ProjectDiskAttachTransactionGeneration,
        expected_disk_revision: ProjectDiskRevision,
        attachment: &ProjectDiskAttachmentLease,
    ) -> Result<(), ProjectDiskAttachmentStateError> {
        if self.project != record.project
            || self.disk_id != record.disk_id
            || self.disk_generation != record.disk_generation
            || self.disk_revision != expected_disk_revision
            || &self.attachment != attachment
            || self.filesystem_generation != record.filesystem.filesystem_generation()
            || self.format_profile_generation != record.filesystem.format_profile_generation()
            || self.filesystem_kind != record.filesystem.kind()
            || !self.host_physical_identity_bound
            || !self.resident_host_identity_bound
            || !self.guest_filesystem_device_bound
            || !self.whole_block_device_bound
            || self.correlation_digest.as_str() == ZERO_DIGEST
        {
            return Err(evidence_mismatch());
        }
        Ok(())
    }

    const fn p1_post_observation(&self) -> ProjectDiskObservation {
        ProjectDiskObservation::new(
            ProjectDiskPhysicalObservation::Exact,
            ProjectDiskUseObservation::CurrentAttachment,
            ProjectDiskLockObservation::CurrentAttachment,
            ProjectDiskRecoverability::Unknown,
        )
    }

    #[cfg(test)]
    fn for_test(
        record: &ProjectDiskAttachmentRecord,
        current: &ProjectDiskLeaseRecord,
        plan: &ProjectDiskAttachPlan,
        digest: Sha256Digest,
    ) -> Self {
        Self {
            project: record.project.clone(),
            disk_id: record.disk_id.clone(),
            disk_generation: record.disk_generation,
            disk_revision: current.revision(),
            attachment: plan.attachment().clone(),
            filesystem_generation: record.filesystem.filesystem_generation(),
            format_profile_generation: record.filesystem.format_profile_generation(),
            filesystem_kind: record.filesystem.kind(),
            host_physical_identity_bound: true,
            resident_host_identity_bound: true,
            guest_filesystem_device_bound: true,
            whole_block_device_bound: true,
            correlation_digest: digest,
        }
    }
}

/// Sealed fresh P2 evidence that the exact owned disk is currently detached and unused.
pub struct ProjectDiskAttachDetachedProof {
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    disk_revision: ProjectDiskRevision,
    attachment: ProjectDiskAttachmentLease,
    exact_detached_unused: bool,
}

impl ProjectDiskAttachDetachedProof {
    fn confirm(
        &self,
        record: &ProjectDiskAttachmentRecord,
        _transaction_generation: ProjectDiskAttachTransactionGeneration,
        expected_disk_revision: ProjectDiskRevision,
        attachment: &ProjectDiskAttachmentLease,
    ) -> Result<(), ProjectDiskAttachmentStateError> {
        if self.project != record.project
            || self.disk_id != record.disk_id
            || self.disk_generation != record.disk_generation
            || self.disk_revision != expected_disk_revision
            || &self.attachment != attachment
            || !self.exact_detached_unused
        {
            return Err(evidence_mismatch());
        }
        Ok(())
    }

    #[cfg(test)]
    fn for_test(
        record: &ProjectDiskAttachmentRecord,
        current: &ProjectDiskLeaseRecord,
        plan: &ProjectDiskAttachPlan,
    ) -> Self {
        Self {
            project: record.project.clone(),
            disk_id: record.disk_id.clone(),
            disk_generation: record.disk_generation,
            disk_revision: current.revision(),
            attachment: plan.attachment().clone(),
            exact_detached_unused: true,
        }
    }
}

/// Sealed future recovery proof that the prior external attach/start process cannot still commit.
pub struct ProjectDiskPriorAttachQuiescentProof {
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    attachment: ProjectDiskAttachmentLease,
}

impl ProjectDiskPriorAttachQuiescentProof {
    fn confirm(
        &self,
        record: &ProjectDiskAttachmentRecord,
        _transaction_generation: ProjectDiskAttachTransactionGeneration,
        attachment: &ProjectDiskAttachmentLease,
    ) -> Result<(), ProjectDiskAttachmentStateError> {
        if self.project != record.project
            || self.disk_id != record.disk_id
            || self.disk_generation != record.disk_generation
            || &self.attachment != attachment
        {
            return Err(evidence_mismatch());
        }
        Ok(())
    }

    #[cfg(test)]
    fn for_test(
        record: &ProjectDiskAttachmentRecord,
        plan: &ProjectDiskAttachPlan,
    ) -> Self {
        Self {
            project: record.project.clone(),
            disk_id: record.disk_id.clone(),
            disk_generation: record.disk_generation,
            attachment: plan.attachment().clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDiskAttachmentStateErrorKind {
    InvalidGeneration,
    InvalidInput,
    InvalidState,
    PlanMismatch,
    EvidenceMismatch,
    GenerationExhausted,
    P1,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProjectDiskAttachmentStateError {
    kind: ProjectDiskAttachmentStateErrorKind,
    code: &'static str,
    message: &'static str,
}

impl ProjectDiskAttachmentStateError {
    #[must_use]
    pub const fn kind(self) -> ProjectDiskAttachmentStateErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for ProjectDiskAttachmentStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectDiskAttachmentStateError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for ProjectDiskAttachmentStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ProjectDiskAttachmentStateError {}

const fn error(
    kind: ProjectDiskAttachmentStateErrorKind,
    code: &'static str,
    message: &'static str,
) -> ProjectDiskAttachmentStateError {
    ProjectDiskAttachmentStateError {
        kind,
        code,
        message,
    }
}

const fn invalid_input() -> ProjectDiskAttachmentStateError {
    error(
        ProjectDiskAttachmentStateErrorKind::InvalidInput,
        "project_disk_attachment_input_invalid",
        "project disk attachment input is invalid",
    )
}

const fn invalid_state(
    code: &'static str,
    message: &'static str,
) -> ProjectDiskAttachmentStateError {
    error(ProjectDiskAttachmentStateErrorKind::InvalidState, code, message)
}

const fn plan_mismatch() -> ProjectDiskAttachmentStateError {
    error(
        ProjectDiskAttachmentStateErrorKind::PlanMismatch,
        "project_disk_attachment_plan_mismatch",
        "project disk attachment plan does not match current authority",
    )
}

const fn evidence_mismatch() -> ProjectDiskAttachmentStateError {
    error(
        ProjectDiskAttachmentStateErrorKind::EvidenceMismatch,
        "project_disk_attachment_evidence_mismatch",
        "project disk attachment evidence does not match the active transaction",
    )
}

const fn started_required() -> ProjectDiskAttachmentStateError {
    invalid_state(
        "project_disk_attachment_started_required",
        "project disk attachment recovery requires a started transaction",
    )
}

const fn generation_exhausted() -> ProjectDiskAttachmentStateError {
    error(
        ProjectDiskAttachmentStateErrorKind::GenerationExhausted,
        "project_disk_attachment_generation_exhausted",
        "project disk attachment generation is exhausted",
    )
}

fn map_p1_error(_error: ProjectDiskLeaseError) -> ProjectDiskAttachmentStateError {
    error(
        ProjectDiskAttachmentStateErrorKind::P1,
        "project_disk_attachment_p1_rejected",
        "P1 project disk lease rejected the attachment publication",
    )
}

#[cfg(test)]
mod tests {
    use super::{
        ProjectDiskAttachDetachedProof, ProjectDiskAttachPreconditionProof,
        ProjectDiskAttachQuarantineReason, ProjectDiskAttachRecoveryAssessment,
        ProjectDiskAttachmentRecord, ProjectDiskAttachmentState,
        ProjectDiskAttachmentSuccessProof, ProjectDiskFormattedDetachedAuthority,
        ProjectDiskPriorAttachQuiescentProof,
    };
    use crate::artifact::Sha256Digest;
    use crate::project_catalog::ProjectIdentity;
    use crate::project_disk_filesystem::{
        ProjectDiskFilesystemBinding, ProjectDiskFilesystemFormatProfileGeneration,
        ProjectDiskFilesystemGeneration, ProjectDiskFilesystemKind,
    };
    use crate::project_disk_lease::{
        ProjectDiskGeneration, ProjectDiskId, ProjectDiskLeaseRecord, ProjectDiskLockObservation,
        ProjectDiskObservation, ProjectDiskPhysicalObservation, ProjectDiskRecoverability,
        ProjectDiskUseObservation, ResidentSandboxGeneration, ResidentSandboxId,
    };

    fn digest(byte: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    fn detached() -> ProjectDiskLeaseRecord {
        ProjectDiskLeaseRecord::new_detached(
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            ProjectDiskId::parse("disk-a").unwrap(),
            ProjectDiskGeneration::new(3).unwrap(),
        )
    }

    fn record() -> ProjectDiskAttachmentRecord {
        let current = detached();
        let fs = ProjectDiskFilesystemBinding::new(
            &current,
            ProjectDiskFilesystemGeneration::new(7).unwrap(),
            ProjectDiskFilesystemFormatProfileGeneration::new(2).unwrap(),
            ProjectDiskFilesystemKind::Ext4,
        );
        ProjectDiskAttachmentRecord::from_formatted_detached(
            ProjectDiskFormattedDetachedAuthority::for_test(fs, digest('a')).unwrap(),
        )
    }

    const fn detached_exact() -> ProjectDiskObservation {
        ProjectDiskObservation::new(
            ProjectDiskPhysicalObservation::Exact,
            ProjectDiskUseObservation::Unused,
            ProjectDiskLockObservation::Unlocked,
            ProjectDiskRecoverability::Unknown,
        )
    }

    fn attach_plan(current: &ProjectDiskLeaseRecord) -> crate::project_disk_lease::ProjectDiskAttachPlan {
        current
            .plan_attach(
                ResidentSandboxId::parse("sandbox-a").unwrap(),
                ResidentSandboxGeneration::new(11).unwrap(),
                detached_exact(),
            )
            .unwrap()
    }

    fn started(
        record: &ProjectDiskAttachmentRecord,
        current: &ProjectDiskLeaseRecord,
        plan: &crate::project_disk_lease::ProjectDiskAttachPlan,
    ) -> ProjectDiskAttachmentRecord {
        let intent = record
            .plan_attach(
                current,
                plan,
                &ProjectDiskAttachPreconditionProof::for_test(record, current, plan),
            )
            .unwrap();
        let authorized = record
            .record_attach_authorized(current, plan, &intent)
            .unwrap();
        let start = authorized.plan_attach_start().unwrap();
        authorized.record_attach_started(&start).unwrap()
    }

    #[test]
    fn exact_p1_plan_is_carried_into_p4_transaction() {
        let current = detached();
        let plan = attach_plan(&current);
        let record = record();
        let intent = record
            .plan_attach(
                &current,
                &plan,
                &ProjectDiskAttachPreconditionProof::for_test(&record, &current, &plan),
            )
            .unwrap();
        assert_eq!(intent.transaction_generation().get(), 1);
        assert_eq!(intent.attachment(), plan.attachment());
        let authorized = record
            .record_attach_authorized(&current, &plan, &intent)
            .unwrap();
        let start = authorized.plan_attach_start().unwrap();
        assert_eq!(start.attachment(), plan.attachment());
        let started = authorized.record_attach_started(&start).unwrap();
        assert!(matches!(
            started.state(),
            ProjectDiskAttachmentState::AttachStarted { .. }
        ));
    }

    #[test]
    fn ambiguous_attach_enters_no_replay_recovery() {
        let current = detached();
        let plan = attach_plan(&current);
        let started = started(&record(), &current, &plan);
        let recovery = started.require_attach_recovery().unwrap();
        assert!(matches!(
            recovery.state(),
            ProjectDiskAttachmentState::AttachRecoveryRequired { .. }
        ));
        assert!(recovery.plan_attach_start().is_err());
    }

    #[test]
    fn exact_observed_attachment_can_reconcile_same_transaction() {
        let current = detached();
        let plan = attach_plan(&current);
        let started = started(&record(), &current, &plan);
        let recovery = started.require_attach_recovery().unwrap();
        let proof = ProjectDiskAttachmentSuccessProof::for_test(
            &recovery,
            &current,
            &plan,
            digest('b'),
        );
        let observed = recovery.record_attachment_observed(&proof).unwrap();
        let ProjectDiskAttachmentState::AttachmentObserved { receipt } = observed.state() else {
            panic!("expected observed attachment");
        };
        assert_eq!(receipt.transaction_generation().get(), 1);
        assert_eq!(receipt.attachment_generation(), plan.attachment().generation());
    }

    #[test]
    fn p1_attached_publication_requires_sealed_success_proof() {
        let current = detached();
        let plan = attach_plan(&current);
        let started = started(&record(), &current, &plan);
        let proof = ProjectDiskAttachmentSuccessProof::for_test(
            &started,
            &current,
            &plan,
            digest('b'),
        );
        let observed = started.record_attachment_observed(&proof).unwrap();
        let attached = observed
            .accept_p1_attach_success(&current, &plan, &proof)
            .unwrap();
        let ProjectDiskLeaseState::Attached { attachment } = attached.state() else {
            panic!("expected P1 attached successor");
        };
        assert_eq!(attachment, plan.attachment());
    }

    #[test]
    fn fresh_detached_after_started_remains_blocked_until_prior_process_quiescent() {
        let current = detached();
        let plan = attach_plan(&current);
        let started = started(&record(), &current, &plan);
        let detached = ProjectDiskAttachDetachedProof::for_test(&started, &current, &plan);
        assert_eq!(
            started.assess_started_detached(&detached).unwrap(),
            ProjectDiskAttachRecoveryAssessment::BlockedPriorAttachMayCommit {
                transaction_generation: crate::project_disk_attachment_state::ProjectDiskAttachTransactionGeneration::new(1).unwrap(),
            }
        );
        let recovered = started
            .record_prior_attach_quiescent_detached(
                &ProjectDiskPriorAttachQuiescentProof::for_test(&started, &plan),
                &detached,
            )
            .unwrap();
        assert!(matches!(
            recovered.state(),
            ProjectDiskAttachmentState::FormattedDetached
        ));
        let next_plan = attach_plan(&current);
        let next_intent = recovered
            .plan_attach(
                &current,
                &next_plan,
                &ProjectDiskAttachPreconditionProof::for_test(
                    &recovered,
                    &current,
                    &next_plan,
                ),
            )
            .unwrap();
        assert_eq!(next_intent.transaction_generation().get(), 2);
    }

    #[test]
    fn conflict_only_quarantines() {
        let current = detached();
        let plan = attach_plan(&current);
        let started = started(&record(), &current, &plan);
        let quarantined = started
            .quarantine_started_conflict(ProjectDiskAttachQuarantineReason::WrongResidentAttachment)
            .unwrap();
        assert!(matches!(
            quarantined.state(),
            ProjectDiskAttachmentState::Quarantined {
                reason: ProjectDiskAttachQuarantineReason::WrongResidentAttachment,
                ..
            }
        ));
    }

    #[test]
    fn stale_p1_revision_rejects_attach_intent() {
        let current = detached();
        let plan = attach_plan(&current);
        let changed = current.require_revalidation().unwrap();
        let record = record();
        assert!(
            record
                .plan_attach(
                    &changed,
                    &plan,
                    &ProjectDiskAttachPreconditionProof::for_test(&record, &current, &plan),
                )
                .is_err()
        );
    }
}
