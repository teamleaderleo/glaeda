use std::fmt;

use serde::Serialize;

use crate::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use crate::execution_admission::ExecutionResourceLimits;
use crate::lima_lifecycle::{INTERACTIVE_MEMORY_BYTES, INTERACTIVE_VCPUS, LimaResourceProfile};
use crate::personal_worker_queue::{
    PERSONAL_WORKER_RESERVED_CPU_MILLIS, PERSONAL_WORKER_RESERVED_MEMORY_BYTES,
    PERSONAL_WORKER_SCHEDULABLE_CPU_MILLIS, PERSONAL_WORKER_SCHEDULABLE_MEMORY_BYTES,
};
use crate::verification_profile::{
    CacheId, CapabilityId, PackageId, RepositoryCommandIdentity, TargetName, TestFilter,
    VerificationProfileId,
};

pub const RUST_VERIFICATION_ENVELOPE_SCHEMA_VERSION: u8 = 1;
pub const MAX_RUST_FEATURES: usize = 128;
pub const MAX_RUST_TARGETS: usize = 128;
pub const MAX_RUST_CAPABILITIES: usize = 64;
pub const MAX_RUST_HEAVY_TEST_RESERVATIONS: usize = 32;
pub const MAX_RUST_CONCURRENCY: u16 = 256;
pub const MAX_RUST_EXECUTION_MILLIS: u64 = 86_400_000;
pub const MAX_RUST_SWAP_HEADROOM_BYTES: u64 = 1_u64 << 40;

macro_rules! identifier_type {
    ($name:ident, $field:literal, $max:expr) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Parse one bounded stable Rust-verification identifier.
            ///
            /// # Errors
            ///
            /// Returns an error for an empty, oversized, non-ASCII, or path-shaped value.
            pub fn parse(value: &str) -> Result<Self, RustVerificationEnvelopeError> {
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

identifier_type!(RustToolchainId, "toolchain.id", 96);
identifier_type!(RustTargetTriple, "toolchain.target_triple", 96);
identifier_type!(RustCargoProfileName, "compilation.profile", 64);
identifier_type!(RustTargetDirectoryId, "cache.target_directory_id", 96);
identifier_type!(RustRetryPolicyId, "retry.policy_id", 96);
identifier_type!(RustHeavyTestClassId, "resources.heavy_test.class_id", 96);
identifier_type!(RustNextestFiltersetId, "runtime.nextest.filterset_id", 96);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct RustFeatureName(String);

impl RustFeatureName {
    /// Parse one exact Cargo feature token.
    ///
    /// Cargo feature tokens may contain dependency-feature separators, but may not contain control
    /// characters, whitespace, leading option markers, or path-traversal-shaped segments.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsafe or oversized token.
    pub fn parse(value: &str) -> Result<Self, RustVerificationEnvelopeError> {
        let valid = !value.is_empty()
            && value.len() <= 128
            && value.is_ascii()
            && !value.starts_with('-')
            && !value.contains("..")
            && !value.contains("//")
            && value
                .bytes()
                .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'\\' | b'\'' | b'\"'));
        if !valid {
            return Err(RustVerificationEnvelopeError::new(
                "compilation.features",
                "invalid_feature_name",
                "feature tokens must be bounded safe ASCII Cargo feature names",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustVerificationSourceIdentity {
    pub repository: RepositoryRef,
    pub commit: CommitId,
    pub tree: GitTreeId,
}

impl RustVerificationSourceIdentity {
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
pub struct RustToolchainIdentity {
    pub toolchain_id: RustToolchainId,
    pub contract_digest: Sha256Digest,
    pub host_triple: RustTargetTriple,
    pub target_triple: RustTargetTriple,
}

impl RustToolchainIdentity {
    #[must_use]
    pub const fn new(
        toolchain_id: RustToolchainId,
        contract_digest: Sha256Digest,
        host_triple: RustTargetTriple,
        target_triple: RustTargetTriple,
    ) -> Self {
        Self {
            toolchain_id,
            contract_digest,
            host_triple,
            target_triple,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RustCargoProfileKind {
    Dev,
    Test,
    Release,
    Named { name: RustCargoProfileName },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum RustFeatureSelection {
    Default,
    NoDefault,
    Exact {
        default_features: bool,
        features: Vec<RustFeatureName>,
    },
    All,
}

impl RustFeatureSelection {
    /// Define an exact sorted feature set.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty, duplicate, or excessive explicit feature list.
    pub fn exact(
        default_features: bool,
        mut features: Vec<RustFeatureName>,
    ) -> Result<Self, RustVerificationEnvelopeError> {
        if features.is_empty() || features.len() > MAX_RUST_FEATURES {
            return Err(RustVerificationEnvelopeError::new(
                "compilation.features",
                "invalid_feature_count",
                "exact feature selection must contain a bounded non-empty feature list",
            ));
        }
        features.sort();
        if features.windows(2).any(|window| window[0] == window[1]) {
            return Err(RustVerificationEnvelopeError::new(
                "compilation.features",
                "duplicate_feature",
                "exact feature selection must not contain duplicates",
            ));
        }
        Ok(Self::Exact {
            default_features,
            features,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RustTargetKind {
    Library,
    Binary,
    IntegrationTest,
    Example,
    Bench,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct RustTargetSelector {
    pub package: PackageId,
    pub kind: RustTargetKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<TargetName>,
}

impl RustTargetSelector {
    /// Define one exact Cargo target selector.
    ///
    /// # Errors
    ///
    /// Returns an error unless library targets are unnamed and all other target kinds are named.
    pub fn new(
        package: PackageId,
        kind: RustTargetKind,
        name: Option<TargetName>,
    ) -> Result<Self, RustVerificationEnvelopeError> {
        let valid = matches!(kind, RustTargetKind::Library) == name.is_none();
        if !valid {
            return Err(RustVerificationEnvelopeError::new(
                "scope.targets",
                "invalid_target_identity",
                "library targets must be unnamed and every non-library target must have one exact name",
            ));
        }
        Ok(Self {
            package,
            kind,
            name,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum RustTargetMode {
    RepositoryDefault,
    Exact { targets: Vec<RustTargetSelector> },
    AllTargets,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustTargetPolicy {
    pub mode: RustTargetMode,
    pub include_examples: bool,
    pub include_benches: bool,
    pub include_doctests: bool,
}

impl RustTargetPolicy {
    #[must_use]
    pub const fn repository_default(
        include_examples: bool,
        include_benches: bool,
        include_doctests: bool,
    ) -> Self {
        Self {
            mode: RustTargetMode::RepositoryDefault,
            include_examples,
            include_benches,
            include_doctests,
        }
    }

    /// Define one exact sorted target set.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty, duplicate, or excessive target list.
    pub fn exact(
        mut targets: Vec<RustTargetSelector>,
        include_examples: bool,
        include_benches: bool,
        include_doctests: bool,
    ) -> Result<Self, RustVerificationEnvelopeError> {
        if targets.is_empty() || targets.len() > MAX_RUST_TARGETS {
            return Err(RustVerificationEnvelopeError::new(
                "scope.targets",
                "invalid_target_count",
                "exact target selection must contain a bounded non-empty target list",
            ));
        }
        targets.sort();
        if targets.windows(2).any(|window| window[0] == window[1]) {
            return Err(RustVerificationEnvelopeError::new(
                "scope.targets",
                "duplicate_target",
                "exact target selection must not contain duplicates",
            ));
        }
        Ok(Self {
            mode: RustTargetMode::Exact { targets },
            include_examples,
            include_benches,
            include_doctests,
        })
    }

    #[must_use]
    pub const fn all_targets(include_doctests: bool) -> Self {
        Self {
            mode: RustTargetMode::AllTargets,
            include_examples: true,
            include_benches: true,
            include_doctests,
        }
    }

    fn exact_targets(&self) -> Option<&[RustTargetSelector]> {
        match &self.mode {
            RustTargetMode::Exact { targets } => Some(targets),
            RustTargetMode::RepositoryDefault | RustTargetMode::AllTargets => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RustPackageSelection {
    Package { package: PackageId },
    Workspace { members_digest: Sha256Digest },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum RustVerificationScope {
    LibraryTests {
        package: PackageId,
    },
    IntegrationTest {
        package: PackageId,
        test: TargetName,
    },
    PackageTests {
        package: PackageId,
        targets: RustTargetPolicy,
    },
    WorkspaceTests {
        members_digest: Sha256Digest,
        targets: RustTargetPolicy,
    },
    Check {
        packages: RustPackageSelection,
        targets: RustTargetPolicy,
    },
    Clippy {
        packages: RustPackageSelection,
        targets: RustTargetPolicy,
    },
    Build {
        packages: RustPackageSelection,
        targets: RustTargetPolicy,
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

    fn validate_targets(&self) -> Result<(), RustVerificationEnvelopeError> {
        match self {
            Self::PackageTests { package, targets } => validate_package_targets(package, targets),
            Self::Check { packages, targets }
            | Self::Clippy { packages, targets }
            | Self::Build { packages, targets } => match packages {
                RustPackageSelection::Package { package } => {
                    validate_package_targets(package, targets)
                }
                RustPackageSelection::Workspace { .. } => Ok(()),
            },
            Self::LibraryTests { .. }
            | Self::IntegrationTest { .. }
            | Self::WorkspaceTests { .. } => Ok(()),
        }
    }
}

fn validate_package_targets(
    package: &PackageId,
    policy: &RustTargetPolicy,
) -> Result<(), RustVerificationEnvelopeError> {
    if policy
        .exact_targets()
        .is_some_and(|targets| targets.iter().any(|target| &target.package != package))
    {
        return Err(RustVerificationEnvelopeError::new(
            "scope.targets",
            "target_package_mismatch",
            "package-scoped verification may select targets only from the exact package",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RustBuildScriptInclusion {
    Excluded,
    Included,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustCompilationContract {
    pub toolchain: RustToolchainIdentity,
    pub cargo_profile: RustCargoProfileKind,
    pub features: RustFeatureSelection,
    pub build_scripts: RustBuildScriptInclusion,
}

impl RustCompilationContract {
    #[must_use]
    pub const fn new(
        toolchain: RustToolchainIdentity,
        cargo_profile: RustCargoProfileKind,
        features: RustFeatureSelection,
        build_scripts: RustBuildScriptInclusion,
    ) -> Self {
        Self {
            toolchain,
            cargo_profile,
            features,
            build_scripts,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustNextestFiltersetIdentity {
    pub filterset_id: RustNextestFiltersetId,
    pub contract_digest: Sha256Digest,
}

impl RustNextestFiltersetIdentity {
    #[must_use]
    pub const fn new(filterset_id: RustNextestFiltersetId, contract_digest: Sha256Digest) -> Self {
        Self {
            filterset_id,
            contract_digest,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "runner", rename_all = "snake_case")]
pub enum RustRuntimeConcurrency {
    NotApplicable,
    Libtest {
        test_threads: u16,
        #[serde(skip_serializing_if = "Option::is_none")]
        filter: Option<TestFilter>,
    },
    Nextest {
        test_threads: u16,
        #[serde(skip_serializing_if = "Option::is_none")]
        filterset: Option<RustNextestFiltersetIdentity>,
    },
}

impl RustRuntimeConcurrency {
    fn test_threads(&self) -> Option<u16> {
        match self {
            Self::NotApplicable => None,
            Self::Libtest { test_threads, .. } | Self::Nextest { test_threads, .. } => {
                Some(*test_threads)
            }
        }
    }

    fn same_authority(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::NotApplicable, Self::NotApplicable) => true,
            (Self::Libtest { filter: left, .. }, Self::Libtest { filter: right, .. }) => {
                left == right
            }
            (
                Self::Nextest {
                    filterset: left, ..
                },
                Self::Nextest {
                    filterset: right, ..
                },
            ) => left == right,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct RustHeavyTestThreadReservation {
    pub class_id: RustHeavyTestClassId,
    pub threads: u16,
}

impl RustHeavyTestThreadReservation {
    /// Define one bounded repository-declared heavy-test thread reservation.
    ///
    /// # Errors
    ///
    /// Returns an error for zero or excessive threads.
    pub fn new(
        class_id: RustHeavyTestClassId,
        threads: u16,
    ) -> Result<Self, RustVerificationEnvelopeError> {
        validate_concurrency_value("resources.heavy_test.threads", threads)?;
        Ok(Self { class_id, threads })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustConcurrencyPlan {
    pub cargo_build_jobs: u16,
    pub runtime: RustRuntimeConcurrency,
    pub heavy_test_thread_reservations: Vec<RustHeavyTestThreadReservation>,
}

impl RustConcurrencyPlan {
    /// Define explicit Cargo build and runtime test concurrency.
    ///
    /// # Errors
    ///
    /// Returns an error for zero/excessive concurrency or duplicate heavy-test classes.
    pub fn new(
        cargo_build_jobs: u16,
        runtime: RustRuntimeConcurrency,
        mut heavy_test_thread_reservations: Vec<RustHeavyTestThreadReservation>,
    ) -> Result<Self, RustVerificationEnvelopeError> {
        validate_concurrency_value("resources.cargo_build_jobs", cargo_build_jobs)?;
        if let Some(test_threads) = runtime.test_threads() {
            validate_concurrency_value("resources.runtime.test_threads", test_threads)?;
        }
        if heavy_test_thread_reservations.len() > MAX_RUST_HEAVY_TEST_RESERVATIONS {
            return Err(RustVerificationEnvelopeError::new(
                "resources.heavy_test",
                "heavy_test_reservation_count_exceeded",
                "heavy-test thread reservations must remain within the fixed count bound",
            ));
        }
        heavy_test_thread_reservations.sort();
        if heavy_test_thread_reservations
            .windows(2)
            .any(|window| window[0].class_id == window[1].class_id)
        {
            return Err(RustVerificationEnvelopeError::new(
                "resources.heavy_test",
                "duplicate_heavy_test_class",
                "heavy-test thread reservations must use unique class identities",
            ));
        }
        Ok(Self {
            cargo_build_jobs,
            runtime,
            heavy_test_thread_reservations,
        })
    }

    fn strictly_lowers(&self, canonical: &Self) -> bool {
        if self.cargo_build_jobs > canonical.cargo_build_jobs
            || !self.runtime.same_authority(&canonical.runtime)
        {
            return false;
        }
        let runtime_lower_or_equal = match (
            self.runtime.test_threads(),
            canonical.runtime.test_threads(),
        ) {
            (None, None) => true,
            (Some(left), Some(right)) => left <= right,
            _ => false,
        };
        if !runtime_lower_or_equal
            || self.heavy_test_thread_reservations.len()
                != canonical.heavy_test_thread_reservations.len()
        {
            return false;
        }
        let mut strictly_lower = self.cargo_build_jobs < canonical.cargo_build_jobs
            || matches!(
                (self.runtime.test_threads(), canonical.runtime.test_threads()),
                (Some(left), Some(right)) if left < right
            );
        for (lower, upper) in self
            .heavy_test_thread_reservations
            .iter()
            .zip(&canonical.heavy_test_thread_reservations)
        {
            if lower.class_id != upper.class_id || lower.threads > upper.threads {
                return false;
            }
            strictly_lower |= lower.threads < upper.threads;
        }
        strictly_lower
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustResourceEnvelope {
    pub required_worker_profile: LimaResourceProfile,
    pub reserved_resources: ExecutionResourceLimits,
    pub concurrency: RustConcurrencyPlan,
    pub minimum_guest_available_memory_bytes: u64,
    pub minimum_guest_available_swap_bytes: u64,
    pub maximum_execution_millis: u64,
}

impl RustResourceEnvelope {
    /// Bind one Rust verification attempt to fixed worker, reservation, concurrency, headroom, and
    /// duration limits.
    ///
    /// # Errors
    ///
    /// Returns an error when the declaration exceeds the reviewed worker profile, oversubscribes
    /// CPU concurrency, lacks enough memory/swap headroom for the reserved memory, or uses an
    /// unbounded duration.
    pub fn new(
        required_worker_profile: LimaResourceProfile,
        reserved_resources: ExecutionResourceLimits,
        concurrency: RustConcurrencyPlan,
        minimum_guest_available_memory_bytes: u64,
        minimum_guest_available_swap_bytes: u64,
        maximum_execution_millis: u64,
    ) -> Result<Self, RustVerificationEnvelopeError> {
        let worker = required_worker_profile.envelope();
        let (schedulable_cpu_millis, schedulable_memory_bytes) = match required_worker_profile {
            LimaResourceProfile::Interactive => (
                u32::from(INTERACTIVE_VCPUS) * 1_000 - PERSONAL_WORKER_RESERVED_CPU_MILLIS,
                INTERACTIVE_MEMORY_BYTES - PERSONAL_WORKER_RESERVED_MEMORY_BYTES,
            ),
            LimaResourceProfile::Work => (
                PERSONAL_WORKER_SCHEDULABLE_CPU_MILLIS,
                PERSONAL_WORKER_SCHEDULABLE_MEMORY_BYTES,
            ),
        };
        if reserved_resources.cpu_millis > schedulable_cpu_millis
            || reserved_resources.memory_bytes > schedulable_memory_bytes
        {
            return Err(RustVerificationEnvelopeError::new(
                "resources.reserved",
                "reservation_exceeds_worker_profile",
                "reserved CPU and memory must fit within the exact schedulable worker capacity",
            ));
        }
        let reserved_whole_vcpus = reserved_resources.cpu_millis / 1_000;
        if reserved_whole_vcpus == 0
            || u32::from(concurrency.cargo_build_jobs) > reserved_whole_vcpus
            || concurrency
                .runtime
                .test_threads()
                .is_some_and(|threads| u32::from(threads) > reserved_whole_vcpus)
        {
            return Err(RustVerificationEnvelopeError::new(
                "resources.concurrency",
                "concurrency_exceeds_reserved_cpu",
                "Cargo build jobs and runtime test threads must fit the job's reserved whole vCPUs",
            ));
        }
        let heavy_threads = concurrency
            .heavy_test_thread_reservations
            .iter()
            .try_fold(0_u16, |total, reservation| {
                total.checked_add(reservation.threads)
            })
            .ok_or_else(|| {
                RustVerificationEnvelopeError::new(
                    "resources.heavy_test",
                    "heavy_test_thread_overflow",
                    "heavy-test thread reservations must remain within the bounded total",
                )
            })?;
        if u32::from(heavy_threads) > reserved_whole_vcpus {
            return Err(RustVerificationEnvelopeError::new(
                "resources.heavy_test",
                "heavy_test_threads_exceed_reserved_cpu",
                "heavy-test thread reservations must fit the job's reserved whole vCPUs",
            ));
        }
        let available =
            minimum_guest_available_memory_bytes.checked_add(minimum_guest_available_swap_bytes);
        if minimum_guest_available_memory_bytes == 0
            || minimum_guest_available_memory_bytes > worker.memory_bytes
            || minimum_guest_available_swap_bytes > MAX_RUST_SWAP_HEADROOM_BYTES
            || available.is_none()
            || available.is_some_and(|bytes| bytes < reserved_resources.memory_bytes)
        {
            return Err(RustVerificationEnvelopeError::new(
                "resources.headroom",
                "invalid_memory_headroom",
                "guest memory and swap headroom must be bounded and cover the reserved memory",
            ));
        }
        if !(1..=MAX_RUST_EXECUTION_MILLIS).contains(&maximum_execution_millis) {
            return Err(RustVerificationEnvelopeError::new(
                "resources.maximum_execution_millis",
                "invalid_execution_duration",
                "maximum execution duration must remain within the fixed positive bound",
            ));
        }
        Ok(Self {
            required_worker_profile,
            reserved_resources,
            concurrency,
            minimum_guest_available_memory_bytes,
            minimum_guest_available_swap_bytes,
            maximum_execution_millis,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RustCacheIdentityClass {
    RepositoryScoped,
    SourceScoped,
    AttemptScoped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CargoTargetDirectoryIdentity {
    pub directory_id: RustTargetDirectoryId,
    pub cache_id: CacheId,
    pub namespace_digest: Sha256Digest,
}

impl CargoTargetDirectoryIdentity {
    #[must_use]
    pub const fn new(
        directory_id: RustTargetDirectoryId,
        cache_id: CacheId,
        namespace_digest: Sha256Digest,
    ) -> Self {
        Self {
            directory_id,
            cache_id,
            namespace_digest,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustCacheContract {
    pub identity_class: RustCacheIdentityClass,
    pub cargo_target_directory: CargoTargetDirectoryIdentity,
}

impl RustCacheContract {
    #[must_use]
    pub const fn new(
        identity_class: RustCacheIdentityClass,
        cargo_target_directory: CargoTargetDirectoryIdentity,
    ) -> Self {
        Self {
            identity_class,
            cargo_target_directory,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum RustRetryPolicy {
    NoRetry {
        policy_id: RustRetryPolicyId,
    },
    OneLowerConcurrency {
        policy_id: RustRetryPolicyId,
        fallback: RustConcurrencyPlan,
    },
}

impl RustRetryPolicy {
    #[must_use]
    pub const fn no_retry(policy_id: RustRetryPolicyId) -> Self {
        Self::NoRetry { policy_id }
    }

    /// Define the only automatic retry shape: one authority-preserving lower-concurrency variant.
    ///
    /// # Errors
    ///
    /// Returns an error unless build jobs, runtime threads, or declared heavy-test threads strictly
    /// decrease without changing runner/filter authority.
    pub fn one_lower_concurrency(
        policy_id: RustRetryPolicyId,
        canonical: &RustConcurrencyPlan,
        fallback: RustConcurrencyPlan,
    ) -> Result<Self, RustVerificationEnvelopeError> {
        if !fallback.strictly_lowers(canonical) {
            return Err(RustVerificationEnvelopeError::new(
                "retry.fallback",
                "non_equivalent_retry_concurrency",
                "retry may only preserve runtime/filter authority and strictly lower declared concurrency",
            ));
        }
        Ok(Self::OneLowerConcurrency {
            policy_id,
            fallback,
        })
    }

    fn validate_against(
        &self,
        canonical: &RustConcurrencyPlan,
    ) -> Result<(), RustVerificationEnvelopeError> {
        if let Self::OneLowerConcurrency { fallback, .. } = self
            && !fallback.strictly_lowers(canonical)
        {
            return Err(RustVerificationEnvelopeError::new(
                "retry.fallback",
                "non_equivalent_retry_concurrency",
                "retry may only preserve runtime/filter authority and strictly lower declared concurrency",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RustVerificationEnvelopeDefinition {
    pub profile_id: VerificationProfileId,
    pub source: RustVerificationSourceIdentity,
    pub command: RepositoryCommandIdentity,
    pub scope: RustVerificationScope,
    pub compilation: RustCompilationContract,
    pub resources: RustResourceEnvelope,
    pub cache: RustCacheContract,
    pub required_capabilities: Vec<CapabilityId>,
    pub retry: RustRetryPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustVerificationEnvelope {
    schema_version: u8,
    profile_id: VerificationProfileId,
    source: RustVerificationSourceIdentity,
    command: RepositoryCommandIdentity,
    scope: RustVerificationScope,
    compilation: RustCompilationContract,
    resources: RustResourceEnvelope,
    cache: RustCacheContract,
    required_capabilities: Vec<CapabilityId>,
    retry: RustRetryPolicy,
}

impl RustVerificationEnvelope {
    /// Validate one complete repository-declared Rust verification envelope.
    ///
    /// This contract contains no command line, shell, environment dump, path, OOM classification,
    /// cgroup authority, or retry state machine. It binds only immutable authority and reviewed
    /// resource/concurrency declarations for later adapters.
    ///
    /// # Errors
    ///
    /// Returns an error for repository drift, target widening, runtime/scope mismatch, duplicate
    /// capabilities, or a retry that changes authority or fails to lower concurrency.
    pub fn new(
        definition: RustVerificationEnvelopeDefinition,
    ) -> Result<Self, RustVerificationEnvelopeError> {
        let RustVerificationEnvelopeDefinition {
            profile_id,
            source,
            command,
            scope,
            compilation,
            resources,
            cache,
            mut required_capabilities,
            retry,
        } = definition;

        if command.repository() != &source.repository {
            return Err(RustVerificationEnvelopeError::new(
                "envelope.repository",
                "repository_identity_mismatch",
                "source and repository command must name the same exact repository",
            ));
        }
        scope.validate_targets()?;
        let runtime_is_test = !matches!(
            &resources.concurrency.runtime,
            RustRuntimeConcurrency::NotApplicable
        );
        if scope.is_test() != runtime_is_test {
            return Err(RustVerificationEnvelopeError::new(
                "resources.runtime",
                "runtime_scope_mismatch",
                "test scopes require explicit test concurrency and non-test scopes forbid it",
            ));
        }
        if required_capabilities.is_empty() || required_capabilities.len() > MAX_RUST_CAPABILITIES {
            return Err(RustVerificationEnvelopeError::new(
                "envelope.required_capabilities",
                "invalid_capability_count",
                "Rust verification must bind a bounded non-empty capability set",
            ));
        }
        required_capabilities.sort();
        if required_capabilities
            .windows(2)
            .any(|window| window[0] == window[1])
        {
            return Err(RustVerificationEnvelopeError::new(
                "envelope.required_capabilities",
                "duplicate_capability",
                "required capability identities must be unique",
            ));
        }
        retry.validate_against(&resources.concurrency)?;

        Ok(Self {
            schema_version: RUST_VERIFICATION_ENVELOPE_SCHEMA_VERSION,
            profile_id,
            source,
            command,
            scope,
            compilation,
            resources,
            cache,
            required_capabilities,
            retry,
        })
    }

    #[must_use]
    pub const fn profile_id(&self) -> &VerificationProfileId {
        &self.profile_id
    }

    #[must_use]
    pub const fn scope(&self) -> &RustVerificationScope {
        &self.scope
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustVerificationEnvelopeError {
    pub field: String,
    pub code: String,
    pub problem: String,
}

impl RustVerificationEnvelopeError {
    fn new(field: impl Into<String>, code: impl Into<String>, problem: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            code: code.into(),
            problem: problem.into(),
        }
    }
}

impl fmt::Display for RustVerificationEnvelopeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.problem)
    }
}

impl std::error::Error for RustVerificationEnvelopeError {}

fn validate_identifier(
    field: &'static str,
    value: &str,
    maximum_length: usize,
) -> Result<(), RustVerificationEnvelopeError> {
    let valid = !value.is_empty()
        && value.len() <= maximum_length
        && value.is_ascii()
        && !value.starts_with('-')
        && !value.ends_with('.')
        && !value.contains("..")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if !valid {
        return Err(RustVerificationEnvelopeError::new(
            field,
            "invalid_identifier",
            "must be bounded safe ASCII letters, digits, '.', '_', or '-'",
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
            "concurrency must remain within the fixed positive bound",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(character: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", character.to_string().repeat(64)))
            .expect("digest")
    }

    fn repository() -> RepositoryRef {
        RepositoryRef::parse("openai/codex").expect("repository")
    }

    fn source() -> RustVerificationSourceIdentity {
        RustVerificationSourceIdentity::new(
            repository(),
            CommitId::parse(&"a".repeat(40)).expect("commit"),
            GitTreeId::parse(&"b".repeat(40)).expect("tree"),
        )
    }

    fn command() -> RepositoryCommandIdentity {
        RepositoryCommandIdentity::new(
            repository(),
            crate::verification_profile::RepositoryCommandId::parse("codex-core-lib")
                .expect("command ID"),
            digest('c'),
        )
    }

    fn toolchain() -> RustToolchainIdentity {
        RustToolchainIdentity::new(
            RustToolchainId::parse("rust-1.88.0").expect("toolchain ID"),
            digest('d'),
            RustTargetTriple::parse("aarch64-apple-darwin").expect("host triple"),
            RustTargetTriple::parse("aarch64-unknown-linux-gnu").expect("target triple"),
        )
    }

    fn compilation() -> RustCompilationContract {
        RustCompilationContract::new(
            toolchain(),
            RustCargoProfileKind::Test,
            RustFeatureSelection::NoDefault,
            RustBuildScriptInclusion::Included,
        )
    }

    fn target_directory() -> CargoTargetDirectoryIdentity {
        CargoTargetDirectoryIdentity::new(
            RustTargetDirectoryId::parse("codex-linux-target").expect("target directory ID"),
            CacheId::parse("codex-target-cache").expect("cache ID"),
            digest('e'),
        )
    }

    fn cache() -> RustCacheContract {
        RustCacheContract::new(RustCacheIdentityClass::RepositoryScoped, target_directory())
    }

    fn capability(value: &str) -> CapabilityId {
        CapabilityId::parse(value).expect("capability")
    }

    fn limits() -> ExecutionResourceLimits {
        ExecutionResourceLimits::new(4_000, 6 * 1024 * 1024 * 1024, 2_048).expect("resource limits")
    }

    fn test_concurrency(build_jobs: u16, test_threads: u16) -> RustConcurrencyPlan {
        RustConcurrencyPlan::new(
            build_jobs,
            RustRuntimeConcurrency::Libtest {
                test_threads,
                filter: Some(TestFilter::parse("state::tests::").expect("filter")),
            },
            vec![
                RustHeavyTestThreadReservation::new(
                    RustHeavyTestClassId::parse("link-heavy").expect("heavy test class"),
                    1,
                )
                .expect("heavy test reservation"),
            ],
        )
        .expect("concurrency")
    }

    fn resources(concurrency: RustConcurrencyPlan) -> RustResourceEnvelope {
        RustResourceEnvelope::new(
            LimaResourceProfile::Work,
            limits(),
            concurrency,
            6 * 1024 * 1024 * 1024,
            1024 * 1024 * 1024,
            30 * 60 * 1_000,
        )
        .expect("resources")
    }

    fn definition(
        scope: RustVerificationScope,
        resources: RustResourceEnvelope,
        retry: RustRetryPolicy,
    ) -> RustVerificationEnvelopeDefinition {
        RustVerificationEnvelopeDefinition {
            profile_id: VerificationProfileId::parse("codex-core-focused").expect("profile ID"),
            source: source(),
            command: command(),
            scope,
            compilation: compilation(),
            resources,
            cache: cache(),
            required_capabilities: vec![capability("cargo"), capability("rustc-linux-target")],
            retry,
        }
    }

    fn library_scope() -> RustVerificationScope {
        RustVerificationScope::LibraryTests {
            package: PackageId::parse("codex-core").expect("package"),
        }
    }

    #[test]
    fn focused_library_scope_cannot_widen_to_integration_targets() {
        let concurrency = test_concurrency(2, 1);
        let envelope = RustVerificationEnvelope::new(definition(
            library_scope(),
            resources(concurrency),
            RustRetryPolicy::no_retry(RustRetryPolicyId::parse("no-retry").expect("retry policy")),
        ))
        .expect("envelope");

        let json = serde_json::to_string(&envelope).expect("JSON");
        assert!(json.contains("\"class\":\"library_tests\""));
        assert!(json.contains("\"cargo_build_jobs\":2"));
        assert!(json.contains("\"test_threads\":1"));
        assert!(!json.contains("integration_test"));
        assert!(!json.contains("all_targets"));
        assert!(!json.contains("linker_jobs"));
    }

    #[test]
    fn exact_integration_target_is_distinct_from_package_and_workspace_tests() {
        let scope = RustVerificationScope::IntegrationTest {
            package: PackageId::parse("codex-core").expect("package"),
            test: TargetName::parse("code_mode_orphan_sessions").expect("test target"),
        };
        let concurrency = test_concurrency(2, 1);
        let envelope = RustVerificationEnvelope::new(definition(
            scope,
            resources(concurrency),
            RustRetryPolicy::no_retry(RustRetryPolicyId::parse("no-retry").expect("retry policy")),
        ))
        .expect("envelope");

        let json = serde_json::to_string(&envelope).expect("JSON");
        assert!(json.contains("\"class\":\"integration_test\""));
        assert!(json.contains("code_mode_orphan_sessions"));
        assert!(!json.contains("package_tests"));
        assert!(!json.contains("workspace_tests"));
    }

    #[test]
    fn package_scope_rejects_targets_from_another_package() {
        let targets = RustTargetPolicy::exact(
            vec![
                RustTargetSelector::new(
                    PackageId::parse("codex-cli").expect("foreign package"),
                    RustTargetKind::IntegrationTest,
                    Some(TargetName::parse("cli_smoke").expect("target")),
                )
                .expect("selector"),
            ],
            false,
            false,
            false,
        )
        .expect("target policy");
        let scope = RustVerificationScope::PackageTests {
            package: PackageId::parse("codex-core").expect("package"),
            targets,
        };
        let concurrency = test_concurrency(2, 1);
        let error = RustVerificationEnvelope::new(definition(
            scope,
            resources(concurrency),
            RustRetryPolicy::no_retry(RustRetryPolicyId::parse("no-retry").expect("retry policy")),
        ))
        .expect_err("foreign target");

        assert_eq!(error.code, "target_package_mismatch");
    }

    #[test]
    fn non_test_scope_forbids_runtime_test_concurrency() {
        let scope = RustVerificationScope::Check {
            packages: RustPackageSelection::Package {
                package: PackageId::parse("codex-core").expect("package"),
            },
            targets: RustTargetPolicy::repository_default(false, false, false),
        };
        let concurrency = test_concurrency(2, 1);
        let error = RustVerificationEnvelope::new(definition(
            scope,
            resources(concurrency),
            RustRetryPolicy::no_retry(RustRetryPolicyId::parse("no-retry").expect("retry policy")),
        ))
        .expect_err("runtime mismatch");

        assert_eq!(error.code, "runtime_scope_mismatch");
    }

    #[test]
    fn build_and_runtime_concurrency_are_separate_without_linker_authority() {
        let concurrency = RustConcurrencyPlan::new(
            2,
            RustRuntimeConcurrency::Nextest {
                test_threads: 1,
                filterset: Some(RustNextestFiltersetIdentity::new(
                    RustNextestFiltersetId::parse("codex-core-focused").expect("filterset ID"),
                    digest('f'),
                )),
            },
            Vec::new(),
        )
        .expect("concurrency");
        let envelope = RustVerificationEnvelope::new(definition(
            library_scope(),
            resources(concurrency),
            RustRetryPolicy::no_retry(RustRetryPolicyId::parse("no-retry").expect("retry policy")),
        ))
        .expect("envelope");

        let value = serde_json::to_value(envelope).expect("JSON value");
        assert_eq!(value["resources"]["concurrency"]["cargo_build_jobs"], 2);
        assert_eq!(
            value["resources"]["concurrency"]["runtime"]["test_threads"],
            1
        );
        assert!(value.to_string().find("linker_jobs").is_none());
    }

    #[test]
    fn retry_may_only_lower_concurrency_without_changing_filter_authority() {
        let canonical = test_concurrency(4, 2);
        let fallback = test_concurrency(2, 1);
        let retry = RustRetryPolicy::one_lower_concurrency(
            RustRetryPolicyId::parse("one-low-memory-retry").expect("retry policy"),
            &canonical,
            fallback,
        )
        .expect("lower retry");
        RustVerificationEnvelope::new(definition(
            library_scope(),
            resources(canonical.clone()),
            retry,
        ))
        .expect("envelope");

        let changed_filter = RustConcurrencyPlan::new(
            2,
            RustRuntimeConcurrency::Libtest {
                test_threads: 1,
                filter: Some(TestFilter::parse("different").expect("filter")),
            },
            vec![
                RustHeavyTestThreadReservation::new(
                    RustHeavyTestClassId::parse("link-heavy").expect("class"),
                    1,
                )
                .expect("reservation"),
            ],
        )
        .expect("changed filter concurrency");
        let error = RustRetryPolicy::one_lower_concurrency(
            RustRetryPolicyId::parse("invalid-retry").expect("retry policy"),
            &canonical,
            changed_filter,
        )
        .expect_err("authority drift");
        assert_eq!(error.code, "non_equivalent_retry_concurrency");
    }

    #[test]
    fn retry_rejects_equal_or_higher_concurrency() {
        let canonical = test_concurrency(2, 1);
        let error = RustRetryPolicy::one_lower_concurrency(
            RustRetryPolicyId::parse("equal-retry").expect("retry policy"),
            &canonical,
            canonical.clone(),
        )
        .expect_err("equal concurrency");
        assert_eq!(error.code, "non_equivalent_retry_concurrency");

        let higher = test_concurrency(4, 1);
        let error = RustRetryPolicy::one_lower_concurrency(
            RustRetryPolicyId::parse("higher-retry").expect("retry policy"),
            &canonical,
            higher,
        )
        .expect_err("higher concurrency");
        assert_eq!(error.code, "non_equivalent_retry_concurrency");
    }

    #[test]
    fn resource_envelope_refuses_profile_overcommit_and_missing_headroom() {
        let concurrency = test_concurrency(2, 1);
        let overcommitted =
            ExecutionResourceLimits::new(4_000, 4 * 1024 * 1024 * 1024, 1_024).expect("limits");
        let error = RustResourceEnvelope::new(
            LimaResourceProfile::Interactive,
            overcommitted,
            concurrency.clone(),
            3 * 1024 * 1024 * 1024,
            1024 * 1024 * 1024,
            60_000,
        )
        .expect_err("profile overcommit");
        assert_eq!(error.code, "reservation_exceeds_worker_profile");

        let fits_vm_but_exceeds_schedulable =
            ExecutionResourceLimits::new(3_001, 1024 * 1024 * 1024, 1_024).expect("limits");
        let error = RustResourceEnvelope::new(
            LimaResourceProfile::Interactive,
            fits_vm_but_exceeds_schedulable,
            concurrency.clone(),
            3 * 1024 * 1024 * 1024,
            1024 * 1024 * 1024,
            60_000,
        )
        .expect_err("host-service reserve overcommit");
        assert_eq!(error.code, "reservation_exceeds_worker_profile");

        let two_cpu_reservation =
            ExecutionResourceLimits::new(2_000, 2 * 1024 * 1024 * 1024, 1_024).expect("limits");
        let error = RustResourceEnvelope::new(
            LimaResourceProfile::Work,
            two_cpu_reservation,
            test_concurrency(3, 1),
            2 * 1024 * 1024 * 1024,
            0,
            60_000,
        )
        .expect_err("concurrency exceeds reserved CPU");
        assert_eq!(error.code, "concurrency_exceeds_reserved_cpu");

        let error = RustResourceEnvelope::new(
            LimaResourceProfile::Work,
            limits(),
            concurrency,
            2 * 1024 * 1024 * 1024,
            0,
            60_000,
        )
        .expect_err("headroom shortfall");
        assert_eq!(error.code, "invalid_memory_headroom");
    }

    #[test]
    fn explicit_all_targets_and_all_features_are_never_implicit() {
        let scope = RustVerificationScope::WorkspaceTests {
            members_digest: digest('9'),
            targets: RustTargetPolicy::all_targets(true),
        };
        let mut definition = definition(
            scope,
            resources(test_concurrency(2, 1)),
            RustRetryPolicy::no_retry(RustRetryPolicyId::parse("no-retry").expect("retry policy")),
        );
        definition.compilation.features = RustFeatureSelection::All;
        let envelope = RustVerificationEnvelope::new(definition).expect("envelope");
        let json = serde_json::to_string(&envelope).expect("JSON");

        assert!(json.contains("\"mode\":\"all_targets\""));
        assert!(json.contains("\"mode\":\"all\""));
        assert!(json.contains("\"include_examples\":true"));
        assert!(json.contains("\"include_benches\":true"));
        assert!(json.contains("\"include_doctests\":true"));
    }

    #[test]
    fn repository_and_capability_identity_drift_fail_closed() {
        let concurrency = test_concurrency(2, 1);
        let mut mismatch = definition(
            library_scope(),
            resources(concurrency.clone()),
            RustRetryPolicy::no_retry(RustRetryPolicyId::parse("no-retry").expect("retry policy")),
        );
        mismatch.command = RepositoryCommandIdentity::new(
            RepositoryRef::parse("teamleaderleo/smolrunner").expect("other repository"),
            crate::verification_profile::RepositoryCommandId::parse("other-command")
                .expect("command ID"),
            digest('8'),
        );
        let error = RustVerificationEnvelope::new(mismatch).expect_err("repository mismatch");
        assert_eq!(error.code, "repository_identity_mismatch");

        let mut duplicate = definition(
            library_scope(),
            resources(concurrency),
            RustRetryPolicy::no_retry(RustRetryPolicyId::parse("no-retry").expect("retry policy")),
        );
        duplicate.required_capabilities = vec![capability("cargo"), capability("cargo")];
        let error = RustVerificationEnvelope::new(duplicate).expect_err("duplicate capability");
        assert_eq!(error.code, "duplicate_capability");
    }

    #[test]
    fn public_contract_contains_no_command_line_environment_or_private_path() {
        let envelope = RustVerificationEnvelope::new(definition(
            library_scope(),
            resources(test_concurrency(2, 1)),
            RustRetryPolicy::no_retry(RustRetryPolicyId::parse("no-retry").expect("retry policy")),
        ))
        .expect("envelope");
        let json = serde_json::to_string(&envelope).expect("JSON");

        assert!(!json.contains("argv"));
        assert!(!json.contains("environment"));
        assert!(!json.contains("/Users/"));
        assert!(!json.contains("/home/"));
        assert_eq!(envelope.profile_id().as_str(), "codex-core-focused");
    }
}
