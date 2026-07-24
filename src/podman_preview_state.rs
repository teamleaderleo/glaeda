use std::fmt;

use serde::Serialize;

use crate::podman_preview::{PreviewContainerSpec, PreviewPodmanCommand, PreviewPodmanOperation};
use crate::podman_preview_execution::{
    PreviewInspectExecutionReceipt, authorize_existing_preview_command_from_receipt,
};
use crate::podman_preview_inspect::{AuthorizedPreviewPodmanCommand, PreviewContainerStatus};

/// Authorize one existing-container mutation only when ownership and observed Podman state match.
///
/// Start is accepted for inactive startable states. Stop is accepted only for a running container.
/// Unforced remove is accepted only for inactive removable states. Missing, paused, transitional,
/// dead, and unknown states fail closed.
///
/// # Errors
///
/// Returns an error when the receipt lacks a known state, the operation is invalid for that state,
/// or the receipt does not prove exact ownership of the planned preview generation.
pub fn authorize_existing_preview_command(
    spec: &PreviewContainerSpec,
    command: &PreviewPodmanCommand,
    receipt: &PreviewInspectExecutionReceipt,
) -> Result<AuthorizedPreviewPodmanCommand, PreviewMutationAuthorizationError> {
    let status = receipt.observation().status().ok_or_else(|| {
        PreviewMutationAuthorizationError::new(
            "state",
            "Podman did not report a container status for mutation authorization",
        )
    })?;
    if !operation_allows_status(command.operation(), status) {
        return Err(PreviewMutationAuthorizationError::new(
            "state",
            format!(
                "operation {} is not permitted for observed Podman state {}",
                operation_name(command.operation()),
                status_name(status)
            ),
        ));
    }

    authorize_existing_preview_command_from_receipt(spec, command, receipt).map_err(|error| {
        PreviewMutationAuthorizationError::new("ownership", error.to_string())
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreviewMutationAuthorizationError {
    pub field: String,
    pub problem: String,
}

impl PreviewMutationAuthorizationError {
    fn new(field: impl Into<String>, problem: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            problem: problem.into(),
        }
    }
}

impl fmt::Display for PreviewMutationAuthorizationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", self.field, self.problem)
    }
}

impl std::error::Error for PreviewMutationAuthorizationError {}

const fn operation_allows_status(
    operation: PreviewPodmanOperation,
    status: &PreviewContainerStatus,
) -> bool {
    match operation {
        PreviewPodmanOperation::Start => matches!(
            status,
            PreviewContainerStatus::Configured
                | PreviewContainerStatus::Created
                | PreviewContainerStatus::Initialized
                | PreviewContainerStatus::Stopped
                | PreviewContainerStatus::Exited
        ),
        PreviewPodmanOperation::Stop => matches!(status, PreviewContainerStatus::Running),
        PreviewPodmanOperation::Remove => matches!(
            status,
            PreviewContainerStatus::Configured
                | PreviewContainerStatus::Created
                | PreviewContainerStatus::Initialized
                | PreviewContainerStatus::Stopped
                | PreviewContainerStatus::Exited
        ),
        PreviewPodmanOperation::Create | PreviewPodmanOperation::Inspect => false,
    }
}

const fn operation_name(operation: PreviewPodmanOperation) -> &'static str {
    match operation {
        PreviewPodmanOperation::Create => "create",
        PreviewPodmanOperation::Start => "start",
        PreviewPodmanOperation::Inspect => "inspect",
        PreviewPodmanOperation::Stop => "stop",
        PreviewPodmanOperation::Remove => "remove",
    }
}

fn status_name(status: &PreviewContainerStatus) -> &str {
    match status {
        PreviewContainerStatus::Configured => "configured",
        PreviewContainerStatus::Created => "created",
        PreviewContainerStatus::Initialized => "initialized",
        PreviewContainerStatus::Running => "running",
        PreviewContainerStatus::Stopped => "stopped",
        PreviewContainerStatus::Paused => "paused",
        PreviewContainerStatus::Exited => "exited",
        PreviewContainerStatus::Removing => "removing",
        PreviewContainerStatus::Stopping => "stopping",
        PreviewContainerStatus::Dead => "dead",
        PreviewContainerStatus::Other(value) => value,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::artifact::{ArtifactIdentity, ArtifactKind, CommitId, RepositoryRef, Sha256Digest};
    use crate::lane_command::{LinuxAccountName, RunnerUserContext};
    use crate::lease::LeaseId;
    use crate::podman_preview::{
        CpuLimitMillis, MemoryLimitMib, OciImageReference, PidsLimit, PreviewContainerSpec,
        PreviewPodmanOperation, PreviewPodmanPlan, PreviewRuntimeLimits, RootlessHostPort,
    };
    use crate::podman_preview_execution::{
        PreviewInspectExecutionReceipt, bind_preview_inspect_execution,
    };
    use crate::podman_preview_inspect::PreviewContainerStatus;
    use crate::preview::{PreviewGeneration, PreviewPort, PreviewRequest, PreviewTtl};
    use crate::process::ExecutionRecord;
    use crate::state::InstallationId;

    use super::{authorize_existing_preview_command, operation_allows_status};

    fn container_spec() -> PreviewContainerSpec {
        let artifact = ArtifactIdentity::new(
            RepositoryRef::parse("example/project").expect("repository"),
            CommitId::parse(&"1a".repeat(20)).expect("commit"),
            ArtifactKind::OciImage,
            Sha256Digest::parse(&format!("sha256:{}", "ab".repeat(32))).expect("digest"),
        );
        let image = OciImageReference::parse(
            &format!("registry.example.com/team/app@sha256:{}", "ab".repeat(32)),
            &artifact,
        )
        .expect("image reference");
        PreviewContainerSpec::new(
            InstallationId::parse("installation-001").expect("installation ID"),
            PreviewRequest::new(
                LeaseId::parse("pr-42").expect("slot"),
                artifact,
                PreviewPort::new(3000).expect("container port"),
                PreviewTtl::new(3600).expect("TTL"),
                None,
            ),
            PreviewGeneration::new(7).expect("generation"),
            image,
            PreviewRuntimeLimits::new(
                MemoryLimitMib::new(512).expect("memory"),
                CpuLimitMillis::new(1500).expect("CPU"),
                PidsLimit::new(256).expect("PIDs"),
            ),
            RootlessHostPort::new(42000).expect("host port"),
        )
        .expect("container spec")
    }

    fn runner() -> RunnerUserContext {
        RunnerUserContext::new(
            LinuxAccountName::parse("project-runner").expect("runner user"),
            1001,
            1001,
            "/var/lib/project-runner",
        )
        .expect("runner context")
    }

    fn receipt(
        spec: &PreviewContainerSpec,
        plan: &PreviewPodmanPlan,
        status: Option<&str>,
    ) -> PreviewInspectExecutionReceipt {
        let inspect = &plan.provision()[2];
        let mut state = serde_json::Map::new();
        if let Some(status) = status {
            state.insert("Status".to_owned(), json!(status));
        }
        let execution = ExecutionRecord {
            argv: inspect.spec().displayed_argv(),
            environment_keys: inspect.spec().environment.keys().cloned().collect(),
            status: Some(0),
            success: true,
            stdout: json!([{
                "Id": "cd".repeat(32),
                "Name": spec.container_name().as_str(),
                "ImageDigest": spec.image().digest().as_str(),
                "State": state,
                "Config": {"Labels": spec.expected_labels()}
            }])
            .to_string(),
            stderr: String::new(),
        };
        bind_preview_inspect_execution(spec, inspect, &execution).expect("bind inspection")
    }

    #[test]
    fn operation_state_matrix_is_conservative() {
        for status in [
            PreviewContainerStatus::Configured,
            PreviewContainerStatus::Created,
            PreviewContainerStatus::Initialized,
            PreviewContainerStatus::Stopped,
            PreviewContainerStatus::Exited,
        ] {
            assert!(operation_allows_status(PreviewPodmanOperation::Start, &status));
            assert!(operation_allows_status(PreviewPodmanOperation::Remove, &status));
            assert!(!operation_allows_status(PreviewPodmanOperation::Stop, &status));
        }
        assert!(operation_allows_status(
            PreviewPodmanOperation::Stop,
            &PreviewContainerStatus::Running
        ));
        assert!(!operation_allows_status(
            PreviewPodmanOperation::Start,
            &PreviewContainerStatus::Running
        ));
        assert!(!operation_allows_status(
            PreviewPodmanOperation::Remove,
            &PreviewContainerStatus::Running
        ));
        for status in [
            PreviewContainerStatus::Paused,
            PreviewContainerStatus::Removing,
            PreviewContainerStatus::Stopping,
            PreviewContainerStatus::Dead,
            PreviewContainerStatus::Other("unknown".to_owned()),
        ] {
            assert!(!operation_allows_status(PreviewPodmanOperation::Start, &status));
            assert!(!operation_allows_status(PreviewPodmanOperation::Stop, &status));
            assert!(!operation_allows_status(PreviewPodmanOperation::Remove, &status));
        }
    }

    #[test]
    fn state_appropriate_commands_authorize_by_container_id() {
        let spec = container_spec();
        let plan = PreviewPodmanPlan::for_container(&spec, &runner());

        let created = receipt(&spec, &plan, Some("created"));
        assert!(
            authorize_existing_preview_command(&spec, &plan.provision()[1], &created).is_ok()
        );

        let running = receipt(&spec, &plan, Some("running"));
        assert!(authorize_existing_preview_command(&spec, &plan.cleanup()[0], &running).is_ok());

        let stopped = receipt(&spec, &plan, Some("stopped"));
        assert!(authorize_existing_preview_command(&spec, &plan.cleanup()[1], &stopped).is_ok());
    }

    #[test]
    fn nonsensical_or_missing_states_fail_before_ownership_authorization() {
        let spec = container_spec();
        let plan = PreviewPodmanPlan::for_container(&spec, &runner());

        let running = receipt(&spec, &plan, Some("running"));
        let error = authorize_existing_preview_command(&spec, &plan.provision()[1], &running)
            .expect_err("running container must not start");
        assert_eq!(error.field, "state");

        let paused = receipt(&spec, &plan, Some("paused"));
        assert!(authorize_existing_preview_command(&spec, &plan.cleanup()[0], &paused).is_err());
        assert!(authorize_existing_preview_command(&spec, &plan.cleanup()[1], &paused).is_err());

        let stopped = receipt(&spec, &plan, Some("stopped"));
        assert!(authorize_existing_preview_command(&spec, &plan.cleanup()[0], &stopped).is_err());

        let missing = receipt(&spec, &plan, None);
        assert!(authorize_existing_preview_command(&spec, &plan.cleanup()[1], &missing).is_err());
    }
}
