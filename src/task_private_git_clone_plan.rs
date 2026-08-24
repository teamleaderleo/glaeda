//! Pure sealed planning for the V1 task-private Git metadata clone.
//!
//! This module validates already-accepted logical source/task/pool relationships and constructs one
//! fixed local Git command. It performs no filesystem I/O, Git execution, account lookup, UID/GID
//! change, mount operation, cleanup, or task-state mutation. Private paths are locators only. A
//! later #580 executor must freshly confirm the retained #609 pool observation, bind every private
//! locator to exact descriptor authority, and apply the verified non-root admin credential policy
//! immediately before spawning this command.

use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::artifact::{CommitId, GitTreeId, Sha256Digest};
use crate::immutable_git_object_pool::{
    GitObjectPoolConsumerLease, GitObjectPoolGeneration, GitObjectPoolRecord, GitObjectPoolState,
};
use crate::immutable_git_object_pool_admin_producer_plan::{
    ImmutableGitObjectPoolAdminCredentialPolicy, ImmutableGitObjectPoolAdminProducerIdentity,
};
use crate::immutable_git_object_pool_marker::git_object_pool_binding_digest;
use crate::immutable_git_object_pool_observation::{
    ImmutableGitObjectPoolObservation, ImmutableGitObjectPoolObservationDisposition,
};
use crate::process::CommandSpec;
use crate::trusted_overlay_task_view::{
    OverlaySourceAnchorGeneration, OverlaySourceAnchorId, OverlaySourceAnchorRecord,
    OverlaySourceAnchorState, OverlayTaskViewLease, OverlayTaskViewRecord, OverlayTaskViewState,
};

pub const TASK_PRIVATE_GIT_CLONE_PLAN_SCHEMA_VERSION: u8 = 1;
pub const TASK_PRIVATE_GIT_CLONE_TIMEOUT: Duration = Duration::from_secs(60);
pub const MAX_TASK_PRIVATE_GIT_CLONE_STDOUT_BYTES: usize = 64 * 1024;
pub const MAX_TASK_PRIVATE_GIT_CLONE_STDERR_BYTES: usize = 64 * 1024;

const GIT: &str = "/usr/bin/git";
const SAFE_PATH: &str = "/usr/bin:/bin";
const MAX_PRIVATE_PATH_BYTES: usize = 1_024;
const REDACTED_PATH: &str = "<private-verified-task-git-path>";
const REDACTED_ACCOUNT: &str = "<verified-nonroot-admin-account>";

/// Private locators and account identity already verified by a later descriptor-bound executor.
///
/// This type has no public constructor. The paths remain locators only: constructing this value does
/// not prove that any filesystem object exists or belongs to SmolRunner.
pub struct TaskPrivateGitCloneTarget {
    pool_root: PathBuf,
    task_git_dir: PathBuf,
    merged_target: PathBuf,
    empty_template: PathBuf,
    config_root: PathBuf,
    admin: ImmutableGitObjectPoolAdminProducerIdentity,
}

impl TaskPrivateGitCloneTarget {
    #[allow(dead_code, clippy::too_many_arguments)]
    pub(crate) fn from_verified(
        pool_root: PathBuf,
        task_git_dir: PathBuf,
        merged_target: PathBuf,
        empty_template: PathBuf,
        config_root: PathBuf,
        admin: ImmutableGitObjectPoolAdminProducerIdentity,
    ) -> Result<Self, TaskPrivateGitClonePlanError> {
        let pool_root = validate_private_absolute_path(pool_root)?;
        let task_git_dir = validate_private_absolute_path(task_git_dir)?;
        let merged_target = validate_private_absolute_path(merged_target)?;
        let empty_template = validate_private_absolute_path(empty_template)?;
        let config_root = validate_private_absolute_path(config_root)?;
        require_private_roots_separate(&[
            &pool_root,
            &task_git_dir,
            &merged_target,
            &empty_template,
            &config_root,
        ])?;
        Ok(Self {
            pool_root,
            task_git_dir,
            merged_target,
            empty_template,
            config_root,
            admin,
        })
    }
}

impl fmt::Debug for TaskPrivateGitCloneTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TaskPrivateGitCloneTarget")
            .field("pool_root", &REDACTED_PATH)
            .field("task_git_dir", &REDACTED_PATH)
            .field("merged_target", &REDACTED_PATH)
            .field("empty_template", &REDACTED_PATH)
            .field("config_root", &REDACTED_PATH)
            .field("admin", &REDACTED_ACCOUNT)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskPrivateGitClonePlanSummary {
    schema_version: u8,
    task_lease: OverlayTaskViewLease,
    source_anchor_id: OverlaySourceAnchorId,
    source_anchor_generation: OverlaySourceAnchorGeneration,
    source_commit: CommitId,
    source_tree: GitTreeId,
    source_index_digest: Sha256Digest,
    pool_generation: GitObjectPoolGeneration,
    pool_binding_digest: Sha256Digest,
    credential_policy: ImmutableGitObjectPoolAdminCredentialPolicy,
    explicit_reference: bool,
    local_hardlinks_disabled: bool,
    no_checkout: bool,
    separate_git_dir: bool,
    reviewed_empty_template: bool,
    origin_removal_required: bool,
    pool_reconfirmation_required_before_spawn: bool,
    post_clone_validation_required: bool,
    ambient_environment_cleared: bool,
    timeout_seconds: u64,
    stdout_limit_bytes: usize,
    stderr_limit_bytes: usize,
    argument_count: usize,
    environment_key_count: usize,
}

impl TaskPrivateGitClonePlanSummary {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn task_lease(&self) -> &OverlayTaskViewLease {
        &self.task_lease
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
    pub const fn source_commit(&self) -> &CommitId {
        &self.source_commit
    }

    #[must_use]
    pub const fn source_tree(&self) -> &GitTreeId {
        &self.source_tree
    }

    #[must_use]
    pub const fn source_index_digest(&self) -> &Sha256Digest {
        &self.source_index_digest
    }

    #[must_use]
    pub const fn pool_generation(&self) -> GitObjectPoolGeneration {
        self.pool_generation
    }

    #[must_use]
    pub const fn pool_binding_digest(&self) -> &Sha256Digest {
        &self.pool_binding_digest
    }

    #[must_use]
    pub const fn credential_policy(&self) -> ImmutableGitObjectPoolAdminCredentialPolicy {
        self.credential_policy
    }

    #[must_use]
    pub const fn explicit_reference(&self) -> bool {
        self.explicit_reference
    }

    #[must_use]
    pub const fn local_hardlinks_disabled(&self) -> bool {
        self.local_hardlinks_disabled
    }

    #[must_use]
    pub const fn no_checkout(&self) -> bool {
        self.no_checkout
    }

    #[must_use]
    pub const fn separate_git_dir(&self) -> bool {
        self.separate_git_dir
    }

    #[must_use]
    pub const fn reviewed_empty_template(&self) -> bool {
        self.reviewed_empty_template
    }

    #[must_use]
    pub const fn origin_removal_required(&self) -> bool {
        self.origin_removal_required
    }

    #[must_use]
    pub const fn pool_reconfirmation_required_before_spawn(&self) -> bool {
        self.pool_reconfirmation_required_before_spawn
    }

    #[must_use]
    pub const fn post_clone_validation_required(&self) -> bool {
        self.post_clone_validation_required
    }

    #[must_use]
    pub const fn ambient_environment_cleared(&self) -> bool {
        self.ambient_environment_cleared
    }

    #[must_use]
    pub const fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_seconds)
    }

    #[must_use]
    pub const fn stdout_limit_bytes(&self) -> usize {
        self.stdout_limit_bytes
    }

    #[must_use]
    pub const fn stderr_limit_bytes(&self) -> usize {
        self.stderr_limit_bytes
    }

    #[must_use]
    pub const fn argument_count(&self) -> usize {
        self.argument_count
    }

    #[must_use]
    pub const fn environment_key_count(&self) -> usize {
        self.environment_key_count
    }
}

pub struct TaskPrivateGitClonePlan {
    summary: TaskPrivateGitClonePlanSummary,
    command: CommandSpec,
    admin: ImmutableGitObjectPoolAdminProducerIdentity,
}

impl TaskPrivateGitClonePlan {
    #[must_use]
    pub const fn summary(&self) -> &TaskPrivateGitClonePlanSummary {
        &self.summary
    }

    /// Borrow the fixed command only inside the later descriptor-bound #580 executor.
    #[allow(dead_code)]
    pub(crate) const fn command(&self) -> &CommandSpec {
        &self.command
    }

    /// Borrow the verified admin identity only inside the later #580 executor.
    #[allow(dead_code)]
    pub(crate) const fn admin(&self) -> ImmutableGitObjectPoolAdminProducerIdentity {
        self.admin
    }
}

impl fmt::Debug for TaskPrivateGitClonePlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TaskPrivateGitClonePlan")
            .field("summary", &self.summary)
            .field("command", &"<private-fixed-task-git-command>")
            .field("admin", &REDACTED_ACCOUNT)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskPrivateGitClonePlanErrorKind {
    InvalidPath,
    ConflictingPrivateRoots,
    AuthorityMismatch,
    PoolUnavailable,
    ObservationMismatch,
    InvalidBinding,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TaskPrivateGitClonePlanError {
    kind: TaskPrivateGitClonePlanErrorKind,
    code: &'static str,
    message: &'static str,
}

impl TaskPrivateGitClonePlanError {
    #[must_use]
    pub const fn kind(self) -> TaskPrivateGitClonePlanErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for TaskPrivateGitClonePlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TaskPrivateGitClonePlanError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for TaskPrivateGitClonePlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for TaskPrivateGitClonePlanError {}

/// Seal the fixed V1 private-Git clone command from exact accepted logical evidence.
///
/// # Errors
///
/// Fails closed unless the source anchor, planned task, active pool consumer, pool record, and
/// already-observed immutable pool all name the same exact logical generations. This function does
/// not reconfirm physical descriptors; the later executor must call #609 confirmation immediately
/// before spawn.
pub fn plan_task_private_git_clone(
    anchor: &OverlaySourceAnchorRecord,
    task: &OverlayTaskViewRecord,
    pool: &GitObjectPoolRecord,
    consumer: &GitObjectPoolConsumerLease,
    pool_observation: &ImmutableGitObjectPoolObservation,
    target: &TaskPrivateGitCloneTarget,
) -> Result<TaskPrivateGitClonePlan, TaskPrivateGitClonePlanError> {
    validate_logical_authority(anchor, task, pool, consumer, pool_observation)?;

    let pool_root = private_utf8(&target.pool_root)?;
    let task_git_dir = private_utf8(&target.task_git_dir)?;
    let merged_target = private_utf8(&target.merged_target)?;
    let empty_template = private_utf8(&target.empty_template)?;
    let config_root = private_utf8(&target.config_root)?;
    let template_argument = format!("--template={empty_template}");

    let command = CommandSpec::new(GIT)
        .argument("--no-optional-locks")
        .argument("-c")
        .argument("credential.helper=")
        .argument("-c")
        .argument("core.fsmonitor=false")
        .argument("-c")
        .argument("core.hooksPath=/dev/null")
        .argument("clone")
        .argument("--reference")
        .argument(pool_root)
        .argument("--no-local")
        .argument("--no-checkout")
        .argument("--separate-git-dir")
        .argument(task_git_dir)
        .argument(template_argument)
        .argument(pool_root)
        .argument(merged_target)
        .environment("GIT_ASKPASS", "/bin/false")
        .environment("GIT_ATTR_NOSYSTEM", "1")
        .environment("GIT_CONFIG_GLOBAL", "/dev/null")
        .environment("GIT_CONFIG_NOSYSTEM", "1")
        .environment("GIT_TERMINAL_PROMPT", "0")
        .environment("HOME", config_root)
        .environment("LANG", "C")
        .environment("LC_ALL", "C")
        .environment("PATH", SAFE_PATH)
        .environment("XDG_CONFIG_HOME", config_root);

    let binding = anchor.binding();
    let pool_binding_digest =
        git_object_pool_binding_digest(pool.binding()).map_err(|_| invalid_binding())?;
    let argument_count = command.arguments.len();
    let environment_key_count = command.environment.len();

    Ok(TaskPrivateGitClonePlan {
        summary: TaskPrivateGitClonePlanSummary {
            schema_version: TASK_PRIVATE_GIT_CLONE_PLAN_SCHEMA_VERSION,
            task_lease: task.lease().clone(),
            source_anchor_id: binding.anchor_id().clone(),
            source_anchor_generation: binding.anchor_generation(),
            source_commit: binding.commit().clone(),
            source_tree: binding.tree().clone(),
            source_index_digest: binding.source_index_digest().clone(),
            pool_generation: pool.binding().generation(),
            pool_binding_digest,
            credential_policy:
                ImmutableGitObjectPoolAdminCredentialPolicy::VerifiedAdminPrimaryIdentityClearSupplementaryGroups,
            explicit_reference: true,
            local_hardlinks_disabled: true,
            no_checkout: true,
            separate_git_dir: true,
            reviewed_empty_template: true,
            origin_removal_required: true,
            pool_reconfirmation_required_before_spawn: true,
            post_clone_validation_required: true,
            ambient_environment_cleared: true,
            timeout_seconds: TASK_PRIVATE_GIT_CLONE_TIMEOUT.as_secs(),
            stdout_limit_bytes: MAX_TASK_PRIVATE_GIT_CLONE_STDOUT_BYTES,
            stderr_limit_bytes: MAX_TASK_PRIVATE_GIT_CLONE_STDERR_BYTES,
            argument_count,
            environment_key_count,
        },
        command,
        admin: target.admin,
    })
}

fn validate_logical_authority(
    anchor: &OverlaySourceAnchorRecord,
    task: &OverlayTaskViewRecord,
    pool: &GitObjectPoolRecord,
    consumer: &GitObjectPoolConsumerLease,
    pool_observation: &ImmutableGitObjectPoolObservation,
) -> Result<(), TaskPrivateGitClonePlanError> {
    if anchor.state() == OverlaySourceAnchorState::Retired
        || !anchor.active_tasks().contains(task.lease())
        || task.state() != OverlayTaskViewState::Planned
        || task.source_anchor() != anchor.binding()
    {
        return Err(authority_mismatch());
    }

    let binding = anchor.binding();
    if consumer.source_anchor_id() != binding.anchor_id()
        || consumer.source_anchor_generation() != binding.anchor_generation()
        || consumer.commit() != binding.commit()
        || consumer.tree() != binding.tree()
    {
        return Err(authority_mismatch());
    }

    if pool.state() == GitObjectPoolState::Retired {
        return Err(pool_unavailable());
    }
    match pool.consumers().get(binding.anchor_id()) {
        Some(current) if current == consumer => {}
        _ => return Err(pool_unavailable()),
    }

    if pool.binding().project() != binding.project()
        || pool.binding().project_disk_id() != binding.disk_id()
        || pool.binding().project_disk_generation() != binding.disk_generation()
    {
        return Err(authority_mismatch());
    }

    let summary = pool_observation.summary();
    if pool_observation.binding() != pool.binding()
        || summary.disposition() != ImmutableGitObjectPoolObservationDisposition::RootOwnedFrozen
        || !summary.marker_matched()
        || !summary.retained_objects_descriptor()
        || !summary.retained_marker_descriptor()
        || !summary.same_filesystem_device()
        || !summary.nested_alternates_absent()
    {
        return Err(observation_mismatch());
    }
    Ok(())
}

fn validate_private_absolute_path(path: PathBuf) -> Result<PathBuf, TaskPrivateGitClonePlanError> {
    if !path.is_absolute()
        || path == Path::new("/")
        || path.as_os_str().as_encoded_bytes().len() > MAX_PRIVATE_PATH_BYTES
        || path.to_str().is_none()
        || path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(invalid_path());
    }
    Ok(path)
}

fn require_private_roots_separate(paths: &[&Path]) -> Result<(), TaskPrivateGitClonePlanError> {
    for (index, left) in paths.iter().enumerate() {
        for right in paths.iter().skip(index + 1) {
            if left == right || left.starts_with(right) || right.starts_with(left) {
                return Err(conflicting_roots());
            }
        }
    }
    Ok(())
}

fn private_utf8(path: &Path) -> Result<&str, TaskPrivateGitClonePlanError> {
    path.to_str().ok_or_else(invalid_path)
}

const fn error(
    kind: TaskPrivateGitClonePlanErrorKind,
    code: &'static str,
    message: &'static str,
) -> TaskPrivateGitClonePlanError {
    TaskPrivateGitClonePlanError {
        kind,
        code,
        message,
    }
}

const fn invalid_path() -> TaskPrivateGitClonePlanError {
    error(
        TaskPrivateGitClonePlanErrorKind::InvalidPath,
        "task_private_git_clone_invalid_path",
        "task-private Git clone private locator is invalid",
    )
}

const fn conflicting_roots() -> TaskPrivateGitClonePlanError {
    error(
        TaskPrivateGitClonePlanErrorKind::ConflictingPrivateRoots,
        "task_private_git_clone_conflicting_roots",
        "task-private Git clone private roots must be disjoint",
    )
}

const fn authority_mismatch() -> TaskPrivateGitClonePlanError {
    error(
        TaskPrivateGitClonePlanErrorKind::AuthorityMismatch,
        "task_private_git_clone_authority_mismatch",
        "task-private Git clone logical task/source authority does not match",
    )
}

const fn pool_unavailable() -> TaskPrivateGitClonePlanError {
    error(
        TaskPrivateGitClonePlanErrorKind::PoolUnavailable,
        "task_private_git_clone_pool_unavailable",
        "task-private Git clone exact immutable pool consumer is unavailable",
    )
}

const fn observation_mismatch() -> TaskPrivateGitClonePlanError {
    error(
        TaskPrivateGitClonePlanErrorKind::ObservationMismatch,
        "task_private_git_clone_pool_observation_mismatch",
        "task-private Git clone immutable pool observation does not match",
    )
}

const fn invalid_binding() -> TaskPrivateGitClonePlanError {
    error(
        TaskPrivateGitClonePlanErrorKind::InvalidBinding,
        "task_private_git_clone_invalid_binding",
        "task-private Git clone immutable pool binding is invalid",
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{
        GIT, REDACTED_ACCOUNT, SAFE_PATH, TaskPrivateGitClonePlanErrorKind,
        TaskPrivateGitCloneTarget, plan_task_private_git_clone,
    };
    use crate::artifact::{CommitId, GitTreeId, Sha256Digest};
    use crate::immutable_git_object_pool::{
        GitObjectFormat, GitObjectPoolBinding, GitObjectPoolConsumerLease, GitObjectPoolGeneration,
        GitObjectPoolId, GitObjectPoolProducerGenerationId, GitObjectPoolRecord,
        GitObjectPoolTrustGenerationId,
    };
    use crate::immutable_git_object_pool_admin_producer_plan::{
        ImmutableGitObjectPoolAdminCredentialPolicy, ImmutableGitObjectPoolAdminProducerIdentity,
    };
    use crate::immutable_git_object_pool_marker::{
        GitObjectPoolMarkerNonce, ImmutableGitObjectPoolMarker,
    };
    use crate::immutable_git_object_pool_observation::{
        IMMUTABLE_GIT_OBJECT_POOL_MARKER_FILE_NAME,
        observe_immutable_git_object_pool_generation_for_test,
    };
    use crate::process::CommandValue;
    use crate::project_catalog::ProjectIdentity;
    use crate::project_disk_lease::{
        ProjectDiskGeneration, ProjectDiskId, ResidentSandboxGeneration, ResidentSandboxId,
    };
    use crate::trusted_overlay_task_view::{
        OverlayGitProofObservation, OverlayGitWorktreeObservation, OverlayIndexObservation,
        OverlayMountObservation, OverlaySourceAnchorBinding, OverlaySourceAnchorGeneration,
        OverlaySourceAnchorId, OverlaySourceAnchorRecord, OverlayTaskProcessObservation,
        OverlayTaskViewGeneration, OverlayTaskViewId, OverlayTaskViewLease,
        OverlayTaskViewObservation, OverlayTaskViewRecord,
    };

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

    fn source_binding() -> OverlaySourceAnchorBinding {
        OverlaySourceAnchorBinding::new(
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            ProjectDiskId::parse("disk-a").unwrap(),
            ProjectDiskGeneration::new(4).unwrap(),
            ResidentSandboxId::parse("sandbox-a").unwrap(),
            ResidentSandboxGeneration::new(9).unwrap(),
            OverlaySourceAnchorId::parse("anchor-a").unwrap(),
            OverlaySourceAnchorGeneration::new(5).unwrap(),
            CommitId::parse("0123456789abcdef0123456789abcdef01234567").unwrap(),
            GitTreeId::parse("89abcdef0123456789abcdef0123456789abcdef").unwrap(),
            Sha256Digest::parse(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .unwrap(),
        )
    }

    fn pool_binding(generation: u64) -> GitObjectPoolBinding {
        GitObjectPoolBinding::new(
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            GitObjectPoolId::parse("pool-a").unwrap(),
            GitObjectPoolGeneration::new(generation).unwrap(),
            ProjectDiskId::parse("disk-a").unwrap(),
            ProjectDiskGeneration::new(4).unwrap(),
            GitObjectFormat::Sha1,
            GitObjectPoolProducerGenerationId::parse("git-2.55.0").unwrap(),
            GitObjectPoolTrustGenerationId::parse("trust-a").unwrap(),
        )
    }

    fn task_lease() -> OverlayTaskViewLease {
        OverlayTaskViewLease::new(
            OverlayTaskViewId::parse("task-a").unwrap(),
            OverlayTaskViewGeneration::new(7).unwrap(),
        )
    }

    fn consumer(binding: &OverlaySourceAnchorBinding) -> GitObjectPoolConsumerLease {
        GitObjectPoolConsumerLease::new(
            binding.anchor_id().clone(),
            binding.anchor_generation(),
            binding.commit().clone(),
            binding.tree().clone(),
        )
    }

    struct PoolFixture {
        base: PathBuf,
        parent: PathBuf,
        pool: PathBuf,
        objects: PathBuf,
        info: PathBuf,
        marker: PathBuf,
        owner: (u32, u32),
    }

    impl PoolFixture {
        fn new(binding: &GitObjectPoolBinding) -> Self {
            let unique = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let base = std::env::temp_dir().join(format!(
                "smolrunner-task-private-git-plan-{}-{unique}",
                std::process::id()
            ));
            let parent = base.join("parent");
            let pool = parent.join("generation");
            let objects = pool.join("objects");
            let info = objects.join("info");
            fs::create_dir_all(&info).unwrap();
            let owner_metadata = fs::metadata(&parent).unwrap();
            let owner = (owner_metadata.uid(), owner_metadata.gid());
            fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
            let marker = pool.join(IMMUTABLE_GIT_OBJECT_POOL_MARKER_FILE_NAME);
            let marker_bytes = ImmutableGitObjectPoolMarker::new(
                binding,
                GitObjectPoolMarkerNonce::new([9; 16]).unwrap(),
            )
            .unwrap()
            .encode()
            .unwrap();
            fs::write(&marker, marker_bytes).unwrap();
            fs::set_permissions(&marker, fs::Permissions::from_mode(0o444)).unwrap();
            fs::set_permissions(&info, fs::Permissions::from_mode(0o555)).unwrap();
            fs::set_permissions(&objects, fs::Permissions::from_mode(0o555)).unwrap();
            fs::set_permissions(&pool, fs::Permissions::from_mode(0o555)).unwrap();
            Self {
                base,
                parent,
                pool,
                objects,
                info,
                marker,
                owner,
            }
        }
    }

    impl Drop for PoolFixture {
        fn drop(&mut self) {
            for directory in [&self.info, &self.objects, &self.pool, &self.parent] {
                if directory.is_dir() {
                    let _ = fs::set_permissions(directory, fs::Permissions::from_mode(0o755));
                }
            }
            if self.marker.is_file() {
                let _ = fs::set_permissions(&self.marker, fs::Permissions::from_mode(0o644));
            }
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    fn target() -> TaskPrivateGitCloneTarget {
        TaskPrivateGitCloneTarget::from_verified(
            PathBuf::from("/srv/smolrunner/pools/generation"),
            PathBuf::from("/srv/smolrunner/tasks/task-a.git"),
            PathBuf::from("/srv/smolrunner/views/task-a"),
            PathBuf::from("/opt/smolrunner/empty-git-template"),
            PathBuf::from("/run/smolrunner/task-git-config"),
            ImmutableGitObjectPoolAdminProducerIdentity::from_verified(1000, 1000).unwrap(),
        )
        .unwrap()
    }

    struct LogicalFixture {
        anchor: OverlaySourceAnchorRecord,
        task: OverlayTaskViewRecord,
        pool: GitObjectPoolRecord,
        consumer: GitObjectPoolConsumerLease,
        pool_fixture: PoolFixture,
    }

    impl LogicalFixture {
        fn new() -> Self {
            let source = source_binding();
            let lease = task_lease();
            let anchor = OverlaySourceAnchorRecord::new_ready(source.clone())
                .acquire_task(lease.clone())
                .unwrap();
            let task = OverlayTaskViewRecord::new_planned(lease, source.clone());
            let consumer = consumer(&source);
            let pool_binding = pool_binding(3);
            let pool = GitObjectPoolRecord::new_ready(pool_binding.clone())
                .acquire_consumer(consumer.clone())
                .unwrap();
            let pool_fixture = PoolFixture::new(&pool_binding);
            Self {
                anchor,
                task,
                pool,
                consumer,
                pool_fixture,
            }
        }

        fn observation(
            &self,
        ) -> crate::immutable_git_object_pool_observation::ImmutableGitObjectPoolObservation
        {
            observe_immutable_git_object_pool_generation_for_test(
                &self.pool_fixture.pool,
                self.pool.binding(),
                self.pool_fixture.owner,
            )
            .unwrap()
        }
    }

    #[test]
    fn exact_leases_seal_explicit_reference_no_local_clone() {
        let fixture = LogicalFixture::new();
        let observation = fixture.observation();
        let plan = plan_task_private_git_clone(
            &fixture.anchor,
            &fixture.task,
            &fixture.pool,
            &fixture.consumer,
            &observation,
            &target(),
        )
        .unwrap();

        let argv = plan.command().displayed_argv();
        assert_eq!(argv[0], GIT);
        for required in [
            "clone",
            "--reference",
            "--no-local",
            "--no-checkout",
            "--separate-git-dir",
            "--no-optional-locks",
        ] {
            assert!(argv.iter().any(|value| value == required));
        }
        assert!(
            argv.iter()
                .any(|value| value == "/srv/smolrunner/pools/generation")
        );
        assert!(
            argv.iter()
                .any(|value| value == "/srv/smolrunner/tasks/task-a.git")
        );
        assert!(
            argv.iter()
                .any(|value| value == "/srv/smolrunner/views/task-a")
        );
        assert!(
            argv.iter()
                .any(|value| value == "--template=/opt/smolrunner/empty-git-template")
        );
        assert!(!argv.iter().any(|value| value == "--shared"));

        let environment = &plan.command().environment;
        assert_eq!(environment.len(), 10);
        for key in [
            "GIT_ASKPASS",
            "GIT_ATTR_NOSYSTEM",
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_NOSYSTEM",
            "GIT_TERMINAL_PROMPT",
            "HOME",
            "LANG",
            "LC_ALL",
            "PATH",
            "XDG_CONFIG_HOME",
        ] {
            assert!(environment.contains_key(key));
        }
        match &environment["PATH"] {
            CommandValue::Plain(value) => assert_eq!(value, SAFE_PATH),
            CommandValue::Secret(_) => panic!("PATH must be plain"),
        }
        assert_eq!(
            plan.summary().credential_policy(),
            ImmutableGitObjectPoolAdminCredentialPolicy::VerifiedAdminPrimaryIdentityClearSupplementaryGroups
        );
        assert!(plan.summary().explicit_reference());
        assert!(plan.summary().local_hardlinks_disabled());
        assert!(plan.summary().no_checkout());
        assert!(plan.summary().separate_git_dir());
        assert!(plan.summary().reviewed_empty_template());
        assert!(plan.summary().origin_removal_required());
        assert!(plan.summary().pool_reconfirmation_required_before_spawn());
        assert!(plan.summary().post_clone_validation_required());
        assert!(plan.summary().ambient_environment_cleared());
        assert_eq!(plan.summary().task_lease(), fixture.task.lease());
    }

    #[test]
    fn missing_active_task_or_non_planned_task_is_refused() {
        let fixture = LogicalFixture::new();
        let observation = fixture.observation();
        let anchor_without_task = OverlaySourceAnchorRecord::new_ready(source_binding());
        assert_eq!(
            plan_task_private_git_clone(
                &anchor_without_task,
                &fixture.task,
                &fixture.pool,
                &fixture.consumer,
                &observation,
                &target(),
            )
            .unwrap_err()
            .kind(),
            TaskPrivateGitClonePlanErrorKind::AuthorityMismatch
        );

        let registered = fixture
            .task
            .record_worktree_registered(OverlayTaskViewObservation::new(
                OverlayGitWorktreeObservation::Exact,
                OverlayMountObservation::Absent,
                OverlayIndexObservation::Absent,
                OverlayGitProofObservation::NotRun,
                OverlayTaskProcessObservation::Absent,
            ))
            .unwrap();
        assert_eq!(
            plan_task_private_git_clone(
                &fixture.anchor,
                &registered,
                &fixture.pool,
                &fixture.consumer,
                &observation,
                &target(),
            )
            .unwrap_err()
            .kind(),
            TaskPrivateGitClonePlanErrorKind::AuthorityMismatch
        );
    }

    #[test]
    fn wrong_consumer_or_pool_observation_is_refused() {
        let fixture = LogicalFixture::new();
        let observation = fixture.observation();
        let wrong_consumer = GitObjectPoolConsumerLease::new(
            fixture.consumer.source_anchor_id().clone(),
            fixture.consumer.source_anchor_generation(),
            CommitId::parse("1111111111111111111111111111111111111111").unwrap(),
            fixture.consumer.tree().clone(),
        );
        assert_eq!(
            plan_task_private_git_clone(
                &fixture.anchor,
                &fixture.task,
                &fixture.pool,
                &wrong_consumer,
                &observation,
                &target(),
            )
            .unwrap_err()
            .kind(),
            TaskPrivateGitClonePlanErrorKind::AuthorityMismatch
        );

        let other_binding = pool_binding(4);
        let other_fixture = PoolFixture::new(&other_binding);
        let other_observation = observe_immutable_git_object_pool_generation_for_test(
            &other_fixture.pool,
            &other_binding,
            other_fixture.owner,
        )
        .unwrap();
        assert_eq!(
            plan_task_private_git_clone(
                &fixture.anchor,
                &fixture.task,
                &fixture.pool,
                &fixture.consumer,
                &other_observation,
                &target(),
            )
            .unwrap_err()
            .kind(),
            TaskPrivateGitClonePlanErrorKind::ObservationMismatch
        );
    }

    #[test]
    fn retired_or_missing_pool_consumer_is_refused() {
        let fixture = LogicalFixture::new();
        let observation = fixture.observation();
        let pool_without_consumer = GitObjectPoolRecord::new_ready(fixture.pool.binding().clone());
        assert_eq!(
            plan_task_private_git_clone(
                &fixture.anchor,
                &fixture.task,
                &pool_without_consumer,
                &fixture.consumer,
                &observation,
                &target(),
            )
            .unwrap_err()
            .kind(),
            TaskPrivateGitClonePlanErrorKind::PoolUnavailable
        );

        let retired = fixture
            .pool
            .request_draining()
            .unwrap()
            .release_consumer(&fixture.consumer)
            .unwrap()
            .retire()
            .unwrap();
        assert_eq!(
            plan_task_private_git_clone(
                &fixture.anchor,
                &fixture.task,
                &retired,
                &fixture.consumer,
                &observation,
                &target(),
            )
            .unwrap_err()
            .kind(),
            TaskPrivateGitClonePlanErrorKind::PoolUnavailable
        );
    }

    #[test]
    fn private_paths_are_disjoint_and_public_surfaces_redact_them() {
        assert_eq!(
            TaskPrivateGitCloneTarget::from_verified(
                PathBuf::from("/srv/pool"),
                PathBuf::from("/srv/pool/task.git"),
                PathBuf::from("/srv/view"),
                PathBuf::from("/opt/template"),
                PathBuf::from("/run/config"),
                ImmutableGitObjectPoolAdminProducerIdentity::from_verified(1000, 1000).unwrap(),
            )
            .unwrap_err()
            .kind(),
            TaskPrivateGitClonePlanErrorKind::ConflictingPrivateRoots
        );

        let fixture = LogicalFixture::new();
        let observation = fixture.observation();
        let plan = plan_task_private_git_clone(
            &fixture.anchor,
            &fixture.task,
            &fixture.pool,
            &fixture.consumer,
            &observation,
            &target(),
        )
        .unwrap();
        let debug = format!("{plan:?}");
        let serialized = serde_json::to_string(plan.summary()).unwrap();
        for private in [
            "/srv/smolrunner/pools/generation",
            "/srv/smolrunner/tasks/task-a.git",
            "/srv/smolrunner/views/task-a",
            "/opt/smolrunner/empty-git-template",
            "/run/smolrunner/task-git-config",
        ] {
            assert!(!debug.contains(private));
            assert!(!serialized.contains(private));
        }
        assert!(debug.contains("<private-fixed-task-git-command>"));
        assert!(debug.contains(REDACTED_ACCOUNT));
    }
}
