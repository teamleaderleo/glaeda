//! Pure crash/replay model for one exact SmolRunner-owned project-disk create attempt.
//!
//! This module performs no persistence, process execution, Lima mutation, formatting, attachment,
//! cleanup, or proof minting. It models the authority checkpoints P3 needs before an exact create
//! executor can exist. Production absence/post-create evidence remains sealed until the #565 P2
//! observer is explicitly composed with these proof types.

use std::fmt;

use serde::Serialize;

use crate::project_catalog::ProjectIdentity;
use crate::project_disk_host_observation::{LimaStandaloneDiskName, ProjectDiskPhysicalIdentity};
use crate::project_disk_lease::{ProjectDiskGeneration, ProjectDiskId};

pub const PROJECT_DISK_CREATE_STATE_SCHEMA_VERSION: u8 = 1;

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProjectDiskCreateState {
    Planned,
    Creating {
        attempt_generation: ProjectDiskCreateAttemptGeneration,
    },
    CreatedUnformatted {
        attempt_generation: ProjectDiskCreateAttemptGeneration,
        physical_identity: ProjectDiskPhysicalIdentity,
        backing_logical_bytes: u64,
    },
    RevalidateRequired {
        attempt_generation: ProjectDiskCreateAttemptGeneration,
    },
    Quarantined {
        attempt_generation: ProjectDiskCreateAttemptGeneration,
    },
}

/// Pure P3 record for one intended project-disk generation.
///
/// `new_planned` records desired logical identity only. It grants zero external create authority.
/// A future production executor must additionally persist the `Creating` successor atomically and
/// hold the reviewed mutation lock before issuing `limactl disk create`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiskCreateRecord {
    schema_version: u8,
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    disk_name: LimaStandaloneDiskName,
    revision: ProjectDiskCreateRevision,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_attempt_generation: Option<ProjectDiskCreateAttemptGeneration>,
    state: ProjectDiskCreateState,
}

impl ProjectDiskCreateRecord {
    #[must_use]
    pub fn new_planned(
        project: ProjectIdentity,
        disk_id: ProjectDiskId,
        disk_generation: ProjectDiskGeneration,
        disk_name: LimaStandaloneDiskName,
    ) -> Self {
        Self {
            schema_version: PROJECT_DISK_CREATE_STATE_SCHEMA_VERSION,
            project,
            disk_id,
            disk_generation,
            disk_name,
            revision: ProjectDiskCreateRevision(1),
            last_attempt_generation: None,
            state: ProjectDiskCreateState::Planned,
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

    /// Plan one create attempt only from exact fresh descriptor-bound absence evidence.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal unless the record is `planned` and `absence` is sealed to this
    /// exact project/disk generation, locator, and current create revision.
    pub fn plan_create(
        &self,
        absence: &ProjectDiskCreateAbsenceProof,
    ) -> Result<ProjectDiskCreatePlan, ProjectDiskCreateStateError> {
        if !matches!(self.state, ProjectDiskCreateState::Planned) {
            return Err(invalid_state(
                "project_disk_create_requires_planned",
                "project disk create planning requires planned state",
            ));
        }
        absence.confirm(self)?;
        let attempt_generation = self.next_attempt_generation()?;
        Ok(ProjectDiskCreatePlan {
            project: self.project.clone(),
            disk_id: self.disk_id.clone(),
            disk_generation: self.disk_generation,
            disk_name: self.disk_name.clone(),
            expected_revision: self.revision,
            attempt_generation,
        })
    }

    /// Accept the durable pre-mutation checkpoint for one exact create plan.
    ///
    /// The returned `Creating` record is still pure data. A production caller must persist this
    /// successor under the P3 mutation lock before any external create command is allowed.
    pub fn record_create_started(
        &self,
        plan: &ProjectDiskCreatePlan,
    ) -> Result<Self, ProjectDiskCreateStateError> {
        if !matches!(self.state, ProjectDiskCreateState::Planned) {
            return Err(invalid_state(
                "project_disk_create_requires_planned",
                "project disk create start requires planned state",
            ));
        }
        self.require_planned_plan(plan)?;
        self.successor(
            ProjectDiskCreateState::Creating {
                attempt_generation: plan.attempt_generation,
            },
            Some(plan.attempt_generation),
        )
    }

    /// Accept one exact post-create physical identity for the active create attempt.
    pub fn record_create_success(
        &self,
        plan: &ProjectDiskCreatePlan,
        physical: &ProjectDiskCreatePhysicalProof,
    ) -> Result<Self, ProjectDiskCreateStateError> {
        self.require_active_plan(plan, false)?;
        physical.confirm(self, plan.attempt_generation)?;
        self.successor(
            ProjectDiskCreateState::CreatedUnformatted {
                attempt_generation: plan.attempt_generation,
                physical_identity: physical.physical_identity.clone(),
                backing_logical_bytes: physical.backing_logical_bytes,
            },
            self.last_attempt_generation,
        )
    }

    /// Record ambiguous create execution evidence after command start/response loss.
    ///
    /// This transition deliberately blocks blind replay. Reconciliation must freshly prove either
    /// the exact created physical identity or exact locator absence before another attempt exists.
    pub fn record_create_ambiguous(
        &self,
        plan: &ProjectDiskCreatePlan,
    ) -> Result<Self, ProjectDiskCreateStateError> {
        self.require_active_plan(plan, false)?;
        self.successor(
            ProjectDiskCreateState::RevalidateRequired {
                attempt_generation: plan.attempt_generation,
            },
            self.last_attempt_generation,
        )
    }

    /// Resolve ambiguous execution as the exact created physical disk.
    pub fn record_revalidation_created(
        &self,
        physical: &ProjectDiskCreatePhysicalProof,
    ) -> Result<Self, ProjectDiskCreateStateError> {
        let ProjectDiskCreateState::RevalidateRequired { attempt_generation } = self.state else {
            return Err(invalid_state(
                "project_disk_create_revalidation_required",
                "project disk create is not awaiting revalidation",
            ));
        };
        physical.confirm(self, attempt_generation)?;
        self.successor(
            ProjectDiskCreateState::CreatedUnformatted {
                attempt_generation,
                physical_identity: physical.physical_identity.clone(),
                backing_logical_bytes: physical.backing_logical_bytes,
            },
            self.last_attempt_generation,
        )
    }

    /// Resolve ambiguous execution as freshly proven absence.
    ///
    /// The prior attempt generation is retained, so a later create plan advances monotonically and
    /// can never masquerade as a replay of the ambiguous attempt.
    pub fn record_revalidation_absent(
        &self,
        absence: &ProjectDiskCreateAbsenceProof,
    ) -> Result<Self, ProjectDiskCreateStateError> {
        if !matches!(self.state, ProjectDiskCreateState::RevalidateRequired { .. }) {
            return Err(invalid_state(
                "project_disk_create_revalidation_required",
                "project disk create is not awaiting revalidation",
            ));
        }
        absence.confirm(self)?;
        self.successor(ProjectDiskCreateState::Planned, self.last_attempt_generation)
    }

    /// Quarantine an ambiguous attempt when fresh observation proves conflicting/foreign state.
    ///
    /// Quarantine only removes authority, so this pure transition requires no positive ownership
    /// evidence and exposes no mutation capability.
    pub fn quarantine_revalidation(&self) -> Result<Self, ProjectDiskCreateStateError> {
        let ProjectDiskCreateState::RevalidateRequired { attempt_generation } = self.state else {
            return Err(invalid_state(
                "project_disk_create_revalidation_required",
                "project disk create is not awaiting revalidation",
            ));
        };
        self.successor(
            ProjectDiskCreateState::Quarantined { attempt_generation },
            self.last_attempt_generation,
        )
    }

    fn next_attempt_generation(
        &self,
    ) -> Result<ProjectDiskCreateAttemptGeneration, ProjectDiskCreateStateError> {
        match self.last_attempt_generation {
            Some(generation) => generation.next(),
            None => ProjectDiskCreateAttemptGeneration::new(1),
        }
    }

    fn require_planned_plan(
        &self,
        plan: &ProjectDiskCreatePlan,
    ) -> Result<(), ProjectDiskCreateStateError> {
        if !plan.matches_record_identity(self) {
            return Err(plan_mismatch());
        }
        if plan.expected_revision != self.revision {
            return Err(stale_plan());
        }
        if plan.attempt_generation != self.next_attempt_generation()? {
            return Err(plan_mismatch());
        }
        Ok(())
    }

    fn require_active_plan(
        &self,
        plan: &ProjectDiskCreatePlan,
        allow_revalidation: bool,
    ) -> Result<(), ProjectDiskCreateStateError> {
        if !plan.matches_record_identity(self)
            || self.last_attempt_generation != Some(plan.attempt_generation)
        {
            return Err(plan_mismatch());
        }
        let matches_state = match self.state {
            ProjectDiskCreateState::Creating { attempt_generation } => {
                attempt_generation == plan.attempt_generation
            }
            ProjectDiskCreateState::RevalidateRequired { attempt_generation }
                if allow_revalidation =>
            {
                attempt_generation == plan.attempt_generation
            }
            _ => false,
        };
        if !matches_state {
            return Err(invalid_state(
                "project_disk_create_attempt_inactive",
                "project disk create plan is not the active attempt",
            ));
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
            disk_name: self.disk_name.clone(),
            revision: self.revision.next()?,
            last_attempt_generation,
            state,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiskCreatePlan {
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    disk_name: LimaStandaloneDiskName,
    expected_revision: ProjectDiskCreateRevision,
    attempt_generation: ProjectDiskCreateAttemptGeneration,
}

impl ProjectDiskCreatePlan {
    #[must_use]
    pub const fn expected_revision(&self) -> ProjectDiskCreateRevision {
        self.expected_revision
    }

    #[must_use]
    pub const fn attempt_generation(&self) -> ProjectDiskCreateAttemptGeneration {
        self.attempt_generation
    }

    #[must_use]
    pub const fn disk_name(&self) -> &LimaStandaloneDiskName {
        &self.disk_name
    }

    fn matches_record_identity(&self, record: &ProjectDiskCreateRecord) -> bool {
        self.project == record.project
            && self.disk_id == record.disk_id
            && self.disk_generation == record.disk_generation
            && self.disk_name == record.disk_name
    }
}

/// Sealed fresh P2 evidence that the exact planned locator is absent.
///
/// There is intentionally no production constructor in this slice. #639 must later mint this only
/// from a live `LimaStandaloneDiskAbsenceObservation` whose hidden request/locator binding matches
/// the exact P3 record. This prevents callers from turning a disk name into create authority.
pub struct ProjectDiskCreateAbsenceProof {
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    disk_name: LimaStandaloneDiskName,
    create_revision: ProjectDiskCreateRevision,
}

impl ProjectDiskCreateAbsenceProof {
    fn confirm(&self, record: &ProjectDiskCreateRecord) -> Result<(), ProjectDiskCreateStateError> {
        if self.project != record.project
            || self.disk_id != record.disk_id
            || self.disk_generation != record.disk_generation
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
            .field("disk_name", &"<non-authoritative-locator>")
            .finish()
    }
}

/// Sealed fresh P2 post-create evidence for one active P3 create attempt.
///
/// There is intentionally no production constructor in this slice. The later P2/P3 composition
/// must create it only from a fresh descriptor-bound post-create observation after the durable
/// `Creating` checkpoint exists.
pub struct ProjectDiskCreatePhysicalProof {
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    disk_name: LimaStandaloneDiskName,
    attempt_generation: ProjectDiskCreateAttemptGeneration,
    physical_identity: ProjectDiskPhysicalIdentity,
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
            || self.disk_name != record.disk_name
            || self.attempt_generation != attempt_generation
            || self.backing_logical_bytes == 0
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
        backing_logical_bytes: u64,
    ) -> Self {
        Self {
            project: record.project.clone(),
            disk_id: record.disk_id.clone(),
            disk_generation: record.disk_generation,
            disk_name: record.disk_name.clone(),
            attempt_generation,
            physical_identity,
            backing_logical_bytes,
        }
    }
}

impl fmt::Debug for ProjectDiskCreatePhysicalProof {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectDiskCreatePhysicalProof")
            .field("project", &self.project)
            .field("disk_id", &self.disk_id)
            .field("disk_generation", &self.disk_generation)
            .field("attempt_generation", &self.attempt_generation)
            .field("physical_identity", &self.physical_identity)
            .field("backing_logical_bytes", &self.backing_logical_bytes)
            .field("disk_name", &"<non-authoritative-locator>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDiskCreateStateErrorKind {
    InvalidGeneration,
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
        ProjectDiskCreateAbsenceProof, ProjectDiskCreatePhysicalProof, ProjectDiskCreateRecord,
        ProjectDiskCreateState, ProjectDiskCreateStateErrorKind,
    };
    use crate::project_catalog::ProjectIdentity;
    use crate::project_disk_host_observation::{LimaStandaloneDiskName, ProjectDiskPhysicalIdentity};
    use crate::project_disk_lease::{ProjectDiskGeneration, ProjectDiskId};

    fn record(disk_id: &str, disk_generation: u64) -> ProjectDiskCreateRecord {
        ProjectDiskCreateRecord::new_planned(
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            ProjectDiskId::parse(disk_id).unwrap(),
            ProjectDiskGeneration::new(disk_generation).unwrap(),
            LimaStandaloneDiskName::parse("smolrunner-project-a").unwrap(),
        )
    }

    fn physical_identity(byte: char) -> ProjectDiskPhysicalIdentity {
        ProjectDiskPhysicalIdentity::parse(&format!("sha256:{}", byte.to_string().repeat(64)))
            .unwrap()
    }

    #[test]
    fn create_requires_sealed_fresh_absence() {
        let current = record("disk-a", 3);
        let foreign = record("disk-b", 3);
        let wrong = ProjectDiskCreateAbsenceProof::for_test(&foreign);
        let error = current.plan_create(&wrong).unwrap_err();
        assert_eq!(error.kind(), ProjectDiskCreateStateErrorKind::EvidenceMismatch);

        let absence = ProjectDiskCreateAbsenceProof::for_test(&current);
        let plan = current.plan_create(&absence).unwrap();
        assert_eq!(plan.attempt_generation().get(), 1);
        assert_eq!(plan.expected_revision(), current.revision());
    }

    #[test]
    fn create_success_requires_durable_started_phase_and_exact_physical_proof() {
        let planned = record("disk-a", 3);
        let plan = planned
            .plan_create(&ProjectDiskCreateAbsenceProof::for_test(&planned))
            .unwrap();
        let physical = ProjectDiskCreatePhysicalProof::for_test(
            &planned,
            plan.attempt_generation(),
            physical_identity('a'),
            1_073_741_824,
        );
        assert!(planned.record_create_success(&plan, &physical).is_err());

        let creating = planned.record_create_started(&plan).unwrap();
        let physical = ProjectDiskCreatePhysicalProof::for_test(
            &creating,
            plan.attempt_generation(),
            physical_identity('a'),
            1_073_741_824,
        );
        let created = creating.record_create_success(&plan, &physical).unwrap();
        assert!(matches!(
            created.state(),
            ProjectDiskCreateState::CreatedUnformatted {
                attempt_generation,
                backing_logical_bytes: 1_073_741_824,
                ..
            } if attempt_generation.get() == 1
        ));
    }

    #[test]
    fn ambiguous_create_cannot_be_blindly_replayed() {
        let planned = record("disk-a", 3);
        let plan = planned
            .plan_create(&ProjectDiskCreateAbsenceProof::for_test(&planned))
            .unwrap();
        let creating = planned.record_create_started(&plan).unwrap();
        let ambiguous = creating.record_create_ambiguous(&plan).unwrap();
        assert!(matches!(
            ambiguous.state(),
            ProjectDiskCreateState::RevalidateRequired { .. }
        ));
        assert!(ambiguous.plan_create(&ProjectDiskCreateAbsenceProof::for_test(&ambiguous)).is_err());
    }

    #[test]
    fn fresh_absence_after_ambiguity_allows_only_a_new_attempt_generation() {
        let planned = record("disk-a", 3);
        let first = planned
            .plan_create(&ProjectDiskCreateAbsenceProof::for_test(&planned))
            .unwrap();
        let creating = planned.record_create_started(&first).unwrap();
        let ambiguous = creating.record_create_ambiguous(&first).unwrap();
        let absent = ProjectDiskCreateAbsenceProof::for_test(&ambiguous);
        let replanned = ambiguous.record_revalidation_absent(&absent).unwrap();
        let second = replanned
            .plan_create(&ProjectDiskCreateAbsenceProof::for_test(&replanned))
            .unwrap();
        assert_eq!(first.attempt_generation().get(), 1);
        assert_eq!(second.attempt_generation().get(), 2);
        assert!(replanned.record_create_started(&first).is_err());
    }

    #[test]
    fn ambiguous_create_can_reconcile_to_exact_created_identity() {
        let planned = record("disk-a", 3);
        let plan = planned
            .plan_create(&ProjectDiskCreateAbsenceProof::for_test(&planned))
            .unwrap();
        let creating = planned.record_create_started(&plan).unwrap();
        let ambiguous = creating.record_create_ambiguous(&plan).unwrap();
        let physical = ProjectDiskCreatePhysicalProof::for_test(
            &ambiguous,
            plan.attempt_generation(),
            physical_identity('b'),
            2_147_483_648,
        );
        let created = ambiguous.record_revalidation_created(&physical).unwrap();
        assert!(matches!(
            created.state(),
            ProjectDiskCreateState::CreatedUnformatted {
                physical_identity,
                backing_logical_bytes: 2_147_483_648,
                ..
            } if physical_identity == &physical_identity('b')
        ));
    }

    #[test]
    fn same_name_never_substitutes_for_exact_generation_binding() {
        let first = record("disk-a", 3);
        let second = record("disk-b", 4);
        assert_eq!(first.disk_name(), second.disk_name());

        let plan = first
            .plan_create(&ProjectDiskCreateAbsenceProof::for_test(&first))
            .unwrap();
        let creating = first.record_create_started(&plan).unwrap();
        let wrong = ProjectDiskCreatePhysicalProof::for_test(
            &second,
            plan.attempt_generation(),
            physical_identity('c'),
            1_073_741_824,
        );
        assert_eq!(
            creating.record_create_success(&plan, &wrong).unwrap_err().kind(),
            ProjectDiskCreateStateErrorKind::EvidenceMismatch
        );
    }

    #[test]
    fn stale_plan_is_rejected_after_record_revision_moves() {
        let planned = record("disk-a", 3);
        let plan = planned
            .plan_create(&ProjectDiskCreateAbsenceProof::for_test(&planned))
            .unwrap();
        let creating = planned.record_create_started(&plan).unwrap();
        assert!(creating.record_create_started(&plan).is_err());
    }

    #[test]
    fn conflicting_revalidation_can_only_quarantine() {
        let planned = record("disk-a", 3);
        let plan = planned
            .plan_create(&ProjectDiskCreateAbsenceProof::for_test(&planned))
            .unwrap();
        let creating = planned.record_create_started(&plan).unwrap();
        let ambiguous = creating.record_create_ambiguous(&plan).unwrap();
        let quarantined = ambiguous.quarantine_revalidation().unwrap();
        assert!(matches!(
            quarantined.state(),
            ProjectDiskCreateState::Quarantined { .. }
        ));
    }
}
