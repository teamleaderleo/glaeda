use std::collections::BTreeSet;
use std::fmt;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::artifact::{CommitId, GitTreeId, RepositoryRef};
use crate::operator_error::{OperatorErrorCode, OperatorPublicError};
use crate::personal_worker_queue::PersonalWorkerSourceIdentity;
use crate::process::{CommandSpec, ExecutionRecord, TimedCommandExecutor};
use crate::verification_profile::VerificationProfileId;
use crate::verification_profile_registry::smolrunner_profile_registry;

pub const REPOSITORY_SOURCE_OBSERVATION_SCHEMA_VERSION: u8 = 1;
pub const REPOSITORY_SOURCE_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_GIT_OBSERVATION_OUTPUT_BYTES: usize = 65_536;
const MAX_REMOTE_URLS: usize = 16;
const MAX_BRANCH_BYTES: usize = 512;

/// Opaque equality-only identity for one exact private repository workspace object.
///
/// The canonical path and filesystem object identity have no serialization or accessors.
/// Higher-level planners can prove that repository source and descriptor-derived workspace
/// evidence refer to the same opened directory without disclosing its location.
#[derive(Clone, PartialEq, Eq)]
pub struct RepositoryWorkspaceLocationIdentity {
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl RepositoryWorkspaceLocationIdentity {
    pub(crate) const fn from_validated(path: PathBuf, device: u64, inode: u64) -> Self {
        Self {
            path,
            device,
            inode,
        }
    }

    fn matches_metadata(&self, metadata: &std::fs::Metadata) -> bool {
        metadata.is_dir() && metadata.dev() == self.device && metadata.ino() == self.inode
    }
}

impl fmt::Debug for RepositoryWorkspaceLocationIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<private-workspace-location>")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryCleanliness {
    Clean,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepositorySourceObservation {
    schema_version: u8,
    source: PersonalWorkerSourceIdentity,
    verification_profile: VerificationProfileId,
    cleanliness: RepositoryCleanliness,
    #[serde(skip)]
    workspace_location_identity: RepositoryWorkspaceLocationIdentity,
}

impl RepositorySourceObservation {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn source(&self) -> &PersonalWorkerSourceIdentity {
        &self.source
    }

    #[must_use]
    pub const fn verification_profile(&self) -> &VerificationProfileId {
        &self.verification_profile
    }

    #[must_use]
    pub const fn cleanliness(&self) -> RepositoryCleanliness {
        self.cleanliness
    }

    #[must_use]
    pub const fn workspace_location_identity(&self) -> &RepositoryWorkspaceLocationIdentity {
        &self.workspace_location_identity
    }

    #[cfg(all(test, target_os = "linux"))]
    pub(crate) const fn for_verification_plan_test(
        source: PersonalWorkerSourceIdentity,
        verification_profile: VerificationProfileId,
        workspace_location_identity: RepositoryWorkspaceLocationIdentity,
    ) -> Self {
        Self {
            schema_version: REPOSITORY_SOURCE_OBSERVATION_SCHEMA_VERSION,
            source,
            verification_profile,
            cleanliness: RepositoryCleanliness::Clean,
            workspace_location_identity,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositorySourceObservationErrorKind {
    RepositoryUnavailable,
    RepositoryIdentityMismatch,
    RepositoryDirty,
    RepositorySourceChanged,
    VerificationProfileUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepositorySourceObservationError {
    kind: RepositorySourceObservationErrorKind,
    public: OperatorPublicError,
}

impl RepositorySourceObservationError {
    #[must_use]
    pub const fn kind(&self) -> RepositorySourceObservationErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn public_error(&self) -> OperatorPublicError {
        self.public
    }

    fn from_kind(kind: RepositorySourceObservationErrorKind) -> Self {
        let code = match kind {
            RepositorySourceObservationErrorKind::RepositoryUnavailable => {
                OperatorErrorCode::RepositoryUnavailable
            }
            RepositorySourceObservationErrorKind::RepositoryIdentityMismatch => {
                OperatorErrorCode::RepositoryIdentityMismatch
            }
            RepositorySourceObservationErrorKind::RepositoryDirty => {
                OperatorErrorCode::RepositoryDirty
            }
            RepositorySourceObservationErrorKind::RepositorySourceChanged => {
                OperatorErrorCode::RepositorySourceChanged
            }
            RepositorySourceObservationErrorKind::VerificationProfileUnavailable => {
                OperatorErrorCode::VerificationProfileUnavailable
            }
        };
        Self {
            kind,
            public: OperatorPublicError::from_code(code),
        }
    }
}

impl fmt::Display for RepositorySourceObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.public.fmt(formatter)
    }
}

impl std::error::Error for RepositorySourceObservationError {}

#[derive(Clone, PartialEq, Eq)]
pub struct RepositorySourceObserver {
    git_program: PathBuf,
}

impl fmt::Debug for RepositorySourceObserver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RepositorySourceObserver")
            .field("git_program", &"<reviewed-absolute-git-program>")
            .finish()
    }
}

impl RepositorySourceObserver {
    pub fn new(git_program: impl Into<PathBuf>) -> Result<Self, RepositorySourceObservationError> {
        let git_program = git_program.into();
        if !is_normalized_absolute_path(&git_program) {
            return Err(unavailable());
        }
        Ok(Self { git_program })
    }

    pub fn observe(
        &self,
        checkout: &Path,
        profile_id: &VerificationProfileId,
        executor: &impl TimedCommandExecutor,
    ) -> Result<RepositorySourceObservation, RepositorySourceObservationError> {
        let registry = smolrunner_profile_registry().map_err(|_| profile_unavailable())?;
        let profile = registry
            .lookup(profile_id)
            .map_err(|_| profile_unavailable())?;
        let expected_repository = profile.canonical_command().identity().repository();

        let checkout = std::fs::canonicalize(checkout).map_err(|_| unavailable())?;
        if !is_normalized_absolute_path(&checkout) || checkout.to_str().is_none() {
            return Err(unavailable());
        }
        let initial_metadata = std::fs::metadata(&checkout).map_err(|_| unavailable())?;
        if !initial_metadata.is_dir() {
            return Err(unavailable());
        }
        let workspace_location_identity = RepositoryWorkspaceLocationIdentity::from_validated(
            checkout.clone(),
            initial_metadata.dev(),
            initial_metadata.ino(),
        );

        let first = self.snapshot(
            &checkout,
            expected_repository,
            SnapshotPhase::First,
            executor,
        )?;
        if !first.clean {
            return Err(dirty());
        }

        let second = self.snapshot(
            &checkout,
            expected_repository,
            SnapshotPhase::Second,
            executor,
        )?;
        if second != first {
            return Err(source_changed());
        }
        let final_metadata = std::fs::metadata(&checkout).map_err(|_| source_changed())?;
        if !workspace_location_identity.matches_metadata(&final_metadata) {
            return Err(source_changed());
        }

        Ok(RepositorySourceObservation {
            schema_version: REPOSITORY_SOURCE_OBSERVATION_SCHEMA_VERSION,
            source: PersonalWorkerSourceIdentity::new(first.repository, first.commit, first.tree),
            verification_profile: profile_id.clone(),
            cleanliness: RepositoryCleanliness::Clean,
            workspace_location_identity,
        })
    }

    fn snapshot(
        &self,
        checkout: &Path,
        expected_repository: &RepositoryRef,
        phase: SnapshotPhase,
        executor: &impl TimedCommandExecutor,
    ) -> Result<RepositorySnapshot, RepositorySourceObservationError> {
        let top_level = self.git(checkout, &["rev-parse", "--show-toplevel"], executor)?;
        require_success(&top_level, phase)?;
        let top_level = parse_for_phase(parse_top_level(&top_level.stdout), phase)?;
        if top_level != checkout {
            return Err(if phase == SnapshotPhase::Second {
                source_changed()
            } else {
                unavailable()
            });
        }

        let branch = self.git(checkout, &["symbolic-ref", "--quiet", "HEAD"], executor)?;
        require_success(&branch, phase)?;
        let branch = parse_for_phase(parse_branch(&branch.stdout), phase)?;

        let local_config = self.git(
            checkout,
            &["config", "--no-includes", "--null", "--list"],
            executor,
        )?;
        let local_config = parse_for_phase(parse_local_config(&local_config), phase)?;

        let index_modes = self.git(checkout, &["ls-files", "--format=%(objectmode)"], executor)?;
        require_success(&index_modes, phase)?;
        parse_for_phase(refuse_gitlinks(&index_modes.stdout), phase)?;

        let commit = self.git(
            checkout,
            &["rev-parse", "--verify", "HEAD^{commit}"],
            executor,
        )?;
        require_success(&commit, phase)?;
        let commit = parse_for_phase(
            parse_single_line(&commit.stdout)
                .and_then(|value| CommitId::parse(value).map_err(|_| unavailable())),
            phase,
        )?;

        let tree = self.git(
            checkout,
            &["rev-parse", "--verify", "HEAD^{tree}"],
            executor,
        )?;
        require_success(&tree, phase)?;
        let tree = parse_for_phase(
            parse_single_line(&tree.stdout)
                .and_then(|value| GitTreeId::parse(value).map_err(|_| unavailable())),
            phase,
        )?;

        let repository = parse_for_phase(resolve_repository(&local_config), phase)?;
        if repository != *expected_repository {
            return Err(if phase == SnapshotPhase::Second {
                source_changed()
            } else {
                identity_mismatch()
            });
        }

        // Gitlinks were refused from index-only evidence above. Ignoring submodules here prevents
        // status from recursively reading a nested repository's separate executable Git config.
        let status = self.git(
            checkout,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--ignore-submodules=all",
            ],
            executor,
        )?;
        require_success(&status, phase)?;

        Ok(RepositorySnapshot {
            top_level,
            branch,
            commit,
            tree,
            repository,
            remote_bindings: local_config.remote_bindings,
            clean: status.stdout.is_empty(),
        })
    }

    fn git(
        &self,
        checkout: &Path,
        arguments: &[&str],
        executor: &impl TimedCommandExecutor,
    ) -> Result<ExecutionRecord, RepositorySourceObservationError> {
        let checkout = checkout.to_str().ok_or_else(unavailable)?;
        let mut spec = CommandSpec::new(&self.git_program)
            .argument("--no-optional-locks")
            .argument("-c")
            .argument("credential.helper=")
            .argument("-c")
            .argument("core.fsmonitor=false")
            .argument("-c")
            .argument("core.hooksPath=/dev/null")
            .argument("-c")
            .argument("diff.external=")
            .argument("-C")
            .argument(checkout)
            .environment("GIT_CONFIG_NOSYSTEM", "1")
            .environment("GIT_CONFIG_GLOBAL", "/dev/null")
            .environment("GIT_NO_REPLACE_OBJECTS", "1")
            .environment("GIT_NO_LAZY_FETCH", "1")
            .environment("GIT_TERMINAL_PROMPT", "0")
            .environment("GIT_ASKPASS", "/bin/false")
            // An empty allowlist disables every transport, including `file`, on the Ubuntu 24.04
            // Git baseline. `GIT_NO_LAZY_FETCH` remains defense in depth for newer Git versions.
            .environment("GIT_ALLOW_PROTOCOL", "")
            .environment("GIT_PROTOCOL_FROM_USER", "0")
            .environment("LC_ALL", "C")
            .environment("LANG", "C");
        for argument in arguments {
            spec = spec.argument(*argument);
        }
        let expected_argv = spec.displayed_argv();
        let expected_environment_keys = spec.environment.keys().cloned().collect::<Vec<_>>();
        let record = executor
            .execute_with_timeout(&spec, REPOSITORY_SOURCE_COMMAND_TIMEOUT)
            .map_err(|_| unavailable())?;
        if record.argv != expected_argv
            || record.environment_keys != expected_environment_keys
            || !record.stderr.is_empty()
            || record.stdout.len() > MAX_GIT_OBSERVATION_OUTPUT_BYTES
            || record.stdout.contains('\u{fffd}')
            || record.stdout.contains('\r')
        {
            return Err(unavailable());
        }
        Ok(record)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapshotPhase {
    First,
    Second,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RepositorySnapshot {
    top_level: PathBuf,
    branch: String,
    commit: CommitId,
    tree: GitTreeId,
    repository: RepositoryRef,
    remote_bindings: Vec<RemoteBinding>,
    clean: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RemoteBinding {
    key: String,
    url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalConfigObservation {
    remote_bindings: Vec<RemoteBinding>,
}

fn require_success(
    record: &ExecutionRecord,
    phase: SnapshotPhase,
) -> Result<(), RepositorySourceObservationError> {
    if record.success && record.status == Some(0) {
        Ok(())
    } else if phase == SnapshotPhase::Second {
        Err(source_changed())
    } else {
        Err(unavailable())
    }
}

fn parse_for_phase<T>(
    result: Result<T, RepositorySourceObservationError>,
    phase: SnapshotPhase,
) -> Result<T, RepositorySourceObservationError> {
    result.map_err(|error| {
        if phase == SnapshotPhase::Second {
            source_changed()
        } else {
            error
        }
    })
}

fn parse_top_level(value: &str) -> Result<PathBuf, RepositorySourceObservationError> {
    let path = PathBuf::from(parse_single_line(value)?);
    if is_normalized_absolute_path(&path) {
        Ok(path)
    } else {
        Err(unavailable())
    }
}

fn parse_branch(value: &str) -> Result<String, RepositorySourceObservationError> {
    let branch = parse_single_line(value)?;
    let Some(name) = branch.strip_prefix("refs/heads/") else {
        return Err(unavailable());
    };
    if name.is_empty()
        || name.len() > MAX_BRANCH_BYTES
        || name.contains("..")
        || name.contains("@{")
        || name.starts_with('.')
        || name.ends_with('.')
        || name.ends_with('/')
        || name.bytes().any(|byte| {
            byte.is_ascii_control()
                || byte == b' '
                || matches!(byte, b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
        })
    {
        return Err(unavailable());
    }
    Ok(branch.to_owned())
}

fn parse_single_line(value: &str) -> Result<&str, RepositorySourceObservationError> {
    let line = value.strip_suffix('\n').unwrap_or(value);
    if line.is_empty() || line.contains('\n') || line.contains('\0') {
        Err(unavailable())
    } else {
        Ok(line)
    }
}

fn refuse_gitlinks(value: &str) -> Result<(), RepositorySourceObservationError> {
    if !value.is_empty() && !value.ends_with('\n') {
        return Err(unavailable());
    }
    for mode in value.lines() {
        match mode {
            "100644" | "100755" | "120000" => {}
            "160000" => return Err(unavailable()),
            _ => return Err(unavailable()),
        }
    }
    Ok(())
}

fn parse_local_config(
    record: &ExecutionRecord,
) -> Result<LocalConfigObservation, RepositorySourceObservationError> {
    if !record.success || record.status != Some(0) {
        return Err(unavailable());
    }

    let mut remote_bindings = Vec::new();
    let mut count = 0;
    for entry in record.stdout.split_terminator('\0') {
        count += 1;
        let Some((key, url)) = entry.split_once('\n') else {
            return Err(unavailable());
        };
        let canonical_key = key.to_ascii_lowercase();
        if canonical_key.starts_with("include.")
            || canonical_key.starts_with("includeif.")
            || canonical_key.starts_with("filter.")
            || canonical_key.starts_with("url.")
                && (canonical_key.ends_with(".insteadof")
                    || canonical_key.ends_with(".pushinsteadof"))
        {
            return Err(unavailable());
        }
        if canonical_key.starts_with("remote.") && canonical_key.ends_with(".url") {
            remote_bindings.push(RemoteBinding {
                key: key.to_owned(),
                url: url.to_owned(),
            });
        }
    }
    if count == 0 || !record.stdout.ends_with('\0') {
        return Err(unavailable());
    }
    remote_bindings.sort();
    Ok(LocalConfigObservation { remote_bindings })
}

fn resolve_repository(
    config: &LocalConfigObservation,
) -> Result<RepositoryRef, RepositorySourceObservationError> {
    if config.remote_bindings.is_empty() || config.remote_bindings.len() > MAX_REMOTE_URLS {
        return Err(identity_mismatch());
    }
    let repositories = config
        .remote_bindings
        .iter()
        .map(|binding| parse_github_remote(&binding.url))
        .collect::<Result<BTreeSet<_>, _>>()?;
    if repositories.len() != 1 {
        return Err(identity_mismatch());
    }
    repositories
        .into_iter()
        .next()
        .ok_or_else(identity_mismatch)
}

fn parse_github_remote(value: &str) -> Result<RepositoryRef, RepositorySourceObservationError> {
    if value.is_empty()
        || value.contains('%')
        || value.contains('?')
        || value.contains('#')
        || value.contains('\\')
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(identity_mismatch());
    }
    let path = if let Some(path) = value.strip_prefix("https://github.com/") {
        path
    } else if let Some(path) = value.strip_prefix("git@github.com:") {
        path
    } else if let Some(path) = value.strip_prefix("ssh://git@github.com/") {
        path
    } else {
        return Err(identity_mismatch());
    };
    let path = path.strip_suffix(".git").unwrap_or(path);
    let canonical = path.to_ascii_lowercase();
    let mut components = canonical.split('/');
    if components
        .by_ref()
        .take(2)
        .any(|component| component == "." || component == "..")
    {
        return Err(identity_mismatch());
    }
    RepositoryRef::parse(&canonical).map_err(|_| identity_mismatch())
}

fn is_normalized_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

fn unavailable() -> RepositorySourceObservationError {
    RepositorySourceObservationError::from_kind(
        RepositorySourceObservationErrorKind::RepositoryUnavailable,
    )
}

fn identity_mismatch() -> RepositorySourceObservationError {
    RepositorySourceObservationError::from_kind(
        RepositorySourceObservationErrorKind::RepositoryIdentityMismatch,
    )
}

fn dirty() -> RepositorySourceObservationError {
    RepositorySourceObservationError::from_kind(
        RepositorySourceObservationErrorKind::RepositoryDirty,
    )
}

fn source_changed() -> RepositorySourceObservationError {
    RepositorySourceObservationError::from_kind(
        RepositorySourceObservationErrorKind::RepositorySourceChanged,
    )
}

fn profile_unavailable() -> RepositorySourceObservationError {
    RepositorySourceObservationError::from_kind(
        RepositorySourceObservationErrorKind::VerificationProfileUnavailable,
    )
}
