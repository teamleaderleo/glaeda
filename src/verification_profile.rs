use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::artifact::{CommitId, RepositoryRef, Sha256Digest};

pub const VERIFICATION_PROFILE_SCHEMA_VERSION: u8 = 1;
pub const MAX_PROFILE_REFS: usize = 16;
pub const MAX_CAPABILITIES: usize = 64;
pub const MAX_EQUIVALENT_COMMANDS: usize = 8;
pub const MAX_DECLARED_DEVIATIONS: usize = 32;
pub const MAX_PHASE_RECORDS: usize = 32;
pub const MAX_SKIP_RECORDS: usize = 32;
pub const MAX_RETRY_RECORDS: usize = 16;
pub const MAX_TIMEOUT_SECONDS: u64 = 86_400;
pub const MAX_PHASE_MILLISECONDS: u64 = 86_400_000;

macro_rules! identifier_type {
    ($name:ident, $field:literal, $max:expr) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Parse one bounded stable identifier.
            ///
            /// # Errors
            ///
            /// Returns an error for an empty, oversized, non-ASCII, or path-shaped value.
            pub fn parse(value: &str) -> Result<Self, VerificationProfileError> {
                validate_identifier($field, value, $max)?;
                Ok(Self(value.to_owned()))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

identifier_type!(VerificationProfileId, "profile.id", 64);
identifier_type!(RunnerInstallationId, "workspace.installation_id", 96);
identifier_type!(RunnerWorkspaceId, "workspace.id", 96);
identifier_type!(CapabilityId, "capability.id", 96);
identifier_type!(RepositoryCommandId, "command.id", 96);
identifier_type!(PackageId, "scope.package", 128);
identifier_type!(TargetName, "scope.target", 128);
identifier_type!(CacheId, "cache.id", 96);
identifier_type!(DeviationCode, "deviation.code", 96);
identifier_type!(SkipCode, "skip.code", 96);
identifier_type!(RetryCode, "retry.code", 96);

#[derive(Clone, PartialEq, Eq)]
struct PrivateAbsolutePath(PathBuf);

impl PrivateAbsolutePath {
    fn parse(
        field: &'static str,
        value: impl Into<PathBuf>,
    ) -> Result<Self, VerificationProfileError> {
        let value = value.into();
        if !valid_absolute_path(&value) {
            return Err(VerificationProfileError::new(
                field,
                "invalid_private_path",
                "must be an absolute normalized path without current-directory or parent components",
            ));
        }
        Ok(Self(value))
    }

    fn as_path(&self) -> &Path {
        &self.0
    }
}

impl fmt::Debug for PrivateAbsolutePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<private-path>")
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct RunnerOwnedWorkspaceIdentity {
    schema_version: u8,
    installation_id: RunnerInstallationId,
    workspace_id: RunnerWorkspaceId,
    repository: RepositoryRef,
    #[serde(skip)]
    root: PrivateAbsolutePath,
}

impl RunnerOwnedWorkspaceIdentity {
    /// Define one runner-owned workspace identity while retaining its root only as private evidence.
    ///
    /// # Errors
    ///
    /// Returns an error unless `root` is an absolute normalized path.
    pub fn new(
        installation_id: RunnerInstallationId,
        workspace_id: RunnerWorkspaceId,
        repository: RepositoryRef,
        root: impl Into<PathBuf>,
    ) -> Result<Self, VerificationProfileError> {
        Ok(Self {
            schema_version: VERIFICATION_PROFILE_SCHEMA_VERSION,
            installation_id,
            workspace_id,
            repository,
            root: PrivateAbsolutePath::parse("workspace.root", root)?,
        })
    }

    #[must_use]
    pub const fn installation_id(&self) -> &RunnerInstallationId {
        &self.installation_id
    }

    #[must_use]
    pub const fn workspace_id(&self) -> &RunnerWorkspaceId {
        &self.workspace_id
    }

    #[must_use]
    pub const fn repository(&self) -> &RepositoryRef {
        &self.repository
    }

    #[must_use]
    pub fn owns_path(&self, path: &Path) -> bool {
        valid_absolute_path(path)
            && path != self.root.as_path()
            && path.starts_with(self.root.as_path())
    }
}

impl fmt::Debug for RunnerOwnedWorkspaceIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RunnerOwnedWorkspaceIdentity")
            .field("schema_version", &self.schema_version)
            .field("installation_id", &self.installation_id)
            .field("workspace_id", &self.workspace_id)
            .field("repository", &self.repository)
            .field("root", &"<private-path>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct CacheIdentity {
    schema_version: u8,
    cache_id: CacheId,
    owner_workspace_id: RunnerWorkspaceId,
    namespace_digest: Sha256Digest,
    #[serde(skip)]
    path: PrivateAbsolutePath,
}

impl CacheIdentity {
    /// Bind one cache to the exact workspace that owns its private path.
    ///
    /// # Errors
    ///
    /// Returns an error when the owner differs or the cache path escapes the runner-owned workspace.
    pub fn new(
        workspace: &RunnerOwnedWorkspaceIdentity,
        cache_id: CacheId,
        owner_workspace_id: RunnerWorkspaceId,
        namespace_digest: Sha256Digest,
        path: impl Into<PathBuf>,
    ) -> Result<Self, VerificationProfileError> {
        if owner_workspace_id != *workspace.workspace_id() {
            return Err(VerificationProfileError::new(
                "cache.owner_workspace_id",
                "invalid_cache_ownership",
                "must match the exact runner-owned workspace identity",
            ));
        }
        let path = PrivateAbsolutePath::parse("cache.path", path)?;
        if !workspace.owns_path(path.as_path()) {
            return Err(VerificationProfileError::new(
                "cache.path",
                "workspace_path_escape",
                "must remain beneath the exact runner-owned workspace root",
            ));
        }
        Ok(Self {
            schema_version: VERIFICATION_PROFILE_SCHEMA_VERSION,
            cache_id,
            owner_workspace_id,
            namespace_digest,
            path,
        })
    }

    #[must_use]
    pub const fn cache_id(&self) -> &CacheId {
        &self.cache_id
    }

    #[must_use]
    pub const fn owner_workspace_id(&self) -> &RunnerWorkspaceId {
        &self.owner_workspace_id
    }

    #[must_use]
    pub const fn namespace_digest(&self) -> &Sha256Digest {
        &self.namespace_digest
    }
}

impl fmt::Debug for CacheIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CacheIdentity")
            .field("schema_version", &self.schema_version)
            .field("cache_id", &self.cache_id)
            .field("owner_workspace_id", &self.owner_workspace_id)
            .field("namespace_digest", &self.namespace_digest)
            .field("path", &"<private-path>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct RepositoryRefName(String);

impl RepositoryRefName {
    /// Parse one bounded Git ref name without resolving it.
    ///
    /// # Errors
    ///
    /// Returns an error for ambiguous, path-traversing, reflog-shaped, or otherwise unsafe names.
    pub fn parse(value: &str) -> Result<Self, VerificationProfileError> {
        if !valid_repository_ref_name(value) {
            return Err(VerificationProfileError::new(
                "source.ref",
                "invalid_ref_name",
                "must be one bounded unambiguous Git ref name",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct GitTreeId(String);

impl GitTreeId {
    /// Parse one complete immutable Git tree object identifier.
    ///
    /// # Errors
    ///
    /// Returns an error for abbreviated, uppercase, or non-hexadecimal values.
    pub fn parse(value: &str) -> Result<Self, VerificationProfileError> {
        if !valid_git_object_id(value) {
            return Err(VerificationProfileError::new(
                "source.tree",
                "invalid_git_tree_id",
                "must be a complete 40- or 64-character lowercase hexadecimal Git object ID",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ImmutableRefInput {
    pub ref_name: RepositoryRefName,
    pub expected_commit: CommitId,
}

impl ImmutableRefInput {
    #[must_use]
    pub const fn new(ref_name: RepositoryRefName, expected_commit: CommitId) -> Self {
        Self {
            ref_name,
            expected_commit,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceComposition {
    SingleRef,
    OrderedComposition,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TestedSourceIdentity {
    Commit { commit: CommitId, tree: GitTreeId },
    SyntheticTree { tree: GitTreeId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImmutableSourceInputs {
    schema_version: u8,
    repository: RepositoryRef,
    refs: Vec<ImmutableRefInput>,
    composition: SourceComposition,
    expected_tested_source: TestedSourceIdentity,
}

impl ImmutableSourceInputs {
    /// Bind requested refs to exact commits and an exact tested commit or synthetic tree.
    ///
    /// # Errors
    ///
    /// Returns an error for empty, duplicate, excessive, or composition-inconsistent refs.
    pub fn new(
        repository: RepositoryRef,
        refs: Vec<ImmutableRefInput>,
        composition: SourceComposition,
        expected_tested_source: TestedSourceIdentity,
    ) -> Result<Self, VerificationProfileError> {
        if refs.is_empty() || refs.len() > MAX_PROFILE_REFS {
            return Err(VerificationProfileError::new(
                "source.refs",
                "invalid_ref_count",
                format!("must contain between 1 and {MAX_PROFILE_REFS} immutable refs"),
            ));
        }
        let mut names = BTreeSet::new();
        if refs
            .iter()
            .any(|input| !names.insert(input.ref_name.clone()))
        {
            return Err(VerificationProfileError::new(
                "source.refs",
                "duplicate_ref",
                "must not contain duplicate ref names",
            ));
        }
        match (&composition, &expected_tested_source) {
            (SourceComposition::SingleRef, TestedSourceIdentity::Commit { commit, .. })
                if refs.len() == 1 && refs[0].expected_commit == *commit => {}
            (SourceComposition::OrderedComposition, TestedSourceIdentity::SyntheticTree { .. })
                if refs.len() >= 2 => {}
            (SourceComposition::SingleRef, _) => {
                return Err(VerificationProfileError::new(
                    "source.expected_tested_source",
                    "single_ref_identity_mismatch",
                    "single-ref input must test the exact sole resolved commit",
                ));
            }
            (SourceComposition::OrderedComposition, _) => {
                return Err(VerificationProfileError::new(
                    "source.expected_tested_source",
                    "composition_identity_mismatch",
                    "ordered multi-ref input must declare one exact synthetic tree identity",
                ));
            }
        }
        Ok(Self {
            schema_version: VERIFICATION_PROFILE_SCHEMA_VERSION,
            repository,
            refs,
            composition,
            expected_tested_source,
        })
    }

    #[must_use]
    pub const fn repository(&self) -> &RepositoryRef {
        &self.repository
    }

    #[must_use]
    pub fn refs(&self) -> &[ImmutableRefInput] {
        &self.refs
    }

    #[must_use]
    pub const fn expected_tested_source(&self) -> &TestedSourceIdentity {
        &self.expected_tested_source
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct TestFilter(String);

impl TestFilter {
    /// Parse one bounded repository-owned test filter identity.
    ///
    /// # Errors
    ///
    /// Returns an error for empty, oversized, or control-character-bearing values.
    pub fn parse(value: &str) -> Result<Self, VerificationProfileError> {
        if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
            return Err(VerificationProfileError::new(
                "scope.filter",
                "invalid_test_filter",
                "must be a non-empty bounded filter without control characters",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KnownTestTarget {
    Library {
        package: PackageId,
    },
    IntegrationTestBinary {
        package: PackageId,
        binary: TargetName,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExactVerificationScope {
    LibraryTests {
        package: PackageId,
    },
    IntegrationTestBinary {
        package: PackageId,
        binary: TargetName,
    },
    FilteredTest {
        target: KnownTestTarget,
        filter: TestFilter,
    },
    WholePackageTests {
        package: PackageId,
    },
    WholeWorkspaceTests,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExactBuildScope {
    LibraryTarget {
        package: PackageId,
    },
    IntegrationTestBinary {
        package: PackageId,
        binary: TargetName,
    },
    WholePackage {
        package: PackageId,
    },
    WholeWorkspace,
}

impl ExactVerificationScope {
    fn required_build_scope(&self) -> ExactBuildScope {
        match self {
            Self::LibraryTests { package } => ExactBuildScope::LibraryTarget {
                package: package.clone(),
            },
            Self::IntegrationTestBinary { package, binary } => {
                ExactBuildScope::IntegrationTestBinary {
                    package: package.clone(),
                    binary: binary.clone(),
                }
            }
            Self::FilteredTest { target, .. } => match target {
                KnownTestTarget::Library { package } => ExactBuildScope::LibraryTarget {
                    package: package.clone(),
                },
                KnownTestTarget::IntegrationTestBinary { package, binary } => {
                    ExactBuildScope::IntegrationTestBinary {
                        package: package.clone(),
                        binary: binary.clone(),
                    }
                }
            },
            Self::WholePackageTests { package } => ExactBuildScope::WholePackage {
                package: package.clone(),
            },
            Self::WholeWorkspaceTests => ExactBuildScope::WholeWorkspace,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct RepositoryCommandIdentity {
    schema_version: u8,
    repository: RepositoryRef,
    command_id: RepositoryCommandId,
    contract_digest: Sha256Digest,
}

impl RepositoryCommandIdentity {
    #[must_use]
    pub const fn new(
        repository: RepositoryRef,
        command_id: RepositoryCommandId,
        contract_digest: Sha256Digest,
    ) -> Self {
        Self {
            schema_version: VERIFICATION_PROFILE_SCHEMA_VERSION,
            repository,
            command_id,
            contract_digest,
        }
    }

    #[must_use]
    pub const fn repository(&self) -> &RepositoryRef {
        &self.repository
    }

    #[must_use]
    pub const fn command_id(&self) -> &RepositoryCommandId {
        &self.command_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepositoryCommandContract {
    identity: RepositoryCommandIdentity,
    test_scope: ExactVerificationScope,
    build_scope: ExactBuildScope,
    required_capabilities: Vec<CapabilityId>,
}

impl RepositoryCommandContract {
    /// Define one repository-approved command without embedding a shell command or argument vector.
    ///
    /// # Errors
    ///
    /// Returns an error for duplicate capabilities or a build scope wider than the exact test target.
    pub fn new(
        identity: RepositoryCommandIdentity,
        test_scope: ExactVerificationScope,
        build_scope: ExactBuildScope,
        required_capabilities: Vec<CapabilityId>,
    ) -> Result<Self, VerificationProfileError> {
        if build_scope != test_scope.required_build_scope() {
            return Err(VerificationProfileError::new(
                "command.build_scope",
                "widened_build_scope",
                "must exactly match the declared test target and may not widen package or workspace scope",
            ));
        }
        validate_unique_capabilities("command.required_capabilities", &required_capabilities)?;
        Ok(Self {
            identity,
            test_scope,
            build_scope,
            required_capabilities,
        })
    }

    #[must_use]
    pub const fn identity(&self) -> &RepositoryCommandIdentity {
        &self.identity
    }

    #[must_use]
    pub const fn test_scope(&self) -> &ExactVerificationScope {
        &self.test_scope
    }

    #[must_use]
    pub const fn build_scope(&self) -> &ExactBuildScope {
        &self.build_scope
    }

    #[must_use]
    pub fn required_capabilities(&self) -> &[CapabilityId] {
        &self.required_capabilities
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct DeclaredDeviation {
    pub code: DeviationCode,
    pub summary: String,
}

impl DeclaredDeviation {
    /// Define one bounded public deviation without ambient diagnostic content.
    ///
    /// # Errors
    ///
    /// Returns an error for empty, oversized, or control-character-bearing summaries.
    pub fn new(
        code: DeviationCode,
        summary: impl Into<String>,
    ) -> Result<Self, VerificationProfileError> {
        let summary = summary.into();
        validate_public_text("deviation.summary", &summary, 256)?;
        Ok(Self { code, summary })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RequiredCapability {
    pub capability: CapabilityId,
}

impl RequiredCapability {
    #[must_use]
    pub const fn new(capability: CapabilityId) -> Self {
        Self { capability }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OptionalCapability {
    pub capability: CapabilityId,
    pub missing_deviation: DeclaredDeviation,
}

impl OptionalCapability {
    #[must_use]
    pub const fn new(capability: CapabilityId, missing_deviation: DeclaredDeviation) -> Self {
        Self {
            capability,
            missing_deviation,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApprovedEquivalentCommand {
    command: RepositoryCommandContract,
    replaces_capabilities: Vec<CapabilityId>,
    deviation: DeclaredDeviation,
}

impl ApprovedEquivalentCommand {
    /// Declare one repository-approved equivalent command with unchanged test and build scope.
    ///
    /// # Errors
    ///
    /// Returns an error unless it replaces a non-empty subset of canonical command capabilities and
    /// preserves repository, test scope, and build scope exactly.
    pub fn new(
        canonical: &RepositoryCommandContract,
        command: RepositoryCommandContract,
        replaces_capabilities: Vec<CapabilityId>,
        deviation: DeclaredDeviation,
    ) -> Result<Self, VerificationProfileError> {
        if command.identity() == canonical.identity() {
            return Err(VerificationProfileError::new(
                "equivalent.command",
                "duplicate_command_identity",
                "must use a distinct repository command identity",
            ));
        }
        if command.identity().repository() != canonical.identity().repository()
            || command.test_scope() != canonical.test_scope()
            || command.build_scope() != canonical.build_scope()
        {
            return Err(VerificationProfileError::new(
                "equivalent.command",
                "non_equivalent_command_scope",
                "must preserve the exact repository, test scope, and build scope",
            ));
        }
        validate_unique_capabilities("equivalent.replaces_capabilities", &replaces_capabilities)?;
        if replaces_capabilities.is_empty()
            || replaces_capabilities
                .iter()
                .any(|capability| !canonical.required_capabilities().contains(capability))
        {
            return Err(VerificationProfileError::new(
                "equivalent.replaces_capabilities",
                "invalid_equivalent_activation",
                "must be a non-empty subset of canonical command capabilities",
            ));
        }
        Ok(Self {
            command,
            replaces_capabilities,
            deviation,
        })
    }

    #[must_use]
    pub const fn command(&self) -> &RepositoryCommandContract {
        &self.command
    }

    #[must_use]
    pub fn replaces_capabilities(&self) -> &[CapabilityId] {
        &self.replaces_capabilities
    }

    #[must_use]
    pub const fn deviation(&self) -> &DeclaredDeviation {
        &self.deviation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MemoryPolicy {
    pub minimum_available_bytes: u64,
    pub minimum_swap_bytes: u64,
    pub estimated_peak_bytes: u64,
}

impl MemoryPolicy {
    /// Define a bounded memory policy whose declared available memory and swap cover peak demand.
    ///
    /// # Errors
    ///
    /// Returns an error for zero values, overflow, or an uncovered peak estimate.
    pub fn new(
        minimum_available_bytes: u64,
        minimum_swap_bytes: u64,
        estimated_peak_bytes: u64,
    ) -> Result<Self, VerificationProfileError> {
        let total_available = minimum_available_bytes.checked_add(minimum_swap_bytes);
        if minimum_available_bytes == 0
            || estimated_peak_bytes == 0
            || total_available.is_none()
            || total_available.is_some_and(|total| total < estimated_peak_bytes)
        {
            return Err(VerificationProfileError::new(
                "resources.memory",
                "invalid_memory_policy",
                "must declare nonzero available and peak bytes with available memory plus swap covering peak demand",
            ));
        }
        Ok(Self {
            minimum_available_bytes,
            minimum_swap_bytes,
            estimated_peak_bytes,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ConcurrencyPolicy {
    pub build_jobs: u16,
    pub linker_jobs: u16,
    pub test_threads: u16,
}

impl ConcurrencyPolicy {
    /// Define explicit bounded concurrency defaults.
    ///
    /// # Errors
    ///
    /// Returns an error for zero, excessive, or linker concurrency above build concurrency.
    pub fn new(
        build_jobs: u16,
        linker_jobs: u16,
        test_threads: u16,
    ) -> Result<Self, VerificationProfileError> {
        let values_valid = [build_jobs, linker_jobs, test_threads]
            .into_iter()
            .all(|value| (1..=256).contains(&value));
        if !values_valid || linker_jobs > build_jobs {
            return Err(VerificationProfileError::new(
                "resources.concurrency",
                "invalid_concurrency_policy",
                "must use values from 1 through 256 with linker jobs no greater than build jobs",
            ));
        }
        Ok(Self {
            build_jobs,
            linker_jobs,
            test_threads,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ResourceDefaults {
    pub memory: MemoryPolicy,
    pub concurrency: ConcurrencyPolicy,
}

impl ResourceDefaults {
    #[must_use]
    pub const fn new(memory: MemoryPolicy, concurrency: ConcurrencyPolicy) -> Self {
        Self {
            memory,
            concurrency,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationPhase {
    Resolve,
    Preflight,
    Prepare,
    Build,
    Test,
    Format,
    Cleanup,
    FinalInspection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PhaseTimeout {
    pub phase: VerificationPhase,
    pub seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TimeoutPolicy {
    total_seconds: u64,
    phases: Vec<PhaseTimeout>,
}

impl TimeoutPolicy {
    /// Define one bounded total timeout and optional per-phase ceilings.
    ///
    /// # Errors
    ///
    /// Returns an error for zero, duplicate, excessive, or total-exceeding phase values.
    pub fn new(
        total_seconds: u64,
        phases: Vec<PhaseTimeout>,
    ) -> Result<Self, VerificationProfileError> {
        if total_seconds == 0
            || total_seconds > MAX_TIMEOUT_SECONDS
            || phases.len() > MAX_PHASE_RECORDS
        {
            return Err(VerificationProfileError::new(
                "timeout.total_seconds",
                "invalid_timeout_settings",
                format!(
                    "must be between 1 and {MAX_TIMEOUT_SECONDS} seconds with bounded phase entries"
                ),
            ));
        }
        let mut seen = BTreeSet::new();
        if phases.iter().any(|phase| {
            phase.seconds == 0 || phase.seconds > total_seconds || !seen.insert(phase.phase)
        }) {
            return Err(VerificationProfileError::new(
                "timeout.phases",
                "invalid_timeout_settings",
                "must contain unique nonzero phase ceilings no greater than the total timeout",
            ));
        }
        Ok(Self {
            total_seconds,
            phases,
        })
    }

    #[must_use]
    pub const fn total_seconds(&self) -> u64 {
        self.total_seconds
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceMutationAuthority {
    ReadOnly,
    ResetRunnerOwnedWorkspace,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "policy", rename_all = "snake_case")]
pub enum DirtyWorkspacePolicy {
    RequireClean,
    AllowDeclaredReset { deviation: DeclaredDeviation },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceMutationPolicy {
    pub authority: WorkspaceMutationAuthority,
    pub dirty_workspace: DirtyWorkspacePolicy,
}

impl WorkspaceMutationPolicy {
    /// Validate coherence between workspace authority and dirty-workspace handling.
    ///
    /// # Errors
    ///
    /// Returns an error when reset handling is declared without reset authority.
    pub fn new(
        authority: WorkspaceMutationAuthority,
        dirty_workspace: DirtyWorkspacePolicy,
    ) -> Result<Self, VerificationProfileError> {
        if matches!(
            dirty_workspace,
            DirtyWorkspacePolicy::AllowDeclaredReset { .. }
        ) && authority != WorkspaceMutationAuthority::ResetRunnerOwnedWorkspace
        {
            return Err(VerificationProfileError::new(
                "authority.workspace",
                "invalid_dirty_workspace_policy",
                "declared reset handling requires runner-owned workspace reset authority",
            ));
        }
        Ok(Self {
            authority,
            dirty_workspace,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalCommitAuthority {
    Forbidden,
    CreateInRunnerOwnedWorkspace,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "authority", rename_all = "snake_case")]
pub enum PublicationAuthority {
    Forbidden,
    CredentialedPublisherOnly {
        repository: RepositoryRef,
        target_ref: RepositoryRefName,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerificationAuthorityPolicy {
    pub workspace: WorkspaceMutationPolicy,
    pub local_commit: LocalCommitAuthority,
    pub publication: PublicationAuthority,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationProfileDefinition {
    pub profile_id: VerificationProfileId,
    pub workspace: RunnerOwnedWorkspaceIdentity,
    pub source: ImmutableSourceInputs,
    pub required_capabilities: Vec<RequiredCapability>,
    pub optional_capabilities: Vec<OptionalCapability>,
    pub canonical_command: RepositoryCommandContract,
    pub approved_equivalents: Vec<ApprovedEquivalentCommand>,
    pub resources: ResourceDefaults,
    pub cache: CacheIdentity,
    pub timeout: TimeoutPolicy,
    pub authority: VerificationAuthorityPolicy,
    pub additional_declared_deviations: Vec<DeclaredDeviation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerificationProfileContract {
    schema_version: u8,
    profile_id: VerificationProfileId,
    workspace: RunnerOwnedWorkspaceIdentity,
    source: ImmutableSourceInputs,
    required_capabilities: Vec<RequiredCapability>,
    optional_capabilities: Vec<OptionalCapability>,
    canonical_command: RepositoryCommandContract,
    approved_equivalents: Vec<ApprovedEquivalentCommand>,
    resources: ResourceDefaults,
    cache: CacheIdentity,
    timeout: TimeoutPolicy,
    authority: VerificationAuthorityPolicy,
    declared_deviations: Vec<DeclaredDeviation>,
}

impl VerificationProfileContract {
    /// Validate one complete named verification profile contract.
    ///
    /// # Errors
    ///
    /// Returns an error for repository drift, duplicate capability classes, duplicate command
    /// identities, cache ownership drift, or conflicting deviation declarations.
    pub fn new(
        definition: VerificationProfileDefinition,
    ) -> Result<Self, VerificationProfileError> {
        let VerificationProfileDefinition {
            profile_id,
            workspace,
            source,
            required_capabilities,
            optional_capabilities,
            canonical_command,
            approved_equivalents,
            resources,
            cache,
            timeout,
            authority,
            additional_declared_deviations,
        } = definition;

        if workspace.repository() != source.repository()
            || workspace.repository() != canonical_command.identity().repository()
        {
            return Err(VerificationProfileError::new(
                "profile.repository",
                "repository_identity_mismatch",
                "workspace, immutable source, and repository command must name the same repository",
            ));
        }
        if cache.owner_workspace_id() != workspace.workspace_id() {
            return Err(VerificationProfileError::new(
                "cache.owner_workspace_id",
                "invalid_cache_ownership",
                "must match the exact profile workspace identity",
            ));
        }
        if required_capabilities.len() > MAX_CAPABILITIES
            || optional_capabilities.len() > MAX_CAPABILITIES
        {
            return Err(VerificationProfileError::new(
                "profile.capabilities",
                "capability_count_exceeded",
                format!("each capability class may contain at most {MAX_CAPABILITIES} entries"),
            ));
        }
        let required_ids = required_capabilities
            .iter()
            .map(|entry| entry.capability.clone())
            .collect::<Vec<_>>();
        let optional_ids = optional_capabilities
            .iter()
            .map(|entry| entry.capability.clone())
            .collect::<Vec<_>>();
        validate_unique_capabilities("profile.required_capabilities", &required_ids)?;
        validate_unique_capabilities("profile.optional_capabilities", &optional_ids)?;
        if required_ids
            .iter()
            .any(|capability| optional_ids.contains(capability))
        {
            return Err(VerificationProfileError::new(
                "profile.capabilities",
                "overlapping_capability_classes",
                "required and optional capabilities must remain distinct",
            ));
        }
        if approved_equivalents.len() > MAX_EQUIVALENT_COMMANDS {
            return Err(VerificationProfileError::new(
                "profile.approved_equivalents",
                "equivalent_command_count_exceeded",
                format!("must contain at most {MAX_EQUIVALENT_COMMANDS} approved commands"),
            ));
        }
        let mut command_ids = BTreeSet::from([canonical_command.identity().clone()]);
        if approved_equivalents
            .iter()
            .any(|equivalent| !command_ids.insert(equivalent.command().identity().clone()))
        {
            return Err(VerificationProfileError::new(
                "profile.approved_equivalents",
                "duplicate_command_identity",
                "must not contain duplicate repository command identities",
            ));
        }

        let mut deviations = BTreeMap::new();
        for deviation in optional_capabilities
            .iter()
            .map(|entry| &entry.missing_deviation)
            .chain(
                approved_equivalents
                    .iter()
                    .map(ApprovedEquivalentCommand::deviation),
            )
            .chain(additional_declared_deviations.iter())
        {
            insert_deviation(&mut deviations, deviation.clone())?;
        }
        if let DirtyWorkspacePolicy::AllowDeclaredReset { deviation } =
            &authority.workspace.dirty_workspace
        {
            insert_deviation(&mut deviations, deviation.clone())?;
        }
        if deviations.len() > MAX_DECLARED_DEVIATIONS {
            return Err(VerificationProfileError::new(
                "profile.declared_deviations",
                "deviation_count_exceeded",
                format!("must contain at most {MAX_DECLARED_DEVIATIONS} declarations"),
            ));
        }

        Ok(Self {
            schema_version: VERIFICATION_PROFILE_SCHEMA_VERSION,
            profile_id,
            workspace,
            source,
            required_capabilities,
            optional_capabilities,
            canonical_command,
            approved_equivalents,
            resources,
            cache,
            timeout,
            authority,
            declared_deviations: deviations.into_values().collect(),
        })
    }

    #[must_use]
    pub const fn profile_id(&self) -> &VerificationProfileId {
        &self.profile_id
    }

    #[must_use]
    pub const fn workspace(&self) -> &RunnerOwnedWorkspaceIdentity {
        &self.workspace
    }

    #[must_use]
    pub const fn source(&self) -> &ImmutableSourceInputs {
        &self.source
    }

    #[must_use]
    pub const fn canonical_command(&self) -> &RepositoryCommandContract {
        &self.canonical_command
    }

    #[must_use]
    pub fn approved_equivalents(&self) -> &[ApprovedEquivalentCommand] {
        &self.approved_equivalents
    }

    #[must_use]
    pub const fn authority(&self) -> &VerificationAuthorityPolicy {
        &self.authority
    }

    fn declared_deviation(&self, code: &DeviationCode) -> Option<&DeclaredDeviation> {
        self.declared_deviations
            .iter()
            .find(|deviation| &deviation.code == code)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WorkspaceCleanliness {
    Clean,
    Dirty { changed_path_count: u16 },
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceObservation {
    installation_id: RunnerInstallationId,
    workspace_id: RunnerWorkspaceId,
    repository: RepositoryRef,
    #[serde(skip)]
    root: PrivateAbsolutePath,
    cleanliness: WorkspaceCleanliness,
}

impl WorkspaceObservation {
    /// Record a pure workspace observation while retaining the root as private evidence.
    ///
    /// # Errors
    ///
    /// Returns an error unless `root` is an absolute normalized path.
    pub fn new(
        installation_id: RunnerInstallationId,
        workspace_id: RunnerWorkspaceId,
        repository: RepositoryRef,
        root: impl Into<PathBuf>,
        cleanliness: WorkspaceCleanliness,
    ) -> Result<Self, VerificationProfileError> {
        Ok(Self {
            installation_id,
            workspace_id,
            repository,
            root: PrivateAbsolutePath::parse("observation.workspace.root", root)?,
            cleanliness,
        })
    }
}

impl fmt::Debug for WorkspaceObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkspaceObservation")
            .field("installation_id", &self.installation_id)
            .field("workspace_id", &self.workspace_id)
            .field("repository", &self.repository)
            .field("root", &"<private-path>")
            .field("cleanliness", &self.cleanliness)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ResolvedRef {
    pub ref_name: RepositoryRefName,
    pub commit: CommitId,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct CapabilityObservation {
    pub capability: CapabilityId,
    pub available: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct HostResourceObservation {
    pub available_memory_bytes: u64,
    pub available_swap_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheObservation {
    pub cache_id: CacheId,
    pub owner_workspace_id: RunnerWorkspaceId,
    pub namespace_digest: Sha256Digest,
    pub present: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestedAuthority {
    WorkspaceReset,
    LocalCommit,
    Publication,
}

#[derive(Clone, PartialEq, Eq, Default)]
pub struct PrivateVerificationEvidence {
    private_paths: Vec<PathBuf>,
    raw_environment: BTreeMap<String, String>,
    credential_material: Vec<String>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl PrivateVerificationEvidence {
    #[must_use]
    pub fn new(
        private_paths: Vec<PathBuf>,
        raw_environment: BTreeMap<String, String>,
        credential_material: Vec<String>,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    ) -> Self {
        Self {
            private_paths,
            raw_environment,
            credential_material,
            stdout,
            stderr,
        }
    }
}

impl fmt::Debug for PrivateVerificationEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PrivateVerificationEvidence")
            .field(
                "private_paths",
                &format_args!("<{} private paths>", self.private_paths.len()),
            )
            .field(
                "raw_environment",
                &format_args!(
                    "<{} private environment entries>",
                    self.raw_environment.len()
                ),
            )
            .field(
                "credential_material",
                &format_args!(
                    "<{} private credential values>",
                    self.credential_material.len()
                ),
            )
            .field(
                "stdout",
                &format_args!("<{} private bytes>", self.stdout.len()),
            )
            .field(
                "stderr",
                &format_args!("<{} private bytes>", self.stderr.len()),
            )
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct VerificationPreflightObservation {
    pub workspace: WorkspaceObservation,
    pub resolved_refs: Vec<ResolvedRef>,
    pub capabilities: Vec<CapabilityObservation>,
    pub resources: HostResourceObservation,
    pub cache: CacheObservation,
    pub selected_command: RepositoryCommandIdentity,
    pub requested_authorities: BTreeSet<RequestedAuthority>,
    #[serde(skip)]
    pub private_evidence: PrivateVerificationEvidence,
}

impl fmt::Debug for VerificationPreflightObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerificationPreflightObservation")
            .field("workspace", &self.workspace)
            .field("resolved_refs", &self.resolved_refs)
            .field("capabilities", &self.capabilities)
            .field("resources", &self.resources)
            .field("cache", &self.cache)
            .field("selected_command", &self.selected_command)
            .field("requested_authorities", &self.requested_authorities)
            .field("private_evidence", &"<retained private evidence>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreflightBlocker {
    WorkspaceIdentityMismatch,
    WorkspacePathMismatch,
    DirtyWorkspaceForbidden,
    WorkspaceResetRequired,
    WorkspaceResetWithoutAuthority,
    MovingRef,
    MissingRequiredCapability,
    MissingCommandCapability,
    MissingEquivalentCapability,
    UndeclaredFallback,
    EquivalentNotApplicable,
    InsufficientMemory,
    InsufficientSwap,
    CacheIdentityMismatch,
    LocalCommitWithoutAuthority,
    PublicationWithoutAuthority,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreflightReadiness {
    Ready,
    ReadyWithDeclaredDeviations,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectedCommandSource {
    Canonical,
    ApprovedEquivalent,
    Unrecognized,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct VerificationPreflightReport {
    schema_version: u8,
    profile_id: VerificationProfileId,
    workspace: RunnerOwnedWorkspaceIdentity,
    resolved_refs: Vec<ResolvedRef>,
    selected_command: RepositoryCommandIdentity,
    selected_command_source: SelectedCommandSource,
    target_scope: ExactVerificationScope,
    readiness: PreflightReadiness,
    blockers: Vec<PreflightBlocker>,
    deviations: Vec<DeclaredDeviation>,
    requested_authorities: BTreeSet<RequestedAuthority>,
    #[serde(skip)]
    private_evidence: PrivateVerificationEvidence,
}

impl VerificationPreflightReport {
    #[must_use]
    pub const fn readiness(&self) -> PreflightReadiness {
        self.readiness
    }

    #[must_use]
    pub fn blockers(&self) -> &[PreflightBlocker] {
        &self.blockers
    }

    #[must_use]
    pub fn deviations(&self) -> &[DeclaredDeviation] {
        &self.deviations
    }

    #[must_use]
    pub const fn selected_command(&self) -> &RepositoryCommandIdentity {
        &self.selected_command
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        format!(
            "profile={} readiness={:?} command={} blockers={} deviations={}",
            self.profile_id.as_str(),
            self.readiness,
            self.selected_command.command_id().as_str(),
            self.blockers.len(),
            self.deviations.len()
        )
    }
}

impl fmt::Debug for VerificationPreflightReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerificationPreflightReport")
            .field("schema_version", &self.schema_version)
            .field("profile_id", &self.profile_id)
            .field("workspace", &self.workspace)
            .field("resolved_refs", &self.resolved_refs)
            .field("selected_command", &self.selected_command)
            .field("selected_command_source", &self.selected_command_source)
            .field("target_scope", &self.target_scope)
            .field("readiness", &self.readiness)
            .field("blockers", &self.blockers)
            .field("deviations", &self.deviations)
            .field("requested_authorities", &self.requested_authorities)
            .field("private_evidence", &"<retained private evidence>")
            .finish()
    }
}

/// Evaluate a profile against already-observed, side-effect-free host and workspace evidence.
///
/// # Errors
///
/// Returns an error for duplicate or excessive observations. Expected environmental failures become
/// typed blockers in the returned report.
pub fn evaluate_verification_preflight(
    profile: &VerificationProfileContract,
    observation: VerificationPreflightObservation,
) -> Result<VerificationPreflightReport, VerificationProfileError> {
    if observation.resolved_refs.len() > MAX_PROFILE_REFS
        || observation.capabilities.len() > MAX_CAPABILITIES
    {
        return Err(VerificationProfileError::new(
            "preflight.observations",
            "observation_count_exceeded",
            "must remain within the profile ref and capability bounds",
        ));
    }
    let mut capability_map = BTreeMap::new();
    for entry in &observation.capabilities {
        if capability_map
            .insert(entry.capability.clone(), entry.available)
            .is_some()
        {
            return Err(VerificationProfileError::new(
                "preflight.capabilities",
                "duplicate_capability_observation",
                "must contain at most one observation per capability",
            ));
        }
    }

    let mut blockers = BTreeSet::new();
    let mut deviations = BTreeMap::new();

    if observation.workspace.installation_id != *profile.workspace.installation_id()
        || observation.workspace.workspace_id != *profile.workspace.workspace_id()
        || observation.workspace.repository != *profile.workspace.repository()
    {
        blockers.insert(PreflightBlocker::WorkspaceIdentityMismatch);
    }
    if observation.workspace.root.as_path() != profile.workspace.root.as_path() {
        blockers.insert(PreflightBlocker::WorkspacePathMismatch);
    }

    match observation.workspace.cleanliness {
        WorkspaceCleanliness::Clean => {}
        WorkspaceCleanliness::Dirty { .. } => match &profile.authority.workspace.dirty_workspace {
            DirtyWorkspacePolicy::RequireClean => {
                blockers.insert(PreflightBlocker::DirtyWorkspaceForbidden);
            }
            DirtyWorkspacePolicy::AllowDeclaredReset { deviation } => {
                if observation
                    .requested_authorities
                    .contains(&RequestedAuthority::WorkspaceReset)
                {
                    insert_deviation(&mut deviations, deviation.clone())?;
                } else {
                    blockers.insert(PreflightBlocker::WorkspaceResetRequired);
                }
            }
        },
    }
    if observation
        .requested_authorities
        .contains(&RequestedAuthority::WorkspaceReset)
        && profile.authority.workspace.authority
            != WorkspaceMutationAuthority::ResetRunnerOwnedWorkspace
    {
        blockers.insert(PreflightBlocker::WorkspaceResetWithoutAuthority);
    }

    if observation.resolved_refs.len() != profile.source.refs().len()
        || observation
            .resolved_refs
            .iter()
            .zip(profile.source.refs())
            .any(|(resolved, expected)| {
                resolved.ref_name != expected.ref_name
                    || resolved.commit != expected.expected_commit
            })
    {
        blockers.insert(PreflightBlocker::MovingRef);
    }

    for required in &profile.required_capabilities {
        if !capability_available(&capability_map, &required.capability) {
            blockers.insert(PreflightBlocker::MissingRequiredCapability);
        }
    }
    for optional in &profile.optional_capabilities {
        if !capability_available(&capability_map, &optional.capability) {
            insert_deviation(&mut deviations, optional.missing_deviation.clone())?;
        }
    }

    let missing_canonical = profile
        .canonical_command
        .required_capabilities()
        .iter()
        .filter(|capability| !capability_available(&capability_map, capability))
        .cloned()
        .collect::<BTreeSet<_>>();

    let (selected_source, selected_contract) =
        if observation.selected_command == *profile.canonical_command.identity() {
            if !missing_canonical.is_empty() {
                blockers.insert(PreflightBlocker::MissingCommandCapability);
            }
            (
                SelectedCommandSource::Canonical,
                Some(&profile.canonical_command),
            )
        } else if let Some(equivalent) = profile
            .approved_equivalents
            .iter()
            .find(|entry| observation.selected_command == *entry.command().identity())
        {
            if missing_canonical.is_empty() {
                blockers.insert(PreflightBlocker::EquivalentNotApplicable);
            }
            let replaced = equivalent
                .replaces_capabilities()
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>();
            if !missing_canonical.is_subset(&replaced) {
                blockers.insert(PreflightBlocker::MissingCommandCapability);
            }
            if equivalent
                .command()
                .required_capabilities()
                .iter()
                .any(|capability| !capability_available(&capability_map, capability))
            {
                blockers.insert(PreflightBlocker::MissingEquivalentCapability);
            }
            insert_deviation(&mut deviations, equivalent.deviation().clone())?;
            (
                SelectedCommandSource::ApprovedEquivalent,
                Some(equivalent.command()),
            )
        } else {
            blockers.insert(PreflightBlocker::UndeclaredFallback);
            (SelectedCommandSource::Unrecognized, None)
        };

    if observation.resources.available_memory_bytes
        < profile.resources.memory.minimum_available_bytes
    {
        blockers.insert(PreflightBlocker::InsufficientMemory);
    }
    if observation.resources.available_swap_bytes < profile.resources.memory.minimum_swap_bytes {
        blockers.insert(PreflightBlocker::InsufficientSwap);
    }
    if observation.cache.cache_id != *profile.cache.cache_id()
        || observation.cache.owner_workspace_id != *profile.cache.owner_workspace_id()
        || observation.cache.namespace_digest != *profile.cache.namespace_digest()
    {
        blockers.insert(PreflightBlocker::CacheIdentityMismatch);
    }
    if observation
        .requested_authorities
        .contains(&RequestedAuthority::LocalCommit)
        && profile.authority.local_commit == LocalCommitAuthority::Forbidden
    {
        blockers.insert(PreflightBlocker::LocalCommitWithoutAuthority);
    }
    if observation
        .requested_authorities
        .contains(&RequestedAuthority::Publication)
        && matches!(
            profile.authority.publication,
            PublicationAuthority::Forbidden
        )
    {
        blockers.insert(PreflightBlocker::PublicationWithoutAuthority);
    }

    let readiness = if blockers.is_empty() {
        if deviations.is_empty() {
            PreflightReadiness::Ready
        } else {
            PreflightReadiness::ReadyWithDeclaredDeviations
        }
    } else {
        PreflightReadiness::Blocked
    };
    let target_scope = selected_contract
        .unwrap_or(&profile.canonical_command)
        .test_scope()
        .clone();

    Ok(VerificationPreflightReport {
        schema_version: VERIFICATION_PROFILE_SCHEMA_VERSION,
        profile_id: profile.profile_id.clone(),
        workspace: profile.workspace.clone(),
        resolved_refs: observation.resolved_refs,
        selected_command: observation.selected_command,
        selected_command_source: selected_source,
        target_scope,
        readiness,
        blockers: blockers.into_iter().collect(),
        deviations: deviations.into_values().collect(),
        requested_authorities: observation.requested_authorities,
        private_evidence: observation.private_evidence,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationTestOutcome {
    Passed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PhaseTiming {
    pub phase: VerificationPhase,
    pub milliseconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheUse {
    Hit,
    Partial,
    Miss,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheUseRecord {
    pub cache_id: CacheId,
    pub use_state: CacheUse,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkipRecord {
    pub phase: VerificationPhase,
    pub code: SkipCode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetryRecord {
    pub phase: VerificationPhase,
    pub code: RetryCode,
    pub attempts: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CleanupStatus {
    NotRequired,
    Complete,
    Incomplete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LocalCommitState {
    Forbidden,
    NotRequested,
    RequestedButUncreated,
    Created { commit: CommitId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PublicationState {
    Forbidden,
    NotRequested,
    PublisherHandoffReady { candidate: CommitId },
    Published { commit: CommitId },
}

#[derive(Clone, PartialEq, Eq)]
pub struct VerificationExecutionEvidence {
    pub tested_source: TestedSourceIdentity,
    pub command: RepositoryCommandIdentity,
    pub target_scope: ExactVerificationScope,
    pub test_outcome: VerificationTestOutcome,
    pub phase_timings: Vec<PhaseTiming>,
    pub cache_use: CacheUseRecord,
    pub skips: Vec<SkipRecord>,
    pub retries: Vec<RetryRecord>,
    pub deviations: Vec<DeclaredDeviation>,
    pub cleanup: CleanupStatus,
    pub local_commit: LocalCommitState,
    pub publication: PublicationState,
    pub private_evidence: PrivateVerificationEvidence,
}

impl fmt::Debug for VerificationExecutionEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerificationExecutionEvidence")
            .field("tested_source", &self.tested_source)
            .field("command", &self.command)
            .field("target_scope", &self.target_scope)
            .field("test_outcome", &self.test_outcome)
            .field("phase_timings", &self.phase_timings)
            .field("cache_use", &self.cache_use)
            .field("skips", &self.skips)
            .field("retries", &self.retries)
            .field("deviations", &self.deviations)
            .field("cleanup", &self.cleanup)
            .field("local_commit", &self.local_commit)
            .field("publication", &self.publication)
            .field("private_evidence", &"<retained private evidence>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationResultDisposition {
    Passed,
    Failed,
    CleanupIncomplete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImmutableVerificationIdentity {
    schema_version: u8,
    receipt_digest: Sha256Digest,
    profile_id: VerificationProfileId,
    repository: RepositoryRef,
    tested_source: TestedSourceIdentity,
    command: RepositoryCommandIdentity,
    target_scope: ExactVerificationScope,
    disposition: VerificationResultDisposition,
}

impl ImmutableVerificationIdentity {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn receipt_digest(&self) -> &Sha256Digest {
        &self.receipt_digest
    }

    #[must_use]
    pub const fn profile_id(&self) -> &VerificationProfileId {
        &self.profile_id
    }

    #[must_use]
    pub const fn repository(&self) -> &RepositoryRef {
        &self.repository
    }

    #[must_use]
    pub const fn tested_source(&self) -> &TestedSourceIdentity {
        &self.tested_source
    }

    #[must_use]
    pub fn tested_commit(&self) -> Option<&CommitId> {
        match &self.tested_source {
            TestedSourceIdentity::Commit { commit, .. } => Some(commit),
            TestedSourceIdentity::SyntheticTree { .. } => None,
        }
    }

    #[must_use]
    pub const fn tested_tree(&self) -> &GitTreeId {
        match &self.tested_source {
            TestedSourceIdentity::Commit { tree, .. }
            | TestedSourceIdentity::SyntheticTree { tree } => tree,
        }
    }

    #[must_use]
    pub const fn command(&self) -> &RepositoryCommandIdentity {
        &self.command
    }

    #[must_use]
    pub const fn target_scope(&self) -> &ExactVerificationScope {
        &self.target_scope
    }

    #[must_use]
    pub const fn disposition(&self) -> VerificationResultDisposition {
        self.disposition
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct VerificationResult {
    schema_version: u8,
    profile_id: VerificationProfileId,
    workspace: RunnerOwnedWorkspaceIdentity,
    resolved_refs: Vec<ResolvedRef>,
    tested_source: TestedSourceIdentity,
    command: RepositoryCommandIdentity,
    target_scope: ExactVerificationScope,
    test_outcome: VerificationTestOutcome,
    disposition: VerificationResultDisposition,
    phase_timings: Vec<PhaseTiming>,
    cache_use: CacheUseRecord,
    skips: Vec<SkipRecord>,
    retries: Vec<RetryRecord>,
    deviations: Vec<DeclaredDeviation>,
    cleanup: CleanupStatus,
    local_commit: LocalCommitState,
    publication: PublicationState,
    #[serde(skip)]
    private_preflight_evidence: PrivateVerificationEvidence,
    #[serde(skip)]
    private_execution_evidence: PrivateVerificationEvidence,
}

impl VerificationResult {
    #[must_use]
    pub const fn disposition(&self) -> VerificationResultDisposition {
        self.disposition
    }

    #[must_use]
    pub fn immutable_identity(
        &self,
        receipt_digest: Sha256Digest,
    ) -> ImmutableVerificationIdentity {
        ImmutableVerificationIdentity {
            schema_version: VERIFICATION_PROFILE_SCHEMA_VERSION,
            receipt_digest,
            profile_id: self.profile_id.clone(),
            repository: self.workspace.repository().clone(),
            tested_source: self.tested_source.clone(),
            command: self.command.clone(),
            target_scope: self.target_scope.clone(),
            disposition: self.disposition,
        }
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        format!(
            "profile={} result={:?} command={} refs={} timings={} skips={} retries={} deviations={}",
            self.profile_id.as_str(),
            self.disposition,
            self.command.command_id().as_str(),
            self.resolved_refs.len(),
            self.phase_timings.len(),
            self.skips.len(),
            self.retries.len(),
            self.deviations.len()
        )
    }
}

impl fmt::Debug for VerificationResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerificationResult")
            .field("schema_version", &self.schema_version)
            .field("profile_id", &self.profile_id)
            .field("workspace", &self.workspace)
            .field("resolved_refs", &self.resolved_refs)
            .field("tested_source", &self.tested_source)
            .field("command", &self.command)
            .field("target_scope", &self.target_scope)
            .field("test_outcome", &self.test_outcome)
            .field("disposition", &self.disposition)
            .field("phase_timings", &self.phase_timings)
            .field("cache_use", &self.cache_use)
            .field("skips", &self.skips)
            .field("retries", &self.retries)
            .field("deviations", &self.deviations)
            .field("cleanup", &self.cleanup)
            .field("local_commit", &self.local_commit)
            .field("publication", &self.publication)
            .field("private_preflight_evidence", &"<retained private evidence>")
            .field("private_execution_evidence", &"<retained private evidence>")
            .finish()
    }
}

/// Build one bounded public verification result from a ready preflight report and typed evidence.
///
/// # Errors
///
/// Returns an error when the preflight is blocked, exact source/command/scope/cache identities drift,
/// result fields exceed bounds, deviations are undeclared, or commit/publication state exceeds profile
/// authority.
pub fn finalize_verification_result(
    profile: &VerificationProfileContract,
    preflight: VerificationPreflightReport,
    execution: VerificationExecutionEvidence,
) -> Result<VerificationResult, VerificationProfileError> {
    if preflight.readiness == PreflightReadiness::Blocked {
        return Err(VerificationProfileError::new(
            "result.preflight",
            "blocked_preflight",
            "cannot finalize a verification result from a blocked preflight",
        ));
    }
    if preflight.profile_id != *profile.profile_id()
        || preflight.tested_command_contract(profile).is_none()
        || execution.command != preflight.selected_command
        || execution.target_scope != preflight.target_scope
        || execution.tested_source != *profile.source.expected_tested_source()
        || execution.cache_use.cache_id != *profile.cache.cache_id()
    {
        return Err(VerificationProfileError::new(
            "result.identity",
            "verification_identity_mismatch",
            "must preserve the exact profile, command, target, source, and cache identities from preflight",
        ));
    }
    validate_execution_bounds(&execution)?;

    let mut deviations = BTreeMap::new();
    for deviation in preflight
        .deviations
        .iter()
        .chain(execution.deviations.iter())
    {
        let Some(declared) = profile.declared_deviation(&deviation.code) else {
            return Err(VerificationProfileError::new(
                "result.deviations",
                "undeclared_deviation",
                "every result deviation must be declared by the selected profile",
            ));
        };
        if declared != deviation {
            return Err(VerificationProfileError::new(
                "result.deviations",
                "deviation_definition_mismatch",
                "deviation code and public summary must match the profile declaration exactly",
            ));
        }
        insert_deviation(&mut deviations, deviation.clone())?;
    }

    validate_local_commit_state(profile, &preflight, &execution.local_commit)?;
    validate_publication_state(profile, &preflight, &execution.publication)?;

    let disposition = if execution.cleanup == CleanupStatus::Incomplete {
        VerificationResultDisposition::CleanupIncomplete
    } else if execution.test_outcome == VerificationTestOutcome::Passed {
        VerificationResultDisposition::Passed
    } else {
        VerificationResultDisposition::Failed
    };

    Ok(VerificationResult {
        schema_version: VERIFICATION_PROFILE_SCHEMA_VERSION,
        profile_id: preflight.profile_id,
        workspace: preflight.workspace,
        resolved_refs: preflight.resolved_refs,
        tested_source: execution.tested_source,
        command: execution.command,
        target_scope: execution.target_scope,
        test_outcome: execution.test_outcome,
        disposition,
        phase_timings: execution.phase_timings,
        cache_use: execution.cache_use,
        skips: execution.skips,
        retries: execution.retries,
        deviations: deviations.into_values().collect(),
        cleanup: execution.cleanup,
        local_commit: execution.local_commit,
        publication: execution.publication,
        private_preflight_evidence: preflight.private_evidence,
        private_execution_evidence: execution.private_evidence,
    })
}

impl VerificationPreflightReport {
    fn tested_command_contract<'a>(
        &self,
        profile: &'a VerificationProfileContract,
    ) -> Option<&'a RepositoryCommandContract> {
        if self.selected_command == *profile.canonical_command.identity() {
            Some(&profile.canonical_command)
        } else {
            profile
                .approved_equivalents
                .iter()
                .find(|equivalent| self.selected_command == *equivalent.command().identity())
                .map(ApprovedEquivalentCommand::command)
        }
    }
}

fn validate_execution_bounds(
    execution: &VerificationExecutionEvidence,
) -> Result<(), VerificationProfileError> {
    if execution.phase_timings.len() > MAX_PHASE_RECORDS
        || execution.skips.len() > MAX_SKIP_RECORDS
        || execution.retries.len() > MAX_RETRY_RECORDS
        || execution.deviations.len() > MAX_DECLARED_DEVIATIONS
    {
        return Err(VerificationProfileError::new(
            "result.records",
            "result_record_count_exceeded",
            "phase, skip, retry, or deviation records exceed their public bounds",
        ));
    }
    let mut phases = BTreeSet::new();
    if execution
        .phase_timings
        .iter()
        .any(|timing| timing.milliseconds > MAX_PHASE_MILLISECONDS || !phases.insert(timing.phase))
    {
        return Err(VerificationProfileError::new(
            "result.phase_timings",
            "invalid_phase_timings",
            "must contain unique bounded phase timings",
        ));
    }
    if execution
        .retries
        .iter()
        .any(|retry| retry.attempts == 0 || retry.attempts > 16)
    {
        return Err(VerificationProfileError::new(
            "result.retries",
            "invalid_retry_record",
            "retry attempts must be between 1 and 16",
        ));
    }
    Ok(())
}

fn validate_local_commit_state(
    profile: &VerificationProfileContract,
    preflight: &VerificationPreflightReport,
    state: &LocalCommitState,
) -> Result<(), VerificationProfileError> {
    let requested = preflight
        .requested_authorities
        .contains(&RequestedAuthority::LocalCommit);
    let valid = matches!(
        (profile.authority.local_commit, requested, state),
        (
            LocalCommitAuthority::Forbidden,
            false,
            LocalCommitState::Forbidden
        ) | (
            LocalCommitAuthority::CreateInRunnerOwnedWorkspace,
            false,
            LocalCommitState::NotRequested,
        ) | (
            LocalCommitAuthority::CreateInRunnerOwnedWorkspace,
            true,
            LocalCommitState::RequestedButUncreated | LocalCommitState::Created { .. },
        )
    );
    if valid {
        Ok(())
    } else {
        Err(VerificationProfileError::new(
            "result.local_commit",
            "local_commit_authority_mismatch",
            "must match the profile authority and the preflight request exactly",
        ))
    }
}

fn validate_publication_state(
    profile: &VerificationProfileContract,
    preflight: &VerificationPreflightReport,
    state: &PublicationState,
) -> Result<(), VerificationProfileError> {
    let requested = preflight
        .requested_authorities
        .contains(&RequestedAuthority::Publication);
    let valid = matches!(
        (&profile.authority.publication, requested, state),
        (
            PublicationAuthority::Forbidden,
            false,
            PublicationState::Forbidden
        ) | (
            PublicationAuthority::CredentialedPublisherOnly { .. },
            false,
            PublicationState::NotRequested,
        ) | (
            PublicationAuthority::CredentialedPublisherOnly { .. },
            true,
            PublicationState::PublisherHandoffReady { .. } | PublicationState::Published { .. },
        )
    );
    if valid {
        Ok(())
    } else {
        Err(VerificationProfileError::new(
            "result.publication",
            "publication_authority_mismatch",
            "must match the profile authority and the preflight request exactly",
        ))
    }
}

fn validate_identifier(
    field: &'static str,
    value: &str,
    maximum_length: usize,
) -> Result<(), VerificationProfileError> {
    let valid = !value.is_empty()
        && value.len() <= maximum_length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(VerificationProfileError::new(
            field,
            "invalid_identifier",
            "must use bounded ASCII letters, digits, '.', '_', or '-'",
        ))
    }
}

fn validate_public_text(
    field: &'static str,
    value: &str,
    maximum_length: usize,
) -> Result<(), VerificationProfileError> {
    if value.is_empty() || value.len() > maximum_length || value.chars().any(char::is_control) {
        Err(VerificationProfileError::new(
            field,
            "invalid_public_text",
            "must be non-empty, bounded, and free of control characters",
        ))
    } else {
        Ok(())
    }
}

fn validate_unique_capabilities(
    field: &'static str,
    capabilities: &[CapabilityId],
) -> Result<(), VerificationProfileError> {
    if capabilities.len() > MAX_CAPABILITIES {
        return Err(VerificationProfileError::new(
            field,
            "capability_count_exceeded",
            format!("must contain at most {MAX_CAPABILITIES} capabilities"),
        ));
    }
    let mut seen = BTreeSet::new();
    if capabilities
        .iter()
        .any(|capability| !seen.insert(capability))
    {
        return Err(VerificationProfileError::new(
            field,
            "duplicate_capability",
            "must not contain duplicate capability identities",
        ));
    }
    Ok(())
}

fn insert_deviation(
    deviations: &mut BTreeMap<DeviationCode, DeclaredDeviation>,
    deviation: DeclaredDeviation,
) -> Result<(), VerificationProfileError> {
    if let Some(existing) = deviations.get(&deviation.code) {
        if existing != &deviation {
            return Err(VerificationProfileError::new(
                "deviation.code",
                "conflicting_deviation_definition",
                "one deviation code must have one exact public definition",
            ));
        }
        return Ok(());
    }
    deviations.insert(deviation.code.clone(), deviation);
    Ok(())
}

fn capability_available(
    observations: &BTreeMap<CapabilityId, bool>,
    capability: &CapabilityId,
) -> bool {
    observations.get(capability).copied().unwrap_or(false)
}

fn valid_repository_ref_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !value.starts_with(['/', '.'])
        && !value.ends_with(['/', '.'])
        && !value.contains("..")
        && !value.contains("@{")
        && !value.chars().any(|character| {
            character.is_control()
                || character.is_whitespace()
                || matches!(character, '~' | '^' | ':' | '?' | '*' | '[' | '\\')
        })
        && value.split('/').all(|component| {
            !component.is_empty()
                && component != "."
                && component != ".."
                && !component.ends_with(".lock")
        })
}

fn valid_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && path.components().all(|component| {
            matches!(
                component,
                Component::Prefix(_) | Component::RootDir | Component::Normal(_)
            )
        })
}

fn valid_git_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerificationProfileError {
    pub field: String,
    pub code: String,
    pub problem: String,
}

impl VerificationProfileError {
    fn new(field: impl Into<String>, code: impl Into<String>, problem: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            code: code.into(),
            problem: problem.into(),
        }
    }
}

impl fmt::Display for VerificationProfileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}: {}", self.field, self.code, self.problem)
    }
}

impl std::error::Error for VerificationProfileError {}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKSPACE_ROOT: &str = "/var/lib/smolrunner/workspaces/example";
    const CACHE_PATH: &str = "/var/lib/smolrunner/workspaces/example/cache/target";

    fn repository() -> RepositoryRef {
        RepositoryRef::parse("example/project").expect("repository")
    }

    fn commit(byte: &str) -> CommitId {
        CommitId::parse(&byte.repeat(20)).expect("commit")
    }

    fn tree(byte: &str) -> GitTreeId {
        GitTreeId::parse(&byte.repeat(20)).expect("tree")
    }

    fn digest(byte: &str) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", byte.repeat(32))).expect("digest")
    }

    fn capability(value: &str) -> CapabilityId {
        CapabilityId::parse(value).expect("capability")
    }

    fn deviation(code: &str, summary: &str) -> DeclaredDeviation {
        DeclaredDeviation::new(DeviationCode::parse(code).expect("code"), summary)
            .expect("deviation")
    }

    fn workspace() -> RunnerOwnedWorkspaceIdentity {
        RunnerOwnedWorkspaceIdentity::new(
            RunnerInstallationId::parse("runner-a").expect("installation"),
            RunnerWorkspaceId::parse("workspace-a").expect("workspace"),
            repository(),
            WORKSPACE_ROOT,
        )
        .expect("workspace")
    }

    fn source() -> ImmutableSourceInputs {
        ImmutableSourceInputs::new(
            repository(),
            vec![ImmutableRefInput::new(
                RepositoryRefName::parse("refs/heads/main").expect("ref"),
                commit("11"),
            )],
            SourceComposition::SingleRef,
            TestedSourceIdentity::Commit {
                commit: commit("11"),
                tree: tree("22"),
            },
        )
        .expect("source")
    }

    fn scope() -> ExactVerificationScope {
        ExactVerificationScope::FilteredTest {
            target: KnownTestTarget::IntegrationTestBinary {
                package: PackageId::parse("core").expect("package"),
                binary: TargetName::parse("orphan_sessions").expect("binary"),
            },
            filter: TestFilter::parse("large_output").expect("filter"),
        }
    }

    fn command(id: &str, required_capabilities: Vec<CapabilityId>) -> RepositoryCommandContract {
        let scope = scope();
        RepositoryCommandContract::new(
            RepositoryCommandIdentity::new(
                repository(),
                RepositoryCommandId::parse(id).expect("command id"),
                digest("ab"),
            ),
            scope.clone(),
            scope.required_build_scope(),
            required_capabilities,
        )
        .expect("command")
    }

    fn profile_with(
        optional: bool,
        allow_reset: bool,
        publication: PublicationAuthority,
    ) -> VerificationProfileContract {
        let workspace = workspace();
        let canonical = command("canonical", vec![capability("just")]);
        let equivalent = ApprovedEquivalentCommand::new(
            &canonical,
            command("cargo-equivalent", vec![capability("cargo")]),
            vec![capability("just")],
            deviation(
                "approved-equivalent",
                "repository-approved cargo equivalent used",
            ),
        )
        .expect("equivalent");
        let dirty_workspace = if allow_reset {
            DirtyWorkspacePolicy::AllowDeclaredReset {
                deviation: deviation("workspace-reset", "runner-owned workspace reset required"),
            }
        } else {
            DirtyWorkspacePolicy::RequireClean
        };
        VerificationProfileContract::new(VerificationProfileDefinition {
            profile_id: VerificationProfileId::parse("orphan-one").expect("profile"),
            workspace: workspace.clone(),
            source: source(),
            required_capabilities: vec![RequiredCapability::new(capability("git"))],
            optional_capabilities: if optional {
                vec![OptionalCapability::new(
                    capability("nextest"),
                    deviation(
                        "optional-nextest-missing",
                        "optional nextest capability unavailable",
                    ),
                )]
            } else {
                Vec::new()
            },
            canonical_command: canonical,
            approved_equivalents: vec![equivalent],
            resources: ResourceDefaults::new(
                MemoryPolicy::new(1_024, 512, 1_536).expect("memory"),
                ConcurrencyPolicy::new(2, 1, 1).expect("concurrency"),
            ),
            cache: CacheIdentity::new(
                &workspace,
                CacheId::parse("cargo-target").expect("cache"),
                workspace.workspace_id().clone(),
                digest("cd"),
                CACHE_PATH,
            )
            .expect("cache"),
            timeout: TimeoutPolicy::new(
                3_600,
                vec![PhaseTimeout {
                    phase: VerificationPhase::Test,
                    seconds: 3_000,
                }],
            )
            .expect("timeout"),
            authority: VerificationAuthorityPolicy {
                workspace: WorkspaceMutationPolicy::new(
                    if allow_reset {
                        WorkspaceMutationAuthority::ResetRunnerOwnedWorkspace
                    } else {
                        WorkspaceMutationAuthority::ReadOnly
                    },
                    dirty_workspace,
                )
                .expect("workspace policy"),
                local_commit: LocalCommitAuthority::Forbidden,
                publication,
            },
            additional_declared_deviations: Vec::new(),
        })
        .expect("profile")
    }

    fn observation_for(
        profile: &VerificationProfileContract,
        selected_command: RepositoryCommandIdentity,
    ) -> VerificationPreflightObservation {
        VerificationPreflightObservation {
            workspace: WorkspaceObservation::new(
                profile.workspace.installation_id().clone(),
                profile.workspace.workspace_id().clone(),
                repository(),
                WORKSPACE_ROOT,
                WorkspaceCleanliness::Clean,
            )
            .expect("observation workspace"),
            resolved_refs: vec![ResolvedRef {
                ref_name: RepositoryRefName::parse("refs/heads/main").expect("ref"),
                commit: commit("11"),
            }],
            capabilities: vec![
                CapabilityObservation {
                    capability: capability("git"),
                    available: true,
                },
                CapabilityObservation {
                    capability: capability("just"),
                    available: true,
                },
                CapabilityObservation {
                    capability: capability("cargo"),
                    available: true,
                },
                CapabilityObservation {
                    capability: capability("nextest"),
                    available: true,
                },
            ],
            resources: HostResourceObservation {
                available_memory_bytes: 1_024,
                available_swap_bytes: 512,
            },
            cache: CacheObservation {
                cache_id: profile.cache.cache_id().clone(),
                owner_workspace_id: profile.workspace.workspace_id().clone(),
                namespace_digest: profile.cache.namespace_digest().clone(),
                present: true,
            },
            selected_command,
            requested_authorities: BTreeSet::new(),
            private_evidence: PrivateVerificationEvidence::default(),
        }
    }

    #[test]
    fn workspace_path_escape_is_rejected() {
        let workspace = workspace();
        let error = CacheIdentity::new(
            &workspace,
            CacheId::parse("escape").expect("cache"),
            workspace.workspace_id().clone(),
            digest("aa"),
            "/tmp/escaped-cache",
        )
        .expect_err("path escape must fail");
        assert_eq!(error.code, "workspace_path_escape");
    }

    #[test]
    fn moving_refs_block_preflight() {
        let profile = profile_with(false, false, PublicationAuthority::Forbidden);
        let mut observation =
            observation_for(&profile, profile.canonical_command.identity().clone());
        observation.resolved_refs[0].commit = commit("22");
        let report = evaluate_verification_preflight(&profile, observation).expect("report");
        assert_eq!(report.readiness(), PreflightReadiness::Blocked);
        assert!(report.blockers().contains(&PreflightBlocker::MovingRef));
    }

    #[test]
    fn missing_required_tools_block_preflight() {
        let profile = profile_with(false, false, PublicationAuthority::Forbidden);
        let mut observation =
            observation_for(&profile, profile.canonical_command.identity().clone());
        observation
            .capabilities
            .iter_mut()
            .find(|entry| entry.capability == capability("git"))
            .expect("git")
            .available = false;
        let report = evaluate_verification_preflight(&profile, observation).expect("report");
        assert!(
            report
                .blockers()
                .contains(&PreflightBlocker::MissingRequiredCapability)
        );
    }

    #[test]
    fn missing_optional_tools_are_declared_deviations() {
        let profile = profile_with(true, false, PublicationAuthority::Forbidden);
        let mut observation =
            observation_for(&profile, profile.canonical_command.identity().clone());
        observation
            .capabilities
            .iter_mut()
            .find(|entry| entry.capability == capability("nextest"))
            .expect("nextest")
            .available = false;
        let report = evaluate_verification_preflight(&profile, observation).expect("report");
        assert_eq!(
            report.readiness(),
            PreflightReadiness::ReadyWithDeclaredDeviations
        );
        assert_eq!(
            report.deviations()[0].code.as_str(),
            "optional-nextest-missing"
        );
    }

    #[test]
    fn undeclared_fallback_blocks_preflight() {
        let profile = profile_with(false, false, PublicationAuthority::Forbidden);
        let unknown = RepositoryCommandIdentity::new(
            repository(),
            RepositoryCommandId::parse("unknown").expect("command"),
            digest("ef"),
        );
        let report = evaluate_verification_preflight(&profile, observation_for(&profile, unknown))
            .expect("report");
        assert!(
            report
                .blockers()
                .contains(&PreflightBlocker::UndeclaredFallback)
        );
    }

    #[test]
    fn widened_build_scope_is_rejected() {
        let scope = ExactVerificationScope::LibraryTests {
            package: PackageId::parse("core").expect("package"),
        };
        let error = RepositoryCommandContract::new(
            RepositoryCommandIdentity::new(
                repository(),
                RepositoryCommandId::parse("wide").expect("command"),
                digest("ab"),
            ),
            scope,
            ExactBuildScope::WholePackage {
                package: PackageId::parse("core").expect("package"),
            },
            vec![capability("cargo")],
        )
        .expect_err("scope widening must fail");
        assert_eq!(error.code, "widened_build_scope");
    }

    #[test]
    fn invalid_memory_policy_is_rejected() {
        assert_eq!(
            MemoryPolicy::new(1_024, 0, 2_048)
                .expect_err("uncovered peak")
                .code,
            "invalid_memory_policy"
        );
    }

    #[test]
    fn invalid_concurrency_policy_is_rejected() {
        assert_eq!(
            ConcurrencyPolicy::new(1, 2, 1)
                .expect_err("linkers exceed build jobs")
                .code,
            "invalid_concurrency_policy"
        );
    }

    #[test]
    fn dirty_workspace_policy_fails_closed_or_declares_reset() {
        let read_only = profile_with(false, false, PublicationAuthority::Forbidden);
        let mut observation =
            observation_for(&read_only, read_only.canonical_command.identity().clone());
        observation.workspace.cleanliness = WorkspaceCleanliness::Dirty {
            changed_path_count: 1,
        };
        let blocked = evaluate_verification_preflight(&read_only, observation).expect("report");
        assert!(
            blocked
                .blockers()
                .contains(&PreflightBlocker::DirtyWorkspaceForbidden)
        );

        let reset = profile_with(false, true, PublicationAuthority::Forbidden);
        let mut observation = observation_for(&reset, reset.canonical_command.identity().clone());
        observation.workspace.cleanliness = WorkspaceCleanliness::Dirty {
            changed_path_count: 1,
        };
        observation
            .requested_authorities
            .insert(RequestedAuthority::WorkspaceReset);
        let ready = evaluate_verification_preflight(&reset, observation).expect("report");
        assert_eq!(
            ready.readiness(),
            PreflightReadiness::ReadyWithDeclaredDeviations
        );
        assert_eq!(ready.deviations()[0].code.as_str(), "workspace-reset");
    }

    #[test]
    fn secret_and_private_path_values_do_not_leak() {
        let profile = profile_with(false, false, PublicationAuthority::Forbidden);
        let mut observation =
            observation_for(&profile, profile.canonical_command.identity().clone());
        observation.private_evidence = PrivateVerificationEvidence::new(
            vec![PathBuf::from("/private/home/runner/workspace")],
            BTreeMap::from([("TOKEN".to_owned(), "secret-token-value".to_owned())]),
            vec!["credential-value".to_owned()],
            b"stdout-secret".to_vec(),
            b"stderr-secret".to_vec(),
        );
        let preflight = evaluate_verification_preflight(&profile, observation).expect("preflight");
        let execution = VerificationExecutionEvidence {
            tested_source: profile.source.expected_tested_source().clone(),
            command: profile.canonical_command.identity().clone(),
            target_scope: profile.canonical_command.test_scope().clone(),
            test_outcome: VerificationTestOutcome::Passed,
            phase_timings: vec![PhaseTiming {
                phase: VerificationPhase::Test,
                milliseconds: 42,
            }],
            cache_use: CacheUseRecord {
                cache_id: profile.cache.cache_id().clone(),
                use_state: CacheUse::Hit,
            },
            skips: Vec::new(),
            retries: Vec::new(),
            deviations: Vec::new(),
            cleanup: CleanupStatus::Complete,
            local_commit: LocalCommitState::Forbidden,
            publication: PublicationState::Forbidden,
            private_evidence: PrivateVerificationEvidence::new(
                vec![PathBuf::from("/private/result/path")],
                BTreeMap::from([("RAW_ENV".to_owned(), "raw-env-secret".to_owned())]),
                vec!["publish-token".to_owned()],
                b"private stdout".to_vec(),
                b"private stderr".to_vec(),
            ),
        };
        let result = finalize_verification_result(&profile, preflight, execution).expect("result");
        let json = serde_json::to_string(&result).expect("json");
        let debug = format!("{result:?}");
        for secret in [
            WORKSPACE_ROOT,
            CACHE_PATH,
            "/private/home/runner/workspace",
            "/private/result/path",
            "secret-token-value",
            "credential-value",
            "stdout-secret",
            "stderr-secret",
            "raw-env-secret",
            "publish-token",
            "private stdout",
            "private stderr",
        ] {
            assert!(!json.contains(secret), "json leaked {secret}");
            assert!(!debug.contains(secret), "debug leaked {secret}");
        }
    }

    #[test]
    fn invalid_cache_ownership_is_rejected() {
        let workspace = workspace();
        let error = CacheIdentity::new(
            &workspace,
            CacheId::parse("foreign").expect("cache"),
            RunnerWorkspaceId::parse("workspace-b").expect("workspace"),
            digest("aa"),
            CACHE_PATH,
        )
        .expect_err("foreign owner must fail");
        assert_eq!(error.code, "invalid_cache_ownership");
    }

    #[test]
    fn invalid_timeout_settings_are_rejected() {
        assert_eq!(
            TimeoutPolicy::new(
                10,
                vec![PhaseTimeout {
                    phase: VerificationPhase::Test,
                    seconds: 11,
                }],
            )
            .expect_err("phase exceeds total")
            .code,
            "invalid_timeout_settings"
        );
    }

    #[test]
    fn publication_requested_without_authority_is_blocked() {
        let profile = profile_with(false, false, PublicationAuthority::Forbidden);
        let mut observation =
            observation_for(&profile, profile.canonical_command.identity().clone());
        observation
            .requested_authorities
            .insert(RequestedAuthority::Publication);
        let report = evaluate_verification_preflight(&profile, observation).expect("report");
        assert!(
            report
                .blockers()
                .contains(&PreflightBlocker::PublicationWithoutAuthority)
        );
    }

    #[test]
    fn repository_ref_names_reject_revision_syntax_and_invalid_components() {
        assert!(RepositoryRefName::parse("refs/heads/feature/nested").is_ok());
        for value in [
            "refs/heads/main~1",
            "refs/heads/main^",
            "refs/heads/main:path",
            "refs/heads/main?",
            "refs/heads/main*",
            "refs/heads/main[",
            "refs/heads/main.lock",
            "refs/heads/topic.lock/child",
            "refs/heads//main",
            "refs/heads/./main",
            "refs/heads/../main",
            "/refs/heads/main",
            "refs/heads/main/",
            ".refs/heads/main",
            "refs/heads/main.",
            "refs\\heads\\main",
            "refs/heads/main name",
            "refs/heads/main\n",
            "refs/heads/main@{1}",
        ] {
            assert!(
                RepositoryRefName::parse(value).is_err(),
                "accepted invalid ref {value:?}"
            );
        }
    }

    #[test]
    fn immutable_verification_identity_is_public_and_authority_free() {
        let profile = profile_with(
            false,
            false,
            PublicationAuthority::CredentialedPublisherOnly {
                repository: repository(),
                target_ref: RepositoryRefName::parse("refs/heads/release").expect("target ref"),
            },
        );
        let preflight = evaluate_verification_preflight(
            &profile,
            observation_for(&profile, profile.canonical_command.identity().clone()),
        )
        .expect("preflight");
        let result = finalize_verification_result(
            &profile,
            preflight,
            VerificationExecutionEvidence {
                tested_source: profile.source.expected_tested_source().clone(),
                command: profile.canonical_command.identity().clone(),
                target_scope: profile.canonical_command.test_scope().clone(),
                test_outcome: VerificationTestOutcome::Passed,
                phase_timings: vec![PhaseTiming {
                    phase: VerificationPhase::Test,
                    milliseconds: 42,
                }],
                cache_use: CacheUseRecord {
                    cache_id: profile.cache.cache_id().clone(),
                    use_state: CacheUse::Hit,
                },
                skips: Vec::new(),
                retries: Vec::new(),
                deviations: Vec::new(),
                cleanup: CleanupStatus::Complete,
                local_commit: LocalCommitState::Forbidden,
                publication: PublicationState::NotRequested,
                private_evidence: PrivateVerificationEvidence::new(
                    vec![PathBuf::from("/private/identity/path")],
                    BTreeMap::from([("TOKEN".to_owned(), "identity-secret".to_owned())]),
                    vec!["identity-credential".to_owned()],
                    b"identity stdout".to_vec(),
                    b"identity stderr".to_vec(),
                ),
            },
        )
        .expect("result");
        let identity = result.immutable_identity(digest("ef"));
        assert_eq!(
            identity.schema_version(),
            VERIFICATION_PROFILE_SCHEMA_VERSION
        );
        assert_eq!(identity.receipt_digest(), &digest("ef"));
        assert_eq!(identity.repository(), &repository());
        assert_eq!(identity.tested_commit(), Some(&commit("11")));
        assert_eq!(identity.tested_tree(), &tree("22"));
        assert_eq!(
            identity.disposition(),
            VerificationResultDisposition::Passed
        );
        let json = serde_json::to_string(&identity).expect("identity json");
        for private in [
            "publication",
            "refs/heads/release",
            "/private/identity/path",
            "identity-secret",
            "identity-credential",
            "identity stdout",
            "identity stderr",
        ] {
            assert!(!json.contains(private), "identity leaked {private}");
        }
    }

    #[test]
    fn ordered_composition_requires_a_synthetic_tree() {
        let source = ImmutableSourceInputs::new(
            repository(),
            vec![
                ImmutableRefInput::new(
                    RepositoryRefName::parse("refs/heads/base").expect("ref"),
                    commit("11"),
                ),
                ImmutableRefInput::new(
                    RepositoryRefName::parse("refs/heads/feature").expect("ref"),
                    commit("22"),
                ),
            ],
            SourceComposition::OrderedComposition,
            TestedSourceIdentity::SyntheticTree { tree: tree("33") },
        )
        .expect("composition");
        assert_eq!(source.refs().len(), 2);
    }
}
