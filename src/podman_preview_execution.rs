use std::fmt;

use serde::Serialize;

use crate::podman_preview::{
    PreviewCommandEffect, PreviewContainerSpec, PreviewPodmanCommand, PreviewPodmanOperation,
};
use crate::podman_preview_inspect::{
    AuthorizedPreviewPodmanCommand, PreviewAuthorizationError, PreviewContainerObservation,
    authorize_existing_preview_command_from_observation, decode_preview_container_inspect,
};
use crate::process::{CommandValue, ExecutionRecord};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreviewInspectExecutionReceipt {
    inspect_command_id: String,
    observation: PreviewContainerObservation,
    #[serde(skip_serializing_if = "Option::is_none")]
    diagnostic: Option<String>,
}

impl PreviewInspectExecutionReceipt {
    #[must_use]
    pub fn inspect_command_id(&self) -> &str {
        &self.inspect_command_id
    }

    #[must_use]
    pub const fn observation(&self) -> &PreviewContainerObservation {
        &self.observation
    }

    #[must_use]
    pub fn diagnostic(&self) -> Option<&str> {
        self.diagnostic.as_deref()
    }
}

/// Bind one successful process execution to the reviewed Podman inspect command and decode its
/// bounded JSON output.
///
/// The execution record must match the exact displayed argv and environment-key set produced by the
/// planner. Invalid UTF-8 replacement characters fail closed because machine-readable ownership
/// evidence must survive process capture without lossy conversion.
///
/// # Errors
///
/// Returns an error unless the command is the planner's read-only inspect operation, the execution
/// succeeded with status zero, its public command evidence matches exactly, and stdout decodes as one
/// bounded Podman container observation.
pub fn bind_preview_inspect_execution(
    spec: &PreviewContainerSpec,
    inspect_command: &PreviewPodmanCommand,
    execution: &ExecutionRecord,
) -> Result<PreviewInspectExecutionReceipt, PreviewInspectExecutionError> {
    if inspect_command.operation() != PreviewPodmanOperation::Inspect
        || inspect_command.effect() != PreviewCommandEffect::ReadOnly
        || inspect_command.requires_matching_labels()
    {
        return Err(PreviewInspectExecutionError::new(
            "command",
            "must be the planner-produced read-only inspect operation",
        ));
    }

    let Some(CommandValue::Plain(target)) = inspect_command.spec().arguments.last() else {
        return Err(PreviewInspectExecutionError::new(
            "command",
            "inspect command must end with one plain container-name target",
        ));
    };
    if target != spec.container_name().as_str() {
        return Err(PreviewInspectExecutionError::new(
            "command",
            "inspect command target does not match the planned preview container",
        ));
    }

    if execution.argv != inspect_command.spec().displayed_argv() {
        return Err(PreviewInspectExecutionError::new(
            "execution",
            "argv does not match the reviewed inspect command",
        ));
    }
    let expected_environment_keys = inspect_command
        .spec()
        .environment
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    if execution.environment_keys != expected_environment_keys {
        return Err(PreviewInspectExecutionError::new(
            "execution",
            "environment keys do not match the reviewed inspect command",
        ));
    }
    if !execution.success || execution.status != Some(0) {
        return Err(PreviewInspectExecutionError::new(
            "execution",
            "inspect command did not complete successfully with status zero",
        ));
    }
    if execution.stdout.contains('\u{fffd}') {
        return Err(PreviewInspectExecutionError::new(
            "stdout",
            "contains a Unicode replacement character and cannot prove lossless JSON capture",
        ));
    }

    let observation = decode_preview_container_inspect(execution.stdout.as_bytes())
        .map_err(|error| PreviewInspectExecutionError::new("stdout", error.to_string()))?;
    let diagnostic = (!execution.stderr.is_empty()).then(|| execution.stderr.clone());

    Ok(PreviewInspectExecutionReceipt {
        inspect_command_id: inspect_command.id().to_owned(),
        observation,
        diagnostic,
    })
}

/// Authorize one existing-container mutation from a bound inspect execution receipt.
///
/// This crate-visible helper proves exact ownership. The public state-aware gate adds operation and
/// Podman-state compatibility before exposing an authorized command.
///
/// # Errors
///
/// Returns an error unless the receipt's observation is managed by the exact preview generation and
/// the mutation is a planner-produced start, stop, or remove command.
pub(crate) fn authorize_existing_preview_command_from_receipt(
    spec: &PreviewContainerSpec,
    command: &PreviewPodmanCommand,
    receipt: &PreviewInspectExecutionReceipt,
) -> Result<AuthorizedPreviewPodmanCommand, PreviewAuthorizationError> {
    authorize_existing_preview_command_from_observation(spec, command, receipt.observation())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreviewInspectExecutionError {
    pub field: String,
    pub problem: String,
}

impl PreviewInspectExecutionError {
    fn new(field: impl Into<String>, problem: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            problem: problem.into(),
        }
    }
}

impl fmt::Display for PreviewInspectExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", self.field, self.problem)
    }
}

impl std::error::Error for PreviewInspectExecutionError {}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::artifact::{ArtifactIdentity, ArtifactKind, CommitId, RepositoryRef, Sha256Digest};
    use crate::lane_command::{LinuxAccountName, RunnerUserContext};
    use crate::lease::LeaseId;
    use crate::podman_preview::{
        CpuLimitMillis, MemoryLimitMib, OciImageReference, PidsLimit, PreviewContainerSpec,
        PreviewPodmanPlan, PreviewRuntimeLimits, RootlessHostPort,
    };
    use crate::preview::{PreviewGeneration, PreviewPort, PreviewRequest, PreviewTtl};
    use crate::process::ExecutionRecord;
    use crate::state::InstallationId;

    use super::{authorize_existing_preview_command_from_receipt, bind_preview_inspect_execution};

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

    fn execution_record(spec: &PreviewContainerSpec, plan: &PreviewPodmanPlan) -> ExecutionRecord {
        let inspect = &plan.provision()[2];
        ExecutionRecord {
            argv: inspect.spec().displayed_argv(),
            environment_keys: inspect.spec().environment.keys().cloned().collect(),
            status: Some(0),
            success: true,
            stdout: json!([{
                "Id": "cd".repeat(32),
                "Name": spec.container_name().as_str(),
                "ImageDigest": spec.image().digest().as_str(),
                "State": {"Status": "created"},
                "Config": {"Labels": spec.expected_labels()}
            }])
            .to_string(),
            stderr: String::new(),
        }
    }

    #[test]
    fn successful_exact_execution_binds_and_authorizes_by_container_id() {
        let spec = container_spec();
        let plan = PreviewPodmanPlan::for_container(&spec, &runner());
        let execution = execution_record(&spec, &plan);
        let receipt = bind_preview_inspect_execution(&spec, &plan.provision()[2], &execution)
            .expect("bind inspection");
        let authorized =
            authorize_existing_preview_command_from_receipt(&spec, &plan.provision()[1], &receipt)
                .expect("authorize start");

        assert_eq!(receipt.inspect_command_id(), plan.provision()[2].id());
        assert_eq!(
            authorized.spec().displayed_argv().last().expect("target"),
            &"cd".repeat(32)
        );
    }

    #[test]
    fn altered_command_evidence_and_failed_execution_are_rejected() {
        let spec = container_spec();
        let plan = PreviewPodmanPlan::for_container(&spec, &runner());
        let mut execution = execution_record(&spec, &plan);
        execution.argv.push("unexpected".to_owned());
        assert!(bind_preview_inspect_execution(&spec, &plan.provision()[2], &execution).is_err());

        let mut execution = execution_record(&spec, &plan);
        execution.success = false;
        execution.status = Some(1);
        assert!(bind_preview_inspect_execution(&spec, &plan.provision()[2], &execution).is_err());

        let execution = execution_record(&spec, &plan);
        assert!(bind_preview_inspect_execution(&spec, &plan.provision()[0], &execution).is_err());
    }

    #[test]
    fn lossy_stdout_and_wrong_environment_keys_are_rejected() {
        let spec = container_spec();
        let plan = PreviewPodmanPlan::for_container(&spec, &runner());
        let mut execution = execution_record(&spec, &plan);
        execution.stdout.push('\u{fffd}');
        assert!(bind_preview_inspect_execution(&spec, &plan.provision()[2], &execution).is_err());

        let mut execution = execution_record(&spec, &plan);
        execution.environment_keys.push("UNEXPECTED".to_owned());
        assert!(bind_preview_inspect_execution(&spec, &plan.provision()[2], &execution).is_err());
    }

    #[test]
    fn diagnostic_stderr_is_retained_but_never_used_as_evidence() {
        let spec = container_spec();
        let plan = PreviewPodmanPlan::for_container(&spec, &runner());
        let mut execution = execution_record(&spec, &plan);
        execution.stderr = "bounded Podman warning".to_owned();
        let receipt = bind_preview_inspect_execution(&spec, &plan.provision()[2], &execution)
            .expect("bind inspection");

        assert_eq!(receipt.diagnostic(), Some("bounded Podman warning"));
        assert_eq!(
            receipt.observation().name().as_str(),
            spec.container_name().as_str()
        );
    }
}
