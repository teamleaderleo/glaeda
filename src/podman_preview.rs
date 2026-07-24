use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use serde::Serialize;

use crate::artifact::{ArtifactIdentity, ArtifactKind, Sha256Digest};
use crate::journal::{ExecutionLane, RollbackClass};
use crate::lane_command::RunnerUserContext;
use crate::lease::LeaseId;
use crate::preview::{PreviewGeneration, PreviewRequest};
use crate::process::CommandSpec;
use crate::state::InstallationId;

const RUNUSER: &str = "/usr/sbin/runuser";
const PODMAN: &str = "/usr/bin/podman";
const MIN_MEMORY_MIB: u32 = 64;
const MAX_MEMORY_MIB: u32 = 65_536;
const MIN_CPU_MILLIS: u32 = 100;
const MAX_CPU_MILLIS: u32 = 64_000;
const MIN_PIDS: u32 = 16;
const MAX_PIDS: u32 = 32_768;
const MIN_ROOTLESS_HOST_PORT: u16 = 1_024;
const MAX_IMAGE_REFERENCE_BYTES: usize = 512;
const CONTAINER_NAME_SLOT_PREFIX_BYTES: usize = 10;
const PREVIEW_LABEL_SCHEMA_VERSION: u8 = 1;
const STOP_TIMEOUT_SECONDS: u8 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct MemoryLimitMib(u32);

impl MemoryLimitMib {
    /// Validate one bounded memory limit for a preview container.
    ///
    /// # Errors
    ///
    /// Returns an error for values below 64 MiB or above 64 GiB.
    pub fn new(value: u32) -> Result<Self, PreviewRuntimeError> {
        if !(MIN_MEMORY_MIB..=MAX_MEMORY_MIB).contains(&value) {
            return Err(PreviewRuntimeError::new(
                "memory_mib",
                format!("must be between {MIN_MEMORY_MIB} and {MAX_MEMORY_MIB}"),
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    fn podman_value(self) -> String {
        format!("{}m", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct CpuLimitMillis(u32);

impl CpuLimitMillis {
    /// Validate CPU capacity in thousandths of one CPU.
    ///
    /// # Errors
    ///
    /// Returns an error for values below 0.1 CPU or above 64 CPUs.
    pub fn new(value: u32) -> Result<Self, PreviewRuntimeError> {
        if !(MIN_CPU_MILLIS..=MAX_CPU_MILLIS).contains(&value) {
            return Err(PreviewRuntimeError::new(
                "cpu_millis",
                format!("must be between {MIN_CPU_MILLIS} and {MAX_CPU_MILLIS}"),
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    fn podman_value(self) -> String {
        let whole = self.0 / 1_000;
        let fraction = self.0 % 1_000;
        if fraction == 0 {
            return whole.to_string();
        }
        let mut value = format!("{whole}.{fraction:03}");
        while value.ends_with('0') {
            value.pop();
        }
        value
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PidsLimit(u32);

impl PidsLimit {
    /// Validate one finite process-count limit.
    ///
    /// # Errors
    ///
    /// Returns an error for values below 16 or above 32768.
    pub fn new(value: u32) -> Result<Self, PreviewRuntimeError> {
        if !(MIN_PIDS..=MAX_PIDS).contains(&value) {
            return Err(PreviewRuntimeError::new(
                "pids",
                format!("must be between {MIN_PIDS} and {MAX_PIDS}"),
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct RootlessHostPort(u16);

impl RootlessHostPort {
    /// Validate one explicitly allocated unprivileged host port.
    ///
    /// # Errors
    ///
    /// Returns an error for ports below 1024.
    pub fn new(value: u16) -> Result<Self, PreviewRuntimeError> {
        if value < MIN_ROOTLESS_HOST_PORT {
            return Err(PreviewRuntimeError::new(
                "host_port",
                format!("must be between {MIN_ROOTLESS_HOST_PORT} and 65535"),
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PreviewRuntimeLimits {
    memory: MemoryLimitMib,
    cpus: CpuLimitMillis,
    pids: PidsLimit,
}

impl PreviewRuntimeLimits {
    #[must_use]
    pub const fn new(memory: MemoryLimitMib, cpus: CpuLimitMillis, pids: PidsLimit) -> Self {
        Self { memory, cpus, pids }
    }

    #[must_use]
    pub const fn memory(self) -> MemoryLimitMib {
        self.memory
    }

    #[must_use]
    pub const fn cpus(self) -> CpuLimitMillis {
        self.cpus
    }

    #[must_use]
    pub const fn pids(self) -> PidsLimit {
        self.pids
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct OciImageReference {
    canonical: String,
    digest: Sha256Digest,
}

impl OciImageReference {
    /// Validate a fully qualified, digest-pinned OCI image reference against artifact evidence.
    ///
    /// Mutable tags, short names, schemes, uppercase names, and digest mismatches fail closed.
    ///
    /// # Errors
    ///
    /// Returns an error unless the artifact is an OCI image and the reference ends in the artifact's
    /// exact canonical SHA-256 digest.
    pub fn parse(value: &str, artifact: &ArtifactIdentity) -> Result<Self, PreviewRuntimeError> {
        if artifact.kind != ArtifactKind::OciImage {
            return Err(PreviewRuntimeError::new(
                "image",
                "artifact kind must be oci_image",
            ));
        }
        if value.len() > MAX_IMAGE_REFERENCE_BYTES || value.chars().any(char::is_whitespace) {
            return Err(PreviewRuntimeError::new(
                "image",
                "must be a bounded OCI reference without whitespace",
            ));
        }
        let Some((name, digest_text)) = value.split_once('@') else {
            return Err(PreviewRuntimeError::new(
                "image",
                "must include one canonical @sha256 digest",
            ));
        };
        if digest_text.contains('@') || !valid_image_name(name) {
            return Err(PreviewRuntimeError::new(
                "image",
                "must use a fully qualified lowercase registry/repository name without a tag",
            ));
        }
        let digest = Sha256Digest::parse(digest_text)
            .map_err(|error| PreviewRuntimeError::new("image", error.to_string()))?;
        if digest != artifact.digest {
            return Err(PreviewRuntimeError::new(
                "image",
                "digest does not match the immutable artifact identity",
            ));
        }
        Ok(Self {
            canonical: format!("{name}@{}", digest.as_str()),
            digest,
        })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.canonical
    }

    #[must_use]
    pub const fn digest(&self) -> &Sha256Digest {
        &self.digest
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PreviewContainerName(String);

impl PreviewContainerName {
    #[must_use]
    pub fn derive(
        installation_id: &InstallationId,
        slot: &LeaseId,
        generation: PreviewGeneration,
    ) -> Self {
        let prefix = normalized_slot_prefix(slot);
        let hash = identity_hash(installation_id, slot, generation);
        Self(format!(
            "smol-preview-{prefix}-g{}-{hash:016x}",
            generation.get()
        ))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreviewContainerSpec {
    installation_id: InstallationId,
    request: PreviewRequest,
    generation: PreviewGeneration,
    image: OciImageReference,
    limits: PreviewRuntimeLimits,
    host_port: RootlessHostPort,
    container_name: PreviewContainerName,
    expected_labels: BTreeMap<String, String>,
}

impl PreviewContainerSpec {
    /// Bind one preview generation to immutable image evidence and bounded runtime policy.
    ///
    /// # Errors
    ///
    /// Returns an error when the image does not match the request's artifact identity.
    pub fn new(
        installation_id: InstallationId,
        request: PreviewRequest,
        generation: PreviewGeneration,
        image: OciImageReference,
        limits: PreviewRuntimeLimits,
        host_port: RootlessHostPort,
    ) -> Result<Self, PreviewRuntimeError> {
        if request.artifact.kind != ArtifactKind::OciImage
            || image.digest != request.artifact.digest
        {
            return Err(PreviewRuntimeError::new(
                "image",
                "preview image evidence does not match the requested artifact",
            ));
        }
        let container_name =
            PreviewContainerName::derive(&installation_id, &request.slot, generation);
        let expected_labels = expected_labels(&installation_id, &request, generation);
        Ok(Self {
            installation_id,
            request,
            generation,
            image,
            limits,
            host_port,
            container_name,
            expected_labels,
        })
    }

    #[must_use]
    pub const fn installation_id(&self) -> &InstallationId {
        &self.installation_id
    }

    #[must_use]
    pub const fn request(&self) -> &PreviewRequest {
        &self.request
    }

    #[must_use]
    pub const fn generation(&self) -> PreviewGeneration {
        self.generation
    }

    #[must_use]
    pub const fn image(&self) -> &OciImageReference {
        &self.image
    }

    #[must_use]
    pub const fn limits(&self) -> PreviewRuntimeLimits {
        self.limits
    }

    #[must_use]
    pub const fn host_port(&self) -> RootlessHostPort {
        self.host_port
    }

    #[must_use]
    pub const fn container_name(&self) -> &PreviewContainerName {
        &self.container_name
    }

    #[must_use]
    pub const fn expected_labels(&self) -> &BTreeMap<String, String> {
        &self.expected_labels
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewPodmanOperation {
    Create,
    Start,
    Inspect,
    Stop,
    Remove,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "rollback", rename_all = "snake_case")]
pub enum PreviewCommandEffect {
    ReadOnly,
    Mutation(RollbackClass),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreviewPodmanCommand {
    id: String,
    lane: ExecutionLane,
    operation: PreviewPodmanOperation,
    effect: PreviewCommandEffect,
    requires_matching_labels: bool,
    spec: CommandSpec,
}

impl PreviewPodmanCommand {
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub const fn lane(&self) -> ExecutionLane {
        self.lane
    }

    #[must_use]
    pub const fn operation(&self) -> PreviewPodmanOperation {
        self.operation
    }

    #[must_use]
    pub const fn effect(&self) -> PreviewCommandEffect {
        self.effect
    }

    #[must_use]
    pub const fn requires_matching_labels(&self) -> bool {
        self.requires_matching_labels
    }

    #[must_use]
    pub const fn spec(&self) -> &CommandSpec {
        &self.spec
    }

    #[must_use]
    pub fn required_programs(&self) -> [&Path; 2] {
        [Path::new(RUNUSER), Path::new(PODMAN)]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreviewPodmanPlan {
    container_name: PreviewContainerName,
    expected_labels: BTreeMap<String, String>,
    provision: Vec<PreviewPodmanCommand>,
    cleanup: Vec<PreviewPodmanCommand>,
}

impl PreviewPodmanPlan {
    #[must_use]
    pub fn for_container(spec: &PreviewContainerSpec, runner: &RunnerUserContext) -> Self {
        let create = command(
            spec,
            runner,
            PreviewPodmanOperation::Create,
            PreviewCommandEffect::Mutation(RollbackClass::Reversible),
            false,
            create_arguments(spec),
        );
        let start = command(
            spec,
            runner,
            PreviewPodmanOperation::Start,
            PreviewCommandEffect::Mutation(RollbackClass::Compensating),
            true,
            vec!["start".to_owned(), spec.container_name.as_str().to_owned()],
        );
        let inspect = command(
            spec,
            runner,
            PreviewPodmanOperation::Inspect,
            PreviewCommandEffect::ReadOnly,
            false,
            vec![
                "container".to_owned(),
                "inspect".to_owned(),
                spec.container_name.as_str().to_owned(),
            ],
        );
        let stop = command(
            spec,
            runner,
            PreviewPodmanOperation::Stop,
            PreviewCommandEffect::Mutation(RollbackClass::Compensating),
            true,
            vec![
                "stop".to_owned(),
                "--ignore".to_owned(),
                "--time".to_owned(),
                STOP_TIMEOUT_SECONDS.to_string(),
                spec.container_name.as_str().to_owned(),
            ],
        );
        let remove = command(
            spec,
            runner,
            PreviewPodmanOperation::Remove,
            PreviewCommandEffect::Mutation(RollbackClass::Irreversible),
            true,
            vec![
                "rm".to_owned(),
                "--ignore".to_owned(),
                spec.container_name.as_str().to_owned(),
            ],
        );
        Self {
            container_name: spec.container_name.clone(),
            expected_labels: spec.expected_labels.clone(),
            provision: vec![create, start, inspect],
            cleanup: vec![stop, remove],
        }
    }

    #[must_use]
    pub const fn container_name(&self) -> &PreviewContainerName {
        &self.container_name
    }

    #[must_use]
    pub const fn expected_labels(&self) -> &BTreeMap<String, String> {
        &self.expected_labels
    }

    #[must_use]
    pub fn provision(&self) -> &[PreviewPodmanCommand] {
        &self.provision
    }

    #[must_use]
    pub fn cleanup(&self) -> &[PreviewPodmanCommand] {
        &self.cleanup
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreviewRuntimeError {
    pub field: String,
    pub problem: String,
}

impl PreviewRuntimeError {
    fn new(field: impl Into<String>, problem: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            problem: problem.into(),
        }
    }
}

impl fmt::Display for PreviewRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", self.field, self.problem)
    }
}

impl std::error::Error for PreviewRuntimeError {}

fn create_arguments(spec: &PreviewContainerSpec) -> Vec<String> {
    let mut arguments = vec![
        "create".to_owned(),
        "--name".to_owned(),
        spec.container_name.as_str().to_owned(),
        "--pull".to_owned(),
        "never".to_owned(),
        "--cap-drop".to_owned(),
        "all".to_owned(),
        "--security-opt".to_owned(),
        "no-new-privileges".to_owned(),
        "--read-only".to_owned(),
        "--read-only-tmpfs=false".to_owned(),
        "--memory".to_owned(),
        spec.limits.memory.podman_value(),
        "--cpus".to_owned(),
        spec.limits.cpus.podman_value(),
        "--pids-limit".to_owned(),
        spec.limits.pids.get().to_string(),
        "--network".to_owned(),
        "private".to_owned(),
        "--publish".to_owned(),
        format!(
            "127.0.0.1:{}:{}/tcp",
            spec.host_port.get(),
            spec.request.port.get()
        ),
        "--tmpfs".to_owned(),
        "/tmp:rw,nosuid,nodev,noexec,size=64m".to_owned(),
        "--log-driver".to_owned(),
        "k8s-file".to_owned(),
        "--log-opt".to_owned(),
        "max-size=10mb".to_owned(),
    ];
    for (key, value) in &spec.expected_labels {
        arguments.push("--label".to_owned());
        arguments.push(format!("{key}={value}"));
    }
    arguments.push("--".to_owned());
    arguments.push(spec.image.as_str().to_owned());
    arguments
}

fn command(
    spec: &PreviewContainerSpec,
    runner: &RunnerUserContext,
    operation: PreviewPodmanOperation,
    effect: PreviewCommandEffect,
    requires_matching_labels: bool,
    arguments: Vec<String>,
) -> PreviewPodmanCommand {
    PreviewPodmanCommand {
        id: format!(
            "preview.{}.g{}.{}",
            spec.request.slot.as_str(),
            spec.generation.get(),
            operation_name(operation)
        ),
        lane: ExecutionLane::RunnerUser,
        operation,
        effect,
        requires_matching_labels,
        spec: runner_user_podman_spec(runner, arguments),
    }
}

fn runner_user_podman_spec(
    runner: &RunnerUserContext,
    arguments: impl IntoIterator<Item = String>,
) -> CommandSpec {
    let mut spec = CommandSpec::new(RUNUSER)
        .argument("--user")
        .argument(runner.username().as_str())
        .argument("--")
        .argument(PODMAN)
        .environment("HOME", runner.home())
        .environment("USER", runner.username().as_str())
        .environment("LOGNAME", runner.username().as_str())
        .environment("XDG_RUNTIME_DIR", runner.runtime_directory());
    for argument in arguments {
        spec = spec.argument(argument);
    }
    spec
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

fn expected_labels(
    installation_id: &InstallationId,
    request: &PreviewRequest,
    generation: PreviewGeneration,
) -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "io.smolrunner.artifact.commit".to_owned(),
            request.artifact.commit.as_str().to_owned(),
        ),
        (
            "io.smolrunner.artifact.digest".to_owned(),
            request.artifact.digest.as_str().to_owned(),
        ),
        (
            "io.smolrunner.artifact.repository".to_owned(),
            request.artifact.repository.as_str().to_owned(),
        ),
        (
            "io.smolrunner.installation".to_owned(),
            installation_id.as_str().to_owned(),
        ),
        (
            "io.smolrunner.lease".to_owned(),
            request.slot.as_str().to_owned(),
        ),
        (
            "io.smolrunner.preview.generation".to_owned(),
            generation.get().to_string(),
        ),
        (
            "io.smolrunner.schema".to_owned(),
            PREVIEW_LABEL_SCHEMA_VERSION.to_string(),
        ),
    ])
}

fn normalized_slot_prefix(slot: &LeaseId) -> String {
    slot.as_str()
        .bytes()
        .take(CONTAINER_NAME_SLOT_PREFIX_BYTES)
        .map(|byte| {
            if byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' {
                char::from(byte)
            } else {
                '-'
            }
        })
        .collect()
}

fn identity_hash(
    installation_id: &InstallationId,
    slot: &LeaseId,
    generation: PreviewGeneration,
) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    hash = fnv1a(hash, installation_id.as_str().as_bytes());
    hash = fnv1a(hash, &[0]);
    hash = fnv1a(hash, slot.as_str().as_bytes());
    hash = fnv1a(hash, &[0]);
    fnv1a(hash, generation.get().to_string().as_bytes())
}

fn fnv1a(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn valid_image_name(name: &str) -> bool {
    if name.is_empty()
        || name.starts_with('-')
        || name.contains("://")
        || !name.is_ascii()
        || name.bytes().any(|byte| byte.is_ascii_uppercase())
    {
        return false;
    }
    let components = name.split('/').collect::<Vec<_>>();
    if components.len() < 2
        || components
            .iter()
            .any(|component| !valid_image_component(component))
    {
        return false;
    }
    let registry = components[0];
    if registry != "localhost" && !registry.contains('.') && !registry.contains(':') {
        return false;
    }
    if let Some((host, port)) = registry.rsplit_once(':')
        && (host.is_empty()
            || port.is_empty()
            || !port.bytes().all(|byte| byte.is_ascii_digit())
            || !port.parse::<u16>().is_ok_and(|value| value != 0))
    {
        return false;
    }
    !components[1..]
        .iter()
        .any(|component| component.contains(':'))
}

fn valid_image_component(component: &str) -> bool {
    let Some(first) = component.as_bytes().first() else {
        return false;
    };
    let Some(last) = component.as_bytes().last() else {
        return false;
    };
    first.is_ascii_alphanumeric()
        && last.is_ascii_alphanumeric()
        && component.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'_' | b'-' | b':')
        })
}

#[cfg(test)]
mod tests {
    use crate::artifact::{ArtifactIdentity, ArtifactKind, CommitId, RepositoryRef, Sha256Digest};
    use crate::lane_command::{LinuxAccountName, RunnerUserContext};
    use crate::lease::LeaseId;
    use crate::preview::{PreviewGeneration, PreviewPort, PreviewRequest, PreviewTtl};
    use crate::state::InstallationId;

    use super::{
        CpuLimitMillis, MemoryLimitMib, OciImageReference, PidsLimit, PreviewCommandEffect,
        PreviewContainerName, PreviewContainerSpec, PreviewPodmanOperation, PreviewPodmanPlan,
        PreviewRuntimeLimits, RootlessHostPort,
    };

    fn make_artifact(kind: ArtifactKind, digest_byte: &str) -> ArtifactIdentity {
        ArtifactIdentity::new(
            RepositoryRef::parse("example/project").expect("repository"),
            CommitId::parse(&"1a".repeat(20)).expect("commit"),
            kind,
            Sha256Digest::parse(&format!("sha256:{}", digest_byte.repeat(32))).expect("digest"),
        )
    }

    fn request(artifact: ArtifactIdentity) -> PreviewRequest {
        PreviewRequest::new(
            LeaseId::parse("pr-42").expect("slot"),
            artifact,
            PreviewPort::new(3000).expect("container port"),
            PreviewTtl::new(3600).expect("TTL"),
            None,
        )
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

    fn container_spec() -> PreviewContainerSpec {
        let artifact = make_artifact(ArtifactKind::OciImage, "ab");
        let image = OciImageReference::parse(
            &format!("registry.example.com/team/app@sha256:{}", "ab".repeat(32)),
            &artifact,
        )
        .expect("image reference");
        PreviewContainerSpec::new(
            InstallationId::parse("installation-001").expect("installation ID"),
            request(artifact),
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

    #[test]
    fn runtime_limits_are_bounded_and_cpu_values_are_canonical() {
        assert!(MemoryLimitMib::new(63).is_err());
        assert!(CpuLimitMillis::new(99).is_err());
        assert!(PidsLimit::new(0).is_err());
        assert!(RootlessHostPort::new(1023).is_err());
        assert_eq!(CpuLimitMillis::new(100).expect("CPU").podman_value(), "0.1");
        assert_eq!(
            CpuLimitMillis::new(1250).expect("CPU").podman_value(),
            "1.25"
        );
        assert_eq!(CpuLimitMillis::new(2000).expect("CPU").podman_value(), "2");
    }

    #[test]
    fn image_references_require_full_digest_bound_evidence() {
        let artifact = make_artifact(ArtifactKind::OciImage, "ab");
        let good = format!("ghcr.io/example/project@sha256:{}", "ab".repeat(32));
        assert!(OciImageReference::parse(&good, &artifact).is_ok());
        assert!(
            OciImageReference::parse(
                &format!("project:latest@sha256:{}", "ab".repeat(32)),
                &artifact,
            )
            .is_err()
        );
        assert!(
            OciImageReference::parse(
                &format!("ghcr.io/example/project:latest@sha256:{}", "ab".repeat(32)),
                &artifact,
            )
            .is_err()
        );
        assert!(
            OciImageReference::parse(
                &format!("ghcr.io/example/project@sha256:{}", "cd".repeat(32)),
                &artifact,
            )
            .is_err()
        );
        assert!(
            OciImageReference::parse(&good, &make_artifact(ArtifactKind::StaticArchive, "ab"),)
                .is_err()
        );
        assert!(
            OciImageReference::parse(
                &format!(
                    "registry.example.com:70000/team/app@sha256:{}",
                    "ab".repeat(32)
                ),
                &artifact,
            )
            .is_err()
        );
    }

    #[test]
    fn container_names_are_deterministic_bounded_locators() {
        let name = PreviewContainerName::derive(
            &InstallationId::parse("installation-001").expect("installation ID"),
            &LeaseId::parse("pr-42").expect("slot"),
            PreviewGeneration::new(7).expect("generation"),
        );
        assert_eq!(name.as_str(), "smol-preview-pr-42-g7-c4fe5732676e8585");
        assert!(name.as_str().len() <= 63);
    }

    #[test]
    fn create_command_has_reviewed_shell_free_argv() {
        let spec = container_spec();
        let plan = PreviewPodmanPlan::for_container(&spec, &runner());
        let create = &plan.provision()[0];
        let digest = format!("sha256:{}", "ab".repeat(32));
        let commit = "1a".repeat(20);
        assert_eq!(create.operation(), PreviewPodmanOperation::Create);
        assert_eq!(create.lane(), crate::journal::ExecutionLane::RunnerUser);
        assert_eq!(
            create.spec().displayed_argv(),
            vec![
                "/usr/sbin/runuser",
                "--user",
                "project-runner",
                "--",
                "/usr/bin/podman",
                "create",
                "--name",
                "smol-preview-pr-42-g7-c4fe5732676e8585",
                "--pull",
                "never",
                "--cap-drop",
                "all",
                "--security-opt",
                "no-new-privileges",
                "--read-only",
                "--read-only-tmpfs=false",
                "--memory",
                "512m",
                "--cpus",
                "1.5",
                "--pids-limit",
                "256",
                "--network",
                "private",
                "--publish",
                "127.0.0.1:42000:3000/tcp",
                "--tmpfs",
                "/tmp:rw,nosuid,nodev,noexec,size=64m",
                "--log-driver",
                "k8s-file",
                "--log-opt",
                "max-size=10mb",
                "--label",
                &format!("io.smolrunner.artifact.commit={commit}"),
                "--label",
                &format!("io.smolrunner.artifact.digest={digest}"),
                "--label",
                "io.smolrunner.artifact.repository=example/project",
                "--label",
                "io.smolrunner.installation=installation-001",
                "--label",
                "io.smolrunner.lease=pr-42",
                "--label",
                "io.smolrunner.preview.generation=7",
                "--label",
                "io.smolrunner.schema=1",
                "--",
                &format!("registry.example.com/team/app@{digest}"),
            ]
        );
        assert_eq!(
            create
                .spec()
                .environment
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            ["HOME", "LOGNAME", "USER", "XDG_RUNTIME_DIR"]
        );
    }

    #[test]
    fn cleanup_is_explicit_and_requires_ownership_evidence() {
        let plan = PreviewPodmanPlan::for_container(&container_spec(), &runner());
        assert_eq!(plan.provision().len(), 3);
        assert!(plan.provision()[1].requires_matching_labels());
        assert_eq!(plan.cleanup().len(), 2);
        assert_eq!(plan.cleanup()[0].operation(), PreviewPodmanOperation::Stop);
        assert_eq!(
            plan.cleanup()[1].operation(),
            PreviewPodmanOperation::Remove
        );
        assert!(
            plan.cleanup()
                .iter()
                .all(|command| command.requires_matching_labels())
        );
        assert_eq!(
            plan.cleanup()[1].effect(),
            PreviewCommandEffect::Mutation(crate::journal::RollbackClass::Irreversible)
        );
        assert_eq!(plan.expected_labels().len(), 7);
    }
}
