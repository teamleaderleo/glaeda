//! Pure crash/replay state for one exact project-disk filesystem format transaction.
//!
//! The transaction carries one complete immutable [`ProjectDiskFilesystemFormatPlan`] selected
//! before any destructive formatter call. This module performs no persistence, Lima operation,
//! formatter execution, device probing, mount mutation, cleanup, or proof minting.

use std::fmt;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use super::ProjectDiskFilesystemBinding;
use super::format_plan::{
    ProjectDiskFilesystemFormatPlan, ProjectDiskFilesystemUuid, ProjectDiskFormatterArchitecture,
};
use crate::artifact::Sha256Digest;
use crate::project_catalog::ProjectIdentity;
use crate::project_disk_lease::{ProjectDiskGeneration, ProjectDiskId};

pub const PROJECT_DISK_FORMAT_STATE_SCHEMA_VERSION: u8 = 1;
const FORMAT_OUTCOME_DOMAIN: &[u8] = b"smolrunner-project-disk-format-outcome-v1\0";
const SHA256_PREFIX: &str = "sha256:";
const HEX: &[u8; 16] = b"0123456789abcdef";

macro_rules! positive_generation_type {
    ($name:ident, $code:literal, $message:literal) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(u64);

        impl $name {
            pub fn new(value: u64) -> Result<Self, ProjectDiskFormatStateError> {
                if value == 0 {
                    return Err(error(
                        ProjectDiskFormatStateErrorKind::InvalidGeneration,
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
    ProjectDiskMaterializationRevision,
    "project_disk_materialization_revision_invalid",
    "project disk materialization revision must be greater than zero"
);
positive_generation_type!(
    ProjectDiskFormatTransactionGeneration,
    "project_disk_format_transaction_generation_invalid",
    "project disk format transaction generation must be greater than zero"
);
positive_generation_type!(
    ProjectDiskAttachmentTransactionGeneration,
    "project_disk_attachment_transaction_generation_invalid",
    "project disk attachment transaction generation must be greater than zero"
);

impl ProjectDiskMaterializationRevision {
    fn next(self) -> Result<Self, ProjectDiskFormatStateError> {
        Self::new(self.0.checked_add(1).ok_or_else(generation_exhausted)?)
    }
}

impl ProjectDiskFormatTransactionGeneration {
    fn next(self) -> Result<Self, ProjectDiskFormatStateError> {
        Self::new(self.0.checked_add(1).ok_or_else(generation_exhausted)?)
    }
}

/// Sealed view of the exact P3 `CreatedUnformatted` ownership binding consumed by P4.
///
/// Production construction remains crate-private until the final P3/P4 adapter is composed. The
/// digests represent the exact P2 physical and backing identities; Lima names never appear here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiskRawOwnershipBinding {
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    physical_identity_digest: Sha256Digest,
    backing_identity_digest: Sha256Digest,
    backing_logical_bytes: u64,
}

impl ProjectDiskRawOwnershipBinding {
    #[allow(dead_code)]
    pub(crate) fn from_verified(
        project: ProjectIdentity,
        disk_id: ProjectDiskId,
        disk_generation: ProjectDiskGeneration,
        physical_identity_digest: Sha256Digest,
        backing_identity_digest: Sha256Digest,
        backing_logical_bytes: u64,
    ) -> Result<Self, ProjectDiskFormatStateError> {
        if backing_logical_bytes == 0
            || is_zero_digest(&physical_identity_digest)
            || is_zero_digest(&backing_identity_digest)
        {
            return Err(invalid_input());
        }
        Ok(Self {
            project,
            disk_id,
            disk_generation,
            physical_identity_digest,
            backing_identity_digest,
            backing_logical_bytes,
        })
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
    pub const fn physical_identity_digest(&self) -> &Sha256Digest {
        &self.physical_identity_digest
    }

    #[must_use]
    pub const fn backing_identity_digest(&self) -> &Sha256Digest {
        &self.backing_identity_digest
    }

    #[must_use]
    pub const fn backing_logical_bytes(&self) -> u64 {
        self.backing_logical_bytes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDiskFormatStage {
    IntentDurable,
    FormatterCarrierAttached,
    GuestBlockDeviceBound,
    ReadyToMkfs,
    MkfsIssued,
    FilesystemObserved,
    FormatterDetachPending,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiskFormatCheckpoint {
    transaction_generation: ProjectDiskFormatTransactionGeneration,
    starting_revision: ProjectDiskMaterializationRevision,
    plan: ProjectDiskFilesystemFormatPlan,
    stage: ProjectDiskFormatStage,
    #[serde(skip_serializing_if = "Option::is_none")]
    post_format_observation_digest: Option<Sha256Digest>,
}

impl ProjectDiskFormatCheckpoint {
    #[must_use]
    pub const fn transaction_generation(&self) -> ProjectDiskFormatTransactionGeneration {
        self.transaction_generation
    }

    #[must_use]
    pub const fn starting_revision(&self) -> ProjectDiskMaterializationRevision {
        self.starting_revision
    }

    #[must_use]
    pub const fn plan(&self) -> &ProjectDiskFilesystemFormatPlan {
        &self.plan
    }

    #[must_use]
    pub const fn stage(&self) -> ProjectDiskFormatStage {
        self.stage
    }

    #[must_use]
    pub const fn post_format_observation_digest(&self) -> Option<&Sha256Digest> {
        self.post_format_observation_digest.as_ref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiskFormatReceipt {
    transaction_generation: ProjectDiskFormatTransactionGeneration,
    filesystem: ProjectDiskFilesystemBinding,
    format_plan_digest: Sha256Digest,
    post_format_observation_digest: Sha256Digest,
    physical_identity_digest: Sha256Digest,
    backing_identity_digest: Sha256Digest,
    outcome_digest: Sha256Digest,
}

impl ProjectDiskFormatReceipt {
    #[must_use]
    pub const fn transaction_generation(&self) -> ProjectDiskFormatTransactionGeneration {
        self.transaction_generation
    }

    #[must_use]
    pub const fn filesystem(&self) -> &ProjectDiskFilesystemBinding {
        &self.filesystem
    }

    #[must_use]
    pub const fn format_plan_digest(&self) -> &Sha256Digest {
        &self.format_plan_digest
    }

    #[must_use]
    pub const fn post_format_observation_digest(&self) -> &Sha256Digest {
        &self.post_format_observation_digest
    }

    #[must_use]
    pub const fn outcome_digest(&self) -> &Sha256Digest {
        &self.outcome_digest
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProjectDiskFilesystemMaterializationState {
    Raw,
    FormatPending {
        checkpoint: ProjectDiskFormatCheckpoint,
    },
    FormatRecoveryRequired {
        checkpoint: ProjectDiskFormatCheckpoint,
    },
    FormattedDetached {
        receipt: ProjectDiskFormatReceipt,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "mutation", rename_all = "snake_case")]
pub enum ProjectDiskActiveMaterializationMutation {
    None,
    Format {
        transaction_generation: ProjectDiskFormatTransactionGeneration,
    },
    Attach {
        transaction_generation: ProjectDiskAttachmentTransactionGeneration,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiskMaterializationRecord {
    schema_version: u8,
    ownership: ProjectDiskRawOwnershipBinding,
    revision: ProjectDiskMaterializationRevision,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_format_transaction_generation: Option<ProjectDiskFormatTransactionGeneration>,
    filesystem: ProjectDiskFilesystemMaterializationState,
    active_mutation: ProjectDiskActiveMaterializationMutation,
}

impl ProjectDiskMaterializationRecord {
    #[must_use]
    pub fn new_raw(ownership: ProjectDiskRawOwnershipBinding) -> Self {
        Self {
            schema_version: PROJECT_DISK_FORMAT_STATE_SCHEMA_VERSION,
            ownership,
            revision: ProjectDiskMaterializationRevision(1),
            last_format_transaction_generation: None,
            filesystem: ProjectDiskFilesystemMaterializationState::Raw,
            active_mutation: ProjectDiskActiveMaterializationMutation::None,
        }
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn ownership(&self) -> &ProjectDiskRawOwnershipBinding {
        &self.ownership
    }

    #[must_use]
    pub const fn revision(&self) -> ProjectDiskMaterializationRevision {
        self.revision
    }

    #[must_use]
    pub const fn filesystem(&self) -> &ProjectDiskFilesystemMaterializationState {
        &self.filesystem
    }

    #[must_use]
    pub const fn active_mutation(&self) -> ProjectDiskActiveMaterializationMutation {
        self.active_mutation
    }

    /// Plan one format transaction only from the exact raw ownership binding and fresh sealed
    /// pre-format evidence.
    pub fn plan_format(
        &self,
        plan: &ProjectDiskFilesystemFormatPlan,
        precondition: &ProjectDiskFormatPreconditionProof,
    ) -> Result<ProjectDiskFormatIntent, ProjectDiskFormatStateError> {
        if !matches!(self.filesystem, ProjectDiskFilesystemMaterializationState::Raw)
            || self.active_mutation != ProjectDiskActiveMaterializationMutation::None
        {
            return Err(invalid_state(
                "project_disk_format_requires_raw_idle",
                "project disk format requires raw state with no active mutation",
            ));
        }
        self.confirm_plan_matches_ownership(plan)?;
        precondition.confirm(self, plan)?;
        Ok(ProjectDiskFormatIntent {
            project: self.ownership.project.clone(),
            disk_id: self.ownership.disk_id.clone(),
            disk_generation: self.ownership.disk_generation,
            expected_revision: self.revision,
            transaction_generation: self.next_format_transaction_generation()?,
            plan: plan.clone(),
        })
    }

    /// Accept the durable format intent/checkpoint before any formatter carrier attachment.
    pub fn record_format_intent(
        &self,
        intent: &ProjectDiskFormatIntent,
    ) -> Result<Self, ProjectDiskFormatStateError> {
        if !matches!(self.filesystem, ProjectDiskFilesystemMaterializationState::Raw)
            || self.active_mutation != ProjectDiskActiveMaterializationMutation::None
        {
            return Err(invalid_state(
                "project_disk_format_requires_raw_idle",
                "project disk format requires raw state with no active mutation",
            ));
        }
        intent.confirm(self)?;
        let checkpoint = ProjectDiskFormatCheckpoint {
            transaction_generation: intent.transaction_generation,
            starting_revision: self.revision,
            plan: intent.plan.clone(),
            stage: ProjectDiskFormatStage::IntentDurable,
            post_format_observation_digest: None,
        };
        self.successor(
            ProjectDiskFilesystemMaterializationState::FormatPending { checkpoint },
            ProjectDiskActiveMaterializationMutation::Format {
                transaction_generation: intent.transaction_generation,
            },
            Some(intent.transaction_generation),
        )
    }

    pub fn record_formatter_carrier_attached(
        &self,
        proof: &ProjectDiskFormatPhaseProof,
    ) -> Result<Self, ProjectDiskFormatStateError> {
        self.advance_pending_stage(
            ProjectDiskFormatStage::IntentDurable,
            ProjectDiskFormatStage::FormatterCarrierAttached,
            proof,
        )
    }

    pub fn record_guest_block_device_bound(
        &self,
        proof: &ProjectDiskFormatPhaseProof,
    ) -> Result<Self, ProjectDiskFormatStateError> {
        self.advance_pending_stage(
            ProjectDiskFormatStage::FormatterCarrierAttached,
            ProjectDiskFormatStage::GuestBlockDeviceBound,
            proof,
        )
    }

    pub fn record_ready_to_mkfs(
        &self,
        proof: &ProjectDiskFormatPhaseProof,
    ) -> Result<Self, ProjectDiskFormatStateError> {
        self.advance_pending_stage(
            ProjectDiskFormatStage::GuestBlockDeviceBound,
            ProjectDiskFormatStage::ReadyToMkfs,
            proof,
        )
    }

    /// Cross the destructive no-replay boundary after the complete last-safe proof has succeeded.
    pub fn record_mkfs_issued(
        &self,
        proof: &ProjectDiskFormatPhaseProof,
    ) -> Result<Self, ProjectDiskFormatStateError> {
        self.advance_pending_stage(
            ProjectDiskFormatStage::ReadyToMkfs,
            ProjectDiskFormatStage::MkfsIssued,
            proof,
        )
    }

    /// Enter explicit recovery debt once the formatter may have been issued and its outcome is not
    /// durably known. No method in `format_recovery_required` issues or plans another formatter call.
    pub fn require_format_recovery(&self) -> Result<Self, ProjectDiskFormatStateError> {
        let ProjectDiskFilesystemMaterializationState::FormatPending { checkpoint } = &self.filesystem
        else {
            return Err(format_pending_required());
        };
        if checkpoint.stage != ProjectDiskFormatStage::MkfsIssued {
            return Err(invalid_state(
                "project_disk_format_recovery_requires_possible_mkfs",
                "format recovery requires a transaction where mkfs may have been issued",
            ));
        }
        self.require_active_format(checkpoint.transaction_generation)?;
        self.successor(
            ProjectDiskFilesystemMaterializationState::FormatRecoveryRequired {
                checkpoint: checkpoint.clone(),
            },
            self.active_mutation,
            self.last_format_transaction_generation,
        )
    }

    /// Accept exact post-format evidence for the same transaction, including during response-loss
    /// recovery. This continues the original transaction; it never allocates or replays `mkfs`.
    pub fn record_exact_filesystem_observed(
        &self,
        proof: &ProjectDiskPostFormatProof,
    ) -> Result<Self, ProjectDiskFormatStateError> {
        let checkpoint = match &self.filesystem {
            ProjectDiskFilesystemMaterializationState::FormatPending { checkpoint }
                if checkpoint.stage == ProjectDiskFormatStage::MkfsIssued => checkpoint,
            ProjectDiskFilesystemMaterializationState::FormatRecoveryRequired { checkpoint }
                if checkpoint.stage == ProjectDiskFormatStage::MkfsIssued => checkpoint,
            _ => {
                return Err(invalid_state(
                    "project_disk_post_format_requires_mkfs_issued",
                    "post-format observation requires the exact mkfs-issued transaction",
                ));
            }
        };
        self.require_active_format(checkpoint.transaction_generation)?;
        proof.confirm(self, checkpoint)?;
        let mut observed = checkpoint.clone();
        observed.stage = ProjectDiskFormatStage::FilesystemObserved;
        observed.post_format_observation_digest = Some(proof.observation_digest.clone());
        self.successor(
            ProjectDiskFilesystemMaterializationState::FormatPending {
                checkpoint: observed,
            },
            self.active_mutation,
            self.last_format_transaction_generation,
        )
    }

    /// A mismatched/partial filesystem after possible mkfs remains recovery debt. Overwriting it
    /// requires a separately designed explicit recovery-format intent; this model exposes no replay.
    pub fn assess_recovery_mismatch(
        &self,
    ) -> Result<ProjectDiskFormatRecoveryAssessment, ProjectDiskFormatStateError> {
        let ProjectDiskFilesystemMaterializationState::FormatRecoveryRequired { checkpoint } =
            &self.filesystem
        else {
            return Err(invalid_state(
                "project_disk_format_recovery_required",
                "project disk format is not awaiting recovery",
            ));
        };
        self.require_active_format(checkpoint.transaction_generation)?;
        Ok(ProjectDiskFormatRecoveryAssessment::ExplicitRecoveryFormatRequired {
            transaction_generation: checkpoint.transaction_generation,
        })
    }

    pub fn record_formatter_detach_pending(
        &self,
        proof: &ProjectDiskFormatPhaseProof,
    ) -> Result<Self, ProjectDiskFormatStateError> {
        self.advance_pending_stage(
            ProjectDiskFormatStage::FilesystemObserved,
            ProjectDiskFormatStage::FormatterDetachPending,
            proof,
        )
    }

    /// Commit `formatted_detached` only after fresh P2 evidence proves the exact same physical and
    /// backing identities are detached/unused after formatter shutdown.
    pub fn record_formatted_detached(
        &self,
        detached: &ProjectDiskFormatterDetachedProof,
    ) -> Result<Self, ProjectDiskFormatStateError> {
        let ProjectDiskFilesystemMaterializationState::FormatPending { checkpoint } = &self.filesystem
        else {
            return Err(format_pending_required());
        };
        if checkpoint.stage != ProjectDiskFormatStage::FormatterDetachPending {
            return Err(invalid_state(
                "project_disk_format_detach_proof_required",
                "formatted-detached publication requires formatter detach pending state",
            ));
        }
        self.require_active_format(checkpoint.transaction_generation)?;
        detached.confirm(self, checkpoint)?;
        let Some(observation_digest) = checkpoint.post_format_observation_digest.clone() else {
            return Err(evidence_mismatch());
        };
        let outcome_digest = derive_outcome_digest(
            checkpoint.transaction_generation,
            checkpoint.plan.plan_digest(),
            &observation_digest,
            self.ownership.physical_identity_digest(),
            self.ownership.backing_identity_digest(),
        )?;
        let receipt = ProjectDiskFormatReceipt {
            transaction_generation: checkpoint.transaction_generation,
            filesystem: checkpoint.plan.filesystem().clone(),
            format_plan_digest: checkpoint.plan.plan_digest().clone(),
            post_format_observation_digest: observation_digest,
            physical_identity_digest: self.ownership.physical_identity_digest.clone(),
            backing_identity_digest: self.ownership.backing_identity_digest.clone(),
            outcome_digest,
        };
        self.successor(
            ProjectDiskFilesystemMaterializationState::FormattedDetached { receipt },
            ProjectDiskActiveMaterializationMutation::None,
            self.last_format_transaction_generation,
        )
    }

    fn advance_pending_stage(
        &self,
        expected_stage: ProjectDiskFormatStage,
        next_stage: ProjectDiskFormatStage,
        proof: &ProjectDiskFormatPhaseProof,
    ) -> Result<Self, ProjectDiskFormatStateError> {
        let ProjectDiskFilesystemMaterializationState::FormatPending { checkpoint } = &self.filesystem
        else {
            return Err(format_pending_required());
        };
        if checkpoint.stage != expected_stage {
            return Err(invalid_state(
                "project_disk_format_stage_mismatch",
                "project disk format checkpoint is at another stage",
            ));
        }
        self.require_active_format(checkpoint.transaction_generation)?;
        proof.confirm(self, checkpoint, expected_stage)?;
        let mut next = checkpoint.clone();
        next.stage = next_stage;
        self.successor(
            ProjectDiskFilesystemMaterializationState::FormatPending { checkpoint: next },
            self.active_mutation,
            self.last_format_transaction_generation,
        )
    }

    fn confirm_plan_matches_ownership(
        &self,
        plan: &ProjectDiskFilesystemFormatPlan,
    ) -> Result<(), ProjectDiskFormatStateError> {
        if !plan.filesystem().matches_project_disk_identity(
            self.ownership.project(),
            self.ownership.disk_id(),
            self.ownership.disk_generation(),
        ) || plan.expected_logical_bytes() != self.ownership.backing_logical_bytes
            || !plan.whole_device()
        {
            return Err(evidence_mismatch());
        }
        Ok(())
    }

    fn require_active_format(
        &self,
        transaction_generation: ProjectDiskFormatTransactionGeneration,
    ) -> Result<(), ProjectDiskFormatStateError> {
        if self.active_mutation
            != ProjectDiskActiveMaterializationMutation::Format {
                transaction_generation,
            }
        {
            return Err(invalid_state(
                "project_disk_format_transaction_inactive",
                "project disk format transaction is not the active mutation",
            ));
        }
        Ok(())
    }

    fn next_format_transaction_generation(
        &self,
    ) -> Result<ProjectDiskFormatTransactionGeneration, ProjectDiskFormatStateError> {
        match self.last_format_transaction_generation {
            Some(generation) => generation.next(),
            None => ProjectDiskFormatTransactionGeneration::new(1),
        }
    }

    fn successor(
        &self,
        filesystem: ProjectDiskFilesystemMaterializationState,
        active_mutation: ProjectDiskActiveMaterializationMutation,
        last_format_transaction_generation: Option<ProjectDiskFormatTransactionGeneration>,
    ) -> Result<Self, ProjectDiskFormatStateError> {
        Ok(Self {
            schema_version: self.schema_version,
            ownership: self.ownership.clone(),
            revision: self.revision.next()?,
            last_format_transaction_generation,
            filesystem,
            active_mutation,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDiskFormatIntent {
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    expected_revision: ProjectDiskMaterializationRevision,
    transaction_generation: ProjectDiskFormatTransactionGeneration,
    plan: ProjectDiskFilesystemFormatPlan,
}

impl ProjectDiskFormatIntent {
    #[must_use]
    pub const fn transaction_generation(&self) -> ProjectDiskFormatTransactionGeneration {
        self.transaction_generation
    }

    #[must_use]
    pub const fn plan(&self) -> &ProjectDiskFilesystemFormatPlan {
        &self.plan
    }

    fn confirm(&self, record: &ProjectDiskMaterializationRecord) -> Result<(), ProjectDiskFormatStateError> {
        if self.project != record.ownership.project
            || self.disk_id != record.ownership.disk_id
            || self.disk_generation != record.ownership.disk_generation
            || self.expected_revision != record.revision
            || self.transaction_generation != record.next_format_transaction_generation()?
        {
            return Err(plan_mismatch());
        }
        record.confirm_plan_matches_ownership(&self.plan)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDiskFormatRecoveryAssessment {
    ExplicitRecoveryFormatRequired {
        transaction_generation: ProjectDiskFormatTransactionGeneration,
    },
}

/// Sealed fresh P3/P2 pre-format evidence for the exact raw detached disk and plan.
pub struct ProjectDiskFormatPreconditionProof {
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    materialization_revision: ProjectDiskMaterializationRevision,
    physical_identity_digest: Sha256Digest,
    backing_identity_digest: Sha256Digest,
    format_plan_digest: Sha256Digest,
}

impl ProjectDiskFormatPreconditionProof {
    fn confirm(
        &self,
        record: &ProjectDiskMaterializationRecord,
        plan: &ProjectDiskFilesystemFormatPlan,
    ) -> Result<(), ProjectDiskFormatStateError> {
        if self.project != record.ownership.project
            || self.disk_id != record.ownership.disk_id
            || self.disk_generation != record.ownership.disk_generation
            || self.materialization_revision != record.revision
            || self.physical_identity_digest != record.ownership.physical_identity_digest
            || self.backing_identity_digest != record.ownership.backing_identity_digest
            || self.format_plan_digest != *plan.plan_digest()
        {
            return Err(evidence_mismatch());
        }
        Ok(())
    }

    #[cfg(test)]
    fn for_test(
        record: &ProjectDiskMaterializationRecord,
        plan: &ProjectDiskFilesystemFormatPlan,
    ) -> Self {
        Self {
            project: record.ownership.project.clone(),
            disk_id: record.ownership.disk_id.clone(),
            disk_generation: record.ownership.disk_generation,
            materialization_revision: record.revision,
            physical_identity_digest: record.ownership.physical_identity_digest.clone(),
            backing_identity_digest: record.ownership.backing_identity_digest.clone(),
            format_plan_digest: plan.plan_digest().clone(),
        }
    }
}

/// Sealed fresh evidence for one non-destructive format phase transition.
pub struct ProjectDiskFormatPhaseProof {
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    transaction_generation: ProjectDiskFormatTransactionGeneration,
    format_plan_digest: Sha256Digest,
    expected_stage: ProjectDiskFormatStage,
}

impl ProjectDiskFormatPhaseProof {
    fn confirm(
        &self,
        record: &ProjectDiskMaterializationRecord,
        checkpoint: &ProjectDiskFormatCheckpoint,
        expected_stage: ProjectDiskFormatStage,
    ) -> Result<(), ProjectDiskFormatStateError> {
        if self.project != record.ownership.project
            || self.disk_id != record.ownership.disk_id
            || self.disk_generation != record.ownership.disk_generation
            || self.transaction_generation != checkpoint.transaction_generation
            || self.format_plan_digest != *checkpoint.plan.plan_digest()
            || self.expected_stage != expected_stage
        {
            return Err(evidence_mismatch());
        }
        Ok(())
    }

    #[cfg(test)]
    fn for_test(
        record: &ProjectDiskMaterializationRecord,
        checkpoint: &ProjectDiskFormatCheckpoint,
        expected_stage: ProjectDiskFormatStage,
    ) -> Self {
        Self {
            project: record.ownership.project.clone(),
            disk_id: record.ownership.disk_id.clone(),
            disk_generation: record.ownership.disk_generation,
            transaction_generation: checkpoint.transaction_generation,
            format_plan_digest: checkpoint.plan.plan_digest().clone(),
            expected_stage,
        }
    }
}

/// Sealed read-only observation of the exact requested filesystem after possible `mkfs` completion.
pub struct ProjectDiskPostFormatProof {
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    transaction_generation: ProjectDiskFormatTransactionGeneration,
    format_plan_digest: Sha256Digest,
    filesystem_uuid: ProjectDiskFilesystemUuid,
    feature_policy_digest: Sha256Digest,
    logical_bytes: u64,
    whole_device: bool,
    no_partition_layer: bool,
    durability_barrier_complete: bool,
    formatter_architecture: ProjectDiskFormatterArchitecture,
    observation_digest: Sha256Digest,
}

impl ProjectDiskPostFormatProof {
    fn confirm(
        &self,
        record: &ProjectDiskMaterializationRecord,
        checkpoint: &ProjectDiskFormatCheckpoint,
    ) -> Result<(), ProjectDiskFormatStateError> {
        let plan = &checkpoint.plan;
        if self.project != record.ownership.project
            || self.disk_id != record.ownership.disk_id
            || self.disk_generation != record.ownership.disk_generation
            || self.transaction_generation != checkpoint.transaction_generation
            || self.format_plan_digest != *plan.plan_digest()
            || self.filesystem_uuid != *plan.filesystem_uuid()
            || self.feature_policy_digest != *plan.feature_policy_digest()
            || self.logical_bytes != plan.expected_logical_bytes()
            || !self.whole_device
            || !self.no_partition_layer
            || !self.durability_barrier_complete
            || self.formatter_architecture != plan.formatter().architecture()
            || is_zero_digest(&self.observation_digest)
        {
            return Err(evidence_mismatch());
        }
        Ok(())
    }

    #[cfg(test)]
    fn for_test(
        record: &ProjectDiskMaterializationRecord,
        checkpoint: &ProjectDiskFormatCheckpoint,
        observation_digest: Sha256Digest,
    ) -> Self {
        let plan = &checkpoint.plan;
        Self {
            project: record.ownership.project.clone(),
            disk_id: record.ownership.disk_id.clone(),
            disk_generation: record.ownership.disk_generation,
            transaction_generation: checkpoint.transaction_generation,
            format_plan_digest: plan.plan_digest().clone(),
            filesystem_uuid: plan.filesystem_uuid().clone(),
            feature_policy_digest: plan.feature_policy_digest().clone(),
            logical_bytes: plan.expected_logical_bytes(),
            whole_device: true,
            no_partition_layer: true,
            durability_barrier_complete: true,
            formatter_architecture: plan.formatter().architecture(),
            observation_digest,
        }
    }
}

/// Sealed fresh P2 proof that formatter cleanup returned the same exact disk/backing to
/// detached/unused state.
pub struct ProjectDiskFormatterDetachedProof {
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    transaction_generation: ProjectDiskFormatTransactionGeneration,
    format_plan_digest: Sha256Digest,
    physical_identity_digest: Sha256Digest,
    backing_identity_digest: Sha256Digest,
    detached_unused: bool,
}

impl ProjectDiskFormatterDetachedProof {
    fn confirm(
        &self,
        record: &ProjectDiskMaterializationRecord,
        checkpoint: &ProjectDiskFormatCheckpoint,
    ) -> Result<(), ProjectDiskFormatStateError> {
        if self.project != record.ownership.project
            || self.disk_id != record.ownership.disk_id
            || self.disk_generation != record.ownership.disk_generation
            || self.transaction_generation != checkpoint.transaction_generation
            || self.format_plan_digest != *checkpoint.plan.plan_digest()
            || self.physical_identity_digest != record.ownership.physical_identity_digest
            || self.backing_identity_digest != record.ownership.backing_identity_digest
            || !self.detached_unused
        {
            return Err(evidence_mismatch());
        }
        Ok(())
    }

    #[cfg(test)]
    fn for_test(
        record: &ProjectDiskMaterializationRecord,
        checkpoint: &ProjectDiskFormatCheckpoint,
    ) -> Self {
        Self {
            project: record.ownership.project.clone(),
            disk_id: record.ownership.disk_id.clone(),
            disk_generation: record.ownership.disk_generation,
            transaction_generation: checkpoint.transaction_generation,
            format_plan_digest: checkpoint.plan.plan_digest().clone(),
            physical_identity_digest: record.ownership.physical_identity_digest.clone(),
            backing_identity_digest: record.ownership.backing_identity_digest.clone(),
            detached_unused: true,
        }
    }
}

fn derive_outcome_digest(
    transaction_generation: ProjectDiskFormatTransactionGeneration,
    format_plan_digest: &Sha256Digest,
    observation_digest: &Sha256Digest,
    physical_identity_digest: &Sha256Digest,
    backing_identity_digest: &Sha256Digest,
) -> Result<Sha256Digest, ProjectDiskFormatStateError> {
    let transaction = transaction_generation.get().to_be_bytes();
    let fields = [
        transaction.as_slice(),
        format_plan_digest.as_str().as_bytes(),
        observation_digest.as_str().as_bytes(),
        physical_identity_digest.as_str().as_bytes(),
        backing_identity_digest.as_str().as_bytes(),
    ];
    let mut hasher = Sha256::new();
    hasher.update(FORMAT_OUTCOME_DOMAIN);
    for field in fields {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field);
    }
    digest_to_sha256(&hasher.finalize())
}

fn digest_to_sha256(bytes: &[u8]) -> Result<Sha256Digest, ProjectDiskFormatStateError> {
    let mut value = String::with_capacity(SHA256_PREFIX.len() + bytes.len() * 2);
    value.push_str(SHA256_PREFIX);
    for byte in bytes {
        value.push(char::from(HEX[usize::from(byte >> 4)]));
        value.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Sha256Digest::parse(&value).map_err(|_| invalid_input())
}

fn is_zero_digest(digest: &Sha256Digest) -> bool {
    digest.as_str() == "sha256:0000000000000000000000000000000000000000000000000000000000000000"
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDiskFormatStateErrorKind {
    InvalidGeneration,
    InvalidInput,
    InvalidState,
    EvidenceMismatch,
    PlanMismatch,
    GenerationExhausted,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProjectDiskFormatStateError {
    kind: ProjectDiskFormatStateErrorKind,
    code: &'static str,
    message: &'static str,
}

impl ProjectDiskFormatStateError {
    #[must_use]
    pub const fn kind(self) -> ProjectDiskFormatStateErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for ProjectDiskFormatStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectDiskFormatStateError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for ProjectDiskFormatStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ProjectDiskFormatStateError {}

const fn error(
    kind: ProjectDiskFormatStateErrorKind,
    code: &'static str,
    message: &'static str,
) -> ProjectDiskFormatStateError {
    ProjectDiskFormatStateError { kind, code, message }
}

const fn invalid_input() -> ProjectDiskFormatStateError {
    error(
        ProjectDiskFormatStateErrorKind::InvalidInput,
        "project_disk_format_state_invalid_input",
        "project disk format-state input is invalid",
    )
}

const fn invalid_state(code: &'static str, message: &'static str) -> ProjectDiskFormatStateError {
    error(ProjectDiskFormatStateErrorKind::InvalidState, code, message)
}

const fn evidence_mismatch() -> ProjectDiskFormatStateError {
    error(
        ProjectDiskFormatStateErrorKind::EvidenceMismatch,
        "project_disk_format_evidence_mismatch",
        "project disk format evidence does not match the active transaction",
    )
}

const fn plan_mismatch() -> ProjectDiskFormatStateError {
    error(
        ProjectDiskFormatStateErrorKind::PlanMismatch,
        "project_disk_format_plan_mismatch",
        "project disk format intent does not match the current materialization record",
    )
}

const fn format_pending_required() -> ProjectDiskFormatStateError {
    invalid_state(
        "project_disk_format_pending_required",
        "project disk format operation requires a pending format checkpoint",
    )
}

const fn generation_exhausted() -> ProjectDiskFormatStateError {
    error(
        ProjectDiskFormatStateErrorKind::GenerationExhausted,
        "project_disk_format_generation_exhausted",
        "project disk format generation is exhausted",
    )
}

#[cfg(test)]
mod tests {
    use super::{
        ProjectDiskActiveMaterializationMutation, ProjectDiskFormatPhaseProof,
        ProjectDiskFormatPreconditionProof, ProjectDiskFormatRecoveryAssessment, ProjectDiskFormatStage,
        ProjectDiskFormatterDetachedProof, ProjectDiskMaterializationRecord,
        ProjectDiskPostFormatProof, ProjectDiskRawOwnershipBinding,
        ProjectDiskFilesystemMaterializationState,
    };
    use crate::artifact::Sha256Digest;
    use crate::project_catalog::ProjectIdentity;
    use crate::project_disk_filesystem::format_plan::{
        ProjectDiskFilesystemFormatPlan, ProjectDiskFilesystemUuid,
        ProjectDiskFormatDurabilityPolicyGeneration, ProjectDiskFormatterArchitecture,
        ProjectDiskFormatterBinaryBinding, ProjectDiskFormatterBinaryGeneration,
        ProjectDiskFormatterGuestGeneration, ProjectDiskFormatterVersion,
    };
    use crate::project_disk_filesystem::{
        ProjectDiskFilesystemBinding, ProjectDiskFilesystemFormatProfileGeneration,
        ProjectDiskFilesystemGeneration, ProjectDiskFilesystemKind,
    };
    use crate::project_disk_lease::{ProjectDiskGeneration, ProjectDiskId};

    fn digest(byte: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    fn raw() -> ProjectDiskMaterializationRecord {
        ProjectDiskMaterializationRecord::new_raw(
            ProjectDiskRawOwnershipBinding::from_verified(
                ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
                ProjectDiskId::parse("disk-a").unwrap(),
                ProjectDiskGeneration::new(3).unwrap(),
                digest('a'),
                digest('b'),
                1_073_741_824,
            )
            .unwrap(),
        )
    }

    fn plan() -> ProjectDiskFilesystemFormatPlan {
        let filesystem = ProjectDiskFilesystemBinding::new_for_project_disk(
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            ProjectDiskId::parse("disk-a").unwrap(),
            ProjectDiskGeneration::new(3).unwrap(),
            ProjectDiskFilesystemGeneration::new(7).unwrap(),
            ProjectDiskFilesystemFormatProfileGeneration::new(2).unwrap(),
            ProjectDiskFilesystemKind::Xfs,
        );
        ProjectDiskFilesystemFormatPlan::new(
            filesystem,
            ProjectDiskFilesystemUuid::parse("01234567-89ab-cdef-0123-456789abcdef").unwrap(),
            1_073_741_824,
            digest('c'),
            ProjectDiskFormatterBinaryBinding::new(
                ProjectDiskFormatterBinaryGeneration::new(4).unwrap(),
                digest('d'),
                ProjectDiskFormatterVersion::parse("6.10.1").unwrap(),
                ProjectDiskFormatterArchitecture::LinuxAarch64,
            ),
            digest('e'),
            ProjectDiskFormatterGuestGeneration::new(5).unwrap(),
            ProjectDiskFormatDurabilityPolicyGeneration::new(3).unwrap(),
        )
        .unwrap()
    }

    fn checkpoint(record: &ProjectDiskMaterializationRecord) -> &super::ProjectDiskFormatCheckpoint {
        match record.filesystem() {
            ProjectDiskFilesystemMaterializationState::FormatPending { checkpoint }
            | ProjectDiskFilesystemMaterializationState::FormatRecoveryRequired { checkpoint } => checkpoint,
            _ => panic!("expected format checkpoint"),
        }
    }

    fn begin() -> ProjectDiskMaterializationRecord {
        let raw = raw();
        let plan = plan();
        let intent = raw
            .plan_format(&plan, &ProjectDiskFormatPreconditionProof::for_test(&raw, &plan))
            .unwrap();
        raw.record_format_intent(&intent).unwrap()
    }

    fn advance(
        record: ProjectDiskMaterializationRecord,
        expected: ProjectDiskFormatStage,
        next: fn(
            &ProjectDiskMaterializationRecord,
            &ProjectDiskFormatPhaseProof,
        ) -> Result<ProjectDiskMaterializationRecord, super::ProjectDiskFormatStateError>,
    ) -> ProjectDiskMaterializationRecord {
        let proof = ProjectDiskFormatPhaseProof::for_test(&record, checkpoint(&record), expected);
        next(&record, &proof).unwrap()
    }

    fn mkfs_issued() -> ProjectDiskMaterializationRecord {
        let carrier = advance(
            begin(),
            ProjectDiskFormatStage::IntentDurable,
            ProjectDiskMaterializationRecord::record_formatter_carrier_attached,
        );
        let device = advance(
            carrier,
            ProjectDiskFormatStage::FormatterCarrierAttached,
            ProjectDiskMaterializationRecord::record_guest_block_device_bound,
        );
        let ready = advance(
            device,
            ProjectDiskFormatStage::GuestBlockDeviceBound,
            ProjectDiskMaterializationRecord::record_ready_to_mkfs,
        );
        advance(
            ready,
            ProjectDiskFormatStage::ReadyToMkfs,
            ProjectDiskMaterializationRecord::record_mkfs_issued,
        )
    }

    #[test]
    fn format_intent_requires_exact_raw_ownership_and_plan() {
        let raw = raw();
        let plan = plan();
        let intent = raw
            .plan_format(&plan, &ProjectDiskFormatPreconditionProof::for_test(&raw, &plan))
            .unwrap();
        let pending = raw.record_format_intent(&intent).unwrap();
        assert_eq!(intent.transaction_generation().get(), 1);
        assert!(matches!(
            pending.active_mutation(),
            ProjectDiskActiveMaterializationMutation::Format { .. }
        ));
        assert_eq!(checkpoint(&pending).stage(), ProjectDiskFormatStage::IntentDurable);
    }

    #[test]
    fn pre_mkfs_progress_keeps_one_transaction_generation() {
        let started = begin();
        let tx = checkpoint(&started).transaction_generation();
        let issued = mkfs_issued();
        assert_eq!(checkpoint(&issued).transaction_generation(), tx);
        assert_eq!(checkpoint(&issued).stage(), ProjectDiskFormatStage::MkfsIssued);
    }

    #[test]
    fn response_loss_after_possible_mkfs_enters_no_replay_recovery() {
        let issued = mkfs_issued();
        let recovery = issued.require_format_recovery().unwrap();
        assert!(matches!(
            recovery.filesystem(),
            ProjectDiskFilesystemMaterializationState::FormatRecoveryRequired { .. }
        ));
        assert!(recovery
            .plan_format(
                &plan(),
                &ProjectDiskFormatPreconditionProof::for_test(&recovery, &plan()),
            )
            .is_err());
        assert_eq!(
            recovery.assess_recovery_mismatch().unwrap(),
            ProjectDiskFormatRecoveryAssessment::ExplicitRecoveryFormatRequired {
                transaction_generation: checkpoint(&recovery).transaction_generation(),
            }
        );
    }

    #[test]
    fn exact_requested_filesystem_reconciles_same_recovery_transaction() {
        let recovery = mkfs_issued().require_format_recovery().unwrap();
        let tx = checkpoint(&recovery).transaction_generation();
        let proof = ProjectDiskPostFormatProof::for_test(&recovery, checkpoint(&recovery), digest('f'));
        let observed = recovery.record_exact_filesystem_observed(&proof).unwrap();
        assert_eq!(checkpoint(&observed).transaction_generation(), tx);
        assert_eq!(checkpoint(&observed).stage(), ProjectDiskFormatStage::FilesystemObserved);
    }

    #[test]
    fn formatted_detached_requires_exact_post_format_and_p2_detach_evidence() {
        let issued = mkfs_issued();
        let post = ProjectDiskPostFormatProof::for_test(&issued, checkpoint(&issued), digest('f'));
        let observed = issued.record_exact_filesystem_observed(&post).unwrap();
        let phase = ProjectDiskFormatPhaseProof::for_test(
            &observed,
            checkpoint(&observed),
            ProjectDiskFormatStage::FilesystemObserved,
        );
        let detach_pending = observed.record_formatter_detach_pending(&phase).unwrap();
        let detached = ProjectDiskFormatterDetachedProof::for_test(
            &detach_pending,
            checkpoint(&detach_pending),
        );
        let formatted = detach_pending.record_formatted_detached(&detached).unwrap();
        assert_eq!(formatted.active_mutation(), ProjectDiskActiveMaterializationMutation::None);
        let ProjectDiskFilesystemMaterializationState::FormattedDetached { receipt } =
            formatted.filesystem()
        else {
            panic!("expected formatted detached");
        };
        assert_eq!(receipt.filesystem().filesystem_generation().get(), 7);
        assert_eq!(receipt.transaction_generation().get(), 1);
        assert!(receipt.outcome_digest().as_str().starts_with("sha256:"));
    }

    #[test]
    fn wrong_detached_physical_binding_is_rejected() {
        let issued = mkfs_issued();
        let post = ProjectDiskPostFormatProof::for_test(&issued, checkpoint(&issued), digest('f'));
        let observed = issued.record_exact_filesystem_observed(&post).unwrap();
        let phase = ProjectDiskFormatPhaseProof::for_test(
            &observed,
            checkpoint(&observed),
            ProjectDiskFormatStage::FilesystemObserved,
        );
        let detach_pending = observed.record_formatter_detach_pending(&phase).unwrap();
        let mut detached = ProjectDiskFormatterDetachedProof::for_test(
            &detach_pending,
            checkpoint(&detach_pending),
        );
        detached.physical_identity_digest = digest('9');
        assert!(detach_pending.record_formatted_detached(&detached).is_err());
    }
}
