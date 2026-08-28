//! Pure workload-neutral pre-admission compute request intent.
//!
//! A request binds one already-validated public request identity, one exact semantic workload, and
//! one already-validated resource-demand vector. It grants no capacity ownership, reservation,
//! placement, scheduler, backend, runner, process, persistence, retry, supersession, publication,
//! or lifecycle authority. Family adapters remain responsible for deriving workload semantics and
//! resource intent from their own evidence.

use serde::Serialize;

use crate::compute_workload::ComputeWorkloadIdentity;
use crate::execution_admission::ExecutionRequestId;
use crate::execution_capacity::CapacityAmounts;

pub const COMPUTE_EXECUTION_REQUEST_SCHEMA_VERSION: u8 = 1;

/// Exact pure intent presented before any capacity claim or execution admission exists.
///
/// `ExecutionRequestId`, `ComputeWorkloadIdentity`, and `CapacityAmounts` are validated by their
/// owning modules before this carrier can be constructed. In particular, an empty or duplicate
/// resource-dimension set cannot inhabit `CapacityAmounts`, and path/command-shaped generic
/// workload identifiers cannot inhabit `ComputeWorkloadIdentity`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ComputeExecutionRequest {
    schema_version: u8,
    request_id: ExecutionRequestId,
    workload: ComputeWorkloadIdentity,
    requested_resources: CapacityAmounts,
}

impl ComputeExecutionRequest {
    #[must_use]
    pub const fn new(
        request_id: ExecutionRequestId,
        workload: ComputeWorkloadIdentity,
        requested_resources: CapacityAmounts,
    ) -> Self {
        Self {
            schema_version: COMPUTE_EXECUTION_REQUEST_SCHEMA_VERSION,
            request_id,
            workload,
            requested_resources,
        }
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn request_id(&self) -> &ExecutionRequestId {
        &self.request_id
    }

    #[must_use]
    pub const fn workload(&self) -> &ComputeWorkloadIdentity {
        &self.workload
    }

    #[must_use]
    pub const fn requested_resources(&self) -> &CapacityAmounts {
        &self.requested_resources
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::Sha256Digest;
    use crate::compute_workload::{
        ComputeCapabilityId, ComputeCapabilitySet, ComputeInputIdentity,
        ComputeOutputContractIdentity, ComputeSemanticGeneration, ComputeTrustClass,
        ComputeWorkloadFamilyId,
    };
    use crate::execution_admission::{ExecutionAdmissionIdentity, RunnerProfileId};
    use crate::execution_capacity::CapacityDimension;
    use crate::verification_profile::VerificationProfileId;

    fn digest(hex: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", hex.to_string().repeat(64))).unwrap()
    }

    fn workload(family: &str, input_hex: char) -> ComputeWorkloadIdentity {
        ComputeWorkloadIdentity::new(
            ComputeWorkloadFamilyId::parse(family).unwrap(),
            ComputeSemanticGeneration::new(1).unwrap(),
            ComputeInputIdentity::new(digest(input_hex)),
            ComputeTrustClass::Trusted,
            ComputeCapabilitySet::new(vec![
                ComputeCapabilityId::parse("cpu.general").unwrap(),
                ComputeCapabilityId::parse("linux.arm64").unwrap(),
            ])
            .unwrap(),
            ComputeOutputContractIdentity::new(digest('f')),
        )
    }

    fn resources(cpu_millis: u64, memory_bytes: u64) -> CapacityAmounts {
        CapacityAmounts::new(&[
            (CapacityDimension::CpuMillis, cpu_millis),
            (CapacityDimension::MemoryBytes, memory_bytes),
        ])
        .unwrap()
    }

    #[test]
    fn request_equality_binds_id_workload_and_resource_intent() {
        let base = ComputeExecutionRequest::new(
            ExecutionRequestId::parse("request1").unwrap(),
            workload("dataset_transform.v1", 'a'),
            resources(2_000, 4 * 1024 * 1024),
        );
        let same = base.clone();
        let different_workload = ComputeExecutionRequest::new(
            ExecutionRequestId::parse("request1").unwrap(),
            workload("dataset_transform.v1", 'b'),
            resources(2_000, 4 * 1024 * 1024),
        );
        let different_resources = ComputeExecutionRequest::new(
            ExecutionRequestId::parse("request1").unwrap(),
            workload("dataset_transform.v1", 'a'),
            resources(3_000, 4 * 1024 * 1024),
        );
        let different_request = ComputeExecutionRequest::new(
            ExecutionRequestId::parse("request2").unwrap(),
            workload("dataset_transform.v1", 'a'),
            resources(2_000, 4 * 1024 * 1024),
        );

        assert_eq!(base, same);
        assert_ne!(base, different_workload);
        assert_ne!(base, different_resources);
        assert_ne!(base, different_request);
    }

    #[test]
    fn typed_inputs_fail_closed_before_generic_request_construction() {
        assert!(ExecutionRequestId::parse("").is_err());
        assert!(ExecutionRequestId::parse("cargo test").is_err());
        assert!(ComputeWorkloadFamilyId::parse("/private/worker").is_err());
        assert!(ComputeCapabilityId::parse("TOKEN=secret").is_err());

        assert!(CapacityAmounts::new(&[]).is_err());
        assert!(CapacityAmounts::new(&[
            (CapacityDimension::CpuMillis, 1),
            (CapacityDimension::CpuMillis, 2),
        ])
        .is_err());
    }

    #[test]
    fn requested_resources_preserve_untracked_and_explicit_zero() {
        let requested_resources = CapacityAmounts::new(&[
            (CapacityDimension::CpuMillis, 0),
            (CapacityDimension::Pids, 8),
        ])
        .unwrap();
        let request = ComputeExecutionRequest::new(
            ExecutionRequestId::parse("request-zero").unwrap(),
            workload("dataset_transform.v1", 'a'),
            requested_resources,
        );

        assert_eq!(
            request
                .requested_resources()
                .amount(CapacityDimension::CpuMillis),
            Some(0)
        );
        assert_eq!(
            request
                .requested_resources()
                .amount(CapacityDimension::MemoryBytes),
            None
        );
        assert_eq!(
            request
                .requested_resources()
                .amount(CapacityDimension::Pids),
            Some(8)
        );
    }

    #[test]
    fn public_json_and_debug_carry_no_execution_authority_surface() {
        let request = ComputeExecutionRequest::new(
            ExecutionRequestId::parse("request-public").unwrap(),
            workload("dataset_transform.v1", 'a'),
            resources(2_000, 4 * 1024 * 1024),
        );
        let json = serde_json::to_string(&request).unwrap();
        let debug = format!("{request:?}").to_ascii_lowercase();

        for forbidden in [
            "/private/",
            "cargo test",
            "command",
            "environment",
            "credential",
            "token=",
            "reservation",
            "capacity_claim",
            "backend",
            "runner_profile",
            "process",
            "spawn",
            "persist",
        ] {
            assert!(!json.to_ascii_lowercase().contains(forbidden));
            assert!(!debug.contains(forbidden));
        }

        let object = serde_json::to_value(&request).unwrap();
        let object = object.as_object().unwrap();
        assert_eq!(
            object.keys().map(String::as_str).collect::<Vec<_>>(),
            vec![
                "request_id",
                "requested_resources",
                "schema_version",
                "workload"
            ]
        );
    }

    #[test]
    fn resource_intent_serialization_is_not_capacity_ownership_evidence() {
        let value = serde_json::to_value(resources(1_000, 1024)).unwrap();
        let object = value.as_object().unwrap();
        assert_eq!(
            object.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["amounts", "tracked_dimensions"]
        );
        for forbidden in [
            "claim_id",
            "domain_id",
            "domain_generation",
            "child_domain_binding",
        ] {
            assert!(!object.contains_key(forbidden));
        }
    }

    #[test]
    fn durable_execution_admission_identity_v1_remains_unchanged() {
        let identity = ExecutionAdmissionIdentity::new(
            ExecutionRequestId::parse("legacy-request").unwrap(),
            VerificationProfileId::parse("verification-profile-v1").unwrap(),
            RunnerProfileId::parse("runner-profile-v1").unwrap(),
        );
        let value = serde_json::to_value(identity).unwrap();
        let object = value.as_object().unwrap();
        assert_eq!(
            object.keys().map(String::as_str).collect::<Vec<_>>(),
            vec![
                "request_id",
                "runner_profile_id",
                "verification_profile_id"
            ]
        );
        assert!(!object.contains_key("workload"));
        assert!(!object.contains_key("requested_resources"));
    }

    #[test]
    fn attempt_and_reservation_identity_are_absent_from_pre_admission_intent() {
        let value = serde_json::to_value(ComputeExecutionRequest::new(
            ExecutionRequestId::parse("request-before-attempt").unwrap(),
            workload("dataset_transform.v1", 'a'),
            resources(1_000, 1024),
        ))
        .unwrap();
        let object = value.as_object().unwrap();
        for absent in ["attempt", "attempt_generation", "reservation", "claim"] {
            assert!(!object.contains_key(absent));
        }
    }
}
