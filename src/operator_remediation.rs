use std::collections::BTreeSet;
use std::fmt;

use serde::Serialize;

use crate::journal::RollbackClass;
use crate::operator_error::OperatorErrorCode;

pub const OPERATOR_REMEDIATION_SCHEMA_VERSION: u8 = 1;
pub const MAX_REMEDIATION_ACTION_ID_BYTES: usize = 80;
pub const MAX_REMEDIATION_SUMMARY_BYTES: usize = 160;
pub const MAX_REMEDIATION_EVIDENCE_ITEMS: usize = 16;
pub const MAX_REMEDIATION_EVIDENCE_BYTES: usize = 128;
pub const MAX_REMEDIATION_BUDGET_UNITS: u16 = 1_000;

/// How strongly accepted evidence supports the proposed response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemediationConfidence {
    Exact,
    Conditional,
    Insufficient,
}

/// Operational consequence if the proposed response executes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemediationSafety {
    ReadOnly,
    Reversible,
    Compensating,
    Irreversible,
}

/// Maximum posture a later policy layer may take toward the candidate.
///
/// This value never grants authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemediationApplicability {
    AdvisoryOnly,
    PlanOnly,
    PolicyEligible,
}

/// Ownership evidence required before a mutating response may execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemediationOwnership {
    NotApplicable,
    ExactManaged,
}

/// Pure public proposal for one possible response to an operator-visible failure.
///
/// The proposal contains no command vector, credential, executor handle, or mutation authority.
/// Its fields remain externally immutable so construction-time validation stays true for the
/// lifetime of the value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct OperatorRemediationCandidate {
    schema_version: u8,
    source_error: OperatorErrorCode,
    action_id: String,
    summary: String,
    confidence: RemediationConfidence,
    safety: RemediationSafety,
    applicability: RemediationApplicability,
    ownership: RemediationOwnership,
    #[serde(skip_serializing_if = "Option::is_none")]
    rollback: Option<RollbackClass>,
    repair_budget_units: u16,
    checkpoint_required: bool,
    fresh_verification_required: bool,
    circuit_breaker_required: bool,
    required_evidence: Vec<String>,
    authorizes_mutation: bool,
}

impl OperatorRemediationCandidate {
    /// Build and validate one non-authorizing remediation candidate.
    ///
    /// # Errors
    ///
    /// Returns an error when the candidate exceeds bounded public fields or requests an
    /// applicability inconsistent with its confidence, safety, or required safeguards.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source_error: OperatorErrorCode,
        action_id: impl Into<String>,
        summary: impl Into<String>,
        confidence: RemediationConfidence,
        safety: RemediationSafety,
        applicability: RemediationApplicability,
        repair_budget_units: u16,
        checkpoint_required: bool,
        fresh_verification_required: bool,
        circuit_breaker_required: bool,
        required_evidence: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, OperatorRemediationError> {
        let ownership = match safety {
            RemediationSafety::ReadOnly => RemediationOwnership::NotApplicable,
            _ => RemediationOwnership::ExactManaged,
        };
        let rollback = match safety {
            RemediationSafety::ReadOnly => None,
            RemediationSafety::Reversible => Some(RollbackClass::Reversible),
            RemediationSafety::Compensating => Some(RollbackClass::Compensating),
            RemediationSafety::Irreversible => Some(RollbackClass::Irreversible),
        };
        let candidate = Self {
            schema_version: OPERATOR_REMEDIATION_SCHEMA_VERSION,
            source_error,
            action_id: action_id.into(),
            summary: summary.into(),
            confidence,
            safety,
            applicability,
            ownership,
            rollback,
            repair_budget_units,
            checkpoint_required,
            fresh_verification_required,
            circuit_breaker_required,
            required_evidence: required_evidence.into_iter().map(Into::into).collect(),
            authorizes_mutation: false,
        };
        candidate.validate()?;
        Ok(candidate)
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn source_error(&self) -> &OperatorErrorCode {
        &self.source_error
    }

    #[must_use]
    pub fn action_id(&self) -> &str {
        &self.action_id
    }

    #[must_use]
    pub fn summary(&self) -> &str {
        &self.summary
    }

    #[must_use]
    pub const fn confidence(&self) -> RemediationConfidence {
        self.confidence
    }

    #[must_use]
    pub const fn safety(&self) -> RemediationSafety {
        self.safety
    }

    #[must_use]
    pub const fn applicability(&self) -> RemediationApplicability {
        self.applicability
    }

    #[must_use]
    pub const fn ownership(&self) -> RemediationOwnership {
        self.ownership
    }

    #[must_use]
    pub fn rollback(&self) -> Option<&RollbackClass> {
        self.rollback.as_ref()
    }

    #[must_use]
    pub const fn repair_budget_units(&self) -> u16 {
        self.repair_budget_units
    }

    #[must_use]
    pub const fn checkpoint_required(&self) -> bool {
        self.checkpoint_required
    }

    #[must_use]
    pub const fn fresh_verification_required(&self) -> bool {
        self.fresh_verification_required
    }

    #[must_use]
    pub const fn circuit_breaker_required(&self) -> bool {
        self.circuit_breaker_required
    }

    #[must_use]
    pub fn required_evidence(&self) -> &[String] {
        &self.required_evidence
    }

    #[must_use]
    pub const fn authorizes_mutation(&self) -> bool {
        self.authorizes_mutation
    }

    fn validate(&self) -> Result<(), OperatorRemediationError> {
        validate_text(
            &self.action_id,
            MAX_REMEDIATION_ACTION_ID_BYTES,
            "remediation action ID",
        )?;
        validate_text(
            &self.summary,
            MAX_REMEDIATION_SUMMARY_BYTES,
            "remediation summary",
        )?;
        if self.repair_budget_units > MAX_REMEDIATION_BUDGET_UNITS {
            return Err(error(
                "remediation repair budget exceeds the accepted maximum",
            ));
        }
        if self.required_evidence.len() > MAX_REMEDIATION_EVIDENCE_ITEMS {
            return Err(error(
                "remediation evidence exceeds the accepted item limit",
            ));
        }
        let mut evidence = BTreeSet::new();
        for item in &self.required_evidence {
            validate_text(item, MAX_REMEDIATION_EVIDENCE_BYTES, "remediation evidence")?;
            if !evidence.insert(item) {
                return Err(error("remediation evidence contains a duplicate item"));
            }
        }
        if self.authorizes_mutation {
            return Err(error("a remediation candidate never authorizes mutation"));
        }

        match self.safety {
            RemediationSafety::ReadOnly => self.validate_read_only()?,
            RemediationSafety::Reversible => {
                self.validate_mutating(RollbackClass::Reversible)?;
            }
            RemediationSafety::Compensating => {
                self.validate_mutating(RollbackClass::Compensating)?;
            }
            RemediationSafety::Irreversible => {
                self.validate_mutating(RollbackClass::Irreversible)?;
                if self.applicability == RemediationApplicability::PolicyEligible {
                    return Err(error("irreversible remediation cannot be policy-eligible"));
                }
            }
        }

        if self.applicability == RemediationApplicability::PolicyEligible {
            if self.confidence != RemediationConfidence::Exact {
                return Err(error(
                    "policy-eligible remediation requires exact diagnostic confidence",
                ));
            }
            if self.required_evidence.is_empty() {
                return Err(error(
                    "policy-eligible remediation requires explicit evidence preconditions",
                ));
            }
            if self.safety != RemediationSafety::ReadOnly && self.repair_budget_units == 0 {
                return Err(error(
                    "policy-eligible mutating remediation requires positive repair budget",
                ));
            }
        }
        if self.confidence == RemediationConfidence::Insufficient
            && self.applicability != RemediationApplicability::AdvisoryOnly
        {
            return Err(error(
                "insufficient diagnostic confidence permits advisory remediation only",
            ));
        }
        Ok(())
    }

    fn validate_read_only(&self) -> Result<(), OperatorRemediationError> {
        if self.rollback.is_some()
            || self.repair_budget_units != 0
            || self.ownership != RemediationOwnership::NotApplicable
        {
            return Err(error("read-only remediation carries mutation metadata"));
        }
        Ok(())
    }

    fn validate_mutating(
        &self,
        expected_rollback: RollbackClass,
    ) -> Result<(), OperatorRemediationError> {
        if self.ownership != RemediationOwnership::ExactManaged {
            return Err(error(
                "mutating remediation requires exact managed ownership",
            ));
        }
        if self.rollback != Some(expected_rollback) {
            return Err(error("mutating remediation rollback class is inconsistent"));
        }
        if !self.checkpoint_required {
            return Err(error("mutating remediation requires a durable checkpoint"));
        }
        if !self.fresh_verification_required {
            return Err(error(
                "mutating remediation requires fresh post-action verification",
            ));
        }
        if !self.circuit_breaker_required {
            return Err(error(
                "mutating remediation requires circuit-breaker participation",
            ));
        }
        Ok(())
    }
}

fn validate_text(value: &str, maximum: usize, label: &str) -> Result<(), OperatorRemediationError> {
    if value.is_empty()
        || value.len() > maximum
        || value.contains('\n')
        || value.contains('\r')
        || value.contains('\0')
    {
        return Err(error(format!("{label} is invalid")));
    }
    Ok(())
}

fn error(message: impl Into<String>) -> OperatorRemediationError {
    OperatorRemediationError {
        message: message.into(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorRemediationError {
    message: String,
}

impl fmt::Display for OperatorRemediationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for OperatorRemediationError {}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        OperatorRemediationCandidate, RemediationApplicability, RemediationConfidence,
        RemediationOwnership, RemediationSafety,
    };
    use crate::journal::RollbackClass;
    use crate::operator_error::OperatorErrorCode;

    #[test]
    fn exact_reversible_candidate_is_policy_eligible_but_non_authorizing() {
        let candidate = OperatorRemediationCandidate::new(
            OperatorErrorCode::CleanupFailed,
            "remove-owned-expired-worker",
            "Remove one exactly owned expired disposable worker.",
            RemediationConfidence::Exact,
            RemediationSafety::Reversible,
            RemediationApplicability::PolicyEligible,
            1,
            true,
            true,
            true,
            ["exact_ownership", "expired_lease", "no_active_job"],
        )
        .expect("valid candidate");

        assert_eq!(candidate.ownership(), RemediationOwnership::ExactManaged);
        assert_eq!(candidate.rollback(), Some(&RollbackClass::Reversible));
        assert!(!candidate.authorizes_mutation());
        assert_eq!(candidate.schema_version(), OPERATOR_REMEDIATION_SCHEMA_VERSION);
        assert_eq!(candidate.source_error(), &OperatorErrorCode::CleanupFailed);
        assert_eq!(candidate.action_id(), "remove-owned-expired-worker");
        assert_eq!(candidate.confidence(), RemediationConfidence::Exact);
        assert_eq!(candidate.safety(), RemediationSafety::Reversible);
        assert_eq!(
            candidate.applicability(),
            RemediationApplicability::PolicyEligible
        );
        assert_eq!(candidate.repair_budget_units(), 1);
        assert!(candidate.checkpoint_required());
        assert!(candidate.fresh_verification_required());
        assert!(candidate.circuit_breaker_required());
        assert_eq!(
            candidate.required_evidence(),
            ["exact_ownership", "expired_lease", "no_active_job"]
        );
        assert_eq!(
            serde_json::to_value(&candidate).expect("serialize candidate"),
            json!({
                "schema_version": 1,
                "source_error": "cleanup_failed",
                "action_id": "remove-owned-expired-worker",
                "summary": "Remove one exactly owned expired disposable worker.",
                "confidence": "exact",
                "safety": "reversible",
                "applicability": "policy_eligible",
                "ownership": "exact_managed",
                "rollback": "reversible",
                "repair_budget_units": 1,
                "checkpoint_required": true,
                "fresh_verification_required": true,
                "circuit_breaker_required": true,
                "required_evidence": ["exact_ownership", "expired_lease", "no_active_job"],
                "authorizes_mutation": false
            })
        );
    }

    #[test]
    fn read_only_candidate_needs_no_mutation_machinery() {
        let candidate = OperatorRemediationCandidate::new(
            OperatorErrorCode::DurableStateRevisionStale,
            "refresh-status",
            "Refresh operator status from current durable state.",
            RemediationConfidence::Exact,
            RemediationSafety::ReadOnly,
            RemediationApplicability::PolicyEligible,
            0,
            false,
            false,
            false,
            ["current_durable_revision"],
        )
        .expect("valid read-only candidate");

        assert_eq!(candidate.ownership(), RemediationOwnership::NotApplicable);
        assert_eq!(candidate.rollback(), None);
        assert!(!candidate.authorizes_mutation());
    }

    #[test]
    fn conditional_and_irreversible_candidates_stay_out_of_automatic_policy() {
        let conditional = OperatorRemediationCandidate::new(
            OperatorErrorCode::LimaBroken,
            "restart-lima",
            "Restart the exactly managed Lima instance.",
            RemediationConfidence::Conditional,
            RemediationSafety::Reversible,
            RemediationApplicability::PolicyEligible,
            1,
            true,
            true,
            true,
            ["exact_ownership", "no_active_job"],
        )
        .expect_err("conditional diagnosis must not be automatic");
        assert_eq!(
            conditional.to_string(),
            "policy-eligible remediation requires exact diagnostic confidence"
        );

        let irreversible = OperatorRemediationCandidate::new(
            OperatorErrorCode::IrreversibleMigrationApprovalRequired,
            "migrate-state",
            "Apply an irreversible durable-state migration.",
            RemediationConfidence::Exact,
            RemediationSafety::Irreversible,
            RemediationApplicability::PolicyEligible,
            1,
            true,
            true,
            true,
            ["exact_ownership", "migration_preflight"],
        )
        .expect_err("irreversible action must stay outside automatic policy");
        assert_eq!(
            irreversible.to_string(),
            "irreversible remediation cannot be policy-eligible"
        );
    }

    #[test]
    fn mutating_candidate_requires_checkpoint_verification_and_circuit_breaker() {
        for (checkpoint, verification, circuit_breaker, expected) in [
            (
                false,
                true,
                true,
                "mutating remediation requires a durable checkpoint",
            ),
            (
                true,
                false,
                true,
                "mutating remediation requires fresh post-action verification",
            ),
            (
                true,
                true,
                false,
                "mutating remediation requires circuit-breaker participation",
            ),
        ] {
            let error = OperatorRemediationCandidate::new(
                OperatorErrorCode::CleanupFailed,
                "repair-cleanup",
                "Repair one exactly owned cleanup failure.",
                RemediationConfidence::Exact,
                RemediationSafety::Compensating,
                RemediationApplicability::PlanOnly,
                1,
                checkpoint,
                verification,
                circuit_breaker,
                ["exact_ownership"],
            )
            .expect_err("mutating contract must be complete");
            assert_eq!(error.to_string(), expected);
        }
    }

    #[test]
    fn insufficient_confidence_is_advisory_only() {
        let error = OperatorRemediationCandidate::new(
            OperatorErrorCode::TerminalClassificationInconclusive,
            "inspect-terminal-result",
            "Inspect the bounded terminal evidence before choosing a response.",
            RemediationConfidence::Insufficient,
            RemediationSafety::ReadOnly,
            RemediationApplicability::PlanOnly,
            0,
            false,
            false,
            false,
            ["terminal_receipt"],
        )
        .expect_err("insufficient confidence cannot become a plan");
        assert_eq!(
            error.to_string(),
            "insufficient diagnostic confidence permits advisory remediation only"
        );
    }
}
