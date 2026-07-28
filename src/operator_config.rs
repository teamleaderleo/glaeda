use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize, Serializer};
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::lima_observation::LimaInstanceName;
use crate::mac_availability::AvailabilityRequest;
use crate::personal_worker_queue::{
    PERSONAL_WORKER_INTERACTIVE_COOLDOWN_MILLIS, PERSONAL_WORKER_STOPPED_COOLDOWN_MILLIS,
};
use crate::verification_profile::VerificationProfileId;

pub const OPERATOR_CONFIG_SCHEMA_VERSION: u8 = 1;
pub const MAX_OPERATOR_CONFIG_PATH_BYTES: usize = 1_024;
pub const MAX_OPERATOR_CONFIG_DOCUMENT_BYTES: usize = 16_384;
pub const MAX_OPERATOR_IDLE_MILLIS: u64 = 7 * 24 * 60 * 60 * 1_000;

const OPERATOR_CONFIG_IDENTITY_DOCUMENT_TYPE: &str = "smolrunner_operator_config";
const SHA256_PREFIX: &str = "sha256:";
const HEX: &[u8; 16] = b"0123456789abcdef";
const REDACTED_STATE_ROOT: &str = "<private-personal-worker-state-root>";
const REDACTED_GUEST_WORKSPACE: &str = "<private-guest-workspace-path>";

#[derive(Clone, PartialEq, Eq)]
pub struct PersonalWorkerStateRoot(PathBuf);

impl PersonalWorkerStateRoot {
    /// Parse one canonical absolute private host state root.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for a relative, aliased, non-UTF-8, control-bearing, root-only,
    /// or oversized path.
    pub fn parse(value: impl Into<PathBuf>) -> Result<Self, OperatorConfigError> {
        Ok(Self(validate_private_absolute_path(
            "state_root",
            value.into(),
        )?))
    }

    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    fn as_str(&self) -> &str {
        self.0
            .to_str()
            .expect("validated operator state root is UTF-8")
    }
}

impl fmt::Debug for PersonalWorkerStateRoot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(REDACTED_STATE_ROOT)
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct GuestWorkspacePath(PathBuf);

impl GuestWorkspacePath {
    /// Parse one canonical absolute private guest workspace path.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for a relative, aliased, non-UTF-8, control-bearing, root-only,
    /// or oversized path.
    pub fn parse(value: impl Into<PathBuf>) -> Result<Self, OperatorConfigError> {
        Ok(Self(validate_private_absolute_path(
            "guest_workspace",
            value.into(),
        )?))
    }

    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    fn as_str(&self) -> &str {
        self.0
            .to_str()
            .expect("validated guest workspace path is UTF-8")
    }
}

impl fmt::Debug for GuestWorkspacePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(REDACTED_GUEST_WORKSPACE)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorOutputPreference {
    Human,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorRemediationPreference {
    IncludeSuggestions,
    CodesOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct OperatorIdlePolicy {
    interactive_after_millis: u64,
    stopped_after_millis: u64,
}

impl OperatorIdlePolicy {
    /// Define explicit bounded interactive and stopped idle thresholds.
    ///
    /// # Errors
    ///
    /// Returns a bounded error unless both values are positive, the stopped threshold is later,
    /// and both values remain within the fixed implementation bound.
    pub fn new(
        interactive_after_millis: u64,
        stopped_after_millis: u64,
    ) -> Result<Self, OperatorConfigError> {
        if interactive_after_millis == 0
            || stopped_after_millis == 0
            || interactive_after_millis > MAX_OPERATOR_IDLE_MILLIS
            || stopped_after_millis > MAX_OPERATOR_IDLE_MILLIS
        {
            return Err(OperatorConfigError::new(
                "idle_policy",
                "invalid_idle_threshold",
                "idle thresholds must remain within the bounded positive range",
            ));
        }
        if stopped_after_millis <= interactive_after_millis {
            return Err(OperatorConfigError::new(
                "idle_policy.stopped_after_millis",
                "invalid_idle_order",
                "the stopped threshold must be later than the interactive threshold",
            ));
        }
        Ok(Self {
            interactive_after_millis,
            stopped_after_millis,
        })
    }

    #[must_use]
    pub const fn interactive_after_millis(self) -> u64 {
        self.interactive_after_millis
    }

    #[must_use]
    pub const fn stopped_after_millis(self) -> u64 {
        self.stopped_after_millis
    }

    #[must_use]
    pub const fn matches_alpha_policy(self) -> bool {
        self.interactive_after_millis == PERSONAL_WORKER_INTERACTIVE_COOLDOWN_MILLIS
            && self.stopped_after_millis == PERSONAL_WORKER_STOPPED_COOLDOWN_MILLIS
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OperatorConfigIdentity {
    schema_version: u8,
    digest: Sha256Digest,
}

impl OperatorConfigIdentity {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn digest(&self) -> &Sha256Digest {
        &self.digest
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct OperatorConfig {
    schema_version: u8,
    state_root: PersonalWorkerStateRoot,
    lima_instance: LimaInstanceName,
    guest_workspace: GuestWorkspacePath,
    default_verification_profile: VerificationProfileId,
    availability: AvailabilityRequest,
    idle_policy: OperatorIdlePolicy,
    output_preference: OperatorOutputPreference,
    remediation_preference: OperatorRemediationPreference,
    identity: OperatorConfigIdentity,
}

impl OperatorConfig {
    /// Construct one exact operator configuration from caller-supplied typed values only.
    ///
    /// # Errors
    ///
    /// Returns a bounded error when the canonical configuration identity cannot be encoded.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        state_root: PersonalWorkerStateRoot,
        lima_instance: LimaInstanceName,
        guest_workspace: GuestWorkspacePath,
        default_verification_profile: VerificationProfileId,
        availability: AvailabilityRequest,
        idle_policy: OperatorIdlePolicy,
        output_preference: OperatorOutputPreference,
        remediation_preference: OperatorRemediationPreference,
    ) -> Result<Self, OperatorConfigError> {
        let mut config = Self {
            schema_version: OPERATOR_CONFIG_SCHEMA_VERSION,
            state_root,
            lima_instance,
            guest_workspace,
            default_verification_profile,
            availability,
            idle_policy,
            output_preference,
            remediation_preference,
            identity: OperatorConfigIdentity {
                schema_version: OPERATOR_CONFIG_SCHEMA_VERSION,
                digest: placeholder_digest()?,
            },
        };
        config.identity = digest_operator_config(&config)?;
        Ok(config)
    }

    /// Decode one strict persisted configuration document.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for oversized or malformed JSON, unknown fields, an unsupported
    /// version, invalid paths or identities, or invalid policy values.
    pub fn decode_persisted_json(bytes: &[u8]) -> Result<Self, OperatorConfigError> {
        if bytes.len() > MAX_OPERATOR_CONFIG_DOCUMENT_BYTES {
            return Err(OperatorConfigError::new(
                "document",
                "document_too_large",
                "operator configuration exceeds the bounded document size",
            ));
        }
        let document: OperatorConfigDocument = decode_json(bytes)?;
        if document.schema_version != OPERATOR_CONFIG_SCHEMA_VERSION {
            return Err(OperatorConfigError::new(
                "schema_version",
                "unsupported_schema_version",
                "operator configuration schema version is unsupported",
            ));
        }
        Self::new(
            PersonalWorkerStateRoot::parse(document.state_root)?,
            LimaInstanceName::parse(&document.lima_instance).map_err(|_| {
                OperatorConfigError::new(
                    "lima_instance",
                    "invalid_lima_instance",
                    "Lima instance identity is invalid",
                )
            })?,
            GuestWorkspacePath::parse(document.guest_workspace)?,
            VerificationProfileId::parse(&document.default_verification_profile).map_err(|_| {
                OperatorConfigError::new(
                    "default_verification_profile",
                    "invalid_verification_profile",
                    "verification profile identity is invalid",
                )
            })?,
            document.availability.into_request(),
            OperatorIdlePolicy::new(
                document.idle_policy.interactive_after_millis,
                document.idle_policy.stopped_after_millis,
            )?,
            document.output_preference,
            document.remediation_preference,
        )
    }

    /// Encode the strict private persisted configuration document.
    ///
    /// This explicit method is the only serialisation surface that includes private paths. Ordinary
    /// `Serialize` and `Debug` implementations expose only the bounded public summary.
    ///
    /// # Errors
    ///
    /// Returns a bounded error when canonical encoding fails or exceeds the fixed document bound.
    pub fn encode_persisted_json(&self) -> Result<Vec<u8>, OperatorConfigError> {
        let document = PersistedOperatorConfigDocument::from_config(self);
        let bytes = serde_json::to_vec(&document).map_err(|_| encoding_error())?;
        if bytes.len() > MAX_OPERATOR_CONFIG_DOCUMENT_BYTES {
            return Err(OperatorConfigError::new(
                "document",
                "document_too_large",
                "operator configuration exceeds the bounded document size",
            ));
        }
        Ok(bytes)
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn state_root(&self) -> &PersonalWorkerStateRoot {
        &self.state_root
    }

    #[must_use]
    pub const fn lima_instance(&self) -> &LimaInstanceName {
        &self.lima_instance
    }

    #[must_use]
    pub const fn guest_workspace(&self) -> &GuestWorkspacePath {
        &self.guest_workspace
    }

    #[must_use]
    pub const fn default_verification_profile(&self) -> &VerificationProfileId {
        &self.default_verification_profile
    }

    #[must_use]
    pub const fn availability(&self) -> AvailabilityRequest {
        self.availability
    }

    #[must_use]
    pub const fn idle_policy(&self) -> OperatorIdlePolicy {
        self.idle_policy
    }

    #[must_use]
    pub const fn output_preference(&self) -> OperatorOutputPreference {
        self.output_preference
    }

    #[must_use]
    pub const fn remediation_preference(&self) -> OperatorRemediationPreference {
        self.remediation_preference
    }

    #[must_use]
    pub const fn identity(&self) -> &OperatorConfigIdentity {
        &self.identity
    }

    #[must_use]
    pub fn public_summary(&self) -> OperatorConfigPublicSummary {
        OperatorConfigPublicSummary {
            schema_version: self.schema_version,
            identity: self.identity.clone(),
            lima_instance: self.lima_instance.clone(),
            default_verification_profile: self.default_verification_profile.clone(),
            availability: self.availability,
            idle_policy: self.idle_policy,
            output_preference: self.output_preference,
            remediation_preference: self.remediation_preference,
        }
    }
}

impl fmt::Debug for OperatorConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OperatorConfig")
            .field("schema_version", &self.schema_version)
            .field("state_root", &REDACTED_STATE_ROOT)
            .field("lima_instance", &self.lima_instance)
            .field("guest_workspace", &REDACTED_GUEST_WORKSPACE)
            .field(
                "default_verification_profile",
                &self.default_verification_profile,
            )
            .field("availability", &self.availability)
            .field("idle_policy", &self.idle_policy)
            .field("output_preference", &self.output_preference)
            .field("remediation_preference", &self.remediation_preference)
            .field("identity", &self.identity)
            .finish()
    }
}

impl Serialize for OperatorConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.public_summary().serialize(serializer)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OperatorConfigPublicSummary {
    schema_version: u8,
    identity: OperatorConfigIdentity,
    lima_instance: LimaInstanceName,
    default_verification_profile: VerificationProfileId,
    availability: AvailabilityRequest,
    idle_policy: OperatorIdlePolicy,
    output_preference: OperatorOutputPreference,
    remediation_preference: OperatorRemediationPreference,
}

impl OperatorConfigPublicSummary {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn identity(&self) -> &OperatorConfigIdentity {
        &self.identity
    }

    #[must_use]
    pub const fn lima_instance(&self) -> &LimaInstanceName {
        &self.lima_instance
    }

    #[must_use]
    pub const fn default_verification_profile(&self) -> &VerificationProfileId {
        &self.default_verification_profile
    }

    #[must_use]
    pub const fn availability(&self) -> AvailabilityRequest {
        self.availability
    }

    #[must_use]
    pub const fn idle_policy(&self) -> OperatorIdlePolicy {
        self.idle_policy
    }

    #[must_use]
    pub const fn output_preference(&self) -> OperatorOutputPreference {
        self.output_preference
    }

    #[must_use]
    pub const fn remediation_preference(&self) -> OperatorRemediationPreference {
        self.remediation_preference
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OperatorConfigError {
    pub field: &'static str,
    pub code: &'static str,
    pub problem: &'static str,
}

impl OperatorConfigError {
    const fn new(field: &'static str, code: &'static str, problem: &'static str) -> Self {
        Self {
            field,
            code,
            problem,
        }
    }
}

impl fmt::Display for OperatorConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.problem)
    }
}

impl std::error::Error for OperatorConfigError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PersistedAvailabilityRequest {
    Active,
    Away,
    Off,
    Auto,
}

impl PersistedAvailabilityRequest {
    const fn from_request(request: AvailabilityRequest) -> Self {
        match request {
            AvailabilityRequest::Active => Self::Active,
            AvailabilityRequest::Away => Self::Away,
            AvailabilityRequest::Off => Self::Off,
            AvailabilityRequest::Auto => Self::Auto,
        }
    }

    const fn into_request(self) -> AvailabilityRequest {
        match self {
            Self::Active => AvailabilityRequest::Active,
            Self::Away => AvailabilityRequest::Away,
            Self::Off => AvailabilityRequest::Off,
            Self::Auto => AvailabilityRequest::Auto,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OperatorIdlePolicyDocument {
    interactive_after_millis: u64,
    stopped_after_millis: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OperatorConfigDocument {
    schema_version: u8,
    state_root: String,
    lima_instance: String,
    guest_workspace: String,
    default_verification_profile: String,
    availability: PersistedAvailabilityRequest,
    idle_policy: OperatorIdlePolicyDocument,
    output_preference: OperatorOutputPreference,
    remediation_preference: OperatorRemediationPreference,
}

#[derive(Serialize)]
struct PersistedOperatorConfigDocument<'a> {
    schema_version: u8,
    state_root: &'a str,
    lima_instance: &'a str,
    guest_workspace: &'a str,
    default_verification_profile: &'a str,
    availability: PersistedAvailabilityRequest,
    idle_policy: OperatorIdlePolicyDocument,
    output_preference: OperatorOutputPreference,
    remediation_preference: OperatorRemediationPreference,
}

impl<'a> PersistedOperatorConfigDocument<'a> {
    fn from_config(config: &'a OperatorConfig) -> Self {
        Self {
            schema_version: config.schema_version,
            state_root: config.state_root.as_str(),
            lima_instance: config.lima_instance.as_str(),
            guest_workspace: config.guest_workspace.as_str(),
            default_verification_profile: config.default_verification_profile.as_str(),
            availability: PersistedAvailabilityRequest::from_request(config.availability),
            idle_policy: OperatorIdlePolicyDocument {
                interactive_after_millis: config.idle_policy.interactive_after_millis,
                stopped_after_millis: config.idle_policy.stopped_after_millis,
            },
            output_preference: config.output_preference,
            remediation_preference: config.remediation_preference,
        }
    }
}

#[derive(Serialize)]
struct OperatorConfigIdentityDocument<'a> {
    document_type: &'static str,
    schema_version: u8,
    config: PersistedOperatorConfigDocument<'a>,
}

fn validate_private_absolute_path(
    field: &'static str,
    value: PathBuf,
) -> Result<PathBuf, OperatorConfigError> {
    let Some(text) = value.to_str() else {
        return Err(path_error(field));
    };
    let canonical_text = text == "/"
        || (!text.ends_with('/')
            && !text.contains("//")
            && !text
                .split('/')
                .any(|component| matches!(component, "." | "..")));
    let components_are_canonical = value.is_absolute()
        && value != Path::new("/")
        && value.components().enumerate().all(|(index, component)| {
            matches!(
                (index, component),
                (0, Component::RootDir) | (_, Component::Normal(_))
            )
        });
    if text.is_empty()
        || text.len() > MAX_OPERATOR_CONFIG_PATH_BYTES
        || text.chars().any(char::is_control)
        || !canonical_text
        || !components_are_canonical
    {
        return Err(path_error(field));
    }
    Ok(value)
}

const fn path_error(field: &'static str) -> OperatorConfigError {
    OperatorConfigError::new(
        field,
        "invalid_private_path",
        "must be one bounded canonical absolute UTF-8 path",
    )
}

fn decode_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, OperatorConfigError> {
    serde_json::from_slice(bytes).map_err(|_| {
        OperatorConfigError::new(
            "document",
            "invalid_document",
            "operator configuration document is invalid",
        )
    })
}

fn digest_operator_config(
    config: &OperatorConfig,
) -> Result<OperatorConfigIdentity, OperatorConfigError> {
    let document = OperatorConfigIdentityDocument {
        document_type: OPERATOR_CONFIG_IDENTITY_DOCUMENT_TYPE,
        schema_version: OPERATOR_CONFIG_SCHEMA_VERSION,
        config: PersistedOperatorConfigDocument::from_config(config),
    };
    let mut writer = BoundedDigestWriter::new();
    serde_json::to_writer(&mut writer, &document).map_err(|_| {
        if writer.exceeded {
            OperatorConfigError::new(
                "identity",
                "identity_document_too_large",
                "operator configuration identity document exceeds its bound",
            )
        } else {
            encoding_error()
        }
    })?;
    let digest = writer.finish();
    let mut value = String::with_capacity(SHA256_PREFIX.len() + digest.len() * 2);
    value.push_str(SHA256_PREFIX);
    for byte in digest {
        value.push(HEX[(byte >> 4) as usize] as char);
        value.push(HEX[(byte & 0x0f) as usize] as char);
    }
    let digest = Sha256Digest::parse(&value).map_err(|_| encoding_error())?;
    Ok(OperatorConfigIdentity {
        schema_version: OPERATOR_CONFIG_SCHEMA_VERSION,
        digest,
    })
}

fn placeholder_digest() -> Result<Sha256Digest, OperatorConfigError> {
    Sha256Digest::parse(&format!("{SHA256_PREFIX}{}", "0".repeat(64)))
        .map_err(|_| encoding_error())
}

const fn encoding_error() -> OperatorConfigError {
    OperatorConfigError::new(
        "document",
        "encoding_failed",
        "operator configuration could not be canonically encoded",
    )
}

struct BoundedDigestWriter {
    hasher: Sha256,
    bytes_written: usize,
    exceeded: bool,
}

impl BoundedDigestWriter {
    fn new() -> Self {
        Self {
            hasher: Sha256::new(),
            bytes_written: 0,
            exceeded: false,
        }
    }

    fn finish(self) -> sha2::digest::Output<Sha256> {
        self.hasher.finalize()
    }
}

impl io::Write for BoundedDigestWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let Some(next_size) = self.bytes_written.checked_add(buffer.len()) else {
            self.exceeded = true;
            return Err(io::Error::other(
                "operator configuration identity exceeds its bound",
            ));
        };
        if next_size > MAX_OPERATOR_CONFIG_DOCUMENT_BYTES {
            self.exceeded = true;
            return Err(io::Error::other(
                "operator configuration identity exceeds its bound",
            ));
        }
        self.hasher.update(buffer);
        self.bytes_written = next_size;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_root(value: &str) -> PersonalWorkerStateRoot {
        PersonalWorkerStateRoot::parse(value).expect("state root")
    }

    fn workspace(value: &str) -> GuestWorkspacePath {
        GuestWorkspacePath::parse(value).expect("workspace")
    }

    fn instance(value: &str) -> LimaInstanceName {
        LimaInstanceName::parse(value).expect("instance")
    }

    fn profile(value: &str) -> VerificationProfileId {
        VerificationProfileId::parse(value).expect("profile")
    }

    fn alpha_idle_policy() -> OperatorIdlePolicy {
        OperatorIdlePolicy::new(
            PERSONAL_WORKER_INTERACTIVE_COOLDOWN_MILLIS,
            PERSONAL_WORKER_STOPPED_COOLDOWN_MILLIS,
        )
        .expect("idle policy")
    }

    #[allow(clippy::too_many_arguments)]
    fn config_with(
        state_root_value: &str,
        instance_value: &str,
        workspace_value: &str,
        profile_value: &str,
        availability: AvailabilityRequest,
        idle_policy: OperatorIdlePolicy,
        output: OperatorOutputPreference,
        remediation: OperatorRemediationPreference,
    ) -> OperatorConfig {
        OperatorConfig::new(
            state_root(state_root_value),
            instance(instance_value),
            workspace(workspace_value),
            profile(profile_value),
            availability,
            idle_policy,
            output,
            remediation,
        )
        .expect("config")
    }

    fn config() -> OperatorConfig {
        config_with(
            "/Users/private-user/Library/Application Support/smolrunner",
            "smolrunner",
            "/home/lima/smolrunner",
            "smolrunner.required",
            AvailabilityRequest::Active,
            alpha_idle_policy(),
            OperatorOutputPreference::Human,
            OperatorRemediationPreference::IncludeSuggestions,
        )
    }

    #[test]
    fn strict_persisted_round_trip_preserves_identity() {
        let original = config();
        let encoded = original.encode_persisted_json().expect("encode");
        let decoded = OperatorConfig::decode_persisted_json(&encoded).expect("decode");
        assert_eq!(decoded, original);
        assert_eq!(decoded.identity(), original.identity());
        assert!(decoded.idle_policy().matches_alpha_policy());
    }

    #[test]
    fn strict_decode_refuses_unknown_field_and_version() {
        let mut value: serde_json::Value =
            serde_json::from_slice(&config().encode_persisted_json().expect("encode"))
                .expect("document");
        value["unknown"] = serde_json::json!(true);
        let error = OperatorConfig::decode_persisted_json(
            &serde_json::to_vec(&value).expect("unknown field document"),
        )
        .expect_err("unknown field");
        assert_eq!(error.code, "invalid_document");

        value.as_object_mut().expect("object").remove("unknown");
        value["schema_version"] = serde_json::json!(2);
        let error = OperatorConfig::decode_persisted_json(
            &serde_json::to_vec(&value).expect("unknown version document"),
        )
        .expect_err("unknown version");
        assert_eq!(error.code, "unsupported_schema_version");
    }

    #[test]
    fn private_paths_fail_closed() {
        for value in [
            "relative/path",
            "/",
            "/tmp//worker",
            "/tmp/./worker",
            "/tmp/../worker",
            "/tmp/worker/",
            "/tmp/worker\nsecret",
        ] {
            assert!(PersonalWorkerStateRoot::parse(value).is_err(), "{value}");
            assert!(GuestWorkspacePath::parse(value).is_err(), "{value}");
        }
        let oversized = format!("/{}", "a".repeat(MAX_OPERATOR_CONFIG_PATH_BYTES));
        assert!(PersonalWorkerStateRoot::parse(oversized).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_private_paths_fail_closed() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let path = PathBuf::from(OsString::from_vec(vec![b'/', b't', b'm', b'p', b'/', 0xff]));
        assert!(PersonalWorkerStateRoot::parse(path.clone()).is_err());
        assert!(GuestWorkspacePath::parse(path).is_err());
    }

    #[test]
    fn delegated_identity_parsers_fail_closed() {
        let mut document: serde_json::Value =
            serde_json::from_slice(&config().encode_persisted_json().expect("encode"))
                .expect("document");
        document["lima_instance"] = serde_json::json!("--unsafe");
        let error = OperatorConfig::decode_persisted_json(
            &serde_json::to_vec(&document).expect("invalid instance document"),
        )
        .expect_err("invalid instance");
        assert_eq!(error.code, "invalid_lima_instance");

        document["lima_instance"] = serde_json::json!("smolrunner");
        document["default_verification_profile"] = serde_json::json!("../unsafe");
        let error = OperatorConfig::decode_persisted_json(
            &serde_json::to_vec(&document).expect("invalid profile document"),
        )
        .expect_err("invalid profile");
        assert_eq!(error.code, "invalid_verification_profile");
    }

    #[test]
    fn idle_policy_refuses_zero_reversed_and_excessive_values() {
        assert!(OperatorIdlePolicy::new(0, 1).is_err());
        assert!(OperatorIdlePolicy::new(1, 1).is_err());
        assert!(OperatorIdlePolicy::new(2, 1).is_err());
        assert!(OperatorIdlePolicy::new(1, MAX_OPERATOR_IDLE_MILLIS + 1).is_err());
    }

    #[test]
    fn every_closed_preference_round_trips() {
        for availability in [
            AvailabilityRequest::Active,
            AvailabilityRequest::Away,
            AvailabilityRequest::Off,
            AvailabilityRequest::Auto,
        ] {
            for output in [
                OperatorOutputPreference::Human,
                OperatorOutputPreference::Json,
            ] {
                for remediation in [
                    OperatorRemediationPreference::IncludeSuggestions,
                    OperatorRemediationPreference::CodesOnly,
                ] {
                    let original = config_with(
                        "/private/state",
                        "smolrunner",
                        "/home/lima/workspace",
                        "smolrunner.required",
                        availability,
                        alpha_idle_policy(),
                        output,
                        remediation,
                    );
                    let decoded = OperatorConfig::decode_persisted_json(
                        &original.encode_persisted_json().expect("encode"),
                    )
                    .expect("decode");
                    assert_eq!(decoded, original);
                }
            }
        }
    }

    #[test]
    fn identity_changes_for_every_semantic_field() {
        let baseline = config();
        let alternatives = [
            config_with(
                "/Users/other/state",
                "smolrunner",
                "/home/lima/smolrunner",
                "smolrunner.required",
                AvailabilityRequest::Active,
                alpha_idle_policy(),
                OperatorOutputPreference::Human,
                OperatorRemediationPreference::IncludeSuggestions,
            ),
            config_with(
                "/Users/private-user/Library/Application Support/smolrunner",
                "other-instance",
                "/home/lima/smolrunner",
                "smolrunner.required",
                AvailabilityRequest::Active,
                alpha_idle_policy(),
                OperatorOutputPreference::Human,
                OperatorRemediationPreference::IncludeSuggestions,
            ),
            config_with(
                "/Users/private-user/Library/Application Support/smolrunner",
                "smolrunner",
                "/home/lima/other",
                "smolrunner.required",
                AvailabilityRequest::Active,
                alpha_idle_policy(),
                OperatorOutputPreference::Human,
                OperatorRemediationPreference::IncludeSuggestions,
            ),
            config_with(
                "/Users/private-user/Library/Application Support/smolrunner",
                "smolrunner",
                "/home/lima/smolrunner",
                "smolrunner.doctor",
                AvailabilityRequest::Active,
                alpha_idle_policy(),
                OperatorOutputPreference::Human,
                OperatorRemediationPreference::IncludeSuggestions,
            ),
            config_with(
                "/Users/private-user/Library/Application Support/smolrunner",
                "smolrunner",
                "/home/lima/smolrunner",
                "smolrunner.required",
                AvailabilityRequest::Away,
                alpha_idle_policy(),
                OperatorOutputPreference::Human,
                OperatorRemediationPreference::IncludeSuggestions,
            ),
            config_with(
                "/Users/private-user/Library/Application Support/smolrunner",
                "smolrunner",
                "/home/lima/smolrunner",
                "smolrunner.required",
                AvailabilityRequest::Active,
                OperatorIdlePolicy::new(1_000, 2_000).expect("alternative idle policy"),
                OperatorOutputPreference::Human,
                OperatorRemediationPreference::IncludeSuggestions,
            ),
            config_with(
                "/Users/private-user/Library/Application Support/smolrunner",
                "smolrunner",
                "/home/lima/smolrunner",
                "smolrunner.required",
                AvailabilityRequest::Active,
                alpha_idle_policy(),
                OperatorOutputPreference::Json,
                OperatorRemediationPreference::IncludeSuggestions,
            ),
            config_with(
                "/Users/private-user/Library/Application Support/smolrunner",
                "smolrunner",
                "/home/lima/smolrunner",
                "smolrunner.required",
                AvailabilityRequest::Active,
                alpha_idle_policy(),
                OperatorOutputPreference::Human,
                OperatorRemediationPreference::CodesOnly,
            ),
        ];
        for alternative in alternatives {
            assert_ne!(alternative.identity(), baseline.identity());
        }
    }

    #[test]
    fn public_serialisation_and_debug_redact_private_paths() {
        let config = config();
        let state_sentinel = config.state_root().as_path().to_string_lossy();
        let workspace_sentinel = config.guest_workspace().as_path().to_string_lossy();

        let public_json = serde_json::to_string(&config).expect("public JSON");
        let public_debug = format!("{config:?}");
        let summary_json = serde_json::to_string(&config.public_summary()).expect("summary JSON");
        for public in [&public_json, &public_debug, &summary_json] {
            assert!(!public.contains(state_sentinel.as_ref()));
            assert!(!public.contains(workspace_sentinel.as_ref()));
        }
        assert!(public_debug.contains(REDACTED_STATE_ROOT));
        assert!(public_debug.contains(REDACTED_GUEST_WORKSPACE));

        let persisted = String::from_utf8(config.encode_persisted_json().expect("persisted JSON"))
            .expect("UTF-8");
        assert!(persisted.contains(state_sentinel.as_ref()));
        assert!(persisted.contains(workspace_sentinel.as_ref()));
    }

    #[test]
    fn module_contains_no_ambient_or_mutating_authority() {
        let source = include_str!("operator_config.rs");
        for forbidden in [
            concat!("std::", "env::"),
            concat!("std::", "fs::"),
            concat!("std::", "process::"),
            concat!("std::", "time::"),
            concat!("Command", "::"),
            concat!("System", "Time"),
            concat!("UnixPersonal", "WorkerStore"),
            concat!("lima", "ctl"),
            concat!("key", "chain"),
            concat!("git", "hub"),
        ] {
            assert!(!source.contains(forbidden), "forbidden token: {forbidden}");
        }
    }
}
