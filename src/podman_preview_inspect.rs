use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::artifact::Sha256Digest;
use crate::podman_preview::{PreviewContainerSpec, PreviewPodmanCommand, PreviewPodmanOperation};
use crate::process::{CommandSpec, CommandValue};

pub const MAX_PODMAN_INSPECT_BYTES: usize = 1_048_576;
const CONTAINER_ID_HEX_LEN: usize = 64;
const MAX_CONTAINER_NAME_BYTES: usize = 128;
const MAX_STATUS_BYTES: usize = 32;
const MAX_LABELS: usize = 256;
const MAX_LABEL_KEY_BYTES: usize = 256;
const MAX_LABEL_VALUE_BYTES: usize = 4_096;

const LABEL_SCHEMA: &str = "io.smolrunner.schema";
const LABEL_INSTALLATION: &str = "io.smolrunner.installation";
const LABEL_LEASE: &str = "io.smolrunner.lease";
const LABEL_GENERATION: &str = "io.smolrunner.preview.generation";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PodmanContainerId(String);

impl PodmanContainerId {
    /// Validate one full Podman container identifier.
    ///
    /// # Errors
    ///
    /// Returns an error unless the value is exactly 64 lowercase hexadecimal characters.
    pub fn parse(value: &str) -> Result<Self, PreviewInspectError> {
        if value.len() != CONTAINER_ID_HEX_LEN || !value.bytes().all(is_lower_hex) {
            return Err(PreviewInspectError::new(
                "container_id",
                "must be a full 64-character lowercase hexadecimal container ID",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ObservedContainerName(String);

impl ObservedContainerName {
    fn parse(value: &str) -> Result<Self, PreviewInspectError> {
        let mut bytes = value.bytes();
        let first_is_safe = bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric());
        let remainder_is_safe =
            bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
        if value.len() > MAX_CONTAINER_NAME_BYTES || !first_is_safe || !remainder_is_safe {
            return Err(PreviewInspectError::new(
                "container_name",
                "must be a bounded Podman name beginning with an ASCII letter or digit",
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
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum PreviewContainerStatus {
    Configured,
    Created,
    Initialized,
    Running,
    Stopped,
    Paused,
    Exited,
    Removing,
    Stopping,
    Dead,
    Other(String),
}

impl PreviewContainerStatus {
    fn parse(value: &str) -> Result<Self, PreviewInspectError> {
        if value.is_empty()
            || value.len() > MAX_STATUS_BYTES
            || !value.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
            })
        {
            return Err(PreviewInspectError::new(
                "status",
                "must be a bounded lowercase ASCII Podman status",
            ));
        }
        Ok(match value {
            "configured" => Self::Configured,
            "created" => Self::Created,
            "initialized" => Self::Initialized,
            "running" => Self::Running,
            "stopped" => Self::Stopped,
            "paused" => Self::Paused,
            "exited" => Self::Exited,
            "removing" => Self::Removing,
            "stopping" => Self::Stopping,
            "dead" => Self::Dead,
            other => Self::Other(other.to_owned()),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreviewContainerObservation {
    container_id: PodmanContainerId,
    name: ObservedContainerName,
    #[serde(skip_serializing_if = "Option::is_none")]
    image_digest: Option<Sha256Digest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<PreviewContainerStatus>,
    labels: BTreeMap<String, String>,
}

impl PreviewContainerObservation {
    #[must_use]
    pub const fn container_id(&self) -> &PodmanContainerId {
        &self.container_id
    }

    #[must_use]
    pub const fn name(&self) -> &ObservedContainerName {
        &self.name
    }

    #[must_use]
    pub const fn image_digest(&self) -> Option<&Sha256Digest> {
        self.image_digest.as_ref()
    }

    #[must_use]
    pub const fn status(&self) -> Option<&PreviewContainerStatus> {
        self.status.as_ref()
    }

    #[must_use]
    pub const fn labels(&self) -> &BTreeMap<String, String> {
        &self.labels
    }
}

#[derive(Debug, Deserialize)]
struct InspectContainerWire {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "ImageDigest", default)]
    image_digest: Option<String>,
    #[serde(rename = "State", default)]
    state: Option<InspectStateWire>,
    #[serde(rename = "Config", default)]
    config: Option<InspectConfigWire>,
}

#[derive(Debug, Deserialize)]
struct InspectStateWire {
    #[serde(rename = "Status", default)]
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct InspectConfigWire {
    #[serde(rename = "Labels", default)]
    labels: Option<BTreeMap<String, String>>,
}

/// Decode the default JSON array emitted by `podman container inspect`.
///
/// Unknown Podman fields are ignored, while the identity, digest, state, and labels SmolRunner uses
/// are bounded and revalidated. Exactly one container result is required.
///
/// # Errors
///
/// Returns an error for oversized output, malformed JSON, zero or multiple results, unsafe identity
/// values, invalid digests or statuses, or excessive label data.
pub fn decode_preview_container_inspect(
    bytes: &[u8],
) -> Result<PreviewContainerObservation, PreviewInspectError> {
    if bytes.is_empty() {
        return Err(PreviewInspectError::new(
            "inspect_output",
            "must contain one Podman JSON result",
        ));
    }
    if bytes.len() > MAX_PODMAN_INSPECT_BYTES {
        return Err(PreviewInspectError::new(
            "inspect_output",
            format!("exceeds the {MAX_PODMAN_INSPECT_BYTES}-byte limit"),
        ));
    }

    let mut results: Vec<InspectContainerWire> = serde_json::from_slice(bytes)
        .map_err(|error| PreviewInspectError::new("inspect_output", error.to_string()))?;
    if results.len() != 1 {
        return Err(PreviewInspectError::new(
            "inspect_output",
            "must contain exactly one container result",
        ));
    }
    let wire = results.pop().expect("length checked");
    let container_id = PodmanContainerId::parse(&wire.id)?;
    let name = ObservedContainerName::parse(&wire.name)?;
    let image_digest = wire
        .image_digest
        .as_deref()
        .map(Sha256Digest::parse)
        .transpose()
        .map_err(|error| PreviewInspectError::new("image_digest", error.to_string()))?;
    let status_text = wire.state.and_then(|state| state.status);
    let status = status_text
        .as_deref()
        .map(PreviewContainerStatus::parse)
        .transpose()?;
    let labels = wire
        .config
        .and_then(|config| config.labels)
        .unwrap_or_default();
    validate_labels(&labels)?;

    Ok(PreviewContainerObservation {
        container_id,
        name,
        image_digest,
        status,
        labels,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewContainerOwnershipClass {
    Managed,
    Foreign,
    Conflicting,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreviewContainerOwnershipAssessment {
    pub class: PreviewContainerOwnershipClass,
    pub reasons: Vec<String>,
}

impl PreviewContainerOwnershipAssessment {
    fn one(class: PreviewContainerOwnershipClass, reason: impl Into<String>) -> Self {
        Self {
            class,
            reasons: vec![reason.into()],
        }
    }

    #[must_use]
    pub const fn allows_existing_container_mutation(&self) -> bool {
        matches!(self.class, PreviewContainerOwnershipClass::Managed)
    }
}

/// Compare one observed container with the exact planned preview generation.
///
/// Container names are locators only. Managed classification additionally requires the exact image
/// digest and every expected SmolRunner ownership label.
#[must_use]
pub fn assess_preview_container_ownership(
    spec: &PreviewContainerSpec,
    observed: &PreviewContainerObservation,
) -> PreviewContainerOwnershipAssessment {
    if observed.name.as_str() != spec.container_name().as_str() {
        return PreviewContainerOwnershipAssessment::one(
            PreviewContainerOwnershipClass::Conflicting,
            "the desired container locator resolves to another container name",
        );
    }

    let Some(image_digest) = observed.image_digest.as_ref() else {
        return PreviewContainerOwnershipAssessment::one(
            PreviewContainerOwnershipClass::Unknown,
            "Podman did not report an immutable container image digest",
        );
    };
    if image_digest != spec.image().digest() {
        return PreviewContainerOwnershipAssessment::one(
            PreviewContainerOwnershipClass::Conflicting,
            "the observed container image digest differs from the planned artifact",
        );
    }

    for (key, expected) in spec.expected_labels() {
        let Some(actual) = observed.labels.get(key) else {
            return PreviewContainerOwnershipAssessment::one(
                PreviewContainerOwnershipClass::Unknown,
                format!("required ownership label {key:?} is missing"),
            );
        };
        if actual == expected {
            continue;
        }
        let class = match key.as_str() {
            LABEL_SCHEMA => PreviewContainerOwnershipClass::Unknown,
            LABEL_INSTALLATION | LABEL_LEASE => PreviewContainerOwnershipClass::Foreign,
            LABEL_GENERATION => PreviewContainerOwnershipClass::Conflicting,
            _ => PreviewContainerOwnershipClass::Conflicting,
        };
        return PreviewContainerOwnershipAssessment::one(
            class,
            format!("ownership label {key:?} does not match the planned preview generation"),
        );
    }

    PreviewContainerOwnershipAssessment::one(
        PreviewContainerOwnershipClass::Managed,
        "container name, image digest, and ownership labels match the planned preview generation",
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuthorizedPreviewPodmanCommand {
    command_id: String,
    operation: PreviewPodmanOperation,
    container_id: PodmanContainerId,
    spec: CommandSpec,
}

impl AuthorizedPreviewPodmanCommand {
    #[must_use]
    pub fn command_id(&self) -> &str {
        &self.command_id
    }

    #[must_use]
    pub const fn operation(&self) -> PreviewPodmanOperation {
        self.operation
    }

    #[must_use]
    pub const fn container_id(&self) -> &PodmanContainerId {
        &self.container_id
    }

    #[must_use]
    pub const fn spec(&self) -> &CommandSpec {
        &self.spec
    }
}

/// Authorize one planned start, stop, or remove command against a fresh inspect observation.
///
/// The returned command targets the immutable container ID instead of the mutable name locator.
/// Read-only commands and create operations do not pass through this existing-container gate.
///
/// # Errors
///
/// Returns an error unless the command requires ownership evidence, the observation is managed by
/// the exact preview generation, and the command's planned name target matches the observation.
pub fn authorize_existing_preview_command(
    spec: &PreviewContainerSpec,
    command: &PreviewPodmanCommand,
    observed: &PreviewContainerObservation,
) -> Result<AuthorizedPreviewPodmanCommand, PreviewAuthorizationError> {
    if !command.requires_matching_labels() {
        return Err(PreviewAuthorizationError::new(
            "command does not require existing-container ownership proof",
        ));
    }
    if !matches!(
        command.operation(),
        PreviewPodmanOperation::Start
            | PreviewPodmanOperation::Stop
            | PreviewPodmanOperation::Remove
    ) {
        return Err(PreviewAuthorizationError::new(
            "only start, stop, and remove use the existing-container authorization gate",
        ));
    }

    let assessment = assess_preview_container_ownership(spec, observed);
    if !assessment.allows_existing_container_mutation() {
        return Err(PreviewAuthorizationError::new(format!(
            "container ownership is {:?}: {}",
            assessment.class,
            assessment.reasons.join("; ")
        )));
    }

    let mut authorized_spec = command.spec().clone();
    let Some(CommandValue::Plain(target)) = authorized_spec.arguments.last_mut() else {
        return Err(PreviewAuthorizationError::new(
            "planned Podman command lacks one plain container target",
        ));
    };
    if target.as_str() != spec.container_name().as_str()
        || target.as_str() != observed.name.as_str()
    {
        return Err(PreviewAuthorizationError::new(
            "planned Podman command target does not match the inspected container locator",
        ));
    }
    *target = observed.container_id.as_str().to_owned();

    Ok(AuthorizedPreviewPodmanCommand {
        command_id: command.id().to_owned(),
        operation: command.operation(),
        container_id: observed.container_id.clone(),
        spec: authorized_spec,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreviewInspectError {
    pub field: String,
    pub problem: String,
}

impl PreviewInspectError {
    fn new(field: impl Into<String>, problem: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            problem: problem.into(),
        }
    }
}

impl fmt::Display for PreviewInspectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", self.field, self.problem)
    }
}

impl std::error::Error for PreviewInspectError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreviewAuthorizationError {
    pub problem: String,
}

impl PreviewAuthorizationError {
    fn new(problem: impl Into<String>) -> Self {
        Self {
            problem: problem.into(),
        }
    }
}

impl fmt::Display for PreviewAuthorizationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.problem)
    }
}

impl std::error::Error for PreviewAuthorizationError {}

fn validate_labels(labels: &BTreeMap<String, String>) -> Result<(), PreviewInspectError> {
    if labels.len() > MAX_LABELS {
        return Err(PreviewInspectError::new(
            "labels",
            format!("contains more than {MAX_LABELS} entries"),
        ));
    }
    for (key, value) in labels {
        if key.is_empty() || key.len() > MAX_LABEL_KEY_BYTES {
            return Err(PreviewInspectError::new(
                "labels",
                format!("contains an empty or oversized key of {} bytes", key.len()),
            ));
        }
        if value.len() > MAX_LABEL_VALUE_BYTES {
            return Err(PreviewInspectError::new(
                "labels",
                format!("label {key:?} exceeds {MAX_LABEL_VALUE_BYTES} bytes"),
            ));
        }
    }
    Ok(())
}

const fn is_lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use crate::artifact::{ArtifactIdentity, ArtifactKind, CommitId, RepositoryRef, Sha256Digest};
    use crate::lane_command::{LinuxAccountName, RunnerUserContext};
    use crate::lease::LeaseId;
    use crate::podman_preview::{
        CpuLimitMillis, MemoryLimitMib, OciImageReference, PidsLimit, PreviewContainerSpec,
        PreviewPodmanPlan, PreviewRuntimeLimits, RootlessHostPort,
    };
    use crate::preview::{PreviewGeneration, PreviewPort, PreviewRequest, PreviewTtl};
    use crate::state::InstallationId;

    use super::{
        MAX_PODMAN_INSPECT_BYTES, PreviewContainerOwnershipClass,
        assess_preview_container_ownership, authorize_existing_preview_command,
        decode_preview_container_inspect,
    };

    fn artifact(digest_byte: &str) -> ArtifactIdentity {
        ArtifactIdentity::new(
            RepositoryRef::parse("example/project").expect("repository"),
            CommitId::parse(&"1a".repeat(20)).expect("commit"),
            ArtifactKind::OciImage,
            Sha256Digest::parse(&format!("sha256:{}", digest_byte.repeat(32))).expect("digest"),
        )
    }

    fn container_spec() -> PreviewContainerSpec {
        let artifact = artifact("ab");
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

    fn inspect_json(
        spec: &PreviewContainerSpec,
        labels: BTreeMap<String, String>,
        image_digest: Option<&str>,
        name: &str,
        status: &str,
    ) -> String {
        json!([{
            "Id": "cd".repeat(32),
            "Name": name,
            "ImageDigest": image_digest,
            "State": {"Status": status, "Running": status == "running"},
            "Config": {"Labels": labels, "Ignored": true},
            "ImageName": spec.image().as_str(),
            "UnknownFutureField": {"nested": true}
        }])
        .to_string()
    }

    #[test]
    fn default_podman_json_decodes_one_bounded_observation() {
        let spec = container_spec();
        let encoded = inspect_json(
            &spec,
            spec.expected_labels().clone(),
            Some(spec.image().digest().as_str()),
            spec.container_name().as_str(),
            "created",
        );
        let observed =
            decode_preview_container_inspect(encoded.as_bytes()).expect("decode inspect");

        assert_eq!(observed.container_id().as_str(), "cd".repeat(32));
        assert_eq!(observed.name().as_str(), spec.container_name().as_str());
        assert_eq!(
            observed.image_digest().expect("image digest"),
            spec.image().digest()
        );
        assert_eq!(observed.labels(), spec.expected_labels());
    }

    #[test]
    fn exact_evidence_is_managed_and_authorization_retargets_to_container_id() {
        let spec = container_spec();
        let encoded = inspect_json(
            &spec,
            spec.expected_labels().clone(),
            Some(spec.image().digest().as_str()),
            spec.container_name().as_str(),
            "created",
        );
        let observed =
            decode_preview_container_inspect(encoded.as_bytes()).expect("decode inspect");
        let assessment = assess_preview_container_ownership(&spec, &observed);
        assert_eq!(assessment.class, PreviewContainerOwnershipClass::Managed);

        let plan = PreviewPodmanPlan::for_container(&spec, &runner());
        let start = &plan.provision()[1];
        let authorized =
            authorize_existing_preview_command(&spec, start, &observed).expect("authorize start");
        assert_eq!(
            authorized.spec().displayed_argv().last().expect("target"),
            observed.container_id().as_str()
        );
        assert_eq!(
            start
                .spec()
                .displayed_argv()
                .last()
                .expect("original target"),
            spec.container_name().as_str()
        );
    }

    #[test]
    fn foreign_conflicting_and_unknown_evidence_fail_closed() {
        let spec = container_spec();

        let mut foreign_labels = spec.expected_labels().clone();
        foreign_labels.insert(
            "io.smolrunner.installation".to_owned(),
            "another-installation".to_owned(),
        );
        let foreign = decode_preview_container_inspect(
            inspect_json(
                &spec,
                foreign_labels,
                Some(spec.image().digest().as_str()),
                spec.container_name().as_str(),
                "running",
            )
            .as_bytes(),
        )
        .expect("decode foreign");
        assert_eq!(
            assess_preview_container_ownership(&spec, &foreign).class,
            PreviewContainerOwnershipClass::Foreign
        );

        let mut conflicting_labels = spec.expected_labels().clone();
        conflicting_labels.insert(
            "io.smolrunner.artifact.digest".to_owned(),
            format!("sha256:{}", "ef".repeat(32)),
        );
        let conflicting = decode_preview_container_inspect(
            inspect_json(
                &spec,
                conflicting_labels,
                Some(spec.image().digest().as_str()),
                spec.container_name().as_str(),
                "running",
            )
            .as_bytes(),
        )
        .expect("decode conflict");
        assert_eq!(
            assess_preview_container_ownership(&spec, &conflicting).class,
            PreviewContainerOwnershipClass::Conflicting
        );

        let mut incomplete_labels = spec.expected_labels().clone();
        incomplete_labels.remove("io.smolrunner.preview.generation");
        let unknown = decode_preview_container_inspect(
            inspect_json(
                &spec,
                incomplete_labels,
                Some(spec.image().digest().as_str()),
                spec.container_name().as_str(),
                "running",
            )
            .as_bytes(),
        )
        .expect("decode incomplete");
        assert_eq!(
            assess_preview_container_ownership(&spec, &unknown).class,
            PreviewContainerOwnershipClass::Unknown
        );
    }

    #[test]
    fn image_or_name_substitution_is_conflicting() {
        let spec = container_spec();
        let wrong_image = format!("sha256:{}", "ef".repeat(32));
        let image_conflict = decode_preview_container_inspect(
            inspect_json(
                &spec,
                spec.expected_labels().clone(),
                Some(&wrong_image),
                spec.container_name().as_str(),
                "running",
            )
            .as_bytes(),
        )
        .expect("decode image conflict");
        assert_eq!(
            assess_preview_container_ownership(&spec, &image_conflict).class,
            PreviewContainerOwnershipClass::Conflicting
        );

        let name_conflict = decode_preview_container_inspect(
            inspect_json(
                &spec,
                spec.expected_labels().clone(),
                Some(spec.image().digest().as_str()),
                "smol-preview-another-g7-0000000000000000",
                "running",
            )
            .as_bytes(),
        )
        .expect("decode name conflict");
        assert_eq!(
            assess_preview_container_ownership(&spec, &name_conflict).class,
            PreviewContainerOwnershipClass::Conflicting
        );
    }

    #[test]
    fn decoder_rejects_malformed_ambiguous_and_oversized_output() {
        assert!(decode_preview_container_inspect(b"[]").is_err());
        assert!(decode_preview_container_inspect(b"[{}, {}]").is_err());
        assert!(decode_preview_container_inspect(b"not json").is_err());
        assert!(
            decode_preview_container_inspect(&vec![b' '; MAX_PODMAN_INSPECT_BYTES + 1]).is_err()
        );

        let invalid_id = json!([{
            "Id": "abc123",
            "Name": "container-1",
            "ImageDigest": format!("sha256:{}", "ab".repeat(32)),
            "State": {"Status": "running"},
            "Config": {"Labels": {}}
        }])
        .to_string();
        assert!(decode_preview_container_inspect(invalid_id.as_bytes()).is_err());

        let invalid_status = json!([{
            "Id": "cd".repeat(32),
            "Name": "container-1",
            "State": {"Status": "RUNNING\n"},
            "Config": {"Labels": {}}
        }])
        .to_string();
        assert!(decode_preview_container_inspect(invalid_status.as_bytes()).is_err());
    }

    #[test]
    fn authorization_rejects_unmanaged_or_read_only_commands() {
        let spec = container_spec();
        let mut labels = spec.expected_labels().clone();
        labels.remove("io.smolrunner.lease");
        let observed = decode_preview_container_inspect(
            inspect_json(
                &spec,
                labels,
                Some(spec.image().digest().as_str()),
                spec.container_name().as_str(),
                "running",
            )
            .as_bytes(),
        )
        .expect("decode inspect");
        let plan = PreviewPodmanPlan::for_container(&spec, &runner());

        assert!(authorize_existing_preview_command(&spec, &plan.cleanup()[0], &observed).is_err());
        assert!(
            authorize_existing_preview_command(&spec, &plan.provision()[2], &observed).is_err()
        );
    }
}
