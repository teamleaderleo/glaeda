//! Pure crash/replay model for one exact SmolRunner-owned project-disk create attempt.
//!
//! This module performs no persistence, process execution, Lima mutation, formatting, attachment,
//! cleanup, or proof minting. It models #644's durable authority checkpoints and hard no-replay
//! boundary before an exact create executor can exist.

use std::fmt;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::project_catalog::ProjectIdentity;
use crate::project_disk_host_observation::{LimaStandaloneDiskName, ProjectDiskPhysicalIdentity};
use crate::project_disk_lease::{ProjectDiskGeneration, ProjectDiskId};

pub const PROJECT_DISK_CREATE_STATE_SCHEMA_VERSION: u8 = 2;
const PROJECT_DISK_LOCATOR_DOMAIN: &[u8] = b"smolrunner-project-disk-locator-v1\0";
const PROJECT_DISK_LOCATOR_PREFIX: &str = "srpd-";
const PROJECT_DISK_LOCATOR_HEX_BYTES: usize = 24;

macro_rules! positive_generation_type {
    ($name:ident, $code:literal, $message:literal) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(u64);

        impl $name {
            pub fn new(value: u64) -> Result<Self, ProjectDiskCreateStateError> {
                if value == 0 {
                    return Err(error(
                        ProjectDiskCreateStateErrorKind::InvalidGeneration,
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
    ProjectDiskCreateRevision,
    "project_disk_create_revision_invalid",
    "project disk create revision must be greater than zero"
);
positive_generation_type!(
    ProjectDiskCreateAttemptGeneration,
    "project_disk_create_attempt_generation_invalid",
    "project disk create attempt generation must be greater than zero"
);

impl ProjectDiskCreateRevision {
    fn next(self) -> Result<Self, ProjectDiskCreateStateError> {
        Self::new(self.0.checked_add(1).ok_or_else(generation_exhausted)?)
    }
}

impl ProjectDiskCreateAttemptGeneration {
    fn next(self) -> Result<Self, ProjectDiskCreateStateError> {
        Self::new(self.0.checked_add(1).ok_or_else(generation_exhausted)?)
    }
}

/// Opaque durable identity for the exact protected Lima source used for one project-disk family.
///
/// The future P2/P3 composition mints this from descriptor-bound protected Lima state. A caller
/// outside this crate cannot bless an arbitrary digest into create authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ProjectDiskCreateSourceIdentity(Sha256Digest);

impl ProjectDiskCreateSourceIdentity {
    #[allow(dead_code)]
    pub(crate) const fn from_digest(digest: Sha256Digest) -> Self {
        Self(digest)
    }

    #[must_use]
    pub const fn digest(&self) -> &Sha256Digest {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDiskCreateQuarantineReason {
    AmbiguousUnboundPresent,
    ConflictingPhysicalState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiskCreatedUnformattedBinding {
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    source_identity: ProjectDiskCreateSourceIdentity,
    disk_name: LimaStandaloneDiskName,
    attempt_generation: ProjectDiskCreateAttemptGeneration,
    physical_identity: ProjectDiskPhysicalIdentity,
    backing_identity_digest: Sha256Digest,
    backing_logical_bytes: u64,
}

impl ProjectDiskCreatedUnformattedBinding {
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
    pub const fn source_identity(&self) -> &ProjectDiskCreateSourceIdentity {
        &self.source_identity
    }

    #[must_use]
    pub const fn disk_name(&self) -> &LimaStandaloneDiskName {
        &self.disk_name
    }

    #[must_use]
    pub const fn attempt_generation(&self) -> ProjectDiskCreateAttemptGeneration {
        self.attempt_generation
    }

    #[must_use]
    pub const fn physical_identity(&self) -> &ProjectDiskPhysicalIdentity {
        &self.physical_identity
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProjectDiskCreateState {
    Accepted,
    CreateAuthorized {
        attempt_generation: ProjectDiskCreateAttemptGeneration,
    },
    CreateStarted {
        attempt_generation: ProjectDiskCreateAttemptGeneration,
    },
    CreatedUnformatted {
        binding: ProjectDiskCreatedUnformattedBinding,
    },
    Quarantined {
        attempt_generation: ProjectDiskCreateAttemptGeneration,
        reason: ProjectDiskCreateQuarantineReason,
    },
}

/// Pure P3 record for one controller-owned project-disk generation.
///
/// The Lima locator is derived internally from the immutable project/disk generation plus exact
/// Lima-source identity. The caller never supplies the disk name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiskCreateRecord {
    schema_version: u8,
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    requested_logical_bytes: u64,
    source_identity: ProjectDiskCreateSourceIdentity,
    disk_name: LimaStandaloneDiskName,
    revision: ProjectDiskCreateRevision,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_attempt_generation: Option<ProjectDiskCreateAttemptGeneration>,
    state: ProjectDiskCreateState,
}

impl ProjectDiskCreateRecord {
    /// Accept one logical generation and derive its durable active locator claim.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal when the requested logical size is zero or the deterministic Lima
    /// locator cannot satisfy the reviewed locator grammar.
    pub fn new_accepted(
        project: ProjectIdentity,
        disk_id: ProjectDiskId,
        disk_generation: ProjectDiskGeneration,
        requested_logical_bytes: u64,
        source_identity: ProjectDiskCreateSourceIdentity,
    ) -> Result<Self, ProjectDiskCreateStateError> {
        if requested_logical_bytes == 0 {
            return Err(invalid_input());
        }
        let disk_name = derive_disk_name(&project, &disk_id, disk_generation, &source_identity)?;
        Ok(Self {
            schema_version: PROJECT_DISK_CREATE_STATE_SCHEMA_VERSION,
            project,
            disk_id,
            disk_generation,
            requested_logical_bytes,
            source_identity,
            disk_name,
            revision: ProjectDiskCreateRevision(1),
            last_attempt_generation: None,
            state: ProjectDiskCreateState::Accepted,
        })
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
    pub const fn requested_logical_bytes(&self) -> u64 {
        self.requested_logical_bytes
    }

    #[must_use]
    pub const fn source_identity(&self) -> &ProjectDiskCreateSourceIdentity {
        &self.source_identity
    }

    #[must_use]
    pub const fn disk_name(&self) -> &LimaStandaloneDiskName {
        &self.disk_name
    }

    #[must_use]
    pub const fn revision(&self) -> ProjectDiskCreateRevision {
        self.revision
    }

    #[must_use]
    pub const fn last_attempt_generation(&self) -> Option<ProjectDiskCreateAttemptGeneration> {
        self.last_attempt_generation
    }

    #[must_use]
    pub const fn state(&self) -> &ProjectDiskCreateState {
        &self.state
    }

    /// Plan one create authorization only from fresh sealed planned-locator absence.
    pub fn plan_create_authorization(
        &self,
        absence: &ProjectDiskCreateAbsenceProof,
    ) -> Result<ProjectDiskCreateAuthorizationPlan, ProjectDiskCreateStateError> {
        if !matches!(self.state, ProjectDiskCreateState::Accepted) {
            return Err(invalid_state(
                "project_disk_create_authorization_requires_accepted",
                "project disk create authorization requires accepted state",
            ));
        }
        absence.confirm(self)?;
        let attempt_generation = self.next_attempt_generation()?;
        Ok(ProjectDiskCreateAuthorizationPlan {
            identity: self.plan_identity(),
            attempt_generation,
        })
    }

    /// Accept the durable authorization checkpoint. This still grants zero process authority until
    /// the exact start plan is durably advanced to `CreateStarted` under the writer lock.
    pub fn record_create_authorized(
        &self,
        plan: &ProjectDiskCreateAuthorizationPlan,
    ) -> Result<Self, ProjectDiskCreateStateError> {
        if !matches!(self.state, ProjectDiskCreateState::Accepted) {
            return Err(invalid_state(
                "project_disk_create_authorization_requires_accepted",
                "project disk create authorization requires accepted state",
            ));
        }
        self.require_authorization_plan(plan)?;
        self.successor(
            ProjectDiskCreateState::CreateAuthorized {
                attempt_generation: plan.attempt_generation,
            },
            Some(plan.attempt_generation),
        )
    }

    /// Produce the exact no-replay start intent for the currently authorized attempt.
    pub fn plan_create_start(&self) -> Result<ProjectDiskCreateStartPlan, ProjectDiskCreateStateError> {
        let ProjectDiskCreateState::CreateAuthorized { attempt_generation } = self.state else {
            return Err(invalid_state(
                "project_disk_create_start_requires_authorized",
                "project disk create start requires an authorized attempt",
            ));
        };
        Ok(ProjectDiskCreateStartPlan {
            identity: self.plan_identity(),
            attempt_generation,
        })
    }

    /// Accept the durable no-replay checkpoint immediately before the external create executor.
    ///
    /// Once this successor exists, this module exposes no automatic create-retry transition.
    pub fn record_create_started(
        &self,
        plan: &ProjectDiskCreateStartPlan,
    ) -> Result<Self, ProjectDiskCreateStateError> {
        let ProjectDiskCreateState::CreateAuthorized { attempt_generation } = self.state else {
            return Err(invalid_state(
                "project_disk_create_start_requires_authorized",
                "project disk create start requires an authorized attempt",
            ));
        };
        if attempt_generation != plan.attempt_generation {
            return Err(plan_mismatch());
        }
        self.require_plan_identity(&plan.identity)?;
        self.successor(
            ProjectDiskCreateState::CreateStarted { attempt_generation },
            self.last_attempt_generation,
        )
    }

    /// Accept creation only inside the uninterrupted current-controller sequence that has both an
    /// exact executor success receipt and fresh descriptor-bound post-create physical evidence.
    pub fn record_create_success(
        &self,
        start: &ProjectDiskCreateStartPlan,
        execution: &ProjectDiskCreateExecutionReceipt,
        physical: &ProjectDiskCreatePhysicalProof,
    ) -> Result<Self, ProjectDiskCreateStateError> {
        let ProjectDiskCreateState::CreateStarted { attempt_generation } = self.state else {
            return Err(invalid_state(
                "project_disk_create_success_requires_started",
                "project disk create success requires the active started attempt",
            ));
        };
        if attempt_generation != start.attempt_generation {
            return Err(plan_mismatch());
        }
        self.require_started_plan_identity(&start.identity)?;
        execution.confirm(self, attempt_generation)?;
        physical.confirm(self, attempt_generation)?;
        let binding = ProjectDiskCreatedUnformattedBinding {
            project: self.project.clone(),
            disk_id: self.disk_id.clone(),
            disk_generation: self.disk_generation,
            source_identity: self.source_identity.clone(),
            disk_name: self.disk_name.clone(),
            attempt_generation,
            physical_identity: physical.physical_identity.clone(),
            backing_identity_digest: physical.backing_identity_digest.clone(),
            backing_logical_bytes: physical.backing_logical_bytes,
        };
        self.successor(
            ProjectDiskCreateState::CreatedUnformatted { binding },
            self.last_attempt_generation,
        )
    }

    /// Classify fresh absence after `CreateStarted` as blocked recovery evidence only.
    ///
    /// Absence cannot prove the prior create process is quiescent, so this method returns no
    /// successor and cannot authorize another create attempt.
    pub fn assess_started_absence(
        &self,
        absence: &ProjectDiskCreateAbsenceProof,
    ) -> Result<ProjectDiskCreateRecoveryAssessment, ProjectDiskCreateStateError> {
        let ProjectDiskCreateState::CreateStarted { attempt_generation } = self.state else {
            return Err(invalid_state(
                "project_disk_create_recovery_requires_started",
                "project disk create recovery requires a started attempt",
            ));
        };
        absence.confirm(self)?;
        Ok(ProjectDiskCreateRecoveryAssessment::BlockedPriorAttemptMayCommit {
            attempt_generation,
        })
    }

    /// Quarantine a started attempt when any physical disk is observed at the claimed locator after
    /// the uninterrupted success sequence has been lost.
    ///
    /// This deliberately refuses adoption even when the object happens to look like the intended
    /// raw disk.
    pub fn quarantine_started_present(&self) -> Result<Self, ProjectDiskCreateStateError> {
        let ProjectDiskCreateState::CreateStarted { attempt_generation } = self.state else {
            return Err(invalid_state(
                "project_disk_create_recovery_requires_started",
                "project disk create recovery requires a started attempt",
            ));
        };
        self.successor(
            ProjectDiskCreateState::Quarantined {
                attempt_generation,
                reason: ProjectDiskCreateQuarantineReason::AmbiguousUnboundPresent,
            },
            self.last_attempt_generation,
        )
    }

    /// Quarantine conflicting/foreign physical evidence. Quarantine only removes authority.
    pub fn quarantine_started_conflict(&self) -> Result<Self, ProjectDiskCreateStateError> {
        let ProjectDiskCreateState::CreateStarted { attempt_generation } = self.state else {
            return Err(invalid_state(
                "project_disk_create_recovery_requires_started",
                "project disk create recovery requires a started attempt",
            ));
        };
        self.successor(
            ProjectDiskCreateState::Quarantined {
                attempt_generation,
                reason: ProjectDiskCreateQuarantineReason::ConflictingPhysicalState,
            },
            self.last_attempt_generation,
        )
    }

    /// Reopen create planning only after a future recovery layer proves the prior process cannot
    /// still commit and P2 freshly proves exact locator absence.
    ///
    /// The old attempt generation remains recorded; the next authorization necessarily advances to
    /// N+1.
    pub fn record_prior_attempt_quiescent_absent(
        &self,
        quiescent: &ProjectDiskCreatePriorAttemptQuiescentProof,
        absence: &ProjectDiskCreateAbsenceProof,
    ) -> Result<Self, ProjectDiskCreateStateError> {
        let ProjectDiskCreateState::CreateStarted { attempt_generation } = self.state else {
            return Err(invalid_state(
                "project_disk_create_recovery_requires_started",
                "project disk create recovery requires a started attempt",
            ));
        };
        quiescent.confirm(self, attempt_generation)?;
        absence.confirm(self)?;
        self.successor(ProjectDiskCreateState::Accepted, self.last_attempt_generation)
    }

    fn next_attempt_generation(
        &self,
    ) -> Result<ProjectDiskCreateAttemptGeneration, ProjectDiskCreateStateError> {
        match self.last_attempt_generation {
            Some(generation) => generation.next(),
            None => ProjectDiskCreateAttemptGeneration::new(1),
        }
    }

    fn plan_identity(&self) -> ProjectDiskCreatePlanIdentity {
        ProjectDiskCreatePlanIdentity {
            project: self.project.clone(),
            disk_id: self.disk_id.clone(),
            disk_generation: self.disk_generation,
            source_identity: self.source_identity.clone(),
            disk_name: self.disk_name.clone(),
            expected_revision: self.revision,
        }
    }

    fn require_authorization_plan(
        &self,
        plan: &ProjectDiskCreateAuthorizationPlan,
    ) -> Result<(), ProjectDiskCreateStateError> {
        self.require_plan_identity(&plan.identity)?;
        if plan.attempt_generation != self.next_attempt_generation()? {
            return Err(plan_mismatch());
        }
        Ok(())
    }

    fn require_plan_identity(
        &self,
        identity: &ProjectDiskCreatePlanIdentity,
    ) -> Result<(), ProjectDiskCreateStateError> {
        if identity.project != self.project
            || identity.disk_id != self.disk_id
            || identity.disk_generation != self.disk_generation
            || identity.source_identity != self.source_identity
            || identity.disk_name != self.disk_name
        {
            return Err(plan_mismatch());
        }
        if identity.expected_revision != self.revision {
            return Err(stale_plan());
        }
        Ok(())
    }

    fn require_started_plan_identity(
        &self,
        identity: &ProjectDiskCreatePlanIdentity,
    ) -> Result<(), ProjectDiskCreateStateError> {
        if identity.project != self.project
            || identity.disk_id != self.disk_id
            || identity.disk_generation != self.disk_generation
            || identity.source_identity != self.source_identity
            || identity.disk_name != self.disk_name
        {
            return Err(plan_mismatch());
        }
        if identity.expected_revision >= self.revision {
            return Err(stale_plan());
        }
        Ok(())
    }

    fn successor(
        &self,
        state: ProjectDiskCreateState,
        last_attempt_generation: Option<ProjectDiskCreateAttemptGeneration>,
    ) -> Result<Self, ProjectDiskCreateStateError> {
        Ok(Self {
            schema_version: self.schema_version,
            project: self.project.clone(),
            disk_id: self.disk_id.clone(),
            disk_generation: self.disk_generation,
            requested_logical_bytes: self.requested_logical_bytes,
            source_identity: self.source_identity.clone(),
            disk_name: self.disk_name.clone(),
            revision: self.revision.next()?,
            last_attempt_generation,
            state,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectDiskCreatePlanIdentity {
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    source_identity: ProjectDiskCreateSourceIdentity,
    disk_name: LimaStandaloneDiskName,
    expected_revision: ProjectDiskCreateRevision,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDiskCreateAuthorizationPlan {
    identity: ProjectDiskCreatePlanIdentity,
    attempt_generation: ProjectDiskCreateAttemptGeneration,
}

impl ProjectDiskCreateAuthorizationPlan {
    #[must_use]
    pub const fn attempt_generation(&self) -> ProjectDiskCreateAttemptGeneration {
        self.attempt_generation
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDiskCreateStartPlan {
    identity: ProjectDiskCreatePlanIdentity,
    attempt_generation: ProjectDiskCreateAttemptGeneration,
}

impl ProjectDiskCreateStartPlan {
    #[must_use]
    pub const fn attempt_generation(&self) -> ProjectDiskCreateAttemptGeneration {
        self.attempt_generation
    }

    #[must_use]
    pub const fn disk_name(&self) -> &LimaStandaloneDiskName {
        &self.identity.disk_name
    }

    #[must_use]
    pub const fn requested_logical_bytes(&self) -> u64 {
        // The executor obtains size from the current record while holding the same writer lock.
        // This plan intentionally carries only immutable attempt identity plus locator.
        0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDiskCreateRecoveryAssessment {
    BlockedPriorAttemptMayCommit {
        attempt_generation: ProjectDiskCreateAttemptGeneration,
    },
}

/// Sealed fresh P2 evidence that the exact derived locator is absent.
pub struct ProjectDiskCreateAbsenceProof {
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    source_identity: ProjectDiskCreateSourceIdentity,
    disk_name: LimaStandaloneDiskName,
    create_revision: ProjectDiskCreateRevision,
}

impl ProjectDiskCreateAbsenceProof {
    fn confirm(&self, record: &ProjectDiskCreateRecord) -> Result<(), ProjectDiskCreateStateError> {
        if self.project != record.project
            || self.disk_id != record.disk_id
            || self.disk_generation != record.disk_generation
            || self.source_identity != record.source_identity
            || self.disk_name != record.disk_name
            || self.create_revision != record.revision
        {
            return Err(evidence_mismatch());
        }
        Ok(())
    }

    #[cfg(test)]
    fn for_test(record: &ProjectDiskCreateRecord) -> Self {
        Self {
            project: record.project.clone(),
            disk_id: record.disk_id.clone(),
            disk_generation: record.disk_generation,
            source_identity: record.source_identity.clone(),
            disk_name: record.disk_name.clone(),
            create_revision: record.revision,
        }
    }
}

impl fmt::Debug for ProjectDiskCreateAbsenceProof {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectDiskCreateAbsenceProof")
            .field("project", &self.project)
            .field("disk_id", &self.disk_id)
            .field("disk_generation", &self.disk_generation)
            .field("create_revision", &self.create_revision)
            .field("disk_name", &"<derived-non-authoritative-locator>")
            .finish()
    }
}

/// Sealed exact successful executor receipt for the current uninterrupted create attempt.
pub struct ProjectDiskCreateExecutionReceipt {
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    source_identity: ProjectDiskCreateSourceIdentity,
    disk_name: LimaStandaloneDiskName,
    attempt_generation: ProjectDiskCreateAttemptGeneration,
}

impl ProjectDiskCreateExecutionReceipt {
    fn confirm(
        &self,
        record: &ProjectDiskCreateRecord,
        attempt_generation: ProjectDiskCreateAttemptGeneration,
    ) -> Result<(), ProjectDiskCreateStateError> {
        if self.project != record.project
            || self.disk_id != record.disk_id
            || self.disk_generation != record.disk_generation
            || self.source_identity != record.source_identity
            || self.disk_name != record.disk_name
            || self.attempt_generation != attempt_generation
        {
            return Err(evidence_mismatch());
        }
        Ok(())
    }

    #[cfg(test)]
    fn for_test(record: &ProjectDiskCreateRecord, attempt_generation: ProjectDiskCreateAttemptGeneration) -> Self {
        Self {
            project: record.project.clone(),
            disk_id: record.disk_id.clone(),
            disk_generation: record.disk_generation,
            source_identity: record.source_identity.clone(),
            disk_name: record.disk_name.clone(),
            attempt_generation,
        }
    }
}

/// Sealed fresh P2 post-create evidence for the uninterrupted successful attempt.
pub struct ProjectDiskCreatePhysicalProof {
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    source_identity: ProjectDiskCreateSourceIdentity,
    disk_name: LimaStandaloneDiskName,
    attempt_generation: ProjectDiskCreateAttemptGeneration,
    physical_identity: ProjectDiskPhysicalIdentity,
    backing_identity_digest: Sha256Digest,
    backing_logical_bytes: u64,
}

impl ProjectDiskCreatePhysicalProof {
    fn confirm(
        &self,
        record: &ProjectDiskCreateRecord,
        attempt_generation: ProjectDiskCreateAttemptGeneration,
    ) -> Result<(), ProjectDiskCreateStateError> {
        if self.project != record.project
            || self.disk_id != record.disk_id
            || self.disk_generation != record.disk_generation
            || self.source_identity != record.source_identity
            || self.disk_name != record.disk_name
            || self.attempt_generation != attempt_generation
            || self.backing_identity_digest.as_str() == "sha256:0000000000000000000000000000000000000000000000000000000000000000"
            || self.backing_logical_bytes != record.requested_logical_bytes
        {
            return Err(evidence_mismatch());
        }
        Ok(())
    }

    #[cfg(test)]
    fn for_test(
        record: &ProjectDiskCreateRecord,
        attempt_generation: ProjectDiskCreateAttemptGeneration,
        physical_identity: ProjectDiskPhysicalIdentity,
        backing_identity_digest: Sha256Digest,
    ) -> Self {
        Self {
            project: record.project.clone(),
            disk_id: record.disk_id.clone(),
            disk_generation: record.disk_generation,
            source_identity: record.source_identity.clone(),
            disk_name: record.disk_name.clone(),
            attempt_generation,
            physical_identity,
            backing_identity_digest,
            backing_logical_bytes: record.requested_logical_bytes,
        }
    }
}

/// Sealed future recovery proof that the previous external create process cannot still commit.
pub struct ProjectDiskCreatePriorAttemptQuiescentProof {
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    source_identity: ProjectDiskCreateSourceIdentity,
    disk_name: LimaStandaloneDiskName,
    attempt_generation: ProjectDiskCreateAttemptGeneration,
}

impl ProjectDiskCreatePriorAttemptQuiescentProof {
    fn confirm(
        &self,
        record: &ProjectDiskCreateRecord,
        attempt_generation: ProjectDiskCreateAttemptGeneration,
    ) -> Result<(), ProjectDiskCreateStateError> {
        if self.project != record.project
            || self.disk_id != record.disk_id
            || self.disk_generation != record.disk_generation
            || self.source_identity != record.source_identity
            || self.disk_name != record.disk_name
            || self.attempt_generation != attempt_generation
        {
            return Err(evidence_mismatch());
        }
        Ok(())
    }

    #[cfg(test)]
    fn for_test(record: &ProjectDiskCreateRecord, attempt_generation: ProjectDiskCreateAttemptGeneration) -> Self {
        Self {
            project: record.project.clone(),
            disk_id: record.disk_id.clone(),
            disk_generation: record.disk_generation,
            source_identity: record.source_identity.clone(),
            disk_name: record.disk_name.clone(),
            attempt_generation,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDiskCreateStateErrorKind {
    InvalidGeneration,
    InvalidInput,
    InvalidState,
    EvidenceMismatch,
    PlanMismatch,
    StalePlan,
    GenerationExhausted,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProjectDiskCreateStateError {
    kind: ProjectDiskCreateStateErrorKind,
    code: &'static str,
    message: &'static str,
}

impl ProjectDiskCreateStateError {
    #[must_use]
    pub const fn kind(self) -> ProjectDiskCreateStateErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for ProjectDiskCreateStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectDiskCreateStateError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for ProjectDiskCreateStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ProjectDiskCreateStateError {}

fn derive_disk_name(
    project: &ProjectIdentity,
    disk_id: &ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    source_identity: &ProjectDiskCreateSourceIdentity,
) -> Result<LimaStandaloneDiskName, ProjectDiskCreateStateError> {
    let mut hasher = Sha256::new();
    hasher.update(PROJECT_DISK_LOCATOR_DOMAIN);
    for field in [
        project.as_str().as_bytes(),
        disk_id.as_str().as_bytes(),
        &disk_generation.get().to_be_bytes(),
        source_identity.digest().as_str().as_bytes(),
    ] {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field);
    }
    let digest = hasher.finalize();
    let mut name = String::with_capacity(
        PROJECT_DISK_LOCATOR_PREFIX.len() + PROJECT_DISK_LOCATOR_HEX_BYTES * 2,
    );
    name.push_str(PROJECT_DISK_LOCATOR_PREFIX);
    for byte in digest.iter().take(PROJECT_DISK_LOCATOR_HEX_BYTES) {
        name.push_str(&format!("{byte:02x}"));
    }
    LimaStandaloneDiskName::parse(&name).map_err(|_| invalid_input())
}

const fn error(
    kind: ProjectDiskCreateStateErrorKind,
    code: &'static str,
    message: &'static str,
) -> ProjectDiskCreateStateError {
    ProjectDiskCreateStateError {
        kind,
        code,
        message,
    }
}

const fn invalid_input() -> ProjectDiskCreateStateError {
    error(
        ProjectDiskCreateStateErrorKind::InvalidInput,
        "project_disk_create_input_invalid",
        "project disk create input is invalid",
    )
}

const fn invalid_state(code: &'static str, message: &'static str) -> ProjectDiskCreateStateError {
    error(ProjectDiskCreateStateErrorKind::InvalidState, code, message)
}

const fn evidence_mismatch() -> ProjectDiskCreateStateError {
    error(
        ProjectDiskCreateStateErrorKind::EvidenceMismatch,
        "project_disk_create_evidence_mismatch",
        "project disk create evidence does not match the current generation",
    )
}

const fn plan_mismatch() -> ProjectDiskCreateStateError {
    error(
        ProjectDiskCreateStateErrorKind::PlanMismatch,
        "project_disk_create_plan_mismatch",
        "project disk create plan does not match the current generation",
    )
}

const fn stale_plan() -> ProjectDiskCreateStateError {
    error(
        ProjectDiskCreateStateErrorKind::StalePlan,
        "project_disk_create_plan_stale",
        "project disk create plan revision is stale",
    )
}

const fn generation_exhausted() -> ProjectDiskCreateStateError {
    error(
        ProjectDiskCreateStateErrorKind::GenerationExhausted,
        "project_disk_create_generation_exhausted",
        "project disk create generation is exhausted",
    )
}

#[cfg(test)]
mod tests {
    use super::{
        ProjectDiskCreateAbsenceProof, ProjectDiskCreateExecutionReceipt,
        ProjectDiskCreatePhysicalProof, ProjectDiskCreatePriorAttemptQuiescentProof,
        ProjectDiskCreateQuarantineReason, ProjectDiskCreateRecord, ProjectDiskCreateRecoveryAssessment,
        ProjectDiskCreateSourceIdentity, ProjectDiskCreateState, ProjectDiskCreateStateErrorKind,
    };
    use crate::artifact::Sha256Digest;
    use crate::project_catalog::ProjectIdentity;
    use crate::project_disk_host_observation::ProjectDiskPhysicalIdentity;
    use crate::project_disk_lease::{ProjectDiskGeneration, ProjectDiskId};

    fn digest(byte: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    fn source(byte: char) -> ProjectDiskCreateSourceIdentity {
        ProjectDiskCreateSourceIdentity::from_digest(digest(byte))
    }

    fn record(
        disk_id: &str,
        disk_generation: u64,
        source_byte: char,
    ) -> ProjectDiskCreateRecord {
        ProjectDiskCreateRecord::new_accepted(
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            ProjectDiskId::parse(disk_id).unwrap(),
            ProjectDiskGeneration::new(disk_generation).unwrap(),
            1_073_741_824,
            source(source_byte),
        )
        .unwrap()
    }

    fn started(record: &ProjectDiskCreateRecord) -> (ProjectDiskCreateRecord, super::ProjectDiskCreateStartPlan) {
        let authorization = record
            .plan_create_authorization(&ProjectDiskCreateAbsenceProof::for_test(record))
            .unwrap();
        let authorized = record.record_create_authorized(&authorization).unwrap();
        let start = authorized.plan_create_start().unwrap();
        let started = authorized.record_create_started(&start).unwrap();
        (started, start)
    }

    #[test]
    fn locator_is_controller_derived_from_generation_and_source_identity() {
        let first = record("disk-a", 3, 'a');
        let same = record("disk-a", 3, 'a');
        let next_generation = record("disk-a", 4, 'a');
        let next_source = record("disk-a", 3, 'b');
        assert_eq!(first.disk_name(), same.disk_name());
        assert_ne!(first.disk_name(), next_generation.disk_name());
        assert_ne!(first.disk_name(), next_source.disk_name());
        assert!(first.disk_name().as_str().starts_with("srpd-"));
        assert!(first.disk_name().as_str().len() >= 37);
    }

    #[test]
    fn authorization_requires_fresh_exact_absence() {
        let current = record("disk-a", 3, 'a');
        let foreign = record("disk-b", 3, 'a');
        assert_eq!(
            current
                .plan_create_authorization(&ProjectDiskCreateAbsenceProof::for_test(&foreign))
                .unwrap_err()
                .kind(),
            ProjectDiskCreateStateErrorKind::EvidenceMismatch
        );
        let plan = current
            .plan_create_authorization(&ProjectDiskCreateAbsenceProof::for_test(&current))
            .unwrap();
        assert_eq!(plan.attempt_generation().get(), 1);
    }

    #[test]
    fn create_started_requires_prior_durable_authorization() {
        let accepted = record("disk-a", 3, 'a');
        assert!(accepted.plan_create_start().is_err());
        let authorization = accepted
            .plan_create_authorization(&ProjectDiskCreateAbsenceProof::for_test(&accepted))
            .unwrap();
        let authorized = accepted.record_create_authorized(&authorization).unwrap();
        assert!(matches!(authorized.state(), ProjectDiskCreateState::CreateAuthorized { .. }));
        let start = authorized.plan_create_start().unwrap();
        let started = authorized.record_create_started(&start).unwrap();
        assert!(matches!(started.state(), ProjectDiskCreateState::CreateStarted { .. }));
    }

    #[test]
    fn uninterrupted_success_requires_executor_and_physical_proof() {
        let accepted = record("disk-a", 3, 'a');
        let (started, start) = started(&accepted);
        let execution = ProjectDiskCreateExecutionReceipt::for_test(&started, start.attempt_generation());
        let physical = ProjectDiskCreatePhysicalProof::for_test(
            &started,
            start.attempt_generation(),
            ProjectDiskPhysicalIdentity::parse(
                "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            )
            .unwrap(),
            digest('c'),
        );
        let created = started
            .record_create_success(&start, &execution, &physical)
            .unwrap();
        let ProjectDiskCreateState::CreatedUnformatted { binding } = created.state() else {
            panic!("expected created unformatted");
        };
        assert_eq!(binding.attempt_generation().get(), 1);
        assert_eq!(binding.backing_logical_bytes(), 1_073_741_824);
        assert_eq!(binding.backing_identity_digest(), &digest('c'));
    }

    #[test]
    fn fresh_absence_after_create_started_never_authorizes_replay() {
        let accepted = record("disk-a", 3, 'a');
        let (started, _) = started(&accepted);
        let assessment = started
            .assess_started_absence(&ProjectDiskCreateAbsenceProof::for_test(&started))
            .unwrap();
        assert_eq!(
            assessment,
            ProjectDiskCreateRecoveryAssessment::BlockedPriorAttemptMayCommit {
                attempt_generation: started.last_attempt_generation().unwrap(),
            }
        );
        assert!(
            started
                .plan_create_authorization(&ProjectDiskCreateAbsenceProof::for_test(&started))
                .is_err()
        );
        assert!(matches!(started.state(), ProjectDiskCreateState::CreateStarted { .. }));
    }

    #[test]
    fn exact_looking_presence_after_lost_sequence_is_quarantined_not_adopted() {
        let accepted = record("disk-a", 3, 'a');
        let (started, _) = started(&accepted);
        let quarantined = started.quarantine_started_present().unwrap();
        assert!(matches!(
            quarantined.state(),
            ProjectDiskCreateState::Quarantined {
                reason: ProjectDiskCreateQuarantineReason::AmbiguousUnboundPresent,
                ..
            }
        ));
    }

    #[test]
    fn retry_requires_separate_quiescence_proof_plus_fresh_absence() {
        let accepted = record("disk-a", 3, 'a');
        let (started, _) = started(&accepted);
        let attempt = started.last_attempt_generation().unwrap();
        let quiescent = ProjectDiskCreatePriorAttemptQuiescentProof::for_test(&started, attempt);
        let absence = ProjectDiskCreateAbsenceProof::for_test(&started);
        let recovered = started
            .record_prior_attempt_quiescent_absent(&quiescent, &absence)
            .unwrap();
        assert!(matches!(recovered.state(), ProjectDiskCreateState::Accepted));
        let second = recovered
            .plan_create_authorization(&ProjectDiskCreateAbsenceProof::for_test(&recovered))
            .unwrap();
        assert_eq!(attempt.get(), 1);
        assert_eq!(second.attempt_generation().get(), 2);
    }

    #[test]
    fn quiescence_proof_from_another_generation_cannot_reopen_create() {
        let accepted = record("disk-a", 3, 'a');
        let (started, _) = started(&accepted);
        let other = record("disk-b", 3, 'a');
        let wrong = ProjectDiskCreatePriorAttemptQuiescentProof::for_test(
            &other,
            started.last_attempt_generation().unwrap(),
        );
        assert!(
            started
                .record_prior_attempt_quiescent_absent(
                    &wrong,
                    &ProjectDiskCreateAbsenceProof::for_test(&started),
                )
                .is_err()
        );
    }

    #[test]
    fn conflicting_physical_state_only_removes_authority() {
        let accepted = record("disk-a", 3, 'a');
        let (started, _) = started(&accepted);
        let quarantined = started.quarantine_started_conflict().unwrap();
        assert!(matches!(
            quarantined.state(),
            ProjectDiskCreateState::Quarantined {
                reason: ProjectDiskCreateQuarantineReason::ConflictingPhysicalState,
                ..
            }
        ));
    }
}
