use std::fmt;

use serde::Serialize;

use crate::podman_preview::{PreviewContainerSpec, PreviewPodmanCommand, PreviewPodmanOperation};
use crate::podman_preview_execution::{
    PreviewInspectExecutionReceipt, authorize_existing_preview_command_from_receipt,
};
use crate::podman_preview_inspect::{AuthorizedPreviewPodmanCommand, PreviewContainerStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewMutationDisposition {
    Execute,
    AlreadySatisfied,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreviewMutationPlan {
    operation: PreviewPodmanOperation,
    disposition: PreviewMutationDisposition,
    #[serde(skip_serializing_if = "Option::is_none")]
    observed_state: Option<String>,
    reason: String,
}

impl PreviewMutationPlan {
    #[must_use]
    pub const fn operation(&self) -> PreviewPodmanOperation {
        self.operation
    }

    #[must_use]
    pub const fn disposition(&self) -> PreviewMutationDisposition {
        self.disposition
    }

    #[must_use]
    pub fn observed_state(&self) -> Option<&str> {
        self.observed_state.as_deref()
    }

    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

/// Plan one existing-container operation from the latest observed Podman state.
///
/// Already satisfied states produce an explicit no-op. Transitional, paused, dead, missing, and
/// unknown states block execution. The function performs no ownership classification or mutation.
#[must_use]
pub fn plan_existing_preview_operation(
    operation: PreviewPodmanOperation,
    status: Option<&PreviewContainerStatus>,
) -> PreviewMutationPlan {
    let observed_state = status.map(status_name).map(ToOwned::to_owned);
    let (disposition, reason) = match (operation, status) {
        (
            PreviewPodmanOperation::Start,
            Some(
                PreviewContainerStatus::Configured
                | PreviewContainerStatus::Created
                | PreviewContainerStatus::Initialized
                | PreviewContainerStatus::Stopped
                | PreviewContainerStatus::Exited,
            ),
        ) => (
            PreviewMutationDisposition::Execute,
            "the inactive container may be started",
        ),
        (PreviewPodmanOperation::Start, Some(PreviewContainerStatus::Running)) => (
            PreviewMutationDisposition::AlreadySatisfied,
            "the container is already running",
        ),
        (PreviewPodmanOperation::Stop, Some(PreviewContainerStatus::Running)) => (
            PreviewMutationDisposition::Execute,
            "the running container may be stopped",
        ),
        (
            PreviewPodmanOperation::Stop,
            Some(
                PreviewContainerStatus::Configured
                | PreviewContainerStatus::Created
                | PreviewContainerStatus::Initialized
                | PreviewContainerStatus::Stopped
                | PreviewContainerStatus::Exited,
            ),
        ) => (
            PreviewMutationDisposition::AlreadySatisfied,
            "the container is already inactive",
        ),
        (
            PreviewPodmanOperation::Remove,
            Some(
                PreviewContainerStatus::Configured
                | PreviewContainerStatus::Created
                | PreviewContainerStatus::Initialized
                | PreviewContainerStatus::Stopped
                | PreviewContainerStatus::Exited,
            ),
        ) => (
            PreviewMutationDisposition::Execute,
            "the inactive container may be removed without force",
        ),
        (PreviewPodmanOperation::Create | PreviewPodmanOperation::Inspect, _) => (
            PreviewMutationDisposition::Blocked,
            "create and inspect do not use existing-container mutation planning",
        ),
        (_, None) => (
            PreviewMutationDisposition::Blocked,
            "Podman did not report a container state",
        ),
        (_, Some(PreviewContainerStatus::Paused)) => (
            PreviewMutationDisposition::Blocked,
            "paused containers require an explicit pause policy",
        ),
        (_, Some(PreviewContainerStatus::Removing | PreviewContainerStatus::Stopping)) => (
            PreviewMutationDisposition::Blocked,
            "the container is in a transitional state",
        ),
        (_, Some(PreviewContainerStatus::Dead)) => (
            PreviewMutationDisposition::Blocked,
            "dead containers require an explicit force-removal policy",
        ),
        (_, Some(PreviewContainerStatus::Other(_))) => (
            PreviewMutationDisposition::Blocked,
            "the observed Podman state is not recognized by this policy",
        ),
        (PreviewPodmanOperation::Remove, Some(PreviewContainerStatus::Running)) => (
            PreviewMutationDisposition::Blocked,
            "the unforced remove command cannot remove a running container",
        ),
    };

    PreviewMutationPlan {
        operation,
        disposition,
        observed_state,
        reason: reason.to_owned(),
    }
}

/// Authorize one existing-container mutation only when state, receipt, and ownership evidence match.
///
/// Callers may use [`plan_existing_preview_operation`] first to distinguish executable work from an
/// already satisfied state. This authorization function accepts only executable plans.
///
/// # Errors
///
/// Returns an error when the operation is already satisfied or blocked for the observed state, or
/// when the receipt does not prove exact ownership of the planned preview generation.
pub fn authorize_existing_preview_command(
    spec: &PreviewContainerSpec,
    command: &PreviewPodmanCommand,
    receipt: &PreviewInspectExecutionReceipt,
) -> Result<AuthorizedPreviewPodmanCommand, PreviewMutationAuthorizationError> {
    let plan = plan_existing_preview_operation(command.operation(), receipt.observation().status());
    if plan.disposition != PreviewMutationDisposition::Execute {
        return Err(PreviewMutationAuthorizationError::new(
            "state",
            format!(
                "operation {} has disposition {}: {}",
                operation_name(command.operation()),
                disposition_name(plan.disposition),
                plan.reason
            ),
        ));
    }

    authorize_existing_preview_command_from_receipt(spec, command, receipt)
        .map_err(|error| PreviewMutationAuthorizationError::new("ownership", error.to_string()))
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

const fn operation_name(operation: PreviewPodmanOperation) -> &'static str {
    match operation {
        PreviewPodmanOperation::Create => "create",
        PreviewPodmanOperation::Start => "start",
        PreviewPodmanOperation::Inspect => "inspect",
        PreviewPodmanOperation::Stop => "stop",
        PreviewPodmanOperation::Remove => "remove",
    }
}

const fn disposition_name(disposition: PreviewMutationDisposition) -> &'static str {
    match disposition {
        PreviewMutationDisposition::Execute => "execute",
        PreviewMutationDisposition::AlreadySatisfied => "already_satisfied",
        PreviewMutationDisposition::Blocked => "blocked",
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

    use super::{
        PreviewMutationDisposition, authorize_existing_preview_command,
        plan_existing_preview_operation,
    };

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
    fn planner_distinguishes_execute_noop_and_blocked_states() {
        for status in [
            PreviewContainerStatus::Configured,
            PreviewContainerStatus::Created,
            PreviewContainerStatus::Initialized,
            PreviewContainerStatus::Stopped,
            PreviewContainerStatus::Exited,
        ] {
            assert_eq!(
                plan_existing_preview_operation(PreviewPodmanOperation::Start, Some(&status))
                    .disposition(),
                PreviewMutationDisposition::Execute
            );
            assert_eq!(
                plan_existing_preview_operation(PreviewPodmanOperation::Stop, Some(&status))
                    .disposition(),
                PreviewMutationDisposition::AlreadySatisfied
            );
            assert_eq!(
                plan_existing_preview_operation(PreviewPodmanOperation::Remove, Some(&status))
                    .disposition(),
                PreviewMutationDisposition::Execute
            );
        }

        assert_eq!(
            plan_existing_preview_operation(
                PreviewPodmanOperation::Start,
                Some(&PreviewContainerStatus::Running),
            )
            .disposition(),
            PreviewMutationDisposition::AlreadySatisfied
        );
        assert_eq!(
            plan_existing_preview_operation(
                PreviewPodmanOperation::Stop,
                Some(&PreviewContainerStatus::Running),
            )
            .disposition(),
            PreviewMutationDisposition::Execute
        );
        assert_eq!(
            plan_existing_preview_operation(
                PreviewPodmanOperation::Remove,
                Some(&PreviewContainerStatus::Running),
            )
            .disposition(),
            PreviewMutationDisposition::Blocked
        );

        for status in [
            PreviewContainerStatus::Paused,
            PreviewContainerStatus::Removing,
            PreviewContainerStatus::Stopping,
            PreviewContainerStatus::Dead,
            PreviewContainerStatus::Other("unknown".to_owned()),
        ] {
            for operation in [
                PreviewPodmanOperation::Start,
                PreviewPodmanOperation::Stop,
                PreviewPodmanOperation::Remove,
            ] {
                assert_eq!(
                    plan_existing_preview_operation(operation, Some(&status)).disposition(),
                    PreviewMutationDisposition::Blocked
                );
            }
        }
        assert_eq!(
            plan_existing_preview_operation(PreviewPodmanOperation::Start, None).disposition(),
            PreviewMutationDisposition::Blocked
        );
    }

    #[test]
    fn state_appropriate_commands_authorize_by_container_id() {
        let spec = container_spec();
        let plan = PreviewPodmanPlan::for_container(&spec, &runner());

        let created = receipt(&spec, &plan, Some("created"));
        assert!(authorize_existing_preview_command(&spec, &plan.provision()[1], &created).is_ok());

        let running = receipt(&spec, &plan, Some("running"));
        assert!(authorize_existing_preview_command(&spec, &plan.cleanup()[0], &running).is_ok());

        let stopped = receipt(&spec, &plan, Some("stopped"));
        assert!(authorize_existing_preview_command(&spec, &plan.cleanup()[1], &stopped).is_ok());
    }

    #[test]
    fn already_satisfied_and_blocked_states_do_not_authorize_subprocesses() {
        let spec = container_spec();
        let plan = PreviewPodmanPlan::for_container(&spec, &runner());

        let running = receipt(&spec, &plan, Some("running"));
        let error = authorize_existing_preview_command(&spec, &plan.provision()[1], &running)
            .expect_err("running container must produce a start no-op");
        assert_eq!(error.field, "state");
        assert!(error.problem.contains("already_satisfied"));

        let stopped = receipt(&spec, &plan, Some("stopped"));
        let error = authorize_existing_preview_command(&spec, &plan.cleanup()[0], &stopped)
            .expect_err("stopped container must produce a stop no-op");
        assert!(error.problem.contains("already_satisfied"));

        let paused = receipt(&spec, &plan, Some("paused"));
        assert!(authorize_existing_preview_command(&spec, &plan.cleanup()[0], &paused).is_err());
        assert!(authorize_existing_preview_command(&spec, &plan.cleanup()[1], &paused).is_err());

        let missing = receipt(&spec, &plan, None);
        assert!(authorize_existing_preview_command(&spec, &plan.cleanup()[1], &missing).is_err());
    }
}
