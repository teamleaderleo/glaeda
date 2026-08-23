use std::collections::BTreeMap;
use std::fmt;

use serde::Serialize;

use crate::artifact::{CommitId, GitTreeId};
use crate::project_catalog::ProjectIdentity;
use crate::project_disk_lease::{ProjectDiskGeneration, ProjectDiskId};
use crate::trusted_overlay_task_view::{OverlaySourceAnchorGeneration, OverlaySourceAnchorId};

pub const IMMUTABLE_GIT_OBJECT_POOL_SCHEMA_VERSION: u8 = 1;
pub const MAX_GIT_OBJECT_POOL_CONSUMERS: usize = 256;
const MAX_IDENTIFIER_BYTES: usize = 96;

macro_rules! identifier_type {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Parse one bounded public generation/identity token.
            ///
            /// # Errors
            ///
            /// Returns a bounded error for an empty, oversized, or non-canonical token.
            pub fn parse(value: &str) -> Result<Self, ImmutableGitObjectPoolError> {
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

identifier_type!(GitObjectPoolId);
identifier_type!(GitObjectPoolProducerGenerationId);
identifier_type!(GitObjectPoolTrustGenerationId);

macro_rules! positive_generation_type {
    ($name:ident, $code:literal, $message:literal) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(u64);

        impl $name {
            pub fn new(value: u64) -> Result<Self, ImmutableGitObjectPoolError> {
                if value == 0 {
                    return Err(error(
                        ImmutableGitObjectPoolErrorKind::InvalidGeneration,
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
    GitObjectPoolGeneration,
    "git_object_pool_generation_invalid",
    "Git object-pool generation must be greater than zero"
);
positive_generation_type!(
    GitObjectPoolRevision,
    "git_object_pool_revision_invalid",
    "Git object-pool revision must be greater than zero"
);

impl GitObjectPoolRevision {
    fn next(self) -> Result<Self, ImmutableGitObjectPoolError> {
        Self::new(self.0.checked_add(1).ok_or_else(generation_exhausted)?)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitObjectFormat {
    Sha1,
    Sha256,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitObjectPoolBinding {
    project: ProjectIdentity,
    pool_id: GitObjectPoolId,
    generation: GitObjectPoolGeneration,
    project_disk_id: ProjectDiskId,
    project_disk_generation: ProjectDiskGeneration,
    object_format: GitObjectFormat,
    producer_generation: GitObjectPoolProducerGenerationId,
    trust_generation: GitObjectPoolTrustGenerationId,
}

impl GitObjectPoolBinding {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        project: ProjectIdentity,
        pool_id: GitObjectPoolId,
        generation: GitObjectPoolGeneration,
        project_disk_id: ProjectDiskId,
        project_disk_generation: ProjectDiskGeneration,
        object_format: GitObjectFormat,
        producer_generation: GitObjectPoolProducerGenerationId,
        trust_generation: GitObjectPoolTrustGenerationId,
    ) -> Self {
        Self {
            project,
            pool_id,
            generation,
            project_disk_id,
            project_disk_generation,
            object_format,
            producer_generation,
            trust_generation,
        }
    }

    #[must_use]
    pub const fn project(&self) -> &ProjectIdentity {
        &self.project
    }

    #[must_use]
    pub const fn pool_id(&self) -> &GitObjectPoolId {
        &self.pool_id
    }

    #[must_use]
    pub const fn generation(&self) -> GitObjectPoolGeneration {
        self.generation
    }

    #[must_use]
    pub const fn project_disk_id(&self) -> &ProjectDiskId {
        &self.project_disk_id
    }

    #[must_use]
    pub const fn project_disk_generation(&self) -> ProjectDiskGeneration {
        self.project_disk_generation
    }

    #[must_use]
    pub const fn object_format(&self) -> GitObjectFormat {
        self.object_format
    }

    #[must_use]
    pub const fn producer_generation(&self) -> &GitObjectPoolProducerGenerationId {
        &self.producer_generation
    }

    #[must_use]
    pub const fn trust_generation(&self) -> &GitObjectPoolTrustGenerationId {
        &self.trust_generation
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitObjectPoolConsumerLease {
    source_anchor_id: OverlaySourceAnchorId,
    source_anchor_generation: OverlaySourceAnchorGeneration,
    commit: CommitId,
    tree: GitTreeId,
}

impl GitObjectPoolConsumerLease {
    #[must_use]
    pub const fn new(
        source_anchor_id: OverlaySourceAnchorId,
        source_anchor_generation: OverlaySourceAnchorGeneration,
        commit: CommitId,
        tree: GitTreeId,
    ) -> Self {
        Self {
            source_anchor_id,
            source_anchor_generation,
            commit,
            tree,
        }
    }

    #[must_use]
    pub const fn source_anchor_id(&self) -> &OverlaySourceAnchorId {
        &self.source_anchor_id
    }

    #[must_use]
    pub const fn source_anchor_generation(&self) -> OverlaySourceAnchorGeneration {
        self.source_anchor_generation
    }

    #[must_use]
    pub const fn commit(&self) -> &CommitId {
        &self.commit
    }

    #[must_use]
    pub const fn tree(&self) -> &GitTreeId {
        &self.tree
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitObjectPoolState {
    Ready,
    Draining,
    Retired,
}

impl GitObjectPoolState {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Retired)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitObjectPoolRecord {
    schema_version: u8,
    revision: GitObjectPoolRevision,
    binding: GitObjectPoolBinding,
    state: GitObjectPoolState,
    consumers: BTreeMap<OverlaySourceAnchorId, GitObjectPoolConsumerLease>,
}

impl GitObjectPoolRecord {
    #[must_use]
    pub fn new_ready(binding: GitObjectPoolBinding) -> Self {
        Self {
            schema_version: IMMUTABLE_GIT_OBJECT_POOL_SCHEMA_VERSION,
            revision: GitObjectPoolRevision(1),
            binding,
            state: GitObjectPoolState::Ready,
            consumers: BTreeMap::new(),
        }
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn revision(&self) -> GitObjectPoolRevision {
        self.revision
    }

    #[must_use]
    pub const fn binding(&self) -> &GitObjectPoolBinding {
        &self.binding
    }

    #[must_use]
    pub const fn state(&self) -> GitObjectPoolState {
        self.state
    }

    #[must_use]
    pub fn consumer_count(&self) -> usize {
        self.consumers.len()
    }

    #[must_use]
    pub fn consumers(&self) -> &BTreeMap<OverlaySourceAnchorId, GitObjectPoolConsumerLease> {
        &self.consumers
    }

    /// Hold this immutable generation for one exact source-anchor generation.
    ///
    /// # Errors
    ///
    /// Refuses new consumers after draining begins, duplicate/conflicting source-anchor IDs, and
    /// the bounded consumer limit.
    pub fn acquire_consumer(
        &self,
        lease: GitObjectPoolConsumerLease,
    ) -> Result<Self, ImmutableGitObjectPoolError> {
        if self.state != GitObjectPoolState::Ready {
            return Err(pool_not_ready());
        }
        if self.consumers.len() >= MAX_GIT_OBJECT_POOL_CONSUMERS {
            return Err(consumer_limit());
        }
        if self.consumers.contains_key(lease.source_anchor_id()) {
            return Err(consumer_conflict());
        }
        let mut consumers = self.consumers.clone();
        consumers.insert(lease.source_anchor_id.clone(), lease);
        self.successor(self.state, consumers)
    }

    /// Release one exact source-anchor consumer from this immutable generation.
    ///
    /// # Errors
    ///
    /// Refuses missing/conflicting leases and terminal pool generations.
    pub fn release_consumer(
        &self,
        lease: &GitObjectPoolConsumerLease,
    ) -> Result<Self, ImmutableGitObjectPoolError> {
        if self.state == GitObjectPoolState::Retired {
            return Err(pool_terminal());
        }
        match self.consumers.get(lease.source_anchor_id()) {
            Some(current) if current == lease => {}
            Some(_) => return Err(consumer_conflict()),
            None => return Err(consumer_missing()),
        }
        let mut consumers = self.consumers.clone();
        consumers.remove(lease.source_anchor_id());
        self.successor(self.state, consumers)
    }

    /// Stop admitting new source anchors while preserving every existing consumer lease.
    ///
    /// # Errors
    ///
    /// Refuses terminal pool generations.
    pub fn request_draining(&self) -> Result<Self, ImmutableGitObjectPoolError> {
        match self.state {
            GitObjectPoolState::Ready => {
                self.successor(GitObjectPoolState::Draining, self.consumers.clone())
            }
            GitObjectPoolState::Draining => Ok(self.clone()),
            GitObjectPoolState::Retired => Err(pool_terminal()),
        }
    }

    /// Retire one drained generation after every source-anchor consumer has released it.
    ///
    /// # Errors
    ///
    /// Refuses retirement before draining or while any consumer remains.
    pub fn retire(&self) -> Result<Self, ImmutableGitObjectPoolError> {
        if self.state != GitObjectPoolState::Draining {
            return Err(retire_requires_draining());
        }
        if !self.consumers.is_empty() {
            return Err(active_consumers());
        }
        self.successor(GitObjectPoolState::Retired, BTreeMap::new())
    }

    fn successor(
        &self,
        state: GitObjectPoolState,
        consumers: BTreeMap<OverlaySourceAnchorId, GitObjectPoolConsumerLease>,
    ) -> Result<Self, ImmutableGitObjectPoolError> {
        Ok(Self {
            schema_version: self.schema_version,
            revision: self.revision.next()?,
            binding: self.binding.clone(),
            state,
            consumers,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImmutableGitObjectPoolErrorKind {
    InvalidIdentifier,
    InvalidGeneration,
    PoolNotReady,
    ConsumerLimit,
    ConsumerConflict,
    ConsumerMissing,
    RetireRequiresDraining,
    ActiveConsumers,
    Terminal,
    GenerationExhausted,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct ImmutableGitObjectPoolError {
    kind: ImmutableGitObjectPoolErrorKind,
    code: &'static str,
    message: &'static str,
}

impl ImmutableGitObjectPoolError {
    #[must_use]
    pub const fn kind(&self) -> ImmutableGitObjectPoolErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for ImmutableGitObjectPoolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImmutableGitObjectPoolError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for ImmutableGitObjectPoolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ImmutableGitObjectPoolError {}

fn validate_identifier(value: &str) -> Result<(), ImmutableGitObjectPoolError> {
    let Some(first) = value.bytes().next() else {
        return Err(invalid_identifier());
    };
    if value.len() > MAX_IDENTIFIER_BYTES
        || !(first.is_ascii_lowercase() || first.is_ascii_digit())
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'-' | b'_' | b'.' | b':')
        })
    {
        return Err(invalid_identifier());
    }
    Ok(())
}

const fn error(
    kind: ImmutableGitObjectPoolErrorKind,
    code: &'static str,
    message: &'static str,
) -> ImmutableGitObjectPoolError {
    ImmutableGitObjectPoolError {
        kind,
        code,
        message,
    }
}

const fn invalid_identifier() -> ImmutableGitObjectPoolError {
    error(
        ImmutableGitObjectPoolErrorKind::InvalidIdentifier,
        "git_object_pool_identifier_invalid",
        "Git object-pool identifier must be one bounded canonical ASCII token",
    )
}

const fn pool_not_ready() -> ImmutableGitObjectPoolError {
    error(
        ImmutableGitObjectPoolErrorKind::PoolNotReady,
        "git_object_pool_not_ready",
        "Git object-pool generation is not accepting new consumers",
    )
}

const fn consumer_limit() -> ImmutableGitObjectPoolError {
    error(
        ImmutableGitObjectPoolErrorKind::ConsumerLimit,
        "git_object_pool_consumer_limit",
        "Git object-pool generation reached its bounded consumer limit",
    )
}

const fn consumer_conflict() -> ImmutableGitObjectPoolError {
    error(
        ImmutableGitObjectPoolErrorKind::ConsumerConflict,
        "git_object_pool_consumer_conflict",
        "Git object-pool source-anchor consumer conflicts with current state",
    )
}

const fn consumer_missing() -> ImmutableGitObjectPoolError {
    error(
        ImmutableGitObjectPoolErrorKind::ConsumerMissing,
        "git_object_pool_consumer_missing",
        "Git object-pool source-anchor consumer is not active",
    )
}

const fn retire_requires_draining() -> ImmutableGitObjectPoolError {
    error(
        ImmutableGitObjectPoolErrorKind::RetireRequiresDraining,
        "git_object_pool_retire_requires_draining",
        "Git object-pool retirement requires draining state",
    )
}

const fn active_consumers() -> ImmutableGitObjectPoolError {
    error(
        ImmutableGitObjectPoolErrorKind::ActiveConsumers,
        "git_object_pool_active_consumers",
        "Git object-pool generation still has active source-anchor consumers",
    )
}

const fn pool_terminal() -> ImmutableGitObjectPoolError {
    error(
        ImmutableGitObjectPoolErrorKind::Terminal,
        "git_object_pool_terminal",
        "Git object-pool generation is terminal",
    )
}

const fn generation_exhausted() -> ImmutableGitObjectPoolError {
    error(
        ImmutableGitObjectPoolErrorKind::GenerationExhausted,
        "git_object_pool_generation_exhausted",
        "Git object-pool revision space is exhausted",
    )
}

#[cfg(test)]
mod tests {
    use super::{
        GitObjectFormat, GitObjectPoolBinding, GitObjectPoolConsumerLease, GitObjectPoolGeneration,
        GitObjectPoolId, GitObjectPoolProducerGenerationId, GitObjectPoolRecord,
        GitObjectPoolState, GitObjectPoolTrustGenerationId, ImmutableGitObjectPoolErrorKind,
    };
    use crate::artifact::{CommitId, GitTreeId};
    use crate::project_catalog::ProjectIdentity;
    use crate::project_disk_lease::{ProjectDiskGeneration, ProjectDiskId};
    use crate::trusted_overlay_task_view::{OverlaySourceAnchorGeneration, OverlaySourceAnchorId};

    fn binding() -> GitObjectPoolBinding {
        GitObjectPoolBinding::new(
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            GitObjectPoolId::parse("pool-a").unwrap(),
            GitObjectPoolGeneration::new(1).unwrap(),
            ProjectDiskId::parse("project-disk").unwrap(),
            ProjectDiskGeneration::new(1).unwrap(),
            GitObjectFormat::Sha1,
            GitObjectPoolProducerGenerationId::parse("git-2.55").unwrap(),
            GitObjectPoolTrustGenerationId::parse("tier3-project").unwrap(),
        )
    }

    fn consumer(name: &str, generation: u64, commit_digit: char) -> GitObjectPoolConsumerLease {
        let commit = commit_digit.to_string().repeat(40);
        let tree_digit = if commit_digit == '1' { '2' } else { '3' };
        let tree = tree_digit.to_string().repeat(40);
        GitObjectPoolConsumerLease::new(
            OverlaySourceAnchorId::parse(name).unwrap(),
            OverlaySourceAnchorGeneration::new(generation).unwrap(),
            CommitId::parse(&commit).unwrap(),
            GitTreeId::parse(&tree).unwrap(),
        )
    }

    #[test]
    fn holds_multiple_exact_source_anchor_generations() {
        let record = GitObjectPoolRecord::new_ready(binding());
        let first = consumer("anchor-a", 1, '1');
        let second = consumer("anchor-b", 1, '4');
        let record = record.acquire_consumer(first.clone()).unwrap();
        let record = record.acquire_consumer(second.clone()).unwrap();
        assert_eq!(record.consumer_count(), 2);
        assert_eq!(record.state(), GitObjectPoolState::Ready);
        assert_eq!(record.binding(), &binding());
        assert!(record.revision().get() > 1);

        let record = record.release_consumer(&first).unwrap();
        assert_eq!(record.consumer_count(), 1);
        let record = record.release_consumer(&second).unwrap();
        assert_eq!(record.consumer_count(), 0);
    }

    #[test]
    fn same_anchor_id_cannot_hold_two_generations_concurrently() {
        let record = GitObjectPoolRecord::new_ready(binding());
        let current = consumer("anchor-a", 1, '1');
        let replacement = consumer("anchor-a", 2, '4');
        let record = record.acquire_consumer(current).unwrap();
        assert_eq!(
            record.acquire_consumer(replacement).unwrap_err().kind(),
            ImmutableGitObjectPoolErrorKind::ConsumerConflict
        );
    }

    #[test]
    fn draining_preserves_consumers_and_blocks_new_ones() {
        let first = consumer("anchor-a", 1, '1');
        let record = GitObjectPoolRecord::new_ready(binding())
            .acquire_consumer(first.clone())
            .unwrap()
            .request_draining()
            .unwrap();
        assert_eq!(record.state(), GitObjectPoolState::Draining);
        assert_eq!(record.consumer_count(), 1);
        assert_eq!(
            record
                .acquire_consumer(consumer("anchor-b", 1, '4'))
                .unwrap_err()
                .kind(),
            ImmutableGitObjectPoolErrorKind::PoolNotReady
        );
        assert_eq!(
            record.retire().unwrap_err().kind(),
            ImmutableGitObjectPoolErrorKind::ActiveConsumers
        );
        let retired = record.release_consumer(&first).unwrap().retire().unwrap();
        assert_eq!(retired.state(), GitObjectPoolState::Retired);
        assert!(retired.state().is_terminal());
    }

    #[test]
    fn release_requires_the_exact_consumer_lease() {
        let current = consumer("anchor-a", 1, '1');
        let conflicting = consumer("anchor-a", 2, '4');
        let record = GitObjectPoolRecord::new_ready(binding())
            .acquire_consumer(current)
            .unwrap();
        assert_eq!(
            record.release_consumer(&conflicting).unwrap_err().kind(),
            ImmutableGitObjectPoolErrorKind::ConsumerConflict
        );
        assert_eq!(
            record
                .release_consumer(&consumer("anchor-b", 1, '4'))
                .unwrap_err()
                .kind(),
            ImmutableGitObjectPoolErrorKind::ConsumerMissing
        );
    }

    #[test]
    fn retirement_requires_draining_and_terminal_state_never_reopens() {
        let ready = GitObjectPoolRecord::new_ready(binding());
        assert_eq!(
            ready.retire().unwrap_err().kind(),
            ImmutableGitObjectPoolErrorKind::RetireRequiresDraining
        );
        let retired = ready.request_draining().unwrap().retire().unwrap();
        assert_eq!(
            retired.request_draining().unwrap_err().kind(),
            ImmutableGitObjectPoolErrorKind::Terminal
        );
        assert_eq!(
            retired
                .acquire_consumer(consumer("anchor-a", 1, '1'))
                .unwrap_err()
                .kind(),
            ImmutableGitObjectPoolErrorKind::PoolNotReady
        );
    }

    #[test]
    fn identifiers_and_generations_are_bounded() {
        assert!(GitObjectPoolId::parse("pool-a").is_ok());
        assert!(GitObjectPoolId::parse("Pool-A").is_err());
        assert!(GitObjectPoolId::parse(&"a".repeat(97)).is_err());
        assert!(GitObjectPoolGeneration::new(0).is_err());
    }
}
