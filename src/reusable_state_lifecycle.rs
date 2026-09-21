//! Evidence-driven lifecycle policy for expensive reusable execution state.
//!
//! The module is deliberately path-free and side-effect-free. Family owners keep physical storage,
//! semantic validation, execution truth, publication mechanics, locking, and deletion authority.
//! This layer answers whether an exact generation has earned reuse and retention.

use std::{cmp::Ordering, fmt};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::artifact::{RepositoryRef, Sha256Digest};

pub const REUSABLE_STATE_LIFECYCLE_SCHEMA_VERSION: u8 = 1;
pub const MAX_REUSABLE_STATE_BYTES: u64 = 1 << 50;
pub const MAX_REUSABLE_STATE_GENERATIONS_PER_PLAN: usize = 256;
pub const MAX_REUSABLE_STATE_EVICTIONS_PER_PASS: usize = 64;
const MAX_METRIC: u64 = 1_000_000_000_000;
const MAX_DURATION_MILLIS: u64 = 365 * 24 * 60 * 60 * 1_000;
const HOUR_MILLIS: u64 = 60 * 60 * 1_000;
const DAY_MILLIS: u64 = 24 * HOUR_MILLIS;
const WEEK_MILLIS: u64 = 7 * DAY_MILLIS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReusableStateClass {
    PackageManagerState,
    CompilerCache,
    IncrementalBuildState,
    ImmutableCompiledProduct,
    ContainerLayer,
    PreparedDependencyGeneration,
    ProjectLocalApprovedHotState,
}

impl ReusableStateClass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PackageManagerState => "package_manager_state",
            Self::CompilerCache => "compiler_cache",
            Self::IncrementalBuildState => "incremental_build_state",
            Self::ImmutableCompiledProduct => "immutable_compiled_product",
            Self::ContainerLayer => "container_layer",
            Self::PreparedDependencyGeneration => "prepared_dependency_generation",
            Self::ProjectLocalApprovedHotState => "project_local_approved_hot_state",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ReusableStateArchitecture(String);

impl ReusableStateArchitecture {
    /// # Errors
    ///
    /// Returns an error unless `value` is a bounded canonical architecture token.
    pub fn parse(value: &str) -> Result<Self, ReusableStatePolicyError> {
        let mut bytes = value.bytes();
        let Some(first) = bytes.next() else {
            return Err(ReusableStatePolicyError::InvalidArchitecture);
        };
        if value.len() > 48
            || !first.is_ascii_lowercase()
            || !bytes.all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'_' | b'-' | b'.')
            })
        {
            return Err(ReusableStatePolicyError::InvalidArchitecture);
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Versioned validity inputs. Mutable aliases such as `main`, `latest`, and `current` are absent.
/// Family-specific validity inputs belong in `family_inputs_digest`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReusableStateIdentityContract {
    pub schema_version: u8,
    pub state_class: ReusableStateClass,
    pub repository: RepositoryRef,
    pub architecture: ReusableStateArchitecture,
    pub toolchain_generation: Sha256Digest,
    pub os_runtime_generation: Sha256Digest,
    pub dependency_lock_digest: Option<Sha256Digest>,
    pub build_configuration_digest: Option<Sha256Digest>,
    pub compiler_flags_digest: Option<Sha256Digest>,
    pub prepared_environment_generation: Option<Sha256Digest>,
    pub cache_schema_generation: Sha256Digest,
    pub family_inputs_digest: Sha256Digest,
}

impl ReusableStateIdentityContract {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub const fn new(
        state_class: ReusableStateClass,
        repository: RepositoryRef,
        architecture: ReusableStateArchitecture,
        toolchain_generation: Sha256Digest,
        os_runtime_generation: Sha256Digest,
        dependency_lock_digest: Option<Sha256Digest>,
        build_configuration_digest: Option<Sha256Digest>,
        compiler_flags_digest: Option<Sha256Digest>,
        prepared_environment_generation: Option<Sha256Digest>,
        cache_schema_generation: Sha256Digest,
        family_inputs_digest: Sha256Digest,
    ) -> Self {
        Self {
            schema_version: REUSABLE_STATE_LIFECYCLE_SCHEMA_VERSION,
            state_class,
            repository,
            architecture,
            toolchain_generation,
            os_runtime_generation,
            dependency_lock_digest,
            build_configuration_digest,
            compiler_flags_digest,
            prepared_environment_generation,
            cache_schema_generation,
            family_inputs_digest,
        }
    }

    /// Return one path-free digest over every validity dimension.
    ///
    /// # Errors
    ///
    /// Returns an error only if the internally generated SHA-256 cannot be represented canonically.
    pub fn digest(&self) -> Result<Sha256Digest, ReusableStatePolicyError> {
        let mut hasher = Sha256::new();
        hash_field(&mut hasher, b"schema", &[self.schema_version]);
        hash_field(&mut hasher, b"class", self.state_class.as_str().as_bytes());
        hash_field(
            &mut hasher,
            b"repository",
            self.repository.as_str().as_bytes(),
        );
        hash_field(
            &mut hasher,
            b"architecture",
            self.architecture.as_str().as_bytes(),
        );
        hash_digest(&mut hasher, b"toolchain", &self.toolchain_generation);
        hash_digest(&mut hasher, b"runtime", &self.os_runtime_generation);
        hash_optional(&mut hasher, b"lock", self.dependency_lock_digest.as_ref());
        hash_optional(
            &mut hasher,
            b"build_configuration",
            self.build_configuration_digest.as_ref(),
        );
        hash_optional(
            &mut hasher,
            b"compiler_flags",
            self.compiler_flags_digest.as_ref(),
        );
        hash_optional(
            &mut hasher,
            b"prepared_environment",
            self.prepared_environment_generation.as_ref(),
        );
        hash_digest(&mut hasher, b"cache_schema", &self.cache_schema_generation);
        hash_digest(&mut hasher, b"family_inputs", &self.family_inputs_digest);
        let digest = hasher.finalize();
        digest_bytes(&digest)
    }

    #[must_use]
    pub fn first_mismatch(&self, observed: &Self) -> Option<ReusableStateIdentityMismatch> {
        macro_rules! mismatch {
            ($field:ident, $kind:ident) => {
                if self.$field != observed.$field {
                    return Some(ReusableStateIdentityMismatch::$kind);
                }
            };
        }
        mismatch!(schema_version, ContractSchema);
        mismatch!(state_class, StateClass);
        mismatch!(repository, Repository);
        mismatch!(architecture, Architecture);
        mismatch!(toolchain_generation, ToolchainGeneration);
        mismatch!(os_runtime_generation, OsRuntimeGeneration);
        mismatch!(dependency_lock_digest, DependencyLockDigest);
        mismatch!(build_configuration_digest, BuildConfigurationDigest);
        mismatch!(compiler_flags_digest, CompilerFlagsDigest);
        mismatch!(
            prepared_environment_generation,
            PreparedEnvironmentGeneration
        );
        mismatch!(cache_schema_generation, CacheSchemaGeneration);
        mismatch!(family_inputs_digest, FamilyInputsDigest);
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReusableStateIdentityMismatch {
    ContractSchema,
    StateClass,
    Repository,
    Architecture,
    ToolchainGeneration,
    OsRuntimeGeneration,
    DependencyLockDigest,
    BuildConfigurationDigest,
    CompilerFlagsDigest,
    PreparedEnvironmentGeneration,
    CacheSchemaGeneration,
    FamilyInputsDigest,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ReusableStateGenerationId(pub Sha256Digest);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReusableStatePublisherAuthority {
    ConsumerOnly,
    ReviewedTrustedPublisher,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReusableStateLifecycle {
    Candidate,
    Validated,
    ObservedConsumers,
    Preferred,
    Demoted,
    Retired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReusableStatePublicationState {
    Complete,
    Partial,
    DiskFull,
    ProducerCrashed,
    ConcurrentPublisherConflict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReusableStateIntegrityState {
    Verified,
    Truncated,
    Corrupt,
}

/// Bounded utility evidence for one exact generation. Durations are cumulative over this record's
/// observation window. Shared telemetry intentionally has no source, credential, path, or log field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReusableStateMetrics {
    pub lookups: u64,
    pub hits: u64,
    pub misses: u64,
    pub restore_duration_millis: u64,
    pub publication_duration_millis: u64,
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub storage_size_bytes: u64,
    pub estimated_cold_work_millis: u64,
    pub estimated_warm_work_millis: u64,
    pub last_useful_hit_epoch_millis: Option<u64>,
    pub validation_failures: u64,
    pub reset_invalidation_count: u64,
    pub reset_invalidation_overhead_millis: u64,
    pub producer_successes: u64,
    pub successful_consumers: u64,
    pub semantic_mismatches: u64,
}

impl ReusableStateMetrics {
    /// # Errors
    ///
    /// Returns an error when counts disagree or any observation exceeds its reviewed bound.
    pub fn validate(&self) -> Result<(), ReusableStatePolicyError> {
        for count in [
            self.lookups,
            self.hits,
            self.misses,
            self.validation_failures,
            self.reset_invalidation_count,
            self.producer_successes,
            self.successful_consumers,
            self.semantic_mismatches,
        ] {
            if count > MAX_METRIC {
                return Err(ReusableStatePolicyError::MetricOutOfRange);
            }
        }
        if self.hits.checked_add(self.misses) != Some(self.lookups)
            || self.successful_consumers > self.hits
        {
            return Err(ReusableStatePolicyError::InconsistentMetrics);
        }
        for duration in [
            self.restore_duration_millis,
            self.publication_duration_millis,
            self.estimated_cold_work_millis,
            self.estimated_warm_work_millis,
            self.reset_invalidation_overhead_millis,
        ] {
            if duration > MAX_DURATION_MILLIS {
                return Err(ReusableStatePolicyError::MetricOutOfRange);
            }
        }
        if [self.bytes_read, self.bytes_written, self.storage_size_bytes]
            .into_iter()
            .any(|bytes| bytes > MAX_REUSABLE_STATE_BYTES)
        {
            return Err(ReusableStatePolicyError::MetricOutOfRange);
        }
        Ok(())
    }

    /// `net = hits * max(cold - warm, 0) - restore - publication - reset/invalidation`.
    /// Storage remains separate.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid metrics or arithmetic overflow.
    pub fn utility(&self) -> Result<ReusableStateUtility, ReusableStatePolicyError> {
        self.validate()?;
        let saved_per_hit = self
            .estimated_cold_work_millis
            .saturating_sub(self.estimated_warm_work_millis);
        let avoided = self
            .hits
            .checked_mul(saved_per_hit)
            .ok_or(ReusableStatePolicyError::UtilityOverflow)?;
        let net = i64::try_from(avoided)
            .map_err(|_| ReusableStatePolicyError::UtilityOverflow)?
            .checked_sub(
                i64::try_from(self.restore_duration_millis)
                    .map_err(|_| ReusableStatePolicyError::UtilityOverflow)?,
            )
            .and_then(|value| {
                value.checked_sub(i64::try_from(self.publication_duration_millis).ok()?)
            })
            .and_then(|value| {
                value.checked_sub(i64::try_from(self.reset_invalidation_overhead_millis).ok()?)
            })
            .ok_or(ReusableStatePolicyError::UtilityOverflow)?;
        Ok(ReusableStateUtility {
            avoided_work_millis: avoided,
            restore_overhead_millis: self.restore_duration_millis,
            publication_overhead_millis: self.publication_duration_millis,
            invalidation_reset_overhead_millis: self.reset_invalidation_overhead_millis,
            net_time_saved_millis: net,
            storage_size_bytes: self.storage_size_bytes,
            estimated_saved_per_hit_millis: saved_per_hit,
        })
    }

    #[must_use]
    pub fn hit_rate_basis_points(&self) -> u16 {
        if self.lookups == 0 {
            0
        } else {
            u16::try_from(self.hits.saturating_mul(10_000) / self.lookups).unwrap_or(10_000)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ReusableStateUtility {
    pub avoided_work_millis: u64,
    pub restore_overhead_millis: u64,
    pub publication_overhead_millis: u64,
    pub invalidation_reset_overhead_millis: u64,
    pub net_time_saved_millis: i64,
    pub storage_size_bytes: u64,
    pub estimated_saved_per_hit_millis: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ReusableStateRetentionFacts {
    pub reconstructible: bool,
    pub unique_local_work: bool,
    pub must_retain: bool,
    pub in_use_consumers: u32,
}

impl ReusableStateRetentionFacts {
    /// # Errors
    ///
    /// Returns an error when state is simultaneously marked reconstructible and unique local work.
    pub fn validate(self) -> Result<(), ReusableStatePolicyError> {
        if self.reconstructible && self.unique_local_work {
            Err(ReusableStatePolicyError::ContradictoryRetention)
        } else {
            Ok(())
        }
    }

    #[must_use]
    pub const fn safe_to_evict(self) -> bool {
        self.reconstructible
            && !self.unique_local_work
            && !self.must_retain
            && self.in_use_consumers == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReusableStateGeneration {
    identity: ReusableStateIdentityContract,
    generation: ReusableStateGenerationId,
    lifecycle: ReusableStateLifecycle,
    publication: ReusableStatePublicationState,
    integrity: ReusableStateIntegrityState,
    revalidation_required: bool,
    metrics: ReusableStateMetrics,
    retention: ReusableStateRetentionFacts,
}

impl ReusableStateGeneration {
    /// # Errors
    ///
    /// Returns an error unless a reviewed trusted publisher creates the candidate and its bounded
    /// evidence is internally consistent.
    #[allow(clippy::too_many_arguments)]
    pub fn candidate(
        publisher: ReusableStatePublisherAuthority,
        identity: ReusableStateIdentityContract,
        generation: ReusableStateGenerationId,
        publication: ReusableStatePublicationState,
        integrity: ReusableStateIntegrityState,
        revalidation_required: bool,
        metrics: ReusableStateMetrics,
        retention: ReusableStateRetentionFacts,
    ) -> Result<Self, ReusableStatePolicyError> {
        if publisher != ReusableStatePublisherAuthority::ReviewedTrustedPublisher {
            return Err(ReusableStatePolicyError::PublisherRequired);
        }
        if identity.schema_version != REUSABLE_STATE_LIFECYCLE_SCHEMA_VERSION {
            return Err(ReusableStatePolicyError::UnsupportedIdentitySchema);
        }
        metrics.validate()?;
        retention.validate()?;
        Ok(Self {
            identity,
            generation,
            lifecycle: ReusableStateLifecycle::Candidate,
            publication,
            integrity,
            revalidation_required,
            metrics,
            retention,
        })
    }

    #[must_use]
    pub const fn identity(&self) -> &ReusableStateIdentityContract {
        &self.identity
    }

    #[must_use]
    pub const fn generation_id(&self) -> &ReusableStateGenerationId {
        &self.generation
    }

    #[must_use]
    pub const fn lifecycle(&self) -> ReusableStateLifecycle {
        self.lifecycle
    }

    #[must_use]
    pub const fn publication(&self) -> ReusableStatePublicationState {
        self.publication
    }

    #[must_use]
    pub const fn integrity(&self) -> ReusableStateIntegrityState {
        self.integrity
    }

    #[must_use]
    pub const fn revalidation_required(&self) -> bool {
        self.revalidation_required
    }

    #[must_use]
    pub const fn metrics(&self) -> &ReusableStateMetrics {
        &self.metrics
    }

    #[must_use]
    pub const fn retention(&self) -> ReusableStateRetentionFacts {
        self.retention
    }

    /// Replace bounded observation evidence without changing generation identity or lifecycle.
    ///
    /// # Errors
    ///
    /// Returns an error for inconsistent metrics or contradictory retention facts.
    pub fn replace_observation(
        &mut self,
        publication: ReusableStatePublicationState,
        integrity: ReusableStateIntegrityState,
        revalidation_required: bool,
        metrics: ReusableStateMetrics,
        retention: ReusableStateRetentionFacts,
    ) -> Result<(), ReusableStatePolicyError> {
        metrics.validate()?;
        retention.validate()?;
        self.publication = publication;
        self.integrity = integrity;
        self.revalidation_required = revalidation_required;
        self.metrics = metrics;
        self.retention = retention;
        Ok(())
    }

    /// Advance exactly one lifecycle edge using the generation's current evidence.
    ///
    /// # Errors
    ///
    /// Returns an error for an out-of-order edge or insufficient promotion evidence.
    pub fn transition(
        mut self,
        target: ReusableStateLifecycle,
        policy: ReusableStatePromotionPolicy,
    ) -> Result<Self, ReusableStatePolicyError> {
        let allowed = matches!(
            (self.lifecycle, target),
            (
                ReusableStateLifecycle::Candidate,
                ReusableStateLifecycle::Validated
            ) | (
                ReusableStateLifecycle::Validated,
                ReusableStateLifecycle::ObservedConsumers
            ) | (
                ReusableStateLifecycle::ObservedConsumers,
                ReusableStateLifecycle::Preferred
            ) | (
                ReusableStateLifecycle::Preferred,
                ReusableStateLifecycle::Demoted
            ) | (
                ReusableStateLifecycle::Demoted,
                ReusableStateLifecycle::Retired
            )
        );
        if !allowed {
            return Err(ReusableStatePolicyError::InvalidTransition);
        }
        match target {
            ReusableStateLifecycle::Validated => {
                if self.publication != ReusableStatePublicationState::Complete
                    || self.integrity != ReusableStateIntegrityState::Verified
                    || self.revalidation_required
                    || self.metrics.producer_successes == 0
                {
                    return Err(ReusableStatePolicyError::PromotionEvidenceMissing);
                }
            }
            ReusableStateLifecycle::ObservedConsumers => {
                if self.metrics.successful_consumers == 0 || self.metrics.semantic_mismatches > 0 {
                    return Err(ReusableStatePolicyError::PromotionEvidenceMissing);
                }
            }
            ReusableStateLifecycle::Preferred => {
                if !policy.accepts_preferred(&self)? {
                    return Err(ReusableStatePolicyError::PromotionEvidenceMissing);
                }
            }
            ReusableStateLifecycle::Retired if self.retention.in_use_consumers > 0 => {
                return Err(ReusableStatePolicyError::GenerationInUse);
            }
            ReusableStateLifecycle::Demoted | ReusableStateLifecycle::Retired => {}
            ReusableStateLifecycle::Candidate => {
                return Err(ReusableStatePolicyError::InvalidTransition);
            }
        }
        self.lifecycle = target;
        Ok(self)
    }

    /// Return the bounded next policy recommendation for this exact generation.
    ///
    /// # Errors
    ///
    /// Returns an error only for invalid utility evidence.
    pub fn recommendation(
        &self,
        policy: ReusableStatePromotionPolicy,
    ) -> Result<ReusableStateRecommendation, ReusableStatePolicyError> {
        if self.publication != ReusableStatePublicationState::Complete
            || self.integrity != ReusableStateIntegrityState::Verified
            || self.revalidation_required
            || self.metrics.semantic_mismatches > 0
        {
            return Ok(ReusableStateRecommendation::Revalidate);
        }
        let utility = self.metrics.utility()?;
        let enough_value_lookups = self.metrics.lookups >= policy.min_lookups_for_value;
        Ok(match self.lifecycle {
            ReusableStateLifecycle::Candidate | ReusableStateLifecycle::Validated
                if enough_value_lookups && utility.net_time_saved_millis <= 0 =>
            {
                ReusableStateRecommendation::StopPublishing
            }
            ReusableStateLifecycle::Candidate => ReusableStateRecommendation::Observe,
            ReusableStateLifecycle::Validated => {
                if self.metrics.successful_consumers > 0 {
                    ReusableStateRecommendation::Promote
                } else {
                    ReusableStateRecommendation::Observe
                }
            }
            ReusableStateLifecycle::ObservedConsumers => {
                if enough_value_lookups && utility.net_time_saved_millis <= 0 {
                    ReusableStateRecommendation::StopPublishing
                } else if policy.accepts_preferred(self)? {
                    ReusableStateRecommendation::Promote
                } else {
                    ReusableStateRecommendation::Observe
                }
            }
            ReusableStateLifecycle::Preferred => {
                if policy.accepts_preferred(self)? {
                    ReusableStateRecommendation::Retain
                } else {
                    ReusableStateRecommendation::Demote
                }
            }
            ReusableStateLifecycle::Demoted if self.retention.safe_to_evict() => {
                ReusableStateRecommendation::Evict
            }
            ReusableStateLifecycle::Demoted => ReusableStateRecommendation::Retire,
            ReusableStateLifecycle::Retired if self.retention.safe_to_evict() => {
                ReusableStateRecommendation::Evict
            }
            ReusableStateLifecycle::Retired => ReusableStateRecommendation::Retain,
        })
    }

    /// Build a path/content-free advisory status record for #970/#546 consumers.
    ///
    /// # Errors
    ///
    /// Returns an error only for invalid utility evidence.
    pub fn status(
        &self,
        now_epoch_millis: u64,
    ) -> Result<ReusableStateStatusSummary, ReusableStatePolicyError> {
        let utility = self.metrics.utility()?;
        let recent_hit =
            classify_recent_hit(self.metrics.last_useful_hit_epoch_millis, now_epoch_millis);
        let heat = if self.revalidation_required
            || matches!(
                self.lifecycle,
                ReusableStateLifecycle::Candidate
                    | ReusableStateLifecycle::Demoted
                    | ReusableStateLifecycle::Retired
            )
            || self.metrics.hits == 0
        {
            ReusableStateHeatClass::Cold
        } else if self.lifecycle == ReusableStateLifecycle::Preferred
            && utility.net_time_saved_millis > 0
            && matches!(
                recent_hit,
                ReusableStateRecentHitClass::WithinHour | ReusableStateRecentHitClass::WithinDay
            )
        {
            ReusableStateHeatClass::Hot
        } else {
            ReusableStateHeatClass::Warm
        };
        Ok(ReusableStateStatusSummary {
            schema_version: REUSABLE_STATE_LIFECYCLE_SCHEMA_VERSION,
            cache_class: self.identity.state_class,
            generation: self.generation.clone(),
            heat,
            size: classify_size(self.metrics.storage_size_bytes),
            recent_hit,
            revalidation_required: self.revalidation_required,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ReusableStatePromotionPolicy {
    pub min_producer_successes: u64,
    pub min_successful_consumers: u64,
    pub min_lookups_for_value: u64,
    pub min_net_time_saved_millis: i64,
    pub max_validation_failures: u64,
    pub max_resets_per_thousand_lookups: u64,
}

impl ReusableStatePromotionPolicy {
    #[must_use]
    pub const fn conservative() -> Self {
        Self {
            min_producer_successes: 2,
            min_successful_consumers: 2,
            min_lookups_for_value: 3,
            min_net_time_saved_millis: 1,
            max_validation_failures: 0,
            max_resets_per_thousand_lookups: 10,
        }
    }

    fn accepts_preferred(
        self,
        generation: &ReusableStateGeneration,
    ) -> Result<bool, ReusableStatePolicyError> {
        if self.min_producer_successes < 2 || self.min_net_time_saved_millis <= 0 {
            return Err(ReusableStatePolicyError::InvalidPromotionPolicy);
        }
        let metrics = &generation.metrics;
        let reset_rate = if metrics.lookups == 0 {
            u64::MAX
        } else {
            metrics.reset_invalidation_count.saturating_mul(1_000) / metrics.lookups
        };
        Ok(
            generation.publication == ReusableStatePublicationState::Complete
                && generation.integrity == ReusableStateIntegrityState::Verified
                && !generation.revalidation_required
                && metrics.producer_successes >= self.min_producer_successes
                && metrics.successful_consumers >= self.min_successful_consumers
                && metrics.lookups >= self.min_lookups_for_value
                && metrics.semantic_mismatches == 0
                && metrics.validation_failures <= self.max_validation_failures
                && reset_rate <= self.max_resets_per_thousand_lookups
                && metrics.utility()?.net_time_saved_millis >= self.min_net_time_saved_millis,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReusableStateRecommendation {
    Observe,
    Promote,
    Retain,
    Revalidate,
    Demote,
    Retire,
    Evict,
    StopPublishing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReusableStateConsumerTrust {
    LowTrust,
    Trusted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ReusableStateConsumptionPolicy {
    pub low_trust_read_only_allowed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReusableStateConsumptionMode {
    ReadOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "disposition", rename_all = "snake_case")]
pub enum ReusableStateConsumptionDisposition {
    Hit { mode: ReusableStateConsumptionMode },
    MissReset { reason: ReusableStateMissReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReusableStateMissReason {
    IdentityMismatch(ReusableStateIdentityMismatch),
    PartialPublish,
    DiskFullPublication,
    ProducerCrash,
    ConcurrentPublisherConflict,
    TruncatedGeneration,
    CorruptGeneration,
    RevalidationRequired,
    InvalidatedPreferredGeneration,
    LifecycleUnavailable,
    LowTrustConsumptionDisallowed,
}

/// Evaluate one read-only consumption attempt. Broken or ambiguous state becomes a miss/reset.
#[must_use]
pub fn evaluate_reusable_state_consumption(
    expected: &ReusableStateIdentityContract,
    generation: &ReusableStateGeneration,
    consumer: ReusableStateConsumerTrust,
    policy: ReusableStateConsumptionPolicy,
) -> ReusableStateConsumptionDisposition {
    if let Some(mismatch) = expected.first_mismatch(&generation.identity) {
        return ReusableStateConsumptionDisposition::MissReset {
            reason: ReusableStateMissReason::IdentityMismatch(mismatch),
        };
    }
    let publication_failure = match generation.publication {
        ReusableStatePublicationState::Complete => None,
        ReusableStatePublicationState::Partial => Some(ReusableStateMissReason::PartialPublish),
        ReusableStatePublicationState::DiskFull => {
            Some(ReusableStateMissReason::DiskFullPublication)
        }
        ReusableStatePublicationState::ProducerCrashed => {
            Some(ReusableStateMissReason::ProducerCrash)
        }
        ReusableStatePublicationState::ConcurrentPublisherConflict => {
            Some(ReusableStateMissReason::ConcurrentPublisherConflict)
        }
    };
    if let Some(reason) = publication_failure {
        return ReusableStateConsumptionDisposition::MissReset { reason };
    }
    match generation.integrity {
        ReusableStateIntegrityState::Truncated => {
            return ReusableStateConsumptionDisposition::MissReset {
                reason: ReusableStateMissReason::TruncatedGeneration,
            };
        }
        ReusableStateIntegrityState::Corrupt => {
            return ReusableStateConsumptionDisposition::MissReset {
                reason: ReusableStateMissReason::CorruptGeneration,
            };
        }
        ReusableStateIntegrityState::Verified => {}
    }
    if generation.revalidation_required {
        return ReusableStateConsumptionDisposition::MissReset {
            reason: if generation.lifecycle == ReusableStateLifecycle::Preferred {
                ReusableStateMissReason::InvalidatedPreferredGeneration
            } else {
                ReusableStateMissReason::RevalidationRequired
            },
        };
    }
    let usable = match consumer {
        ReusableStateConsumerTrust::LowTrust => {
            policy.low_trust_read_only_allowed
                && generation.lifecycle == ReusableStateLifecycle::Preferred
        }
        ReusableStateConsumerTrust::Trusted => matches!(
            generation.lifecycle,
            ReusableStateLifecycle::Validated
                | ReusableStateLifecycle::ObservedConsumers
                | ReusableStateLifecycle::Preferred
        ),
    };
    if usable {
        ReusableStateConsumptionDisposition::Hit {
            mode: ReusableStateConsumptionMode::ReadOnly,
        }
    } else {
        ReusableStateConsumptionDisposition::MissReset {
            reason: if consumer == ReusableStateConsumerTrust::LowTrust
                && !policy.low_trust_read_only_allowed
            {
                ReusableStateMissReason::LowTrustConsumptionDisallowed
            } else {
                ReusableStateMissReason::LifecycleUnavailable
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ReusableStateDiskBudget {
    pub low_watermark_bytes: u64,
    pub high_watermark_bytes: u64,
    pub max_evictions_per_pass: usize,
}

impl ReusableStateDiskBudget {
    /// # Errors
    ///
    /// Returns an error for reversed/unbounded watermarks or an unbounded pass size.
    pub fn validate(self) -> Result<(), ReusableStatePolicyError> {
        if self.low_watermark_bytes >= self.high_watermark_bytes
            || self.high_watermark_bytes > MAX_REUSABLE_STATE_BYTES
            || self.max_evictions_per_pass == 0
            || self.max_evictions_per_pass > MAX_REUSABLE_STATE_EVICTIONS_PER_PASS
        {
            Err(ReusableStatePolicyError::InvalidDiskBudget)
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReusableStateEvictionSelection {
    pub cache_class: ReusableStateClass,
    pub generation: ReusableStateGenerationId,
    pub storage_size_bytes: u64,
    pub net_time_saved_millis: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReusableStateEvictionPlan {
    pub schema_version: u8,
    pub pressure_triggered: bool,
    pub before_bytes: u64,
    pub target_bytes: u64,
    pub projected_bytes: u64,
    pub selected: Vec<ReusableStateEvictionSelection>,
    pub budget_satisfied: bool,
}

/// Plan bounded whole-generation reclamation without mutating physical state.
///
/// # Errors
///
/// Returns an error for invalid budgets, excessive input, invalid metrics, or arithmetic overflow.
pub fn plan_reusable_state_eviction(
    budget: ReusableStateDiskBudget,
    generations: &[ReusableStateGeneration],
) -> Result<ReusableStateEvictionPlan, ReusableStatePolicyError> {
    budget.validate()?;
    if generations.len() > MAX_REUSABLE_STATE_GENERATIONS_PER_PLAN {
        return Err(ReusableStatePolicyError::TooManyGenerations);
    }
    let before_bytes = generations.iter().try_fold(0_u64, |sum, generation| {
        generation.metrics.validate()?;
        sum.checked_add(generation.metrics.storage_size_bytes)
            .ok_or(ReusableStatePolicyError::MetricOutOfRange)
    })?;
    if before_bytes <= budget.high_watermark_bytes {
        return Ok(ReusableStateEvictionPlan {
            schema_version: REUSABLE_STATE_LIFECYCLE_SCHEMA_VERSION,
            pressure_triggered: false,
            before_bytes,
            target_bytes: budget.low_watermark_bytes,
            projected_bytes: before_bytes,
            selected: Vec::new(),
            budget_satisfied: true,
        });
    }
    let mut candidates = generations
        .iter()
        .filter(|generation| generation.retention.safe_to_evict())
        .map(|generation| {
            Ok((
                generation,
                generation.metrics.utility()?.net_time_saved_millis,
                generation.metrics.last_useful_hit_epoch_millis.unwrap_or(0),
            ))
        })
        .collect::<Result<Vec<_>, ReusableStatePolicyError>>()?;
    candidates.sort_by(|left, right| eviction_order(*left, *right));

    let mut projected_bytes = before_bytes;
    let mut selected = Vec::new();
    for (generation, net, _) in candidates {
        if projected_bytes <= budget.low_watermark_bytes
            || selected.len() == budget.max_evictions_per_pass
        {
            break;
        }
        projected_bytes = projected_bytes.saturating_sub(generation.metrics.storage_size_bytes);
        selected.push(ReusableStateEvictionSelection {
            cache_class: generation.identity.state_class,
            generation: generation.generation.clone(),
            storage_size_bytes: generation.metrics.storage_size_bytes,
            net_time_saved_millis: net,
        });
    }
    Ok(ReusableStateEvictionPlan {
        schema_version: REUSABLE_STATE_LIFECYCLE_SCHEMA_VERSION,
        pressure_triggered: true,
        before_bytes,
        target_bytes: budget.low_watermark_bytes,
        projected_bytes,
        budget_satisfied: projected_bytes <= budget.low_watermark_bytes,
        selected,
    })
}

fn eviction_order(
    left: (&ReusableStateGeneration, i64, u64),
    right: (&ReusableStateGeneration, i64, u64),
) -> Ordering {
    lifecycle_eviction_rank(left.0.lifecycle)
        .cmp(&lifecycle_eviction_rank(right.0.lifecycle))
        .then_with(|| left.1.cmp(&right.1))
        .then_with(|| left.2.cmp(&right.2))
        .then_with(|| {
            right
                .0
                .metrics
                .storage_size_bytes
                .cmp(&left.0.metrics.storage_size_bytes)
        })
        .then_with(|| left.0.generation.cmp(&right.0.generation))
}

const fn lifecycle_eviction_rank(lifecycle: ReusableStateLifecycle) -> u8 {
    match lifecycle {
        ReusableStateLifecycle::Retired => 0,
        ReusableStateLifecycle::Demoted => 1,
        ReusableStateLifecycle::Candidate => 2,
        ReusableStateLifecycle::Validated => 3,
        ReusableStateLifecycle::ObservedConsumers => 4,
        ReusableStateLifecycle::Preferred => 5,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReusableStateHeatClass {
    Cold,
    Warm,
    Hot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReusableStateSizeClass {
    Empty,
    Tiny,
    Small,
    Medium,
    Large,
    Huge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReusableStateRecentHitClass {
    Never,
    WithinHour,
    WithinDay,
    WithinWeek,
    Older,
    ClockSkew,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReusableStateStatusSummary {
    pub schema_version: u8,
    pub cache_class: ReusableStateClass,
    pub generation: ReusableStateGenerationId,
    pub heat: ReusableStateHeatClass,
    pub size: ReusableStateSizeClass,
    pub recent_hit: ReusableStateRecentHitClass,
    pub revalidation_required: bool,
}

impl fmt::Display for ReusableStateStatusSummary {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "cache_class={} generation={} heat={:?} size={:?} recent_hit={:?} revalidation_required={}",
            self.cache_class.as_str(),
            self.generation.0.as_str(),
            self.heat,
            self.size,
            self.recent_hit,
            self.revalidation_required
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReusableStateUtilityReport {
    pub schema_version: u8,
    pub cache_class: ReusableStateClass,
    pub generation: ReusableStateGenerationId,
    pub storage_size_bytes: u64,
    pub hit_rate_basis_points: u16,
    pub estimated_saved_per_hit_millis: u64,
    pub restore_duration_millis: u64,
    pub publication_duration_millis: u64,
    pub net_time_saved_millis: i64,
    pub last_useful_hit_epoch_millis: Option<u64>,
    pub recommendation: ReusableStateRecommendation,
}

impl ReusableStateUtilityReport {
    /// # Errors
    ///
    /// Returns an error for invalid utility evidence.
    pub fn build(
        generation: &ReusableStateGeneration,
        policy: ReusableStatePromotionPolicy,
    ) -> Result<Self, ReusableStatePolicyError> {
        let utility = generation.metrics.utility()?;
        Ok(Self {
            schema_version: REUSABLE_STATE_LIFECYCLE_SCHEMA_VERSION,
            cache_class: generation.identity.state_class,
            generation: generation.generation.clone(),
            storage_size_bytes: generation.metrics.storage_size_bytes,
            hit_rate_basis_points: generation.metrics.hit_rate_basis_points(),
            estimated_saved_per_hit_millis: utility.estimated_saved_per_hit_millis,
            restore_duration_millis: generation.metrics.restore_duration_millis,
            publication_duration_millis: generation.metrics.publication_duration_millis,
            net_time_saved_millis: utility.net_time_saved_millis,
            last_useful_hit_epoch_millis: generation.metrics.last_useful_hit_epoch_millis,
            recommendation: generation.recommendation(policy)?,
        })
    }
}

fn classify_size(bytes: u64) -> ReusableStateSizeClass {
    match bytes {
        0 => ReusableStateSizeClass::Empty,
        1..=67_108_863 => ReusableStateSizeClass::Tiny,
        67_108_864..=536_870_911 => ReusableStateSizeClass::Small,
        536_870_912..=2_147_483_647 => ReusableStateSizeClass::Medium,
        2_147_483_648..=8_589_934_591 => ReusableStateSizeClass::Large,
        _ => ReusableStateSizeClass::Huge,
    }
}

fn classify_recent_hit(last_hit: Option<u64>, now: u64) -> ReusableStateRecentHitClass {
    let Some(last_hit) = last_hit else {
        return ReusableStateRecentHitClass::Never;
    };
    let Some(age) = now.checked_sub(last_hit) else {
        return ReusableStateRecentHitClass::ClockSkew;
    };
    if age <= HOUR_MILLIS {
        ReusableStateRecentHitClass::WithinHour
    } else if age <= DAY_MILLIS {
        ReusableStateRecentHitClass::WithinDay
    } else if age <= WEEK_MILLIS {
        ReusableStateRecentHitClass::WithinWeek
    } else {
        ReusableStateRecentHitClass::Older
    }
}

fn hash_field(hasher: &mut Sha256, name: &[u8], value: &[u8]) {
    hasher.update((name.len() as u64).to_be_bytes());
    hasher.update(name);
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn hash_digest(hasher: &mut Sha256, name: &[u8], digest: &Sha256Digest) {
    hash_field(hasher, name, digest.as_str().as_bytes());
}

fn hash_optional(hasher: &mut Sha256, name: &[u8], digest: Option<&Sha256Digest>) {
    match digest {
        Some(digest) => hash_digest(hasher, name, digest),
        None => hash_field(hasher, name, b"absent"),
    }
}

fn digest_bytes(bytes: &[u8]) -> Result<Sha256Digest, ReusableStatePolicyError> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut value = String::with_capacity(71);
    value.push_str("sha256:");
    for byte in bytes {
        value.push(char::from(HEX[usize::from(byte >> 4)]));
        value.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Sha256Digest::parse(&value).map_err(|_| ReusableStatePolicyError::IdentityDigest)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReusableStatePolicyError {
    InvalidArchitecture,
    IdentityDigest,
    MetricOutOfRange,
    InconsistentMetrics,
    UtilityOverflow,
    ContradictoryRetention,
    PublisherRequired,
    UnsupportedIdentitySchema,
    InvalidTransition,
    PromotionEvidenceMissing,
    GenerationInUse,
    InvalidPromotionPolicy,
    InvalidDiskBudget,
    TooManyGenerations,
}

impl fmt::Display for ReusableStatePolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "reusable-state policy error: {self:?}")
    }
}

impl std::error::Error for ReusableStatePolicyError {}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn digest(hex: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", hex.to_string().repeat(64))).unwrap()
    }

    fn identity(class: ReusableStateClass) -> ReusableStateIdentityContract {
        ReusableStateIdentityContract::new(
            class,
            RepositoryRef::parse("teamleaderleo/glaeda").unwrap(),
            ReusableStateArchitecture::parse("arm64").unwrap(),
            digest('1'),
            digest('2'),
            Some(digest('3')),
            Some(digest('4')),
            Some(digest('5')),
            Some(digest('6')),
            digest('7'),
            digest('8'),
        )
    }

    fn metrics(
        lookups: u64,
        hits: u64,
        cold: u64,
        warm: u64,
        storage: u64,
        producers: u64,
        consumers: u64,
    ) -> ReusableStateMetrics {
        ReusableStateMetrics {
            lookups,
            hits,
            misses: lookups - hits,
            restore_duration_millis: 100,
            publication_duration_millis: 500,
            bytes_read: 1_000,
            bytes_written: 1_000,
            storage_size_bytes: storage,
            estimated_cold_work_millis: cold,
            estimated_warm_work_millis: warm,
            last_useful_hit_epoch_millis: Some(10_000),
            validation_failures: 0,
            reset_invalidation_count: 0,
            reset_invalidation_overhead_millis: 0,
            producer_successes: producers,
            successful_consumers: consumers,
            semantic_mismatches: 0,
        }
    }

    fn candidate(
        class: ReusableStateClass,
        metrics: ReusableStateMetrics,
    ) -> ReusableStateGeneration {
        ReusableStateGeneration::candidate(
            ReusableStatePublisherAuthority::ReviewedTrustedPublisher,
            identity(class),
            ReusableStateGenerationId(digest('a')),
            ReusableStatePublicationState::Complete,
            ReusableStateIntegrityState::Verified,
            false,
            metrics,
            ReusableStateRetentionFacts {
                reconstructible: true,
                unique_local_work: false,
                must_retain: false,
                in_use_consumers: 0,
            },
        )
        .unwrap()
    }

    fn preferred(mut generation: ReusableStateGeneration) -> ReusableStateGeneration {
        let policy = ReusableStatePromotionPolicy::conservative();
        for target in [
            ReusableStateLifecycle::Validated,
            ReusableStateLifecycle::ObservedConsumers,
            ReusableStateLifecycle::Preferred,
        ] {
            if generation.lifecycle < target {
                generation = generation.transition(target, policy).unwrap();
            }
        }
        generation
    }

    #[test]
    fn identity_changes_when_any_common_validity_dimension_changes() {
        let baseline = identity(ReusableStateClass::IncrementalBuildState);
        let expected = baseline.digest().unwrap();
        let mut variants = Vec::new();
        macro_rules! variant {
            ($field:ident = $value:expr) => {{
                let mut value = baseline.clone();
                value.$field = $value;
                variants.push(value);
            }};
        }
        variant!(state_class = ReusableStateClass::CompilerCache);
        variant!(repository = RepositoryRef::parse("teamleaderleo/other").unwrap());
        variant!(architecture = ReusableStateArchitecture::parse("x86_64").unwrap());
        variant!(toolchain_generation = digest('9'));
        variant!(os_runtime_generation = digest('9'));
        variant!(dependency_lock_digest = Some(digest('9')));
        variant!(build_configuration_digest = Some(digest('9')));
        variant!(compiler_flags_digest = Some(digest('9')));
        variant!(prepared_environment_generation = Some(digest('9')));
        variant!(cache_schema_generation = digest('9'));
        variant!(family_inputs_digest = digest('9'));
        assert!(
            variants
                .into_iter()
                .all(|value| value.digest().unwrap() != expected)
        );
    }

    #[test]
    fn consumer_only_work_cannot_create_future_reuse_candidate() {
        let error = ReusableStateGeneration::candidate(
            ReusableStatePublisherAuthority::ConsumerOnly,
            identity(ReusableStateClass::PackageManagerState),
            ReusableStateGenerationId(digest('a')),
            ReusableStatePublicationState::Complete,
            ReusableStateIntegrityState::Verified,
            false,
            metrics(3, 3, 1_000, 100, 10_000, 2, 3),
            ReusableStateRetentionFacts {
                reconstructible: true,
                unique_local_work: false,
                must_retain: false,
                in_use_consumers: 0,
            },
        )
        .unwrap_err();
        assert_eq!(error, ReusableStatePolicyError::PublisherRequired);
    }

    #[test]
    fn unknown_identity_schema_cannot_enter_the_lifecycle() {
        let mut future = identity(ReusableStateClass::PackageManagerState);
        future.schema_version = 2;
        let error = ReusableStateGeneration::candidate(
            ReusableStatePublisherAuthority::ReviewedTrustedPublisher,
            future,
            ReusableStateGenerationId(digest('a')),
            ReusableStatePublicationState::Complete,
            ReusableStateIntegrityState::Verified,
            false,
            metrics(3, 3, 1_000, 100, 10_000, 2, 3),
            ReusableStateRetentionFacts {
                reconstructible: true,
                unique_local_work: false,
                must_retain: false,
                in_use_consumers: 0,
            },
        )
        .unwrap_err();
        assert_eq!(error, ReusableStatePolicyError::UnsupportedIdentitySchema);
    }

    #[test]
    fn preferred_requires_multiple_producers_consumers_and_positive_value() {
        let policy = ReusableStatePromotionPolicy::conservative();
        let observed = candidate(
            ReusableStateClass::CompilerCache,
            metrics(3, 3, 10_000, 1_000, 1_000_000, 1, 3),
        )
        .transition(ReusableStateLifecycle::Validated, policy)
        .unwrap()
        .transition(ReusableStateLifecycle::ObservedConsumers, policy)
        .unwrap();
        assert_eq!(
            observed
                .clone()
                .transition(ReusableStateLifecycle::Preferred, policy)
                .unwrap_err(),
            ReusableStatePolicyError::PromotionEvidenceMissing
        );
        let mut observed = observed;
        observed.metrics.producer_successes = 2;
        assert_eq!(
            observed
                .transition(ReusableStateLifecycle::Preferred, policy)
                .unwrap()
                .lifecycle,
            ReusableStateLifecycle::Preferred
        );
    }

    #[test]
    fn utility_subtracts_restore_publication_and_invalidation_with_storage_separate() {
        let mut observed = metrics(4, 3, 10_000, 2_000, 50_000_000, 2, 3);
        observed.restore_duration_millis = 900;
        observed.publication_duration_millis = 500;
        observed.reset_invalidation_overhead_millis = 600;
        let utility = observed.utility().unwrap();
        assert_eq!(utility.avoided_work_millis, 24_000);
        assert_eq!(utility.net_time_saved_millis, 22_000);
        assert_eq!(utility.storage_size_bytes, 50_000_000);
    }

    #[test]
    fn cargo_incremental_proving_observation_recommends_retain() {
        // #926 measured 43.985s cold -> 3.355s warm with ~1.895GB retained target.
        let mut evidence = metrics(6, 6, 43_985, 3_355, 1_895_000_000, 2, 6);
        evidence.restore_duration_millis = 0;
        evidence.publication_duration_millis = 43_985;
        let cargo = preferred(candidate(
            ReusableStateClass::IncrementalBuildState,
            evidence,
        ));
        assert_eq!(
            cargo
                .recommendation(ReusableStatePromotionPolicy::conservative())
                .unwrap(),
            ReusableStateRecommendation::Retain
        );
    }

    #[test]
    fn slower_xcode_compiler_cache_stops_future_publication() {
        // #1048 observed cache-hit checks at 65-76s versus 51-55s hashing-only controls.
        let mut evidence = metrics(3, 3, 53_000, 70_500, 1, 2, 3);
        evidence.restore_duration_millis = 0;
        evidence.publication_duration_millis = 0;
        let observed = candidate(ReusableStateClass::CompilerCache, evidence)
            .transition(
                ReusableStateLifecycle::Validated,
                ReusableStatePromotionPolicy::conservative(),
            )
            .unwrap()
            .transition(
                ReusableStateLifecycle::ObservedConsumers,
                ReusableStatePromotionPolicy::conservative(),
            )
            .unwrap();
        assert_eq!(
            observed
                .recommendation(ReusableStatePromotionPolicy::conservative())
                .unwrap(),
            ReusableStateRecommendation::StopPublishing
        );
    }

    #[test]
    fn low_trust_consumption_is_preferred_and_read_only() {
        let policy = ReusableStatePromotionPolicy::conservative();
        let validated = candidate(
            ReusableStateClass::ImmutableCompiledProduct,
            metrics(3, 3, 10_000, 1_000, 606_055_512, 2, 3),
        )
        .transition(ReusableStateLifecycle::Validated, policy)
        .unwrap();
        assert!(matches!(
            evaluate_reusable_state_consumption(
                &validated.identity,
                &validated,
                ReusableStateConsumerTrust::LowTrust,
                ReusableStateConsumptionPolicy {
                    low_trust_read_only_allowed: true
                },
            ),
            ReusableStateConsumptionDisposition::MissReset { .. }
        ));
        let preferred = preferred(validated);
        assert_eq!(
            evaluate_reusable_state_consumption(
                &preferred.identity,
                &preferred,
                ReusableStateConsumerTrust::LowTrust,
                ReusableStateConsumptionPolicy {
                    low_trust_read_only_allowed: true
                },
            ),
            ReusableStateConsumptionDisposition::Hit {
                mode: ReusableStateConsumptionMode::ReadOnly
            }
        );
    }

    #[test]
    fn corruption_identity_and_publication_failures_are_miss_reset() {
        let base = candidate(
            ReusableStateClass::PreparedDependencyGeneration,
            metrics(3, 3, 10_000, 1_000, 10_000, 2, 3),
        )
        .transition(
            ReusableStateLifecycle::Validated,
            ReusableStatePromotionPolicy::conservative(),
        )
        .unwrap();
        let consume = |expected: &ReusableStateIdentityContract,
                       generation: &ReusableStateGeneration| {
            evaluate_reusable_state_consumption(
                expected,
                generation,
                ReusableStateConsumerTrust::Trusted,
                ReusableStateConsumptionPolicy {
                    low_trust_read_only_allowed: true,
                },
            )
        };

        let mut expected = base.identity.clone();
        expected.toolchain_generation = digest('9');
        assert_eq!(
            consume(&expected, &base),
            ReusableStateConsumptionDisposition::MissReset {
                reason: ReusableStateMissReason::IdentityMismatch(
                    ReusableStateIdentityMismatch::ToolchainGeneration
                )
            }
        );
        let mut expected = base.identity.clone();
        expected.dependency_lock_digest = Some(digest('9'));
        assert_eq!(
            consume(&expected, &base),
            ReusableStateConsumptionDisposition::MissReset {
                reason: ReusableStateMissReason::IdentityMismatch(
                    ReusableStateIdentityMismatch::DependencyLockDigest
                )
            }
        );

        for (publication, reason) in [
            (
                ReusableStatePublicationState::Partial,
                ReusableStateMissReason::PartialPublish,
            ),
            (
                ReusableStatePublicationState::DiskFull,
                ReusableStateMissReason::DiskFullPublication,
            ),
            (
                ReusableStatePublicationState::ProducerCrashed,
                ReusableStateMissReason::ProducerCrash,
            ),
            (
                ReusableStatePublicationState::ConcurrentPublisherConflict,
                ReusableStateMissReason::ConcurrentPublisherConflict,
            ),
        ] {
            let mut broken = base.clone();
            broken.publication = publication;
            assert_eq!(
                consume(&broken.identity, &broken),
                ReusableStateConsumptionDisposition::MissReset { reason }
            );
        }
        for (integrity, reason) in [
            (
                ReusableStateIntegrityState::Truncated,
                ReusableStateMissReason::TruncatedGeneration,
            ),
            (
                ReusableStateIntegrityState::Corrupt,
                ReusableStateMissReason::CorruptGeneration,
            ),
        ] {
            let mut broken = base.clone();
            broken.integrity = integrity;
            assert_eq!(
                consume(&broken.identity, &broken),
                ReusableStateConsumptionDisposition::MissReset { reason }
            );
        }
    }

    #[test]
    fn invalidated_preferred_generation_becomes_miss_reset() {
        let mut generation = preferred(candidate(
            ReusableStateClass::ContainerLayer,
            metrics(3, 3, 10_000, 1_000, 10_000, 2, 3),
        ));
        generation.revalidation_required = true;
        assert_eq!(
            evaluate_reusable_state_consumption(
                &generation.identity,
                &generation,
                ReusableStateConsumerTrust::Trusted,
                ReusableStateConsumptionPolicy {
                    low_trust_read_only_allowed: true,
                },
            ),
            ReusableStateConsumptionDisposition::MissReset {
                reason: ReusableStateMissReason::InvalidatedPreferredGeneration
            }
        );
    }

    #[test]
    fn incomplete_package_cache_requires_revalidation() {
        // #21: strict offline npm ci failed because nominal warm state lacked zip-dir@2.0.0.
        let mut generation = candidate(
            ReusableStateClass::PackageManagerState,
            metrics(3, 2, 4_309, 900, 191_680_512, 2, 2),
        );
        generation.revalidation_required = true;
        assert_eq!(
            generation
                .recommendation(ReusableStatePromotionPolicy::conservative())
                .unwrap(),
            ReusableStateRecommendation::Revalidate
        );
    }

    #[test]
    fn consumer_crash_visible_use_blocks_whole_generation_eviction() {
        let mut generation = candidate(
            ReusableStateClass::ProjectLocalApprovedHotState,
            metrics(3, 0, 1_000, 1_000, 600, 2, 0),
        );
        generation.lifecycle = ReusableStateLifecycle::Retired;
        generation.retention.in_use_consumers = 1;
        let plan = plan_reusable_state_eviction(
            ReusableStateDiskBudget {
                low_watermark_bytes: 100,
                high_watermark_bytes: 500,
                max_evictions_per_pass: 8,
            },
            &[generation],
        )
        .unwrap();
        assert!(plan.selected.is_empty());
        assert!(!plan.budget_satisfied);
    }

    #[test]
    fn budget_evicts_low_value_generation_before_high_value_generation() {
        let mut low = candidate(
            ReusableStateClass::CompilerCache,
            metrics(3, 3, 5_000, 6_000, 800, 2, 3),
        );
        low.lifecycle = ReusableStateLifecycle::Demoted;
        low.generation = ReusableStateGenerationId(digest('b'));
        let hot = preferred(candidate(
            ReusableStateClass::IncrementalBuildState,
            metrics(6, 6, 40_000, 3_000, 400, 2, 6),
        ));
        let mut unique = candidate(
            ReusableStateClass::ProjectLocalApprovedHotState,
            metrics(3, 3, 10_000, 1_000, 600, 2, 3),
        );
        unique.retention = ReusableStateRetentionFacts {
            reconstructible: false,
            unique_local_work: true,
            must_retain: true,
            in_use_consumers: 0,
        };
        unique.generation = ReusableStateGenerationId(digest('c'));

        let plan = plan_reusable_state_eviction(
            ReusableStateDiskBudget {
                low_watermark_bytes: 1_100,
                high_watermark_bytes: 1_500,
                max_evictions_per_pass: 8,
            },
            &[hot, low, unique],
        )
        .unwrap();
        assert_eq!(plan.selected.len(), 1);
        assert_eq!(plan.selected[0].generation.0.as_str(), digest('b').as_str());
        assert!(plan.budget_satisfied);
    }

    #[test]
    fn status_is_small_agent_readable_and_path_free() {
        let hot = preferred(candidate(
            ReusableStateClass::IncrementalBuildState,
            metrics(6, 6, 43_985, 3_355, 1_895_000_000, 2, 6),
        ));
        let status = hot.status(10_500).unwrap();
        let encoded = serde_json::to_string(&status).unwrap();
        assert_eq!(status.heat, ReusableStateHeatClass::Hot);
        assert_eq!(status.size, ReusableStateSizeClass::Medium);
        for forbidden in [
            "/Users/",
            "/home/",
            "credential",
            "source_content",
            "argv",
            "log",
        ] {
            assert!(!encoded.contains(forbidden));
        }
        assert_eq!(
            serde_json::to_value(status).unwrap(),
            json!({
                "schema_version": 1,
                "cache_class": "incremental_build_state",
                "generation": digest('a').as_str(),
                "heat": "hot",
                "size": "medium",
                "recent_hit": "within_hour",
                "revalidation_required": false
            })
        );
    }
}
