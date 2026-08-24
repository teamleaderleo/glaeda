//! Pure short-lived witness for one exact planned project-disk attachment.
//!
//! This module performs no persistence, process execution, Lima observation, disk mutation, guest
//! mutation, or proof minting. It exists so P4 can carry the exact P1 attachment plan through a
//! physical attach attempt without fabricating an already-`Attached` lease record merely to let a
//! later observer correlate the result.

// Staged #640 integration seam. #639/P4 will consume this type once the descriptor observer is
// composed with the pending attachment path; until then it intentionally has no production caller.
#![allow(dead_code)]

use crate::project_catalog::ProjectIdentity;
use crate::project_disk_lease::{
    ProjectDiskAttachPlan, ProjectDiskAttachmentLease, ProjectDiskGeneration, ProjectDiskId,
    ProjectDiskLeaseError, ProjectDiskLeaseRecord, ProjectDiskLeaseState, ProjectDiskObservation,
    ProjectDiskRevision, ResidentSandboxGeneration, ResidentSandboxId,
};

/// One exact P1 attachment plan bound to the detached lease revision that created it.
///
/// The witness is deliberately crate-private, non-serializable, and non-cloneable. It carries no
/// physical attachment authority by itself. P4 must still add its separately reviewed durable
/// attach-attempt checkpoint, and P2 must freshly prove the resulting host/resident attachment
/// before `ProjectDiskLeaseRecord::record_attach_success` may publish `Attached`.
pub(crate) struct ProjectDiskPendingAttachmentWitness {
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    expected_revision: ProjectDiskRevision,
    attachment: ProjectDiskAttachmentLease,
    attach_plan: ProjectDiskAttachPlan,
}

impl ProjectDiskPendingAttachmentWitness {
    /// Plan one exact pending attachment through P1's existing single-writer rules.
    ///
    /// # Errors
    ///
    /// Returns the existing P1 refusal when the current record is not detached or the supplied
    /// physical observation is not exact, unused, and unlocked.
    pub(crate) fn new(
        record: &ProjectDiskLeaseRecord,
        sandbox_id: ResidentSandboxId,
        sandbox_generation: ResidentSandboxGeneration,
        observation: ProjectDiskObservation,
    ) -> Result<Self, ProjectDiskLeaseError> {
        let attach_plan = record.plan_attach(sandbox_id, sandbox_generation, observation)?;
        let attachment = attach_plan.attachment().clone();
        Ok(Self {
            project: record.project().clone(),
            disk_id: record.disk_id().clone(),
            disk_generation: record.disk_generation(),
            expected_revision: record.revision(),
            attachment,
            attach_plan,
        })
    }

    #[must_use]
    pub(crate) const fn expected_revision(&self) -> ProjectDiskRevision {
        self.expected_revision
    }

    #[must_use]
    pub(crate) const fn attachment(&self) -> &ProjectDiskAttachmentLease {
        &self.attachment
    }

    #[must_use]
    pub(crate) const fn attach_plan(&self) -> &ProjectDiskAttachPlan {
        &self.attach_plan
    }

    /// Return whether the witness still names the exact detached record revision that produced it.
    ///
    /// This is a pure freshness check for later P2/P4 composition. A revision/state/identity drift
    /// expires the witness before any physical result can be accepted through it.
    #[must_use]
    pub(crate) fn matches_current_detached_record(&self, record: &ProjectDiskLeaseRecord) -> bool {
        record.project() == &self.project
            && record.disk_id() == &self.disk_id
            && record.disk_generation() == self.disk_generation
            && record.revision() == self.expected_revision
            && matches!(record.state(), ProjectDiskLeaseState::Detached)
    }
}

#[cfg(test)]
mod tests {
    use super::ProjectDiskPendingAttachmentWitness;
    use crate::project_catalog::ProjectIdentity;
    use crate::project_disk_lease::{
        ProjectDiskGeneration, ProjectDiskId, ProjectDiskLeaseRecord, ProjectDiskLockObservation,
        ProjectDiskObservation, ProjectDiskPhysicalObservation, ProjectDiskRecoverability,
        ProjectDiskUseObservation, ResidentSandboxGeneration, ResidentSandboxId,
    };

    fn detached_record() -> ProjectDiskLeaseRecord {
        ProjectDiskLeaseRecord::new_detached(
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            ProjectDiskId::parse("disk-a").unwrap(),
            ProjectDiskGeneration::new(3).unwrap(),
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

    const fn attached_exact() -> ProjectDiskObservation {
        ProjectDiskObservation::new(
            ProjectDiskPhysicalObservation::Exact,
            ProjectDiskUseObservation::CurrentAttachment,
            ProjectDiskLockObservation::CurrentAttachment,
            ProjectDiskRecoverability::Unknown,
        )
    }

    #[test]
    fn witness_is_created_only_through_the_exact_p1_attach_plan() {
        let record = detached_record();
        let witness = ProjectDiskPendingAttachmentWitness::new(
            &record,
            ResidentSandboxId::parse("sandbox-a").unwrap(),
            ResidentSandboxGeneration::new(11).unwrap(),
            detached_exact(),
        )
        .unwrap();

        assert!(witness.matches_current_detached_record(&record));
        assert_eq!(witness.expected_revision(), record.revision());
        assert_eq!(witness.attachment().generation().get(), 1);
        assert_eq!(
            witness.attach_plan().identity().expected_revision(),
            record.revision()
        );
    }

    #[test]
    fn witness_refuses_inexact_or_in_use_pre_attach_evidence() {
        let record = detached_record();
        let result = ProjectDiskPendingAttachmentWitness::new(
            &record,
            ResidentSandboxId::parse("sandbox-a").unwrap(),
            ResidentSandboxGeneration::new(11).unwrap(),
            ProjectDiskObservation::new(
                ProjectDiskPhysicalObservation::Exact,
                ProjectDiskUseObservation::CurrentAttachment,
                ProjectDiskLockObservation::CurrentAttachment,
                ProjectDiskRecoverability::Unknown,
            ),
        );
        assert!(result.is_err());
    }

    #[test]
    fn witness_expires_when_the_detached_record_changes() {
        let record = detached_record();
        let witness = ProjectDiskPendingAttachmentWitness::new(
            &record,
            ResidentSandboxId::parse("sandbox-a").unwrap(),
            ResidentSandboxGeneration::new(11).unwrap(),
            detached_exact(),
        )
        .unwrap();
        let changed = record.require_revalidation().unwrap();
        assert!(!witness.matches_current_detached_record(&changed));

        let foreign = ProjectDiskLeaseRecord::new_detached(
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            ProjectDiskId::parse("disk-b").unwrap(),
            ProjectDiskGeneration::new(3).unwrap(),
        );
        assert!(!witness.matches_current_detached_record(&foreign));
    }

    #[test]
    fn witness_preserves_monotonic_attachment_generation_after_detach() {
        let first = detached_record();
        let first_plan = first
            .plan_attach(
                ResidentSandboxId::parse("sandbox-a").unwrap(),
                ResidentSandboxGeneration::new(11).unwrap(),
                detached_exact(),
            )
            .unwrap();
        let attached = first
            .record_attach_success(&first_plan, attached_exact())
            .unwrap();
        let detach_plan = attached.plan_detach(attached_exact()).unwrap();
        let detached = attached
            .record_detach_success(&detach_plan, detached_exact())
            .unwrap();

        let witness = ProjectDiskPendingAttachmentWitness::new(
            &detached,
            ResidentSandboxId::parse("sandbox-a").unwrap(),
            ResidentSandboxGeneration::new(11).unwrap(),
            detached_exact(),
        )
        .unwrap();
        assert_eq!(witness.attachment().generation().get(), 2);
    }
}
