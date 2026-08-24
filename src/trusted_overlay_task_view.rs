use std::collections::BTreeSet;
use std::fmt;

use serde::Serialize;

use crate::artifact::{CommitId, GitTreeId, Sha256Digest};
use crate::project_catalog::ProjectIdentity;
use crate::project_disk_lease::{
    ProjectDiskGeneration, ProjectDiskId, ResidentSandboxGeneration, ResidentSandboxId,
};

pub const TRUSTED_OVERLAY_TASK_VIEW_SCHEMA_VERSION: u8 = 1;
pub const MAX_OVERLAY_SOURCE_ANCHOR_TASKS: usize = 256;
const MAX_IDENTIFIER_BYTES: usize = 96;

macro_rules! identifier_type {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn parse(value: &str) -> Result<Self, TrustedOverlayTaskViewError> {
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

identifier_type!(OverlaySourceAnchorId);
identifier_type!(OverlayTaskViewId);

macro_rules! positive_generation_type {
    ($name:ident, $code:literal, $message:literal) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(u64);

        impl $name {
            pub fn new(value: u64) -> Result<Self, TrustedOverlayTaskViewError> {
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
    OverlaySourceAnchorGeneration,
    "invalid_overlay_source_anchor_generation",
    "overlay source-anchor generation must be greater than zero"
);
positive_generation_type!(
    OverlayTaskViewGeneration,
    "invalid_overlay_task_view_generation",
    "overlay task-view generation must be greater than zero"
);
positive_generation_type!(
    OverlayTaskViewRevision,
    "invalid_overlay_task_view_revision",
    "overlay task-view revision must be greater than zero"
);

impl OverlayTaskViewRevision {
    fn next(self) -> Result<Self, TrustedOverlayTaskViewError> {
        Self::new(self.0.checked_add(1).ok_or_else(generation_exhausted)?)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OverlaySourceAnchorBinding {
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    resident_sandbox_id: ResidentSandboxId,
    resident_sandbox_generation: ResidentSandboxGeneration,
    anchor_id: OverlaySourceAnchorId,
    anchor_generation: OverlaySourceAnchorGeneration,
    commit: CommitId,
    tree: GitTreeId,
    source_index_digest: Sha256Digest,
}

impl OverlaySourceAnchorBinding {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        project: ProjectIdentity,
        disk_id: ProjectDiskId,
        disk_generation: ProjectDiskGeneration,
        resident_sandbox_id: ResidentSandboxId,
        resident_sandbox_generation: ResidentSandboxGeneration,
        anchor_id: OverlaySourceAnchorId,
        anchor_generation: OverlaySourceAnchorGeneration,
        commit: CommitId,
        tree: GitTreeId,
        source_index_digest: Sha256Digest,
    ) -> Self {
        Self {
            project,
            disk_id,
            disk_generation,
            resident_sandbox_id,
            resident_sandbox_generation,
            anchor_id,
            anchor_generation,
            commit,
            tree,
            source_index_digest,
        }
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
    pub const fn resident_sandbox_id(&self) -> &ResidentSandboxId {
        &self.resident_sandbox_id
    }

    #[must_use]
    pub const fn resident_sandbox_generation(&self) -> ResidentSandboxGeneration {
        self.resident_sandbox_generation
    }

    #[must_use]
    pub const fn anchor_id(&self) -> &OverlaySourceAnchorId {
        &self.anchor_id
    }

    #[must_use]
    pub const fn anchor_generation(&self) -> OverlaySourceAnchorGeneration {
        self.anchor_generation
    }

    #[must_use]
    pub const fn commit(&self) -> &CommitId {
        &self.commit
    }

    #[must_use]
    pub const fn tree(&self) -> &GitTreeId {
        &self.tree
    }

    #[must_use]
    pub const fn source_index_digest(&self) -> &Sha256Digest {
        &self.source_index_digest
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct OverlayTaskViewLease {
    task_id: OverlayTaskViewId,
    generation: OverlayTaskViewGeneration,
}

impl OverlayTaskViewLease {
    #[must_use]
    pub const fn new(task_id: OverlayTaskViewId, generation: OverlayTaskViewGeneration) -> Self {
        Self {
            task_id,
            generation,
        }
    }

    #[must_use]
    pub const fn task_id(&self) -> &OverlayTaskViewId {
        &self.task_id
    }

    #[must_use]
    pub const fn generation(&self) -> OverlayTaskViewGeneration {
        self.generation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlaySourceAnchorState {
    Ready,
    Draining,
    Retired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OverlaySourceAnchorRecord {
    schema_version: u8,
    revision: OverlayTaskViewRevision,
    binding: OverlaySourceAnchorBinding,
    state: OverlaySourceAnchorState,
    active_tasks: BTreeSet<OverlayTaskViewLease>,
}

impl OverlaySourceAnchorRecord {
    #[must_use]
    pub fn new_ready(binding: OverlaySourceAnchorBinding) -> Self {
        Self {
            schema_version: TRUSTED_OVERLAY_TASK_VIEW_SCHEMA_VERSION,
            revision: OverlayTaskViewRevision(1),
            binding,
            state: OverlaySourceAnchorState::Ready,
            active_tasks: BTreeSet::new(),
        }
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn revision(&self) -> OverlayTaskViewRevision {
        self.revision
    }

    #[must_use]
    pub const fn binding(&self) -> &OverlaySourceAnchorBinding {
        &self.binding
    }

    #[must_use]
    pub const fn state(&self) -> OverlaySourceAnchorState {
        self.state
    }

    #[must_use]
    pub fn active_tasks(&self) -> &BTreeSet<OverlayTaskViewLease> {
        &self.active_tasks
    }

    #[must_use]
    pub fn active_task_count(&self) -> usize {
        self.active_tasks.len()
    }

    pub fn acquire_task(
        &self,
        lease: OverlayTaskViewLease,
    ) -> Result<Self, TrustedOverlayTaskViewError> {
        if self.state != OverlaySourceAnchorState::Ready {
            return Err(anchor_not_ready());
        }
        if self.active_tasks.len() >= MAX_OVERLAY_SOURCE_ANCHOR_TASKS {
            return Err(anchor_task_limit());
        }
        if self.active_tasks.contains(&lease)
            || self
                .active_tasks
                .iter()
                .any(|current| current.task_id == lease.task_id)
        {
            return Err(task_lease_conflict());
        }
        let mut active_tasks = self.active_tasks.clone();
        active_tasks.insert(lease);
        self.successor(self.state, active_tasks)
    }

    pub fn release_task(
        &self,
        lease: &OverlayTaskViewLease,
    ) -> Result<Self, TrustedOverlayTaskViewError> {
        if self.state == OverlaySourceAnchorState::Retired {
            return Err(anchor_terminal());
        }
        if !self.active_tasks.contains(lease) {
            return Err(task_lease_missing());
        }
        let mut active_tasks = self.active_tasks.clone();
        active_tasks.remove(lease);
        self.successor(self.state, active_tasks)
    }

    pub fn request_draining(&self) -> Result<Self, TrustedOverlayTaskViewError> {
        match self.state {
            OverlaySourceAnchorState::Ready => self.successor(
                OverlaySourceAnchorState::Draining,
                self.active_tasks.clone(),
            ),
            OverlaySourceAnchorState::Draining => Ok(self.clone()),
            OverlaySourceAnchorState::Retired => Err(anchor_terminal()),
        }
    }

    pub fn retire(&self) -> Result<Self, TrustedOverlayTaskViewError> {
        if self.state != OverlaySourceAnchorState::Draining {
            return Err(anchor_retire_requires_draining());
        }
        if !self.active_tasks.is_empty() {
            return Err(anchor_has_active_tasks());
        }
        self.successor(OverlaySourceAnchorState::Retired, BTreeSet::new())
    }

    fn successor(
        &self,
        state: OverlaySourceAnchorState,
        active_tasks: BTreeSet<OverlayTaskViewLease>,
    ) -> Result<Self, TrustedOverlayTaskViewError> {
        Ok(Self {
            schema_version: self.schema_version,
            revision: self.revision.next()?,
            binding: self.binding.clone(),
            state,
            active_tasks,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayTaskViewState {
    Planned,
    WorktreeRegistered,
    Mounted,
    Ready,
    Running,
    CleanupUnmountRequired,
    CleanupWorktreeRemoveRequired,
    Quarantined,
    Retired,
}

impl OverlayTaskViewState {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Retired)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayGitWorktreeObservation {
    Absent,
    Exact,
    Other,
    Unknown,
}

/// Compatibility alias for the original linked-worktree-specific M6 vocabulary.
pub type OverlayLinkedWorktreeObservation = OverlayGitWorktreeObservation;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayMountObservation {
    Absent,
    Exact,
    Other,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayIndexObservation {
    Absent,
    ExactSource,
    Other,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayGitProofObservation {
    NotRun,
    ExactClean,
    Mismatch,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayTaskProcessObservation {
    Absent,
    ExactRunning,
    Other,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct OverlayTaskViewObservation {
    worktree: OverlayGitWorktreeObservation,
    mount: OverlayMountObservation,
    index: OverlayIndexObservation,
    git_proof: OverlayGitProofObservation,
    process: OverlayTaskProcessObservation,
}

impl OverlayTaskViewObservation {
    #[must_use]
    pub const fn new(
        worktree: OverlayGitWorktreeObservation,
        mount: OverlayMountObservation,
        index: OverlayIndexObservation,
        git_proof: OverlayGitProofObservation,
        process: OverlayTaskProcessObservation,
    ) -> Self {
        Self {
            worktree,
            mount,
            index,
            git_proof,
            process,
        }
    }

    #[must_use]
    pub const fn worktree(self) -> OverlayGitWorktreeObservation {
        self.worktree
    }

    #[must_use]
    pub const fn mount(self) -> OverlayMountObservation {
        self.mount
    }

    #[must_use]
    pub const fn index(self) -> OverlayIndexObservation {
        self.index
    }

    #[must_use]
    pub const fn git_proof(self) -> OverlayGitProofObservation {
        self.git_proof
    }

    #[must_use]
    pub const fn process(self) -> OverlayTaskProcessObservation {
        self.process
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayCleanupAction {
    UnmountExact,
    RemoveExactWorktree,
    CompleteAbsent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OverlayTaskViewRecord {
    schema_version: u8,
    revision: OverlayTaskViewRevision,
    lease: OverlayTaskViewLease,
    source_anchor: OverlaySourceAnchorBinding,
    state: OverlayTaskViewState,
}

impl OverlayTaskViewRecord {
    #[must_use]
    pub fn new_planned(
        lease: OverlayTaskViewLease,
        source_anchor: OverlaySourceAnchorBinding,
    ) -> Self {
        Self {
            schema_version: TRUSTED_OVERLAY_TASK_VIEW_SCHEMA_VERSION,
            revision: OverlayTaskViewRevision(1),
            lease,
            source_anchor,
            state: OverlayTaskViewState::Planned,
        }
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn revision(&self) -> OverlayTaskViewRevision {
        self.revision
    }

    #[must_use]
    pub const fn lease(&self) -> &OverlayTaskViewLease {
        &self.lease
    }

    #[must_use]
    pub const fn source_anchor(&self) -> &OverlaySourceAnchorBinding {
        &self.source_anchor
    }

    #[must_use]
    pub const fn state(&self) -> OverlayTaskViewState {
        self.state
    }

    pub fn record_worktree_registered(
        &self,
        observation: OverlayTaskViewObservation,
    ) -> Result<Self, TrustedOverlayTaskViewError> {
        self.require_state(
            OverlayTaskViewState::Planned,
            "overlay_worktree_registration_requires_planned",
            "overlay worktree registration requires planned state",
        )?;
        require_process_absent(observation)?;
        if observation.worktree != OverlayGitWorktreeObservation::Exact
            || observation.mount != OverlayMountObservation::Absent
        {
            return Err(worktree_registration_unproven());
        }
        self.successor(OverlayTaskViewState::WorktreeRegistered)
    }

    pub fn record_mounted(
        &self,
        observation: OverlayTaskViewObservation,
    ) -> Result<Self, TrustedOverlayTaskViewError> {
        self.require_state(
            OverlayTaskViewState::WorktreeRegistered,
            "overlay_mount_requires_registered_worktree",
            "overlay mount acceptance requires registered-worktree state",
        )?;
        require_process_absent(observation)?;
        if observation.worktree != OverlayGitWorktreeObservation::Exact
            || observation.mount != OverlayMountObservation::Exact
        {
            return Err(mount_unproven());
        }
        self.successor(OverlayTaskViewState::Mounted)
    }

    pub fn record_ready(
        &self,
        observation: OverlayTaskViewObservation,
    ) -> Result<Self, TrustedOverlayTaskViewError> {
        self.require_state(
            OverlayTaskViewState::Mounted,
            "overlay_ready_requires_mounted",
            "overlay readiness requires mounted state",
        )?;
        require_process_absent(observation)?;
        if observation.worktree != OverlayGitWorktreeObservation::Exact
            || observation.mount != OverlayMountObservation::Exact
            || observation.index != OverlayIndexObservation::ExactSource
            || observation.git_proof != OverlayGitProofObservation::ExactClean
        {
            return Err(readiness_unproven());
        }
        self.successor(OverlayTaskViewState::Ready)
    }

    pub fn record_running(
        &self,
        observation: OverlayTaskViewObservation,
    ) -> Result<Self, TrustedOverlayTaskViewError> {
        self.require_state(
            OverlayTaskViewState::Ready,
            "overlay_running_requires_ready",
            "overlay task start requires ready state",
        )?;
        if observation.worktree != OverlayGitWorktreeObservation::Exact
            || observation.mount != OverlayMountObservation::Exact
            || observation.process != OverlayTaskProcessObservation::ExactRunning
        {
            return Err(task_start_unproven());
        }
        self.successor(OverlayTaskViewState::Running)
    }

    pub fn request_cleanup(&self) -> Result<Self, TrustedOverlayTaskViewError> {
        match self.state {
            OverlayTaskViewState::Planned | OverlayTaskViewState::WorktreeRegistered => {
                self.successor(OverlayTaskViewState::CleanupWorktreeRemoveRequired)
            }
            OverlayTaskViewState::Mounted
            | OverlayTaskViewState::Ready
            | OverlayTaskViewState::Running => {
                self.successor(OverlayTaskViewState::CleanupUnmountRequired)
            }
            OverlayTaskViewState::CleanupUnmountRequired
            | OverlayTaskViewState::CleanupWorktreeRemoveRequired => Ok(self.clone()),
            OverlayTaskViewState::Quarantined => Err(task_quarantined()),
            OverlayTaskViewState::Retired => Err(task_terminal()),
        }
    }

    pub fn plan_cleanup_action(
        &self,
        observation: OverlayTaskViewObservation,
    ) -> Result<OverlayCleanupAction, TrustedOverlayTaskViewError> {
        require_process_absent(observation)?;
        require_nonforeign_cleanup_observation(observation)?;

        match self.state {
            OverlayTaskViewState::CleanupUnmountRequired => {
                match (observation.mount, observation.worktree) {
                    (OverlayMountObservation::Exact, OverlayGitWorktreeObservation::Exact) => {
                        Ok(OverlayCleanupAction::UnmountExact)
                    }
                    (OverlayMountObservation::Absent, OverlayGitWorktreeObservation::Exact) => {
                        Ok(OverlayCleanupAction::RemoveExactWorktree)
                    }
                    (OverlayMountObservation::Absent, OverlayGitWorktreeObservation::Absent) => {
                        Ok(OverlayCleanupAction::CompleteAbsent)
                    }
                    (OverlayMountObservation::Exact, OverlayGitWorktreeObservation::Absent) => {
                        Err(cleanup_identity_conflict())
                    }
                    _ => Err(cleanup_inconclusive()),
                }
            }
            OverlayTaskViewState::CleanupWorktreeRemoveRequired => {
                match (observation.mount, observation.worktree) {
                    (OverlayMountObservation::Absent, OverlayGitWorktreeObservation::Exact) => {
                        Ok(OverlayCleanupAction::RemoveExactWorktree)
                    }
                    (OverlayMountObservation::Absent, OverlayGitWorktreeObservation::Absent) => {
                        Ok(OverlayCleanupAction::CompleteAbsent)
                    }
                    (OverlayMountObservation::Exact, _) => Err(cleanup_mount_still_present()),
                    _ => Err(cleanup_inconclusive()),
                }
            }
            _ => Err(cleanup_not_requested()),
        }
    }

    pub fn record_unmount_success(
        &self,
        observation: OverlayTaskViewObservation,
    ) -> Result<Self, TrustedOverlayTaskViewError> {
        self.require_state(
            OverlayTaskViewState::CleanupUnmountRequired,
            "overlay_unmount_requires_cleanup",
            "overlay unmount acceptance requires cleanup-unmount state",
        )?;
        require_process_absent(observation)?;
        if observation.mount != OverlayMountObservation::Absent
            || observation.worktree != OverlayGitWorktreeObservation::Exact
        {
            return Err(unmount_postcondition_unproven());
        }
        self.successor(OverlayTaskViewState::CleanupWorktreeRemoveRequired)
    }

    pub fn record_worktree_remove_success(
        &self,
        observation: OverlayTaskViewObservation,
    ) -> Result<Self, TrustedOverlayTaskViewError> {
        if !matches!(
            self.state,
            OverlayTaskViewState::CleanupUnmountRequired
                | OverlayTaskViewState::CleanupWorktreeRemoveRequired
        ) {
            return Err(cleanup_not_requested());
        }
        require_process_absent(observation)?;
        if observation.mount != OverlayMountObservation::Absent
            || observation.worktree != OverlayGitWorktreeObservation::Absent
        {
            return Err(worktree_remove_postcondition_unproven());
        }
        self.successor(OverlayTaskViewState::Retired)
    }

    pub fn record_quarantined(&self) -> Result<Self, TrustedOverlayTaskViewError> {
        if self.state == OverlayTaskViewState::Retired {
            return Err(task_terminal());
        }
        if self.state == OverlayTaskViewState::Quarantined {
            return Ok(self.clone());
        }
        self.successor(OverlayTaskViewState::Quarantined)
    }

    fn require_state(
        &self,
        expected: OverlayTaskViewState,
        code: &'static str,
        message: &'static str,
    ) -> Result<(), TrustedOverlayTaskViewError> {
        if self.state != expected {
            return Err(error(code, message));
        }
        Ok(())
    }

    fn successor(&self, state: OverlayTaskViewState) -> Result<Self, TrustedOverlayTaskViewError> {
        Ok(Self {
            schema_version: self.schema_version,
            revision: self.revision.next()?,
            lease: self.lease.clone(),
            source_anchor: self.source_anchor.clone(),
            state,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedOverlayTaskViewError {
    code: &'static str,
    message: &'static str,
}

impl TrustedOverlayTaskViewError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for TrustedOverlayTaskViewError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for TrustedOverlayTaskViewError {}

fn validate_identifier(value: &str) -> Result<(), TrustedOverlayTaskViewError> {
    let Some(first) = value.bytes().next() else {
        return Err(invalid_identifier());
    };
    if value.len() > MAX_IDENTIFIER_BYTES
        || !(first.is_ascii_lowercase() || first.is_ascii_digit())
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
    {
        return Err(invalid_identifier());
    }
    Ok(())
}

fn require_process_absent(
    observation: OverlayTaskViewObservation,
) -> Result<(), TrustedOverlayTaskViewError> {
    if observation.process != OverlayTaskProcessObservation::Absent {
        return Err(task_process_active_or_unknown());
    }
    Ok(())
}

fn require_nonforeign_cleanup_observation(
    observation: OverlayTaskViewObservation,
) -> Result<(), TrustedOverlayTaskViewError> {
    if matches!(observation.mount, OverlayMountObservation::Other)
        || matches!(observation.worktree, OverlayGitWorktreeObservation::Other)
    {
        return Err(cleanup_identity_conflict());
    }
    Ok(())
}

const fn error(code: &'static str, message: &'static str) -> TrustedOverlayTaskViewError {
    TrustedOverlayTaskViewError { code, message }
}

const fn invalid_identifier() -> TrustedOverlayTaskViewError {
    error(
        "invalid_overlay_identifier",
        "overlay identifier must be a bounded lowercase ASCII token",
    )
}

const fn generation_exhausted() -> TrustedOverlayTaskViewError {
    error(
        "overlay_generation_exhausted",
        "overlay generation counter is exhausted",
    )
}

const fn anchor_not_ready() -> TrustedOverlayTaskViewError {
    error(
        "overlay_anchor_not_ready",
        "overlay source anchor is not accepting new task views",
    )
}

const fn anchor_task_limit() -> TrustedOverlayTaskViewError {
    error(
        "overlay_anchor_task_limit",
        "overlay source anchor reached its bounded task-view limit",
    )
}

const fn task_lease_conflict() -> TrustedOverlayTaskViewError {
    error(
        "overlay_task_lease_conflict",
        "overlay task-view identity conflicts with an active anchor lease",
    )
}

const fn task_lease_missing() -> TrustedOverlayTaskViewError {
    error(
        "overlay_task_lease_missing",
        "overlay task-view lease is not active on this source anchor",
    )
}

const fn anchor_terminal() -> TrustedOverlayTaskViewError {
    error(
        "overlay_anchor_terminal",
        "overlay source anchor is retired",
    )
}

const fn anchor_retire_requires_draining() -> TrustedOverlayTaskViewError {
    error(
        "overlay_anchor_retire_requires_draining",
        "overlay source anchor retirement requires draining state",
    )
}

const fn anchor_has_active_tasks() -> TrustedOverlayTaskViewError {
    error(
        "overlay_anchor_has_active_tasks",
        "overlay source anchor cannot retire while task views remain active",
    )
}

const fn worktree_registration_unproven() -> TrustedOverlayTaskViewError {
    error(
        "overlay_worktree_registration_unproven",
        "exact Git-worktree registration is not proven",
    )
}

const fn mount_unproven() -> TrustedOverlayTaskViewError {
    error(
        "overlay_mount_unproven",
        "exact overlay mount is not proven",
    )
}

const fn readiness_unproven() -> TrustedOverlayTaskViewError {
    error(
        "overlay_readiness_unproven",
        "overlay task-view readiness proof is incomplete",
    )
}

const fn task_start_unproven() -> TrustedOverlayTaskViewError {
    error(
        "overlay_task_start_unproven",
        "exact running overlay task is not proven",
    )
}

const fn task_process_active_or_unknown() -> TrustedOverlayTaskViewError {
    error(
        "overlay_task_process_not_absent",
        "overlay cleanup or setup requires proven task-process absence",
    )
}

const fn cleanup_identity_conflict() -> TrustedOverlayTaskViewError {
    error(
        "overlay_cleanup_identity_conflict",
        "foreign overlay mount or Git-worktree evidence blocks cleanup",
    )
}

const fn cleanup_inconclusive() -> TrustedOverlayTaskViewError {
    error(
        "overlay_cleanup_inconclusive",
        "overlay cleanup evidence remains inconclusive",
    )
}

const fn cleanup_mount_still_present() -> TrustedOverlayTaskViewError {
    error(
        "overlay_cleanup_mount_still_present",
        "overlay mount must be absent before Git-worktree removal",
    )
}

const fn cleanup_not_requested() -> TrustedOverlayTaskViewError {
    error(
        "overlay_cleanup_not_requested",
        "overlay cleanup action requires a cleanup state",
    )
}

const fn unmount_postcondition_unproven() -> TrustedOverlayTaskViewError {
    error(
        "overlay_unmount_postcondition_unproven",
        "overlay unmount postcondition is not proven",
    )
}

const fn worktree_remove_postcondition_unproven() -> TrustedOverlayTaskViewError {
    error(
        "overlay_worktree_remove_postcondition_unproven",
        "Git-worktree removal postcondition is not proven",
    )
}

const fn task_quarantined() -> TrustedOverlayTaskViewError {
    error(
        "overlay_task_quarantined",
        "overlay task view is quarantined",
    )
}

const fn task_terminal() -> TrustedOverlayTaskViewError {
    error("overlay_task_terminal", "overlay task view is retired")
}

#[cfg(test)]
mod tests {
    use super::{
        OverlayCleanupAction, OverlayGitProofObservation, OverlayGitWorktreeObservation,
        OverlayIndexObservation, OverlayLinkedWorktreeObservation, OverlayMountObservation,
        OverlaySourceAnchorBinding, OverlaySourceAnchorGeneration, OverlaySourceAnchorId,
        OverlaySourceAnchorRecord, OverlaySourceAnchorState, OverlayTaskProcessObservation,
        OverlayTaskViewGeneration, OverlayTaskViewId, OverlayTaskViewLease,
        OverlayTaskViewObservation, OverlayTaskViewRecord, OverlayTaskViewState,
    };
    use crate::artifact::{CommitId, GitTreeId, Sha256Digest};
    use crate::project_catalog::ProjectIdentity;
    use crate::project_disk_lease::{
        ProjectDiskGeneration, ProjectDiskId, ResidentSandboxGeneration, ResidentSandboxId,
    };

    fn binding() -> OverlaySourceAnchorBinding {
        OverlaySourceAnchorBinding::new(
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            ProjectDiskId::parse("smolrunner-project-disk").unwrap(),
            ProjectDiskGeneration::new(1).unwrap(),
            ResidentSandboxId::parse("resident-a").unwrap(),
            ResidentSandboxGeneration::new(1).unwrap(),
            OverlaySourceAnchorId::parse("anchor-a").unwrap(),
            OverlaySourceAnchorGeneration::new(1).unwrap(),
            CommitId::parse("1111111111111111111111111111111111111111").unwrap(),
            GitTreeId::parse("2222222222222222222222222222222222222222").unwrap(),
            Sha256Digest::parse(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .unwrap(),
        )
    }

    fn lease(name: &str, generation: u64) -> OverlayTaskViewLease {
        OverlayTaskViewLease::new(
            OverlayTaskViewId::parse(name).unwrap(),
            OverlayTaskViewGeneration::new(generation).unwrap(),
        )
    }

    fn observation(
        worktree: OverlayGitWorktreeObservation,
        mount: OverlayMountObservation,
        index: OverlayIndexObservation,
        proof: OverlayGitProofObservation,
        process: OverlayTaskProcessObservation,
    ) -> OverlayTaskViewObservation {
        OverlayTaskViewObservation::new(worktree, mount, index, proof, process)
    }

    fn registered() -> OverlayTaskViewObservation {
        observation(
            OverlayGitWorktreeObservation::Exact,
            OverlayMountObservation::Absent,
            OverlayIndexObservation::Absent,
            OverlayGitProofObservation::NotRun,
            OverlayTaskProcessObservation::Absent,
        )
    }

    fn mounted() -> OverlayTaskViewObservation {
        observation(
            OverlayGitWorktreeObservation::Exact,
            OverlayMountObservation::Exact,
            OverlayIndexObservation::Absent,
            OverlayGitProofObservation::NotRun,
            OverlayTaskProcessObservation::Absent,
        )
    }

    fn ready() -> OverlayTaskViewObservation {
        observation(
            OverlayGitWorktreeObservation::Exact,
            OverlayMountObservation::Exact,
            OverlayIndexObservation::ExactSource,
            OverlayGitProofObservation::ExactClean,
            OverlayTaskProcessObservation::Absent,
        )
    }

    fn running() -> OverlayTaskViewObservation {
        observation(
            OverlayGitWorktreeObservation::Exact,
            OverlayMountObservation::Exact,
            OverlayIndexObservation::Other,
            OverlayGitProofObservation::Mismatch,
            OverlayTaskProcessObservation::ExactRunning,
        )
    }

    fn cleanup_mounted() -> OverlayTaskViewObservation {
        observation(
            OverlayGitWorktreeObservation::Exact,
            OverlayMountObservation::Exact,
            OverlayIndexObservation::Other,
            OverlayGitProofObservation::Mismatch,
            OverlayTaskProcessObservation::Absent,
        )
    }

    fn cleanup_unmounted() -> OverlayTaskViewObservation {
        observation(
            OverlayGitWorktreeObservation::Exact,
            OverlayMountObservation::Absent,
            OverlayIndexObservation::Other,
            OverlayGitProofObservation::Mismatch,
            OverlayTaskProcessObservation::Absent,
        )
    }

    fn cleanup_absent() -> OverlayTaskViewObservation {
        observation(
            OverlayGitWorktreeObservation::Absent,
            OverlayMountObservation::Absent,
            OverlayIndexObservation::Absent,
            OverlayGitProofObservation::NotRun,
            OverlayTaskProcessObservation::Absent,
        )
    }

    #[test]
    fn anchor_holds_multiple_exact_child_leases_until_release() {
        let anchor = OverlaySourceAnchorRecord::new_ready(binding());
        let first = lease("task-a", 1);
        let second = lease("task-b", 1);
        let anchor = anchor.acquire_task(first.clone()).unwrap();
        let anchor = anchor.acquire_task(second.clone()).unwrap();
        assert_eq!(anchor.active_task_count(), 2);
        assert_eq!(anchor.state(), OverlaySourceAnchorState::Ready);

        let draining = anchor.request_draining().unwrap();
        assert_eq!(draining.state(), OverlaySourceAnchorState::Draining);
        assert_eq!(
            draining
                .acquire_task(lease("task-c", 1))
                .expect_err("draining anchor refuses new children")
                .code(),
            "overlay_anchor_not_ready"
        );
        assert_eq!(
            draining
                .retire()
                .expect_err("active children protect the anchor")
                .code(),
            "overlay_anchor_has_active_tasks"
        );

        let draining = draining.release_task(&first).unwrap();
        let draining = draining.release_task(&second).unwrap();
        let retired = draining.retire().unwrap();
        assert_eq!(retired.state(), OverlaySourceAnchorState::Retired);
    }

    #[test]
    fn anchor_rejects_same_task_id_with_another_generation() {
        let anchor = OverlaySourceAnchorRecord::new_ready(binding());
        let anchor = anchor.acquire_task(lease("task-a", 1)).unwrap();
        assert_eq!(
            anchor
                .acquire_task(lease("task-a", 2))
                .expect_err("same task ID cannot gain two active generations")
                .code(),
            "overlay_task_lease_conflict"
        );
    }

    #[test]
    fn task_creation_requires_exact_worktree_mount_index_and_git_proof() {
        let task_lease = lease("task-a", 1);
        let record = OverlayTaskViewRecord::new_planned(task_lease, binding());
        let registered_record = record.record_worktree_registered(registered()).unwrap();
        assert_eq!(
            registered_record.state(),
            OverlayTaskViewState::WorktreeRegistered
        );
        let mounted_record = registered_record.record_mounted(mounted()).unwrap();
        assert_eq!(mounted_record.state(), OverlayTaskViewState::Mounted);
        let ready_record = mounted_record.record_ready(ready()).unwrap();
        assert_eq!(ready_record.state(), OverlayTaskViewState::Ready);
        let running_record = ready_record.record_running(running()).unwrap();
        assert_eq!(running_record.state(), OverlayTaskViewState::Running);
    }

    #[test]
    fn readiness_refuses_mismatched_index_or_git_proof() {
        let record = OverlayTaskViewRecord::new_planned(lease("task-a", 1), binding())
            .record_worktree_registered(registered())
            .unwrap()
            .record_mounted(mounted())
            .unwrap();
        for (index, proof) in [
            (
                OverlayIndexObservation::Other,
                OverlayGitProofObservation::ExactClean,
            ),
            (
                OverlayIndexObservation::ExactSource,
                OverlayGitProofObservation::Mismatch,
            ),
            (
                OverlayIndexObservation::Unknown,
                OverlayGitProofObservation::Unknown,
            ),
        ] {
            let observed = observation(
                OverlayGitWorktreeObservation::Exact,
                OverlayMountObservation::Exact,
                index,
                proof,
                OverlayTaskProcessObservation::Absent,
            );
            assert_eq!(
                record
                    .record_ready(observed)
                    .expect_err("incomplete readiness cannot publish")
                    .code(),
                "overlay_readiness_unproven"
            );
        }
    }

    #[test]
    fn cleanup_stops_at_process_boundary_then_unmounts_and_unregisters() {
        let record = OverlayTaskViewRecord::new_planned(lease("task-a", 1), binding())
            .record_worktree_registered(registered())
            .unwrap()
            .record_mounted(mounted())
            .unwrap()
            .record_ready(ready())
            .unwrap()
            .record_running(running())
            .unwrap()
            .request_cleanup()
            .unwrap();
        assert_eq!(record.state(), OverlayTaskViewState::CleanupUnmountRequired);
        assert_eq!(
            record
                .plan_cleanup_action(running())
                .expect_err("active task process blocks cleanup")
                .code(),
            "overlay_task_process_not_absent"
        );
        assert_eq!(
            record.plan_cleanup_action(cleanup_mounted()).unwrap(),
            OverlayCleanupAction::UnmountExact
        );
        let record = record.record_unmount_success(cleanup_unmounted()).unwrap();
        assert_eq!(
            record.state(),
            OverlayTaskViewState::CleanupWorktreeRemoveRequired
        );
        assert_eq!(
            record.plan_cleanup_action(cleanup_unmounted()).unwrap(),
            OverlayCleanupAction::RemoveExactWorktree
        );
        let retired = record
            .record_worktree_remove_success(cleanup_absent())
            .unwrap();
        assert_eq!(retired.state(), OverlayTaskViewState::Retired);
        assert!(retired.state().is_terminal());
    }

    #[test]
    fn cleanup_can_complete_after_restart_when_mount_and_worktree_are_already_absent() {
        let record = OverlayTaskViewRecord::new_planned(lease("task-a", 1), binding())
            .request_cleanup()
            .unwrap();
        assert_eq!(
            record.plan_cleanup_action(cleanup_absent()).unwrap(),
            OverlayCleanupAction::CompleteAbsent
        );
        assert_eq!(
            record
                .record_worktree_remove_success(cleanup_absent())
                .unwrap()
                .state(),
            OverlayTaskViewState::Retired
        );
    }

    #[test]
    fn foreign_or_unknown_cleanup_evidence_authorizes_no_mutation() {
        let record = OverlayTaskViewRecord::new_planned(lease("task-a", 1), binding())
            .record_worktree_registered(registered())
            .unwrap()
            .request_cleanup()
            .unwrap();

        let foreign = observation(
            OverlayGitWorktreeObservation::Other,
            OverlayMountObservation::Absent,
            OverlayIndexObservation::Other,
            OverlayGitProofObservation::Unknown,
            OverlayTaskProcessObservation::Absent,
        );
        assert_eq!(
            record
                .plan_cleanup_action(foreign)
                .expect_err("foreign worktree blocks cleanup")
                .code(),
            "overlay_cleanup_identity_conflict"
        );

        let unknown = observation(
            OverlayGitWorktreeObservation::Unknown,
            OverlayMountObservation::Unknown,
            OverlayIndexObservation::Unknown,
            OverlayGitProofObservation::Unknown,
            OverlayTaskProcessObservation::Absent,
        );
        assert_eq!(
            record
                .plan_cleanup_action(unknown)
                .expect_err("unknown cleanup evidence authorizes nothing")
                .code(),
            "overlay_cleanup_inconclusive"
        );
    }

    #[test]
    fn quarantined_and_retired_states_are_mutation_terminal() {
        let record = OverlayTaskViewRecord::new_planned(lease("task-a", 1), binding());
        let quarantined = record.record_quarantined().unwrap();
        assert_eq!(quarantined.state(), OverlayTaskViewState::Quarantined);
        assert_eq!(
            quarantined
                .request_cleanup()
                .expect_err("quarantine authorizes no cleanup mutation")
                .code(),
            "overlay_task_quarantined"
        );

        let retired = OverlayTaskViewRecord::new_planned(lease("task-b", 1), binding())
            .request_cleanup()
            .unwrap()
            .record_worktree_remove_success(cleanup_absent())
            .unwrap();
        assert_eq!(
            retired
                .request_cleanup()
                .expect_err("retired task is terminal")
                .code(),
            "overlay_task_terminal"
        );
    }

    #[test]
    fn linked_worktree_observation_alias_preserves_generic_vocabulary() {
        let legacy: OverlayLinkedWorktreeObservation = OverlayGitWorktreeObservation::Exact;
        assert_eq!(legacy, OverlayGitWorktreeObservation::Exact);
        assert_eq!(serde_json::to_string(&legacy).unwrap(), "\"exact\"");
    }

    #[test]
    fn identifiers_are_bounded_lowercase_tokens() {
        assert!(OverlaySourceAnchorId::parse("anchor-1.alpha").is_ok());
        assert!(OverlayTaskViewId::parse("task_1").is_ok());
        assert!(OverlayTaskViewId::parse("Task-A").is_err());
        assert!(OverlayTaskViewId::parse("").is_err());
    }
}
