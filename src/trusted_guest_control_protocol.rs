//! Canonical common envelope for one-shot Linux guest-control transactions.
//!
//! This module is pure protocol vocabulary. It performs no process execution, filesystem I/O,
//! privilege escalation, mount operation, Git operation, durable-state mutation, or guest
//! observation.
//!
//! Requests carry one purpose-typed target/authority claim, one closed operation tag, and an opaque
//! digest of a later operation-specific payload. Decoded fields are claims, never ownership
//! capabilities. The Mac invocation adapter must freshly re-confirm the owning resident or
//! formatter state and installed guest binary immediately before spawn, and the Linux handler must
//! prove operation-specific authority before doing any work.
//!
//! Protocol v3 is the Glaeda generation used for every fresh request. The explicit legacy v2
//! decoder returns a separate inspection-only type so old SmolRunner traffic can be interpreted
//! without becoming eligible for current encoding or execution.

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::project_catalog::ProjectIdentity;
use crate::project_disk_lease::{
    ProjectDiskAttachmentGeneration, ProjectDiskGeneration, ProjectDiskId, ProjectDiskLeaseRecord,
    ProjectDiskLeaseState, ProjectDiskRevision, ResidentSandboxGeneration, ResidentSandboxId,
};

pub const TRUSTED_GUEST_CONTROL_PROTOCOL_SCHEMA_VERSION: u8 = 3;
pub const LEGACY_SMOLRUNNER_TRUSTED_GUEST_CONTROL_PROTOCOL_SCHEMA_VERSION: u8 = 2;
pub const MAX_TRUSTED_GUEST_CONTROL_REQUEST_BYTES: usize = 4 * 1024;
pub const MAX_TRUSTED_GUEST_CONTROL_RECEIPT_BYTES: usize = 2 * 1024;

const REQUEST_DIGEST_DOMAIN: &[u8] = b"glaeda-trusted-guest-control-request-v3\0";
const AUTHORITY_DIGEST_DOMAIN: &[u8] = b"glaeda-trusted-guest-control-authority-v3\0";
const LEGACY_SMOLRUNNER_REQUEST_DIGEST_DOMAIN: &[u8] =
    b"smolrunner-trusted-guest-control-request-v2\0";
const LEGACY_SMOLRUNNER_AUTHORITY_DIGEST_DOMAIN: &[u8] =
    b"smolrunner-trusted-guest-control-authority-v2\0";
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

macro_rules! positive_generation {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
        #[serde(transparent)]
        pub struct $name(u64);

        impl $name {
            /// Construct one positive protocol-local generation claim.
            ///
            /// # Errors
            ///
            /// Returns a bounded error when `value` is zero.
            pub fn new(value: u64) -> Result<Self, TrustedGuestControlProtocolError> {
                if value == 0 {
                    return Err(invalid_identity());
                }
                Ok(Self(value))
            }

            #[must_use]
            pub const fn get(self) -> u64 {
                self.0
            }
        }
    };
}

positive_generation!(TrustedGuestControlResidentConfigGeneration);
positive_generation!(TrustedGuestControlResidentAuthorityGeneration);
positive_generation!(TrustedGuestControlFormatTransactionGeneration);
positive_generation!(TrustedGuestControlFormatterCarrierGeneration);
positive_generation!(TrustedGuestControlFormatterConfigGeneration);
positive_generation!(TrustedGuestControlAttachTransactionGeneration);

macro_rules! digest_claim {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Serialize)]
        #[serde(transparent)]
        pub struct $name(Sha256Digest);

        impl $name {
            /// Construct one typed serialized claim from a canonical SHA-256 digest.
            ///
            /// This never constructs or recovers the owner-side capability whose identity the
            /// digest claims.
            #[must_use]
            pub const fn new(digest: Sha256Digest) -> Self {
                Self(digest)
            }

            #[must_use]
            pub const fn digest(&self) -> &Sha256Digest {
                &self.0
            }
        }
    };
}

digest_claim!(TrustedGuestControlResidentConfigClaim);
digest_claim!(TrustedGuestControlFormatAuthorityClaim);
digest_claim!(TrustedGuestControlCreatedProvenanceClaim);
digest_claim!(TrustedGuestControlFormatterConfigClaim);
digest_claim!(TrustedGuestControlAttachAuthorityClaim);

/// Closed logical identity of the guest target selected by a sealed invocation capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrustedGuestControlTargetIdentity {
    Resident {
        project: ProjectIdentity,
        sandbox_id: ResidentSandboxId,
        sandbox_generation: ResidentSandboxGeneration,
    },
    Formatter {
        project: ProjectIdentity,
        project_disk_id: ProjectDiskId,
        project_disk_generation: ProjectDiskGeneration,
        format_transaction_generation: TrustedGuestControlFormatTransactionGeneration,
        formatter_carrier_generation: TrustedGuestControlFormatterCarrierGeneration,
    },
}

impl TrustedGuestControlTargetIdentity {
    #[must_use]
    pub fn resident(
        project: ProjectIdentity,
        sandbox_id: ResidentSandboxId,
        sandbox_generation: ResidentSandboxGeneration,
    ) -> Self {
        Self::Resident {
            project,
            sandbox_id,
            sandbox_generation,
        }
    }

    #[must_use]
    pub fn formatter(
        project: ProjectIdentity,
        project_disk_id: ProjectDiskId,
        project_disk_generation: ProjectDiskGeneration,
        format_transaction_generation: TrustedGuestControlFormatTransactionGeneration,
        formatter_carrier_generation: TrustedGuestControlFormatterCarrierGeneration,
    ) -> Self {
        Self::Formatter {
            project,
            project_disk_id,
            project_disk_generation,
            format_transaction_generation,
            formatter_carrier_generation,
        }
    }

    #[must_use]
    pub const fn project(&self) -> &ProjectIdentity {
        match self {
            Self::Resident { project, .. } | Self::Formatter { project, .. } => project,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustedGuestControlAuthorityKind {
    ResidentSandbox,
    ResidentPendingProjectDiskAttachment,
    ResidentAttachedProjectDisk,
    FormatterProjectDisk,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedGuestControlResidentAttachedProjectDiskAuthority {
    project_disk_id: ProjectDiskId,
    project_disk_generation: ProjectDiskGeneration,
    project_disk_revision: ProjectDiskRevision,
    attachment_generation: ProjectDiskAttachmentGeneration,
}

impl TrustedGuestControlResidentAttachedProjectDiskAuthority {
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
}

/// Purpose-typed authority claim carried by one trusted guest-control request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedGuestControlAuthority {
    target: TrustedGuestControlTargetIdentity,
    variant: TrustedGuestControlAuthorityVariant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TrustedGuestControlAuthorityVariant {
    ResidentSandbox {
        config_generation: TrustedGuestControlResidentConfigGeneration,
        config_identity: TrustedGuestControlResidentConfigClaim,
        authority_generation: TrustedGuestControlResidentAuthorityGeneration,
    },
    ResidentPendingProjectDiskAttachment {
        project_disk_id: ProjectDiskId,
        project_disk_generation: ProjectDiskGeneration,
        starting_project_disk_revision: ProjectDiskRevision,
        reserved_attachment_generation: ProjectDiskAttachmentGeneration,
        attach_transaction_generation: TrustedGuestControlAttachTransactionGeneration,
        attach_authority_identity: TrustedGuestControlAttachAuthorityClaim,
    },
    ResidentAttachedProjectDisk(TrustedGuestControlResidentAttachedProjectDiskAuthority),
    FormatterProjectDisk {
        created_provenance_identity: TrustedGuestControlCreatedProvenanceClaim,
        format_authority_identity: TrustedGuestControlFormatAuthorityClaim,
        formatter_config_generation: TrustedGuestControlFormatterConfigGeneration,
        formatter_config_identity: TrustedGuestControlFormatterConfigClaim,
    },
}

impl TrustedGuestControlAuthority {
    #[must_use]
    pub fn resident_sandbox(
        project: ProjectIdentity,
        sandbox_id: ResidentSandboxId,
        sandbox_generation: ResidentSandboxGeneration,
        config_generation: TrustedGuestControlResidentConfigGeneration,
        config_identity: TrustedGuestControlResidentConfigClaim,
        authority_generation: TrustedGuestControlResidentAuthorityGeneration,
    ) -> Self {
        Self {
            target: TrustedGuestControlTargetIdentity::resident(
                project,
                sandbox_id,
                sandbox_generation,
            ),
            variant: TrustedGuestControlAuthorityVariant::ResidentSandbox {
                config_generation,
                config_identity,
                authority_generation,
            },
        }
    }

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
            target: TrustedGuestControlTargetIdentity::resident(
                record.project().clone(),
                attachment.sandbox_id().clone(),
                attachment.sandbox_generation(),
            ),
            variant: TrustedGuestControlAuthorityVariant::ResidentAttachedProjectDisk(
                TrustedGuestControlResidentAttachedProjectDiskAuthority {
                    project_disk_id: record.disk_id().clone(),
                    project_disk_generation: record.disk_generation(),
                    project_disk_revision: record.revision(),
                    attachment_generation: attachment.generation(),
                },
            ),
        })
    }

    #[must_use]
    pub const fn project(&self) -> &ProjectIdentity {
        self.target.project()
    }

    #[must_use]
    pub const fn kind(&self) -> TrustedGuestControlAuthorityKind {
        match &self.variant {
            TrustedGuestControlAuthorityVariant::ResidentSandbox { .. } => {
                TrustedGuestControlAuthorityKind::ResidentSandbox
            }
            TrustedGuestControlAuthorityVariant::ResidentPendingProjectDiskAttachment { .. } => {
                TrustedGuestControlAuthorityKind::ResidentPendingProjectDiskAttachment
            }
            TrustedGuestControlAuthorityVariant::ResidentAttachedProjectDisk(_) => {
                TrustedGuestControlAuthorityKind::ResidentAttachedProjectDisk
            }
            TrustedGuestControlAuthorityVariant::FormatterProjectDisk { .. } => {
                TrustedGuestControlAuthorityKind::FormatterProjectDisk
            }
        }
    }

    #[must_use]
    pub const fn target_identity(&self) -> &TrustedGuestControlTargetIdentity {
        &self.target
    }

    #[must_use]
    pub const fn resident_attached_project_disk(
        &self,
    ) -> Option<&TrustedGuestControlResidentAttachedProjectDiskAuthority> {
        match &self.variant {
            TrustedGuestControlAuthorityVariant::ResidentAttachedProjectDisk(authority) => {
                Some(authority)
            }
            _ => None,
        }
    }

    #[must_use]
    pub const fn resident_sandbox_id(&self) -> Option<&ResidentSandboxId> {
        match &self.target {
            TrustedGuestControlTargetIdentity::Resident { sandbox_id, .. } => Some(sandbox_id),
            TrustedGuestControlTargetIdentity::Formatter { .. } => None,
        }
    }

    #[must_use]
    pub const fn resident_sandbox_generation(&self) -> Option<ResidentSandboxGeneration> {
        match &self.target {
            TrustedGuestControlTargetIdentity::Resident {
                sandbox_generation, ..
            } => Some(*sandbox_generation),
            TrustedGuestControlTargetIdentity::Formatter { .. } => None,
        }
    }

    pub fn formatter_project_disk(
        target: TrustedGuestControlTargetIdentity,
        created_provenance_identity: TrustedGuestControlCreatedProvenanceClaim,
        format_authority_identity: TrustedGuestControlFormatAuthorityClaim,
        formatter_config_generation: TrustedGuestControlFormatterConfigGeneration,
        formatter_config_identity: TrustedGuestControlFormatterConfigClaim,
    ) -> Result<Self, TrustedGuestControlProtocolError> {
        if !matches!(&target, TrustedGuestControlTargetIdentity::Formatter { .. }) {
            return Err(invalid_authority());
        }
        Ok(Self {
            target,
            variant: TrustedGuestControlAuthorityVariant::FormatterProjectDisk {
                created_provenance_identity,
                format_authority_identity,
                formatter_config_generation,
                formatter_config_identity,
            },
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn resident_pending_project_disk_attachment(
        target: TrustedGuestControlTargetIdentity,
        project_disk_id: ProjectDiskId,
        project_disk_generation: ProjectDiskGeneration,
        starting_project_disk_revision: ProjectDiskRevision,
        reserved_attachment_generation: ProjectDiskAttachmentGeneration,
        attach_transaction_generation: TrustedGuestControlAttachTransactionGeneration,
        attach_authority_identity: TrustedGuestControlAttachAuthorityClaim,
    ) -> Result<Self, TrustedGuestControlProtocolError> {
        if !matches!(target, TrustedGuestControlTargetIdentity::Resident { .. }) {
            return Err(invalid_authority());
        }
        Ok(Self {
            target,
            variant: TrustedGuestControlAuthorityVariant::ResidentPendingProjectDiskAttachment {
                project_disk_id,
                project_disk_generation,
                starting_project_disk_revision,
                reserved_attachment_generation,
                attach_transaction_generation,
                attach_authority_identity,
            },
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustedGuestControlOperation {
    ProbeGuestControl,
    ObservePendingProjectDiskAttachment,
    ObserveProjectFilesystem,
    MountProjectFilesystem,
    ObserveProjectBlockDeviceForFormat,
    FormatProjectFilesystem,
    ObserveFormattedProjectFilesystem,
    ObserveImmutableGitPool,
    PublishImmutableGitPoolGeneration,
    PrepareTrustedTaskView,
    ObserveTrustedTaskView,
    CleanupTrustedTaskView,
}

impl TrustedGuestControlOperation {
    #[must_use]
    pub const fn authority_kind(self) -> TrustedGuestControlAuthorityKind {
        match self {
            Self::ProbeGuestControl => TrustedGuestControlAuthorityKind::ResidentSandbox,
            Self::ObservePendingProjectDiskAttachment => {
                TrustedGuestControlAuthorityKind::ResidentPendingProjectDiskAttachment
            }
            Self::ObserveProjectBlockDeviceForFormat
            | Self::FormatProjectFilesystem
            | Self::ObserveFormattedProjectFilesystem => {
                TrustedGuestControlAuthorityKind::FormatterProjectDisk
            }
            Self::ObserveProjectFilesystem
            | Self::MountProjectFilesystem
            | Self::ObserveImmutableGitPool
            | Self::PublishImmutableGitPoolGeneration
            | Self::PrepareTrustedTaskView
            | Self::ObserveTrustedTaskView
            | Self::CleanupTrustedTaskView => {
                TrustedGuestControlAuthorityKind::ResidentAttachedProjectDisk
            }
        }
    }

    #[must_use]
    pub const fn accepts_authority_kind(self, kind: TrustedGuestControlAuthorityKind) -> bool {
        matches!(
            (self.authority_kind(), kind),
            (
                TrustedGuestControlAuthorityKind::ResidentSandbox,
                TrustedGuestControlAuthorityKind::ResidentSandbox
            ) | (
                TrustedGuestControlAuthorityKind::ResidentPendingProjectDiskAttachment,
                TrustedGuestControlAuthorityKind::ResidentPendingProjectDiskAttachment
            ) | (
                TrustedGuestControlAuthorityKind::ResidentAttachedProjectDisk,
                TrustedGuestControlAuthorityKind::ResidentAttachedProjectDisk
            ) | (
                TrustedGuestControlAuthorityKind::FormatterProjectDisk,
                TrustedGuestControlAuthorityKind::FormatterProjectDisk
            )
        )
    }
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
    /// Construct one current-generation request only when the closed operation accepts the supplied
    /// authority kind.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal for an operation/authority mismatch.
    pub fn new(
        request_id: TrustedGuestControlRequestId,
        binary: TrustedGuestControlBinaryBinding,
        authority: TrustedGuestControlAuthority,
        operation: TrustedGuestControlOperation,
        payload_digest: Sha256Digest,
    ) -> Result<Self, TrustedGuestControlProtocolError> {
        if !operation.accepts_authority_kind(authority.kind()) {
            return Err(invalid_authority());
        }
        Ok(Self {
            request_id,
            binary,
            authority,
            operation,
            payload_digest,
        })
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

/// Canonically decoded SmolRunner protocol-v2 request retained only for old-generation inspection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacySmolRunnerTrustedGuestControlRequestV2 {
    request: TrustedGuestControlRequest,
    canonical: Vec<u8>,
}

impl LegacySmolRunnerTrustedGuestControlRequestV2 {
    #[must_use]
    pub const fn request(&self) -> &TrustedGuestControlRequest {
        &self.request
    }

    #[must_use]
    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical
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

/// Canonically decoded SmolRunner protocol-v2 receipt retained only for old-generation inspection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacySmolRunnerTrustedGuestControlReceiptV2 {
    receipt: TrustedGuestControlReceipt,
}

impl LegacySmolRunnerTrustedGuestControlReceiptV2 {
    #[must_use]
    pub const fn receipt(&self) -> &TrustedGuestControlReceipt {
        &self.receipt
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

/// Encode one strict compact current-generation request followed by a single newline.
pub fn encode_trusted_guest_control_request(
    request: &TrustedGuestControlRequest,
) -> Result<Vec<u8>, TrustedGuestControlProtocolError> {
    let mut bytes = encode_trusted_guest_control_request_body(request)?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Encode the canonical current-generation request JSON body without a transport newline.
pub fn encode_trusted_guest_control_request_body(
    request: &TrustedGuestControlRequest,
) -> Result<Vec<u8>, TrustedGuestControlProtocolError> {
    if !request
        .operation
        .accepts_authority_kind(request.authority.kind())
    {
        return Err(invalid_authority());
    }
    canonical_json_body(
        &RequestWire::from(request),
        MAX_TRUSTED_GUEST_CONTROL_REQUEST_BYTES - 1,
    )
}

/// Decode one bounded canonical current-generation request without granting execution authority.
pub fn decode_trusted_guest_control_request(
    bytes: &[u8],
) -> Result<TrustedGuestControlRequest, TrustedGuestControlProtocolError> {
    require_size(bytes, MAX_TRUSTED_GUEST_CONTROL_REQUEST_BYTES)?;
    let Some(body) = bytes.strip_suffix(b"\n") else {
        return Err(noncanonical());
    };
    decode_trusted_guest_control_request_body(body)
}

/// Decode one canonical current-generation request JSON body without granting execution authority.
pub fn decode_trusted_guest_control_request_body(
    bytes: &[u8],
) -> Result<TrustedGuestControlRequest, TrustedGuestControlProtocolError> {
    require_size(bytes, MAX_TRUSTED_GUEST_CONTROL_REQUEST_BYTES - 1)?;
    require_version(bytes, TRUSTED_GUEST_CONTROL_PROTOCOL_SCHEMA_VERSION)?;
    let wire: RequestWire = serde_json::from_slice(bytes).map_err(|_| malformed())?;
    let request = request_from_wire(wire)?;
    if encode_trusted_guest_control_request_body(&request)? != bytes {
        return Err(noncanonical());
    }
    Ok(request)
}

/// Decode one exact canonical SmolRunner protocol-v2 request into an inspection-only value.
pub fn decode_legacy_smolrunner_trusted_guest_control_request_v2(
    bytes: &[u8],
) -> Result<LegacySmolRunnerTrustedGuestControlRequestV2, TrustedGuestControlProtocolError> {
    require_size(bytes, MAX_TRUSTED_GUEST_CONTROL_REQUEST_BYTES)?;
    let Some(body) = bytes.strip_suffix(b"\n") else {
        return Err(noncanonical());
    };
    require_version(
        body,
        LEGACY_SMOLRUNNER_TRUSTED_GUEST_CONTROL_PROTOCOL_SCHEMA_VERSION,
    )?;
    let wire: RequestWire = serde_json::from_slice(body).map_err(|_| malformed())?;
    if canonical_json_body(&wire, MAX_TRUSTED_GUEST_CONTROL_REQUEST_BYTES - 1)? != body {
        return Err(noncanonical());
    }
    let request = request_from_wire_for_schema(
        wire,
        LEGACY_SMOLRUNNER_TRUSTED_GUEST_CONTROL_PROTOCOL_SCHEMA_VERSION,
    )?;
    Ok(LegacySmolRunnerTrustedGuestControlRequestV2 {
        request,
        canonical: bytes.to_vec(),
    })
}

/// Derive the Glaeda-v3 request digest every current terminal receipt must bind.
pub fn trusted_guest_control_request_digest(
    request: &TrustedGuestControlRequest,
) -> Result<Sha256Digest, TrustedGuestControlProtocolError> {
    digest_with_domain(
        REQUEST_DIGEST_DOMAIN,
        &encode_trusted_guest_control_request(request)?,
    )
}

/// Recompute the exact historical SmolRunner-v2 digest of an explicitly decoded legacy request.
pub fn legacy_smolrunner_trusted_guest_control_request_v2_digest(
    request: &LegacySmolRunnerTrustedGuestControlRequestV2,
) -> Result<Sha256Digest, TrustedGuestControlProtocolError> {
    digest_with_domain(LEGACY_SMOLRUNNER_REQUEST_DIGEST_DOMAIN, &request.canonical)
}

/// Derive the exact Glaeda-v3 identity of one complete serialized authority claim.
pub fn trusted_guest_control_authority_digest(
    authority: &TrustedGuestControlAuthority,
) -> Result<Sha256Digest, TrustedGuestControlProtocolError> {
    authority_digest_with_domain(authority, AUTHORITY_DIGEST_DOMAIN)
}

/// Recompute the historical SmolRunner-v2 authority identity for old-generation inspection.
pub fn legacy_smolrunner_trusted_guest_control_authority_v2_digest(
    authority: &TrustedGuestControlAuthority,
) -> Result<Sha256Digest, TrustedGuestControlProtocolError> {
    authority_digest_with_domain(authority, LEGACY_SMOLRUNNER_AUTHORITY_DIGEST_DOMAIN)
}

fn authority_digest_with_domain(
    authority: &TrustedGuestControlAuthority,
    domain: &[u8],
) -> Result<Sha256Digest, TrustedGuestControlProtocolError> {
    let bytes = serde_json::to_vec(&AuthorityWire::from(authority)).map_err(|_| malformed())?;
    digest_with_domain(domain, &bytes)
}

fn digest_with_domain(
    domain: &[u8],
    bytes: &[u8],
) -> Result<Sha256Digest, TrustedGuestControlProtocolError> {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(bytes);
    raw_digest(&hasher.finalize())
}

/// Encode one current-generation terminal outcome for an exact request.
pub fn encode_trusted_guest_control_receipt(
    request: &TrustedGuestControlRequest,
    outcome: &TrustedGuestControlOutcome,
) -> Result<Vec<u8>, TrustedGuestControlProtocolError> {
    let mut bytes = encode_trusted_guest_control_receipt_body(request, outcome)?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Encode the canonical current common receipt JSON body without a transport newline.
pub fn encode_trusted_guest_control_receipt_body(
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
    canonical_json_body(&wire, MAX_TRUSTED_GUEST_CONTROL_RECEIPT_BYTES - 1)
}

/// Decode a current terminal receipt only against the exact request that caused the transaction.
pub fn decode_trusted_guest_control_receipt(
    bytes: &[u8],
    expected_request: &TrustedGuestControlRequest,
) -> Result<TrustedGuestControlReceipt, TrustedGuestControlProtocolError> {
    require_size(bytes, MAX_TRUSTED_GUEST_CONTROL_RECEIPT_BYTES)?;
    let Some(body) = bytes.strip_suffix(b"\n") else {
        return Err(noncanonical());
    };
    decode_trusted_guest_control_receipt_body(body, expected_request)
}

/// Decode a canonical current common receipt JSON body against the exact originating request.
pub fn decode_trusted_guest_control_receipt_body(
    bytes: &[u8],
    expected_request: &TrustedGuestControlRequest,
) -> Result<TrustedGuestControlReceipt, TrustedGuestControlProtocolError> {
    require_size(bytes, MAX_TRUSTED_GUEST_CONTROL_RECEIPT_BYTES - 1)?;
    require_version(bytes, TRUSTED_GUEST_CONTROL_PROTOCOL_SCHEMA_VERSION)?;
    let wire: ReceiptWire = serde_json::from_slice(bytes).map_err(|_| malformed())?;
    let receipt = receipt_from_wire_for_schema(wire, TRUSTED_GUEST_CONTROL_PROTOCOL_SCHEMA_VERSION)?;
    if receipt.request_id != expected_request.request_id
        || receipt.binary != expected_request.binary
        || receipt.operation != expected_request.operation
        || receipt.request_digest != trusted_guest_control_request_digest(expected_request)?
    {
        return Err(receipt_mismatch());
    }
    if encode_trusted_guest_control_receipt_body(expected_request, &receipt.outcome)? != bytes {
        return Err(noncanonical());
    }
    Ok(receipt)
}

/// Decode one exact SmolRunner protocol-v2 receipt against an explicitly decoded legacy request.
pub fn decode_legacy_smolrunner_trusted_guest_control_receipt_v2(
    bytes: &[u8],
    expected_request: &LegacySmolRunnerTrustedGuestControlRequestV2,
) -> Result<LegacySmolRunnerTrustedGuestControlReceiptV2, TrustedGuestControlProtocolError> {
    require_size(bytes, MAX_TRUSTED_GUEST_CONTROL_RECEIPT_BYTES)?;
    let Some(body) = bytes.strip_suffix(b"\n") else {
        return Err(noncanonical());
    };
    require_version(
        body,
        LEGACY_SMOLRUNNER_TRUSTED_GUEST_CONTROL_PROTOCOL_SCHEMA_VERSION,
    )?;
    let wire: ReceiptWire = serde_json::from_slice(body).map_err(|_| malformed())?;
    if canonical_json_body(&wire, MAX_TRUSTED_GUEST_CONTROL_RECEIPT_BYTES - 1)? != body {
        return Err(noncanonical());
    }
    let receipt = receipt_from_wire_for_schema(
        wire,
        LEGACY_SMOLRUNNER_TRUSTED_GUEST_CONTROL_PROTOCOL_SCHEMA_VERSION,
    )?;
    let expected = expected_request.request();
    if receipt.request_id != expected.request_id
        || receipt.binary != expected.binary
        || receipt.operation != expected.operation
        || receipt.request_digest
            != legacy_smolrunner_trusted_guest_control_request_v2_digest(expected_request)?
    {
        return Err(receipt_mismatch());
    }
    Ok(LegacySmolRunnerTrustedGuestControlReceiptV2 { receipt })
}

fn require_size(bytes: &[u8], limit: usize) -> Result<(), TrustedGuestControlProtocolError> {
    if bytes.len() > limit {
        return Err(too_large());
    }
    Ok(())
}

fn require_version(
    bytes: &[u8],
    expected: u8,
) -> Result<(), TrustedGuestControlProtocolError> {
    let version: VersionWire = serde_json::from_slice(bytes).map_err(|_| malformed())?;
    if version.schema_version != expected {
        return Err(version_incompatible());
    }
    Ok(())
}

fn canonical_json_body(
    value: &impl Serialize,
    limit: usize,
) -> Result<Vec<u8>, TrustedGuestControlProtocolError> {
    let bytes = serde_json::to_vec(value).map_err(|_| malformed())?;
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
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum AuthorityWire {
    ResidentSandbox {
        project: String,
        resident_sandbox_id: String,
        resident_sandbox_generation: u64,
        resident_config_generation: u64,
        resident_config_identity: String,
        resident_authority_generation: u64,
    },
    ResidentPendingProjectDiskAttachment {
        project: String,
        resident_sandbox_id: String,
        resident_sandbox_generation: u64,
        project_disk_id: String,
        project_disk_generation: u64,
        starting_project_disk_revision: u64,
        reserved_attachment_generation: u64,
        attach_transaction_generation: u64,
        attach_authority_identity: String,
    },
    ResidentAttachedProjectDisk {
        project: String,
        project_disk_id: String,
        project_disk_generation: u64,
        project_disk_revision: u64,
        attachment_generation: u64,
        resident_sandbox_id: String,
        resident_sandbox_generation: u64,
    },
    FormatterProjectDisk {
        project: String,
        project_disk_id: String,
        project_disk_generation: u64,
        format_transaction_generation: u64,
        formatter_carrier_generation: u64,
        created_provenance_identity: String,
        format_authority_identity: String,
        formatter_config_generation: u64,
        formatter_config_identity: String,
    },
}

impl From<&TrustedGuestControlAuthority> for AuthorityWire {
    fn from(value: &TrustedGuestControlAuthority) -> Self {
        match (&value.target, &value.variant) {
            (
                TrustedGuestControlTargetIdentity::Resident {
                    project,
                    sandbox_id,
                    sandbox_generation,
                },
                TrustedGuestControlAuthorityVariant::ResidentSandbox {
                    config_generation,
                    config_identity,
                    authority_generation,
                },
            ) => Self::ResidentSandbox {
                project: project.as_str().to_owned(),
                resident_sandbox_id: sandbox_id.as_str().to_owned(),
                resident_sandbox_generation: sandbox_generation.get(),
                resident_config_generation: config_generation.get(),
                resident_config_identity: config_identity.digest().as_str().to_owned(),
                resident_authority_generation: authority_generation.get(),
            },
            (
                TrustedGuestControlTargetIdentity::Resident {
                    project,
                    sandbox_id,
                    sandbox_generation,
                },
                TrustedGuestControlAuthorityVariant::ResidentPendingProjectDiskAttachment {
                    project_disk_id,
                    project_disk_generation,
                    starting_project_disk_revision,
                    reserved_attachment_generation,
                    attach_transaction_generation,
                    attach_authority_identity,
                },
            ) => Self::ResidentPendingProjectDiskAttachment {
                project: project.as_str().to_owned(),
                resident_sandbox_id: sandbox_id.as_str().to_owned(),
                resident_sandbox_generation: sandbox_generation.get(),
                project_disk_id: project_disk_id.as_str().to_owned(),
                project_disk_generation: project_disk_generation.get(),
                starting_project_disk_revision: starting_project_disk_revision.get(),
                reserved_attachment_generation: reserved_attachment_generation.get(),
                attach_transaction_generation: attach_transaction_generation.get(),
                attach_authority_identity: attach_authority_identity.digest().as_str().to_owned(),
            },
            (
                TrustedGuestControlTargetIdentity::Resident {
                    project,
                    sandbox_id,
                    sandbox_generation,
                },
                TrustedGuestControlAuthorityVariant::ResidentAttachedProjectDisk(authority),
            ) => Self::ResidentAttachedProjectDisk {
                project: project.as_str().to_owned(),
                project_disk_id: authority.project_disk_id.as_str().to_owned(),
                project_disk_generation: authority.project_disk_generation.get(),
                project_disk_revision: authority.project_disk_revision.get(),
                attachment_generation: authority.attachment_generation.get(),
                resident_sandbox_id: sandbox_id.as_str().to_owned(),
                resident_sandbox_generation: sandbox_generation.get(),
            },
            (
                TrustedGuestControlTargetIdentity::Formatter {
                    project,
                    project_disk_id,
                    project_disk_generation,
                    format_transaction_generation,
                    formatter_carrier_generation,
                },
                TrustedGuestControlAuthorityVariant::FormatterProjectDisk {
                    created_provenance_identity,
                    format_authority_identity,
                    formatter_config_generation,
                    formatter_config_identity,
                    ..
                },
            ) => Self::FormatterProjectDisk {
                project: project.as_str().to_owned(),
                project_disk_id: project_disk_id.as_str().to_owned(),
                project_disk_generation: project_disk_generation.get(),
                format_transaction_generation: format_transaction_generation.get(),
                formatter_carrier_generation: formatter_carrier_generation.get(),
                created_provenance_identity: created_provenance_identity
                    .digest()
                    .as_str()
                    .to_owned(),
                format_authority_identity: format_authority_identity.digest().as_str().to_owned(),
                formatter_config_generation: formatter_config_generation.get(),
                formatter_config_identity: formatter_config_identity.digest().as_str().to_owned(),
            },
            _ => unreachable!("private authority constructor preserves target/variant kind"),
        }
    }
}

fn authority_from_wire(
    wire: AuthorityWire,
) -> Result<TrustedGuestControlAuthority, TrustedGuestControlProtocolError> {
    match wire {
        AuthorityWire::ResidentSandbox {
            project,
            resident_sandbox_id,
            resident_sandbox_generation,
            resident_config_generation,
            resident_config_identity,
            resident_authority_generation,
        } => Ok(TrustedGuestControlAuthority::resident_sandbox(
            parse_project(&project)?,
            parse_sandbox_id(&resident_sandbox_id)?,
            parse_sandbox_generation(resident_sandbox_generation)?,
            TrustedGuestControlResidentConfigGeneration::new(resident_config_generation)?,
            TrustedGuestControlResidentConfigClaim::new(parse_claim(&resident_config_identity)?),
            TrustedGuestControlResidentAuthorityGeneration::new(resident_authority_generation)?,
        )),
        AuthorityWire::ResidentPendingProjectDiskAttachment {
            project,
            resident_sandbox_id,
            resident_sandbox_generation,
            project_disk_id,
            project_disk_generation,
            starting_project_disk_revision,
            reserved_attachment_generation,
            attach_transaction_generation,
            attach_authority_identity,
        } => TrustedGuestControlAuthority::resident_pending_project_disk_attachment(
            TrustedGuestControlTargetIdentity::resident(
                parse_project(&project)?,
                parse_sandbox_id(&resident_sandbox_id)?,
                parse_sandbox_generation(resident_sandbox_generation)?,
            ),
            parse_disk_id(&project_disk_id)?,
            parse_disk_generation(project_disk_generation)?,
            parse_disk_revision(starting_project_disk_revision)?,
            parse_attachment_generation(reserved_attachment_generation)?,
            TrustedGuestControlAttachTransactionGeneration::new(attach_transaction_generation)?,
            TrustedGuestControlAttachAuthorityClaim::new(parse_claim(&attach_authority_identity)?),
        ),
        AuthorityWire::ResidentAttachedProjectDisk {
            project,
            project_disk_id,
            project_disk_generation,
            project_disk_revision,
            attachment_generation,
            resident_sandbox_id,
            resident_sandbox_generation,
        } => Ok(TrustedGuestControlAuthority {
            target: TrustedGuestControlTargetIdentity::resident(
                parse_project(&project)?,
                parse_sandbox_id(&resident_sandbox_id)?,
                parse_sandbox_generation(resident_sandbox_generation)?,
            ),
            variant: TrustedGuestControlAuthorityVariant::ResidentAttachedProjectDisk(
                TrustedGuestControlResidentAttachedProjectDiskAuthority {
                    project_disk_id: parse_disk_id(&project_disk_id)?,
                    project_disk_generation: parse_disk_generation(project_disk_generation)?,
                    project_disk_revision: parse_disk_revision(project_disk_revision)?,
                    attachment_generation: parse_attachment_generation(attachment_generation)?,
                },
            ),
        }),
        AuthorityWire::FormatterProjectDisk {
            project,
            project_disk_id,
            project_disk_generation,
            format_transaction_generation,
            formatter_carrier_generation,
            created_provenance_identity,
            format_authority_identity,
            formatter_config_generation,
            formatter_config_identity,
        } => TrustedGuestControlAuthority::formatter_project_disk(
            TrustedGuestControlTargetIdentity::formatter(
                parse_project(&project)?,
                parse_disk_id(&project_disk_id)?,
                parse_disk_generation(project_disk_generation)?,
                TrustedGuestControlFormatTransactionGeneration::new(format_transaction_generation)?,
                TrustedGuestControlFormatterCarrierGeneration::new(formatter_carrier_generation)?,
            ),
            TrustedGuestControlCreatedProvenanceClaim::new(parse_claim(
                &created_provenance_identity,
            )?),
            TrustedGuestControlFormatAuthorityClaim::new(parse_claim(&format_authority_identity)?),
            TrustedGuestControlFormatterConfigGeneration::new(formatter_config_generation)?,
            TrustedGuestControlFormatterConfigClaim::new(parse_claim(&formatter_config_identity)?),
        ),
    }
}

fn parse_project(value: &str) -> Result<ProjectIdentity, TrustedGuestControlProtocolError> {
    ProjectIdentity::parse(value).map_err(|_| invalid_authority())
}

fn parse_disk_id(value: &str) -> Result<ProjectDiskId, TrustedGuestControlProtocolError> {
    ProjectDiskId::parse(value).map_err(|_| invalid_authority())
}

fn parse_disk_generation(
    value: u64,
) -> Result<ProjectDiskGeneration, TrustedGuestControlProtocolError> {
    ProjectDiskGeneration::new(value).map_err(|_| invalid_authority())
}

fn parse_disk_revision(
    value: u64,
) -> Result<ProjectDiskRevision, TrustedGuestControlProtocolError> {
    ProjectDiskRevision::new(value).map_err(|_| invalid_authority())
}

fn parse_attachment_generation(
    value: u64,
) -> Result<ProjectDiskAttachmentGeneration, TrustedGuestControlProtocolError> {
    ProjectDiskAttachmentGeneration::new(value).map_err(|_| invalid_authority())
}

fn parse_sandbox_id(value: &str) -> Result<ResidentSandboxId, TrustedGuestControlProtocolError> {
    ResidentSandboxId::parse(value).map_err(|_| invalid_authority())
}

fn parse_sandbox_generation(
    value: u64,
) -> Result<ResidentSandboxGeneration, TrustedGuestControlProtocolError> {
    ResidentSandboxGeneration::new(value).map_err(|_| invalid_authority())
}

fn parse_claim(value: &str) -> Result<Sha256Digest, TrustedGuestControlProtocolError> {
    Sha256Digest::parse(value).map_err(|_| invalid_authority())
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
    request_from_wire_for_schema(wire, TRUSTED_GUEST_CONTROL_PROTOCOL_SCHEMA_VERSION)
}

fn request_from_wire_for_schema(
    wire: RequestWire,
    expected_schema_version: u8,
) -> Result<TrustedGuestControlRequest, TrustedGuestControlProtocolError> {
    if wire.schema_version != expected_schema_version {
        return Err(version_incompatible());
    }
    TrustedGuestControlRequest::new(
        TrustedGuestControlRequestId::parse(&wire.request_id)?,
        binary_from_wire(wire.binary)?,
        authority_from_wire(wire.authority)?,
        wire.operation,
        Sha256Digest::parse(&wire.payload_digest).map_err(|_| malformed())?,
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum OutcomeWire {
    Succeeded { result_digest: String },
    Refused { reason: TrustedGuestControlRefusal },
    RecoveryRequired { debt: TrustedGuestControlRecoveryDebt },
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

fn receipt_from_wire_for_schema(
    wire: ReceiptWire,
    expected_schema_version: u8,
) -> Result<TrustedGuestControlReceipt, TrustedGuestControlProtocolError> {
    if wire.schema_version != expected_schema_version {
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
        "trusted guest-control authority is invalid",
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
        .unwrap()
    }

    fn resident_sandbox_authority(project: &str) -> TrustedGuestControlAuthority {
        TrustedGuestControlAuthority::resident_sandbox(
            ProjectIdentity::parse(project).unwrap(),
            ResidentSandboxId::parse("sandbox-a").unwrap(),
            ResidentSandboxGeneration::new(11).unwrap(),
            TrustedGuestControlResidentConfigGeneration::new(3).unwrap(),
            TrustedGuestControlResidentConfigClaim::new(digest('c')),
            TrustedGuestControlResidentAuthorityGeneration::new(4).unwrap(),
        )
    }

    fn formatter_authority() -> TrustedGuestControlAuthority {
        TrustedGuestControlAuthority::formatter_project_disk(
            TrustedGuestControlTargetIdentity::formatter(
                ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
                ProjectDiskId::parse("disk-a").unwrap(),
                ProjectDiskGeneration::new(3).unwrap(),
                TrustedGuestControlFormatTransactionGeneration::new(5).unwrap(),
                TrustedGuestControlFormatterCarrierGeneration::new(7).unwrap(),
            ),
            TrustedGuestControlCreatedProvenanceClaim::new(digest('d')),
            TrustedGuestControlFormatAuthorityClaim::new(digest('e')),
            TrustedGuestControlFormatterConfigGeneration::new(9).unwrap(),
            TrustedGuestControlFormatterConfigClaim::new(digest('f')),
        )
        .unwrap()
    }

    fn pending_authority() -> TrustedGuestControlAuthority {
        TrustedGuestControlAuthority::resident_pending_project_disk_attachment(
            TrustedGuestControlTargetIdentity::resident(
                ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
                ResidentSandboxId::parse("sandbox-a").unwrap(),
                ResidentSandboxGeneration::new(11).unwrap(),
            ),
            ProjectDiskId::parse("disk-a").unwrap(),
            ProjectDiskGeneration::new(3).unwrap(),
            ProjectDiskRevision::new(2).unwrap(),
            ProjectDiskAttachmentGeneration::new(1).unwrap(),
            TrustedGuestControlAttachTransactionGeneration::new(6).unwrap(),
            TrustedGuestControlAttachAuthorityClaim::new(digest('a')),
        )
        .unwrap()
    }

    #[test]
    fn attached_authority_binds_current_revision_and_attachment_generation() {
        let authority =
            TrustedGuestControlAuthority::from_attached_project_disk(&attached_record()).unwrap();
        let attached = authority.resident_attached_project_disk().unwrap();
        assert_eq!(attached.project_disk_revision().get(), 2);
        assert_eq!(attached.attachment_generation().get(), 1);
        assert_eq!(authority.resident_sandbox_generation().unwrap().get(), 11);
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
    fn every_purpose_typed_authority_round_trips_only_with_its_closed_operation() {
        for (authority, operation, marker) in [
            (
                resident_sandbox_authority("github.com/teamleaderleo/smolrunner"),
                TrustedGuestControlOperation::ProbeGuestControl,
                "resident_sandbox",
            ),
            (
                pending_authority(),
                TrustedGuestControlOperation::ObservePendingProjectDiskAttachment,
                "resident_pending_project_disk_attachment",
            ),
            (
                TrustedGuestControlAuthority::from_attached_project_disk(&attached_record())
                    .unwrap(),
                TrustedGuestControlOperation::ObserveProjectFilesystem,
                "resident_attached_project_disk",
            ),
            (
                formatter_authority(),
                TrustedGuestControlOperation::FormatProjectFilesystem,
                "formatter_project_disk",
            ),
        ] {
            let request = TrustedGuestControlRequest::new(
                TrustedGuestControlRequestId::parse("request-purpose").unwrap(),
                TrustedGuestControlBinaryBinding::new(
                    7,
                    digest('b'),
                    TrustedGuestControlArchitecture::LinuxAarch64,
                )
                .unwrap(),
                authority,
                operation,
                digest('c'),
            )
            .unwrap();
            let bytes = encode_trusted_guest_control_request(&request).unwrap();
            assert!(std::str::from_utf8(&bytes).unwrap().contains(marker));
            assert_eq!(
                decode_trusted_guest_control_request(&bytes).unwrap(),
                request
            );
        }

        assert_eq!(
            TrustedGuestControlRequest::new(
                TrustedGuestControlRequestId::parse("request-purpose").unwrap(),
                TrustedGuestControlBinaryBinding::new(
                    7,
                    digest('b'),
                    TrustedGuestControlArchitecture::LinuxAarch64,
                )
                .unwrap(),
                formatter_authority(),
                TrustedGuestControlOperation::ObserveProjectFilesystem,
                digest('c'),
            )
            .unwrap_err()
            .kind(),
            TrustedGuestControlProtocolErrorKind::InvalidAuthority
        );
    }

    #[test]
    fn project_scope_and_authority_variant_change_request_identity() {
        let binary = TrustedGuestControlBinaryBinding::new(
            7,
            digest('a'),
            TrustedGuestControlArchitecture::LinuxAarch64,
        )
        .unwrap();
        let first = TrustedGuestControlRequest::new(
            TrustedGuestControlRequestId::parse("probe-1").unwrap(),
            binary.clone(),
            resident_sandbox_authority("github.com/teamleaderleo/smolrunner"),
            TrustedGuestControlOperation::ProbeGuestControl,
            digest('b'),
        )
        .unwrap();
        let other_project = TrustedGuestControlRequest::new(
            TrustedGuestControlRequestId::parse("probe-1").unwrap(),
            binary,
            resident_sandbox_authority("github.com/teamleaderleo/quarry"),
            TrustedGuestControlOperation::ProbeGuestControl,
            digest('b'),
        )
        .unwrap();
        assert_ne!(
            trusted_guest_control_request_digest(&first).unwrap(),
            trusted_guest_control_request_digest(&other_project).unwrap()
        );
        assert_ne!(
            first.authority().target_identity(),
            other_project.authority().target_identity()
        );
    }

    #[test]
    fn current_v3_and_legacy_v2_are_separate_generation_surfaces() {
        let current = encode_trusted_guest_control_request(&request()).unwrap();
        let legacy = std::str::from_utf8(&current).unwrap().replacen(
            "\"schema_version\":3",
            "\"schema_version\":2",
            1,
        );
        assert_eq!(
            decode_trusted_guest_control_request(legacy.as_bytes())
                .unwrap_err()
                .kind(),
            TrustedGuestControlProtocolErrorKind::VersionIncompatible
        );
        let decoded = decode_legacy_smolrunner_trusted_guest_control_request_v2(legacy.as_bytes())
            .unwrap();
        assert_eq!(decoded.request(), &request());
        assert_eq!(decoded.canonical_bytes(), legacy.as_bytes());
        assert_ne!(
            legacy_smolrunner_trusted_guest_control_request_v2_digest(&decoded).unwrap(),
            trusted_guest_control_request_digest(decoded.request()).unwrap()
        );
        assert_ne!(
            legacy_smolrunner_trusted_guest_control_authority_v2_digest(
                decoded.request().authority()
            )
            .unwrap(),
            trusted_guest_control_authority_digest(decoded.request().authority()).unwrap()
        );
    }

    #[test]
    fn protocol_v1_request_is_rejected_by_current_and_legacy_decoders() {
        let bytes = encode_trusted_guest_control_request(&request()).unwrap();
        let v1 = std::str::from_utf8(&bytes).unwrap().replacen(
            "\"schema_version\":3",
            "\"schema_version\":1",
            1,
        );
        assert_eq!(
            decode_trusted_guest_control_request(v1.as_bytes())
                .unwrap_err()
                .kind(),
            TrustedGuestControlProtocolErrorKind::VersionIncompatible
        );
        assert_eq!(
            decode_legacy_smolrunner_trusted_guest_control_request_v2(v1.as_bytes())
                .unwrap_err()
                .kind(),
            TrustedGuestControlProtocolErrorKind::VersionIncompatible
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
            "\"schema_version\":3",
            "\"schema_version\":3,\"extra\":true",
            1,
        );
        assert_eq!(
            decode_trusted_guest_control_request(unknown.as_bytes())
                .unwrap_err()
                .kind(),
            TrustedGuestControlProtocolErrorKind::Malformed
        );

        let future = text.replacen("\"schema_version\":3", "\"schema_version\":4", 1);
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
        let TrustedGuestControlAuthorityVariant::ResidentAttachedProjectDisk(authority) =
            &mut attachment.authority.variant
        else {
            panic!("fixture authority must be resident attached");
        };
        authority.attachment_generation = ProjectDiskAttachmentGeneration::new(2).unwrap();
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
    fn legacy_v2_receipt_requires_the_exact_legacy_request_digest() {
        let current_request = request();
        let current_request_bytes = encode_trusted_guest_control_request(&current_request).unwrap();
        let legacy_request_bytes = std::str::from_utf8(&current_request_bytes)
            .unwrap()
            .replacen("\"schema_version\":3", "\"schema_version\":2", 1);
        let legacy_request =
            decode_legacy_smolrunner_trusted_guest_control_request_v2(legacy_request_bytes.as_bytes())
                .unwrap();
        let outcome = TrustedGuestControlOutcome::Succeeded {
            result_digest: digest('d'),
        };
        let legacy_digest =
            legacy_smolrunner_trusted_guest_control_request_v2_digest(&legacy_request).unwrap();
        let wire = ReceiptWire {
            schema_version: 2,
            request_id: current_request.request_id().as_str().to_owned(),
            binary: BinaryWire::from(current_request.binary()),
            request_digest: legacy_digest.as_str().to_owned(),
            operation: current_request.operation(),
            outcome: OutcomeWire::from(&outcome),
        };
        let mut bytes = serde_json::to_vec(&wire).unwrap();
        bytes.push(b'\n');
        assert_eq!(
            decode_legacy_smolrunner_trusted_guest_control_receipt_v2(&bytes, &legacy_request)
                .unwrap()
                .receipt()
                .outcome(),
            &outcome
        );
        assert_eq!(
            decode_trusted_guest_control_receipt(&bytes, &current_request)
                .unwrap_err()
                .kind(),
            TrustedGuestControlProtocolErrorKind::VersionIncompatible
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
