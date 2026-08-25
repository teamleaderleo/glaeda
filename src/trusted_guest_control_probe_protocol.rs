//! Mutation-free protocol for proving the root-only guest-control transport.
//!
//! The probe reports only fixed dispatcher-local facts. It does not inspect the guest environment,
//! filesystem, network, hostname, process table, or arbitrary command output, and it creates no
//! durable guest authority.

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::project_catalog::ProjectIdentity;
use crate::project_disk_lease::{ResidentSandboxGeneration, ResidentSandboxId};
use crate::trusted_guest_control_protocol::{
    TRUSTED_GUEST_CONTROL_PROTOCOL_SCHEMA_VERSION, TrustedGuestControlArchitecture,
    TrustedGuestControlAuthority, TrustedGuestControlAuthorityKind,
    TrustedGuestControlBinaryBinding, TrustedGuestControlOperation, TrustedGuestControlRequest,
    TrustedGuestControlTargetIdentity, trusted_guest_control_authority_digest,
};

pub const TRUSTED_GUEST_CONTROL_PROBE_SCHEMA_VERSION: u8 = 1;
pub const MAX_TRUSTED_GUEST_CONTROL_PROBE_PAYLOAD_BYTES: usize = 1_024;
pub const MAX_TRUSTED_GUEST_CONTROL_PROBE_RESULT_BYTES: usize = 1_024;

const PAYLOAD_DIGEST_DOMAIN: &[u8] = b"smolrunner-trusted-guest-control-probe-payload-v1\0";
const RESULT_DIGEST_DOMAIN: &[u8] = b"smolrunner-trusted-guest-control-probe-result-v1\0";
const SHA256_PREFIX: &str = "sha256:";
const HEX: &[u8; 16] = b"0123456789abcdef";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedGuestControlProbePayload {
    project: ProjectIdentity,
    sandbox_id: ResidentSandboxId,
    sandbox_generation: ResidentSandboxGeneration,
    authority_digest: Sha256Digest,
    expected_binary_generation: u64,
    protocol_schema_version: u8,
    probe_policy_generation: u64,
}

impl TrustedGuestControlProbePayload {
    /// Bind one resident-sandbox authority to the expected binary and probe-policy generations.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal unless the authority is exactly `ResidentSandbox`, its target is
    /// resident, and the probe-policy generation is positive.
    pub fn new(
        authority: &TrustedGuestControlAuthority,
        binary: &TrustedGuestControlBinaryBinding,
        probe_policy_generation: u64,
    ) -> Result<Self, TrustedGuestControlProbeProtocolError> {
        if authority.kind() != TrustedGuestControlAuthorityKind::ResidentSandbox
            || probe_policy_generation == 0
        {
            return Err(authority_mismatch());
        }
        let TrustedGuestControlTargetIdentity::Resident {
            project,
            sandbox_id,
            sandbox_generation,
        } = authority.target_identity()
        else {
            return Err(authority_mismatch());
        };
        Ok(Self {
            project: project.clone(),
            sandbox_id: sandbox_id.clone(),
            sandbox_generation: *sandbox_generation,
            authority_digest: trusted_guest_control_authority_digest(authority)
                .map_err(|_| authority_mismatch())?,
            expected_binary_generation: binary.generation(),
            protocol_schema_version: TRUSTED_GUEST_CONTROL_PROTOCOL_SCHEMA_VERSION,
            probe_policy_generation,
        })
    }

    #[must_use]
    pub const fn project(&self) -> &ProjectIdentity {
        &self.project
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
    pub const fn expected_binary_generation(&self) -> u64 {
        self.expected_binary_generation
    }

    #[must_use]
    pub const fn authority_digest(&self) -> &Sha256Digest {
        &self.authority_digest
    }

    #[must_use]
    pub const fn protocol_schema_version(&self) -> u8 {
        self.protocol_schema_version
    }

    #[must_use]
    pub const fn probe_policy_generation(&self) -> u64 {
        self.probe_policy_generation
    }

    /// Confirm the exact common request before the trusted dispatcher admits the probe handler.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal on operation, authority, target, binary, protocol, or payload
    /// digest mismatch.
    pub fn confirm_common_request(
        &self,
        request: &TrustedGuestControlRequest,
    ) -> Result<(), TrustedGuestControlProbeProtocolError> {
        let expected_target = TrustedGuestControlTargetIdentity::resident(
            self.project.clone(),
            self.sandbox_id.clone(),
            self.sandbox_generation,
        );
        if request.operation() != TrustedGuestControlOperation::ProbeGuestControl
            || request.authority().kind() != TrustedGuestControlAuthorityKind::ResidentSandbox
            || request.authority().target_identity() != &expected_target
            || trusted_guest_control_authority_digest(request.authority())
                .map_err(|_| authority_mismatch())?
                != self.authority_digest
            || request.binary().generation() != self.expected_binary_generation
            || self.protocol_schema_version != TRUSTED_GUEST_CONTROL_PROTOCOL_SCHEMA_VERSION
        {
            return Err(authority_mismatch());
        }
        if request.payload_digest() != &trusted_guest_control_probe_payload_digest(self)? {
            return Err(digest_mismatch());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedGuestControlProbeResult {
    protocol_schema_version: u8,
    binary_generation: u64,
    effective_root_admitted: bool,
    architecture: TrustedGuestControlArchitecture,
    probe_policy_generation: u64,
}

impl TrustedGuestControlProbeResult {
    /// Mint the fixed mutation-free result only behind the future root dispatcher.
    #[must_use]
    #[allow(dead_code)]
    pub(crate) const fn from_verified_dispatch(payload: &TrustedGuestControlProbePayload) -> Self {
        Self {
            protocol_schema_version: TRUSTED_GUEST_CONTROL_PROTOCOL_SCHEMA_VERSION,
            binary_generation: payload.expected_binary_generation,
            effective_root_admitted: true,
            architecture: TrustedGuestControlArchitecture::LinuxAarch64,
            probe_policy_generation: payload.probe_policy_generation,
        }
    }

    #[must_use]
    pub const fn protocol_schema_version(&self) -> u8 {
        self.protocol_schema_version
    }

    #[must_use]
    pub const fn binary_generation(&self) -> u64 {
        self.binary_generation
    }

    #[must_use]
    pub const fn effective_root_admitted(&self) -> bool {
        self.effective_root_admitted
    }

    #[must_use]
    pub const fn architecture(&self) -> TrustedGuestControlArchitecture {
        self.architecture
    }

    #[must_use]
    pub const fn probe_policy_generation(&self) -> u64 {
        self.probe_policy_generation
    }

    /// Confirm that the result belongs to the exact probe payload.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal for any binary/protocol/policy or fixed-success mismatch.
    pub fn confirm_payload(
        &self,
        payload: &TrustedGuestControlProbePayload,
    ) -> Result<(), TrustedGuestControlProbeProtocolError> {
        if self.protocol_schema_version != payload.protocol_schema_version
            || self.binary_generation != payload.expected_binary_generation
            || !self.effective_root_admitted
            || self.architecture != TrustedGuestControlArchitecture::LinuxAarch64
            || self.probe_policy_generation != payload.probe_policy_generation
        {
            return Err(result_mismatch());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustedGuestControlProbeProtocolErrorKind {
    AuthorityMismatch,
    DigestMismatch,
    ResultMismatch,
    TooLarge,
    Malformed,
    NonCanonical,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TrustedGuestControlProbeProtocolError {
    kind: TrustedGuestControlProbeProtocolErrorKind,
    code: &'static str,
    message: &'static str,
}

impl TrustedGuestControlProbeProtocolError {
    #[must_use]
    pub const fn kind(self) -> TrustedGuestControlProbeProtocolErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for TrustedGuestControlProbeProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedGuestControlProbeProtocolError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for TrustedGuestControlProbeProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for TrustedGuestControlProbeProtocolError {}

pub fn encode_trusted_guest_control_probe_payload(
    payload: &TrustedGuestControlProbePayload,
) -> Result<Vec<u8>, TrustedGuestControlProbeProtocolError> {
    canonical_json(
        &ProbePayloadWire::from(payload),
        MAX_TRUSTED_GUEST_CONTROL_PROBE_PAYLOAD_BYTES,
    )
}

pub fn decode_trusted_guest_control_probe_payload(
    bytes: &[u8],
) -> Result<TrustedGuestControlProbePayload, TrustedGuestControlProbeProtocolError> {
    require_size(bytes, MAX_TRUSTED_GUEST_CONTROL_PROBE_PAYLOAD_BYTES)?;
    let wire: ProbePayloadWire = serde_json::from_slice(bytes).map_err(|_| malformed())?;
    let payload = payload_from_wire(wire)?;
    if encode_trusted_guest_control_probe_payload(&payload)? != bytes {
        return Err(noncanonical());
    }
    Ok(payload)
}

pub fn trusted_guest_control_probe_payload_digest(
    payload: &TrustedGuestControlProbePayload,
) -> Result<Sha256Digest, TrustedGuestControlProbeProtocolError> {
    digest(
        PAYLOAD_DIGEST_DOMAIN,
        &encode_trusted_guest_control_probe_payload(payload)?,
    )
}

pub fn encode_trusted_guest_control_probe_result(
    result: &TrustedGuestControlProbeResult,
) -> Result<Vec<u8>, TrustedGuestControlProbeProtocolError> {
    canonical_json(
        &ProbeResultWire::from(result),
        MAX_TRUSTED_GUEST_CONTROL_PROBE_RESULT_BYTES,
    )
}

pub fn decode_trusted_guest_control_probe_result(
    bytes: &[u8],
) -> Result<TrustedGuestControlProbeResult, TrustedGuestControlProbeProtocolError> {
    require_size(bytes, MAX_TRUSTED_GUEST_CONTROL_PROBE_RESULT_BYTES)?;
    let wire: ProbeResultWire = serde_json::from_slice(bytes).map_err(|_| malformed())?;
    let result = result_from_wire(wire)?;
    if encode_trusted_guest_control_probe_result(&result)? != bytes {
        return Err(noncanonical());
    }
    Ok(result)
}

pub fn trusted_guest_control_probe_result_digest(
    result: &TrustedGuestControlProbeResult,
) -> Result<Sha256Digest, TrustedGuestControlProbeProtocolError> {
    digest(
        RESULT_DIGEST_DOMAIN,
        &encode_trusted_guest_control_probe_result(result)?,
    )
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProbePayloadWire {
    schema_version: u8,
    project: String,
    resident_sandbox_id: String,
    resident_sandbox_generation: u64,
    authority_digest: String,
    expected_binary_generation: u64,
    protocol_schema_version: u8,
    probe_policy_generation: u64,
}

impl From<&TrustedGuestControlProbePayload> for ProbePayloadWire {
    fn from(value: &TrustedGuestControlProbePayload) -> Self {
        Self {
            schema_version: TRUSTED_GUEST_CONTROL_PROBE_SCHEMA_VERSION,
            project: value.project.as_str().to_owned(),
            resident_sandbox_id: value.sandbox_id.as_str().to_owned(),
            resident_sandbox_generation: value.sandbox_generation.get(),
            authority_digest: value.authority_digest.as_str().to_owned(),
            expected_binary_generation: value.expected_binary_generation,
            protocol_schema_version: value.protocol_schema_version,
            probe_policy_generation: value.probe_policy_generation,
        }
    }
}

fn payload_from_wire(
    wire: ProbePayloadWire,
) -> Result<TrustedGuestControlProbePayload, TrustedGuestControlProbeProtocolError> {
    if wire.schema_version != TRUSTED_GUEST_CONTROL_PROBE_SCHEMA_VERSION
        || wire.protocol_schema_version != TRUSTED_GUEST_CONTROL_PROTOCOL_SCHEMA_VERSION
        || wire.expected_binary_generation == 0
        || wire.probe_policy_generation == 0
    {
        return Err(malformed());
    }
    Ok(TrustedGuestControlProbePayload {
        project: ProjectIdentity::parse(&wire.project).map_err(|_| malformed())?,
        sandbox_id: ResidentSandboxId::parse(&wire.resident_sandbox_id).map_err(|_| malformed())?,
        sandbox_generation: ResidentSandboxGeneration::new(wire.resident_sandbox_generation)
            .map_err(|_| malformed())?,
        authority_digest: Sha256Digest::parse(&wire.authority_digest).map_err(|_| malformed())?,
        expected_binary_generation: wire.expected_binary_generation,
        protocol_schema_version: wire.protocol_schema_version,
        probe_policy_generation: wire.probe_policy_generation,
    })
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProbeResultWire {
    schema_version: u8,
    protocol_schema_version: u8,
    binary_generation: u64,
    effective_root_admitted: bool,
    architecture: TrustedGuestControlArchitecture,
    probe_policy_generation: u64,
}

impl From<&TrustedGuestControlProbeResult> for ProbeResultWire {
    fn from(value: &TrustedGuestControlProbeResult) -> Self {
        Self {
            schema_version: TRUSTED_GUEST_CONTROL_PROBE_SCHEMA_VERSION,
            protocol_schema_version: value.protocol_schema_version,
            binary_generation: value.binary_generation,
            effective_root_admitted: value.effective_root_admitted,
            architecture: value.architecture,
            probe_policy_generation: value.probe_policy_generation,
        }
    }
}

fn result_from_wire(
    wire: ProbeResultWire,
) -> Result<TrustedGuestControlProbeResult, TrustedGuestControlProbeProtocolError> {
    if wire.schema_version != TRUSTED_GUEST_CONTROL_PROBE_SCHEMA_VERSION
        || wire.protocol_schema_version != TRUSTED_GUEST_CONTROL_PROTOCOL_SCHEMA_VERSION
        || wire.binary_generation == 0
        || !wire.effective_root_admitted
        || wire.architecture != TrustedGuestControlArchitecture::LinuxAarch64
        || wire.probe_policy_generation == 0
    {
        return Err(result_mismatch());
    }
    Ok(TrustedGuestControlProbeResult {
        protocol_schema_version: wire.protocol_schema_version,
        binary_generation: wire.binary_generation,
        effective_root_admitted: wire.effective_root_admitted,
        architecture: wire.architecture,
        probe_policy_generation: wire.probe_policy_generation,
    })
}

fn canonical_json(
    value: &impl Serialize,
    limit: usize,
) -> Result<Vec<u8>, TrustedGuestControlProbeProtocolError> {
    let mut bytes = serde_json::to_vec(value).map_err(|_| malformed())?;
    bytes.push(b'\n');
    require_size(&bytes, limit)?;
    Ok(bytes)
}

fn require_size(bytes: &[u8], limit: usize) -> Result<(), TrustedGuestControlProbeProtocolError> {
    if bytes.len() > limit {
        return Err(too_large());
    }
    Ok(())
}

fn digest(
    domain: &[u8],
    bytes: &[u8],
) -> Result<Sha256Digest, TrustedGuestControlProbeProtocolError> {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(bytes);
    let value = hasher.finalize();
    let mut encoded = String::with_capacity(SHA256_PREFIX.len() + value.len() * 2);
    encoded.push_str(SHA256_PREFIX);
    for byte in value {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Sha256Digest::parse(&encoded).map_err(|_| malformed())
}

const fn error(
    kind: TrustedGuestControlProbeProtocolErrorKind,
    code: &'static str,
    message: &'static str,
) -> TrustedGuestControlProbeProtocolError {
    TrustedGuestControlProbeProtocolError {
        kind,
        code,
        message,
    }
}

const fn authority_mismatch() -> TrustedGuestControlProbeProtocolError {
    error(
        TrustedGuestControlProbeProtocolErrorKind::AuthorityMismatch,
        "trusted_guest_control_probe_authority_mismatch",
        "guest-control probe authority does not match the exact resident target",
    )
}

const fn digest_mismatch() -> TrustedGuestControlProbeProtocolError {
    error(
        TrustedGuestControlProbeProtocolErrorKind::DigestMismatch,
        "trusted_guest_control_probe_digest_mismatch",
        "guest-control probe payload digest does not match the common request",
    )
}

const fn result_mismatch() -> TrustedGuestControlProbeProtocolError {
    error(
        TrustedGuestControlProbeProtocolErrorKind::ResultMismatch,
        "trusted_guest_control_probe_result_mismatch",
        "guest-control probe result does not match the exact probe payload",
    )
}

const fn too_large() -> TrustedGuestControlProbeProtocolError {
    error(
        TrustedGuestControlProbeProtocolErrorKind::TooLarge,
        "trusted_guest_control_probe_document_too_large",
        "guest-control probe document exceeds its bounded size",
    )
}

const fn malformed() -> TrustedGuestControlProbeProtocolError {
    error(
        TrustedGuestControlProbeProtocolErrorKind::Malformed,
        "trusted_guest_control_probe_document_malformed",
        "guest-control probe document is malformed",
    )
}

const fn noncanonical() -> TrustedGuestControlProbeProtocolError {
    error(
        TrustedGuestControlProbeProtocolErrorKind::NonCanonical,
        "trusted_guest_control_probe_document_noncanonical",
        "guest-control probe document is noncanonical",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trusted_guest_control_protocol::{
        TrustedGuestControlRequestId, TrustedGuestControlResidentAuthorityGeneration,
        TrustedGuestControlResidentConfigClaim, TrustedGuestControlResidentConfigGeneration,
    };

    fn digest_value(byte: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    fn authority() -> TrustedGuestControlAuthority {
        TrustedGuestControlAuthority::resident_sandbox(
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            ResidentSandboxId::parse("resident-a").unwrap(),
            ResidentSandboxGeneration::new(3).unwrap(),
            TrustedGuestControlResidentConfigGeneration::new(4).unwrap(),
            TrustedGuestControlResidentConfigClaim::new(digest_value('a')),
            TrustedGuestControlResidentAuthorityGeneration::new(5).unwrap(),
        )
    }

    fn binary() -> TrustedGuestControlBinaryBinding {
        TrustedGuestControlBinaryBinding::new(
            7,
            digest_value('b'),
            TrustedGuestControlArchitecture::LinuxAarch64,
        )
        .unwrap()
    }

    #[test]
    fn probe_payload_and_result_are_canonical_and_request_bound() {
        let authority = authority();
        let binary = binary();
        let payload = TrustedGuestControlProbePayload::new(&authority, &binary, 9).unwrap();
        let payload_bytes = encode_trusted_guest_control_probe_payload(&payload).unwrap();
        let decoded = decode_trusted_guest_control_probe_payload(&payload_bytes).unwrap();
        assert_eq!(decoded, payload);
        let request = TrustedGuestControlRequest::new(
            TrustedGuestControlRequestId::parse("probe-1").unwrap(),
            binary,
            authority,
            TrustedGuestControlOperation::ProbeGuestControl,
            trusted_guest_control_probe_payload_digest(&payload).unwrap(),
        )
        .unwrap();
        decoded.confirm_common_request(&request).unwrap();

        let result = TrustedGuestControlProbeResult::from_verified_dispatch(&decoded);
        result.confirm_payload(&decoded).unwrap();
        let result_bytes = encode_trusted_guest_control_probe_result(&result).unwrap();
        let decoded_result = decode_trusted_guest_control_probe_result(&result_bytes).unwrap();
        decoded_result.confirm_payload(&decoded).unwrap();
        assert!(
            trusted_guest_control_probe_result_digest(&decoded_result)
                .unwrap()
                .as_str()
                .starts_with("sha256:")
        );
    }

    #[test]
    fn probe_refuses_noncanonical_unknown_and_false_root_result() {
        let payload = TrustedGuestControlProbePayload::new(&authority(), &binary(), 9).unwrap();
        let canonical = encode_trusted_guest_control_probe_payload(&payload).unwrap();
        let mut spaced = canonical.clone();
        spaced.insert(0, b' ');
        assert_eq!(
            decode_trusted_guest_control_probe_payload(&spaced)
                .unwrap_err()
                .kind(),
            TrustedGuestControlProbeProtocolErrorKind::NonCanonical
        );
        let text = String::from_utf8(canonical).unwrap();
        let unknown = text.trim_end().strip_suffix('}').unwrap().to_owned() + ",\"path\":\"/\"}\n";
        assert_eq!(
            decode_trusted_guest_control_probe_payload(unknown.as_bytes())
                .unwrap_err()
                .kind(),
            TrustedGuestControlProbeProtocolErrorKind::Malformed
        );

        let result = TrustedGuestControlProbeResult::from_verified_dispatch(&payload);
        let invalid =
            String::from_utf8(encode_trusted_guest_control_probe_result(&result).unwrap())
                .unwrap()
                .replace(
                    "\"effective_root_admitted\":true",
                    "\"effective_root_admitted\":false",
                );
        assert_eq!(
            decode_trusted_guest_control_probe_result(invalid.as_bytes())
                .unwrap_err()
                .kind(),
            TrustedGuestControlProbeProtocolErrorKind::ResultMismatch
        );
    }

    #[test]
    fn probe_authority_cannot_construct_project_disk_or_git_operations() {
        let payload = TrustedGuestControlProbePayload::new(&authority(), &binary(), 9).unwrap();
        for operation in [
            TrustedGuestControlOperation::ObservePendingProjectDiskAttachment,
            TrustedGuestControlOperation::ObserveProjectFilesystem,
            TrustedGuestControlOperation::MountProjectFilesystem,
            TrustedGuestControlOperation::ObserveProjectBlockDeviceForFormat,
            TrustedGuestControlOperation::FormatProjectFilesystem,
            TrustedGuestControlOperation::ObserveFormattedProjectFilesystem,
            TrustedGuestControlOperation::ObserveImmutableGitPool,
            TrustedGuestControlOperation::PublishImmutableGitPoolGeneration,
            TrustedGuestControlOperation::PrepareTrustedTaskView,
            TrustedGuestControlOperation::ObserveTrustedTaskView,
            TrustedGuestControlOperation::CleanupTrustedTaskView,
        ] {
            assert_eq!(
                TrustedGuestControlRequest::new(
                    TrustedGuestControlRequestId::parse("probe-1").unwrap(),
                    binary(),
                    authority(),
                    operation,
                    trusted_guest_control_probe_payload_digest(&payload).unwrap(),
                )
                .unwrap_err()
                .kind(),
                crate::trusted_guest_control_protocol::TrustedGuestControlProtocolErrorKind::InvalidAuthority
            );
        }
    }
}
