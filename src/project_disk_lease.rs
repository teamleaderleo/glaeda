use std::fmt;

use serde::Serialize;

use crate::project_catalog::ProjectIdentity;

pub const PROJECT_DISK_LEASE_SCHEMA_VERSION: u8 = 1;
const MAX_IDENTIFIER_BYTES: usize = 96;

macro_rules! identifier_type {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn parse(value: &str) -> Result<Self, ProjectDiskLeaseError> {
                validate_identifier(value)?;
                Ok(Self(value.to_owned()))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

identifier_type!(ProjectDiskId);
identifier_type!(ResidentSandboxId);

macro_rules! positive_generation_type {
    ($name:ident, $code:literal, $message:literal) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(u64);

        impl $name {
            pub fn new(value: u64) -> Result<Self, ProjectDiskLeaseError> {
                if value == 0 {
                    return Err(error($code, $message));
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
    ProjectDiskGeneration,
    "invalid_project_disk_generation",
    "project disk generation must be greater than zero"
);
positive_generation_type!(
    ProjectDiskRevision,
    "invalid_project_disk_revision",
    "project disk revision must be greater than zero"
);
positive_generation_type!(
    ProjectDiskAttachmentGeneration,
    "invalid_project_disk_attachment_generation",
    "project disk attachment generation must be greater than zero"
);
positive_generation_type!(
    ResidentSandboxGeneration,
    "invalid_resident_sandbox_generation",
    "resident sandbox generation must be greater than zero"
);

impl ProjectDiskRevision {
    fn next(self) -> Result<Self, ProjectDiskLeaseError> {
        Self::new(self.0.checked_add(1).ok_or_else(generation_exhausted)?)
    }
}

impl ProjectDiskAttachmentGeneration {
    fn next(self) -> Result<Self, ProjectDiskLeaseError> {
        Self::new(self.0.checked_add(1).ok_or_else(generation_exhausted)?)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiskAttachmentLease {
    generation: ProjectDiskAttachmentGeneration,
    sandbox_id: ResidentSandboxId,
    sandbox_generation: ResidentSandboxGeneration,
}

impl ProjectDiskAttachmentLease {
    #[must_use]
    pub const fn generation(&self) -> ProjectDiskAttachmentGeneration {
        self.generation
    }

    #[must_use]
    pub fn sandbox_id(&self) -> &ResidentSandboxId {
        &self.sandbox_id
    }

    #[must_use]
    pub const fn sandbox_generation(&self) -> ResidentSandboxGeneration {
        self.sandbox_generation
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProjectDiskLeaseState {
    Detached,
    Attached {
        attachment: ProjectDiskAttachmentLease,
    },
    RevalidateRequired {
        #[serde(skip_serializing_if = "Option::is_none")]
        attachment: Option<ProjectDiskAttachmentLease>,
    },
    UnlockRequired {
        predecessor: ProjectDiskAttachmentLease,
    },
    Quarantined,
    RetireRequested,
    Retired,
}

impl ProjectDiskLeaseState {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Retired)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDiskPhysicalObservation {
    Exact,
    Absent,
    Foreign,
    Conflicting,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDiskUseObservation {
    Unused,
    CurrentAttachment,
    Other,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDiskLockObservation {
    Unlocked,
    CurrentAttachment,
    ExpectedPredecessor,
    Other,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDiskRecoverability {
    Rebuildable,
    UniqueLocalWork,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProjectDiskObservation {
    physical: ProjectDiskPhysicalObservation,
    use_state: ProjectDiskUseObservation,
    lock_state: ProjectDiskLockObservation,
    recoverability: ProjectDiskRecoverability,
}

impl ProjectDiskObservation {
    #[must_use]
    pub const fn new(
        physical: ProjectDiskPhysicalObservation,
        use_state: ProjectDiskUseObservation,
        lock_state: ProjectDiskLockObservation,
        recoverability: ProjectDiskRecoverability,
    ) -> Self {
        Self {
            physical,
            use_state,
            lock_state,
            recoverability,
        }
    }

    #[must_use]
    pub const fn physical(self) -> ProjectDiskPhysicalObservation {
        self.physical
    }

    #[must_use]
    pub const fn use_state(self) -> ProjectDiskUseObservation {
        self.use_state
    }

    #[must_use]
    pub const fn lock_state(self) -> ProjectDiskLockObservation {
        self.lock_state
    }

    #[must_use]
    pub const fn recoverability(self) -> ProjectDiskRecoverability {
        self.recoverability
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiskLeaseRecord {
    schema_version: u8,
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    revision: ProjectDiskRevision,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_attachment_generation: Option<ProjectDiskAttachmentGeneration>,
    state: ProjectDiskLeaseState,
}

impl ProjectDiskLeaseRecord {
    /// Create the lease for one already-created, already-formatted, detached project disk.
    #[must_use]
    pub fn new_detached(
        project: ProjectIdentity,
        disk_id: ProjectDiskId,
        disk_generation: ProjectDiskGeneration,
    ) -> Self {
        Self {
            schema_version: PROJECT_DISK_LEASE_SCHEMA_VERSION,
            project,
            disk_id,
            disk_generation,
            revision: ProjectDiskRevision(1),
            last_attachment_generation: None,
            state: ProjectDiskLeaseState::Detached,
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
    pub const fn revision(&self) -> ProjectDiskRevision {
        self.revision
    }

    #[must_use]
    pub const fn last_attachment_generation(&self) -> Option<ProjectDiskAttachmentGeneration> {
        self.last_attachment_generation
    }

    #[must_use]
    pub const fn state(&self) -> &ProjectDiskLeaseState {
        &self.state
    }

    /// Plan one writable attachment from a proven detached disk.
    pub fn plan_attach(
        &self,
        sandbox_id: ResidentSandboxId,
        sandbox_generation: ResidentSandboxGeneration,
        observation: ProjectDiskObservation,
    ) -> Result<ProjectDiskAttachPlan, ProjectDiskLeaseError> {
        self.require_nonterminal()?;
        if !matches!(self.state, ProjectDiskLeaseState::Detached) {
            return Err(invalid_state(
                "project_disk_attach_requires_detached",
                "project disk attach requires detached state",
            ));
        }
        require_exact_unused_unlocked(observation)?;
        let generation = self.next_attachment_generation()?;
        Ok(ProjectDiskAttachPlan {
            identity: self.plan_identity(),
            attachment: ProjectDiskAttachmentLease {
                generation,
                sandbox_id,
                sandbox_generation,
            },
        })
    }

    /// Accept the durable successor after the planned attachment is freshly observed.
    pub fn record_attach_success(
        &self,
        plan: &ProjectDiskAttachPlan,
        post: ProjectDiskObservation,
    ) -> Result<Self, ProjectDiskLeaseError> {
        self.require_plan_identity(&plan.identity)?;
        if !matches!(self.state, ProjectDiskLeaseState::Detached) {
            return Err(invalid_state(
                "project_disk_attach_requires_detached",
                "project disk attach requires detached state",
            ));
        }
        if plan.attachment.generation != self.next_attachment_generation()? {
            return Err(plan_mismatch());
        }
        require_exact_current_attachment(post)?;
        self.successor(
            ProjectDiskLeaseState::Attached {
                attachment: plan.attachment.clone(),
            },
            Some(plan.attachment.generation),
        )
    }

    /// Plan detachment only for the exact current writable attachment.
    pub fn plan_detach(
        &self,
        observation: ProjectDiskObservation,
    ) -> Result<ProjectDiskDetachPlan, ProjectDiskLeaseError> {
        self.require_nonterminal()?;
        let ProjectDiskLeaseState::Attached { attachment } = &self.state else {
            return Err(invalid_state(
                "project_disk_detach_requires_attached",
                "project disk detach requires attached state",
            ));
        };
        require_exact_current_attachment(observation)?;
        Ok(ProjectDiskDetachPlan {
            identity: self.plan_identity(),
            attachment: attachment.clone(),
        })
    }

    /// Accept detached state only after the exact disk is freshly observed unused and unlocked.
    pub fn record_detach_success(
        &self,
        plan: &ProjectDiskDetachPlan,
        post: ProjectDiskObservation,
    ) -> Result<Self, ProjectDiskLeaseError> {
        self.require_plan_identity(&plan.identity)?;
        let ProjectDiskLeaseState::Attached { attachment } = &self.state else {
            return Err(invalid_state(
                "project_disk_detach_requires_attached",
                "project disk detach requires attached state",
            ));
        };
        if attachment != &plan.attachment {
            return Err(plan_mismatch());
        }
        require_exact_unused_unlocked(post)?;
        self.successor(
            ProjectDiskLeaseState::Detached,
            self.last_attachment_generation,
        )
    }

    /// Reconcile one previously attached disk after controller/VM interruption.
    ///
    /// Exact current-use evidence keeps the attachment. Proven unused/unlocked evidence closes it.
    /// A proven stale predecessor lock enters `unlock_required`. Foreign/conflicting physical state
    /// quarantines the lease. Remaining ambiguity enters `revalidate_required` and authorizes no
    /// external mutation.
    pub fn reconcile_attached_observation(
        &self,
        observation: ProjectDiskObservation,
    ) -> Result<Self, ProjectDiskLeaseError> {
        self.require_nonterminal()?;
        let ProjectDiskLeaseState::Attached { attachment } = &self.state else {
            return Err(invalid_state(
                "project_disk_reconcile_requires_attached",
                "attached recovery requires attached state",
            ));
        };

        match observation.physical {
            ProjectDiskPhysicalObservation::Foreign
            | ProjectDiskPhysicalObservation::Conflicting => self.successor(
                ProjectDiskLeaseState::Quarantined,
                self.last_attachment_generation,
            ),
            ProjectDiskPhysicalObservation::Exact
                if observation.use_state == ProjectDiskUseObservation::CurrentAttachment
                    && observation.lock_state == ProjectDiskLockObservation::CurrentAttachment =>
            {
                Ok(self.clone())
            }
            ProjectDiskPhysicalObservation::Exact
                if observation.use_state == ProjectDiskUseObservation::Unused
                    && observation.lock_state == ProjectDiskLockObservation::Unlocked =>
            {
                self.successor(
                    ProjectDiskLeaseState::Detached,
                    self.last_attachment_generation,
                )
            }
            ProjectDiskPhysicalObservation::Exact
                if observation.use_state == ProjectDiskUseObservation::Unused
                    && observation.lock_state
                        == ProjectDiskLockObservation::ExpectedPredecessor =>
            {
                self.successor(
                    ProjectDiskLeaseState::UnlockRequired {
                        predecessor: attachment.clone(),
                    },
                    self.last_attachment_generation,
                )
            }
            _ => self.successor(
                ProjectDiskLeaseState::RevalidateRequired {
                    attachment: Some(attachment.clone()),
                },
                self.last_attachment_generation,
            ),
        }
    }

    /// Force fresh revalidation after a policy/generation parent changes.
    pub fn require_revalidation(&self) -> Result<Self, ProjectDiskLeaseError> {
        self.require_nonterminal()?;
        let attachment = match &self.state {
            ProjectDiskLeaseState::Attached { attachment } => Some(attachment.clone()),
            ProjectDiskLeaseState::Detached => None,
            ProjectDiskLeaseState::RevalidateRequired { .. } => return Ok(self.clone()),
            _ => {
                return Err(invalid_state(
                    "project_disk_revalidation_transition_invalid",
                    "current project disk state cannot enter revalidation",
                ));
            }
        };
        self.successor(
            ProjectDiskLeaseState::RevalidateRequired { attachment },
            self.last_attachment_generation,
        )
    }

    /// Resolve a revalidation state only from conclusive current observations.
    pub fn record_revalidation(
        &self,
        observation: ProjectDiskObservation,
    ) -> Result<Self, ProjectDiskLeaseError> {
        self.require_nonterminal()?;
        let ProjectDiskLeaseState::RevalidateRequired { attachment } = &self.state else {
            return Err(invalid_state(
                "project_disk_revalidation_required",
                "project disk is not awaiting revalidation",
            ));
        };

        match observation.physical {
            ProjectDiskPhysicalObservation::Foreign
            | ProjectDiskPhysicalObservation::Conflicting => self.successor(
                ProjectDiskLeaseState::Quarantined,
                self.last_attachment_generation,
            ),
            ProjectDiskPhysicalObservation::Exact
                if observation.use_state == ProjectDiskUseObservation::Unused
                    && observation.lock_state == ProjectDiskLockObservation::Unlocked =>
            {
                self.successor(
                    ProjectDiskLeaseState::Detached,
                    self.last_attachment_generation,
                )
            }
            ProjectDiskPhysicalObservation::Exact
                if attachment.is_some()
                    && observation.use_state == ProjectDiskUseObservation::CurrentAttachment
                    && observation.lock_state == ProjectDiskLockObservation::CurrentAttachment =>
            {
                self.successor(
                    ProjectDiskLeaseState::Attached {
                        attachment: attachment.clone().expect("checked above"),
                    },
                    self.last_attachment_generation,
                )
            }
            ProjectDiskPhysicalObservation::Exact
                if attachment.is_some()
                    && observation.use_state == ProjectDiskUseObservation::Unused
                    && observation.lock_state
                        == ProjectDiskLockObservation::ExpectedPredecessor =>
            {
                self.successor(
                    ProjectDiskLeaseState::UnlockRequired {
                        predecessor: attachment.clone().expect("checked above"),
                    },
                    self.last_attachment_generation,
                )
            }
            _ => Err(error(
                "project_disk_revalidation_inconclusive",
                "project disk revalidation remains inconclusive",
            )),
        }
    }

    /// Plan Lima stale-lock recovery only for the exact predecessor lock on a proven unused disk.
    pub fn plan_unlock(
        &self,
        observation: ProjectDiskObservation,
    ) -> Result<ProjectDiskUnlockPlan, ProjectDiskLeaseError> {
        self.require_nonterminal()?;
        let ProjectDiskLeaseState::UnlockRequired { predecessor } = &self.state else {
            return Err(invalid_state(
                "project_disk_unlock_requires_stale_predecessor",
                "project disk unlock requires an accepted stale predecessor state",
            ));
        };
        if observation.physical != ProjectDiskPhysicalObservation::Exact
            || observation.use_state != ProjectDiskUseObservation::Unused
            || observation.lock_state != ProjectDiskLockObservation::ExpectedPredecessor
        {
            return Err(error(
                "project_disk_unlock_observation_invalid",
                "project disk unlock requires exact, unused, expected-predecessor evidence",
            ));
        }
        Ok(ProjectDiskUnlockPlan {
            identity: self.plan_identity(),
            predecessor: predecessor.clone(),
        })
    }

    /// Accept stale-lock recovery only after the exact disk is freshly observed unused/unlocked.
    pub fn record_unlock_success(
        &self,
        plan: &ProjectDiskUnlockPlan,
        post: ProjectDiskObservation,
    ) -> Result<Self, ProjectDiskLeaseError> {
        self.require_plan_identity(&plan.identity)?;
        let ProjectDiskLeaseState::UnlockRequired { predecessor } = &self.state else {
            return Err(invalid_state(
                "project_disk_unlock_requires_stale_predecessor",
                "project disk unlock requires an accepted stale predecessor state",
            ));
        };
        if predecessor != &plan.predecessor {
            return Err(plan_mismatch());
        }
        require_exact_unused_unlocked(post)?;
        self.successor(
            ProjectDiskLeaseState::Detached,
            self.last_attachment_generation,
        )
    }

    /// Record intent to retire a detached project disk before any destructive external mutation.
    pub fn request_retire(&self) -> Result<Self, ProjectDiskLeaseError> {
        self.require_nonterminal()?;
        if !matches!(self.state, ProjectDiskLeaseState::Detached) {
            return Err(invalid_state(
                "project_disk_retire_requires_detached",
                "project disk retirement requires detached state",
            ));
        }
        self.successor(
            ProjectDiskLeaseState::RetireRequested,
            self.last_attachment_generation,
        )
    }

    /// Authorize either exact physical deletion or completion of already-proven absence.
    pub fn plan_retire(
        &self,
        observation: ProjectDiskObservation,
    ) -> Result<ProjectDiskRetirePlan, ProjectDiskLeaseError> {
        self.require_nonterminal()?;
        if !matches!(self.state, ProjectDiskLeaseState::RetireRequested) {
            return Err(invalid_state(
                "project_disk_retire_request_required",
                "project disk retirement must be requested before physical retirement",
            ));
        }
        if observation.recoverability != ProjectDiskRecoverability::Rebuildable {
            return Err(error(
                "project_disk_retire_recoverability_blocked",
                "project disk retirement requires proven rebuildable state",
            ));
        }
        let action = match observation.physical {
            ProjectDiskPhysicalObservation::Absent => ProjectDiskRetireAction::CompleteAbsent,
            ProjectDiskPhysicalObservation::Exact
                if observation.use_state == ProjectDiskUseObservation::Unused
                    && observation.lock_state == ProjectDiskLockObservation::Unlocked =>
            {
                ProjectDiskRetireAction::DeleteExact
            }
            ProjectDiskPhysicalObservation::Foreign
            | ProjectDiskPhysicalObservation::Conflicting
            | ProjectDiskPhysicalObservation::Unknown => return Err(protected_physical_state()),
            _ => {
                return Err(error(
                    "project_disk_retire_observation_invalid",
                    "project disk retirement requires absence or exact unused unlocked evidence",
                ));
            }
        };
        Ok(ProjectDiskRetirePlan {
            identity: self.plan_identity(),
            action,
        })
    }

    /// Accept terminal retirement only after the physical result is freshly observed absent.
    pub fn record_retire_success(
        &self,
        plan: &ProjectDiskRetirePlan,
        post: ProjectDiskObservation,
    ) -> Result<Self, ProjectDiskLeaseError> {
        self.require_plan_identity(&plan.identity)?;
        if !matches!(self.state, ProjectDiskLeaseState::RetireRequested) {
            return Err(invalid_state(
                "project_disk_retire_request_required",
                "project disk retirement must be requested before physical retirement",
            ));
        }
        if post.physical != ProjectDiskPhysicalObservation::Absent {
            return Err(error(
                "project_disk_retire_absence_unproven",
                "project disk retirement requires fresh physical absence",
            ));
        }
        self.successor(
            ProjectDiskLeaseState::Retired,
            self.last_attachment_generation,
        )
    }

    fn next_attachment_generation(
        &self,
    ) -> Result<ProjectDiskAttachmentGeneration, ProjectDiskLeaseError> {
        match self.last_attachment_generation {
            Some(generation) => generation.next(),
            None => ProjectDiskAttachmentGeneration::new(1),
        }
    }

    fn plan_identity(&self) -> ProjectDiskPlanIdentity {
        ProjectDiskPlanIdentity {
            project: self.project.clone(),
            disk_id: self.disk_id.clone(),
            disk_generation: self.disk_generation,
            expected_revision: self.revision,
        }
    }

    fn require_plan_identity(
        &self,
        identity: &ProjectDiskPlanIdentity,
    ) -> Result<(), ProjectDiskLeaseError> {
        if identity.project != self.project
            || identity.disk_id != self.disk_id
            || identity.disk_generation != self.disk_generation
        {
            return Err(plan_mismatch());
        }
        if identity.expected_revision != self.revision {
            return Err(error(
                "stale_project_disk_plan",
                "project disk plan revision is stale",
            ));
        }
        Ok(())
    }

    fn require_nonterminal(&self) -> Result<(), ProjectDiskLeaseError> {
        if self.state.is_terminal() {
            return Err(error(
                "project_disk_terminal",
                "retired project disk state is terminal",
            ));
        }
        Ok(())
    }

    fn successor(
        &self,
        state: ProjectDiskLeaseState,
        last_attachment_generation: Option<ProjectDiskAttachmentGeneration>,
    ) -> Result<Self, ProjectDiskLeaseError> {
        Ok(Self {
            schema_version: self.schema_version,
            project: self.project.clone(),
            disk_id: self.disk_id.clone(),
            disk_generation: self.disk_generation,
            revision: self.revision.next()?,
            last_attachment_generation,
            state,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiskPlanIdentity {
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    expected_revision: ProjectDiskRevision,
}

impl ProjectDiskPlanIdentity {
    #[must_use]
    pub const fn expected_revision(&self) -> ProjectDiskRevision {
        self.expected_revision
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiskAttachPlan {
    identity: ProjectDiskPlanIdentity,
    attachment: ProjectDiskAttachmentLease,
}

impl ProjectDiskAttachPlan {
    #[must_use]
    pub const fn identity(&self) -> &ProjectDiskPlanIdentity {
        &self.identity
    }

    #[must_use]
    pub const fn attachment(&self) -> &ProjectDiskAttachmentLease {
        &self.attachment
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiskDetachPlan {
    identity: ProjectDiskPlanIdentity,
    attachment: ProjectDiskAttachmentLease,
}

impl ProjectDiskDetachPlan {
    #[must_use]
    pub const fn identity(&self) -> &ProjectDiskPlanIdentity {
        &self.identity
    }

    #[must_use]
    pub const fn attachment(&self) -> &ProjectDiskAttachmentLease {
        &self.attachment
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiskUnlockPlan {
    identity: ProjectDiskPlanIdentity,
    predecessor: ProjectDiskAttachmentLease,
}

impl ProjectDiskUnlockPlan {
    #[must_use]
    pub const fn identity(&self) -> &ProjectDiskPlanIdentity {
        &self.identity
    }

    #[must_use]
    pub const fn predecessor(&self) -> &ProjectDiskAttachmentLease {
        &self.predecessor
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDiskRetireAction {
    DeleteExact,
    CompleteAbsent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiskRetirePlan {
    identity: ProjectDiskPlanIdentity,
    action: ProjectDiskRetireAction,
}

impl ProjectDiskRetirePlan {
    #[must_use]
    pub const fn identity(&self) -> &ProjectDiskPlanIdentity {
        &self.identity
    }

    #[must_use]
    pub const fn action(&self) -> ProjectDiskRetireAction {
        self.action
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDiskLeaseError {
    code: &'static str,
    message: &'static str,
}

impl ProjectDiskLeaseError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for ProjectDiskLeaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ProjectDiskLeaseError {}

fn validate_identifier(value: &str) -> Result<(), ProjectDiskLeaseError> {
    let Some(first) = value.bytes().next() else {
        return Err(invalid_identifier());
    };
    if value.len() > MAX_IDENTIFIER_BYTES
        || !(first.is_ascii_lowercase() || first.is_ascii_digit())
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'_' | b'-' | b':')
        })
    {
        return Err(invalid_identifier());
    }
    Ok(())
}

fn require_exact_unused_unlocked(
    observation: ProjectDiskObservation,
) -> Result<(), ProjectDiskLeaseError> {
    if observation.physical != ProjectDiskPhysicalObservation::Exact {
        return Err(protected_physical_state());
    }
    if observation.use_state != ProjectDiskUseObservation::Unused
        || observation.lock_state != ProjectDiskLockObservation::Unlocked
    {
        return Err(error(
            "project_disk_not_detached",
            "project disk must be proven unused and unlocked",
        ));
    }
    Ok(())
}

fn require_exact_current_attachment(
    observation: ProjectDiskObservation,
) -> Result<(), ProjectDiskLeaseError> {
    if observation.physical != ProjectDiskPhysicalObservation::Exact {
        return Err(protected_physical_state());
    }
    if observation.use_state != ProjectDiskUseObservation::CurrentAttachment
        || observation.lock_state != ProjectDiskLockObservation::CurrentAttachment
    {
        return Err(error(
            "project_disk_current_attachment_unproven",
            "project disk current attachment is not proven",
        ));
    }
    Ok(())
}

const fn error(code: &'static str, message: &'static str) -> ProjectDiskLeaseError {
    ProjectDiskLeaseError { code, message }
}

const fn invalid_state(code: &'static str, message: &'static str) -> ProjectDiskLeaseError {
    error(code, message)
}

const fn invalid_identifier() -> ProjectDiskLeaseError {
    error(
        "invalid_project_disk_identifier",
        "project disk identifier must be a bounded lowercase ASCII token",
    )
}

const fn generation_exhausted() -> ProjectDiskLeaseError {
    error(
        "project_disk_generation_exhausted",
        "project disk generation counter is exhausted",
    )
}

const fn plan_mismatch() -> ProjectDiskLeaseError {
    error(
        "project_disk_plan_mismatch",
        "project disk plan does not match the current lease",
    )
}

const fn protected_physical_state() -> ProjectDiskLeaseError {
    error(
        "project_disk_physical_state_protected",
        "foreign, conflicting, absent, or unknown project disk state cannot authorize this mutation",
    )
}

#[cfg(test)]
mod tests {
    use super::{
        ProjectDiskAttachmentGeneration, ProjectDiskGeneration, ProjectDiskId,
        ProjectDiskLeaseRecord, ProjectDiskLeaseState, ProjectDiskLockObservation,
        ProjectDiskObservation, ProjectDiskPhysicalObservation, ProjectDiskRecoverability,
        ProjectDiskRetireAction, ProjectDiskUseObservation, ResidentSandboxGeneration,
        ResidentSandboxId,
    };
    use crate::project_catalog::ProjectIdentity;

    fn project() -> ProjectIdentity {
        ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").expect("project is valid")
    }

    fn record() -> ProjectDiskLeaseRecord {
        ProjectDiskLeaseRecord::new_detached(
            project(),
            ProjectDiskId::parse("smolrunner-project-disk").expect("disk ID is valid"),
            ProjectDiskGeneration::new(1).expect("generation is valid"),
        )
    }

    fn observation(
        physical: ProjectDiskPhysicalObservation,
        use_state: ProjectDiskUseObservation,
        lock_state: ProjectDiskLockObservation,
        recoverability: ProjectDiskRecoverability,
    ) -> ProjectDiskObservation {
        ProjectDiskObservation::new(physical, use_state, lock_state, recoverability)
    }

    fn exact_unused_unlocked() -> ProjectDiskObservation {
        observation(
            ProjectDiskPhysicalObservation::Exact,
            ProjectDiskUseObservation::Unused,
            ProjectDiskLockObservation::Unlocked,
            ProjectDiskRecoverability::Rebuildable,
        )
    }

    fn exact_current_attachment() -> ProjectDiskObservation {
        observation(
            ProjectDiskPhysicalObservation::Exact,
            ProjectDiskUseObservation::CurrentAttachment,
            ProjectDiskLockObservation::CurrentAttachment,
            ProjectDiskRecoverability::Rebuildable,
        )
    }

    fn attach(record: &ProjectDiskLeaseRecord, sandbox: &str) -> ProjectDiskLeaseRecord {
        let plan = record
            .plan_attach(
                ResidentSandboxId::parse(sandbox).expect("sandbox ID is valid"),
                ResidentSandboxGeneration::new(1).expect("sandbox generation is valid"),
                exact_unused_unlocked(),
            )
            .expect("attach plan is valid");
        record
            .record_attach_success(&plan, exact_current_attachment())
            .expect("attachment is recorded")
    }

    #[test]
    fn one_writer_attachment_generation_is_monotonic() {
        let initial = record();
        assert_eq!(initial.revision().get(), 1);
        let first_plan = initial
            .plan_attach(
                ResidentSandboxId::parse("sandbox-a").unwrap(),
                ResidentSandboxGeneration::new(1).unwrap(),
                exact_unused_unlocked(),
            )
            .expect("first attach plans");
        assert_eq!(first_plan.attachment().generation().get(), 1);
        let attached = initial
            .record_attach_success(&first_plan, exact_current_attachment())
            .expect("first attach records");
        assert!(matches!(
            attached.state(),
            ProjectDiskLeaseState::Attached { .. }
        ));
        assert_eq!(attached.last_attachment_generation().unwrap().get(), 1);
        assert_eq!(
            attached
                .plan_attach(
                    ResidentSandboxId::parse("sandbox-b").unwrap(),
                    ResidentSandboxGeneration::new(1).unwrap(),
                    exact_unused_unlocked(),
                )
                .expect_err("second writer is refused")
                .code(),
            "project_disk_attach_requires_detached"
        );

        let detach = attached
            .plan_detach(exact_current_attachment())
            .expect("detach plans");
        let detached = attached
            .record_detach_success(&detach, exact_unused_unlocked())
            .expect("detach records");
        let second = detached
            .plan_attach(
                ResidentSandboxId::parse("sandbox-b").unwrap(),
                ResidentSandboxGeneration::new(2).unwrap(),
                exact_unused_unlocked(),
            )
            .expect("second attach plans");
        assert_eq!(second.attachment().generation().get(), 2);
    }

    #[test]
    fn stale_attach_plan_is_rejected_after_revision_moves() {
        let initial = record();
        let attach = initial
            .plan_attach(
                ResidentSandboxId::parse("sandbox-a").unwrap(),
                ResidentSandboxGeneration::new(1).unwrap(),
                exact_unused_unlocked(),
            )
            .expect("attach plans");
        let retiring = initial
            .request_retire()
            .expect("retire request advances revision");
        assert_eq!(
            retiring
                .record_attach_success(&attach, exact_current_attachment())
                .expect_err("stale attach plan is refused")
                .code(),
            "stale_project_disk_plan"
        );
    }

    #[test]
    fn attached_crash_with_stale_predecessor_lock_enters_unlock_required() {
        let attached = attach(&record(), "sandbox-a");
        let stale_lock = observation(
            ProjectDiskPhysicalObservation::Exact,
            ProjectDiskUseObservation::Unused,
            ProjectDiskLockObservation::ExpectedPredecessor,
            ProjectDiskRecoverability::Rebuildable,
        );
        let recovery = attached
            .reconcile_attached_observation(stale_lock)
            .expect("stale owned lock is classified");
        let ProjectDiskLeaseState::UnlockRequired { predecessor } = recovery.state() else {
            panic!("expected unlock_required");
        };
        assert_eq!(predecessor.generation().get(), 1);

        let unlock = recovery.plan_unlock(stale_lock).expect("unlock plans");
        let detached = recovery
            .record_unlock_success(&unlock, exact_unused_unlocked())
            .expect("unlock records only after fresh unlocked observation");
        assert!(matches!(detached.state(), ProjectDiskLeaseState::Detached));
    }

    #[test]
    fn current_or_foreign_lock_does_not_authorize_unlock() {
        let attached = attach(&record(), "sandbox-a");
        let stale_lock = observation(
            ProjectDiskPhysicalObservation::Exact,
            ProjectDiskUseObservation::Unused,
            ProjectDiskLockObservation::ExpectedPredecessor,
            ProjectDiskRecoverability::Rebuildable,
        );
        let recovery = attached.reconcile_attached_observation(stale_lock).unwrap();
        for lock in [
            ProjectDiskLockObservation::Unlocked,
            ProjectDiskLockObservation::CurrentAttachment,
            ProjectDiskLockObservation::Other,
            ProjectDiskLockObservation::Unknown,
        ] {
            let observed = observation(
                ProjectDiskPhysicalObservation::Exact,
                ProjectDiskUseObservation::Unused,
                lock,
                ProjectDiskRecoverability::Rebuildable,
            );
            assert_eq!(
                recovery
                    .plan_unlock(observed)
                    .expect_err("only expected predecessor lock authorizes unlock")
                    .code(),
                "project_disk_unlock_observation_invalid"
            );
        }
    }

    #[test]
    fn foreign_or_conflicting_attached_disk_is_quarantined() {
        for physical in [
            ProjectDiskPhysicalObservation::Foreign,
            ProjectDiskPhysicalObservation::Conflicting,
        ] {
            let attached = attach(&record(), "sandbox-a");
            let next = attached
                .reconcile_attached_observation(observation(
                    physical,
                    ProjectDiskUseObservation::Unknown,
                    ProjectDiskLockObservation::Unknown,
                    ProjectDiskRecoverability::Unknown,
                ))
                .expect("foreign evidence is recorded as quarantine");
            assert!(matches!(next.state(), ProjectDiskLeaseState::Quarantined));
        }
    }

    #[test]
    fn unknown_attached_state_requires_revalidation_and_authorizes_nothing() {
        let attached = attach(&record(), "sandbox-a");
        let revalidate = attached
            .reconcile_attached_observation(observation(
                ProjectDiskPhysicalObservation::Unknown,
                ProjectDiskUseObservation::Unknown,
                ProjectDiskLockObservation::Unknown,
                ProjectDiskRecoverability::Unknown,
            ))
            .expect("ambiguity enters revalidation");
        assert!(matches!(
            revalidate.state(),
            ProjectDiskLeaseState::RevalidateRequired {
                attachment: Some(_)
            }
        ));
        assert_eq!(
            revalidate
                .plan_unlock(observation(
                    ProjectDiskPhysicalObservation::Exact,
                    ProjectDiskUseObservation::Unused,
                    ProjectDiskLockObservation::ExpectedPredecessor,
                    ProjectDiskRecoverability::Rebuildable,
                ))
                .expect_err("revalidation state cannot unlock")
                .code(),
            "project_disk_unlock_requires_stale_predecessor"
        );
    }

    #[test]
    fn revalidation_can_restore_attached_or_detached_state() {
        let attached = attach(&record(), "sandbox-a");
        let revalidate = attached
            .require_revalidation()
            .expect("policy can require revalidation");
        let restored = revalidate
            .record_revalidation(exact_current_attachment())
            .expect("exact current attachment restores attached state");
        assert!(matches!(
            restored.state(),
            ProjectDiskLeaseState::Attached { .. }
        ));

        let revalidate = restored.require_revalidation().unwrap();
        let detached = revalidate
            .record_revalidation(exact_unused_unlocked())
            .expect("exact unused/unlocked restores detached state");
        assert!(matches!(detached.state(), ProjectDiskLeaseState::Detached));
    }

    #[test]
    fn retire_blocks_unique_or_unknown_local_work() {
        for recoverability in [
            ProjectDiskRecoverability::UniqueLocalWork,
            ProjectDiskRecoverability::Unknown,
        ] {
            let retiring = record().request_retire().unwrap();
            let error = retiring
                .plan_retire(observation(
                    ProjectDiskPhysicalObservation::Exact,
                    ProjectDiskUseObservation::Unused,
                    ProjectDiskLockObservation::Unlocked,
                    recoverability,
                ))
                .expect_err("unrecoverable state blocks retirement");
            assert_eq!(error.code(), "project_disk_retire_recoverability_blocked");
        }
    }

    #[test]
    fn retire_distinguishes_exact_delete_from_already_absent() {
        let retiring = record().request_retire().unwrap();
        let delete = retiring
            .plan_retire(exact_unused_unlocked())
            .expect("exact detached disk may be deleted");
        assert_eq!(delete.action(), ProjectDiskRetireAction::DeleteExact);

        let absent = observation(
            ProjectDiskPhysicalObservation::Absent,
            ProjectDiskUseObservation::Unknown,
            ProjectDiskLockObservation::Unknown,
            ProjectDiskRecoverability::Rebuildable,
        );
        let complete = retiring
            .plan_retire(absent)
            .expect("proven absence completes without delete");
        assert_eq!(complete.action(), ProjectDiskRetireAction::CompleteAbsent);
        let retired = retiring
            .record_retire_success(&complete, absent)
            .expect("terminal absence records retirement");
        assert!(matches!(retired.state(), ProjectDiskLeaseState::Retired));
        assert!(retired.state().is_terminal());
    }

    #[test]
    fn foreign_conflicting_and_unknown_physical_state_authorize_no_mutation() {
        for physical in [
            ProjectDiskPhysicalObservation::Foreign,
            ProjectDiskPhysicalObservation::Conflicting,
            ProjectDiskPhysicalObservation::Unknown,
        ] {
            let initial = record();
            let observed = observation(
                physical,
                ProjectDiskUseObservation::Unused,
                ProjectDiskLockObservation::Unlocked,
                ProjectDiskRecoverability::Rebuildable,
            );
            assert_eq!(
                initial
                    .plan_attach(
                        ResidentSandboxId::parse("sandbox-a").unwrap(),
                        ResidentSandboxGeneration::new(1).unwrap(),
                        observed,
                    )
                    .expect_err("protected physical state cannot attach")
                    .code(),
                "project_disk_physical_state_protected"
            );
            let retiring = initial.request_retire().unwrap();
            assert_eq!(
                retiring
                    .plan_retire(observed)
                    .expect_err("protected physical state cannot retire")
                    .code(),
                "project_disk_physical_state_protected"
            );
        }
    }

    #[test]
    fn retired_state_is_terminal() {
        let retiring = record().request_retire().unwrap();
        let absent = observation(
            ProjectDiskPhysicalObservation::Absent,
            ProjectDiskUseObservation::Unknown,
            ProjectDiskLockObservation::Unknown,
            ProjectDiskRecoverability::Rebuildable,
        );
        let plan = retiring.plan_retire(absent).unwrap();
        let retired = retiring.record_retire_success(&plan, absent).unwrap();
        assert_eq!(
            retired
                .request_retire()
                .expect_err("retired state is terminal")
                .code(),
            "project_disk_terminal"
        );
    }

    #[test]
    fn identifiers_and_generations_are_bounded_without_echoing_private_input() {
        let private_path = "/private/project/disk";
        let error = ProjectDiskId::parse(private_path).expect_err("path-like ID is refused");
        assert_eq!(error.code(), "invalid_project_disk_identifier");
        assert!(!error.to_string().contains(private_path));
        assert!(!format!("{error:?}").contains(private_path));
        assert_eq!(
            ProjectDiskAttachmentGeneration::new(0)
                .expect_err("zero generation is refused")
                .code(),
            "invalid_project_disk_attachment_generation"
        );
    }
}
