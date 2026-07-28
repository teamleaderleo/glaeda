fn read_only_project_volume(alias: &Path) -> String {
    format!("{}:{PROJECT_CONTAINER_PATH}:ro", alias.display())
}

pub(super) fn plan_renderprove_native_probe_runs_read_only(
    plan: &RenderproveNativeProbePlan,
    worker_image: RenderproveWorkerImageObservation,
) -> Result<RenderproveNativeProbeRunPlan, RenderproveNativeProbeError> {
    let mut identity_arguments = common_run_arguments();
    identity_arguments.extend([
        ReviewedLaunchValue::plain("--entrypoint"),
        ReviewedLaunchValue::plain("node"),
        ReviewedLaunchValue::plain("--env"),
        ReviewedLaunchValue::plain(format!(
            "RENDERPROVE_WORKER_IMAGE={}",
            plan.canonical_worker_image
        )),
        ReviewedLaunchValue::plain("--env"),
        ReviewedLaunchValue::plain(format!(
            "RENDERPROVE_WORKER_IMAGE_ID={}",
            worker_image.image_id().as_str()
        )),
        ReviewedLaunchValue::plain("--env"),
        ReviewedLaunchValue::plain(format!(
            "RENDERPROVE_WORKER_IMAGE_DIGEST={}",
            plan.request.worker_image().digest().as_str()
        )),
        ReviewedLaunchValue::plain(plan.canonical_worker_image.clone()),
        ReviewedLaunchValue::plain(WORKER_IDENTITY_SCRIPT),
    ]);
    let record_worker_identity = make_command(
        "renderprove.probe.worker.identity",
        RenderproveNativeProbeOperation::RecordWorkerIdentity,
        &plan.context,
        identity_arguments,
    )?;

    let evidence_container_path = format!(
        "{PROJECT_CONTAINER_PATH}/{}",
        plan.context.mounts.evidence_directory.display()
    );
    let mut review_arguments = common_run_arguments();
    review_arguments.extend([
        ReviewedLaunchValue::plain("--volume"),
        ReviewedLaunchValue::secret(read_only_project_volume(
            &plan.context.mounts.project_alias,
        )),
        ReviewedLaunchValue::plain("--volume"),
        ReviewedLaunchValue::secret(format!(
            "{}:{evidence_container_path}:rw",
            plan.context.mounts.evidence_alias.display()
        )),
        ReviewedLaunchValue::plain(plan.canonical_worker_image.clone()),
        ReviewedLaunchValue::plain("review"),
        ReviewedLaunchValue::plain(PROJECT_CONTAINER_PATH),
        ReviewedLaunchValue::plain("--output"),
        ReviewedLaunchValue::plain(plan.context.mounts.evidence_directory.display().to_string()),
        ReviewedLaunchValue::plain("--json"),
    ]);
    let review_project = make_command(
        "renderprove.probe.project.review",
        RenderproveNativeProbeOperation::ReviewProject,
        &plan.context,
        review_arguments,
    )?;

    Ok(RenderproveNativeProbeRunPlan {
        schema_version: RENDERPROVE_NATIVE_PROBE_SCHEMA_VERSION,
        worker_image,
        record_worker_identity,
        review_project,
        worker_identity_output: PathBuf::from("worker.json"),
        review_stdout_output: PathBuf::from("review.stdout.json"),
    })
}

#[cfg(test)]
mod read_only_mount_tests {
    use super::*;

    #[test]
    fn project_mount_is_read_only() {
        let volume = read_only_project_volume(Path::new(
            "/run/smolrunner/renderprove-mounts/project-001",
        ));
        assert_eq!(
            volume,
            "/run/smolrunner/renderprove-mounts/project-001:/workspace/project:ro"
        );
        assert!(!volume.ends_with(":rw"));
    }
}
