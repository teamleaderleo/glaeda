//! Canonical operation-specific protocol for one resident project-filesystem observation.
//!
//! The common #588 guest-control envelope already binds the exact project-disk attachment identity
//! and an opaque payload digest. This module defines the closed payload/result behind the
//! `observe_project_filesystem` operation. It performs no process execution, filesystem I/O, mount
//! mutation, Lima operation, durable-state mutation, or project-filesystem proof minting.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::artifact::Sha256Digest;
use crate::project_catalog::ProjectIdentity;
use crate::project_disk_filesystem::{
    ProjectDiskFilesystemBinding, ProjectDiskFilesystemFormatProfileGeneration,
    ProjectDiskFilesystemGeneration, ProjectDiskFilesystemKind,
};
use crate::project_disk_lease::{
    ProjectDiskAttachmentGeneration, ProjectDiskGeneration, ProjectDiskId, ProjectDiskRevision,
    ResidentSandboxGeneration, ResidentSandboxId,
};
use crate::trusted_guest_control_protocol::{
    TrustedGuestControlAuthority, TrustedGuestControlOperation, TrustedGuestControlRequest,
};
use crate::trusted_guest_control_transaction::{
    trusted_guest_control_payload_body_digest, trusted_guest_control_result_body_digest,
};
use crate::trusted_project_filesystem_guest_observation::TrustedProjectFilesystemGuestObservation;

pub const TRUSTED_PROJECT_FILESYSTEM_GUEST_PROTOCOL_SCHEMA_VERSION: u8 = 1;
pub const MAX_TRUSTED_PROJECT_FILESYSTEM_PAYLOAD_BYTES: usize = 1_024;
pub const MAX_TRUSTED_PROJECT_FILESYSTEM_RESULT_BYTES: usize = 1_024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustedProjectFilesystemSelector {
    ResidentProjectRoot,
}

/// Closed payload for `TrustedGuestControlOperation::ObserveProjectFilesystem`.
///
/// Every durable attachment field is repeated here intentionally. The guest must require exact
/// equality with the common envelope before observing anything, so a payload cannot be replayed
/// under another disk revision, attachment generation, or resident sandbox generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedProjectFilesystemObservationPayload {
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    disk_revision: ProjectDiskRevision,
    attachment_generation: ProjectDiskAttachmentGeneration,
    sandbox_id: ResidentSandboxId,
    sandbox_generation: ResidentSandboxGeneration,
    filesystem_generation: ProjectDiskFilesystemGeneration,
    format_profile_generation: ProjectDiskFilesystemFormatProfileGeneration,
    filesystem_kind: ProjectDiskFilesystemKind,
    selector: TrustedProjectFilesystemSelector,
}

impl TrustedProjectFilesystemObservationPayload {
    /// Bind one current guest-control authority to one accepted P4 filesystem declaration.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal unless the filesystem declaration names the same project-disk
    /// generation as the common guest-control authority.
    pub fn new(
        authority: &TrustedGuestControlAuthority,
        filesystem: &ProjectDiskFilesystemBinding,
    ) -> Result<Self, TrustedProjectFilesystemGuestProtocolError> {
        let Some(attachment) = authority.resident_attached_project_disk() else {
            return Err(authority_mismatch());
        };
        let (Some(sandbox_id), Some(sandbox_generation)) = (
            authority.resident_sandbox_id(),
            authority.resident_sandbox_generation(),
        ) else {
            return Err(authority_mismatch());
        };
        if authority.project() != filesystem.project()
            || attachment.project_disk_id() != filesystem.disk_id()
            || attachment.project_disk_generation() != filesystem.disk_generation()
        {
            return Err(authority_mismatch());
        }
        Ok(Self {
            project: authority.project().clone(),
            disk_id: attachment.project_disk_id().clone(),
            disk_generation: attachment.project_disk_generation(),
            disk_revision: attachment.project_disk_revision(),
            attachment_generation: attachment.attachment_generation(),
            sandbox_id: sandbox_id.clone(),
            sandbox_generation,
            filesystem_generation: filesystem.filesystem_generation(),
            format_profile_generation: filesystem.format_profile_generation(),
            filesystem_kind: filesystem.kind(),
            selector: TrustedProjectFilesystemSelector::ResidentProjectRoot,
        })
    }

    #[must_use]
    pub const fn project(&self) -> &ProjectIdentity {
        &self.project
    }

    #[must_use]
    pub const fn disk_id(&self) -> &ProjectDiskId {
        &self.disk_id
    }

    #[must_use]
    pub const fn disk_generation(&self) -> ProjectDiskGeneration {
        self.disk_generation
    }

    #[must_use]
    pub const fn disk_revision(&self) -> ProjectDiskRevision {
        self.disk_revision
    }

    #[must_use]
    pub const fn attachment_generation(&self) -> ProjectDiskAttachmentGeneration {
        self.attachment_generation
    }

    #[must_use]
    pub const fn sandbox_id(&self) -> &ResidentSandboxId {
        &self.sandbox_id
    }

    #[must_use]
    pub const fn sandbox_generation(&self) -> ResidentSandboxGeneration {
        self.sandbox_generation
    }

    #[must_use]
    pub const fn filesystem_generation(&self) -> ProjectDiskFilesystemGeneration {
        self.filesystem_generation
    }

    #[must_use]
    pub const fn format_profile_generation(&self) -> ProjectDiskFilesystemFormatProfileGeneration {
        self.format_profile_generation
    }

    #[must_use]
    pub const fn filesystem_kind(&self) -> ProjectDiskFilesystemKind {
        self.filesystem_kind
    }

    #[must_use]
    pub const fn selector(&self) -> TrustedProjectFilesystemSelector {
        self.selector
    }

    /// Require exact operation, durable authority, and canonical payload digest equality with one
    /// common guest-control request before the guest handler may observe the filesystem.
    pub fn confirm_common_request(
        &self,
        request: &TrustedGuestControlRequest,
    ) -> Result<(), TrustedProjectFilesystemGuestProtocolError> {
        if request.operation() != TrustedGuestControlOperation::ObserveProjectFilesystem {
            return Err(operation_mismatch());
        }
        let authority = request.authority();
        let Some(attachment) = authority.resident_attached_project_disk() else {
            return Err(authority_mismatch());
        };
        if authority.project() != &self.project
            || attachment.project_disk_id() != &self.disk_id
            || attachment.project_disk_generation() != self.disk_generation
            || attachment.project_disk_revision() != self.disk_revision
            || attachment.attachment_generation() != self.attachment_generation
            || authority.resident_sandbox_id() != Some(&self.sandbox_id)
            || authority.resident_sandbox_generation() != Some(self.sandbox_generation)
        {
            return Err(authority_mismatch());
        }
        if request.payload_digest() != &trusted_project_filesystem_payload_digest(self)? {
            return Err(digest_mismatch());
        }
        Ok(())
    }
}

/// Path-free typed result of one successful trusted guest project-filesystem observation.
///
/// Construction requires the opaque successful guest observation. Raw `st_dev`, inode, mountpoint,
/// device-node name, and mountinfo bytes are absent from this result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedProjectFilesystemObservationResult {
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    disk_revision: ProjectDiskRevision,
    attachment_generation: ProjectDiskAttachmentGeneration,
    sandbox_id: ResidentSandboxId,
    sandbox_generation: ResidentSandboxGeneration,
    filesystem_generation: ProjectDiskFilesystemGeneration,
    format_profile_generation: ProjectDiskFilesystemFormatProfileGeneration,
    filesystem_kind: ProjectDiskFilesystemKind,
    device_mountinfo_bound: bool,
    read_write: bool,
}

impl TrustedProjectFilesystemObservationResult {
    /// Build one result only from a successful guest observation matching the exact request payload.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal if the observed filesystem generation/profile/kind disagrees with
    /// the accepted payload or the observation lacks its required device/mount/policy binding.
    pub fn from_guest_observation(
        payload: &TrustedProjectFilesystemObservationPayload,
        observation: &TrustedProjectFilesystemGuestObservation,
    ) -> Result<Self, TrustedProjectFilesystemGuestProtocolError> {
        let summary = observation.summary();
        if summary.filesystem_generation() != payload.filesystem_generation
            || summary.format_profile_generation() != payload.format_profile_generation
            || summary.filesystem_kind() != payload.filesystem_kind
            || !summary.filesystem_device_bound()
            || !summary.read_write()
        {
            return Err(result_mismatch());
        }
        Ok(Self {
            project: payload.project.clone(),
            disk_id: payload.disk_id.clone(),
            disk_generation: payload.disk_generation,
            disk_revision: payload.disk_revision,
            attachment_generation: payload.attachment_generation,
            sandbox_id: payload.sandbox_id.clone(),
            sandbox_generation: payload.sandbox_generation,
            filesystem_generation: payload.filesystem_generation,
            format_profile_generation: payload.format_profile_generation,
            filesystem_kind: payload.filesystem_kind,
            device_mountinfo_bound: true,
            read_write: true,
        })
    }

    #[must_use]
    pub const fn project(&self) -> &ProjectIdentity {
        &self.project
    }

    #[must_use]
    pub const fn disk_id(&self) -> &ProjectDiskId {
        &self.disk_id
    }

    #[must_use]
    pub const fn disk_generation(&self) -> ProjectDiskGeneration {
        self.disk_generation
    }

    #[must_use]
    pub const fn disk_revision(&self) -> ProjectDiskRevision {
        self.disk_revision
    }

    #[must_use]
    pub const fn attachment_generation(&self) -> ProjectDiskAttachmentGeneration {
        self.attachment_generation
    }

    #[must_use]
    pub const fn sandbox_id(&self) -> &ResidentSandboxId {
        &self.sandbox_id
    }

    #[must_use]
    pub const fn sandbox_generation(&self) -> ResidentSandboxGeneration {
        self.sandbox_generation
    }

    #[must_use]
    pub const fn filesystem_generation(&self) -> ProjectDiskFilesystemGeneration {
        self.filesystem_generation
    }

    #[must_use]
    pub const fn format_profile_generation(&self) -> ProjectDiskFilesystemFormatProfileGeneration {
        self.format_profile_generation
    }

    #[must_use]
    pub const fn filesystem_kind(&self) -> ProjectDiskFilesystemKind {
        self.filesystem_kind
    }

    #[must_use]
    pub const fn device_mountinfo_bound(&self) -> bool {
        self.device_mountinfo_bound
    }

    #[must_use]
    pub const fn read_write(&self) -> bool {
        self.read_write
    }

    /// Reconfirm that a decoded/result value still corresponds to the exact request payload.
    pub fn confirm_payload(
        &self,
        payload: &TrustedProjectFilesystemObservationPayload,
    ) -> Result<(), TrustedProjectFilesystemGuestProtocolError> {
        if self.project != payload.project
            || self.disk_id != payload.disk_id
            || self.disk_generation != payload.disk_generation
            || self.disk_revision != payload.disk_revision
            || self.attachment_generation != payload.attachment_generation
            || self.sandbox_id != payload.sandbox_id
            || self.sandbox_generation != payload.sandbox_generation
            || self.filesystem_generation != payload.filesystem_generation
            || self.format_profile_generation != payload.format_profile_generation
            || self.filesystem_kind != payload.filesystem_kind
            || !self.device_mountinfo_bound
            || !self.read_write
        {
            return Err(result_mismatch());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustedProjectFilesystemGuestProtocolErrorKind {
    InvalidIdentity,
    AuthorityMismatch,
    OperationMismatch,
    DigestMismatch,
    TooLarge,
    Malformed,
    VersionIncompatible,
    NonCanonical,
    ResultMismatch,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TrustedProjectFilesystemGuestProtocolError {
    kind: TrustedProjectFilesystemGuestProtocolErrorKind,
    code: &'static str,
    message: &'static str,
}

impl TrustedProjectFilesystemGuestProtocolError {
    #[must_use]
    pub const fn kind(self) -> TrustedProjectFilesystemGuestProtocolErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for TrustedProjectFilesystemGuestProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedProjectFilesystemGuestProtocolError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for TrustedProjectFilesystemGuestProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for TrustedProjectFilesystemGuestProtocolError {}

/// Encode one strict compact operation payload followed by one newline.
pub fn encode_trusted_project_filesystem_payload(
    payload: &TrustedProjectFilesystemObservationPayload,
) -> Result<Vec<u8>, TrustedProjectFilesystemGuestProtocolError> {
    let mut bytes = encode_trusted_project_filesystem_payload_body(payload)?;
    bytes.push(b'\n');
    Ok(bytes)
}

pub fn encode_trusted_project_filesystem_payload_body(
    payload: &TrustedProjectFilesystemObservationPayload,
) -> Result<Vec<u8>, TrustedProjectFilesystemGuestProtocolError> {
    canonical_json_body(
        &PayloadWire::from(payload),
        MAX_TRUSTED_PROJECT_FILESYSTEM_PAYLOAD_BYTES - 1,
    )
}

/// Decode one canonical operation payload. Decoded bytes remain claims until
/// `confirm_common_request` succeeds against the common #588 envelope.
pub fn decode_trusted_project_filesystem_payload(
    bytes: &[u8],
) -> Result<TrustedProjectFilesystemObservationPayload, TrustedProjectFilesystemGuestProtocolError>
{
    require_size(bytes, MAX_TRUSTED_PROJECT_FILESYSTEM_PAYLOAD_BYTES)?;
    let Some(body) = bytes.strip_suffix(b"\n") else {
        return Err(noncanonical());
    };
    decode_trusted_project_filesystem_payload_body(body)
}

pub fn decode_trusted_project_filesystem_payload_body(
    bytes: &[u8],
) -> Result<TrustedProjectFilesystemObservationPayload, TrustedProjectFilesystemGuestProtocolError>
{
    require_size(bytes, MAX_TRUSTED_PROJECT_FILESYSTEM_PAYLOAD_BYTES - 1)?;
    let wire: PayloadWire = serde_json::from_slice(bytes).map_err(|_| malformed())?;
    if wire.schema_version != TRUSTED_PROJECT_FILESYSTEM_GUEST_PROTOCOL_SCHEMA_VERSION {
        return Err(version_incompatible());
    }
    let payload = payload_from_wire(wire)?;
    if encode_trusted_project_filesystem_payload_body(&payload)? != bytes {
        return Err(noncanonical());
    }
    Ok(payload)
}

/// Domain-separated digest placed in the common #588 request envelope.
pub fn trusted_project_filesystem_payload_digest(
    payload: &TrustedProjectFilesystemObservationPayload,
) -> Result<Sha256Digest, TrustedProjectFilesystemGuestProtocolError> {
    trusted_guest_control_payload_body_digest(
        TrustedGuestControlOperation::ObserveProjectFilesystem,
        &encode_trusted_project_filesystem_payload_body(payload)?,
    )
    .map_err(|_| malformed())
}

/// Encode one path-free typed result followed by one newline.
pub fn encode_trusted_project_filesystem_result(
    result: &TrustedProjectFilesystemObservationResult,
) -> Result<Vec<u8>, TrustedProjectFilesystemGuestProtocolError> {
    let mut bytes = encode_trusted_project_filesystem_result_body(result)?;
    bytes.push(b'\n');
    Ok(bytes)
}

pub fn encode_trusted_project_filesystem_result_body(
    result: &TrustedProjectFilesystemObservationResult,
) -> Result<Vec<u8>, TrustedProjectFilesystemGuestProtocolError> {
    canonical_json_body(
        &ResultWire::from(result),
        MAX_TRUSTED_PROJECT_FILESYSTEM_RESULT_BYTES - 1,
    )
}

/// Decode one path-free result claim. The Mac/common receipt must still bind its result digest and
/// current durable reconciliation before accepting it as a completed observation.
pub fn decode_trusted_project_filesystem_result(
    bytes: &[u8],
) -> Result<TrustedProjectFilesystemObservationResult, TrustedProjectFilesystemGuestProtocolError> {
    require_size(bytes, MAX_TRUSTED_PROJECT_FILESYSTEM_RESULT_BYTES)?;
    let Some(body) = bytes.strip_suffix(b"\n") else {
        return Err(noncanonical());
    };
    decode_trusted_project_filesystem_result_body(body)
}

pub fn decode_trusted_project_filesystem_result_body(
    bytes: &[u8],
) -> Result<TrustedProjectFilesystemObservationResult, TrustedProjectFilesystemGuestProtocolError> {
    require_size(bytes, MAX_TRUSTED_PROJECT_FILESYSTEM_RESULT_BYTES - 1)?;
    let wire: ResultWire = serde_json::from_slice(bytes).map_err(|_| malformed())?;
    if wire.schema_version != TRUSTED_PROJECT_FILESYSTEM_GUEST_PROTOCOL_SCHEMA_VERSION {
        return Err(version_incompatible());
    }
    let result = result_from_wire(wire)?;
    if encode_trusted_project_filesystem_result_body(&result)? != bytes {
        return Err(noncanonical());
    }
    Ok(result)
}

/// Domain-separated digest suitable for the common guest-control success outcome.
pub fn trusted_project_filesystem_result_digest(
    result: &TrustedProjectFilesystemObservationResult,
) -> Result<Sha256Digest, TrustedProjectFilesystemGuestProtocolError> {
    trusted_guest_control_result_body_digest(
        TrustedGuestControlOperation::ObserveProjectFilesystem,
        &encode_trusted_project_filesystem_result_body(result)?,
    )
    .map_err(|_| malformed())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FilesystemKindWire {
    Ext4,
    Xfs,
}

impl From<ProjectDiskFilesystemKind> for FilesystemKindWire {
    fn from(value: ProjectDiskFilesystemKind) -> Self {
        match value {
            ProjectDiskFilesystemKind::Ext4 => Self::Ext4,
            ProjectDiskFilesystemKind::Xfs => Self::Xfs,
        }
    }
}

impl From<FilesystemKindWire> for ProjectDiskFilesystemKind {
    fn from(value: FilesystemKindWire) -> Self {
        match value {
            FilesystemKindWire::Ext4 => Self::Ext4,
            FilesystemKindWire::Xfs => Self::Xfs,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SelectorWire {
    ResidentProjectRoot,
}

impl From<TrustedProjectFilesystemSelector> for SelectorWire {
    fn from(value: TrustedProjectFilesystemSelector) -> Self {
        match value {
            TrustedProjectFilesystemSelector::ResidentProjectRoot => Self::ResidentProjectRoot,
        }
    }
}

impl From<SelectorWire> for TrustedProjectFilesystemSelector {
    fn from(value: SelectorWire) -> Self {
        match value {
            SelectorWire::ResidentProjectRoot => Self::ResidentProjectRoot,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PayloadWire {
    schema_version: u8,
    project: String,
    disk_id: String,
    disk_generation: u64,
    disk_revision: u64,
    attachment_generation: u64,
    sandbox_id: String,
    sandbox_generation: u64,
    filesystem_generation: u64,
    format_profile_generation: u64,
    filesystem_kind: FilesystemKindWire,
    selector: SelectorWire,
}

impl From<&TrustedProjectFilesystemObservationPayload> for PayloadWire {
    fn from(value: &TrustedProjectFilesystemObservationPayload) -> Self {
        Self {
            schema_version: TRUSTED_PROJECT_FILESYSTEM_GUEST_PROTOCOL_SCHEMA_VERSION,
            project: value.project.as_str().to_owned(),
            disk_id: value.disk_id.as_str().to_owned(),
            disk_generation: value.disk_generation.get(),
            disk_revision: value.disk_revision.get(),
            attachment_generation: value.attachment_generation.get(),
            sandbox_id: value.sandbox_id.as_str().to_owned(),
            sandbox_generation: value.sandbox_generation.get(),
            filesystem_generation: value.filesystem_generation.get(),
            format_profile_generation: value.format_profile_generation.get(),
            filesystem_kind: value.filesystem_kind.into(),
            selector: value.selector.into(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultWire {
    schema_version: u8,
    project: String,
    disk_id: String,
    disk_generation: u64,
    disk_revision: u64,
    attachment_generation: u64,
    sandbox_id: String,
    sandbox_generation: u64,
    filesystem_generation: u64,
    format_profile_generation: u64,
    filesystem_kind: FilesystemKindWire,
    device_mountinfo_bound: bool,
    read_write: bool,
}

impl From<&TrustedProjectFilesystemObservationResult> for ResultWire {
    fn from(value: &TrustedProjectFilesystemObservationResult) -> Self {
        Self {
            schema_version: TRUSTED_PROJECT_FILESYSTEM_GUEST_PROTOCOL_SCHEMA_VERSION,
            project: value.project.as_str().to_owned(),
            disk_id: value.disk_id.as_str().to_owned(),
            disk_generation: value.disk_generation.get(),
            disk_revision: value.disk_revision.get(),
            attachment_generation: value.attachment_generation.get(),
            sandbox_id: value.sandbox_id.as_str().to_owned(),
            sandbox_generation: value.sandbox_generation.get(),
            filesystem_generation: value.filesystem_generation.get(),
            format_profile_generation: value.format_profile_generation.get(),
            filesystem_kind: value.filesystem_kind.into(),
            device_mountinfo_bound: value.device_mountinfo_bound,
            read_write: value.read_write,
        }
    }
}

fn payload_from_wire(
    wire: PayloadWire,
) -> Result<TrustedProjectFilesystemObservationPayload, TrustedProjectFilesystemGuestProtocolError>
{
    Ok(TrustedProjectFilesystemObservationPayload {
        project: ProjectIdentity::parse(&wire.project).map_err(|_| invalid_identity())?,
        disk_id: ProjectDiskId::parse(&wire.disk_id).map_err(|_| invalid_identity())?,
        disk_generation: ProjectDiskGeneration::new(wire.disk_generation)
            .map_err(|_| invalid_identity())?,
        disk_revision: ProjectDiskRevision::new(wire.disk_revision)
            .map_err(|_| invalid_identity())?,
        attachment_generation: ProjectDiskAttachmentGeneration::new(wire.attachment_generation)
            .map_err(|_| invalid_identity())?,
        sandbox_id: ResidentSandboxId::parse(&wire.sandbox_id).map_err(|_| invalid_identity())?,
        sandbox_generation: ResidentSandboxGeneration::new(wire.sandbox_generation)
            .map_err(|_| invalid_identity())?,
        filesystem_generation: ProjectDiskFilesystemGeneration::new(wire.filesystem_generation)
            .map_err(|_| invalid_identity())?,
        format_profile_generation: ProjectDiskFilesystemFormatProfileGeneration::new(
            wire.format_profile_generation,
        )
        .map_err(|_| invalid_identity())?,
        filesystem_kind: wire.filesystem_kind.into(),
        selector: wire.selector.into(),
    })
}

fn result_from_wire(
    wire: ResultWire,
) -> Result<TrustedProjectFilesystemObservationResult, TrustedProjectFilesystemGuestProtocolError> {
    if !wire.device_mountinfo_bound || !wire.read_write {
        return Err(result_mismatch());
    }
    Ok(TrustedProjectFilesystemObservationResult {
        project: ProjectIdentity::parse(&wire.project).map_err(|_| invalid_identity())?,
        disk_id: ProjectDiskId::parse(&wire.disk_id).map_err(|_| invalid_identity())?,
        disk_generation: ProjectDiskGeneration::new(wire.disk_generation)
            .map_err(|_| invalid_identity())?,
        disk_revision: ProjectDiskRevision::new(wire.disk_revision)
            .map_err(|_| invalid_identity())?,
        attachment_generation: ProjectDiskAttachmentGeneration::new(wire.attachment_generation)
            .map_err(|_| invalid_identity())?,
        sandbox_id: ResidentSandboxId::parse(&wire.sandbox_id).map_err(|_| invalid_identity())?,
        sandbox_generation: ResidentSandboxGeneration::new(wire.sandbox_generation)
            .map_err(|_| invalid_identity())?,
        filesystem_generation: ProjectDiskFilesystemGeneration::new(wire.filesystem_generation)
            .map_err(|_| invalid_identity())?,
        format_profile_generation: ProjectDiskFilesystemFormatProfileGeneration::new(
            wire.format_profile_generation,
        )
        .map_err(|_| invalid_identity())?,
        filesystem_kind: wire.filesystem_kind.into(),
        device_mountinfo_bound: true,
        read_write: true,
    })
}

fn canonical_json_body<T: Serialize>(
    value: &T,
    maximum: usize,
) -> Result<Vec<u8>, TrustedProjectFilesystemGuestProtocolError> {
    let bytes = serde_json::to_vec(value).map_err(|_| malformed())?;
    if bytes.len() > maximum {
        return Err(too_large());
    }
    Ok(bytes)
}

fn require_size(
    bytes: &[u8],
    maximum: usize,
) -> Result<(), TrustedProjectFilesystemGuestProtocolError> {
    if bytes.is_empty() || bytes.len() > maximum {
        return Err(too_large());
    }
    Ok(())
}

const fn error(
    kind: TrustedProjectFilesystemGuestProtocolErrorKind,
    code: &'static str,
    message: &'static str,
) -> TrustedProjectFilesystemGuestProtocolError {
    TrustedProjectFilesystemGuestProtocolError {
        kind,
        code,
        message,
    }
}

const fn invalid_identity() -> TrustedProjectFilesystemGuestProtocolError {
    error(
        TrustedProjectFilesystemGuestProtocolErrorKind::InvalidIdentity,
        "project_filesystem_guest_protocol_identity_invalid",
        "project filesystem guest protocol identity is invalid",
    )
}

const fn authority_mismatch() -> TrustedProjectFilesystemGuestProtocolError {
    error(
        TrustedProjectFilesystemGuestProtocolErrorKind::AuthorityMismatch,
        "project_filesystem_guest_protocol_authority_mismatch",
        "project filesystem payload disagrees with the current guest-control authority",
    )
}

const fn operation_mismatch() -> TrustedProjectFilesystemGuestProtocolError {
    error(
        TrustedProjectFilesystemGuestProtocolErrorKind::OperationMismatch,
        "project_filesystem_guest_protocol_operation_mismatch",
        "project filesystem payload was supplied to another guest-control operation",
    )
}

const fn digest_mismatch() -> TrustedProjectFilesystemGuestProtocolError {
    error(
        TrustedProjectFilesystemGuestProtocolErrorKind::DigestMismatch,
        "project_filesystem_guest_protocol_digest_mismatch",
        "project filesystem payload digest disagrees with the common guest-control request",
    )
}

const fn too_large() -> TrustedProjectFilesystemGuestProtocolError {
    error(
        TrustedProjectFilesystemGuestProtocolErrorKind::TooLarge,
        "project_filesystem_guest_protocol_too_large",
        "project filesystem guest protocol document exceeds its fixed bound",
    )
}

const fn malformed() -> TrustedProjectFilesystemGuestProtocolError {
    error(
        TrustedProjectFilesystemGuestProtocolErrorKind::Malformed,
        "project_filesystem_guest_protocol_malformed",
        "project filesystem guest protocol document is malformed",
    )
}

const fn version_incompatible() -> TrustedProjectFilesystemGuestProtocolError {
    error(
        TrustedProjectFilesystemGuestProtocolErrorKind::VersionIncompatible,
        "project_filesystem_guest_protocol_version_incompatible",
        "project filesystem guest protocol version is incompatible",
    )
}

const fn noncanonical() -> TrustedProjectFilesystemGuestProtocolError {
    error(
        TrustedProjectFilesystemGuestProtocolErrorKind::NonCanonical,
        "project_filesystem_guest_protocol_noncanonical",
        "project filesystem guest protocol document is not canonical",
    )
}

const fn result_mismatch() -> TrustedProjectFilesystemGuestProtocolError {
    error(
        TrustedProjectFilesystemGuestProtocolErrorKind::ResultMismatch,
        "project_filesystem_guest_protocol_result_mismatch",
        "project filesystem observation result disagrees with the accepted request",
    )
}

#[cfg(test)]
mod tests {
    use super::{
        TrustedProjectFilesystemGuestProtocolErrorKind, TrustedProjectFilesystemObservationPayload,
        TrustedProjectFilesystemObservationResult, decode_trusted_project_filesystem_payload,
        decode_trusted_project_filesystem_result, encode_trusted_project_filesystem_payload,
        encode_trusted_project_filesystem_result, trusted_project_filesystem_payload_digest,
        trusted_project_filesystem_result_digest,
    };
    use crate::artifact::Sha256Digest;
    use crate::project_catalog::ProjectIdentity;
    use crate::project_disk_filesystem::{
        ProjectDiskFilesystemBinding, ProjectDiskFilesystemFormatProfileGeneration,
        ProjectDiskFilesystemGeneration, ProjectDiskFilesystemKind,
    };
    use crate::project_disk_lease::{
        ProjectDiskGeneration, ProjectDiskId, ProjectDiskLeaseRecord, ProjectDiskLockObservation,
        ProjectDiskObservation, ProjectDiskPhysicalObservation, ProjectDiskRecoverability,
        ProjectDiskUseObservation, ResidentSandboxGeneration, ResidentSandboxId,
    };
    use crate::trusted_guest_control_protocol::{
        TrustedGuestControlArchitecture, TrustedGuestControlAuthority,
        TrustedGuestControlBinaryBinding, TrustedGuestControlOperation, TrustedGuestControlRequest,
        TrustedGuestControlRequestId,
    };
    use crate::trusted_project_filesystem_guest_observation::observe_trusted_project_filesystem_guest;

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
                    ProjectDiskRecoverability::Unknown,
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
                    ProjectDiskRecoverability::Unknown,
                ),
            )
            .unwrap()
    }

    fn filesystem(
        record: &ProjectDiskLeaseRecord,
        generation: u64,
    ) -> ProjectDiskFilesystemBinding {
        ProjectDiskFilesystemBinding::new(
            record,
            ProjectDiskFilesystemGeneration::new(generation).unwrap(),
            ProjectDiskFilesystemFormatProfileGeneration::new(2).unwrap(),
            ProjectDiskFilesystemKind::Ext4,
        )
    }

    fn common_request(
        authority: TrustedGuestControlAuthority,
        operation: TrustedGuestControlOperation,
        digest: Sha256Digest,
    ) -> TrustedGuestControlRequest {
        TrustedGuestControlRequest::new(
            TrustedGuestControlRequestId::parse("project-fs-1").unwrap(),
            TrustedGuestControlBinaryBinding::new(
                4,
                Sha256Digest::parse(
                    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                )
                .unwrap(),
                TrustedGuestControlArchitecture::LinuxAarch64,
            )
            .unwrap(),
            authority,
            operation,
            digest,
        )
        .unwrap()
    }

    #[test]
    fn payload_is_canonical_and_binds_exact_common_authority() {
        let record = attached_record();
        let authority = TrustedGuestControlAuthority::from_attached_project_disk(&record).unwrap();
        let payload =
            TrustedProjectFilesystemObservationPayload::new(&authority, &filesystem(&record, 7))
                .unwrap();
        let bytes = encode_trusted_project_filesystem_payload(&payload).unwrap();
        let decoded = decode_trusted_project_filesystem_payload(&bytes).unwrap();
        assert_eq!(decoded, payload);
        let digest = trusted_project_filesystem_payload_digest(&payload).unwrap();
        let request = common_request(
            authority,
            TrustedGuestControlOperation::ObserveProjectFilesystem,
            digest,
        );
        decoded.confirm_common_request(&request).unwrap();
    }

    #[test]
    fn payload_refuses_wrong_project_disk_binding() {
        let record = attached_record();
        let authority = TrustedGuestControlAuthority::from_attached_project_disk(&record).unwrap();
        let other = ProjectDiskLeaseRecord::new_detached(
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            ProjectDiskId::parse("disk-b").unwrap(),
            ProjectDiskGeneration::new(3).unwrap(),
        );
        assert_eq!(
            TrustedProjectFilesystemObservationPayload::new(&authority, &filesystem(&other, 7))
                .unwrap_err()
                .kind(),
            TrustedProjectFilesystemGuestProtocolErrorKind::AuthorityMismatch
        );
    }

    #[test]
    fn common_request_operation_and_digest_must_match() {
        let record = attached_record();
        let authority = TrustedGuestControlAuthority::from_attached_project_disk(&record).unwrap();
        let payload =
            TrustedProjectFilesystemObservationPayload::new(&authority, &filesystem(&record, 7))
                .unwrap();
        let digest = trusted_project_filesystem_payload_digest(&payload).unwrap();
        let wrong_operation = common_request(
            authority.clone(),
            TrustedGuestControlOperation::ObserveImmutableGitPool,
            digest.clone(),
        );
        assert_eq!(
            payload
                .confirm_common_request(&wrong_operation)
                .unwrap_err()
                .kind(),
            TrustedProjectFilesystemGuestProtocolErrorKind::OperationMismatch
        );
        let wrong_digest = common_request(
            authority,
            TrustedGuestControlOperation::ObserveProjectFilesystem,
            Sha256Digest::parse(
                "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            )
            .unwrap(),
        );
        assert_eq!(
            payload
                .confirm_common_request(&wrong_digest)
                .unwrap_err()
                .kind(),
            TrustedProjectFilesystemGuestProtocolErrorKind::DigestMismatch
        );
    }

    #[test]
    fn noncanonical_and_unknown_payload_fields_are_rejected() {
        let record = attached_record();
        let authority = TrustedGuestControlAuthority::from_attached_project_disk(&record).unwrap();
        let payload =
            TrustedProjectFilesystemObservationPayload::new(&authority, &filesystem(&record, 7))
                .unwrap();
        let canonical = encode_trusted_project_filesystem_payload(&payload).unwrap();
        let mut spaced = canonical.clone();
        spaced.insert(1, b' ');
        assert_eq!(
            decode_trusted_project_filesystem_payload(&spaced)
                .unwrap_err()
                .kind(),
            TrustedProjectFilesystemGuestProtocolErrorKind::NonCanonical
        );

        let text = std::str::from_utf8(&canonical).unwrap().trim_end();
        let with_unknown = text.strip_suffix('}').unwrap().to_owned() + ",\"extra\":1}\n";
        assert_eq!(
            decode_trusted_project_filesystem_payload(with_unknown.as_bytes())
                .unwrap_err()
                .kind(),
            TrustedProjectFilesystemGuestProtocolErrorKind::Malformed
        );
    }

    #[test]
    fn successful_guest_observation_produces_path_free_result() {
        let record = attached_record();
        let authority = TrustedGuestControlAuthority::from_attached_project_disk(&record).unwrap();
        let fs = filesystem(&record, 7);
        let payload = TrustedProjectFilesystemObservationPayload::new(&authority, &fs).unwrap();
        let observation = observe_trusted_project_filesystem_guest(
            &fs,
            2049,
            99,
            b"/srv/project",
            b"123 45 8:1 / /srv/project rw - ext4 /dev/vda1 rw\n",
        )
        .unwrap();
        let result = TrustedProjectFilesystemObservationResult::from_guest_observation(
            &payload,
            &observation,
        )
        .unwrap();
        result.confirm_payload(&payload).unwrap();
        let bytes = encode_trusted_project_filesystem_result(&result).unwrap();
        let decoded = decode_trusted_project_filesystem_result(&bytes).unwrap();
        decoded.confirm_payload(&payload).unwrap();
        assert!(decoded.device_mountinfo_bound());
        assert!(decoded.read_write());
        assert!(
            !bytes
                .windows(b"/srv/project".len())
                .any(|w| w == b"/srv/project")
        );
        assert!(!bytes.windows(b"/dev/vda1".len()).any(|w| w == b"/dev/vda1"));
        assert!(!bytes.windows(b"2049".len()).any(|w| w == b"2049"));
        let digest = trusted_project_filesystem_result_digest(&decoded).unwrap();
        assert!(digest.as_str().starts_with("sha256:"));
    }

    #[test]
    fn decoded_success_result_refuses_negative_success_flags() {
        let record = attached_record();
        let authority = TrustedGuestControlAuthority::from_attached_project_disk(&record).unwrap();
        let fs = filesystem(&record, 7);
        let payload = TrustedProjectFilesystemObservationPayload::new(&authority, &fs).unwrap();
        let observation = observe_trusted_project_filesystem_guest(
            &fs,
            2049,
            99,
            b"/srv/project",
            b"123 45 8:1 / /srv/project rw - ext4 /dev/vda1 rw\n",
        )
        .unwrap();
        let result = TrustedProjectFilesystemObservationResult::from_guest_observation(
            &payload,
            &observation,
        )
        .unwrap();
        let canonical =
            String::from_utf8(encode_trusted_project_filesystem_result(&result).unwrap()).unwrap();
        for field in ["\"device_mountinfo_bound\":true", "\"read_write\":true"] {
            let invalid = canonical.replacen(field, &field.replace("true", "false"), 1);
            assert_eq!(
                decode_trusted_project_filesystem_result(invalid.as_bytes())
                    .unwrap_err()
                    .kind(),
                TrustedProjectFilesystemGuestProtocolErrorKind::ResultMismatch
            );
        }
    }

    #[test]
    fn result_from_another_filesystem_generation_is_rejected() {
        let record = attached_record();
        let authority = TrustedGuestControlAuthority::from_attached_project_disk(&record).unwrap();
        let expected_fs = filesystem(&record, 7);
        let observed_fs = filesystem(&record, 8);
        let payload =
            TrustedProjectFilesystemObservationPayload::new(&authority, &expected_fs).unwrap();
        let observation = observe_trusted_project_filesystem_guest(
            &observed_fs,
            2049,
            99,
            b"/srv/project",
            b"123 45 8:1 / /srv/project rw - ext4 /dev/vda1 rw\n",
        )
        .unwrap();
        assert_eq!(
            TrustedProjectFilesystemObservationResult::from_guest_observation(
                &payload,
                &observation
            )
            .unwrap_err()
            .kind(),
            TrustedProjectFilesystemGuestProtocolErrorKind::ResultMismatch
        );
    }
}
