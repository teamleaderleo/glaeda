//! Opaque composition of the exact guest project-filesystem and whole-block-device observations.
//!
//! This closes the guest-local evidence chain without minting any host/project ownership proof:
//!
//! `whole block st_rdev == project mount-root st_dev == exact mountinfo major:minor`.
//!
//! The value is short-lived, non-serializable, and non-cloneable. It performs no I/O, process
//! execution, format, mount mutation, Lima operation, or #589 proof construction.

use std::fmt;

use serde::Serialize;

use crate::project_disk_filesystem::{
    ProjectDiskFilesystemFormatProfileGeneration, ProjectDiskFilesystemGeneration,
    ProjectDiskFilesystemKind,
};
use crate::trusted_project_block_device_correlation::TrustedProjectBlockDeviceCorrelation;
use crate::trusted_project_filesystem_guest_observation::TrustedProjectFilesystemGuestObservation;

pub const TRUSTED_PROJECT_FILESYSTEM_FULL_GUEST_CORRELATION_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrustedProjectFilesystemFullGuestCorrelationSummary {
    schema_version: u8,
    filesystem_generation: ProjectDiskFilesystemGeneration,
    format_profile_generation: ProjectDiskFilesystemFormatProfileGeneration,
    filesystem_kind: ProjectDiskFilesystemKind,
    mount_id: u64,
    mount_root_identity_bound: bool,
    mountinfo_device_bound: bool,
    whole_block_device_bound: bool,
}

impl TrustedProjectFilesystemFullGuestCorrelationSummary {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
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
    pub const fn mount_id(&self) -> u64 {
        self.mount_id
    }

    #[must_use]
    pub const fn mount_root_identity_bound(&self) -> bool {
        self.mount_root_identity_bound
    }

    #[must_use]
    pub const fn mountinfo_device_bound(&self) -> bool {
        self.mountinfo_device_bound
    }

    #[must_use]
    pub const fn whole_block_device_bound(&self) -> bool {
        self.whole_block_device_bound
    }
}

/// Exact guest-local correlation for one accepted filesystem generation.
///
/// Raw mount-root device/inode and block-device rdev/inode remain inside the consumed component
/// observations. Public output exposes only the bounded summary.
pub struct TrustedProjectFilesystemFullGuestCorrelation {
    summary: TrustedProjectFilesystemFullGuestCorrelationSummary,
    filesystem: TrustedProjectFilesystemGuestObservation,
    block_device: TrustedProjectBlockDeviceCorrelation,
}

impl TrustedProjectFilesystemFullGuestCorrelation {
    #[must_use]
    pub const fn summary(&self) -> &TrustedProjectFilesystemFullGuestCorrelationSummary {
        &self.summary
    }

    /// Revalidate one exact reopened mount-root descriptor against the original guest observation.
    #[must_use]
    pub const fn matches_mount_root_identity(
        &self,
        observed_filesystem_device: u64,
        observed_filesystem_inode: u64,
    ) -> bool {
        self.filesystem
            .matches_mount_root_identity(observed_filesystem_device, observed_filesystem_inode)
    }

    /// Require an exact later role descriptor (for example #589 lower/upper/work) to live on the
    /// same whole device as the mounted project filesystem.
    #[must_use]
    pub const fn matches_role_filesystem_device(&self, observed_filesystem_device: u64) -> bool {
        self.block_device
            .matches_filesystem_device(observed_filesystem_device)
    }

    /// Revalidate the exact whole block-device node identity from a fresh held descriptor.
    #[must_use]
    pub const fn matches_block_device_identity(
        &self,
        observed_rdev: u64,
        observed_inode: u64,
    ) -> bool {
        self.block_device
            .matches_block_device_identity(observed_rdev, observed_inode)
    }
}

impl fmt::Debug for TrustedProjectFilesystemFullGuestCorrelation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedProjectFilesystemFullGuestCorrelation")
            .field("summary", &self.summary)
            .field("filesystem_private_evidence", &"<redacted>")
            .field("block_device_private_evidence", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustedProjectFilesystemFullGuestCorrelationErrorKind {
    FilesystemMismatch,
    IncompleteEvidence,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TrustedProjectFilesystemFullGuestCorrelationError {
    kind: TrustedProjectFilesystemFullGuestCorrelationErrorKind,
    code: &'static str,
    message: &'static str,
}

impl TrustedProjectFilesystemFullGuestCorrelationError {
    #[must_use]
    pub const fn kind(self) -> TrustedProjectFilesystemFullGuestCorrelationErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for TrustedProjectFilesystemFullGuestCorrelationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedProjectFilesystemFullGuestCorrelationError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for TrustedProjectFilesystemFullGuestCorrelationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for TrustedProjectFilesystemFullGuestCorrelationError {}

/// Consume the two independently validated guest observations into one exact correlation value.
///
/// # Errors
///
/// Returns a bounded refusal unless both observations name the same filesystem generation/profile,
/// kind and mount ID and both report all required binding predicates.
pub fn correlate_trusted_project_filesystem_guest(
    filesystem: TrustedProjectFilesystemGuestObservation,
    block_device: TrustedProjectBlockDeviceCorrelation,
) -> Result<
    TrustedProjectFilesystemFullGuestCorrelation,
    TrustedProjectFilesystemFullGuestCorrelationError,
> {
    let filesystem_summary = filesystem.summary();
    let block_summary = block_device.summary();
    if filesystem_summary.filesystem_generation() != block_summary.filesystem_generation()
        || filesystem_summary.format_profile_generation()
            != block_summary.format_profile_generation()
        || filesystem_summary.filesystem_kind() != block_summary.filesystem_kind()
        || filesystem_summary.mount_id() != block_summary.mount_id()
    {
        return Err(filesystem_mismatch());
    }
    if !filesystem_summary.filesystem_device_bound()
        || !filesystem_summary.mount_root_identity_bound()
        || !filesystem_summary.read_write()
        || !block_summary.block_device_bound()
        || !block_summary.whole_device()
        || !block_summary.filesystem_device_bound()
    {
        return Err(incomplete_evidence());
    }
    let summary = TrustedProjectFilesystemFullGuestCorrelationSummary {
        schema_version: TRUSTED_PROJECT_FILESYSTEM_FULL_GUEST_CORRELATION_SCHEMA_VERSION,
        filesystem_generation: filesystem_summary.filesystem_generation(),
        format_profile_generation: filesystem_summary.format_profile_generation(),
        filesystem_kind: filesystem_summary.filesystem_kind(),
        mount_id: filesystem_summary.mount_id(),
        mount_root_identity_bound: true,
        mountinfo_device_bound: true,
        whole_block_device_bound: true,
    };
    Ok(TrustedProjectFilesystemFullGuestCorrelation {
        summary,
        filesystem,
        block_device,
    })
}

const fn error(
    kind: TrustedProjectFilesystemFullGuestCorrelationErrorKind,
    code: &'static str,
    message: &'static str,
) -> TrustedProjectFilesystemFullGuestCorrelationError {
    TrustedProjectFilesystemFullGuestCorrelationError {
        kind,
        code,
        message,
    }
}

const fn filesystem_mismatch() -> TrustedProjectFilesystemFullGuestCorrelationError {
    error(
        TrustedProjectFilesystemFullGuestCorrelationErrorKind::FilesystemMismatch,
        "project_filesystem_full_guest_mismatch",
        "guest filesystem and block-device observations name different filesystem evidence",
    )
}

const fn incomplete_evidence() -> TrustedProjectFilesystemFullGuestCorrelationError {
    error(
        TrustedProjectFilesystemFullGuestCorrelationErrorKind::IncompleteEvidence,
        "project_filesystem_full_guest_incomplete",
        "guest project-filesystem correlation evidence is incomplete",
    )
}

#[cfg(test)]
mod tests {
    use super::{
        TrustedProjectFilesystemFullGuestCorrelationErrorKind,
        correlate_trusted_project_filesystem_guest,
    };
    use crate::project_catalog::ProjectIdentity;
    use crate::project_disk_filesystem::{
        ProjectDiskFilesystemBinding, ProjectDiskFilesystemFormatProfileGeneration,
        ProjectDiskFilesystemGeneration, ProjectDiskFilesystemKind,
    };
    use crate::project_disk_lease::{ProjectDiskGeneration, ProjectDiskId};
    use crate::trusted_project_block_device_correlation::correlate_trusted_project_block_device;
    use crate::trusted_project_filesystem_guest_observation::observe_trusted_project_filesystem_guest;

    fn filesystem(generation: u64) -> ProjectDiskFilesystemBinding {
        ProjectDiskFilesystemBinding::new_for_project_disk(
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            ProjectDiskId::parse("disk-a").unwrap(),
            ProjectDiskGeneration::new(3).unwrap(),
            ProjectDiskFilesystemGeneration::new(generation).unwrap(),
            ProjectDiskFilesystemFormatProfileGeneration::new(2).unwrap(),
            ProjectDiskFilesystemKind::Ext4,
        )
    }

    fn mounted(
        generation: u64,
    ) -> crate::trusted_project_filesystem_guest_observation::TrustedProjectFilesystemGuestObservation
    {
        observe_trusted_project_filesystem_guest(
            &filesystem(generation),
            2049,
            99,
            b"/srv/project",
            b"123 45 8:1 / /srv/project rw - ext4 /dev/vda rw\n",
        )
        .unwrap()
    }

    #[test]
    fn exact_guest_chain_is_consumed_into_one_opaque_value() {
        let filesystem = mounted(7);
        let block =
            correlate_trusted_project_block_device(&filesystem, 2049, 77, true, true).unwrap();
        let full = correlate_trusted_project_filesystem_guest(filesystem, block).unwrap();
        assert_eq!(full.summary().filesystem_generation().get(), 7);
        assert_eq!(full.summary().mount_id(), 123);
        assert!(full.summary().mount_root_identity_bound());
        assert!(full.summary().mountinfo_device_bound());
        assert!(full.summary().whole_block_device_bound());
        assert!(full.matches_mount_root_identity(2049, 99));
        assert!(full.matches_role_filesystem_device(2049));
        assert!(full.matches_block_device_identity(2049, 77));
    }

    #[test]
    fn component_filesystem_generation_mismatch_is_rejected() {
        let first = mounted(7);
        let second = mounted(8);
        let block = correlate_trusted_project_block_device(&second, 2049, 77, true, true).unwrap();
        let error = correlate_trusted_project_filesystem_guest(first, block).unwrap_err();
        assert_eq!(
            error.kind(),
            TrustedProjectFilesystemFullGuestCorrelationErrorKind::FilesystemMismatch
        );
    }

    #[test]
    fn debug_exposes_only_bounded_summary() {
        let filesystem = mounted(7);
        let block =
            correlate_trusted_project_block_device(&filesystem, 2049, 77, true, true).unwrap();
        let full = correlate_trusted_project_filesystem_guest(filesystem, block).unwrap();
        let debug = format!("{full:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("2049"));
        assert!(!debug.contains("99"));
        assert!(!debug.contains("77"));
    }
}
