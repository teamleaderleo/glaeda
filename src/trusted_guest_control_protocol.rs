//! Canonical common envelope for one-shot resident Linux guest-control transactions.
//!
//! This module is pure protocol vocabulary. It performs no process execution, filesystem I/O,
//! privilege escalation, mount operation, Git operation, durable-state mutation, or guest
//! observation.
//!
//! A request binds the exact current project-disk attachment generation and one reviewed guest
//! binary generation. The operation tag is closed. Operation-specific payloads are represented here
//! only by a canonical SHA-256 digest; later per-operation codecs must validate their own typed
//! bounded documents before any Linux-local authority can be exercised.
//!
//! Encoding a request or receipt grants no execution authority. The Mac invocation adapter must
//! re-confirm the durable attachment and installed guest binary immediately before spawn, and the
//! guest handler must freshly prove operation-specific authority.

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
const SHA256_PREFIX: &str = "sha256:";
const HEX: &[u8; 16] = b"0123456789abcdef";
const MAX_REQUEST_ID_BYTES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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
    /// Bind one already-reviewed guest-control binary generation.
    ///
    /// This is declaration data only. The Mac adapter still has to observe the exact installed
    /// root-owned executable generation immediately before invocation.
    ///
    /// # Errors
    ///
    /// Returns a bounded error when the generation is zero.
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
    /// Parse one bounded replay/correlation token chosen by the Mac durable control plane.
    ///
    /// # Errors
    ///
    /// Returns a bounded error unless the token is canonical lowercase ASCII letters/digits with
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

/// Exact Mac-side durable identity carried by one guest transaction.
///
/// The public constructor accepts only a currently attached project-disk record. The resulting
/// value still requires a fresh record comparison at the Mac process boundary; it is request data,
/// not durable authority by itself.
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
    /// Derive the request authority from one exact current attached lease.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustedGuestControlOperation {
    ObserveProjectFilesystem,
    ObserveImmutableGitPool,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustedGuestControlRefusal {
    AuthorityChanged,
    CorrelationUnproven,
    UnsupportedOperation,
    InvalidPayload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustedGuestControlRecoveryDebt {
    RevalidationRequired,
    WorkdirResetRequired,
    MountCleanupRequired,
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

/// Encode one request as strict compact JSON followed by one newline.
///
/// # Errors
///
/// Returns a bounded error when encoding exceeds the fixed request size.
pub fn encode_trusted_guest_control_request(
    request: &TrustedGuestControlRequest,
) -> Result<Vec<u8>, TrustedGuestControlProtocolError> {
    canonical_json(
        &RequestWire::from(request),
        MAX_TRUSTED_GUEST_CONTROL_REQUEST_BYTES,
    )
}

/// Decode one strict canonical bounded request without executing anything.
///
/// # Errors
///
/// Returns a bounded error for malformed, unsupported, invalid, oversized, or noncanonical bytes.
pub fn decode_trusted_guest_control_request(
    bytes: &[u8],
) -> Result<TrustedGuestControlRequest, TrustedGuestControlProtocolError> {
    if bytes.len() > MAX_TRUSTED_GUEST_CONTROL_REQUEST_BYTES {
        return Err(too_large());
    }
    let version: VersionWire = serde_json::from_slice(bytes).map_err(|_| malformed())?;
    if version.schema_version != TRUSTED_GUEST_CONTROL_PROTOCOL_SCHEMA_VERSION {
        return Err(version_incompatible());
    }
    let wire: RequestWire = serde_json::from_slice(bytes).map_err(|_| malformed())?;
    let request = request_from_wire(wire)?;
    if encode_trusted_guest_control_request(&request)? != bytes {
        return Err(noncanonical());
    }
    Ok(request)
}

/// Derive the transaction digest that every receipt must bind.
///
/// # Errors
///
/// Returns a bounded error if the request cannot be encoded.
pub fn trusted_guest_control_request_digest(
    request: &TrustedGuestControlRequest,
) -> Result<Sha256Digest, TrustedGuestControlProtocolError> {
    let bytes = encode_trusted_guest_control_request(request)?;
    let mut hasher = Sha256::new();
    hasher.update(REQUEST_DIGEST_DOMAIN);
    hasher.update(bytes);
    raw_digest(&hasher.finalize())
}

/// Encode one guest result for the exact request.
///
/// This only creates protocol bytes. Receipt bytes gain meaning from the separately verified
/// one-shot guest process plus fresh Mac reconciliation.
///
/// # Errors
///
/// Returns a bounded error when the receipt exceeds the fixed size.
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
        operation: request.operation.into(),
        outcome: OutcomeWire::from(outcome),
    };
    canonical_json(&wire, MAX_TRUSTED_GUEST_CONTROL_RECEIPT_BYTES)
}

/// Decode one receipt only against the exact request that caused the one-shot guest process.
///
/// # Errors
///
/// Returns a bounded error for malformed/noncanonical bytes or any request/binary/digest/operation
/// mismatch.
pub fn decode_trusted_guest_control_receipt(
    bytes: &[u8],
    expected_request: &TrustedGuestControlRequest,
) -> Result<TrustedGuestControlReceipt, TrustedGuestControlProtocolError> {
    if bytes.len() > MAX_TRUSTED_GUEST_CONTROL_RECEIPT_BYTES {
        return Err(too_large());
    }
    let version: VersionWire = serde_json::from_slice(bytes).map_err(|_| malformed())?;
    if version.schema_version != TRUSTED_GUEST_CONTROL_PROTOCOL_SCHEMA_VERSION {
        return Err(version_incompatible());
    }
    let wire: ReceiptWire = serde_json::from_slice(bytes).map_err(|_| malformed())?;
    let receipt = receipt_from_wire(wire)?;
    if receipt.request_id != expected_request.request_id
        || receipt.binary != expected_request.binary
        || receipt.operation != expected_request.operation
        || receipt.request_digest != trusted_guest_control_request_digest(expected_request)?
    {
        return Err(receipt_mismatch());
    }
    let canonical = encode_trusted_guest_control_receipt(expected_request, &receipt.outcome)?;
    if canonical != bytes {
        return Err(noncanonical());
    }
    Ok(receipt)
}

fn canonical_json(
    value: &impl Serialize,
    limit: usize,
) -> Result<Vec<u8>, TrustedGuestControlProtocolError> {
    let mut bytes = serde_json::to_vec(value).map_err(|_| malformed())?;
    bytes.push(b'\n');
    if bytes.len() > limit {
        return Err(too_large());
    }
    Ok(bytes)
}

#[derive(Debug, Deserialize)]
struct VersionWire {
    schema_version: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestWire {
    schema_version: u8,
    request_id: String,
    binary: BinaryWire,
    authority: AuthorityWire,
    operation: OperationWire,
    payload_digest: String,
}

impl From<&TrustedGuestControlRequest> for RequestWire {
    fn from(request: &TrustedGuestControlRequest) -> Self {
        Self {
            schema_version: TRUSTED_GUEST_CONTROL_PROTOCOL_SCHEMA_VERSION,
            request_id: request.request_id.as_str().to_owned(),
            binary: BinaryWire::from(&request.binary),
            authority: AuthorityWire::from(&request.authority),
            operation: request.operation.into(),
            payload_digest: request.payload_digest.as_str().to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BinaryWire {
    generation: u64,
    digest: String,
    architecture: ArchitectureWire,
}

impl From<&TrustedGuestControlBinaryBinding> for BinaryWire {
    fn from(binding: &TrustedGuestControlBinaryBinding) -> Self {
        Self {
            generation: binding.generation,
            digest: binding.digest.as_str().to_owned(),
            architecture: match binding.architecture {
                TrustedGuestControlArchitecture::LinuxAarch64 => ArchitectureWire::LinuxAarch64,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ArchitectureWire {
    LinuxAarch64,
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
    fn from(authority: &TrustedGuestControlAuthority) -> Self {
        Self {
            project: authority.project.as_str().to_owned(),
            project_disk_id: authority.project_disk_id.as_str().to_owned(),
            project_disk_generation: authority.project_disk_generation.get(),
            project_disk_revision: authority.project_disk_revision.get(),
            attachment_generation: authority.attachment_generation.get(),
            resident_sandbox_id: authority.resident_sandbox_id.as_str().to_owned(),
            resident_sandbox_generation: authority.resident_sandbox_generation.get(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OperationWire {
    ObserveProjectFilesystem,
    ObserveImmutableGitPool,
    PrepareTrustedTaskView,
    ObserveTrustedTaskView,
    CleanupTrustedTaskView,
}

impl From<TrustedGuestControlOperation> for OperationWire {
    fn from(operation: TrustedGuestControlOperation) -> Self {
        match operation {
            TrustedGuestControlOperation::ObserveProjectFilesystem => {
                Self::ObserveProjectFilesystem
            }
            TrustedGuestControlOperation::ObserveImmutableGitPool => Self::ObserveImmutableGitPool,
            TrustedGuestControlOperation::PrepareTrustedTaskView => Self::PrepareTrustedTaskView,
            TrustedGuestControlOperation::ObserveTrustedTaskView => Self::ObserveTrustedTaskView,
            TrustedGuestControlOperation::CleanupTrustedTaskView => Self::CleanupTrustedTaskView,
        }
    }
}

impl From<OperationWire> for TrustedGuestControlOperation {
    fn from(operation: OperationWire) -> Self {
        match operation {
            OperationWire::ObserveProjectFilesystem => Self::ObserveProjectFilesystem,
            OperationWire::ObserveImmutableGitPool => Self::ObserveImmutableGitPool,
            OperationWire::PrepareTrustedTaskView => Self::PrepareTrustedTaskView,
            OperationWire::ObserveTrustedTaskView => Self::ObserveTrustedTaskView,
            OperationWire::CleanupTrustedTaskView => Self::CleanupTrustedTaskView,
        }
    }
}

fn request_from_wire(
    wire: RequestWire,
) -> Result<TrustedGuestControlRequest, TrustedGuestControlProtocolError> {
    if wire.schema_version != TRUSTED_GUEST_CONTROL_PROTOCOL_SCHEMA_VERSION {
        return Err(version_incompatible());
    }
    let binary = TrustedGuestControlBinaryBinding::new(
        wire.binary.generation,
        Sha256Digest::parse(&wire.binary.digest).map_err(|_| invalid_identity())?,
        match wire.binary.architecture {
            ArchitectureWire::LinuxAarch64 => TrustedGuestControlArchitecture::LinuxAarch64,
        },
    )?;
    let authority = TrustedGuestControlAuthority {
        project: ProjectIdentity::parse(&wire.authority.project)
            .map_err(|_| invalid_authority())?,
        project_disk_id: ProjectDiskId::parse(&wire.authority.project_disk_id)
            .map_err(|_| invalid_authority())?,
        project_disk_generation: ProjectDiskGeneration::new(wire.authority.project_disk_generation)
            .map_err(|_| invalid_authority())?,
        project_disk_revision: ProjectDiskRevision::new(wire.authority.project_disk_revision)
            .map_err(|_| invalid_authority())?,
        attachment_generation: ProjectDiskAttachmentGeneration::new(
            wire.authority.attachment_generation,
        )
        .map_err(|_| invalid_authority())?,
        resident_sandbox_id: ResidentSandboxId::parse(&wire.authority.resident_sandbox_id)
            .map_err(|_| invalid_authority())?,
        resident_sandbox_generation: ResidentSandboxGeneration::new(
            wire.authority.resident_sandbox_generation,
        )
        .map_err(|_| invalid_authority())?,
    };
    Ok(TrustedGuestControlRequest::new(
        TrustedGuestControlRequestId::parse(&wire.request_id)?,
        binary,
        authority,
        wire.operation.into(),
        Sha256Digest::parse(&wire.payload_digest).map_err(|_| malformed())?,
    ))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptWire {
    schema_version: u8,
    request_id: String,
    binary: BinaryWire,
    request_digest: String,
    operation: OperationWire,
    outcome: OutcomeWire,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum OutcomeWire {
    Succeeded { result_digest: String },
    Refused { reason: RefusalWire },
    RecoveryRequired { debt: RecoveryDebtWire },
}

impl From<&TrustedGuestControlOutcome> for OutcomeWire {
    fn from(outcome: &TrustedGuestControlOutcome) -> Self {
        match outcome {
            TrustedGuestControlOutcome::Succeeded { result_digest } => Self::Succeeded {
                result_digest: result_digest.as_str().to_owned(),
            },
            TrustedGuestControlOutcome::Refused(reason) => Self::Refused {
                reason: (*reason).into(),
            },
            TrustedGuestControlOutcome::RecoveryRequired(debt) => Self::RecoveryRequired {
                debt: (*debt).into(),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RefusalWire {
    AuthorityChanged,
    CorrelationUnproven,
    UnsupportedOperation,
    InvalidPayload,
}

impl From<TrustedGuestControlRefusal> for RefusalWire {
    fn from(value: TrustedGuestControlRefusal) -> Self {
        match value {
            TrustedGuestControlRefusal::AuthorityChanged => Self::AuthorityChanged,
            TrustedGuestControlRefusal::CorrelationUnproven => Self::CorrelationUnproven,
            TrustedGuestControlRefusal::UnsupportedOperation => Self::UnsupportedOperation,
            TrustedGuestControlRefusal::InvalidPayload => Self::InvalidPayload,
        }
    }
}

impl From<RefusalWire> for TrustedGuestControlRefusal {
    fn from(value: RefusalWire) -> Self {
        match value {
            RefusalWire::AuthorityChanged => Self::AuthorityChanged,
            RefusalWire::CorrelationUnproven => Self::CorrelationUnproven,
            RefusalWire::UnsupportedOperation => Self::UnsupportedOperation,
            RefusalWire::InvalidPayload => Self::InvalidPayload,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RecoveryDebtWire {
    RevalidationRequired,
    WorkdirResetRequired,
    MountCleanupRequired,
}

impl From<TrustedGuestControlRecoveryDebt> for RecoveryDebtWire {
    fn from(value: TrustedGuestControlRecoveryDebt) -> Self {
        match value {
            TrustedGuestControlRecoveryDebt::RevalidationRequired => Self::RevalidationRequired,
            TrustedGuestControlRecoveryDebt::WorkdirResetRequired => Self::WorkdirResetRequired,
            TrustedGuestControlRecoveryDebt::MountCleanupRequired => Self::MountCleanupRequired,
        }
    }
}

impl From<RecoveryDebtWire> for TrustedGuestControlRecoveryDebt {
    fn from(value: RecoveryDebtWire) -> Self {
        match value {
            RecoveryDebtWire::RevalidationRequired => Self::RevalidationRequired,
            RecoveryDebtWire::WorkdirResetRequired => Self::WorkdirResetRequired,
            RecoveryDebtWire::MountCleanupRequired => Self::MountCleanupRequired,
        }
    }
}

fn receipt_from_wire(
    wire: ReceiptWire,
) -> Result<TrustedGuestControlReceipt, TrustedGuestControlProtocolError> {
    if wire.schema_version != TRUSTED_GUEST_CONTROL_PROTOCOL_SCHEMA_VERSION {
        return Err(version_incompatible());
    }
    let binary = TrustedGuestControlBinaryBinding::new(
        wire.binary.generation,
        Sha256Digest::parse(&wire.binary.digest).map_err(|_| malformed())?,
        match wire.binary.architecture {
            ArchitectureWire::LinuxAarch64 => TrustedGuestControlArchitecture::LinuxAarch64,
        },
    )
    .map_err(|_| malformed())?;
    let outcome = match wire.outcome {
        OutcomeWire::Succeeded { result_digest } => TrustedGuestControlOutcome::Succeeded {
            result_digest: Sha256Digest::parse(&result_digest).map_err(|_| malformed())?,
        },
        OutcomeWire::Refused { reason } => TrustedGuestControlOutcome::Refused(reason.into()),
        OutcomeWire::RecoveryRequired { debt } => {
            TrustedGuestControlOutcome::RecoveryRequired(debt.into())
        }
    };
    Ok(TrustedGuestControlReceipt {
        request_id: TrustedGuestControlRequestId::parse(&wire.request_id)
            .map_err(|_| malformed())?,
        binary,
        request_digest: Sha256Digest::parse(&wire.request_digest).map_err(|_| malformed())?,
        operation: wire.operation.into(),
        outcome,
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
        let record = attached_record();
        let authority = TrustedGuestControlAuthority::from_attached_project_disk(&record).unwrap();
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
    fn unknown_noncanonical_and_arbitrary_operation_documents_fail_closed() {
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
    fn receipt_round_trip_requires_exact_request_digest_and_operation() {
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

        let mut other_operation = request;
        other_operation.operation = TrustedGuestControlOperation::CleanupTrustedTaskView;
        assert_eq!(
            decode_trusted_guest_control_receipt(&bytes, &other_operation)
                .unwrap_err()
                .kind(),
            TrustedGuestControlProtocolErrorKind::ReceiptMismatch
        );
    }

    #[test]
    fn refusal_and_recovery_are_closed_typed_receipts() {
        let request = request();
        for outcome in [
            TrustedGuestControlOutcome::Refused(TrustedGuestControlRefusal::AuthorityChanged),
            TrustedGuestControlOutcome::RecoveryRequired(
                TrustedGuestControlRecoveryDebt::WorkdirResetRequired,
            ),
            TrustedGuestControlOutcome::RecoveryRequired(
                TrustedGuestControlRecoveryDebt::MountCleanupRequired,
            ),
        ] {
            let bytes = encode_trusted_guest_control_receipt(&request, &outcome).unwrap();
            assert_eq!(
                decode_trusted_guest_control_receipt(&bytes, &request)
                    .unwrap()
                    .outcome(),
                &outcome
            );
        }
    }
}
