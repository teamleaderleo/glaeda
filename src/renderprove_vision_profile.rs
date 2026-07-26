use std::collections::BTreeSet;
use std::fmt;
use std::path::{Component, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::artifact::{CommitId, RepositoryRef, Sha256Digest};
use crate::verification_profile::{
    RepositoryCommandId, RepositoryCommandIdentity, ResourceDefaults, TestedSourceIdentity,
    TimeoutPolicy, VerificationProfileId,
};

pub const RENDERPROVE_VISION_PROFILE_SCHEMA_VERSION: u8 = 1;
pub const RENDERPROVE_REPOSITORY: &str = "teamleaderleo/renderprove";
pub const RENDERPROVE_VISION_COMMAND_ID: &str = "renderprove.vision-check.v1";
pub const RENDERPROVE_VISION_COMMAND_CONTRACT_DIGEST: &str =
    "sha256:8025df77cfcfd743f3007fd129239bed417ff2095b9f744d23bb7c0e5ec56e4f";
pub const RENDERPROVE_VISION_REQUEST_SCHEMA: &str = "vision-request-v1";
pub const RENDERPROVE_VISION_REQUEST_SCHEMA_URI: &str = "https://raw.githubusercontent.com/teamleaderleo/renderprove/main/schema/vision-request-v1.schema.json";
pub const RENDERPROVE_VISION_PROMPT_POLICY: &str = "vision-prompt-policy-v1";
pub const RENDERPROVE_VISION_CANONICALIZATION_PROFILE: &str = "rgba8-png-zlib9-v1";
pub const MAX_VISION_SCREENSHOT_BYTES: u64 = 8_000_000;
pub const MAX_VISION_BRIEF_BYTES: u64 = 2_400;
pub const MAX_VISION_RECEIPT_BYTES: u64 = 256_000;
pub const MAX_VISION_PREVIEW_BYTES: usize = 65_536;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderproveVisionToolIdentity {
    schema_version: u8,
    repository: RepositoryRef,
    commit: CommitId,
    artifact_digest: Sha256Digest,
    command_id: RepositoryCommandId,
    command_contract_digest: Sha256Digest,
    request_schema: &'static str,
    prompt_policy: &'static str,
    canonicalization_profile: &'static str,
}

impl RenderproveVisionToolIdentity {
    /// Bind one immutable Renderprove build to the reviewed vision command contract.
    ///
    /// # Errors
    ///
    /// Returns an error unless the repository, command ID, and contract digest match the reviewed
    /// Renderprove vision packet contract.
    pub fn new(
        repository: RepositoryRef,
        commit: CommitId,
        artifact_digest: Sha256Digest,
        command_id: RepositoryCommandId,
        command_contract_digest: Sha256Digest,
    ) -> Result<Self, RenderproveVisionProfileError> {
        if repository.as_str() != RENDERPROVE_REPOSITORY {
            return Err(RenderproveVisionProfileError::new(
                "tool.repository",
                "unexpected_renderprove_repository",
                "must identify the reviewed Renderprove repository",
            ));
        }
        if command_id.as_str() != RENDERPROVE_VISION_COMMAND_ID {
            return Err(RenderproveVisionProfileError::new(
                "tool.command_id",
                "unexpected_command_identity",
                "must identify renderprove.vision-check.v1",
            ));
        }
        if command_contract_digest.as_str() != RENDERPROVE_VISION_COMMAND_CONTRACT_DIGEST {
            return Err(RenderproveVisionProfileError::new(
                "tool.command_contract_digest",
                "unexpected_command_contract_digest",
                "must match the reviewed Renderprove vision command contract",
            ));
        }
        Ok(Self {
            schema_version: RENDERPROVE_VISION_PROFILE_SCHEMA_VERSION,
            repository,
            commit,
            artifact_digest,
            command_id,
            command_contract_digest,
            request_schema: RENDERPROVE_VISION_REQUEST_SCHEMA,
            prompt_policy: RENDERPROVE_VISION_PROMPT_POLICY,
            canonicalization_profile: RENDERPROVE_VISION_CANONICALIZATION_PROFILE,
        })
    }

    #[must_use]
    pub const fn repository(&self) -> &RepositoryRef {
        &self.repository
    }

    #[must_use]
    pub const fn commit(&self) -> &CommitId {
        &self.commit
    }

    #[must_use]
    pub const fn command_id(&self) -> &RepositoryCommandId {
        &self.command_id
    }

    #[must_use]
    pub const fn command_contract_digest(&self) -> &Sha256Digest {
        &self.command_contract_digest
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderproveVisionInputKind {
    ScreenshotPng,
    OperatorBriefUtf8,
    ReceiptV1,
}

impl RenderproveVisionInputKind {
    const fn max_bytes(self) -> u64 {
        match self {
            Self::ScreenshotPng => MAX_VISION_SCREENSHOT_BYTES,
            Self::OperatorBriefUtf8 => MAX_VISION_BRIEF_BYTES,
            Self::ReceiptV1 => MAX_VISION_RECEIPT_BYTES,
        }
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PrivateProjectPath(PathBuf);

impl PrivateProjectPath {
    fn parse(value: impl Into<PathBuf>) -> Result<Self, RenderproveVisionProfileError> {
        let value = value.into();
        let Some(text) = value.to_str() else {
            return Err(RenderproveVisionProfileError::new(
                "input.path",
                "invalid_project_path",
                "must be valid UTF-8",
            ));
        };
        let segments_valid = text
            .split('/')
            .all(|segment| !segment.is_empty() && !matches!(segment, "." | ".."));
        let components_valid = value
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
        if text.is_empty()
            || text == "-"
            || value.is_absolute()
            || text.contains("://")
            || text.contains('\\')
            || text.chars().any(char::is_control)
            || !segments_valid
            || !components_valid
        {
            return Err(RenderproveVisionProfileError::new(
                "input.path",
                "invalid_project_path",
                "must be one normalized project-relative file path without URL, stdin, traversal, backslash, or control syntax",
            ));
        }
        Ok(Self(value))
    }

    fn as_utf8(&self) -> &str {
        self.0
            .to_str()
            .expect("validated project paths remain UTF-8")
    }
}

impl fmt::Debug for PrivateProjectPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<private-project-path>")
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct RenderproveVisionInputSlot {
    kind: RenderproveVisionInputKind,
    bytes: u64,
    digest: Sha256Digest,
    #[serde(skip)]
    path: PrivateProjectPath,
}

impl RenderproveVisionInputSlot {
    pub fn screenshot_png(
        path: impl Into<PathBuf>,
        bytes: u64,
        digest: Sha256Digest,
    ) -> Result<Self, RenderproveVisionProfileError> {
        Self::new(
            RenderproveVisionInputKind::ScreenshotPng,
            path,
            bytes,
            digest,
        )
    }

    pub fn operator_brief_utf8(
        path: impl Into<PathBuf>,
        bytes: u64,
        digest: Sha256Digest,
    ) -> Result<Self, RenderproveVisionProfileError> {
        Self::new(
            RenderproveVisionInputKind::OperatorBriefUtf8,
            path,
            bytes,
            digest,
        )
    }

    pub fn receipt_v1(
        path: impl Into<PathBuf>,
        bytes: u64,
        digest: Sha256Digest,
    ) -> Result<Self, RenderproveVisionProfileError> {
        Self::new(RenderproveVisionInputKind::ReceiptV1, path, bytes, digest)
    }

    fn new(
        kind: RenderproveVisionInputKind,
        path: impl Into<PathBuf>,
        bytes: u64,
        digest: Sha256Digest,
    ) -> Result<Self, RenderproveVisionProfileError> {
        if bytes == 0 || bytes > kind.max_bytes() {
            return Err(RenderproveVisionProfileError::new(
                "input.bytes",
                "input_byte_limit_exceeded",
                format!(
                    "must contain between 1 and {} bytes for the declared input kind",
                    kind.max_bytes()
                ),
            ));
        }
        Ok(Self {
            kind,
            bytes,
            digest,
            path: PrivateProjectPath::parse(path)?,
        })
    }

    #[must_use]
    pub const fn kind(&self) -> RenderproveVisionInputKind {
        self.kind
    }

    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }

    #[must_use]
    pub const fn digest(&self) -> &Sha256Digest {
        &self.digest
    }
}

impl fmt::Debug for RenderproveVisionInputSlot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderproveVisionInputSlot")
            .field("kind", &self.kind)
            .field("bytes", &self.bytes)
            .field("digest", &self.digest)
            .field("path", &"<private-project-path>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderproveVisionInputSlots {
    screenshot: RenderproveVisionInputSlot,
    brief: RenderproveVisionInputSlot,
    receipt: Option<RenderproveVisionInputSlot>,
}

impl RenderproveVisionInputSlots {
    /// Define exactly one screenshot, one brief, and zero or one receipt input.
    ///
    /// # Errors
    ///
    /// Returns an error for a mismatched slot kind or repeated private path.
    pub fn new(
        screenshot: RenderproveVisionInputSlot,
        brief: RenderproveVisionInputSlot,
        receipt: Option<RenderproveVisionInputSlot>,
    ) -> Result<Self, RenderproveVisionProfileError> {
        if screenshot.kind != RenderproveVisionInputKind::ScreenshotPng {
            return Err(RenderproveVisionProfileError::new(
                "inputs.screenshot",
                "wrong_input_kind",
                "must use one PNG screenshot slot",
            ));
        }
        if brief.kind != RenderproveVisionInputKind::OperatorBriefUtf8 {
            return Err(RenderproveVisionProfileError::new(
                "inputs.brief",
                "wrong_input_kind",
                "must use one UTF-8 operator brief slot",
            ));
        }
        if receipt
            .as_ref()
            .is_some_and(|slot| slot.kind != RenderproveVisionInputKind::ReceiptV1)
        {
            return Err(RenderproveVisionProfileError::new(
                "inputs.receipt",
                "wrong_input_kind",
                "must use a receipt-v1 slot when present",
            ));
        }
        let mut paths = BTreeSet::new();
        for slot in [&screenshot, &brief].into_iter().chain(receipt.as_ref()) {
            if !paths.insert(slot.path.clone()) {
                return Err(RenderproveVisionProfileError::new(
                    "inputs",
                    "duplicate_input_path",
                    "each typed input must use a distinct private project path",
                ));
            }
        }
        Ok(Self {
            screenshot,
            brief,
            receipt,
        })
    }

    #[must_use]
    pub const fn screenshot(&self) -> &RenderproveVisionInputSlot {
        &self.screenshot
    }

    #[must_use]
    pub const fn brief(&self) -> &RenderproveVisionInputSlot {
        &self.brief
    }

    #[must_use]
    pub const fn receipt(&self) -> Option<&RenderproveVisionInputSlot> {
        self.receipt.as_ref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderproveVisionNetworkAuthority {
    Denied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderproveVisionCredentialAuthority {
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderproveVisionWorkspaceAuthority {
    ReadOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderproveVisionLocalCommitAuthority {
    Forbidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderproveVisionPublicationAuthority {
    Forbidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RenderproveVisionExecutionPolicy {
    network: RenderproveVisionNetworkAuthority,
    credentials: RenderproveVisionCredentialAuthority,
    workspace: RenderproveVisionWorkspaceAuthority,
    local_commit: RenderproveVisionLocalCommitAuthority,
    publication: RenderproveVisionPublicationAuthority,
}

impl RenderproveVisionExecutionPolicy {
    #[must_use]
    pub const fn credentialless_packet() -> Self {
        Self {
            network: RenderproveVisionNetworkAuthority::Denied,
            credentials: RenderproveVisionCredentialAuthority::None,
            workspace: RenderproveVisionWorkspaceAuthority::ReadOnly,
            local_commit: RenderproveVisionLocalCommitAuthority::Forbidden,
            publication: RenderproveVisionPublicationAuthority::Forbidden,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderproveVisionPacketProfileDefinition {
    pub profile_id: VerificationProfileId,
    pub project_repository: RepositoryRef,
    pub tested_source: TestedSourceIdentity,
    pub project_command: RepositoryCommandIdentity,
    pub tool: RenderproveVisionToolIdentity,
    pub inputs: RenderproveVisionInputSlots,
    pub resources: ResourceDefaults,
    pub timeout: TimeoutPolicy,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct RenderproveVisionPacketProfile {
    schema_version: u8,
    profile_id: VerificationProfileId,
    project_repository: RepositoryRef,
    tested_source: TestedSourceIdentity,
    project_command: RepositoryCommandIdentity,
    tool: RenderproveVisionToolIdentity,
    inputs: RenderproveVisionInputSlots,
    resources: ResourceDefaults,
    timeout: TimeoutPolicy,
    execution: RenderproveVisionExecutionPolicy,
}

impl RenderproveVisionPacketProfile {
    /// Define one credentialless Renderprove vision packet profile.
    ///
    /// # Errors
    ///
    /// Returns an error unless the project-owned command identity belongs to the exact project
    /// repository. The external Renderprove tool identity remains deliberately separate.
    pub fn new(
        definition: RenderproveVisionPacketProfileDefinition,
    ) -> Result<Self, RenderproveVisionProfileError> {
        let RenderproveVisionPacketProfileDefinition {
            profile_id,
            project_repository,
            tested_source,
            project_command,
            tool,
            inputs,
            resources,
            timeout,
        } = definition;
        if project_command.repository() != &project_repository {
            return Err(RenderproveVisionProfileError::new(
                "project_command.repository",
                "project_command_repository_mismatch",
                "must match the exact project repository",
            ));
        }
        Ok(Self {
            schema_version: RENDERPROVE_VISION_PROFILE_SCHEMA_VERSION,
            profile_id,
            project_repository,
            tested_source,
            project_command,
            tool,
            inputs,
            resources,
            timeout,
            execution: RenderproveVisionExecutionPolicy::credentialless_packet(),
        })
    }

    #[must_use]
    pub const fn profile_id(&self) -> &VerificationProfileId {
        &self.profile_id
    }

    #[must_use]
    pub const fn project_repository(&self) -> &RepositoryRef {
        &self.project_repository
    }

    #[must_use]
    pub const fn tested_source(&self) -> &TestedSourceIdentity {
        &self.tested_source
    }

    #[must_use]
    pub const fn project_command(&self) -> &RepositoryCommandIdentity {
        &self.project_command
    }

    #[must_use]
    pub const fn tool(&self) -> &RenderproveVisionToolIdentity {
        &self.tool
    }

    #[must_use]
    pub const fn inputs(&self) -> &RenderproveVisionInputSlots {
        &self.inputs
    }

    #[must_use]
    pub fn command_plan(&self) -> RenderproveVisionCommandPlan {
        let mut argv = vec![
            "renderprove".to_owned(),
            "vision-check".to_owned(),
            ".".to_owned(),
            "--screenshot".to_owned(),
            self.inputs.screenshot.path.as_utf8().to_owned(),
            "--brief".to_owned(),
            self.inputs.brief.path.as_utf8().to_owned(),
        ];
        if let Some(receipt) = &self.inputs.receipt {
            argv.push("--receipt".to_owned());
            argv.push(receipt.path.as_utf8().to_owned());
        }
        argv.push("--dry-run".to_owned());
        argv.push("--json".to_owned());
        RenderproveVisionCommandPlan { argv }
    }
}

impl fmt::Debug for RenderproveVisionPacketProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderproveVisionPacketProfile")
            .field("schema_version", &self.schema_version)
            .field("profile_id", &self.profile_id)
            .field("project_repository", &self.project_repository)
            .field("tested_source", &self.tested_source)
            .field("project_command", &self.project_command)
            .field("tool", &self.tool)
            .field("inputs", &self.inputs)
            .field("resources", &self.resources)
            .field("timeout", &self.timeout)
            .field("execution", &self.execution)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct RenderproveVisionCommandPlan {
    argv: Vec<String>,
}

impl RenderproveVisionCommandPlan {
    #[must_use]
    pub fn argv(&self) -> &[String] {
        &self.argv
    }
}

impl fmt::Debug for RenderproveVisionCommandPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderproveVisionCommandPlan")
            .field("command", &RENDERPROVE_VISION_COMMAND_ID)
            .field("argv", &"<fixed-argv-with-private-project-paths>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderproveVisionPreviewEvidence {
    schema_version: &'static str,
    command_contract_id: &'static str,
    command_contract_digest: Sha256Digest,
    request_digest: Sha256Digest,
    bytes: u64,
    artifact_digest: Sha256Digest,
}

impl RenderproveVisionPreviewEvidence {
    /// Validate the identity-bearing fields from one bounded public Renderprove preview.
    ///
    /// The caller supplies the digest of the exact retained preview bytes through the existing
    /// content-addressed artifact boundary.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid UTF-8/JSON, size exhaustion, or a reviewed identity mismatch.
    pub fn from_public_preview(
        preview_bytes: &[u8],
        artifact_digest: Sha256Digest,
        tool: &RenderproveVisionToolIdentity,
    ) -> Result<Self, RenderproveVisionProfileError> {
        if preview_bytes.is_empty() || preview_bytes.len() > MAX_VISION_PREVIEW_BYTES {
            return Err(RenderproveVisionProfileError::new(
                "preview.bytes",
                "preview_byte_limit_exceeded",
                format!("must contain between 1 and {MAX_VISION_PREVIEW_BYTES} bytes"),
            ));
        }
        let text = std::str::from_utf8(preview_bytes).map_err(|_| {
            RenderproveVisionProfileError::new(
                "preview",
                "invalid_preview_utf8",
                "must be valid UTF-8",
            )
        })?;
        let value: Value = serde_json::from_str(text).map_err(|_| {
            RenderproveVisionProfileError::new(
                "preview",
                "invalid_preview_json",
                "must be valid JSON",
            )
        })?;
        let object = value.as_object().ok_or_else(|| {
            RenderproveVisionProfileError::new(
                "preview",
                "invalid_preview_shape",
                "must be a JSON object",
            )
        })?;

        require_preview_string(object, "$schema", RENDERPROVE_VISION_REQUEST_SCHEMA_URI)?;
        require_preview_string(object, "schemaVersion", RENDERPROVE_VISION_REQUEST_SCHEMA)?;
        require_preview_string(object, "mode", "dry-run")?;
        require_preview_string(object, "authority", "advisory")?;
        require_preview_string(object, "commandContractId", RENDERPROVE_VISION_COMMAND_ID)?;
        require_preview_string(
            object,
            "commandContractDigest",
            tool.command_contract_digest()
                .as_str()
                .strip_prefix("sha256:")
                .expect("validated SHA-256 digest has prefix"),
        )?;
        require_preview_string(
            object,
            "promptPolicyVersion",
            RENDERPROVE_VISION_PROMPT_POLICY,
        )?;
        require_preview_string(
            object,
            "canonicalizationProfile",
            RENDERPROVE_VISION_CANONICALIZATION_PROFILE,
        )?;
        let request_digest = object
            .get("requestDigest")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                RenderproveVisionProfileError::new(
                    "preview.requestDigest",
                    "missing_request_digest",
                    "must contain one lowercase SHA-256 digest",
                )
            })?;
        if request_digest.len() != 64 || !request_digest.bytes().all(is_lower_hex) {
            return Err(RenderproveVisionProfileError::new(
                "preview.requestDigest",
                "invalid_request_digest",
                "must contain one lowercase SHA-256 digest",
            ));
        }
        let request_digest =
            Sha256Digest::parse(&format!("sha256:{request_digest}")).map_err(|_| {
                RenderproveVisionProfileError::new(
                    "preview.requestDigest",
                    "invalid_request_digest",
                    "must contain one lowercase SHA-256 digest",
                )
            })?;
        Ok(Self {
            schema_version: RENDERPROVE_VISION_REQUEST_SCHEMA,
            command_contract_id: RENDERPROVE_VISION_COMMAND_ID,
            command_contract_digest: tool.command_contract_digest().clone(),
            request_digest,
            bytes: preview_bytes.len() as u64,
            artifact_digest,
        })
    }

    #[must_use]
    pub const fn request_digest(&self) -> &Sha256Digest {
        &self.request_digest
    }

    #[must_use]
    pub const fn artifact_digest(&self) -> &Sha256Digest {
        &self.artifact_digest
    }
}

fn require_preview_string(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
    expected: &str,
) -> Result<(), RenderproveVisionProfileError> {
    if object.get(field).and_then(Value::as_str) != Some(expected) {
        return Err(RenderproveVisionProfileError::new(
            format!("preview.{field}"),
            "preview_identity_mismatch",
            "must match the reviewed Renderprove vision packet contract",
        ));
    }
    Ok(())
}

const fn is_lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderproveVisionProfileError {
    pub field: String,
    pub code: String,
    pub problem: String,
}

impl RenderproveVisionProfileError {
    fn new(field: impl Into<String>, code: impl Into<String>, problem: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            code: code.into(),
            problem: problem.into(),
        }
    }
}

impl fmt::Display for RenderproveVisionProfileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", self.code, self.problem)
    }
}

impl std::error::Error for RenderproveVisionProfileError {}
