use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::artifact::Sha256Digest;
use crate::descriptor_bound_launcher::{
    REDACTED, ReviewedFilesystemIdentity, ReviewedLaunchCredentials, ReviewedLaunchValue,
    ReviewedLinuxLaunchPlan,
};
use crate::lane_command::RunnerUserContext;
use crate::renderprove_verification::{
    RenderproveReviewNetworkPolicy, RenderproveSourceIdentity, RenderproveVerificationRequest,
    RenderproveWorkerImageIdentity,
};

pub const RENDERPROVE_NATIVE_PROBE_SCHEMA_VERSION: u8 = 1;
pub const RENDERPROVE_PROTECTED_MOUNT_ROOT: &str = "/run/smolrunner/renderprove-mounts";

const PODMAN: &str = "/usr/bin/podman";
const CLEAN_PATH: &str = "/usr/local/bin:/usr/bin:/bin";
const PROJECT_CONTAINER_PATH: &str = "/workspace/project";
const WORKER_IDENTITY_SCRIPT: &str = "/opt/renderprove/scripts/worker-identity.mjs";
const MAX_IMAGE_REFERENCE_BYTES: usize = 512;
#[cfg(test)]
const MAX_ALIAS_COMPONENT_BYTES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderproveNativeProbeOperation {
    InspectWorkerImage,
    RecordWorkerIdentity,
    ReviewProject,
}

#[derive(Clone, PartialEq, Eq)]
pub struct RenderproveProtectedMountReceipt {
    schema_version: u8,
    source: RenderproveSourceIdentity,
    project_alias: PathBuf,
    project_identity: ReviewedFilesystemIdentity,
    evidence_alias: PathBuf,
    evidence_identity: ReviewedFilesystemIdentity,
    evidence_directory: PathBuf,
}

impl RenderproveProtectedMountReceipt {
    /// Bind one protected project alias and one separate protected evidence alias.
    ///
    /// This constructor is crate-private because only a later descriptor-relative mount producer may
    /// issue the receipt. The aliases must be direct children of SmolRunner's fixed root-owned runtime
    /// mount directory. The producer must retain the mount lease until every planned Podman process
    /// and evidence write has completed.
    #[cfg(test)]
    pub(crate) fn new(
        source: RenderproveSourceIdentity,
        project_alias: impl Into<PathBuf>,
        project_identity: ReviewedFilesystemIdentity,
        evidence_alias: impl Into<PathBuf>,
        evidence_identity: ReviewedFilesystemIdentity,
        evidence_directory: impl Into<PathBuf>,
    ) -> Result<Self, RenderproveNativeProbeError> {
        let project_alias = validate_protected_alias("mount.project", project_alias.into())?;
        let evidence_alias = validate_protected_alias("mount.evidence", evidence_alias.into())?;
        if project_alias == evidence_alias {
            return Err(RenderproveNativeProbeError::new(
                "mount.aliases",
                "project and evidence aliases must be distinct protected mounts",
            ));
        }
        let evidence_directory = evidence_directory.into();
        if !valid_relative_path(&evidence_directory) {
            return Err(RenderproveNativeProbeError::new(
                "mount.evidence_directory",
                "evidence directory must be a non-empty normalized project-relative path",
            ));
        }
        Ok(Self {
            schema_version: RENDERPROVE_NATIVE_PROBE_SCHEMA_VERSION,
            source,
            project_alias,
            project_identity,
            evidence_alias,
            evidence_identity,
            evidence_directory,
        })
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn source(&self) -> &RenderproveSourceIdentity {
        &self.source
    }

    #[must_use]
    pub fn evidence_directory(&self) -> &Path {
        &self.evidence_directory
    }
}

impl fmt::Debug for RenderproveProtectedMountReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderproveProtectedMountReceipt")
            .field("schema_version", &self.schema_version)
            .field("source", &self.source)
            .field("project_alias", &"<private protected mount>")
            .field("project_identity", &"<private exact filesystem identity>")
            .field("evidence_alias", &"<private protected mount>")
            .field("evidence_identity", &"<private exact filesystem identity>")
            .field("evidence_directory", &self.evidence_directory)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct RenderproveNativeProbeContext {
    runner: RunnerUserContext,
    podman_identity: ReviewedFilesystemIdentity,
    mounts: RenderproveProtectedMountReceipt,
}

impl RenderproveNativeProbeContext {
    /// Combine reviewed runner identity, exact Podman identity, and protected mount evidence.
    #[must_use]
    pub const fn new(
        runner: RunnerUserContext,
        podman_identity: ReviewedFilesystemIdentity,
        mounts: RenderproveProtectedMountReceipt,
    ) -> Self {
        Self {
            runner,
            podman_identity,
            mounts,
        }
    }

    #[must_use]
    pub const fn runner(&self) -> &RunnerUserContext {
        &self.runner
    }

    #[must_use]
    pub const fn mounts(&self) -> &RenderproveProtectedMountReceipt {
        &self.mounts
    }
}

impl fmt::Debug for RenderproveNativeProbeContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderproveNativeProbeContext")
            .field("runner", &"<reviewed runner identity>")
            .field("podman_identity", &"<private exact executable identity>")
            .field("mounts", &self.mounts)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct RenderproveNativeProbeCommand {
    id: String,
    operation: RenderproveNativeProbeOperation,
    launch: ReviewedLinuxLaunchPlan,
    displayed_argv: Vec<String>,
}

impl RenderproveNativeProbeCommand {
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub const fn operation(&self) -> RenderproveNativeProbeOperation {
        self.operation
    }

    #[must_use]
    pub const fn launch(&self) -> &ReviewedLinuxLaunchPlan {
        &self.launch
    }

    #[must_use]
    pub fn displayed_argv(&self) -> &[String] {
        &self.displayed_argv
    }

    #[must_use]
    pub fn host_executable(&self) -> &'static Path {
        Path::new(PODMAN)
    }
}

impl fmt::Debug for RenderproveNativeProbeCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderproveNativeProbeCommand")
            .field("id", &self.id)
            .field("operation", &self.operation)
            .field("displayed_argv", &self.displayed_argv)
            .field("launch", &"<retained exact descriptor-bound launch plan>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct RenderproveNativeProbePlan {
    schema_version: u8,
    request: RenderproveVerificationRequest,
    canonical_worker_image: String,
    context: RenderproveNativeProbeContext,
    inspect_worker_image: RenderproveNativeProbeCommand,
}

impl RenderproveNativeProbePlan {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn request(&self) -> &RenderproveVerificationRequest {
        &self.request
    }

    #[must_use]
    pub fn canonical_worker_image(&self) -> &str {
        &self.canonical_worker_image
    }

    #[must_use]
    pub const fn inspect_worker_image(&self) -> &RenderproveNativeProbeCommand {
        &self.inspect_worker_image
    }
}

impl fmt::Debug for RenderproveNativeProbePlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderproveNativeProbePlan")
            .field("schema_version", &self.schema_version)
            .field("request", &"<retained exact Renderprove request>")
            .field("canonical_worker_image", &self.canonical_worker_image)
            .field("context", &self.context)
            .field("inspect_worker_image", &self.inspect_worker_image)
            .finish()
    }
}

/// Plan the first direct-ELF step of one native Renderprove probe.
///
/// The plan deliberately contains no Bash, `runuser`, `env`, npm, host Node, Renderprove checkout,
/// image build, or mutable image tag. The sole host executable is exact descriptor-bound Podman,
/// dropped directly to the reviewed runner UID/GID. Project and evidence paths come only from a
/// protected mount receipt issued by a later descriptor-relative mount producer.
///
/// # Errors
///
/// Returns an error for deployed-origin review, mutable or mismatched worker-image references,
/// source/evidence mismatch, or an invalid descriptor-bound launch plan.
pub fn plan_renderprove_native_probe(
    request: RenderproveVerificationRequest,
    context: RenderproveNativeProbeContext,
) -> Result<RenderproveNativeProbePlan, RenderproveNativeProbeError> {
    if !matches!(
        request.network(),
        RenderproveReviewNetworkPolicy::LoopbackOnly
    ) {
        return Err(RenderproveNativeProbeError::new(
            "request.network",
            "native probe planning currently supports loopback_only review",
        ));
    }
    if request.source() != context.mounts.source() {
        return Err(RenderproveNativeProbeError::new(
            "mount.source",
            "protected project mount does not match the exact requested source",
        ));
    }
    if request.evidence().directory() != context.mounts.evidence_directory() {
        return Err(RenderproveNativeProbeError::new(
            "mount.evidence_directory",
            "protected evidence mount does not match the requested evidence directory",
        ));
    }
    let canonical_worker_image = canonical_worker_reference(request.worker_image())?;
    let inspect_worker_image = make_command(
        "renderprove.probe.worker.inspect",
        RenderproveNativeProbeOperation::InspectWorkerImage,
        &context,
        vec![
            ReviewedLaunchValue::plain("image"),
            ReviewedLaunchValue::plain("inspect"),
            ReviewedLaunchValue::plain("--format"),
            ReviewedLaunchValue::plain("{{.Id}}"),
            ReviewedLaunchValue::plain(canonical_worker_image.clone()),
        ],
    )?;
    Ok(RenderproveNativeProbePlan {
        schema_version: RENDERPROVE_NATIVE_PROBE_SCHEMA_VERSION,
        request,
        canonical_worker_image,
        context,
        inspect_worker_image,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderproveWorkerImageObservation {
    image_id: Sha256Digest,
    canonical_worker_image: String,
}

impl RenderproveWorkerImageObservation {
    #[must_use]
    pub const fn image_id(&self) -> &Sha256Digest {
        &self.image_id
    }

    #[must_use]
    pub fn canonical_worker_image(&self) -> &str {
        &self.canonical_worker_image
    }
}

/// Parse one bounded exact image ID from the planned `podman image inspect` stdout.
///
/// # Errors
///
/// Returns an error unless stdout contains exactly one canonical SHA-256 value and at most one final
/// line feed.
pub fn parse_renderprove_worker_image_observation(
    plan: &RenderproveNativeProbePlan,
    stdout: &str,
) -> Result<RenderproveWorkerImageObservation, RenderproveNativeProbeError> {
    let value = stdout.strip_suffix('\n').unwrap_or(stdout);
    if value.is_empty() || value.contains(['\n', '\r']) {
        return Err(RenderproveNativeProbeError::new(
            "worker_image.stdout",
            "worker image inspection must return exactly one canonical SHA-256 image ID",
        ));
    }
    let image_id = Sha256Digest::parse(value).map_err(|_| {
        RenderproveNativeProbeError::new(
            "worker_image.stdout",
            "worker image inspection returned an invalid image ID",
        )
    })?;
    Ok(RenderproveWorkerImageObservation {
        image_id,
        canonical_worker_image: plan.canonical_worker_image.clone(),
    })
}

#[derive(Clone, PartialEq, Eq)]
pub struct RenderproveNativeProbeRunPlan {
    schema_version: u8,
    worker_image: RenderproveWorkerImageObservation,
    record_worker_identity: RenderproveNativeProbeCommand,
    review_project: RenderproveNativeProbeCommand,
    worker_identity_output: PathBuf,
    review_stdout_output: PathBuf,
}

impl RenderproveNativeProbeRunPlan {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn worker_image(&self) -> &RenderproveWorkerImageObservation {
        &self.worker_image
    }

    #[must_use]
    pub const fn record_worker_identity(&self) -> &RenderproveNativeProbeCommand {
        &self.record_worker_identity
    }

    #[must_use]
    pub const fn review_project(&self) -> &RenderproveNativeProbeCommand {
        &self.review_project
    }

    #[must_use]
    pub fn worker_identity_output(&self) -> &Path {
        &self.worker_identity_output
    }

    #[must_use]
    pub fn review_stdout_output(&self) -> &Path {
        &self.review_stdout_output
    }
}

impl fmt::Debug for RenderproveNativeProbeRunPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderproveNativeProbeRunPlan")
            .field("schema_version", &self.schema_version)
            .field("worker_image", &self.worker_image)
            .field("record_worker_identity", &self.record_worker_identity)
            .field("review_project", &self.review_project)
            .field("worker_identity_output", &self.worker_identity_output)
            .field("review_stdout_output", &self.review_stdout_output)
            .finish()
    }
}

/// Plan the two immutable worker-container runs after exact local image inspection.
///
/// The worker identity and project review are separate Podman processes. Both select the exact
/// digest-pinned worker image and use the same fixed rootless limits. The review gets two protected
/// mounts: the disposable project and a separate writable evidence directory. Raw stdout remains a
/// later executor concern; the plan only declares fixed relative evidence sink names.
///
/// # Errors
///
/// Returns an error if either descriptor-bound Podman launch plan cannot be constructed.
pub fn plan_renderprove_native_probe_runs(
    plan: &RenderproveNativeProbePlan,
    worker_image: RenderproveWorkerImageObservation,
) -> Result<RenderproveNativeProbeRunPlan, RenderproveNativeProbeError> {
    if worker_image.canonical_worker_image != plan.canonical_worker_image {
        return Err(RenderproveNativeProbeError::new(
            "worker_image.observation",
            "worker image observation does not match the exact inspected image reference",
        ));
    }
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
        ReviewedLaunchValue::secret(format!(
            "{}:{PROJECT_CONTAINER_PATH}:ro",
            plan.context.mounts.project_alias.display()
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

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct RenderproveNativeProbeError {
    field: &'static str,
    message: String,
}

impl RenderproveNativeProbeError {
    fn new(field: &'static str, message: impl Into<String>) -> Self {
        Self {
            field,
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn field(&self) -> &'static str {
        self.field
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Debug for RenderproveNativeProbeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderproveNativeProbeError")
            .field("field", &self.field)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for RenderproveNativeProbeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for RenderproveNativeProbeError {}

fn make_command(
    id: &str,
    operation: RenderproveNativeProbeOperation,
    context: &RenderproveNativeProbeContext,
    arguments: Vec<ReviewedLaunchValue>,
) -> Result<RenderproveNativeProbeCommand, RenderproveNativeProbeError> {
    let displayed_argv = std::iter::once(PODMAN.to_owned())
        .chain(arguments.iter().map(displayed_value))
        .collect::<Vec<_>>();
    let launch = ReviewedLinuxLaunchPlan::new(
        id,
        PODMAN,
        context.podman_identity.clone(),
        context.mounts.project_alias.clone(),
        context.mounts.project_identity.clone(),
        arguments,
        runner_environment(&context.runner),
        ReviewedLaunchCredentials::DropPrivileges {
            launcher_uid: 0,
            launcher_gid: 0,
            target_uid: context.runner.uid(),
            target_gid: context.runner.primary_gid(),
        },
    )
    .map_err(|error| RenderproveNativeProbeError::new("launch", error.message()))?;
    Ok(RenderproveNativeProbeCommand {
        id: id.to_owned(),
        operation,
        launch,
        displayed_argv,
    })
}

fn displayed_value(value: &ReviewedLaunchValue) -> String {
    match value {
        ReviewedLaunchValue::Plain(value) => value.clone(),
        ReviewedLaunchValue::Secret(_) => REDACTED.to_owned(),
    }
}

fn runner_environment(runner: &RunnerUserContext) -> BTreeMap<String, ReviewedLaunchValue> {
    BTreeMap::from([
        (
            "HOME".to_owned(),
            ReviewedLaunchValue::secret(runner.home()),
        ),
        (
            "LOGNAME".to_owned(),
            ReviewedLaunchValue::plain(runner.username().as_str()),
        ),
        ("PATH".to_owned(), ReviewedLaunchValue::plain(CLEAN_PATH)),
        (
            "USER".to_owned(),
            ReviewedLaunchValue::plain(runner.username().as_str()),
        ),
        (
            "XDG_RUNTIME_DIR".to_owned(),
            ReviewedLaunchValue::secret(runner.runtime_directory()),
        ),
    ])
}

fn common_run_arguments() -> Vec<ReviewedLaunchValue> {
    [
        "run",
        "--pull=never",
        "--rm",
        "--init",
        "--userns=keep-id",
        "--read-only",
        "--shm-size=1g",
        "--network=none",
        "--cap-drop=all",
        "--security-opt=no-new-privileges",
        "--pids-limit=768",
        "--memory=2048m",
        "--cpus=2",
        "--tmpfs=/tmp:rw,nosuid,nodev,size=1g",
    ]
    .into_iter()
    .map(ReviewedLaunchValue::plain)
    .collect()
}

fn canonical_worker_reference(
    worker: &RenderproveWorkerImageIdentity,
) -> Result<String, RenderproveNativeProbeError> {
    let value = worker.reference();
    if value.len() > MAX_IMAGE_REFERENCE_BYTES || value.chars().any(char::is_whitespace) {
        return Err(RenderproveNativeProbeError::new(
            "worker_image.reference",
            "worker image must be one bounded digest-pinned OCI reference",
        ));
    }
    let Some((name, digest_text)) = value.split_once('@') else {
        return Err(RenderproveNativeProbeError::new(
            "worker_image.reference",
            "worker image must include its exact @sha256 digest",
        ));
    };
    if digest_text.contains('@') || !valid_image_name(name) {
        return Err(RenderproveNativeProbeError::new(
            "worker_image.reference",
            "worker image must use a fully qualified lowercase registry/repository name without a tag",
        ));
    }
    let digest = Sha256Digest::parse(digest_text).map_err(|_| {
        RenderproveNativeProbeError::new(
            "worker_image.reference",
            "worker image reference contains an invalid digest",
        )
    })?;
    if &digest != worker.digest() {
        return Err(RenderproveNativeProbeError::new(
            "worker_image.reference",
            "worker image reference digest does not match the exact worker identity",
        ));
    }
    Ok(format!("{name}@{}", digest.as_str()))
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
fn validate_protected_alias(
    field: &'static str,
    path: PathBuf,
) -> Result<PathBuf, RenderproveNativeProbeError> {
    let root = Path::new(RENDERPROVE_PROTECTED_MOUNT_ROOT);
    let Some(parent) = path.parent() else {
        return Err(RenderproveNativeProbeError::new(
            field,
            "protected mount alias must be a direct child of the fixed runtime mount root",
        ));
    };
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return Err(RenderproveNativeProbeError::new(
            field,
            "protected mount alias must use one bounded UTF-8 component",
        ));
    };
    if parent != root
        || name.is_empty()
        || name.len() > MAX_ALIAS_COMPONENT_BYTES
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(RenderproveNativeProbeError::new(
            field,
            "protected mount alias must be one lowercase direct child of the fixed runtime mount root",
        ));
    }
    Ok(path)
}

#[cfg(test)]
fn valid_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(value) if !value.is_empty()))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::artifact::{ArtifactIdentity, ArtifactKind, CommitId, RepositoryRef, Sha256Digest};
    use crate::descriptor_bound_launcher::{
        REDACTED, ReviewedFilesystemIdentity, ReviewedLaunchCredentials,
    };
    use crate::lane_command::{LinuxAccountName, RunnerUserContext};
    use crate::renderprove_verification::{
        RenderproveEvidencePolicy, RenderproveReviewNetworkPolicy, RenderproveSourceIdentity,
        RenderproveVerificationRequest, RenderproveWorkerImageIdentity,
    };

    use super::{
        PODMAN, RENDERPROVE_PROTECTED_MOUNT_ROOT, RenderproveNativeProbeContext,
        RenderproveNativeProbeOperation, RenderproveProtectedMountReceipt,
        parse_renderprove_worker_image_observation, plan_renderprove_native_probe,
        plan_renderprove_native_probe_runs,
    };

    fn digest(byte: &str) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", byte.repeat(32))).expect("digest")
    }

    fn source() -> RenderproveSourceIdentity {
        RenderproveSourceIdentity::new(
            RepositoryRef::parse("example/project").expect("repository"),
            CommitId::parse(&"1a".repeat(20)).expect("commit"),
        )
    }

    fn request() -> RenderproveVerificationRequest {
        let source = source();
        let project_image = ArtifactIdentity::new(
            source.repository.clone(),
            source.commit.clone(),
            ArtifactKind::OciImage,
            digest("bc"),
        );
        let worker_digest = digest("ab");
        RenderproveVerificationRequest::new(
            source,
            project_image,
            RenderproveWorkerImageIdentity::new(
                format!(
                    "registry.example.com/renderprove/worker@{}",
                    worker_digest.as_str()
                ),
                worker_digest,
            )
            .expect("worker image"),
            digest("cd"),
            RenderproveEvidencePolicy::new(".smolrunner/renderprove", 64 * 1024 * 1024)
                .expect("evidence"),
            RenderproveReviewNetworkPolicy::LoopbackOnly,
        )
        .expect("request")
    }

    fn identity(inode: u64) -> ReviewedFilesystemIdentity {
        ReviewedFilesystemIdentity::new(7, inode, 0, 0, 0o755).expect("identity")
    }

    fn context() -> RenderproveNativeProbeContext {
        let mounts = RenderproveProtectedMountReceipt::new(
            source(),
            format!("{RENDERPROVE_PROTECTED_MOUNT_ROOT}/project-001"),
            identity(11),
            format!("{RENDERPROVE_PROTECTED_MOUNT_ROOT}/evidence-001"),
            identity(12),
            ".smolrunner/renderprove",
        )
        .expect("mounts");
        RenderproveNativeProbeContext::new(
            RunnerUserContext::new(
                LinuxAccountName::parse("project-runner").expect("name"),
                1001,
                1001,
                "/var/lib/project-runner",
            )
            .expect("runner"),
            identity(21),
            mounts,
        )
    }

    #[test]
    fn plans_only_direct_descriptor_bound_podman_operations() {
        let plan = plan_renderprove_native_probe(request(), context()).expect("plan");
        assert_eq!(
            plan.inspect_worker_image().operation(),
            RenderproveNativeProbeOperation::InspectWorkerImage
        );
        assert_eq!(
            plan.inspect_worker_image().host_executable(),
            Path::new(PODMAN)
        );
        assert_eq!(
            plan.inspect_worker_image().displayed_argv(),
            [
                PODMAN,
                "image",
                "inspect",
                "--format",
                "{{.Id}}",
                &format!(
                    "registry.example.com/renderprove/worker@{}",
                    digest("ab").as_str()
                ),
            ]
        );
        assert_eq!(
            plan.inspect_worker_image().launch().credentials(),
            ReviewedLaunchCredentials::DropPrivileges {
                launcher_uid: 0,
                launcher_gid: 0,
                target_uid: 1001,
                target_gid: 1001,
            }
        );

        let observed = parse_renderprove_worker_image_observation(
            &plan,
            &format!("{}\n", digest("ef").as_str()),
        )
        .expect("observation");
        let runs = plan_renderprove_native_probe_runs(&plan, observed).expect("runs");
        for command in [runs.record_worker_identity(), runs.review_project()] {
            assert_eq!(command.host_executable(), Path::new(PODMAN));
            assert_eq!(
                command.launch().environment_keys(),
                ["HOME", "LOGNAME", "PATH", "USER", "XDG_RUNTIME_DIR"]
            );
            let debug = format!("{command:?}");
            assert!(!debug.contains("/run/smolrunner/renderprove-mounts/project-001"));
            assert!(!debug.contains("/run/smolrunner/renderprove-mounts/evidence-001"));
        }
        assert_eq!(
            runs.record_worker_identity().operation(),
            RenderproveNativeProbeOperation::RecordWorkerIdentity
        );
        assert_eq!(
            runs.review_project().operation(),
            RenderproveNativeProbeOperation::ReviewProject
        );
        assert_eq!(runs.worker_identity_output(), Path::new("worker.json"));
        assert_eq!(runs.review_stdout_output(), Path::new("review.stdout.json"));
        assert_eq!(
            runs.review_project()
                .displayed_argv()
                .iter()
                .filter(|value| value.as_str() == REDACTED)
                .count(),
            2
        );
        let argv = runs.review_project().displayed_argv().join(" ");
        for forbidden in ["/usr/sbin/runuser", "/usr/bin/env", "bash", "npm"] {
            assert!(!argv.contains(forbidden));
        }
        assert!(argv.contains("--pull=never"));
        assert!(argv.contains("--read-only"));
        assert!(argv.contains("--shm-size=1g"));
        assert!(argv.contains("--network=none"));
        assert!(!argv.contains("--ipc=host"));
        assert!(!argv.contains(",Z"));
        assert!(argv.contains("/workspace/project"));
        assert!(argv.contains(".smolrunner/renderprove"));
        assert!(!argv.contains("/workspace/project/.smolrunner/renderprove"));
    }

    #[test]
    fn mutable_or_mismatched_worker_references_fail_closed() {
        let base = request();
        for reference in [
            "registry.example.com/renderprove/worker:latest",
            &format!(
                "registry.example.com/renderprove/worker@{}",
                digest("ff").as_str()
            ),
        ] {
            let altered = RenderproveVerificationRequest::new(
                base.source().clone(),
                base.project_image().clone(),
                RenderproveWorkerImageIdentity::new(reference, digest("ab"))
                    .expect("worker identity shape"),
                base.manifest_digest().clone(),
                base.evidence().clone(),
                base.network().clone(),
            )
            .expect("request");
            assert!(plan_renderprove_native_probe(altered, context()).is_err());
        }
    }

    #[test]
    fn worker_image_observation_is_bound_to_the_inspected_reference() {
        let first_plan = plan_renderprove_native_probe(request(), context()).expect("first plan");
        let observed = parse_renderprove_worker_image_observation(
            &first_plan,
            &format!("{}\n", digest("ef").as_str()),
        )
        .expect("observation");
        assert_eq!(
            observed.canonical_worker_image(),
            first_plan.canonical_worker_image()
        );

        let base = request();
        let other_digest = digest("de");
        let other_request = RenderproveVerificationRequest::new(
            base.source().clone(),
            base.project_image().clone(),
            RenderproveWorkerImageIdentity::new(
                format!(
                    "registry.example.com/renderprove/other-worker@{}",
                    other_digest.as_str()
                ),
                other_digest,
            )
            .expect("other worker image"),
            base.manifest_digest().clone(),
            base.evidence().clone(),
            base.network().clone(),
        )
        .expect("other request");
        let other_plan =
            plan_renderprove_native_probe(other_request, context()).expect("other plan");

        assert!(plan_renderprove_native_probe_runs(&other_plan, observed).is_err());
    }

    #[test]
    fn protected_mount_source_and_evidence_must_match_request() {
        let request = request();
        let mut wrong_source = context();
        wrong_source.mounts.source.commit = CommitId::parse(&"2b".repeat(20)).expect("commit");
        assert!(plan_renderprove_native_probe(request.clone(), wrong_source).is_err());

        let wrong_evidence = RenderproveProtectedMountReceipt::new(
            source(),
            format!("{RENDERPROVE_PROTECTED_MOUNT_ROOT}/project-002"),
            identity(31),
            format!("{RENDERPROVE_PROTECTED_MOUNT_ROOT}/evidence-002"),
            identity(32),
            ".smolrunner/other",
        )
        .expect("mounts");
        let context =
            RenderproveNativeProbeContext::new(context().runner, identity(33), wrong_evidence);
        assert!(plan_renderprove_native_probe(request, context).is_err());
    }

    #[test]
    fn image_observation_and_aliases_are_strictly_bounded() {
        let plan = plan_renderprove_native_probe(request(), context()).expect("plan");
        for invalid in ["", "not-a-digest", "sha256:00\nsha256:11", "sha256:00\r"] {
            assert!(parse_renderprove_worker_image_observation(&plan, invalid).is_err());
        }
        assert!(
            RenderproveProtectedMountReceipt::new(
                source(),
                "/tmp/project",
                identity(41),
                format!("{RENDERPROVE_PROTECTED_MOUNT_ROOT}/evidence-003"),
                identity(42),
                ".smolrunner/renderprove",
            )
            .is_err()
        );
        assert!(
            RenderproveProtectedMountReceipt::new(
                source(),
                format!("{RENDERPROVE_PROTECTED_MOUNT_ROOT}/Project"),
                identity(43),
                format!("{RENDERPROVE_PROTECTED_MOUNT_ROOT}/evidence-004"),
                identity(44),
                ".smolrunner/renderprove",
            )
            .is_err()
        );
    }
}
