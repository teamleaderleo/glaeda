mod inner {
    include!("renderprove_native_probe_body.rs");
    include!("renderprove_native_probe_read_only.rs");
}

pub use inner::{
    RENDERPROVE_NATIVE_PROBE_SCHEMA_VERSION, RENDERPROVE_PROTECTED_MOUNT_ROOT,
    RenderproveNativeProbeCommand, RenderproveNativeProbeContext, RenderproveNativeProbeError,
    RenderproveNativeProbeOperation, RenderproveNativeProbePlan, RenderproveNativeProbeRunPlan,
    RenderproveProtectedMountReceipt, RenderproveWorkerImageObservation,
    parse_renderprove_worker_image_observation, plan_renderprove_native_probe,
};

/// Plan the immutable worker runs with a read-only project tree and one writable evidence mount.
///
/// # Errors
///
/// Returns an error if either descriptor-bound Podman launch plan cannot be constructed.
pub fn plan_renderprove_native_probe_runs(
    plan: &RenderproveNativeProbePlan,
    worker_image: RenderproveWorkerImageObservation,
) -> Result<RenderproveNativeProbeRunPlan, RenderproveNativeProbeError> {
    inner::plan_renderprove_native_probe_runs_read_only(plan, worker_image)
}
