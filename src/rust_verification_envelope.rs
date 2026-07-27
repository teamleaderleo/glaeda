use std::collections::BTreeSet;
use std::fmt;

use serde::Serialize;

use crate::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use crate::lima_lifecycle::{INTERACTIVE_MEMORY_BYTES, INTERACTIVE_VCPUS};
use crate::personal_worker_queue::{
    PERSONAL_WORKER_RESERVED_CPU_MILLIS, PERSONAL_WORKER_RESERVED_MEMORY_BYTES,
    PERSONAL_WORKER_SCHEDULABLE_CPU_MILLIS, PERSONAL_WORKER_SCHEDULABLE_MEMORY_BYTES,
    PersonalWorkerProfile,
};
use crate::verification_profile::{
    CacheId, CapabilityId, PackageId, RepositoryCommandIdentity, TargetName, VerificationProfileId,
};

pub const RUST_VERIFICATION_ENVELOPE_SCHEMA_VERSION: u8 = 1;
pub const MAX_RUST_FEATURES: usize = 64;
pub const MAX_RUST_NAMED_TARGETS: usize = 64;
pub const MAX_RUST_CAPABILITIES: usize = 64;
pub const MAX_RUST_CONCURRENCY: u16 = 256;
pub const MAX_RUST_EXECUTION_SECONDS: u64 = 86_400;

const MAX_IDENTIFIER_BYTES: usize = 128;
const MAX_FILTER_BYTES: usize = 512;

macro_rules! identifier_type {
    ($name:ident, $field:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Parse one bounded canonical Rust execution identifier.
            ///
            /// # Errors
            ///
            /// Returns an error for empty, oversized, non-ASCII, control-bearing, or path-shaped
            /// values.
            pub fn parse(value: &str) -> Result<Self, RustVerificationEnvelopeError> {
                validate_identifier($field, value)?;
                Ok(Self(value.to_owned()))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

identifier_type!(RustTargetTriple, "cargo.target_triple");
identifier_type!(RustCargoProfileName, "cargo.profile");
identifier_type!(RustFeatureName, "cargo.features");
identifier_type!(RustRetryPolicyId, "retry.policy_id");

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct RustTestFilter(String);

impl RustTestFilter {
    /// Parse one bounded repository-owned test filter.
    ///
    /// # Errors
    ///
    /// Returns an error for empty, oversized, or control-character-bearing filters.
    pub fn parse(value: &str) -> Result<Self, RustVerificationEnvelopeError> {
        validate_public_text("scope.filter", value, MAX_FILTER_BYTES)?;
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct RustNextestFilterset(String);

impl RustNextestFilterset {
    /// Parse one bounded repository-owned nextest filterset.
    ///
    /// # Errors
    ///
    /// Returns an error for empty, oversized, or control-character-bearing filtersets.
    pub fn parse(value: &str) -> Result<Self, RustVerificationEnvelopeError> {
        validate_public_text("test_backend.nextest_filterset", value, MAX_FILTER_BYTES)?;
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustSourceIdentity {
    pub repository: RepositoryRef,
    pub commit: CommitId,
    pub tree: GitTreeId,
}

impl RustSourceIdentity {
    #[must_use]
    pub const fn new(repository: RepositoryRef, commit: CommitId, tree: GitTreeId) -> Self {
        Self {
            repository,
            commit,
            tree,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RustPackageSelection {
    One { package: PackageId },
    Workspace,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "selection", rename_all = "snake_case")]
pub enum RustNamedTargetSelection {
    None,
    Named { targets: Vec<TargetName> },
    All,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustTargetSelection {
    pub library: bool,
    pub binaries: RustNamedTargetSelection,
    pub integration_tests: RustNamedTargetSelection,
    pub examples: RustNamedTargetSelection,
    pub benches: RustNamedTargetSelection,
    pub doctests: bool,
    pub build_scripts: bool,
}

impl RustTargetSelection {
    #[must_use]
    pub const fn library_only() -> Self {
        Self {
            library: true,
            binaries: RustNamedTargetSelection::None,
            integration_tests: RustNamedTargetSelection::None,
            examples: RustNamedTargetSelection::None,
            benches: RustNamedTargetSelection::None,
            doctests: false,
            build_scripts: true,
        }
    }

    fn validate(&self) -> Result<(), RustVerificationEnvelopeError> {
        validate_named_targets("scope.targets.binaries", &self.binaries)?;
        validate_named_targets("scope.targets.integration_tests", &self.integration_tests)?;
        validate_named_targets("scope.targets.examples", &self.examples)?;
        validate_named_targets("scope.targets.benches", &self.benches)?;
        if !self.library
            && matches!(&self.binaries, RustNamedTargetSelection::None)
            && matches!(&self.integration_tests, RustNamedTargetSelection::None)
            && matches!(&self.examples, RustNamedTargetSelection::None)
            && matches!(&self.benches, RustNamedTargetSelection::None)
            && !self.doctests
        {
            return Err(RustVerificationEnvelopeError::new(
                "scope.targets",
                "empty_target_selection",
                "at least one exact Rust target class must be selected",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RustVerificationScope {
    LibraryTests {
        package: PackageId,
        #[serde(skip_serializing_if = "Option::is_none")]
        filter: Option<RustTestFilter>,
    },
    IntegrationTest {
        package: PackageId,
        target: TargetName,
        #[serde(skip_serializing_if = "Option::is_none")]
        filter: Option<RustTestFilter>,
    },
    PackageTests {
        package: PackageId,
        targets: RustTargetSelection,
    },
    WorkspaceTests {
        targets: RustTargetSelection,
    },
    Check {
        packages: RustPackageSelection,
        targets: RustTargetSelection,
    },
    Clippy {
        packages: RustPackageSelection,
        targets: RustTargetSelection,
    },
    Build {
        packages: RustPackageSelection,
        targets: RustTargetSelection,
    },
}

impl RustVerificationScope {
    #[must_use]
    pub const fn is_test(&self) -> bool {
        matches!(
            self,
            Self::LibraryTests { .. }
                | Self::IntegrationTest { .. }
                | Self::PackageTests { .. }
                | Self::WorkspaceTests { .. }
        )
    }

    fn validate(&self) -> Result<(), RustVerificationEnvelopeError> {
        match self {
            Self::LibraryTests { .. } | Self::IntegrationTest { .. } => Ok(()),
            Self::PackageTests { targets, .. }
            | Self::WorkspaceTests { targets }
            | Self::Check { targets, .. }
            | Self::Clippy { targets, .. }
            | Self::Build { targets, .. } => targets.validate(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum RustFeatureSelection {
    Default,
    NoDefault { features: Vec<RustFeatureName> },
    DefaultPlus { features: Vec<RustFeatureName> },
    All,
}

impl RustFeatureSelection {
    fn validate(&self) -> Result<(), RustVerificationEnvelopeError> {
        match self {
            Self::Default | Self::All => Ok(()),
            Self::NoDefault { features } => validate_features(features, false),
            Self::DefaultPlus { features } => validate_features(features, true),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RustCargoProfile {
    Dev,
    Test,
    Release,
    Bench,
    Custom { name: RustCargoProfileName },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustTargetCacheIdentity {
    pub cache_id: CacheId,
    pub namespace_digest: Sha256Digest,
}

impl RustTargetCacheIdentity {
    #[must_use]
    pub const fn new(cache_id: CacheId, namespace_digest: Sha256Digest) -> Self {
        Self {
            cache_id,
            namespace_digest,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustCargoContract {
    target_triple: RustTargetTriple,
    profile: RustCargoProfile,
    features: RustFeatureSelection,
    incremental: bool,
    toolchain_digest: Sha256Digest,
    target_cache: RustTargetCacheIdentity,
    required_capabilities: Vec<CapabilityId>,
}

impl RustCargoContract {
    /// Define exact Cargo, toolchain, feature, and cache inputs for one reviewed Rust command.
    ///
    /// # Errors
    ///
    /// Returns an error for duplicate or excessive capabilities and invalid feature selections.
    pub fn new(
        target_triple: RustTargetTriple,
        profile: RustCargoProfile,
        features: RustFeatureSelection,
        incremental: bool,
        toolchain_digest: Sha256Digest,
        target_cache: RustTargetCacheIdentity,
        required_capabilities: Vec<CapabilityId>,
    ) -> Result<Self, RustVerificationEnvelopeError> {
        features.validate()?;
        validate_unique_capabilities(&required_capabilities)?;
        Ok(Self {
            target_triple,
            profile,
            features,
            incremental,
            toolchain_digest,
            target_cache,
            required_capabilities,
        })
    }

    #[must_use]
    pub const fn target_triple(&self) -> &RustTargetTriple {
        &self.target_triple
    }

    #[must_use]
    pub const fn profile(&self) -> &RustCargoProfile {
        &self.profile
    }

    #[must_use]
    pub const fn features(&self) -> &RustFeatureSelection {
        &self.features
    }

    #[must_use]
    pub const fn incremental(&self) -> bool {
        self.incremental
    }

    #[must_use]
    pub const fn toolchain_digest(&self) -> &Sha256Digest {
        &self.toolchain_digest
    }

    #[must_use]
    pub const fn target_cache(&self) -> &RustTargetCacheIdentity {
        &self.target_cache
    }

    #[must_use]
    pub fn required_capabilities(&self) -> &[CapabilityId] {
        &self.required_capabilities
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RustTestBackend {
    None,
    Libtest {
        test_threads: u16,
    },
    Nextest {
        test_threads: u16,
        #[serde(skip_serializing_if = "Option::is_none")]
        filterset: Option<RustNextestFilterset>,
    },
}

impl RustTestBackend {
    #[must_use]
    pub const fn test_threads(&self) -> Option<u16> {
        match self {
            Self::None => None,
            Self::Libtest { test_threads } | Self::Nextest { test_threads, .. } => {
                Some(*test_threads)
            }
        }
    }

    fn accepts_retry_threads(&self, retry_threads: Option<u16>) -> bool {
        matches!(
            (self, retry_threads),
            (Self::None, None) | (Self::Libtest { .. } | Self::Nextest { .. }, Some(_))
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustConcurrencyEnvelope {
    cargo_build_jobs: u16,
    test_backend: RustTestBackend,
    heavy_test_thread_reservations: u16,
}

impl RustConcurrencyEnvelope {
    /// Define enforceable Cargo and test-runtime concurrency separately.
    ///
    /// # Errors
    ///
    /// Returns an error for zero or excessive concurrency and invalid heavy-test reservations.
    pub fn new(
        cargo_build_jobs: u16,
        test_backend: RustTestBackend,
        heavy_test_thread_reservations: u16,
    ) -> Result<Self, RustVerificationEnvelopeError> {
        validate_concurrency_value("resources.cargo_build_jobs", cargo_build_jobs)?;
        if let Some(test_threads) = test_backend.test_threads() {
            validate_concurrency_value("resources.test_threads", test_threads)?;
            if heavy_test_thread_reservations > test_threads {
                return Err(RustVerificationEnvelopeError::new(
                    "resources.heavy_test_thread_reservations",
                    "invalid_heavy_test_reservation",
                    "heavy-test thread reservations may not exceed runtime test concurrency",
                ));
            }
        } else if heavy_test_thread_reservations != 0 {
            return Err(RustVerificationEnvelopeError::new(
                "resources.heavy_test_thread_reservations",
                "unexpected_heavy_test_reservation",
                "non-test commands may not reserve test-runtime threads",
            ));
        }
        Ok(Self {
            cargo_build_jobs,
            test_backend,
            heavy_test_thread_reservations,
        })
    }

    #[must_use]
    pub const fn cargo_build_jobs(&self) -> u16 {
        self.cargo_build_jobs
    }

    #[must_use]
    pub const fn test_backend(&self) -> &RustTestBackend {
        &self.test_backend
    }

    #[must_use]
    pub const fn heavy_test_thread_reservations(&self) -> u16 {
        self.heavy_test_thread_reservations
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustResourceEnvelope {
    required_worker_profile: PersonalWorkerProfile,
    reserved_cpu_millis: u32,
    reserved_memory_bytes: u64,
    minimum_available_memory_bytes: u64,
    minimum_available_swap_bytes: u64,
    estimated_peak_memory_bytes: u64,
    maximum_execution_seconds: u64,
    concurrency: RustConcurrencyEnvelope,
}

impl RustResourceEnvelope {
    /// Define one bounded worker reservation and memory-headroom policy.
    ///
    /// # Errors
    ///
    /// Returns an error for a stopped profile, overcommit, uncovered peak demand, or invalid timeout.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        required_worker_profile: PersonalWorkerProfile,
        reserved_cpu_millis: u32,
        reserved_memory_bytes: u64,
        minimum_available_memory_bytes: u64,
        minimum_available_swap_bytes: u64,
        estimated_peak_memory_bytes: u64,
        maximum_execution_seconds: u64,
        concurrency: RustConcurrencyEnvelope,
    ) -> Result<Self, RustVerificationEnvelopeError> {
        if required_worker_profile == PersonalWorkerProfile::Stopped {
            return Err(RustVerificationEnvelopeError::new(
                "resources.required_worker_profile",
                "stopped_worker_profile",
                "Rust verification requires an interactive or work worker profile",
            ));
        }
        let (maximum_cpu_millis, maximum_memory_bytes) =
            schedulable_resources(required_worker_profile);
        if reserved_cpu_millis == 0
            || reserved_cpu_millis > maximum_cpu_millis
            || reserved_memory_bytes == 0
            || reserved_memory_bytes > maximum_memory_bytes
        {
            return Err(RustVerificationEnvelopeError::new(
                "resources.reservation",
                "worker_resource_overcommit",
                "requested CPU and memory must fit the selected worker profile after its fixed host-service reserve",
            ));
        }
        let reserved_cpu_threads = reserved_cpu_millis / 1_000;
        let test_threads = concurrency.test_backend.test_threads().unwrap_or(0);
        if reserved_cpu_threads == 0
            || u32::from(concurrency.cargo_build_jobs) > reserved_cpu_threads
            || u32::from(test_threads) > reserved_cpu_threads
        {
            return Err(RustVerificationEnvelopeError::new(
                "resources.concurrency",
                "concurrency_exceeds_cpu_reservation",
                "Cargo build jobs and runtime test threads must each fit the whole vCPUs reserved for the verification",
            ));
        }
        let observed_headroom = minimum_available_memory_bytes
            .checked_add(minimum_available_swap_bytes)
            .ok_or_else(|| {
                RustVerificationEnvelopeError::new(
                    "resources.memory_headroom",
                    "memory_headroom_overflow",
                    "declared memory and swap headroom overflowed",
                )
            })?;
        if minimum_available_memory_bytes == 0
            || estimated_peak_memory_bytes == 0
            || estimated_peak_memory_bytes > reserved_memory_bytes
            || observed_headroom < estimated_peak_memory_bytes
        {
            return Err(RustVerificationEnvelopeError::new(
                "resources.memory_headroom",
                "uncovered_peak_memory",
                "reserved memory and required memory-plus-swap headroom must cover estimated peak demand",
            ));
        }
        if !(1..=MAX_RUST_EXECUTION_SECONDS).contains(&maximum_execution_seconds) {
            return Err(RustVerificationEnvelopeError::new(
                "resources.maximum_execution_seconds",
                "invalid_execution_timeout",
                "execution timeout must be positive and within the reviewed maximum",
            ));
        }
        Ok(Self {
            required_worker_profile,
            reserved_cpu_millis,
            reserved_memory_bytes,
            minimum_available_memory_bytes,
            minimum_available_swap_bytes,
            estimated_peak_memory_bytes,
            maximum_execution_seconds,
            concurrency,
        })
    }

    #[must_use]
    pub const fn required_worker_profile(&self) -> PersonalWorkerProfile {
        self.required_worker_profile
    }

    #[must_use]
    pub const fn reserved_cpu_millis(&self) -> u32 {
        self.reserved_cpu_millis
    }

    #[must_use]
    pub const fn reserved_memory_bytes(&self) -> u64 {
        self.reserved_memory_bytes
    }

    #[must_use]
    pub const fn minimum_available_memory_bytes(&self) -> u64 {
        self.minimum_available_memory_bytes
    }

    #[must_use]
    pub const fn minimum_available_swap_bytes(&self) -> u64 {
        self.minimum_available_swap_bytes
    }

    #[must_use]
    pub const fn estimated_peak_memory_bytes(&self) -> u64 {
        self.estimated_peak_memory_bytes
    }

    #[must_use]
    pub const fn maximum_execution_seconds(&self) -> u64 {
        self.maximum_execution_seconds
    }

    #[must_use]
    pub const fn concurrency(&self) -> &RustConcurrencyEnvelope {
        &self.concurrency
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustRetryConcurrency {
    cargo_build_jobs: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    test_threads: Option<u16>,
    heavy_test_thread_reservations: u16,
}

impl RustRetryConcurrency {
    /// Define the only fields an equivalent low-concurrency retry may change.
    ///
    /// # Errors
    ///
    /// Returns an error for zero, excessive, or inconsistent values.
    pub fn new(
        cargo_build_jobs: u16,
        test_threads: Option<u16>,
        heavy_test_thread_reservations: u16,
    ) -> Result<Self, RustVerificationEnvelopeError> {
        validate_concurrency_value("retry.cargo_build_jobs", cargo_build_jobs)?;
        if let Some(test_threads) = test_threads {
            validate_concurrency_value("retry.test_threads", test_threads)?;
            if heavy_test_thread_reservations > test_threads {
                return Err(RustVerificationEnvelopeError::new(
                    "retry.heavy_test_thread_reservations",
                    "invalid_retry_heavy_test_reservation",
                    "retry heavy-test reservations may not exceed retry test concurrency",
                ));
            }
        } else if heavy_test_thread_reservations != 0 {
            return Err(RustVerificationEnvelopeError::new(
                "retry.heavy_test_thread_reservations",
                "unexpected_retry_heavy_test_reservation",
                "a non-test retry may not reserve test-runtime threads",
            ));
        }
        Ok(Self {
            cargo_build_jobs,
            test_threads,
            heavy_test_thread_reservations,
        })
    }

    #[must_use]
    pub const fn cargo_build_jobs(&self) -> u16 {
        self.cargo_build_jobs
    }

    #[must_use]
    pub const fn test_threads(&self) -> Option<u16> {
        self.test_threads
    }

    #[must_use]
    pub const fn heavy_test_thread_reservations(&self) -> u16 {
        self.heavy_test_thread_reservations
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RustRetryPolicy {
    None,
    OneLowerConcurrency {
        policy_id: RustRetryPolicyId,
        concurrency: RustRetryConcurrency,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustVerificationEnvelope {
    schema_version: u8,
    profile_id: VerificationProfileId,
    source: RustSourceIdentity,
    command: RepositoryCommandIdentity,
    scope: RustVerificationScope,
    cargo: RustCargoContract,
    resources: RustResourceEnvelope,
    retry: RustRetryPolicy,
}

impl RustVerificationEnvelope {
    /// Bind one exact Rust scope, immutable source, command, toolchain/cache identity, resource
    /// envelope, and optional equivalent lower-concurrency retry.
    ///
    /// # Errors
    ///
    /// Returns bounded errors for repository drift, target ambiguity, test-backend drift,
    /// overcommit, or a retry that does anything other than lower declared concurrency.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        profile_id: VerificationProfileId,
        source: RustSourceIdentity,
        command: RepositoryCommandIdentity,
        scope: RustVerificationScope,
        cargo: RustCargoContract,
        resources: RustResourceEnvelope,
        retry: RustRetryPolicy,
    ) -> Result<Self, RustVerificationEnvelopeError> {
        if command.repository() != &source.repository {
            return Err(RustVerificationEnvelopeError::new(
                "command.repository",
                "source_command_repository_mismatch",
                "the repository command must belong to the exact immutable source repository",
            ));
        }
        scope.validate()?;
        validate_test_backend(&scope, &resources.concurrency.test_backend)?;
        validate_retry(&resources.concurrency, &retry)?;
        Ok(Self {
            schema_version: RUST_VERIFICATION_ENVELOPE_SCHEMA_VERSION,
            profile_id,
            source,
            command,
            scope,
            cargo,
            resources,
            retry,
        })
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn profile_id(&self) -> &VerificationProfileId {
        &self.profile_id
    }

    #[must_use]
    pub const fn source(&self) -> &RustSourceIdentity {
        &self.source
    }

    #[must_use]
    pub const fn command(&self) -> &RepositoryCommandIdentity {
        &self.command
    }

    #[must_use]
    pub const fn scope(&self) -> &RustVerificationScope {
        &self.scope
    }

    #[must_use]
    pub const fn cargo(&self) -> &RustCargoContract {
        &self.cargo
    }

    #[must_use]
    pub const fn resources(&self) -> &RustResourceEnvelope {
        &self.resources
    }

    #[must_use]
    pub const fn retry(&self) -> &RustRetryPolicy {
        &self.retry
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RustVerificationEnvelopeError {
    pub field: &'static str,
    pub code: &'static str,
    pub message: &'static str,
}

impl RustVerificationEnvelopeError {
    const fn new(field: &'static str, code: &'static str, message: &'static str) -> Self {
        Self {
            field,
            code,
            message,
        }
    }
}

impl fmt::Display for RustVerificationEnvelopeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for RustVerificationEnvelopeError {}

fn schedulable_resources(profile: PersonalWorkerProfile) -> (u32, u64) {
    match profile {
        PersonalWorkerProfile::Stopped => (0, 0),
        PersonalWorkerProfile::Interactive => (
            u32::from(INTERACTIVE_VCPUS)
                .saturating_mul(1_000)
                .saturating_sub(PERSONAL_WORKER_RESERVED_CPU_MILLIS),
            INTERACTIVE_MEMORY_BYTES.saturating_sub(PERSONAL_WORKER_RESERVED_MEMORY_BYTES),
        ),
        PersonalWorkerProfile::Work => (
            PERSONAL_WORKER_SCHEDULABLE_CPU_MILLIS,
            PERSONAL_WORKER_SCHEDULABLE_MEMORY_BYTES,
        ),
    }
}

fn validate_identifier(
    field: &'static str,
    value: &str,
) -> Result<(), RustVerificationEnvelopeError> {
    let valid = !value.is_empty()
        && value.len() <= MAX_IDENTIFIER_BYTES
        && value.is_ascii()
        && value.trim() == value
        && !value.starts_with('.')
        && !value.ends_with('.')
        && !value.contains("..")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+'));
    if !valid {
        return Err(RustVerificationEnvelopeError::new(
            field,
            "invalid_identifier",
            "must be one bounded canonical ASCII identifier without path or alias syntax",
        ));
    }
    Ok(())
}

fn validate_public_text(
    field: &'static str,
    value: &str,
    maximum_bytes: usize,
) -> Result<(), RustVerificationEnvelopeError> {
    if value.is_empty()
        || value.len() > maximum_bytes
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(RustVerificationEnvelopeError::new(
            field,
            "invalid_public_text",
            "must be non-empty bounded text without leading/trailing whitespace or controls",
        ));
    }
    Ok(())
}

fn validate_features(
    features: &[RustFeatureName],
    require_nonempty: bool,
) -> Result<(), RustVerificationEnvelopeError> {
    if features.len() > MAX_RUST_FEATURES || require_nonempty && features.is_empty() {
        return Err(RustVerificationEnvelopeError::new(
            "cargo.features",
            "invalid_feature_count",
            "feature selection exceeded the reviewed bound or omitted required explicit features",
        ));
    }
    let mut unique = BTreeSet::new();
    if features.iter().any(|feature| !unique.insert(feature)) {
        return Err(RustVerificationEnvelopeError::new(
            "cargo.features",
            "duplicate_feature",
            "feature selection must not contain duplicates",
        ));
    }
    Ok(())
}

fn validate_named_targets(
    field: &'static str,
    selection: &RustNamedTargetSelection,
) -> Result<(), RustVerificationEnvelopeError> {
    let RustNamedTargetSelection::Named { targets } = selection else {
        return Ok(());
    };
    if targets.is_empty() || targets.len() > MAX_RUST_NAMED_TARGETS {
        return Err(RustVerificationEnvelopeError::new(
            field,
            "invalid_named_target_count",
            "named target selections must be non-empty and within the reviewed bound",
        ));
    }
    let mut unique = BTreeSet::new();
    if targets.iter().any(|target| !unique.insert(target)) {
        return Err(RustVerificationEnvelopeError::new(
            field,
            "duplicate_named_target",
            "named target selections must not contain duplicates",
        ));
    }
    Ok(())
}

fn validate_unique_capabilities(
    capabilities: &[CapabilityId],
) -> Result<(), RustVerificationEnvelopeError> {
    if capabilities.len() > MAX_RUST_CAPABILITIES {
        return Err(RustVerificationEnvelopeError::new(
            "cargo.required_capabilities",
            "too_many_capabilities",
            "required capability count exceeded the reviewed bound",
        ));
    }
    let mut unique = BTreeSet::new();
    if capabilities
        .iter()
        .any(|capability| !unique.insert(capability))
    {
        return Err(RustVerificationEnvelopeError::new(
            "cargo.required_capabilities",
            "duplicate_capability",
            "required capabilities must not contain duplicates",
        ));
    }
    Ok(())
}

fn validate_concurrency_value(
    field: &'static str,
    value: u16,
) -> Result<(), RustVerificationEnvelopeError> {
    if !(1..=MAX_RUST_CONCURRENCY).contains(&value) {
        return Err(RustVerificationEnvelopeError::new(
            field,
            "invalid_concurrency",
            "concurrency must be positive and within the reviewed maximum",
        ));
    }
    Ok(())
}

fn validate_test_backend(
    scope: &RustVerificationScope,
    backend: &RustTestBackend,
) -> Result<(), RustVerificationEnvelopeError> {
    if scope.is_test() == matches!(backend, RustTestBackend::None) {
        return Err(RustVerificationEnvelopeError::new(
            "resources.test_backend",
            "test_backend_scope_mismatch",
            "test scopes require one explicit test backend and non-test scopes require none",
        ));
    }
    Ok(())
}

fn validate_retry(
    primary: &RustConcurrencyEnvelope,
    retry: &RustRetryPolicy,
) -> Result<(), RustVerificationEnvelopeError> {
    let RustRetryPolicy::OneLowerConcurrency { concurrency, .. } = retry else {
        return Ok(());
    };
    if !primary
        .test_backend
        .accepts_retry_threads(concurrency.test_threads)
    {
        return Err(RustVerificationEnvelopeError::new(
            "retry.test_threads",
            "retry_backend_mismatch",
            "the retry must preserve whether the command uses a test backend",
        ));
    }
    let primary_threads = primary.test_backend.test_threads();
    let threads_not_higher = match (primary_threads, concurrency.test_threads) {
        (None, None) => true,
        (Some(primary), Some(retry)) => retry <= primary,
        _ => false,
    };
    let strictly_lower = concurrency.cargo_build_jobs < primary.cargo_build_jobs
        || matches!(
            (primary_threads, concurrency.test_threads),
            (Some(primary), Some(retry)) if retry < primary
        )
        || concurrency.heavy_test_thread_reservations < primary.heavy_test_thread_reservations;
    if concurrency.cargo_build_jobs > primary.cargo_build_jobs
        || !threads_not_higher
        || concurrency.heavy_test_thread_reservations > primary.heavy_test_thread_reservations
        || !strictly_lower
    {
        return Err(RustVerificationEnvelopeError::new(
            "retry.concurrency",
            "non_lower_concurrency_retry",
            "the sole equivalent retry may only lower Cargo or test concurrency",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
