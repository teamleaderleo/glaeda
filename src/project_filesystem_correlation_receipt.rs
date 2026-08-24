//! Pure bounded validator for the final #628 physical project-filesystem correlation receipt.
//!
//! The receipt is private evidence. Validation emits only a bounded `YES` / `NO` / `AMBIGUOUS`
//! report and never constructs `TrustedProjectFilesystemCorrelationProof`, executes Lima, touches a
//! disk, invokes a guest, or mutates OverlayFS state.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::artifact::Sha256Digest;
use crate::project_catalog::ProjectIdentity;
use crate::project_disk_filesystem::{
    ProjectDiskFilesystemFormatProfileGeneration, ProjectDiskFilesystemGeneration,
};
use crate::project_disk_lease::{
    ProjectDiskAttachmentGeneration, ProjectDiskGeneration, ProjectDiskId, ProjectDiskRevision,
    ResidentSandboxGeneration, ResidentSandboxId,
};

pub const PROJECT_FILESYSTEM_CORRELATION_RECEIPT_SCHEMA_VERSION: u8 = 1;
pub const PROJECT_FILESYSTEM_CORRELATION_RECEIPT_TYPE: &str =
    "smolrunner-project-filesystem-full-correlation-receipt";
const MAX_RECEIPT_BYTES: usize = 128 * 1024;
const MAX_VERSION_BYTES: usize = 64;
const MAX_COMMIT_BYTES: usize = 40;
const MAX_UUID_BYTES: usize = 36;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ProjectFilesystemCorrelationVerdict {
    Yes,
    No,
    Ambiguous,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectFilesystemCorrelationPublicTuple {
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    disk_revision: ProjectDiskRevision,
    attachment_generation: ProjectDiskAttachmentGeneration,
    sandbox_id: ResidentSandboxId,
    sandbox_generation: ResidentSandboxGeneration,
    filesystem_generation: ProjectDiskFilesystemGeneration,
    format_profile_generation: ProjectDiskFilesystemFormatProfileGeneration,
    filesystem_kind: ProjectFilesystemReceiptKind,
}

impl ProjectFilesystemCorrelationPublicTuple {
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
    pub const fn filesystem_kind(&self) -> ProjectFilesystemReceiptKind {
        self.filesystem_kind
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProjectFilesystemCorrelationChecks {
    physical_provenance_matches: bool,
    durable_tuple_stable: bool,
    descriptor_rebind_passed: bool,
    resident_attachment_bound: bool,
    filesystem_metadata_matches: bool,
    guest_transaction_bound: bool,
    whole_block_device_matches: bool,
    role_filesystem_device_matches: bool,
}

impl ProjectFilesystemCorrelationChecks {
    #[must_use]
    pub const fn physical_provenance_matches(self) -> bool {
        self.physical_provenance_matches
    }

    #[must_use]
    pub const fn durable_tuple_stable(self) -> bool {
        self.durable_tuple_stable
    }

    #[must_use]
    pub const fn descriptor_rebind_passed(self) -> bool {
        self.descriptor_rebind_passed
    }

    #[must_use]
    pub const fn resident_attachment_bound(self) -> bool {
        self.resident_attachment_bound
    }

    #[must_use]
    pub const fn filesystem_metadata_matches(self) -> bool {
        self.filesystem_metadata_matches
    }

    #[must_use]
    pub const fn guest_transaction_bound(self) -> bool {
        self.guest_transaction_bound
    }

    #[must_use]
    pub const fn whole_block_device_matches(self) -> bool {
        self.whole_block_device_matches
    }

    #[must_use]
    pub const fn role_filesystem_device_matches(self) -> bool {
        self.role_filesystem_device_matches
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectFilesystemCorrelationReceiptReport {
    schema_version: u8,
    verdict: ProjectFilesystemCorrelationVerdict,
    current: ProjectFilesystemCorrelationPublicTuple,
    checks: ProjectFilesystemCorrelationChecks,
}

impl ProjectFilesystemCorrelationReceiptReport {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn verdict(&self) -> ProjectFilesystemCorrelationVerdict {
        self.verdict
    }

    #[must_use]
    pub const fn current(&self) -> &ProjectFilesystemCorrelationPublicTuple {
        &self.current
    }

    #[must_use]
    pub const fn checks(&self) -> ProjectFilesystemCorrelationChecks {
        self.checks
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectFilesystemReceiptKind {
    Ext4,
    Xfs,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawReceipt {
    schema_version: u8,
    receipt_type: String,
    capture_commit: String,
    lima_version: String,
    before: RawTuple,
    after: RawTuple,
    p3_physical_identity_digest: String,
    p3_backing_identity_digest: String,
    p2_physical_identity_digest: String,
    p2_backing_identity_digest: String,
    descriptor_rebind_passed: bool,
    resident_host_identity_bound: bool,
    lima_attachment_agrees: bool,
    host_guest_block_bridge_bound: bool,
    filesystem: RawFilesystemIdentity,
    guest_transaction: RawGuestTransaction,
    guest_filesystem: RawGuestFilesystem,
    guest_block_device: RawGuestBlockDevice,
    role_filesystem_device: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTuple {
    project: String,
    disk_id: String,
    disk_generation: u64,
    disk_revision: u64,
    attachment_generation: u64,
    sandbox_id: String,
    sandbox_generation: u64,
    filesystem_generation: u64,
    format_profile_generation: u64,
    filesystem_kind: ProjectFilesystemReceiptKind,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFilesystemIdentity {
    filesystem_uuid: String,
    feature_policy_digest: String,
    logical_bytes: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGuestTransaction {
    authority: RawTuple,
    exact_invocation_target_bound: bool,
    limactl_generation: u64,
    limactl_digest: String,
    guest_binary_generation: u64,
    guest_binary_digest: String,
    protocol_generation: u64,
    request_digest: String,
    result_digest: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGuestFilesystem {
    filesystem_kind: ProjectFilesystemReceiptKind,
    filesystem_uuid: String,
    feature_policy_digest: String,
    logical_bytes: u64,
    stat_device: u64,
    mountinfo_major: u64,
    mountinfo_minor: u64,
    mount_root_identity_bound: bool,
    read_write: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGuestBlockDevice {
    rdev: u64,
    inode: u64,
    whole_device: bool,
}

/// Parse and validate one private full-correlation receipt.
///
/// A conclusive identity/device mismatch returns a `NO` report. Missing freshness/binding evidence
/// returns `AMBIGUOUS`. Exact agreement across every required link returns `YES`.
///
/// # Errors
///
/// Returns a bounded parse/schema/canonical-input error when the private receipt itself is malformed
/// or exceeds the accepted bound.
pub fn validate_project_filesystem_correlation_receipt_json(
    bytes: &[u8],
) -> Result<ProjectFilesystemCorrelationReceiptReport, ProjectFilesystemCorrelationReceiptError> {
    if bytes.is_empty() || bytes.len() > MAX_RECEIPT_BYTES {
        return Err(invalid_receipt());
    }
    let raw: RawReceipt = serde_json::from_slice(bytes).map_err(|_| malformed_receipt())?;
    if raw.schema_version != PROJECT_FILESYSTEM_CORRELATION_RECEIPT_SCHEMA_VERSION
        || raw.receipt_type != PROJECT_FILESYSTEM_CORRELATION_RECEIPT_TYPE
    {
        return Err(unsupported_schema());
    }
    validate_commit(&raw.capture_commit)?;
    validate_version(&raw.lima_version)?;

    let current = parse_tuple(&raw.before)?;
    let _after = parse_tuple(&raw.after)?;
    let _guest_authority = parse_tuple(&raw.guest_transaction.authority)?;

    validate_uuid(&raw.filesystem.filesystem_uuid)?;
    validate_uuid(&raw.guest_filesystem.filesystem_uuid)?;
    validate_digest(&raw.filesystem.feature_policy_digest)?;
    validate_digest(&raw.guest_filesystem.feature_policy_digest)?;
    for digest in [
        &raw.p3_physical_identity_digest,
        &raw.p3_backing_identity_digest,
        &raw.p2_physical_identity_digest,
        &raw.p2_backing_identity_digest,
        &raw.guest_transaction.limactl_digest,
        &raw.guest_transaction.guest_binary_digest,
        &raw.guest_transaction.request_digest,
        &raw.guest_transaction.result_digest,
    ] {
        validate_digest(digest)?;
    }
    if raw.filesystem.logical_bytes == 0
        || raw.guest_filesystem.logical_bytes == 0
        || raw.guest_transaction.limactl_generation == 0
        || raw.guest_transaction.guest_binary_generation == 0
        || raw.guest_transaction.protocol_generation == 0
    {
        return Err(invalid_receipt());
    }

    let physical_provenance_matches = raw.p3_physical_identity_digest
        == raw.p2_physical_identity_digest
        && raw.p3_backing_identity_digest == raw.p2_backing_identity_digest;
    let durable_tuple_stable = raw.before == raw.after;
    let guest_authority_matches = raw.before == raw.guest_transaction.authority;
    let filesystem_metadata_matches = raw.before.filesystem_kind
        == raw.guest_filesystem.filesystem_kind
        && raw.filesystem.filesystem_uuid == raw.guest_filesystem.filesystem_uuid
        && raw.filesystem.feature_policy_digest == raw.guest_filesystem.feature_policy_digest
        && raw.filesystem.logical_bytes == raw.guest_filesystem.logical_bytes;

    let (decoded_major, decoded_minor) = linux_decode_dev(raw.guest_filesystem.stat_device);
    let mountinfo_device_matches = decoded_major == raw.guest_filesystem.mountinfo_major
        && decoded_minor == raw.guest_filesystem.mountinfo_minor;
    let whole_block_device_matches = raw.guest_block_device.whole_device
        && raw.guest_block_device.inode != 0
        && raw.guest_block_device.rdev == raw.guest_filesystem.stat_device
        && mountinfo_device_matches;
    let role_filesystem_device_matches =
        raw.role_filesystem_device == raw.guest_filesystem.stat_device;

    let resident_attachment_bound = raw.resident_host_identity_bound
        && raw.lima_attachment_agrees
        && raw.host_guest_block_bridge_bound;
    let guest_transaction_bound = raw.guest_transaction.exact_invocation_target_bound
        && guest_authority_matches
        && raw.guest_filesystem.mount_root_identity_bound
        && raw.guest_filesystem.read_write;

    let checks = ProjectFilesystemCorrelationChecks {
        physical_provenance_matches,
        durable_tuple_stable,
        descriptor_rebind_passed: raw.descriptor_rebind_passed,
        resident_attachment_bound,
        filesystem_metadata_matches,
        guest_transaction_bound,
        whole_block_device_matches,
        role_filesystem_device_matches,
    };

    let conclusive_mismatch = !physical_provenance_matches
        || !guest_authority_matches
        || !filesystem_metadata_matches
        || !whole_block_device_matches
        || !role_filesystem_device_matches;
    let freshness_missing = !durable_tuple_stable
        || !raw.descriptor_rebind_passed
        || !resident_attachment_bound
        || !raw.guest_transaction.exact_invocation_target_bound
        || !raw.guest_filesystem.mount_root_identity_bound
        || !raw.guest_filesystem.read_write;

    let verdict = if freshness_missing {
        ProjectFilesystemCorrelationVerdict::Ambiguous
    } else if conclusive_mismatch {
        ProjectFilesystemCorrelationVerdict::No
    } else {
        ProjectFilesystemCorrelationVerdict::Yes
    };

    Ok(ProjectFilesystemCorrelationReceiptReport {
        schema_version: PROJECT_FILESYSTEM_CORRELATION_RECEIPT_SCHEMA_VERSION,
        verdict,
        current,
        checks,
    })
}

fn parse_tuple(
    raw: &RawTuple,
) -> Result<ProjectFilesystemCorrelationPublicTuple, ProjectFilesystemCorrelationReceiptError> {
    Ok(ProjectFilesystemCorrelationPublicTuple {
        project: ProjectIdentity::parse(&raw.project).map_err(|_| invalid_receipt())?,
        disk_id: ProjectDiskId::parse(&raw.disk_id).map_err(|_| invalid_receipt())?,
        disk_generation: ProjectDiskGeneration::new(raw.disk_generation)
            .map_err(|_| invalid_receipt())?,
        disk_revision: ProjectDiskRevision::new(raw.disk_revision)
            .map_err(|_| invalid_receipt())?,
        attachment_generation: ProjectDiskAttachmentGeneration::new(raw.attachment_generation)
            .map_err(|_| invalid_receipt())?,
        sandbox_id: ResidentSandboxId::parse(&raw.sandbox_id).map_err(|_| invalid_receipt())?,
        sandbox_generation: ResidentSandboxGeneration::new(raw.sandbox_generation)
            .map_err(|_| invalid_receipt())?,
        filesystem_generation: ProjectDiskFilesystemGeneration::new(raw.filesystem_generation)
            .map_err(|_| invalid_receipt())?,
        format_profile_generation: ProjectDiskFilesystemFormatProfileGeneration::new(
            raw.format_profile_generation,
        )
        .map_err(|_| invalid_receipt())?,
        filesystem_kind: raw.filesystem_kind,
    })
}

fn validate_commit(value: &str) -> Result<(), ProjectFilesystemCorrelationReceiptError> {
    if value.len() != MAX_COMMIT_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid_receipt());
    }
    Ok(())
}

fn validate_version(value: &str) -> Result<(), ProjectFilesystemCorrelationReceiptError> {
    if value.is_empty()
        || value.len() > MAX_VERSION_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+' | b'_'))
    {
        return Err(invalid_receipt());
    }
    Ok(())
}

fn validate_uuid(value: &str) -> Result<(), ProjectFilesystemCorrelationReceiptError> {
    if value.len() != MAX_UUID_BYTES {
        return Err(invalid_receipt());
    }
    for (index, byte) in value.bytes().enumerate() {
        match index {
            8 | 13 | 18 | 23 if byte == b'-' => {}
            8 | 13 | 18 | 23 => return Err(invalid_receipt()),
            _ if byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte) => {}
            _ => return Err(invalid_receipt()),
        }
    }
    Ok(())
}

fn validate_digest(value: &str) -> Result<(), ProjectFilesystemCorrelationReceiptError> {
    let parsed = Sha256Digest::parse(value).map_err(|_| invalid_receipt())?;
    if parsed.as_str() == "sha256:0000000000000000000000000000000000000000000000000000000000000000"
    {
        return Err(invalid_receipt());
    }
    Ok(())
}

/// Decode Linux `dev_t` using the kernel/glibc major/minor layout.
fn linux_decode_dev(device: u64) -> (u64, u64) {
    let major = ((device & 0x0000_0000_000f_ff00) >> 8) | ((device & 0xffff_f000_0000_0000) >> 32);
    let minor = (device & 0x0000_0000_0000_00ff) | ((device & 0x0000_0fff_fff0_0000) >> 12);
    (major, minor)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectFilesystemCorrelationReceiptErrorKind {
    Malformed,
    UnsupportedSchema,
    InvalidReceipt,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProjectFilesystemCorrelationReceiptError {
    kind: ProjectFilesystemCorrelationReceiptErrorKind,
    code: &'static str,
    message: &'static str,
}

impl ProjectFilesystemCorrelationReceiptError {
    #[must_use]
    pub const fn kind(self) -> ProjectFilesystemCorrelationReceiptErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for ProjectFilesystemCorrelationReceiptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectFilesystemCorrelationReceiptError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for ProjectFilesystemCorrelationReceiptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ProjectFilesystemCorrelationReceiptError {}

const fn error(
    kind: ProjectFilesystemCorrelationReceiptErrorKind,
    code: &'static str,
    message: &'static str,
) -> ProjectFilesystemCorrelationReceiptError {
    ProjectFilesystemCorrelationReceiptError {
        kind,
        code,
        message,
    }
}

const fn malformed_receipt() -> ProjectFilesystemCorrelationReceiptError {
    error(
        ProjectFilesystemCorrelationReceiptErrorKind::Malformed,
        "project_filesystem_correlation_receipt_malformed",
        "project filesystem correlation receipt is malformed",
    )
}

const fn unsupported_schema() -> ProjectFilesystemCorrelationReceiptError {
    error(
        ProjectFilesystemCorrelationReceiptErrorKind::UnsupportedSchema,
        "project_filesystem_correlation_receipt_schema_unsupported",
        "project filesystem correlation receipt schema is unsupported",
    )
}

const fn invalid_receipt() -> ProjectFilesystemCorrelationReceiptError {
    error(
        ProjectFilesystemCorrelationReceiptErrorKind::InvalidReceipt,
        "project_filesystem_correlation_receipt_invalid",
        "project filesystem correlation receipt is invalid",
    )
}

#[cfg(test)]
mod tests {
    use super::{
        PROJECT_FILESYSTEM_CORRELATION_RECEIPT_TYPE, ProjectFilesystemCorrelationVerdict,
        linux_decode_dev, validate_project_filesystem_correlation_receipt_json,
    };
    use serde_json::json;

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn tuple() -> serde_json::Value {
        json!({
            "project": "github.com/teamleaderleo/smolrunner",
            "disk_id": "disk-a",
            "disk_generation": 3,
            "disk_revision": 7,
            "attachment_generation": 2,
            "sandbox_id": "sandbox-a",
            "sandbox_generation": 11,
            "filesystem_generation": 5,
            "format_profile_generation": 2,
            "filesystem_kind": "ext4"
        })
    }

    fn exact_receipt() -> serde_json::Value {
        json!({
            "schema_version": 1,
            "receipt_type": PROJECT_FILESYSTEM_CORRELATION_RECEIPT_TYPE,
            "capture_commit": "0123456789abcdef0123456789abcdef01234567",
            "lima_version": "2.2.0",
            "before": tuple(),
            "after": tuple(),
            "p3_physical_identity_digest": digest('a'),
            "p3_backing_identity_digest": digest('b'),
            "p2_physical_identity_digest": digest('a'),
            "p2_backing_identity_digest": digest('b'),
            "descriptor_rebind_passed": true,
            "resident_host_identity_bound": true,
            "lima_attachment_agrees": true,
            "host_guest_block_bridge_bound": true,
            "filesystem": {
                "filesystem_uuid": "01234567-89ab-cdef-0123-456789abcdef",
                "feature_policy_digest": digest('c'),
                "logical_bytes": 1073741824_u64
            },
            "guest_transaction": {
                "authority": tuple(),
                "exact_invocation_target_bound": true,
                "limactl_generation": 4,
                "limactl_digest": digest('d'),
                "guest_binary_generation": 5,
                "guest_binary_digest": digest('e'),
                "protocol_generation": 1,
                "request_digest": digest('f'),
                "result_digest": digest('1')
            },
            "guest_filesystem": {
                "filesystem_kind": "ext4",
                "filesystem_uuid": "01234567-89ab-cdef-0123-456789abcdef",
                "feature_policy_digest": digest('c'),
                "logical_bytes": 1073741824_u64,
                "stat_device": 2049_u64,
                "mountinfo_major": 8,
                "mountinfo_minor": 1,
                "mount_root_identity_bound": true,
                "read_write": true
            },
            "guest_block_device": {
                "rdev": 2049_u64,
                "inode": 77_u64,
                "whole_device": true
            },
            "role_filesystem_device": 2049_u64
        })
    }

    fn validate(value: serde_json::Value) -> super::ProjectFilesystemCorrelationReceiptReport {
        validate_project_filesystem_correlation_receipt_json(&serde_json::to_vec(&value).unwrap())
            .unwrap()
    }

    #[test]
    fn exact_full_receipt_is_yes() {
        let report = validate(exact_receipt());
        assert_eq!(report.verdict(), ProjectFilesystemCorrelationVerdict::Yes);
        assert!(report.checks().physical_provenance_matches());
        assert!(report.checks().whole_block_device_matches());
        assert!(report.checks().role_filesystem_device_matches());
        assert_eq!(report.current().disk_revision().get(), 7);
        assert_eq!(report.current().filesystem_generation().get(), 5);
    }

    #[test]
    fn physical_or_device_identity_mismatch_is_no() {
        let mut receipt = exact_receipt();
        receipt["p2_physical_identity_digest"] = json!(digest('9'));
        assert_eq!(
            validate(receipt).verdict(),
            ProjectFilesystemCorrelationVerdict::No
        );

        let mut receipt = exact_receipt();
        receipt["role_filesystem_device"] = json!(2050_u64);
        assert_eq!(
            validate(receipt).verdict(),
            ProjectFilesystemCorrelationVerdict::No
        );
    }

    #[test]
    fn durable_or_descriptor_drift_is_ambiguous() {
        let mut receipt = exact_receipt();
        receipt["after"]["disk_revision"] = json!(8_u64);
        assert_eq!(
            validate(receipt).verdict(),
            ProjectFilesystemCorrelationVerdict::Ambiguous
        );

        let mut receipt = exact_receipt();
        receipt["descriptor_rebind_passed"] = json!(false);
        assert_eq!(
            validate(receipt).verdict(),
            ProjectFilesystemCorrelationVerdict::Ambiguous
        );
    }

    #[test]
    fn wrong_guest_transaction_authority_is_no() {
        let mut receipt = exact_receipt();
        receipt["guest_transaction"]["authority"]["attachment_generation"] = json!(3_u64);
        assert_eq!(
            validate(receipt).verdict(),
            ProjectFilesystemCorrelationVerdict::No
        );
    }

    #[test]
    fn missing_resident_or_invocation_freshness_is_ambiguous() {
        let mut receipt = exact_receipt();
        receipt["resident_host_identity_bound"] = json!(false);
        assert_eq!(
            validate(receipt).verdict(),
            ProjectFilesystemCorrelationVerdict::Ambiguous
        );

        let mut receipt = exact_receipt();
        receipt["guest_transaction"]["exact_invocation_target_bound"] = json!(false);
        assert_eq!(
            validate(receipt).verdict(),
            ProjectFilesystemCorrelationVerdict::Ambiguous
        );
    }

    #[test]
    fn linux_dev_decoder_matches_expected_major_minor() {
        assert_eq!(linux_decode_dev(2049), (8, 1));
    }

    #[test]
    fn unknown_fields_and_noncanonical_uuid_are_rejected() {
        let mut receipt = exact_receipt();
        receipt["unexpected"] = json!(true);
        assert!(
            validate_project_filesystem_correlation_receipt_json(
                &serde_json::to_vec(&receipt).unwrap()
            )
            .is_err()
        );

        let mut receipt = exact_receipt();
        receipt["filesystem"]["filesystem_uuid"] = json!("01234567-89AB-cdef-0123-456789abcdef");
        assert!(
            validate_project_filesystem_correlation_receipt_json(
                &serde_json::to_vec(&receipt).unwrap()
            )
            .is_err()
        );
    }
}
