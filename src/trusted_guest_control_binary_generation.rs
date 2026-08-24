//! Exact declaration identity for the resident Linux guest-control executable generation.
//!
//! This module is pure. It performs no build, filesystem I/O, publication, privilege escalation,
//! guest invocation, or project mutation. A generation is reviewed input for #629; surviving bytes
//! gain no authority until the Linux observer independently proves the installed executable.

use std::fmt;

use serde::Serialize;

use crate::artifact::{CommitId, Sha256Digest};
use crate::trusted_guest_control_protocol::{
    TRUSTED_GUEST_CONTROL_PROTOCOL_SCHEMA_VERSION, TrustedGuestControlArchitecture,
    TrustedGuestControlBinaryBinding,
};

pub const TRUSTED_GUEST_CONTROL_BINARY_GENERATION_SCHEMA_VERSION: u8 = 1;
pub const MAX_TRUSTED_GUEST_CONTROL_BINARY_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustedGuestControlInstallSlot {
    ProtectedLibexecV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct TrustedGuestControlToolchainGeneration(u64);

impl TrustedGuestControlToolchainGeneration {
    /// Create one positive reviewed guest build-toolchain generation.
    ///
    /// # Errors
    ///
    /// Returns a bounded error when `value` is zero.
    pub fn new(value: u64) -> Result<Self, TrustedGuestControlBinaryGenerationError> {
        if value == 0 {
            return Err(error(
                "invalid_guest_control_toolchain_generation",
                "guest-control toolchain generation must be positive",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Reviewed expected identity for one immutable guest-control executable generation.
///
/// This is declaration data only. It can supply the protocol-level binary binding because that
/// binding is also declaration data; neither type proves that an installed path contains these
/// bytes. #629's descriptor-bound observer must make that physical claim independently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrustedGuestControlBinaryGeneration {
    schema_version: u8,
    source_commit: CommitId,
    artifact_size_bytes: u64,
    toolchain_generation: TrustedGuestControlToolchainGeneration,
    toolchain_digest: Sha256Digest,
    protocol_schema_version: u8,
    install_slot: TrustedGuestControlInstallSlot,
    binding: TrustedGuestControlBinaryBinding,
}

impl TrustedGuestControlBinaryGeneration {
    /// Declare one exact reviewed Linux/aarch64 guest-control executable generation.
    ///
    /// The artifact digest is used directly by the canonical protocol binary binding. The fixed
    /// v1 install slot is identity-bearing; a later publication/observer slice owns the concrete
    /// protected guest path for that slot.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for a zero binary generation, a zero toolchain generation, or an
    /// empty/oversized executable artifact.
    pub fn new(
        generation: u64,
        source_commit: CommitId,
        artifact_digest: Sha256Digest,
        artifact_size_bytes: u64,
        toolchain_generation: TrustedGuestControlToolchainGeneration,
        toolchain_digest: Sha256Digest,
    ) -> Result<Self, TrustedGuestControlBinaryGenerationError> {
        if artifact_size_bytes == 0 || artifact_size_bytes > MAX_TRUSTED_GUEST_CONTROL_BINARY_BYTES
        {
            return Err(error(
                "invalid_guest_control_artifact_size",
                "guest-control executable size is outside the reviewed bound",
            ));
        }
        let binding = TrustedGuestControlBinaryBinding::new(
            generation,
            artifact_digest,
            TrustedGuestControlArchitecture::LinuxAarch64,
        )
        .map_err(|_| {
            error(
                "invalid_guest_control_binary_generation",
                "guest-control binary generation must be positive",
            )
        })?;
        Ok(Self {
            schema_version: TRUSTED_GUEST_CONTROL_BINARY_GENERATION_SCHEMA_VERSION,
            source_commit,
            artifact_size_bytes,
            toolchain_generation,
            toolchain_digest,
            protocol_schema_version: TRUSTED_GUEST_CONTROL_PROTOCOL_SCHEMA_VERSION,
            install_slot: TrustedGuestControlInstallSlot::ProtectedLibexecV1,
            binding,
        })
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn source_commit(&self) -> &CommitId {
        &self.source_commit
    }

    #[must_use]
    pub const fn artifact_size_bytes(&self) -> u64 {
        self.artifact_size_bytes
    }

    #[must_use]
    pub const fn toolchain_generation(&self) -> TrustedGuestControlToolchainGeneration {
        self.toolchain_generation
    }

    #[must_use]
    pub const fn toolchain_digest(&self) -> &Sha256Digest {
        &self.toolchain_digest
    }

    #[must_use]
    pub const fn protocol_schema_version(&self) -> u8 {
        self.protocol_schema_version
    }

    #[must_use]
    pub const fn install_slot(&self) -> TrustedGuestControlInstallSlot {
        self.install_slot
    }

    #[must_use]
    pub const fn binding(&self) -> &TrustedGuestControlBinaryBinding {
        &self.binding
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct TrustedGuestControlBinaryGenerationError {
    pub code: &'static str,
    pub message: &'static str,
}

impl fmt::Debug for TrustedGuestControlBinaryGenerationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedGuestControlBinaryGenerationError")
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for TrustedGuestControlBinaryGenerationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for TrustedGuestControlBinaryGenerationError {}

const fn error(
    code: &'static str,
    message: &'static str,
) -> TrustedGuestControlBinaryGenerationError {
    TrustedGuestControlBinaryGenerationError { code, message }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    const ARTIFACT: &str =
        "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const TOOLCHAIN: &str =
        "sha256:89abcdef0123456789abcdef0123456789abcdef0123456789abcdef01234567";

    fn generation() -> TrustedGuestControlBinaryGeneration {
        TrustedGuestControlBinaryGeneration::new(
            7,
            CommitId::parse(COMMIT).unwrap(),
            Sha256Digest::parse(ARTIFACT).unwrap(),
            12_345_678,
            TrustedGuestControlToolchainGeneration::new(3).unwrap(),
            Sha256Digest::parse(TOOLCHAIN).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn generation_binds_protocol_binary_source_toolchain_and_slot() {
        let value = generation();
        assert_eq!(value.schema_version(), 1);
        assert_eq!(value.source_commit().as_str(), COMMIT);
        assert_eq!(value.artifact_size_bytes(), 12_345_678);
        assert_eq!(value.toolchain_generation().get(), 3);
        assert_eq!(value.toolchain_digest().as_str(), TOOLCHAIN);
        assert_eq!(
            value.protocol_schema_version(),
            TRUSTED_GUEST_CONTROL_PROTOCOL_SCHEMA_VERSION
        );
        assert_eq!(
            value.install_slot(),
            TrustedGuestControlInstallSlot::ProtectedLibexecV1
        );
        assert_eq!(value.binding().generation(), 7);
        assert_eq!(value.binding().digest().as_str(), ARTIFACT);
        assert_eq!(
            value.binding().architecture(),
            TrustedGuestControlArchitecture::LinuxAarch64
        );
    }

    #[test]
    fn invalid_generations_and_artifact_sizes_fail_closed() {
        assert!(TrustedGuestControlToolchainGeneration::new(0).is_err());
        for size in [0, MAX_TRUSTED_GUEST_CONTROL_BINARY_BYTES + 1] {
            let result = TrustedGuestControlBinaryGeneration::new(
                7,
                CommitId::parse(COMMIT).unwrap(),
                Sha256Digest::parse(ARTIFACT).unwrap(),
                size,
                TrustedGuestControlToolchainGeneration::new(3).unwrap(),
                Sha256Digest::parse(TOOLCHAIN).unwrap(),
            );
            assert!(result.is_err());
        }
        let zero_generation = TrustedGuestControlBinaryGeneration::new(
            0,
            CommitId::parse(COMMIT).unwrap(),
            Sha256Digest::parse(ARTIFACT).unwrap(),
            1,
            TrustedGuestControlToolchainGeneration::new(3).unwrap(),
            Sha256Digest::parse(TOOLCHAIN).unwrap(),
        )
        .unwrap_err();
        assert_eq!(
            zero_generation.code,
            "invalid_guest_control_binary_generation"
        );
    }

    #[test]
    fn serialized_declaration_exposes_slot_without_path_fields() {
        let encoded = serde_json::to_string(&generation()).unwrap();
        assert!(encoded.contains("protected_libexec_v1"));
        assert!(!encoded.contains("\"path\""));
        assert!(!encoded.contains("install_parent"));
        assert!(!encoded.contains("install_name"));
    }
}
