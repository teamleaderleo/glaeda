use std::fmt;

use serde::Serialize;

use crate::artifact::ArtifactIdentity;
use crate::lease::{LeaseId, LeaseIdentity, LeaseKind};
use crate::state::InstallationId;

pub const MIN_PREVIEW_TTL_SECONDS: u64 = 60;
pub const MAX_PREVIEW_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;
const MAX_HEALTH_PATH_BYTES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PreviewPort(u16);

impl PreviewPort {
    /// Validate one container port.
    ///
    /// # Errors
    ///
    /// Returns an error for port zero.
    pub fn new(value: u16) -> Result<Self, PreviewRequestError> {
        if value == 0 {
            return Err(PreviewRequestError::new(
                "port",
                "must be between 1 and 65535",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PreviewTtl(u64);

impl PreviewTtl {
    /// Validate one bounded preview lifetime.
    ///
    /// # Errors
    ///
    /// Returns an error for lifetimes below one minute or above seven days.
    pub fn new(seconds: u64) -> Result<Self, PreviewRequestError> {
        if !(MIN_PREVIEW_TTL_SECONDS..=MAX_PREVIEW_TTL_SECONDS).contains(&seconds) {
            return Err(PreviewRequestError::new(
                "ttl_seconds",
                format!("must be between {MIN_PREVIEW_TTL_SECONDS} and {MAX_PREVIEW_TTL_SECONDS}"),
            ));
        }
        Ok(Self(seconds))
    }

    #[must_use]
    pub const fn seconds(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PreviewGeneration(u64);

impl PreviewGeneration {
    #[must_use]
    pub const fn first() -> Self {
        Self(1)
    }

    /// Validate one persisted preview generation.
    ///
    /// # Errors
    ///
    /// Returns an error for generation zero.
    pub fn new(value: u64) -> Result<Self, PreviewPlanError> {
        if value == 0 {
            return Err(PreviewPlanError::new(
                "preview generation must be greater than zero",
            ));
        }
        Ok(Self(value))
    }

    /// Advance one preview generation.
    ///
    /// # Errors
    ///
    /// Returns an error when the generation counter is exhausted.
    pub fn next(self) -> Result<Self, PreviewPlanError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or_else(|| PreviewPlanError::new("preview generation counter is exhausted"))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct HealthPath(String);

impl HealthPath {
    /// Validate one path-only HTTP health endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error for relative paths, queries, fragments, malformed percent escapes,
    /// unsupported punctuation, non-ASCII values, or paths above the bounded length.
    pub fn parse(value: &str) -> Result<Self, PreviewRequestError> {
        if !valid_health_path(value) {
            return Err(PreviewRequestError::new(
                "health_path",
                "must be an absolute bounded ASCII path with valid percent escapes",
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
pub struct PreviewRequest {
    pub slot: LeaseId,
    pub artifact: ArtifactIdentity,
    pub port: PreviewPort,
    pub ttl: PreviewTtl,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health_path: Option<HealthPath>,
}

impl PreviewRequest {
    #[must_use]
    pub const fn new(
        slot: LeaseId,
        artifact: ArtifactIdentity,
        port: PreviewPort,
        ttl: PreviewTtl,
        health_path: Option<HealthPath>,
    ) -> Self {
        Self {
            slot,
            artifact,
            port,
            ttl,
            health_path,
        }
    }

    #[must_use]
    pub fn lease_identity(&self, installation_id: InstallationId) -> LeaseIdentity {
        LeaseIdentity::new(self.slot.clone(), installation_id, LeaseKind::Preview)
    }

    fn same_runtime(&self, state: &PreviewState) -> bool {
        self.artifact == state.artifact
            && self.port == state.port
            && self.health_path == state.health_path
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreviewState {
    pub slot: LeaseId,
    pub artifact: ArtifactIdentity,
    pub port: PreviewPort,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health_path: Option<HealthPath>,
    pub generation: PreviewGeneration,
}

impl PreviewState {
    #[must_use]
    pub fn from_request(request: &PreviewRequest, generation: PreviewGeneration) -> Self {
        Self {
            slot: request.slot.clone(),
            artifact: request.artifact.clone(),
            port: request.port,
            health_path: request.health_path.clone(),
            generation,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewPlanAction {
    Create,
    ReuseAndRenew,
    Replace,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreviewPlan {
    action: PreviewPlanAction,
    request: PreviewRequest,
    generation: PreviewGeneration,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_artifact: Option<ArtifactIdentity>,
}

impl PreviewPlan {
    #[must_use]
    pub const fn action(&self) -> PreviewPlanAction {
        self.action
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
    pub const fn previous_artifact(&self) -> Option<&ArtifactIdentity> {
        self.previous_artifact.as_ref()
    }

    #[must_use]
    pub fn resulting_state(&self) -> PreviewState {
        PreviewState::from_request(&self.request, self.generation)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreviewRequestError {
    pub field: String,
    pub problem: String,
}

impl PreviewRequestError {
    fn new(field: impl Into<String>, problem: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            problem: problem.into(),
        }
    }
}

impl fmt::Display for PreviewRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", self.field, self.problem)
    }
}

impl std::error::Error for PreviewRequestError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreviewPlanError {
    pub problem: String,
}

impl PreviewPlanError {
    fn new(problem: impl Into<String>) -> Self {
        Self {
            problem: problem.into(),
        }
    }
}

impl fmt::Display for PreviewPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.problem)
    }
}

impl std::error::Error for PreviewPlanError {}

/// Decide whether one explicit preview request creates, reuses, or replaces a slot.
///
/// Repeated requests for the same runtime inputs reuse the current generation and renew its lease.
/// A changed artifact, port, or health endpoint replaces the slot exactly once. The function plans
/// no host mutation.
///
/// # Errors
///
/// Returns an error when the supplied current state belongs to another slot or its generation is
/// exhausted.
pub fn plan_preview(
    current: Option<&PreviewState>,
    request: PreviewRequest,
) -> Result<PreviewPlan, PreviewPlanError> {
    let Some(current) = current else {
        return Ok(PreviewPlan {
            action: PreviewPlanAction::Create,
            request,
            generation: PreviewGeneration::first(),
            previous_artifact: None,
        });
    };
    if current.slot != request.slot {
        return Err(PreviewPlanError::new(
            "current preview state belongs to another slot",
        ));
    }
    if request.same_runtime(current) {
        return Ok(PreviewPlan {
            action: PreviewPlanAction::ReuseAndRenew,
            request,
            generation: current.generation,
            previous_artifact: None,
        });
    }
    let generation = current.generation.next()?;
    Ok(PreviewPlan {
        action: PreviewPlanAction::Replace,
        request,
        generation,
        previous_artifact: Some(current.artifact.clone()),
    })
}

fn valid_health_path(value: &str) -> bool {
    if !value.starts_with('/') || value.len() > MAX_HEALTH_PATH_BYTES {
        return false;
    }
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'%' {
            if index + 2 >= bytes.len()
                || !bytes[index + 1].is_ascii_hexdigit()
                || !bytes[index + 2].is_ascii_hexdigit()
            {
                return false;
            }
            index += 3;
            continue;
        }
        if !(byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'.' | b'_' | b'~')) {
            return false;
        }
        index += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use crate::artifact::{ArtifactIdentity, ArtifactKind, CommitId, RepositoryRef, Sha256Digest};
    use crate::lease::LeaseId;
    use crate::state::InstallationId;

    use super::{
        HealthPath, PreviewGeneration, PreviewPlanAction, PreviewPort, PreviewRequest,
        PreviewState, PreviewTtl, plan_preview,
    };

    fn artifact(byte: &str) -> ArtifactIdentity {
        ArtifactIdentity::new(
            RepositoryRef::parse("example/project").expect("repository"),
            CommitId::parse(&"1a".repeat(20)).expect("commit"),
            ArtifactKind::OciImage,
            Sha256Digest::parse(&format!("sha256:{}", byte.repeat(64))).expect("digest"),
        )
    }

    fn request(artifact: ArtifactIdentity, ttl_seconds: u64) -> PreviewRequest {
        PreviewRequest::new(
            LeaseId::parse("pr-42").expect("slot"),
            artifact,
            PreviewPort::new(3000).expect("port"),
            PreviewTtl::new(ttl_seconds).expect("TTL"),
            Some(HealthPath::parse("/health").expect("health path")),
        )
    }

    #[test]
    fn first_request_creates_generation_one() {
        let request = request(artifact("a"), 3600);
        let plan = plan_preview(None, request).expect("plan preview");

        assert_eq!(plan.action(), PreviewPlanAction::Create);
        assert_eq!(plan.generation().get(), 1);
        assert!(plan.previous_artifact().is_none());
    }

    #[test]
    fn repeated_artifact_reuses_slot_and_renews_ttl() {
        let first = request(artifact("a"), 3600);
        let current =
            PreviewState::from_request(&first, PreviewGeneration::new(7).expect("generation"));
        let repeated = request(artifact("a"), 7200);
        let plan = plan_preview(Some(&current), repeated).expect("plan preview");

        assert_eq!(plan.action(), PreviewPlanAction::ReuseAndRenew);
        assert_eq!(plan.generation().get(), 7);
        assert_eq!(plan.request().ttl.seconds(), 7200);
        assert!(plan.previous_artifact().is_none());
    }

    #[test]
    fn changed_artifact_replaces_slot_once() {
        let first = request(artifact("a"), 3600);
        let current =
            PreviewState::from_request(&first, PreviewGeneration::new(7).expect("generation"));
        let replacement = request(artifact("b"), 3600);
        let plan = plan_preview(Some(&current), replacement).expect("plan preview");

        assert_eq!(plan.action(), PreviewPlanAction::Replace);
        assert_eq!(plan.generation().get(), 8);
        assert_eq!(plan.previous_artifact(), Some(&current.artifact));
    }

    #[test]
    fn different_slot_state_is_rejected() {
        let request = request(artifact("a"), 3600);
        let mut current = PreviewState::from_request(&request, PreviewGeneration::first());
        current.slot = LeaseId::parse("pr-99").expect("other slot");

        assert!(plan_preview(Some(&current), request).is_err());
    }

    #[test]
    fn generation_zero_and_exhaustion_are_rejected() {
        assert!(PreviewGeneration::new(0).is_err());

        let first = request(artifact("a"), 3600);
        let current = PreviewState::from_request(
            &first,
            PreviewGeneration::new(u64::MAX).expect("maximum generation"),
        );
        let replacement = request(artifact("b"), 3600);
        assert!(plan_preview(Some(&current), replacement).is_err());
    }

    #[test]
    fn request_bounds_reject_invalid_ports_ttls_and_health_paths() {
        assert!(PreviewPort::new(0).is_err());
        assert!(PreviewTtl::new(30).is_err());
        assert!(HealthPath::parse("health").is_err());
        assert!(HealthPath::parse("/health?full=true").is_err());
        assert!(HealthPath::parse("/health\\internal").is_err());
        assert!(HealthPath::parse("/health%2").is_err());
        assert!(HealthPath::parse("/health%2Fready").is_ok());
    }

    #[test]
    fn request_derives_a_preview_lease_identity() {
        let request = request(artifact("a"), 3600);
        let identity = request
            .lease_identity(InstallationId::parse("installation-001").expect("installation ID"));

        assert_eq!(identity.lease_id.as_str(), "pr-42");
        assert_eq!(identity.kind, crate::lease::LeaseKind::Preview);
    }
}
