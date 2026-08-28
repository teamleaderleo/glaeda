//! Pure frontier-inference adapter into generic pre-admission compute request intent.
//!
//! Host-tool phases are sequential model inputs. This family therefore derives one requested host
//! envelope as the per-dimension peak across phases with one exact tracked-dimension set; it never
//! asks the generic compute layer to sum sequential phases or infer accelerator placement.

use std::fmt;

use serde::Serialize;

use crate::compute_execution_request::ComputeExecutionRequest;
use crate::execution_admission::ExecutionRequestId;
use crate::execution_capacity::{CapacityAmounts, CapacityDimension};
use crate::frontier_inference_workload::{
    FrontierHostToolPhase, FrontierInferenceWorkloadReceiptV1,
};

/// Derive the family-owned host resource envelope for sequential frontier host-tool phases.
///
/// # Errors
///
/// Returns an error when no host phase exists or phases disagree on which resource dimensions are
/// tracked. The adapter intentionally refuses to normalize missing dimensions into explicit zero.
pub fn frontier_inference_host_resource_envelope(
    host_tool_phases: &[FrontierHostToolPhase],
) -> Result<CapacityAmounts, FrontierInferenceComputeRequestError> {
    let Some(first) = host_tool_phases.first() else {
        return Err(FrontierInferenceComputeRequestError::new(
            "host_tool_phases",
            "empty_frontier_host_resource_envelope",
            "frontier compute request requires at least one host-tool demand phase",
        ));
    };
    let tracked_dimensions = first.host_capacity_demand().tracked_dimensions();
    if host_tool_phases
        .iter()
        .skip(1)
        .any(|phase| phase.host_capacity_demand().tracked_dimensions() != tracked_dimensions)
    {
        return Err(FrontierInferenceComputeRequestError::new(
            "host_tool_phases.host_capacity_demand.tracked_dimensions",
            "frontier_host_resource_dimension_mismatch",
            "sequential frontier host-tool phases must use one exact tracked-dimension set",
        ));
    }

    let entries = tracked_dimensions
        .as_slice()
        .iter()
        .copied()
        .map(|dimension| {
            let peak = host_tool_phases
                .iter()
                .map(|phase| {
                    phase
                        .host_capacity_demand()
                        .amount(dimension)
                        .expect("equal tracked-dimension sets must contain each amount")
                })
                .max()
                .expect("non-empty phase list has a per-dimension peak");
            (dimension, peak)
        })
        .collect::<Vec<_>>();

    Ok(CapacityAmounts::new(&entries)
        .expect("canonical non-empty tracked dimensions must rebuild as capacity amounts"))
}

/// Preserve the receipt's exact semantic workload and add only family-derived host resource intent.
///
/// # Errors
///
/// Returns the same fail-closed envelope errors as [`frontier_inference_host_resource_envelope`].
pub fn frontier_inference_compute_execution_request(
    request_id: ExecutionRequestId,
    receipt: &FrontierInferenceWorkloadReceiptV1,
) -> Result<ComputeExecutionRequest, FrontierInferenceComputeRequestError> {
    let requested_resources = frontier_inference_host_resource_envelope(receipt.host_tool_phases())?;
    Ok(ComputeExecutionRequest::new(
        request_id,
        receipt.workload().clone(),
        requested_resources,
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FrontierInferenceComputeRequestError {
    field: &'static str,
    code: &'static str,
    message: &'static str,
}

impl FrontierInferenceComputeRequestError {
    const fn new(field: &'static str, code: &'static str, message: &'static str) -> Self {
        Self {
            field,
            code,
            message,
        }
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for FrontierInferenceComputeRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl std::error::Error for FrontierInferenceComputeRequestError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontier_inference_workload::{
        frontier_inference_synthetic_sensitivity_fixtures, FrontierHostToolPhaseKind,
    };

    fn demand(entries: &[(CapacityDimension, u64)]) -> CapacityAmounts {
        CapacityAmounts::new(entries).unwrap()
    }

    fn phase(kind: FrontierHostToolPhaseKind, entries: &[(CapacityDimension, u64)]) -> FrontierHostToolPhase {
        FrontierHostToolPhase::new(kind, 1_000, demand(entries)).unwrap()
    }

    #[test]
    fn sequential_phases_use_per_dimension_peak_not_sum() {
        let phases = vec![
            phase(
                FrontierHostToolPhaseKind::RepositoryIo,
                &[
                    (CapacityDimension::CpuMillis, 1_000),
                    (CapacityDimension::MemoryBytes, 4_000),
                    (CapacityDimension::DiskBytes, 8_000),
                    (CapacityDimension::Pids, 8),
                ],
            ),
            phase(
                FrontierHostToolPhaseKind::BuildTest,
                &[
                    (CapacityDimension::CpuMillis, 3_000),
                    (CapacityDimension::MemoryBytes, 2_000),
                    (CapacityDimension::DiskBytes, 10_000),
                    (CapacityDimension::Pids, 32),
                ],
            ),
        ];

        let envelope = frontier_inference_host_resource_envelope(&phases).unwrap();
        assert_eq!(envelope.amount(CapacityDimension::CpuMillis), Some(3_000));
        assert_eq!(envelope.amount(CapacityDimension::MemoryBytes), Some(4_000));
        assert_eq!(envelope.amount(CapacityDimension::DiskBytes), Some(10_000));
        assert_eq!(envelope.amount(CapacityDimension::Pids), Some(32));
        assert_ne!(envelope.amount(CapacityDimension::CpuMillis), Some(4_000));
        assert_ne!(envelope.amount(CapacityDimension::MemoryBytes), Some(6_000));
    }

    #[test]
    fn envelope_refuses_empty_or_dimension_drifting_phase_sets() {
        assert_eq!(
            frontier_inference_host_resource_envelope(&[])
                .unwrap_err()
                .code(),
            "empty_frontier_host_resource_envelope"
        );
        let phases = vec![
            phase(
                FrontierHostToolPhaseKind::RepositoryIo,
                &[
                    (CapacityDimension::CpuMillis, 1_000),
                    (CapacityDimension::MemoryBytes, 4_000),
                ],
            ),
            phase(
                FrontierHostToolPhaseKind::BuildTest,
                &[
                    (CapacityDimension::CpuMillis, 2_000),
                    (CapacityDimension::Pids, 32),
                ],
            ),
        ];
        assert_eq!(
            frontier_inference_host_resource_envelope(&phases)
                .unwrap_err()
                .code(),
            "frontier_host_resource_dimension_mismatch"
        );
    }

    #[test]
    fn synthetic_frontier_receipt_preserves_workload_and_exact_host_envelope() {
        let fixture = frontier_inference_synthetic_sensitivity_fixtures()
            .into_iter()
            .next()
            .unwrap();
        let request = frontier_inference_compute_execution_request(
            ExecutionRequestId::parse("frontier-request1").unwrap(),
            fixture.receipt(),
        )
        .unwrap();

        assert_eq!(request.workload(), fixture.receipt().workload());
        assert_eq!(
            request
                .requested_resources()
                .amount(CapacityDimension::CpuMillis),
            Some(4_000)
        );
        assert_eq!(
            request
                .requested_resources()
                .amount(CapacityDimension::MemoryBytes),
            Some(4 * 1024 * 1024 * 1024)
        );
        assert_eq!(
            request
                .requested_resources()
                .amount(CapacityDimension::DiskBytes),
            Some(8 * 1024 * 1024 * 1024)
        );
        assert_eq!(
            request
                .requested_resources()
                .amount(CapacityDimension::Pids),
            Some(128)
        );

        let json = serde_json::to_string(&request).unwrap();
        for verification_specific in [
            "verification",
            "repository",
            "pull_request",
            "test_command",
            "runner_profile",
        ] {
            assert!(!json.contains(verification_specific));
        }
    }
}
