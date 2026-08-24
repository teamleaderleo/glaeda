use std::fmt;

use serde::{Deserialize, Serialize};

mod validate;

use validate::validate_document;
#[cfg(target_os = "macos")]
pub(crate) use validate::{
    same_entry_binding, valid_git_commit, validate_absolute_path, validate_locator,
};
pub const PROJECT_DISK_PHYSICAL_RECEIPT_SCHEMA_VERSION: u8 = 1;
pub const MAX_PROJECT_DISK_PHYSICAL_RECEIPT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_PROJECT_DISK_RECEIPT_ENTRY_COUNT: usize = 64;
pub const MAX_PROJECT_DISK_RECEIPT_SMALL_FILE_BYTES: usize = 64 * 1024;
pub(crate) const MAX_PATH_BYTES: usize = 4_096;
const MAX_ENTRY_NAME_BYTES: usize = 255;
const BLOCK_ALLOCATION_UNIT_BYTES: u64 = 512;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectDiskPhysicalReceiptDocument {
    pub(crate) schema_version: u8,
    pub(crate) repo_commit: String,
    pub(crate) captured_at_unix_millis: u64,
    pub(crate) declared_binding: DeclaredProjectDiskBindingDocument,
    pub(crate) lima: ProjectDiskLimaEvidenceDocument,
    pub(crate) disk_directory: ProjectDiskDirectoryEvidenceDocument,
    pub(crate) guest: ProjectDiskGuestEvidenceDocument,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclaredProjectDiskBindingDocument {
    pub(crate) project_identity: String,
    pub(crate) project_disk_id: String,
    pub(crate) project_disk_generation: u64,
    pub(crate) project_disk_revision: u64,
    pub(crate) attachment_generation: u64,
    pub(crate) resident_sandbox_id: String,
    pub(crate) resident_sandbox_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectDiskLimaEvidenceDocument {
    pub(crate) lima_home: String,
    pub(crate) disk_name: String,
    pub(crate) resident_sandbox_instance: String,
    pub(crate) host_identity_digest: String,
    pub(crate) limactl_version: ReceiptCommandEvidence,
    pub(crate) disk_list_json: ReceiptCommandEvidence,
    pub(crate) instance_list_json: ReceiptCommandEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectDiskDirectoryEvidenceDocument {
    pub(crate) path: String,
    pub(crate) before: ReceiptFilesystemSnapshot,
    pub(crate) entries: Vec<ReceiptDirectoryEntryEvidence>,
    pub(crate) after_entry_names_hex: Vec<String>,
    pub(crate) after: ReceiptFilesystemSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptDirectoryEntryEvidence {
    pub(crate) name_hex: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) name_utf8: Option<String>,
    pub(crate) kind: ReceiptDirectoryEntryKind,
    pub(crate) before: ReceiptFilesystemSnapshot,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) symlink_target_hex: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) small_regular_file_hex: Option<String>,
    pub(crate) after: ReceiptFilesystemSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptDirectoryEntryKind {
    Regular,
    Directory,
    Symlink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptFilesystemSnapshot {
    pub(crate) device: u64,
    pub(crate) inode: u64,
    pub(crate) uid: u32,
    pub(crate) gid: u32,
    pub(crate) mode: u32,
    pub(crate) links: u64,
    pub(crate) logical_bytes: u64,
    pub(crate) allocated_bytes: u64,
    pub(crate) mtime_seconds: i64,
    pub(crate) mtime_nanoseconds: i64,
    pub(crate) ctime_seconds: i64,
    pub(crate) ctime_nanoseconds: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectDiskGuestEvidenceDocument {
    pub(crate) project_mount: String,
    pub(crate) mountinfo: ReceiptCommandEvidence,
    pub(crate) block_devices_json: ReceiptCommandEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptCommandEvidence {
    pub(crate) argv: Vec<String>,
    pub(crate) environment_keys: Vec<String>,
    pub(crate) status: Option<i32>,
    pub(crate) success: bool,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDiskPhysicalReceipt {
    pub(crate) document: ProjectDiskPhysicalReceiptDocument,
    pub(crate) guest_filesystem_device: GuestFilesystemDeviceObservation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestFilesystemDeviceObservation {
    pub(crate) major: u32,
    pub(crate) minor: u32,
    pub(crate) filesystem_type: String,
    pub(crate) source: String,
}

impl ProjectDiskPhysicalReceipt {
    /// Decode one private observation-only physical receipt.
    ///
    /// This receipt carries no attach, unlock, format, resize, delete, cleanup, or trusted-mount
    /// authority and exposes no project-filesystem correlation-proof constructor.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for malformed, drifted, failed, or ambiguous observation evidence.
    pub fn decode_private_json(bytes: &[u8]) -> Result<Self, ProjectDiskPhysicalReceiptError> {
        if bytes.len() > MAX_PROJECT_DISK_PHYSICAL_RECEIPT_BYTES {
            return Err(error(
                ProjectDiskPhysicalReceiptErrorKind::TooLarge,
                "document",
                "project_disk_receipt_too_large",
                "project disk physical receipt exceeds the bounded size",
            ));
        }
        let document: ProjectDiskPhysicalReceiptDocument = serde_json::from_slice(bytes)
            .map_err(|_| malformed("document", "project_disk_receipt_malformed"))?;
        validate_document(document)
    }

    pub fn encode_private_json_pretty(&self) -> Result<Vec<u8>, ProjectDiskPhysicalReceiptError> {
        serde_json::to_vec_pretty(&self.document)
            .map_err(|_| malformed("document", "project_disk_receipt_encode_failed"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDiskPhysicalReceiptErrorKind {
    TooLarge,
    Malformed,
    UnsupportedSchema,
    InvalidField,
    ChangedDuringObservation,
    DuplicateEntry,
    CommandFailed,
    InvalidJsonEvidence,
    AmbiguousGuestMount,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiskPhysicalReceiptError {
    kind: ProjectDiskPhysicalReceiptErrorKind,
    field: &'static str,
    code: &'static str,
    message: &'static str,
}

impl ProjectDiskPhysicalReceiptError {
    #[must_use]
    pub const fn kind(&self) -> ProjectDiskPhysicalReceiptErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn field(&self) -> &'static str {
        self.field
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for ProjectDiskPhysicalReceiptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectDiskPhysicalReceiptError")
            .field("kind", &self.kind)
            .field("field", &self.field)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for ProjectDiskPhysicalReceiptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ProjectDiskPhysicalReceiptError {}

const fn error(
    kind: ProjectDiskPhysicalReceiptErrorKind,
    field: &'static str,
    code: &'static str,
    message: &'static str,
) -> ProjectDiskPhysicalReceiptError {
    ProjectDiskPhysicalReceiptError {
        kind,
        field,
        code,
        message,
    }
}

const fn malformed(field: &'static str, code: &'static str) -> ProjectDiskPhysicalReceiptError {
    error(
        ProjectDiskPhysicalReceiptErrorKind::Malformed,
        field,
        code,
        "project disk physical receipt evidence is malformed",
    )
}

const fn invalid_field(field: &'static str) -> ProjectDiskPhysicalReceiptError {
    error(
        ProjectDiskPhysicalReceiptErrorKind::InvalidField,
        field,
        "project_disk_receipt_field_invalid",
        "project disk physical receipt field is outside the reviewed contract",
    )
}

const fn changed(field: &'static str) -> ProjectDiskPhysicalReceiptError {
    error(
        ProjectDiskPhysicalReceiptErrorKind::ChangedDuringObservation,
        field,
        "project_disk_receipt_changed_during_observation",
        "project disk physical receipt evidence changed during observation",
    )
}

#[cfg(test)]
mod tests;
