//! Canonical common envelope for one-shot resident Linux guest-control transactions.
//!
//! This module is pure protocol vocabulary. It performs no process execution, filesystem I/O,
//! privilege escalation, mount operation, Git operation, durable-state mutation, or guest
//! observation.
//!
//! Requests carry exact durable identities plus one closed operation tag and an opaque digest of a
//! later operation-specific payload. Decoded fields are claims. The Mac invocation adapter must
//! freshly re-confirm the attached project disk, resident sandbox, and installed guest binary
//! immediately before spawn, and the Linux handler must prove operation-specific authority before
//! doing any work.

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::project_catalog::ProjectIdentity;
use crate::project_disk_lease::{
    ProjectDiskAttachmentGeneration, ProjectDiskGeneration, ProjectDiskId, ProjectDiskLeaseRecord,
    ProjectDiskLeaseState, ProjectDiskRevision, ResidentSandboxGeneration, ResidentSandboxId,
};

pub const TRUSTED_GUEST_CONTROL_PROTOCOL_SCHEMA_VERSION: u8 = 1;
pub const MAX_TRUSTED_GUEST_CONTROL_REQUEST_BYTES: usize = 4 * 1024;
pub const MAX_TRUSTED_GUEST_CONTROL_RECEIPT_BYTES: usize = 2 * 1024;

const REQUEST_DIGEST_DOMAIN: &[u8] = b"smolrunner-trusted-guest-control-request-v1\0";
const MAX_REQUEST_ID_BYTES: usize = 64;
const SHA256_PREFIX: &str = "sha256:";
const HEX: &[u8; 16] = b"0123456789abcdef";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustedGuestControlArchitecture {
    LinuxAarch64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrustedGuestControlBinaryBinding {
    generation: u64,
    digest: Sha256Digest,
    architecture: TrustedGuestControlArchitecture,
}

impl TrustedGuestControlBinaryBinding {
    /// Bind one reviewed guest-control binary generation.
    ///
    /// This value is declaration data. The later invocation adapter must independently observe the
    /// exact installed root-owned executable before use.
    ///
    /// # Errors
    ///
    /// Returns a bounded error when `generation` is zero.
    pub fn new(
        generation: u64,
        digest: Sha256Digest,
        architecture: TrustedGuestControlArchitecture,
    ) -> Result<Self, TrustedGuestControlProtocolError> {
        if generation == 0 {
            return Err(invalid_identity());
        }
        Ok(Self {
            generation,
            digest,
            architecture,
        })
    }

    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub const fn digest(&self) -> &Sha256Digest {
        &self.digest
    }

    #[must_use]
    pub const fn architecture(&self) -> TrustedGuestControlArchitecture {
        self.architecture
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct TrustedGuestControlRequestId(String);

impl TrustedGuestControlRequestId {
    /// Parse one bounded replay/correlation token selected by the durable Mac control plane.
    ///
    /// # Errors
    ///
    /// Returns a bounded error unless the token is lowercase ASCII alphanumeric text separated by
    /// optional single hyphens.
    pub fn parse(value: &str) -> Result<Self, TrustedGuestControlProtocolError> {
        if value.is_empty()
            || value.len() > MAX_REQUEST_ID_BYTES
            || value.starts_with('-')
            || value.ends_with('-')
            || value.contains("--")
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(invalid_identity());
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Exact durable attachment identity carried by one guest-control request.
///
/// This is request data, not an authority token. A decoded value can only be trusted after the Mac
/// adapter compares every field with the live durable lease immediately before invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrustedGuestControlAuthority {
    project: ProjectIdentity,
    project_disk_id: ProjectDiskId,
    project_disk_generation: ProjectDiskGeneration,
    project_disk_revision: ProjectDiskRevision,
    attachment_generation: ProjectDiskAttachmentGeneration,
    resident_sandbox_id: ResidentSandboxId,
    resident_sandbox_generation: ResidentSandboxGeneration,
}

impl TrustedGuestControlAuthority {
    /// Derive request identity from one currently attached project-disk lease.
    ///
    /// # Errors
    ///
    /// Returns a bounded error unless the record is currently attached.
    pub fn from_attached_project_disk(
        record: &ProjectDiskLeaseRecord,
    ) -> Result<Self, TrustedGuestControlProtocolError> {
        let ProjectDiskLeaseState::Attached { attachment } = record.state() else {
            return Err(invalid_authority());
        };
        Ok(Self {
            project: record.project().clone(),
            project_disk_id: record.disk_id().clone(),
            project_disk_generation: record.disk_generation(),
            project_disk_revision: record.revision(),
            attachment_generation: attachment.generation(),
            resident_sandbox_id: attachment.sandbox_id().clone(),
            resident_sandbox_generation: attachment.sandbox_generation(),
        })
    }

    #[must_use]
    pub const fn project(&self) -> &ProjectIdentity {
        &self.project
    }

    #[must_use]
    pub const fn project_disk_id(&self) -> &ProjectDiskId {
        &self.project_disk_id
    }

    #[must_use]
    pub const fn project_disk_generation(&self) -> ProjectDiskGeneration {
        self.project_disk_generation
    }

    #[must_use]
    pub const fn project_disk_revision(&self) -> ProjectDiskRevision {
        self.project_disk_revision
    }

    #[must_use]
    pub const fn attachment_generation(&self) -> ProjectDiskAttachmentGeneration {
        self.attachment_generation
    }

    #[must_use]
    pub const fn resident_sandbox_id(&self) -> &ResidentSandboxId {
        &self.resident_sandbox_id
    }

    #[must_use]
    pub const fn resident_sandbox_generation(&self) -> ResidentSandboxGeneration {
        self.resident_sandbox_generation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustedGuestControlOperation {
    ObserveProjectFilesystem,
    ObserveImmutableGitPool,
    PublishImmutableGitPoolGeneration,
    PrepareTrustedTaskView,
    ObserveTrustedTaskView,
    CleanupTrustedTaskView,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedGuestControlRequest {
    request_id: TrustedGuestControlRequestId,
    binary: TrustedGuestControlBinaryBinding,
    authority: TrustedGuestControlAuthority,
    operation: TrustedGuestControlOperation,
    payload_digest: Sha256Digest,
}

impl TrustedGuestControlRequest {
    #[must_use]
    pub const fn new(
        request_id: TrustedGuestControlRequestId,
        binary: TrustedGuestControlBinaryBinding,
        authority: TrustedGuestControlAuthority,
        operation: TrustedGuestControlOperation,
        payload_digest: Sha256Digest,
    ) -> Self {
        Self {
            request_id,
            binary,
            authority,
            operation,
            payload_digest,
        }
    }

    #[must_use]
    pub const fn request_id(&self) -> &TrustedGuestControlRequestId {
        &self.request_id
    }

    #[must_use]
    pub const fn binary(&self) -> &TrustedGuestControlBinaryBinding {
        &self.binary
    }

    #[must_use]
    pub const fn authority(&self) -> &TrustedGuestControlAuthority {
        &self.authority
    }

    #[must_use]
    pub const fn operation(&self) -> TrustedGuestControlOperation {
        self.operation
    }

    #[must_use]
    pub const fn payload_digest(&self) -> &Sha256Digest {
        &self.payload_digest
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustedGuestControlRefusal {
    AuthorityChanged,
    CorrelationUnproven,
    UnsupportedOperation,
    InvalidPayload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrustedGuestControlRecoveryDebt {
    #[serde(rename = "revalidation_required")]
    Revalidation,
    #[serde(rename = "workdir_reset_required")]
    WorkdirReset,
    #[serde(rename = "mount_cleanup_required")]
    MountCleanup,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustedGuestControlOutcome {
    Succeeded { result_digest: Sha256Digest },
    Refused(TrustedGuestControlRefusal),
    RecoveryRequired(TrustedGuestControlRecoveryDebt),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedGuestControlReceipt {
    request_id: TrustedGuestControlRequestId,
    binary: TrustedGuestControlBinaryBinding,
    request_digest: Sha256Digest,
    operation: TrustedGuestControlOperation,
    outcome: TrustedGuestControlOutcome,
}

impl TrustedGuestControlReceipt {
    #[must_use]
    pub const fn request_id(&self) -> &TrustedGuestControlRequestId {
        &self.request_id
    }

    #[must_use]
    pub const fn binary(&self) -> &TrustedGuestControlBinaryBinding {
        &self.binary
    }

    #[must_use]
    pub const fn request_digest(&self) -> &Sha256Digest {
        &self.request_digest
    }

    #[must_use]
    pub const fn operation(&self) -> TrustedGuestControlOperation {
        self.operation
    }

    #[must_use]
    pub const fn outcome(&self) -> &TrustedGuestControlOutcome {
        &self.outcome
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustedGuestControlProtocolErrorKind {
    InvalidIdentity,
    InvalidAuthority,
    TooLarge,
    Malformed,
    VersionIncompatible,
    NonCanonical,
    ReceiptMismatch,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TrustedGuestControlProtocolError {
    kind: TrustedGuestControlProtocolErrorKind,
    code: &'static str,
    message: &'static str,
}

impl TrustedGuestControlProtocolError {
    #[must_use]
    pub const fn kind(self) -> TrustedGuestControlProtocolErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for TrustedGuestControlProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedGuestControlProtocolError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for TrustedGuestControlProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for TrustedGuestControlProtocolError {}

/// Encode one strict compact request followed by a single newline.
///
/// # Errors
///
/// Returns a bounded error if serialization fails or the request exceeds its fixed byte limit.
pub fn encode_trusted_guest_control_request(
    request: &TrustedGuestControlRequest,
) -> Result<Vec<u8>, TrustedGuestControlProtocolError> {
    canonical_json(
        &RequestWire::from(request),
        MAX_TRUSTED_GUEST_CONTROL_REQUEST_BYTES,
    )
}

/// Decode one bounded canonical request without granting any execution authority.
///
/// # Errors
///
/// Returns a bounded error for oversized, malformed, unsupported, invalid, or noncanonical bytes.
pub fn decode_trusted_guest_control_request(
    bytes: &[u8],
) -> Result<TrustedGuestControlRequest, TrustedGuestControlProtocolError> {
    require_size(bytes, MAX_TRUSTED_GUEST_CONTROL_REQUEST_BYTES)?;
    require_version(bytes)?;
    let wire: RequestWire = serde_json::from_slice(bytes).map_err(|_| malformed())?;
    let request = request_from_wire(wire)?;
    if encode_trusted_guest_control_request(&request)? != bytes {
        return Err(noncanonical());
    }
    Ok(request)
}

/// Derive the domain-separated digest every terminal receipt must bind.
///
/// # Errors
///
/// Returns a bounded error if the request cannot be canonically encoded.
pub fn trusted_guest_control_request_digest(
    request: &TrustedGuestControlRequest,
) -> Result<Sha256Digest, TrustedGuestControlProtocolError> {
    let mut hasher = Sha256::new();
    hasher.update(REQUEST_DIGEST_DOMAIN);
    hasher.update(encode_trusted_guest_control_request(request)?);
    raw_digest(&hasher.finalize())
}

/// Encode one terminal outcome for an exact request.
///
/// Receipt bytes remain observation data until the later one-shot process adapter binds them to the
/// verified spawned executable and performs fresh durable reconciliation.
///
/// # Errors
///
/// Returns a bounded error if serialization fails or the receipt exceeds its fixed byte limit.
pub fn encode_trusted_guest_control_receipt(
    request: &TrustedGuestControlRequest,
    outcome: &TrustedGuestControlOutcome,
) -> Result<Vec<u8>, TrustedGuestControlProtocolError> {
    let wire = ReceiptWire {
        schema_version: TRUSTED_GUEST_CONTROL_PROTOCOL_SCHEMA_VERSION,
        request_id: request.request_id.as_str().to_owned(),
        binary: BinaryWire::from(&request.binary),
        request_digest: trusted_guest_control_request_digest(request)?
            .as_str()
            .to_owned(),
        operation: request.operation,
        outcome: OutcomeWire::from(outcome),
    };
    canonical_json(&wire, MAX_TRUSTED_GUEST_CONTROL_RECEIPT_BYTES)
}

/// Decode a terminal receipt only against the exact request that caused the guest transaction.
///
/// # Errors
///
/// Returns a bounded error for malformed/noncanonical bytes or any request, binary, operation, or
/// request-digest mismatch.
pub fn decode_trusted_guest_control_receipt(
    bytes: &[u8],
    expected_request: &TrustedGuestControlRequest,
) -> Result<TrustedGuestControlReceipt, TrustedGuestControlProtocolError> {
    require_size(bytes, MAX_TRUSTED_GUEST_CONTROL_RECEIPT_BYTES)?;
    require_version(bytes)?;
    let wire: ReceiptWire = serde_json::from_slice(bytes).map_err(|_| malformed())?;
    let receipt = receipt_from_wire(wire)?;
    if receipt.request_id != expected_request.request_id
        || receipt.binary != expected_request.binary
        || receipt.operation != expected_request.operation
        || receipt.request_digest != trusted_guest_control_request_digest(expected_request)?
    {
        return Err(receipt_mismatch());
    }
    if encode_trusted_guest_control_receipt(expected_request, &receipt.outcome)? != bytes {
        return Err(noncanonical());
    }
    Ok(receipt)
}

fn require_size(bytes: &[u8], limit: usize) -> Result<(), TrustedGuestControlProtocolError> {
    if bytes.len() > limit {
        return Err(too_large());
    }
    Ok(())
}

fn require_version(bytes: &[u8]) -> Result<(), TrustedGuestControlProtocolError> {
    let version: VersionWire = serde_json::from_slice(bytes).map_err(|_| malformed())?;
    if version.schema_version != TRUSTED_GUEST_CONTROL_PROTOCOL_SCHEMA_VERSION {
        return Err(version_incompatible());
    }
    Ok(())
}

fn canonical_json(
    value: &impl Serialize,
    limit: usize,
) -> Result<Vec<u8>, TrustedGuestControlProtocolError> {
    let mut bytes = serde_json::to_vec(value).map_err(|_| malformed())?;
    bytes.push(b'\n');
    require_size(&bytes, limit)?;
    Ok(bytes)
}

#[derive(Deserialize)]
struct VersionWire {
    schema_version: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BinaryWire {
    generation: u64,
    digest: String,
    architecture: TrustedGuestControlArchitecture,
}

impl From<&TrustedGuestControlBinaryBinding> for BinaryWire {
    fn from(value: &TrustedGuestControlBinaryBinding) -> Self {
        Self {
            generation: value.generation,
            digest: value.digest.as_str().to_owned(),
            architecture: value.architecture,
        }
    }
}

fn binary_from_wire(
    wire: BinaryWire,
) -> Result<TrustedGuestControlBinaryBinding, TrustedGuestControlProtocolError> {
    TrustedGuestControlBinaryBinding::new(
        wire.generation,
        Sha256Digest::parse(&wire.digest).map_err(|_| invalid_identity())?,
        wire.architecture,
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorityWire {
    project: String,
    project_disk_id: String,
    project_disk_generation: u64,
    project_disk_revision: u64,
    attachment_generation: u64,
    resident_sandbox_id: String,
    resident_sandbox_generation: u64,
}

impl From<&TrustedGuestControlAuthority> for AuthorityWire {
    fn from(value: &TrustedGuestControlAuthority) -> Self {
        Self {
            project: value.project.as_str().to_owned(),
            project_disk_id: value.project_disk_id.as_str().to_owned(),
            project_disk_generation: value.project_disk_generation.get(),
            project_disk_revision: value.project_disk_revision.get(),
            attachment_generation: value.attachment_generation.get(),
            resident_sandbox_id: value.resident_sandbox_id.as_str().to_owned(),
            resident_sandbox_generation: value.resident_sandbox_generation.get(),
        }
    }
}

fn authority_from_wire(
    wire: AuthorityWire,
) -> Result<TrustedGuestControlAuthority, TrustedGuestControlProtocolError> {
    Ok(TrustedGuestControlAuthority {
        project: ProjectIdentity::parse(&wire.project).map_err(|_| invalid_authority())?,
        project_disk_id: ProjectDiskId::parse(&wire.project_disk_id)
            .map_err(|_| invalid_authority())?,
        project_disk_generation: ProjectDiskGeneration::new(wire.project_disk_generation)
            .map_err(|_| invalid_authority())?,
        project_disk_revision: ProjectDiskRevision::new(wire.project_disk_revision)
            .map_err(|_| invalid_authority())?,
        attachment_generation: ProjectDiskAttachmentGeneration::new(wire.attachment_generation)
            .map_err(|_| invalid_authority())?,
        resident_sandbox_id: ResidentSandboxId::parse(&wire.resident_sandbox_id)
            .map_err(|_| invalid_authority())?,
        resident_sandbox_generation: ResidentSandboxGeneration::new(
            wire.resident_sandbox_generation,
        )
        .map_err(|_| invalid_authority())?,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestWire {
    schema_version: u8,
    request_id: String,
    binary: BinaryWire,
    authority: AuthorityWire,
    operation: TrustedGuestControlOperation,
    payload_digest: String,
}

impl From<&TrustedGuestControlRequest> for RequestWire {
    fn from(value: &TrustedGuestControlRequest) -> Self {
        Self {
            schema_version: TRUSTED_GUEST_CONTROL_PROTOCOL_SCHEMA_VERSION,
            request_id: value.request_id.as_str().to_owned(),
            binary: BinaryWire::from(&value.binary),
            authority: AuthorityWire::from(&value.authority),
            operation: value.operation,
            payload_digest: value.payload_digest.as_str().to_owned(),
        }
    }
}

fn request_from_wire(
    wire: RequestWire,
) -> Result<TrustedGuestControlRequest, TrustedGuestControlProtocolError> {
    if wire.schema_version != TRUSTED_GUEST_CONTROL_PROTOCOL_SCHEMA_VERSION {
        return Err(version_incompatible());
    }
    Ok(TrustedGuestControlRequest::new(
        TrustedGuestControlRequestId::parse(&wire.request_id)?,
        binary_from_wire(wire.binary)?,
        authority_from_wire(wire.authority)?,
        wire.operation,
        Sha256Digest::parse(&wire.payload_digest).map_err(|_| malformed())?,
    ))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum OutcomeWire {
    Succeeded {
        result_digest: String,
    },
    Refused {
        reason: TrustedGuestControlRefusal,
    },
    RecoveryRequired {
        debt: TrustedGuestControlRecoveryDebt,
    },
}

impl From<&TrustedGuestControlOutcome> for OutcomeWire {
    fn from(value: &TrustedGuestControlOutcome) -> Self {
        match value {
            TrustedGuestControlOutcome::Succeeded { result_digest } => Self::Succeeded {
                result_digest: result_digest.as_str().to_owned(),
            },
            TrustedGuestControlOutcome::Refused(reason) => Self::Refused { reason: *reason },
            TrustedGuestControlOutcome::RecoveryRequired(debt) => {
                Self::RecoveryRequired { debt: *debt }
            }
        }
    }
}

fn outcome_from_wire(
    wire: OutcomeWire,
) -> Result<TrustedGuestControlOutcome, TrustedGuestControlProtocolError> {
    match wire {
        OutcomeWire::Succeeded { result_digest } => Ok(TrustedGuestControlOutcome::Succeeded {
            result_digest: Sha256Digest::parse(&result_digest).map_err(|_| malformed())?,
        }),
        OutcomeWire::Refused { reason } => Ok(TrustedGuestControlOutcome::Refused(reason)),
        OutcomeWire::RecoveryRequired { debt } => {
            Ok(TrustedGuestControlOutcome::RecoveryRequired(debt))
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptWire {
    schema_version: u8,
    request_id: String,
    binary: BinaryWire,
    request_digest: String,
    operation: TrustedGuestControlOperation,
    outcome: OutcomeWire,
}

fn receipt_from_wire(
    wire: ReceiptWire,
) -> Result<TrustedGuestControlReceipt, TrustedGuestControlProtocolError> {
    if wire.schema_version != TRUSTED_GUEST_CONTROL_PROTOCOL_SCHEMA_VERSION {
        return Err(version_incompatible());
    }
    Ok(TrustedGuestControlReceipt {
        request_id: TrustedGuestControlRequestId::parse(&wire.request_id)
            .map_err(|_| malformed())?,
        binary: binary_from_wire(wire.binary).map_err(|_| malformed())?,
        request_digest: Sha256Digest::parse(&wire.request_digest).map_err(|_| malformed())?,
        operation: wire.operation,
        outcome: outcome_from_wire(wire.outcome)?,
    })
}

fn raw_digest(bytes: &[u8]) -> Result<Sha256Digest, TrustedGuestControlProtocolError> {
    let mut value = String::with_capacity(SHA256_PREFIX.len() + bytes.len() * 2);
    value.push_str(SHA256_PREFIX);
    for byte in bytes {
        value.push(char::from(HEX[usize::from(byte >> 4)]));
        value.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Sha256Digest::parse(&value).map_err(|_| malformed())
}

const fn protocol_error(
    kind: TrustedGuestControlProtocolErrorKind,
    code: &'static str,
    message: &'static str,
) -> TrustedGuestControlProtocolError {
    TrustedGuestControlProtocolError {
        kind,
        code,
        message,
    }
}

const fn invalid_identity() -> TrustedGuestControlProtocolError {
    protocol_error(
        TrustedGuestControlProtocolErrorKind::InvalidIdentity,
        "trusted_guest_control_identity_invalid",
        "trusted guest-control identity is invalid",
    )
}

const fn invalid_authority() -> TrustedGuestControlProtocolError {
    protocol_error(
        TrustedGuestControlProtocolErrorKind::InvalidAuthority,
        "trusted_guest_control_authority_invalid",
        "trusted guest-control attachment authority is invalid",
    )
}

const fn too_large() -> TrustedGuestControlProtocolError {
    protocol_error(
        TrustedGuestControlProtocolErrorKind::TooLarge,
        "trusted_guest_control_document_too_large",
        "trusted guest-control document exceeds its bounded size",
    )
}

const fn malformed() -> TrustedGuestControlProtocolError {
    protocol_error(
        TrustedGuestControlProtocolErrorKind::Malformed,
        "trusted_guest_control_document_malformed",
        "trusted guest-control document is malformed",
    )
}

const fn version_incompatible() -> TrustedGuestControlProtocolError {
    protocol_error(
        TrustedGuestControlProtocolErrorKind::VersionIncompatible,
        "trusted_guest_control_protocol_incompatible",
        "trusted guest-control protocol version is incompatible",
    )
}

const fn noncanonical() -> TrustedGuestControlProtocolError {
    protocol_error(
        TrustedGuestControlProtocolErrorKind::NonCanonical,
        "trusted_guest_control_document_noncanonical",
        "trusted guest-control document is noncanonical",
    )
}

const fn receipt_mismatch() -> TrustedGuestControlProtocolError {
    protocol_error(
        TrustedGuestControlProtocolErrorKind::ReceiptMismatch,
        "trusted_guest_control_receipt_mismatch",
        "trusted guest-control receipt does not match the exact request",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_disk_lease::{
        ProjectDiskLockObservation, ProjectDiskObservation, ProjectDiskPhysicalObservation,
        ProjectDiskRecoverability, ProjectDiskUseObservation,
    };

    fn digest(byte: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    fn attached_record() -> ProjectDiskLeaseRecord {
        let detached = ProjectDiskLeaseRecord::new_detached(
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            ProjectDiskId::parse("disk-a").unwrap(),
            ProjectDiskGeneration::new(3).unwrap(),
        );
        let plan = detached
            .plan_attach(
                ResidentSandboxId::parse("sandbox-a").unwrap(),
                ResidentSandboxGeneration::new(11).unwrap(),
                ProjectDiskObservation::new(
                    ProjectDiskPhysicalObservation::Exact,
                    ProjectDiskUseObservation::Unused,
                    ProjectDiskLockObservation::Unlocked,
                    ProjectDiskRecoverability::Rebuildable,
                ),
            )
            .unwrap();
        detached
            .record_attach_success(
                &plan,
                ProjectDiskObservation::new(
                    ProjectDiskPhysicalObservation::Exact,
                    ProjectDiskUseObservation::CurrentAttachment,
                    ProjectDiskLockObservation::CurrentAttachment,
                    ProjectDiskRecoverability::Rebuildable,
                ),
            )
            .unwrap()
    }

    fn request() -> TrustedGuestControlRequest {
        let record = attached_record();
        TrustedGuestControlRequest::new(
            TrustedGuestControlRequestId::parse("request-1").unwrap(),
            TrustedGuestControlBinaryBinding::new(
                7,
                digest('a'),
                TrustedGuestControlArchitecture::LinuxAarch64,
            )
            .unwrap(),
            TrustedGuestControlAuthority::from_attached_project_disk(&record).unwrap(),
            TrustedGuestControlOperation::PrepareTrustedTaskView,
            digest('b'),
        )
    }

    #[test]
    fn attached_authority_binds_current_revision_and_attachment_generation() {
        let authority =
            TrustedGuestControlAuthority::from_attached_project_disk(&attached_record()).unwrap();
        assert_eq!(authority.project_disk_revision().get(), 2);
        assert_eq!(authority.attachment_generation().get(), 1);
        assert_eq!(authority.resident_sandbox_generation().get(), 11);
    }

    #[test]
    fn detached_disk_cannot_construct_request_authority() {
        let detached = ProjectDiskLeaseRecord::new_detached(
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            ProjectDiskId::parse("disk-a").unwrap(),
            ProjectDiskGeneration::new(3).unwrap(),
        );
        assert_eq!(
            TrustedGuestControlAuthority::from_attached_project_disk(&detached)
                .unwrap_err()
                .kind(),
            TrustedGuestControlProtocolErrorKind::InvalidAuthority
        );
    }

    #[test]
    fn canonical_request_round_trips_without_command_or_path_surface() {
        let request = request();
        let bytes = encode_trusted_guest_control_request(&request).unwrap();
        assert_eq!(
            decode_trusted_guest_control_request(&bytes).unwrap(),
            request
        );
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(!text.contains("\"argv\""));
        assert!(!text.contains("\"environment\""));
        assert!(!text.contains("\"path\""));
        assert!(bytes.ends_with(b"\n"));
    }

    #[test]
    fn unknown_version_noncanonical_and_arbitrary_operation_fail_closed() {
        let bytes = encode_trusted_guest_control_request(&request()).unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();

        let unknown = text.replacen(
            "\"schema_version\":1",
            "\"schema_version\":1,\"extra\":true",
            1,
        );
        assert_eq!(
            decode_trusted_guest_control_request(unknown.as_bytes())
                .unwrap_err()
                .kind(),
            TrustedGuestControlProtocolErrorKind::Malformed
        );

        let future = text.replacen("\"schema_version\":1", "\"schema_version\":2", 1);
        assert_eq!(
            decode_trusted_guest_control_request(future.as_bytes())
                .unwrap_err()
                .kind(),
            TrustedGuestControlProtocolErrorKind::VersionIncompatible
        );

        let arbitrary = text.replace(
            "\"operation\":\"prepare_trusted_task_view\"",
            "\"operation\":\"run_command\"",
        );
        assert_eq!(
            decode_trusted_guest_control_request(arbitrary.as_bytes())
                .unwrap_err()
                .kind(),
            TrustedGuestControlProtocolErrorKind::Malformed
        );

        let mut spaced = bytes;
        spaced.insert(0, b' ');
        assert_eq!(
            decode_trusted_guest_control_request(&spaced)
                .unwrap_err()
                .kind(),
            TrustedGuestControlProtocolErrorKind::NonCanonical
        );
    }

    #[test]
    fn immutable_pool_publication_operation_has_exact_closed_wire_value() {
        let mut publication = request();
        publication.operation = TrustedGuestControlOperation::PublishImmutableGitPoolGeneration;
        let bytes = encode_trusted_guest_control_request(&publication).unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(text.contains("\"operation\":\"publish_immutable_git_pool_generation\""));
        assert_eq!(
            decode_trusted_guest_control_request(&bytes).unwrap(),
            publication
        );
    }

    #[test]
    fn oversized_documents_fail_before_decode() {
        let oversized = vec![b'x'; MAX_TRUSTED_GUEST_CONTROL_REQUEST_BYTES + 1];
        assert_eq!(
            decode_trusted_guest_control_request(&oversized)
                .unwrap_err()
                .kind(),
            TrustedGuestControlProtocolErrorKind::TooLarge
        );
    }

    #[test]
    fn request_digest_changes_with_attachment_binary_operation_and_payload() {
        let original = request();
        let original_digest = trusted_guest_control_request_digest(&original).unwrap();

        let mut attachment = request();
        attachment.authority.attachment_generation =
            ProjectDiskAttachmentGeneration::new(2).unwrap();
        assert_ne!(
            original_digest,
            trusted_guest_control_request_digest(&attachment).unwrap()
        );

        let mut binary = request();
        binary.binary = TrustedGuestControlBinaryBinding::new(
            8,
            digest('a'),
            TrustedGuestControlArchitecture::LinuxAarch64,
        )
        .unwrap();
        assert_ne!(
            original_digest,
            trusted_guest_control_request_digest(&binary).unwrap()
        );

        let mut operation = request();
        operation.operation = TrustedGuestControlOperation::ObserveTrustedTaskView;
        assert_ne!(
            original_digest,
            trusted_guest_control_request_digest(&operation).unwrap()
        );

        let mut payload = request();
        payload.payload_digest = digest('c');
        assert_ne!(
            original_digest,
            trusted_guest_control_request_digest(&payload).unwrap()
        );
    }

    #[test]
    fn receipt_round_trip_requires_exact_request() {
        let request = request();
        let outcome = TrustedGuestControlOutcome::Succeeded {
            result_digest: digest('d'),
        };
        let bytes = encode_trusted_guest_control_receipt(&request, &outcome).unwrap();
        let receipt = decode_trusted_guest_control_receipt(&bytes, &request).unwrap();
        assert_eq!(receipt.outcome(), &outcome);
        assert_eq!(
            receipt.request_digest(),
            &trusted_guest_control_request_digest(&request).unwrap()
        );

        let mut other = request.clone();
        other.request_id = TrustedGuestControlRequestId::parse("request-2").unwrap();
        assert_eq!(
            decode_trusted_guest_control_receipt(&bytes, &other)
                .unwrap_err()
                .kind(),
            TrustedGuestControlProtocolErrorKind::ReceiptMismatch
        );
    }

    #[test]
    fn refusal_and_recovery_wire_values_remain_closed_and_explicit() {
        let request = request();
        for (outcome, marker) in [
            (
                TrustedGuestControlOutcome::Refused(TrustedGuestControlRefusal::AuthorityChanged),
                "authority_changed",
            ),
            (
                TrustedGuestControlOutcome::RecoveryRequired(
                    TrustedGuestControlRecoveryDebt::WorkdirReset,
                ),
                "workdir_reset_required",
            ),
            (
                TrustedGuestControlOutcome::RecoveryRequired(
                    TrustedGuestControlRecoveryDebt::MountCleanup,
                ),
                "mount_cleanup_required",
            ),
        ] {
            let bytes = encode_trusted_guest_control_receipt(&request, &outcome).unwrap();
            assert!(std::str::from_utf8(&bytes).unwrap().contains(marker));
            assert_eq!(
                decode_trusted_guest_control_receipt(&bytes, &request)
                    .unwrap()
                    .outcome(),
                &outcome
            );
        }
    }
}
