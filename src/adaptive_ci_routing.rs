//! Pure evidence-driven CI placement recommendations across generic execution pools.
//!
//! This module is deliberately recommendation-only. It consumes bounded observations and configured
//! pool/accounting facts, predicts completion ranges, applies an explicit operator policy, and emits
//! one inspectable report. It performs no dispatch, admission, host probing, billing API calls,
//! persistence, or lifecycle mutation.

use std::cmp::Ordering;
use std::fmt;

use serde::Serialize;

use crate::compute_workload::ComputeTrustClass;

pub const ADAPTIVE_CI_ROUTING_SCHEMA_VERSION: u8 = 1;
pub const MAX_ROUTING_ID_BYTES: usize = 96;
pub const MAX_ROUTING_CAPABILITIES: usize = 32;
pub const MAX_ROUTING_OBSERVATIONS: usize = 256;
pub const MAX_ROUTING_PHASE_MILLIS: u64 = 24 * 60 * 60 * 1_000;
pub const MAX_ROUTING_AGE_MILLIS: u64 = 180 * 24 * 60 * 60 * 1_000;
const PARTS_PER_MILLION: u64 = 1_000_000;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct RoutingId(String);

impl RoutingId {
    /// Parse one bounded opaque public routing identity.
    ///
    /// # Errors
    ///
    /// Returns an error for empty, oversized, path-shaped, command-shaped, or non-canonical text.
    pub fn parse(value: &str) -> Result<Self, RoutingError> {
        let Some(first) = value.bytes().next() else {
            return Err(error(
                "identity",
                "routing_identity_empty",
                "routing identity must not be empty",
            ));
        };
        if value.len() > MAX_ROUTING_ID_BYTES
            || !(first.is_ascii_alphanumeric())
            || !value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':' | b'/')
            })
            || value.contains("//")
            || value.contains("..")
        {
            return Err(error(
                "identity",
                "routing_identity_invalid",
                "routing identity must be bounded canonical text",
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
pub struct WorkloadClassV1 {
    pub project: RoutingId,
    pub profile: RoutingId,
    pub trust_class: ComputeTrustClass,
    pub source_class: RoutingId,
    pub input_class: RoutingId,
    pub resource_profile: RoutingId,
    pub requirements: Vec<RoutingId>,
}

impl WorkloadClassV1 {
    /// Construct a stable workload class without binding each source revision into a new class.
    ///
    /// # Errors
    ///
    /// Returns an error for duplicate or excessive requirements.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        project: RoutingId,
        profile: RoutingId,
        trust_class: ComputeTrustClass,
        source_class: RoutingId,
        input_class: RoutingId,
        resource_profile: RoutingId,
        mut requirements: Vec<RoutingId>,
    ) -> Result<Self, RoutingError> {
        if requirements.len() > MAX_ROUTING_CAPABILITIES {
            return Err(error(
                "workload.requirements",
                "routing_too_many_requirements",
                "workload requirements exceed the bounded maximum",
            ));
        }
        requirements.sort();
        if requirements.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(error(
                "workload.requirements",
                "routing_duplicate_requirement",
                "workload requirements must be unique",
            ));
        }
        Ok(Self {
            project,
            profile,
            trust_class,
            source_class,
            input_class,
            resource_profile,
            requirements,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PoolAccountingClass {
    Owned,
    UnmeteredIncluded,
    MeteredIncluded,
    PaidBurst,
}

impl PoolAccountingClass {
    const fn economy_rank(self) -> u8 {
        match self {
            Self::Owned | Self::UnmeteredIncluded => 0,
            Self::MeteredIncluded => 1,
            Self::PaidBurst => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecutionPoolV1 {
    pub pool_id: RoutingId,
    pub execution_class: RoutingId,
    pub accounting_class: PoolAccountingClass,
    pub capabilities: Vec<RoutingId>,
}

impl ExecutionPoolV1 {
    /// Construct one generic execution pool. Provider names, if any, remain opaque configuration.
    ///
    /// # Errors
    ///
    /// Returns an error for duplicate or excessive capabilities.
    pub fn new(
        pool_id: RoutingId,
        execution_class: RoutingId,
        accounting_class: PoolAccountingClass,
        mut capabilities: Vec<RoutingId>,
    ) -> Result<Self, RoutingError> {
        if capabilities.len() > MAX_ROUTING_CAPABILITIES {
            return Err(error(
                "pool.capabilities",
                "routing_too_many_capabilities",
                "pool capabilities exceed the bounded maximum",
            ));
        }
        capabilities.sort();
        if capabilities.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(error(
                "pool.capabilities",
                "routing_duplicate_capability",
                "pool capabilities must be unique",
            ));
        }
        Ok(Self {
            pool_id,
            execution_class,
            accounting_class,
            capabilities,
        })
    }

    fn satisfies(&self, workload: &WorkloadClassV1) -> bool {
        workload
            .requirements
            .iter()
            .all(|required| self.capabilities.binary_search(required).is_ok())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HotStateClass {
    Unknown,
    Cold,
    Warm,
    HotExact,
}

impl HotStateClass {
    const fn preference_rank(self) -> u8 {
        match self {
            Self::Unknown => 0,
            Self::Cold => 1,
            Self::Warm => 2,
            Self::HotExact => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalityEvidenceSource {
    LocalAccepted,
    RemoteAdvisory,
    DeclaredUnknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HotStateEvidenceV1 {
    pub class: HotStateClass,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_identity: Option<RoutingId>,
    pub source: LocalityEvidenceSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostPressureClass {
    Low,
    Moderate,
    High,
    Critical,
    Unknown,
}

impl HostPressureClass {
    const fn policy_rank(self) -> u8 {
        match self {
            Self::Low => 0,
            Self::Moderate => 1,
            Self::High => 2,
            Self::Critical => 3,
            Self::Unknown => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PoolEligibility {
    Eligible,
    Ineligible,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AllowanceBudgetV1 {
    pub period_id: RoutingId,
    pub included_budget_units: u64,
    pub remaining_estimate_units: u64,
    pub reset_at_millis: u64,
    pub observed_consumption_units: u64,
}

impl AllowanceBudgetV1 {
    /// Validate configured bounded accounting input.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero budget or a remaining estimate above the period budget.
    pub fn validate(&self) -> Result<(), RoutingError> {
        if self.included_budget_units == 0 {
            return Err(error(
                "allowance.included_budget_units",
                "routing_allowance_budget_zero",
                "allowance budget must be positive",
            ));
        }
        if self.remaining_estimate_units > self.included_budget_units {
            return Err(error(
                "allowance.remaining_estimate_units",
                "routing_allowance_remaining_exceeds_budget",
                "allowance remaining estimate cannot exceed the configured period budget",
            ));
        }
        Ok(())
    }

    fn projected_remaining_ppm(&self, consumption: u64) -> u32 {
        let remaining = self.remaining_estimate_units.saturating_sub(consumption);
        let ppm = u128::from(remaining) * u128::from(PARTS_PER_MILLION)
            / u128::from(self.included_budget_units);
        u32::try_from(ppm).unwrap_or(1_000_000)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContentionEvidenceV1 {
    pub window_count: u16,
    pub offered_tasks: u32,
    pub validated_completions: u32,
    pub elapsed_millis: u64,
    pub final_result_p50_millis: u64,
    pub final_result_p90_millis: u64,
    pub semantic_mismatches: u16,
    pub failures: u16,
    pub fallbacks: u16,
    pub unfinished: u16,
    pub peak_pressure: HostPressureClass,
}

impl ContentionEvidenceV1 {
    /// Validate one bounded #760-style aggregate.
    ///
    /// # Errors
    ///
    /// Returns an error for impossible counts or zero elapsed time.
    pub fn validate(&self) -> Result<(), RoutingError> {
        if self.window_count == 0 || self.elapsed_millis == 0 {
            return Err(error(
                "contention",
                "routing_contention_empty",
                "contention evidence requires at least one window and positive elapsed time",
            ));
        }
        if self.validated_completions > self.offered_tasks {
            return Err(error(
                "contention.validated_completions",
                "routing_contention_validated_exceeds_offered",
                "validated completions cannot exceed offered tasks",
            ));
        }
        if self.final_result_p90_millis < self.final_result_p50_millis {
            return Err(error(
                "contention.final_result_p90_millis",
                "routing_contention_percentile_order",
                "contention p90 cannot be below p50",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PoolCandidateV1 {
    pub pool: ExecutionPoolV1,
    pub eligibility: PoolEligibility,
    pub hot_state: HotStateEvidenceV1,
    pub pressure_after_admission: HostPressureClass,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowance: Option<AllowanceBudgetV1>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contention: Option<ContentionEvidenceV1>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationOutcome {
    ValidatedSuccess,
    Failure,
    Fallback,
    SemanticMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ObservationTimingV1 {
    pub queue_millis: u64,
    pub start_millis: u64,
    pub preparation_millis: u64,
    pub execution_millis: u64,
    pub settlement_millis: u64,
}

impl ObservationTimingV1 {
    fn validate(self) -> Result<(), RoutingError> {
        for (field, value) in [
            ("queue_millis", self.queue_millis),
            ("start_millis", self.start_millis),
            ("preparation_millis", self.preparation_millis),
            ("execution_millis", self.execution_millis),
            ("settlement_millis", self.settlement_millis),
        ] {
            if value > MAX_ROUTING_PHASE_MILLIS {
                return Err(error(
                    field,
                    "routing_phase_duration_out_of_range",
                    "routing phase duration exceeds the bounded maximum",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ResourceEnvelopeV1 {
    pub cpu_millicores: u32,
    pub memory_bytes: u64,
    pub disk_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoutingObservationV1 {
    pub workload: WorkloadClassV1,
    pub pool_id: RoutingId,
    pub observed_at_millis: u64,
    pub hot_state: HotStateClass,
    pub timing: ObservationTimingV1,
    pub outcome: ObservationOutcome,
    pub resource: ResourceEnvelopeV1,
    pub marginal_cost_microusd: u64,
    pub allowance_units: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct QuantilesU64 {
    pub p50: u64,
    pub p90: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CompletionPredictionV1 {
    pub queue: QuantilesU64,
    pub start: QuantilesU64,
    pub preparation: QuantilesU64,
    pub execution: QuantilesU64,
    pub settlement: QuantilesU64,
    pub total: QuantilesU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ResourcePredictionV1 {
    pub cpu_millicores_p90: u64,
    pub memory_bytes_p90: u64,
    pub disk_bytes_p90: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct EvidenceBasisV1 {
    pub fresh_observations: u16,
    pub validated_successes: u16,
    pub first_observed_at_millis: u64,
    pub last_observed_at_millis: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PoolPredictionV1 {
    pub pool_id: RoutingId,
    pub execution_class: RoutingId,
    pub accounting_class: PoolAccountingClass,
    pub hot_state: HotStateEvidenceV1,
    pub basis: EvidenceBasisV1,
    pub completion: CompletionPredictionV1,
    pub marginal_cost_microusd: QuantilesU64,
    pub allowance_units: QuantilesU64,
    pub resource: ResourcePredictionV1,
    pub failure_permille: u16,
    pub fallback_permille: u16,
    pub semantic_mismatch_count: u16,
    pub pressure_after_admission: HostPressureClass,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projected_allowance_remaining_ppm: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contention: Option<ContentionEvidenceV1>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PredictionConfigV1 {
    pub max_age_millis: u64,
    pub max_samples: u16,
    pub min_validated_samples: u16,
}

impl Default for PredictionConfigV1 {
    fn default() -> Self {
        Self {
            max_age_millis: 30 * 24 * 60 * 60 * 1_000,
            max_samples: 64,
            min_validated_samples: 3,
        }
    }
}

impl PredictionConfigV1 {
    fn validate(self) -> Result<(), RoutingError> {
        if self.max_age_millis == 0 || self.max_age_millis > MAX_ROUTING_AGE_MILLIS {
            return Err(error(
                "prediction.max_age_millis",
                "routing_prediction_age_out_of_range",
                "prediction recency window must be positive and bounded",
            ));
        }
        if self.max_samples == 0
            || usize::from(self.max_samples) > MAX_ROUTING_OBSERVATIONS
            || self.min_validated_samples == 0
            || self.min_validated_samples > self.max_samples
        {
            return Err(error(
                "prediction.samples",
                "routing_prediction_sample_bounds",
                "prediction sample bounds are invalid",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingPolicyMode {
    Economy,
    Balanced,
    Latency,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RoutingPolicyV1 {
    pub mode: RoutingPolicyMode,
    pub acceptable_completion_slack_millis: u64,
    pub min_latency_gain_to_spend_millis: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spend_ceiling_microusd: Option<u64>,
    pub min_allowance_remaining_ppm: u32,
    pub max_failure_permille: u16,
    pub max_pressure: HostPressureClass,
}

impl RoutingPolicyV1 {
    #[must_use]
    pub const fn economy(acceptable_completion_slack_millis: u64) -> Self {
        Self {
            mode: RoutingPolicyMode::Economy,
            acceptable_completion_slack_millis,
            min_latency_gain_to_spend_millis: 0,
            spend_ceiling_microusd: None,
            min_allowance_remaining_ppm: 100_000,
            max_failure_permille: 100,
            max_pressure: HostPressureClass::High,
        }
    }

    #[must_use]
    pub const fn balanced(
        acceptable_completion_slack_millis: u64,
        min_latency_gain_to_spend_millis: u64,
    ) -> Self {
        Self {
            mode: RoutingPolicyMode::Balanced,
            acceptable_completion_slack_millis,
            min_latency_gain_to_spend_millis,
            spend_ceiling_microusd: None,
            min_allowance_remaining_ppm: 50_000,
            max_failure_permille: 100,
            max_pressure: HostPressureClass::High,
        }
    }

    #[must_use]
    pub const fn latency(spend_ceiling_microusd: u64) -> Self {
        Self {
            mode: RoutingPolicyMode::Latency,
            acceptable_completion_slack_millis: 0,
            min_latency_gain_to_spend_millis: 0,
            spend_ceiling_microusd: Some(spend_ceiling_microusd),
            min_allowance_remaining_ppm: 0,
            max_failure_permille: 100,
            max_pressure: HostPressureClass::High,
        }
    }

    fn validate(self) -> Result<(), RoutingError> {
        if self.min_allowance_remaining_ppm > 1_000_000 || self.max_failure_permille > 1_000 {
            return Err(error(
                "policy",
                "routing_policy_ratio_out_of_range",
                "policy ratios exceed their bounded range",
            ));
        }
        if self.max_pressure == HostPressureClass::Unknown {
            return Err(error(
                "policy.max_pressure",
                "routing_policy_unknown_pressure_ceiling",
                "policy pressure ceiling must be an observed pressure class",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendationAuthority {
    RecommendationOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendationStatus {
    Recommended,
    Abstained,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PoolExclusionReason {
    Ineligible,
    EligibilityUnknown,
    MissingCapability,
    MissingEvidence,
    StaleEvidence,
    InsufficientEvidence,
    SemanticMismatch,
    ReliabilityAbovePolicy,
    PressureAbovePolicy,
    SpendCeiling,
    AllowanceReserve,
    InvalidAllowance,
    InvalidContentionEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PoolExclusionV1 {
    pub pool_id: RoutingId,
    pub reason: PoolExclusionReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoutingRecommendationV1 {
    pub schema_version: u8,
    pub authority: RecommendationAuthority,
    pub automatic_routing_eligible: bool,
    pub workload: WorkloadClassV1,
    pub policy: RoutingPolicyV1,
    pub status: RecommendationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub choice: Option<RoutingId>,
    pub predictions: Vec<PoolPredictionV1>,
    pub exclusions: Vec<PoolExclusionV1>,
}

impl RoutingRecommendationV1 {
    /// Render an inspectable human explanation from the same typed report as JSON output.
    #[must_use]
    pub fn render_human(&self) -> String {
        let mut out = format!(
            "workload: {} / {}
policy: {:?}
authority: recommendation_only
automatic routing: disabled
",
            self.workload.project.as_str(),
            self.workload.profile.as_str(),
            self.policy.mode
        );
        match &self.choice {
            Some(choice) => {
                out.push_str(&format!("choice: {}
", choice.as_str()));
                if let Some(prediction) = self
                    .predictions
                    .iter()
                    .find(|prediction| &prediction.pool_id == choice)
                {
                    out.push_str("basis:
");
                    out.push_str(&format!(
                        "  compatible hot state: {:?}
  predicted completion: {}-{} ms
  queue: {}-{} ms
  preparation: {}-{} ms
  execution: {}-{} ms
  settlement: {}-{} ms
  comparable validated runs: {}
  marginal money cost: {}-{} microUSD
  allowance consumption: {}-{} units
  pressure after admission: {:?}
  failure rate: {}/1000
  fallback rate: {}/1000
",
                        prediction.hot_state.class,
                        prediction.completion.total.p50,
                        prediction.completion.total.p90,
                        prediction.completion.queue.p50,
                        prediction.completion.queue.p90,
                        prediction.completion.preparation.p50,
                        prediction.completion.preparation.p90,
                        prediction.completion.execution.p50,
                        prediction.completion.execution.p90,
                        prediction.completion.settlement.p50,
                        prediction.completion.settlement.p90,
                        prediction.basis.validated_successes,
                        prediction.marginal_cost_microusd.p50,
                        prediction.marginal_cost_microusd.p90,
                        prediction.allowance_units.p50,
                        prediction.allowance_units.p90,
                        prediction.pressure_after_admission,
                        prediction.failure_permille,
                        prediction.fallback_permille,
                    ));
                    if prediction.hot_state.source == LocalityEvidenceSource::RemoteAdvisory {
                        out.push_str(
                            "  locality source: remote advisory; pool eligibility remains independently declared
",
                        );
                    }
                    if let Some(remaining) = prediction.projected_allowance_remaining_ppm {
                        out.push_str(&format!(
                            "  projected allowance remaining: {remaining}/1000000
"
                        ));
                    }
                    if let Some(contention) = &prediction.contention {
                        out.push_str(&format!(
                            "  contention: {}/{} validated in {} ms; p90 {} ms; pressure {:?}
",
                            contention.validated_completions,
                            contention.offered_tasks,
                            contention.elapsed_millis,
                            contention.final_result_p90_millis,
                            contention.peak_pressure
                        ));
                    }
                }
            }
            None => out.push_str("choice: abstained
"),
        }
        if self.predictions.len() > 1 {
            out.push_str("alternatives:
");
            for prediction in &self.predictions {
                if self.choice.as_ref() == Some(&prediction.pool_id) {
                    continue;
                }
                out.push_str(&format!(
                    "  {}: {}-{} ms, {}-{} microUSD, {:?}
",
                    prediction.pool_id.as_str(),
                    prediction.completion.total.p50,
                    prediction.completion.total.p90,
                    prediction.marginal_cost_microusd.p50,
                    prediction.marginal_cost_microusd.p90,
                    prediction.accounting_class
                ));
            }
        }
        if !self.exclusions.is_empty() {
            out.push_str("excluded:
");
            for exclusion in &self.exclusions {
                out.push_str(&format!(
                    "  {}: {:?}
",
                    exclusion.pool_id.as_str(),
                    exclusion.reason
                ));
            }
        }
        out
    }

    /// Render deterministic pretty JSON from the typed report.
    ///
    /// # Errors
    ///
    /// Returns only if serialization of the fixed report fails.
    pub fn render_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

/// Produce a pure recommendation from bounded observations and explicit policy.
///
/// Remote heat evidence may affect the predicted locality class for an already eligible pool, but
/// it never changes pool eligibility. The returned report also carries zero dispatch authority.
///
/// # Errors
///
/// Returns an error for invalid global configuration or malformed observation timing.
pub fn recommend_ci_pool(
    workload: &WorkloadClassV1,
    candidates: &[PoolCandidateV1],
    observations: &[RoutingObservationV1],
    now_millis: u64,
    prediction_config: PredictionConfigV1,
    policy: RoutingPolicyV1,
) -> Result<RoutingRecommendationV1, RoutingError> {
    prediction_config.validate()?;
    policy.validate()?;
    if observations.len() > MAX_ROUTING_OBSERVATIONS {
        return Err(error(
            "observations",
            "routing_too_many_observations",
            "routing observation input exceeds the bounded maximum",
        ));
    }
    for observation in observations {
        observation.timing.validate()?;
    }

    let mut predictions = Vec::new();
    let mut exclusions = Vec::new();
    let mut evaluated = Vec::new();

    for candidate in candidates {
        let pool_id = candidate.pool.pool_id.clone();
        let hard_exclusion = match candidate.eligibility {
            PoolEligibility::Ineligible => Some(PoolExclusionReason::Ineligible),
            PoolEligibility::Unknown => Some(PoolExclusionReason::EligibilityUnknown),
            PoolEligibility::Eligible if !candidate.pool.satisfies(workload) => {
                Some(PoolExclusionReason::MissingCapability)
            }
            PoolEligibility::Eligible => None,
        };
        if let Some(reason) = hard_exclusion {
            exclusions.push(PoolExclusionV1 { pool_id, reason });
            continue;
        }

        if let Some(allowance) = &candidate.allowance {
            if allowance.validate().is_err() {
                exclusions.push(PoolExclusionV1 {
                    pool_id,
                    reason: PoolExclusionReason::InvalidAllowance,
                });
                continue;
            }
        }
        if let Some(contention) = &candidate.contention {
            if contention.validate().is_err() {
                exclusions.push(PoolExclusionV1 {
                    pool_id,
                    reason: PoolExclusionReason::InvalidContentionEvidence,
                });
                continue;
            }
        }

        let prediction = match predict_pool(
            workload,
            candidate,
            observations,
            now_millis,
            prediction_config,
        ) {
            Ok(prediction) => prediction,
            Err(EvidenceRefusal::Missing) => {
                exclusions.push(PoolExclusionV1 {
                    pool_id,
                    reason: PoolExclusionReason::MissingEvidence,
                });
                continue;
            }
            Err(EvidenceRefusal::Stale) => {
                exclusions.push(PoolExclusionV1 {
                    pool_id,
                    reason: PoolExclusionReason::StaleEvidence,
                });
                continue;
            }
            Err(EvidenceRefusal::Insufficient) => {
                exclusions.push(PoolExclusionV1 {
                    pool_id,
                    reason: PoolExclusionReason::InsufficientEvidence,
                });
                continue;
            }
        };

        let policy_exclusion = policy_exclusion(&prediction, candidate, policy);
        predictions.push(prediction.clone());
        if let Some(reason) = policy_exclusion {
            exclusions.push(PoolExclusionV1 { pool_id, reason });
        } else {
            evaluated.push(EvaluatedCandidate { prediction });
        }
    }

    predictions.sort_by(|left, right| left.pool_id.cmp(&right.pool_id));
    exclusions.sort_by(|left, right| left.pool_id.cmp(&right.pool_id));

    let choice =
        select_candidate(&evaluated, policy).map(|entry| entry.prediction.pool_id.clone());
    let status = if choice.is_some() {
        RecommendationStatus::Recommended
    } else {
        RecommendationStatus::Abstained
    };

    Ok(RoutingRecommendationV1 {
        schema_version: ADAPTIVE_CI_ROUTING_SCHEMA_VERSION,
        authority: RecommendationAuthority::RecommendationOnly,
        automatic_routing_eligible: false,
        workload: workload.clone(),
        policy,
        status,
        choice,
        predictions,
        exclusions,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvidenceRefusal {
    Missing,
    Stale,
    Insufficient,
}

struct EvaluatedCandidate {
    prediction: PoolPredictionV1,
}

fn predict_pool(
    workload: &WorkloadClassV1,
    candidate: &PoolCandidateV1,
    observations: &[RoutingObservationV1],
    now_millis: u64,
    config: PredictionConfigV1,
) -> Result<PoolPredictionV1, EvidenceRefusal> {
    let matching: Vec<&RoutingObservationV1> = observations
        .iter()
        .filter(|observation| {
            observation.workload == *workload
                && observation.pool_id == candidate.pool.pool_id
                && observation.hot_state == candidate.hot_state.class
        })
        .collect();
    if matching.is_empty() {
        return Err(EvidenceRefusal::Missing);
    }

    let mut fresh: Vec<&RoutingObservationV1> = matching
        .into_iter()
        .filter(|observation| {
            observation.observed_at_millis <= now_millis
                && now_millis - observation.observed_at_millis <= config.max_age_millis
        })
        .collect();
    if fresh.is_empty() {
        return Err(EvidenceRefusal::Stale);
    }
    fresh.sort_by_key(|observation| std::cmp::Reverse(observation.observed_at_millis));
    fresh.truncate(usize::from(config.max_samples));

    let successes: Vec<&RoutingObservationV1> = fresh
        .iter()
        .copied()
        .filter(|observation| observation.outcome == ObservationOutcome::ValidatedSuccess)
        .collect();
    if successes.len() < usize::from(config.min_validated_samples) {
        return Err(EvidenceRefusal::Insufficient);
    }

    let queue = quantiles(
        successes
            .iter()
            .map(|observation| observation.timing.queue_millis),
    );
    let start = quantiles(
        successes
            .iter()
            .map(|observation| observation.timing.start_millis),
    );
    let preparation = quantiles(
        successes
            .iter()
            .map(|observation| observation.timing.preparation_millis),
    );
    let execution = quantiles(
        successes
            .iter()
            .map(|observation| observation.timing.execution_millis),
    );
    let settlement = quantiles(
        successes
            .iter()
            .map(|observation| observation.timing.settlement_millis),
    );
    let total = QuantilesU64 {
        p50: sum_components([
            queue.p50,
            start.p50,
            preparation.p50,
            execution.p50,
            settlement.p50,
        ]),
        p90: sum_components([
            queue.p90,
            start.p90,
            preparation.p90,
            execution.p90,
            settlement.p90,
        ]),
    };

    let marginal_cost_microusd = quantiles(
        successes
            .iter()
            .map(|observation| observation.marginal_cost_microusd),
    );
    let allowance_units = quantiles(
        successes
            .iter()
            .map(|observation| observation.allowance_units),
    );
    let resource = ResourcePredictionV1 {
        cpu_millicores_p90: quantiles(
            successes
                .iter()
                .map(|observation| u64::from(observation.resource.cpu_millicores)),
        )
        .p90,
        memory_bytes_p90: quantiles(
            successes
                .iter()
                .map(|observation| observation.resource.memory_bytes),
        )
        .p90,
        disk_bytes_p90: quantiles(
            successes
                .iter()
                .map(|observation| observation.resource.disk_bytes),
        )
        .p90,
    };

    let failures = fresh
        .iter()
        .filter(|observation| observation.outcome == ObservationOutcome::Failure)
        .count();
    let fallbacks = fresh
        .iter()
        .filter(|observation| observation.outcome == ObservationOutcome::Fallback)
        .count();
    let semantic_mismatches = fresh
        .iter()
        .filter(|observation| observation.outcome == ObservationOutcome::SemanticMismatch)
        .count();
    let denominator = fresh.len();
    let failure_permille = rate_permille(failures + semantic_mismatches, denominator);
    let fallback_permille = rate_permille(fallbacks, denominator);
    let first_observed_at_millis = fresh
        .iter()
        .map(|observation| observation.observed_at_millis)
        .min()
        .unwrap_or(now_millis);
    let last_observed_at_millis = fresh
        .iter()
        .map(|observation| observation.observed_at_millis)
        .max()
        .unwrap_or(now_millis);
    let projected_allowance_remaining_ppm = candidate
        .allowance
        .as_ref()
        .map(|allowance| allowance.projected_remaining_ppm(allowance_units.p50));

    Ok(PoolPredictionV1 {
        pool_id: candidate.pool.pool_id.clone(),
        execution_class: candidate.pool.execution_class.clone(),
        accounting_class: candidate.pool.accounting_class,
        hot_state: candidate.hot_state.clone(),
        basis: EvidenceBasisV1 {
            fresh_observations: u16::try_from(fresh.len()).unwrap_or(u16::MAX),
            validated_successes: u16::try_from(successes.len()).unwrap_or(u16::MAX),
            first_observed_at_millis,
            last_observed_at_millis,
        },
        completion: CompletionPredictionV1 {
            queue,
            start,
            preparation,
            execution,
            settlement,
            total,
        },
        marginal_cost_microusd,
        allowance_units,
        resource,
        failure_permille,
        fallback_permille,
        semantic_mismatch_count: u16::try_from(semantic_mismatches).unwrap_or(u16::MAX),
        pressure_after_admission: candidate.pressure_after_admission,
        projected_allowance_remaining_ppm,
        contention: candidate.contention.clone(),
    })
}

fn policy_exclusion(
    prediction: &PoolPredictionV1,
    candidate: &PoolCandidateV1,
    policy: RoutingPolicyV1,
) -> Option<PoolExclusionReason> {
    if prediction.semantic_mismatch_count > 0
        || candidate
            .contention
            .as_ref()
            .is_some_and(|contention| contention.semantic_mismatches > 0)
    {
        return Some(PoolExclusionReason::SemanticMismatch);
    }
    if prediction.failure_permille > policy.max_failure_permille {
        return Some(PoolExclusionReason::ReliabilityAbovePolicy);
    }
    if prediction.pressure_after_admission.policy_rank() > policy.max_pressure.policy_rank()
        || candidate
            .contention
            .as_ref()
            .is_some_and(|contention| contention.peak_pressure == HostPressureClass::Critical)
    {
        return Some(PoolExclusionReason::PressureAbovePolicy);
    }
    if policy
        .spend_ceiling_microusd
        .is_some_and(|ceiling| prediction.marginal_cost_microusd.p50 > ceiling)
    {
        return Some(PoolExclusionReason::SpendCeiling);
    }
    if candidate.allowance.is_some()
        && prediction
            .projected_allowance_remaining_ppm
            .is_some_and(|remaining| remaining < policy.min_allowance_remaining_ppm)
    {
        return Some(PoolExclusionReason::AllowanceReserve);
    }
    None
}

fn select_candidate<'a>(
    candidates: &'a [EvaluatedCandidate],
    policy: RoutingPolicyV1,
) -> Option<&'a EvaluatedCandidate> {
    match policy.mode {
        RoutingPolicyMode::Economy => select_economy(candidates, policy),
        RoutingPolicyMode::Balanced => {
            let economy = select_economy(candidates, policy)?;
            let fastest = candidates
                .iter()
                .min_by(|left, right| latency_cmp(left, right))?;
            if fastest
                .prediction
                .completion
                .total
                .p50
                .saturating_add(policy.min_latency_gain_to_spend_millis)
                <= economy.prediction.completion.total.p50
            {
                Some(fastest)
            } else {
                Some(economy)
            }
        }
        RoutingPolicyMode::Latency => candidates
            .iter()
            .min_by(|left, right| latency_cmp(left, right)),
    }
}

fn select_economy<'a>(
    candidates: &'a [EvaluatedCandidate],
    policy: RoutingPolicyV1,
) -> Option<&'a EvaluatedCandidate> {
    let fastest_p90 = candidates
        .iter()
        .map(|entry| entry.prediction.completion.total.p90)
        .min()?;
    let deadline = fastest_p90.saturating_add(policy.acceptable_completion_slack_millis);
    candidates
        .iter()
        .filter(|entry| entry.prediction.completion.total.p90 <= deadline)
        .min_by(|left, right| economy_cmp(left, right))
}

fn economy_cmp(left: &EvaluatedCandidate, right: &EvaluatedCandidate) -> Ordering {
    left.prediction
        .accounting_class
        .economy_rank()
        .cmp(&right.prediction.accounting_class.economy_rank())
        .then_with(|| {
            right
                .prediction
                .projected_allowance_remaining_ppm
                .unwrap_or(1_000_000)
                .cmp(
                    &left
                        .prediction
                        .projected_allowance_remaining_ppm
                        .unwrap_or(1_000_000),
                )
        })
        .then_with(|| {
            left.prediction
                .marginal_cost_microusd
                .p50
                .cmp(&right.prediction.marginal_cost_microusd.p50)
        })
        .then_with(|| pressure_cmp(left, right))
        .then_with(|| contention_cmp(left, right))
        .then_with(|| {
            left.prediction
                .failure_permille
                .cmp(&right.prediction.failure_permille)
        })
        .then_with(|| {
            left.prediction
                .completion
                .total
                .p50
                .cmp(&right.prediction.completion.total.p50)
        })
        .then_with(|| {
            right
                .prediction
                .hot_state
                .class
                .preference_rank()
                .cmp(&left.prediction.hot_state.class.preference_rank())
        })
        .then_with(|| left.prediction.pool_id.cmp(&right.prediction.pool_id))
}

fn latency_cmp(left: &EvaluatedCandidate, right: &EvaluatedCandidate) -> Ordering {
    left.prediction
        .completion
        .total
        .p50
        .cmp(&right.prediction.completion.total.p50)
        .then_with(|| {
            left.prediction
                .completion
                .total
                .p90
                .cmp(&right.prediction.completion.total.p90)
        })
        .then_with(|| {
            left.prediction
                .failure_permille
                .cmp(&right.prediction.failure_permille)
        })
        .then_with(|| contention_cmp(left, right))
        .then_with(|| {
            left.prediction
                .marginal_cost_microusd
                .p50
                .cmp(&right.prediction.marginal_cost_microusd.p50)
        })
        .then_with(|| left.prediction.pool_id.cmp(&right.prediction.pool_id))
}

fn pressure_cmp(left: &EvaluatedCandidate, right: &EvaluatedCandidate) -> Ordering {
    left.prediction
        .pressure_after_admission
        .policy_rank()
        .cmp(&right.prediction.pressure_after_admission.policy_rank())
}

fn contention_cmp(left: &EvaluatedCandidate, right: &EvaluatedCandidate) -> Ordering {
    match (&left.prediction.contention, &right.prediction.contention) {
        (Some(left), Some(right)) => {
            let left_rate =
                u128::from(left.validated_completions) * u128::from(right.elapsed_millis);
            let right_rate =
                u128::from(right.validated_completions) * u128::from(left.elapsed_millis);
            right_rate
                .cmp(&left_rate)
                .then_with(|| {
                    left.final_result_p90_millis
                        .cmp(&right.final_result_p90_millis)
                })
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn quantiles(values: impl Iterator<Item = u64>) -> QuantilesU64 {
    let mut values: Vec<u64> = values.collect();
    values.sort_unstable();
    let p50_index = values.len().div_ceil(2).saturating_sub(1);
    let p90_index = (values.len() * 9).div_ceil(10).saturating_sub(1);
    QuantilesU64 {
        p50: values[p50_index],
        p90: values[p90_index],
    }
}

fn sum_components(values: [u64; 5]) -> u64 {
    values.into_iter().fold(0_u64, u64::saturating_add)
}

fn rate_permille(numerator: usize, denominator: usize) -> u16 {
    let value = numerator.saturating_mul(1_000) / denominator.max(1);
    u16::try_from(value).unwrap_or(1_000)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoutingError {
    pub field: &'static str,
    pub code: &'static str,
    pub message: &'static str,
}

impl fmt::Display for RoutingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl std::error::Error for RoutingError {}

const fn error(field: &'static str, code: &'static str, message: &'static str) -> RoutingError {
    RoutingError {
        field,
        code,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000_000;

    fn id(value: &str) -> RoutingId {
        RoutingId::parse(value).unwrap()
    }

    fn workload() -> WorkloadClassV1 {
        WorkloadClassV1::new(
            id("project:cmux"),
            id("macos-compile-admission"),
            ComputeTrustClass::Trusted,
            id("ordinary-pr"),
            id("swift-app-edit"),
            id("compile-medium"),
            vec![id("os:macos"), id("arch:arm64")],
        )
        .unwrap()
    }

    fn pool(
        pool_id: &str,
        accounting_class: PoolAccountingClass,
        heat: HotStateClass,
    ) -> PoolCandidateV1 {
        PoolCandidateV1 {
            pool: ExecutionPoolV1::new(
                id(pool_id),
                id("macos-arm64"),
                accounting_class,
                vec![id("arch:arm64"), id("os:macos")],
            )
            .unwrap(),
            eligibility: PoolEligibility::Eligible,
            hot_state: HotStateEvidenceV1 {
                class: heat,
                state_identity: (heat == HotStateClass::HotExact)
                    .then(|| id("state:main-xcode27")),
                source: LocalityEvidenceSource::LocalAccepted,
            },
            pressure_after_admission: HostPressureClass::Low,
            allowance: None,
            contention: None,
        }
    }

    fn observation(
        workload: &WorkloadClassV1,
        pool_id: &str,
        heat: HotStateClass,
        age_millis: u64,
        total_execution_millis: u64,
        cost: u64,
        allowance_units: u64,
        outcome: ObservationOutcome,
    ) -> RoutingObservationV1 {
        RoutingObservationV1 {
            workload: workload.clone(),
            pool_id: id(pool_id),
            observed_at_millis: NOW - age_millis,
            hot_state: heat,
            timing: ObservationTimingV1 {
                queue_millis: 5_000,
                start_millis: 5_000,
                preparation_millis: 10_000,
                execution_millis: total_execution_millis,
                settlement_millis: 5_000,
            },
            outcome,
            resource: ResourceEnvelopeV1 {
                cpu_millicores: 4_000,
                memory_bytes: 8_u64 << 30,
                disk_bytes: 2_u64 << 30,
            },
            marginal_cost_microusd: cost,
            allowance_units,
        }
    }

    fn three_successes(
        workload: &WorkloadClassV1,
        pool_id: &str,
        heat: HotStateClass,
        execution_millis: u64,
        cost: u64,
        allowance_units: u64,
    ) -> Vec<RoutingObservationV1> {
        [1_000, 2_000, 3_000]
            .into_iter()
            .map(|age| {
                observation(
                    workload,
                    pool_id,
                    heat,
                    age,
                    execution_millis,
                    cost,
                    allowance_units,
                    ObservationOutcome::ValidatedSuccess,
                )
            })
            .collect()
    }

    #[test]
    fn missing_stale_and_insufficient_evidence_are_explicit() {
        let workload = workload();
        let candidates = vec![
            pool(
                "owned",
                PoolAccountingClass::Owned,
                HotStateClass::HotExact,
            ),
            pool(
                "stale",
                PoolAccountingClass::Owned,
                HotStateClass::HotExact,
            ),
            pool(
                "thin",
                PoolAccountingClass::Owned,
                HotStateClass::HotExact,
            ),
        ];
        let mut observations = three_successes(
            &workload,
            "stale",
            HotStateClass::HotExact,
            60_000,
            0,
            0,
        );
        for observation in &mut observations {
            observation.observed_at_millis = NOW - 40 * 24 * 60 * 60 * 1_000;
        }
        observations.push(observation(
            &workload,
            "thin",
            HotStateClass::HotExact,
            1_000,
            60_000,
            0,
            0,
            ObservationOutcome::ValidatedSuccess,
        ));

        let report = recommend_ci_pool(
            &workload,
            &candidates,
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::economy(60_000),
        )
        .unwrap();

        assert_eq!(report.status, RecommendationStatus::Abstained);
        assert!(report.exclusions.iter().any(|entry| {
            entry.pool_id == id("owned") && entry.reason == PoolExclusionReason::MissingEvidence
        }));
        assert!(report.exclusions.iter().any(|entry| {
            entry.pool_id == id("stale") && entry.reason == PoolExclusionReason::StaleEvidence
        }));
        assert!(report.exclusions.iter().any(|entry| {
            entry.pool_id == id("thin")
                && entry.reason == PoolExclusionReason::InsufficientEvidence
        }));
    }

    #[test]
    fn economy_prefers_owned_when_delay_is_inside_explicit_slack() {
        let workload = workload();
        let candidates = vec![
            pool(
                "owned",
                PoolAccountingClass::Owned,
                HotStateClass::HotExact,
            ),
            pool(
                "hosted",
                PoolAccountingClass::PaidBurst,
                HotStateClass::Cold,
            ),
        ];
        let mut observations = three_successes(
            &workload,
            "owned",
            HotStateClass::HotExact,
            75_000,
            0,
            0,
        );
        observations.extend(three_successes(
            &workload,
            "hosted",
            HotStateClass::Cold,
            45_000,
            45_000,
            0,
        ));

        let report = recommend_ci_pool(
            &workload,
            &candidates,
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::economy(60_000),
        )
        .unwrap();

        assert_eq!(report.choice, Some(id("owned")));
        assert!(!report.automatic_routing_eligible);
    }

    #[test]
    fn balanced_spends_when_finish_time_gain_is_material() {
        let workload = workload();
        let candidates = vec![
            pool("owned", PoolAccountingClass::Owned, HotStateClass::Cold),
            pool(
                "burst",
                PoolAccountingClass::PaidBurst,
                HotStateClass::Cold,
            ),
        ];
        let mut observations =
            three_successes(&workload, "owned", HotStateClass::Cold, 180_000, 0, 0);
        observations.extend(three_successes(
            &workload,
            "burst",
            HotStateClass::Cold,
            60_000,
            90_000,
            0,
        ));

        let report = recommend_ci_pool(
            &workload,
            &candidates,
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::balanced(180_000, 60_000),
        )
        .unwrap();

        assert_eq!(report.choice, Some(id("burst")));
    }

    #[test]
    fn latency_policy_can_prove_hosted_is_the_best_choice() {
        let workload = workload();
        let candidates = vec![
            pool("owned", PoolAccountingClass::Owned, HotStateClass::Warm),
            pool(
                "hosted",
                PoolAccountingClass::PaidBurst,
                HotStateClass::Cold,
            ),
        ];
        let mut observations =
            three_successes(&workload, "owned", HotStateClass::Warm, 110_000, 0, 0);
        observations.extend(three_successes(
            &workload,
            "hosted",
            HotStateClass::Cold,
            35_000,
            70_000,
            0,
        ));

        let report = recommend_ci_pool(
            &workload,
            &candidates,
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::latency(100_000),
        )
        .unwrap();

        assert_eq!(report.choice, Some(id("hosted")));
    }

    #[test]
    fn exact_hot_locality_can_beat_faster_nominal_cold_pool() {
        let workload = workload();
        let candidates = vec![
            pool("hot", PoolAccountingClass::Owned, HotStateClass::HotExact),
            pool("cold", PoolAccountingClass::Owned, HotStateClass::Cold),
        ];
        let mut observations =
            three_successes(&workload, "hot", HotStateClass::HotExact, 40_000, 0, 0);
        observations.extend(three_successes(
            &workload,
            "cold",
            HotStateClass::Cold,
            85_000,
            0,
            0,
        ));

        let report = recommend_ci_pool(
            &workload,
            &candidates,
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::latency(0),
        )
        .unwrap();

        assert_eq!(report.choice, Some(id("hot")));
    }

    #[test]
    fn allowance_reserve_keeps_scarce_included_capacity_for_later() {
        let workload = workload();
        let owned = pool("owned", PoolAccountingClass::Owned, HotStateClass::Cold);
        let mut included = pool(
            "included",
            PoolAccountingClass::MeteredIncluded,
            HotStateClass::Cold,
        );
        included.allowance = Some(AllowanceBudgetV1 {
            period_id: id("2026-09"),
            included_budget_units: 1_000,
            remaining_estimate_units: 90,
            reset_at_millis: NOW + 10_000,
            observed_consumption_units: 910,
        });
        let candidates = vec![owned, included];
        let mut observations =
            three_successes(&workload, "owned", HotStateClass::Cold, 90_000, 0, 0);
        observations.extend(three_successes(
            &workload,
            "included",
            HotStateClass::Cold,
            45_000,
            0,
            50,
        ));

        let report = recommend_ci_pool(
            &workload,
            &candidates,
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::economy(120_000),
        )
        .unwrap();

        assert_eq!(report.choice, Some(id("owned")));
        assert!(report.exclusions.iter().any(|entry| {
            entry.pool_id == id("included")
                && entry.reason == PoolExclusionReason::AllowanceReserve
        }));
    }

    #[test]
    fn contention_evidence_rejects_semantic_mismatch_and_critical_pressure() {
        let workload = workload();
        let mut bad = pool(
            "bad",
            PoolAccountingClass::Owned,
            HotStateClass::HotExact,
        );
        bad.contention = Some(ContentionEvidenceV1 {
            window_count: 4,
            offered_tasks: 16,
            validated_completions: 14,
            elapsed_millis: 60_000,
            final_result_p50_millis: 20_000,
            final_result_p90_millis: 50_000,
            semantic_mismatches: 1,
            failures: 0,
            fallbacks: 0,
            unfinished: 1,
            peak_pressure: HostPressureClass::Critical,
        });
        let observations =
            three_successes(&workload, "bad", HotStateClass::HotExact, 40_000, 0, 0);

        let report = recommend_ci_pool(
            &workload,
            &[bad],
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::economy(60_000),
        )
        .unwrap();

        assert_eq!(report.status, RecommendationStatus::Abstained);
        assert!(
            report
                .exclusions
                .iter()
                .any(|entry| { entry.reason == PoolExclusionReason::SemanticMismatch })
        );
    }

    #[test]
    fn remote_heat_is_advisory_and_never_changes_eligibility() {
        let workload = workload();
        let mut remote = pool(
            "remote",
            PoolAccountingClass::Owned,
            HotStateClass::HotExact,
        );
        remote.eligibility = PoolEligibility::Unknown;
        remote.hot_state.source = LocalityEvidenceSource::RemoteAdvisory;
        let observations =
            three_successes(&workload, "remote", HotStateClass::HotExact, 30_000, 0, 0);

        let report = recommend_ci_pool(
            &workload,
            &[remote],
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::latency(0),
        )
        .unwrap();

        assert_eq!(report.status, RecommendationStatus::Abstained);
        assert!(
            report
                .exclusions
                .iter()
                .any(|entry| { entry.reason == PoolExclusionReason::EligibilityUnknown })
        );
    }

    #[test]
    fn owned_native_linux_pilot_is_generic_and_recommendable() {
        let workload = WorkloadClassV1::new(
            id("project:glaeda"),
            id("required-verify"),
            ComputeTrustClass::Trusted,
            id("ordinary-change"),
            id("rust-workspace"),
            id("verify-medium"),
            vec![id("arch:x86_64"), id("os:linux")],
        )
        .unwrap();
        let candidate = PoolCandidateV1 {
            pool: ExecutionPoolV1::new(
                id("owned-native-linux"),
                id("native-linux-x86_64"),
                PoolAccountingClass::Owned,
                vec![id("arch:x86_64"), id("os:linux")],
            )
            .unwrap(),
            eligibility: PoolEligibility::Eligible,
            hot_state: HotStateEvidenceV1 {
                class: HotStateClass::Warm,
                state_identity: Some(id("state:rust-main")),
                source: LocalityEvidenceSource::LocalAccepted,
            },
            pressure_after_admission: HostPressureClass::Moderate,
            allowance: None,
            contention: Some(ContentionEvidenceV1 {
                window_count: 4,
                offered_tasks: 16,
                validated_completions: 16,
                elapsed_millis: 120_000,
                final_result_p50_millis: 28_000,
                final_result_p90_millis: 41_000,
                semantic_mismatches: 0,
                failures: 0,
                fallbacks: 0,
                unfinished: 0,
                peak_pressure: HostPressureClass::Moderate,
            }),
        };
        let observations =
            three_successes(&workload, "owned-native-linux", HotStateClass::Warm, 55_000, 0, 0);

        let report = recommend_ci_pool(
            &workload,
            &[candidate],
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::economy(60_000),
        )
        .unwrap();

        assert_eq!(report.choice, Some(id("owned-native-linux")));
        assert_eq!(
            report.predictions[0].execution_class,
            id("native-linux-x86_64")
        );
    }

    #[test]
    fn human_and_json_explain_the_same_recommendation() {
        let workload = workload();
        let candidate = pool(
            "owned",
            PoolAccountingClass::Owned,
            HotStateClass::HotExact,
        );
        let observations =
            three_successes(&workload, "owned", HotStateClass::HotExact, 60_000, 0, 0);
        let report = recommend_ci_pool(
            &workload,
            &[candidate],
            &observations,
            NOW,
            PredictionConfigV1::default(),
            RoutingPolicyV1::economy(60_000),
        )
        .unwrap();

        let human = report.render_human();
        let json = report.render_json().unwrap();
        assert!(human.contains("choice: owned"));
        assert!(human.contains("automatic routing: disabled"));
        assert!(json.contains("\"choice\": \"owned\""));
        assert!(json.contains("\"automatic_routing_eligible\": false"));
        assert!(json.contains("\"authority\": \"recommendation_only\""));
    }
}
