//! Pure immutable pre-format identity for one project-disk filesystem generation.
//!
//! P4 chooses this complete plan before any formatter sandbox attach or `mkfs` request. The plan is
//! declaration data only: it performs no persistence, process execution, device probing, format,
//! mount, or proof minting.

use std::fmt;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use super::{
    ProjectDiskFilesystemBinding, ProjectDiskFilesystemFormatProfileGeneration,
    ProjectDiskFilesystemGeneration, ProjectDiskFilesystemKind,
};
use crate::artifact::Sha256Digest;
use crate::project_catalog::ProjectIdentity;
use crate::project_disk_lease::{ProjectDiskGeneration, ProjectDiskId};

pub const PROJECT_DISK_FILESYSTEM_FORMAT_PLAN_SCHEMA_VERSION: u8 = 1;
const FORMAT_PLAN_DOMAIN: &[u8] = b"smolrunner-project-disk-filesystem-format-plan-v1\0";
const SHA256_PREFIX: &str = "sha256:";
const HEX: &[u8; 16] = b"0123456789abcdef";
const MAX_FORMATTER_VERSION_BYTES: usize = 64;

macro_rules! positive_generation_type {
    ($name:ident, $code:literal, $message:literal) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(u64);

        impl $name {
            /// Construct one positive generation.
            ///
            /// # Errors
            ///
            /// Returns a bounded error when `value` is zero.
            pub fn new(value: u64) -> Result<Self, ProjectDiskFilesystemFormatPlanError> {
                if value == 0 {
                    return Err(error(
                        ProjectDiskFilesystemFormatPlanErrorKind::InvalidGeneration,
                        $code,
                        $message,
                    ));
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

positive_generation_type!(
    ProjectDiskFormatterBinaryGeneration,
    "project_disk_formatter_binary_generation_invalid",
    "project disk formatter binary generation must be greater than zero"
);
positive_generation_type!(
    ProjectDiskFormatterGuestGeneration,
    "project_disk_formatter_guest_generation_invalid",
    "project disk formatter guest generation must be greater than zero"
);
positive_generation_type!(
    ProjectDiskFormatDurabilityPolicyGeneration,
    "project_disk_format_durability_policy_generation_invalid",
    "project disk format durability-policy generation must be greater than zero"
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDiskFormatterArchitecture {
    LinuxAarch64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ProjectDiskFormatterVersion(String);

impl ProjectDiskFormatterVersion {
    /// Parse one bounded canonical formatter version token.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal unless the token is lowercase/uppercase ASCII alphanumeric text
    /// plus `.`, `_`, `+`, or `-`, with no whitespace or path syntax.
    pub fn parse(value: &str) -> Result<Self, ProjectDiskFilesystemFormatPlanError> {
        if value.is_empty()
            || value.len() > MAX_FORMATTER_VERSION_BYTES
            || !value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-')
            })
        {
            return Err(invalid_input());
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiskFormatterBinaryBinding {
    generation: ProjectDiskFormatterBinaryGeneration,
    digest: Sha256Digest,
    version: ProjectDiskFormatterVersion,
    architecture: ProjectDiskFormatterArchitecture,
}

impl ProjectDiskFormatterBinaryBinding {
    #[must_use]
    pub const fn new(
        generation: ProjectDiskFormatterBinaryGeneration,
        digest: Sha256Digest,
        version: ProjectDiskFormatterVersion,
        architecture: ProjectDiskFormatterArchitecture,
    ) -> Self {
        Self {
            generation,
            digest,
            version,
            architecture,
        }
    }

    #[must_use]
    pub const fn generation(&self) -> ProjectDiskFormatterBinaryGeneration {
        self.generation
    }

    #[must_use]
    pub const fn digest(&self) -> &Sha256Digest {
        &self.digest
    }

    #[must_use]
    pub const fn version(&self) -> &ProjectDiskFormatterVersion {
        &self.version
    }

    #[must_use]
    pub const fn architecture(&self) -> ProjectDiskFormatterArchitecture {
        self.architecture
    }
}

/// Canonical controller-chosen filesystem UUID for one format generation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ProjectDiskFilesystemUuid(String);

impl ProjectDiskFilesystemUuid {
    /// Parse one canonical lowercase RFC-4122 textual UUID.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal for uppercase, malformed, or nil UUIDs.
    pub fn parse(value: &str) -> Result<Self, ProjectDiskFilesystemFormatPlanError> {
        let bytes = value.as_bytes();
        if bytes.len() != 36
            || !bytes.iter().enumerate().all(|(index, byte)| match index {
                8 | 13 | 18 | 23 => *byte == b'-',
                _ => byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'),
            })
            || bytes
                .iter()
                .filter(|byte| **byte != b'-')
                .all(|byte| *byte == b'0')
        {
            return Err(invalid_input());
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Complete immutable format identity selected before the destructive formatter boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiskFilesystemFormatPlan {
    schema_version: u8,
    filesystem: ProjectDiskFilesystemBinding,
    filesystem_uuid: ProjectDiskFilesystemUuid,
    whole_device: bool,
    expected_logical_bytes: u64,
    feature_policy_digest: Sha256Digest,
    formatter: ProjectDiskFormatterBinaryBinding,
    formatter_config_digest: Sha256Digest,
    formatter_guest_generation: ProjectDiskFormatterGuestGeneration,
    durability_policy_generation: ProjectDiskFormatDurabilityPolicyGeneration,
    plan_digest: Sha256Digest,
}

impl ProjectDiskFilesystemFormatPlan {
    /// Seal one complete format plan before any formatter attachment or `mkfs` request.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal when the expected whole-device logical size is zero or when the
    /// canonical plan digest cannot be represented.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        filesystem: ProjectDiskFilesystemBinding,
        filesystem_uuid: ProjectDiskFilesystemUuid,
        expected_logical_bytes: u64,
        feature_policy_digest: Sha256Digest,
        formatter: ProjectDiskFormatterBinaryBinding,
        formatter_config_digest: Sha256Digest,
        formatter_guest_generation: ProjectDiskFormatterGuestGeneration,
        durability_policy_generation: ProjectDiskFormatDurabilityPolicyGeneration,
    ) -> Result<Self, ProjectDiskFilesystemFormatPlanError> {
        if expected_logical_bytes == 0 {
            return Err(invalid_input());
        }
        let plan_digest = derive_plan_digest(
            &filesystem,
            &filesystem_uuid,
            expected_logical_bytes,
            &feature_policy_digest,
            &formatter,
            &formatter_config_digest,
            formatter_guest_generation,
            durability_policy_generation,
        )?;
        Ok(Self {
            schema_version: PROJECT_DISK_FILESYSTEM_FORMAT_PLAN_SCHEMA_VERSION,
            filesystem,
            filesystem_uuid,
            whole_device: true,
            expected_logical_bytes,
            feature_policy_digest,
            formatter,
            formatter_config_digest,
            formatter_guest_generation,
            durability_policy_generation,
            plan_digest,
        })
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn filesystem(&self) -> &ProjectDiskFilesystemBinding {
        &self.filesystem
    }

    #[must_use]
    pub const fn filesystem_uuid(&self) -> &ProjectDiskFilesystemUuid {
        &self.filesystem_uuid
    }

    #[must_use]
    pub const fn whole_device(&self) -> bool {
        self.whole_device
    }

    #[must_use]
    pub const fn expected_logical_bytes(&self) -> u64 {
        self.expected_logical_bytes
    }

    #[must_use]
    pub const fn feature_policy_digest(&self) -> &Sha256Digest {
        &self.feature_policy_digest
    }

    #[must_use]
    pub const fn formatter(&self) -> &ProjectDiskFormatterBinaryBinding {
        &self.formatter
    }

    #[must_use]
    pub const fn formatter_config_digest(&self) -> &Sha256Digest {
        &self.formatter_config_digest
    }

    #[must_use]
    pub const fn formatter_guest_generation(&self) -> ProjectDiskFormatterGuestGeneration {
        self.formatter_guest_generation
    }

    #[must_use]
    pub const fn durability_policy_generation(
        &self,
    ) -> ProjectDiskFormatDurabilityPolicyGeneration {
        self.durability_policy_generation
    }

    #[must_use]
    pub const fn plan_digest(&self) -> &Sha256Digest {
        &self.plan_digest
    }
}

fn derive_plan_digest(
    filesystem: &ProjectDiskFilesystemBinding,
    filesystem_uuid: &ProjectDiskFilesystemUuid,
    expected_logical_bytes: u64,
    feature_policy_digest: &Sha256Digest,
    formatter: &ProjectDiskFormatterBinaryBinding,
    formatter_config_digest: &Sha256Digest,
    formatter_guest_generation: ProjectDiskFormatterGuestGeneration,
    durability_policy_generation: ProjectDiskFormatDurabilityPolicyGeneration,
) -> Result<Sha256Digest, ProjectDiskFilesystemFormatPlanError> {
    let kind = match filesystem.kind() {
        ProjectDiskFilesystemKind::Ext4 => b"ext4".as_slice(),
        ProjectDiskFilesystemKind::Xfs => b"xfs".as_slice(),
    };
    let architecture = match formatter.architecture() {
        ProjectDiskFormatterArchitecture::LinuxAarch64 => b"linux_aarch64".as_slice(),
    };
    let fields: [&[u8]; 18] = [
        filesystem.project().as_str().as_bytes(),
        filesystem.disk_id().as_str().as_bytes(),
        &filesystem.disk_generation().get().to_be_bytes(),
        &filesystem.filesystem_generation().get().to_be_bytes(),
        &filesystem.format_profile_generation().get().to_be_bytes(),
        kind,
        filesystem_uuid.as_str().as_bytes(),
        b"whole_device=true",
        &expected_logical_bytes.to_be_bytes(),
        feature_policy_digest.as_str().as_bytes(),
        &formatter.generation().get().to_be_bytes(),
        formatter.digest().as_str().as_bytes(),
        formatter.version().as_str().as_bytes(),
        architecture,
        formatter_config_digest.as_str().as_bytes(),
        &formatter_guest_generation.get().to_be_bytes(),
        &durability_policy_generation.get().to_be_bytes(),
        b"format_plan_v1",
    ];
    let mut hasher = Sha256::new();
    hasher.update(FORMAT_PLAN_DOMAIN);
    for field in fields {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field);
    }
    digest_to_sha256(&hasher.finalize())
}

fn digest_to_sha256(bytes: &[u8]) -> Result<Sha256Digest, ProjectDiskFilesystemFormatPlanError> {
    let mut value = String::with_capacity(SHA256_PREFIX.len() + bytes.len() * 2);
    value.push_str(SHA256_PREFIX);
    for byte in bytes {
        value.push(char::from(HEX[usize::from(byte >> 4)]));
        value.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Sha256Digest::parse(&value).map_err(|_| invalid_input())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDiskFilesystemFormatPlanErrorKind {
    InvalidGeneration,
    InvalidInput,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProjectDiskFilesystemFormatPlanError {
    kind: ProjectDiskFilesystemFormatPlanErrorKind,
    code: &'static str,
    message: &'static str,
}

impl ProjectDiskFilesystemFormatPlanError {
    #[must_use]
    pub const fn kind(self) -> ProjectDiskFilesystemFormatPlanErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for ProjectDiskFilesystemFormatPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectDiskFilesystemFormatPlanError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for ProjectDiskFilesystemFormatPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ProjectDiskFilesystemFormatPlanError {}

const fn error(
    kind: ProjectDiskFilesystemFormatPlanErrorKind,
    code: &'static str,
    message: &'static str,
) -> ProjectDiskFilesystemFormatPlanError {
    ProjectDiskFilesystemFormatPlanError {
        kind,
        code,
        message,
    }
}

const fn invalid_input() -> ProjectDiskFilesystemFormatPlanError {
    error(
        ProjectDiskFilesystemFormatPlanErrorKind::InvalidInput,
        "project_disk_filesystem_format_plan_invalid_input",
        "project disk filesystem format-plan input is invalid",
    )
}

#[cfg(test)]
mod tests {
    use super::{
        ProjectDiskFilesystemFormatPlan, ProjectDiskFilesystemUuid,
        ProjectDiskFormatDurabilityPolicyGeneration, ProjectDiskFormatterArchitecture,
        ProjectDiskFormatterBinaryBinding, ProjectDiskFormatterBinaryGeneration,
        ProjectDiskFormatterGuestGeneration, ProjectDiskFormatterVersion,
    };
    use crate::artifact::Sha256Digest;
    use crate::project_catalog::ProjectIdentity;
    use crate::project_disk_filesystem::{
        ProjectDiskFilesystemBinding, ProjectDiskFilesystemFormatProfileGeneration,
        ProjectDiskFilesystemGeneration, ProjectDiskFilesystemKind,
    };
    use crate::project_disk_lease::{ProjectDiskGeneration, ProjectDiskId};

    fn digest(byte: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    fn filesystem(generation: u64) -> ProjectDiskFilesystemBinding {
        ProjectDiskFilesystemBinding::new_for_project_disk(
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            ProjectDiskId::parse("disk-a").unwrap(),
            ProjectDiskGeneration::new(3).unwrap(),
            ProjectDiskFilesystemGeneration::new(generation).unwrap(),
            ProjectDiskFilesystemFormatProfileGeneration::new(2).unwrap(),
            ProjectDiskFilesystemKind::Xfs,
        )
    }

    fn formatter() -> ProjectDiskFormatterBinaryBinding {
        ProjectDiskFormatterBinaryBinding::new(
            ProjectDiskFormatterBinaryGeneration::new(4).unwrap(),
            digest('a'),
            ProjectDiskFormatterVersion::parse("6.10.1").unwrap(),
            ProjectDiskFormatterArchitecture::LinuxAarch64,
        )
    }

    fn plan(
        filesystem_generation: u64,
        uuid: &str,
        bytes: u64,
        feature: char,
    ) -> ProjectDiskFilesystemFormatPlan {
        ProjectDiskFilesystemFormatPlan::new(
            filesystem(filesystem_generation),
            ProjectDiskFilesystemUuid::parse(uuid).unwrap(),
            bytes,
            digest(feature),
            formatter(),
            digest('b'),
            ProjectDiskFormatterGuestGeneration::new(5).unwrap(),
            ProjectDiskFormatDurabilityPolicyGeneration::new(3).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn canonical_uuid_is_required() {
        assert!(ProjectDiskFilesystemUuid::parse("01234567-89ab-cdef-0123-456789abcdef").is_ok());
        assert!(ProjectDiskFilesystemUuid::parse("01234567-89AB-cdef-0123-456789abcdef").is_err());
        assert!(ProjectDiskFilesystemUuid::parse("00000000-0000-0000-0000-000000000000").is_err());
        assert!(ProjectDiskFilesystemUuid::parse("bad").is_err());
    }

    #[test]
    fn formatter_version_is_bounded_and_path_free() {
        assert!(ProjectDiskFormatterVersion::parse("1.47.2").is_ok());
        assert!(ProjectDiskFormatterVersion::parse("xfsprogs-6.10.1").is_ok());
        assert!(ProjectDiskFormatterVersion::parse("6.10.1 /tmp/tool").is_err());
    }

    #[test]
    fn complete_plan_is_sealed_before_format() {
        let plan = plan(
            7,
            "01234567-89ab-cdef-0123-456789abcdef",
            1_073_741_824,
            'c',
        );
        assert!(plan.whole_device());
        assert_eq!(plan.filesystem().filesystem_generation().get(), 7);
        assert_eq!(plan.filesystem().kind(), ProjectDiskFilesystemKind::Xfs);
        assert_eq!(plan.expected_logical_bytes(), 1_073_741_824);
        assert_eq!(plan.formatter().generation().get(), 4);
        assert_eq!(plan.formatter_guest_generation().get(), 5);
        assert_eq!(plan.durability_policy_generation().get(), 3);
        assert!(plan.plan_digest().as_str().starts_with("sha256:"));
    }

    #[test]
    fn every_material_format_parent_changes_plan_identity() {
        let first = plan(
            7,
            "01234567-89ab-cdef-0123-456789abcdef",
            1_073_741_824,
            'c',
        );
        let next_generation = plan(
            8,
            "01234567-89ab-cdef-0123-456789abcdef",
            1_073_741_824,
            'c',
        );
        let next_uuid = plan(
            7,
            "01234567-89ab-cdef-0123-456789abcdee",
            1_073_741_824,
            'c',
        );
        let next_size = plan(
            7,
            "01234567-89ab-cdef-0123-456789abcdef",
            2_147_483_648,
            'c',
        );
        let next_features = plan(
            7,
            "01234567-89ab-cdef-0123-456789abcdef",
            1_073_741_824,
            'd',
        );
        assert_ne!(first.plan_digest(), next_generation.plan_digest());
        assert_ne!(first.plan_digest(), next_uuid.plan_digest());
        assert_ne!(first.plan_digest(), next_size.plan_digest());
        assert_ne!(first.plan_digest(), next_features.plan_digest());
    }

    #[test]
    fn zero_expected_bytes_are_rejected() {
        assert!(
            ProjectDiskFilesystemFormatPlan::new(
                filesystem(7),
                ProjectDiskFilesystemUuid::parse("01234567-89ab-cdef-0123-456789abcdef").unwrap(),
                0,
                digest('c'),
                formatter(),
                digest('b'),
                ProjectDiskFormatterGuestGeneration::new(5).unwrap(),
                ProjectDiskFormatDurabilityPolicyGeneration::new(3).unwrap(),
            )
            .is_err()
        );
    }

    #[test]
    fn same_inputs_produce_same_canonical_plan_digest() {
        let first = plan(
            7,
            "01234567-89ab-cdef-0123-456789abcdef",
            1_073_741_824,
            'c',
        );
        let second = plan(
            7,
            "01234567-89ab-cdef-0123-456789abcdef",
            1_073_741_824,
            'c',
        );
        assert_eq!(first.plan_digest(), second.plan_digest());
    }
}
