//! Pure guest correlation of one exact whole block device with the mounted project filesystem.
//!
//! This adds the remaining #628/#645 guest-side equality: the held whole block-device node's
//! `st_rdev` must be the same Linux device as the already-validated project mount root `st_dev` and
//! its exact mountinfo major/minor row. It performs no filesystem I/O itself, process execution,
//! format, mount mutation, Lima operation, or proof minting.

use std::fmt;

use serde::Serialize;

use crate::project_disk_filesystem::{
    ProjectDiskFilesystemFormatProfileGeneration, ProjectDiskFilesystemGeneration,
    ProjectDiskFilesystemKind,
};
use crate::trusted_project_filesystem_guest_observation::TrustedProjectFilesystemGuestObservation;

pub const TRUSTED_PROJECT_BLOCK_DEVICE_CORRELATION_SCHEMA_VERSION: u8 = 1;
const REDACTED_DEVICE: &str = "<private-linux-block-device>";
const REDACTED_INODE: &str = "<private-linux-block-device-inode>";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrustedProjectBlockDeviceCorrelationSummary {
    schema_version: u8,
    filesystem_generation: ProjectDiskFilesystemGeneration,
    format_profile_generation: ProjectDiskFilesystemFormatProfileGeneration,
    filesystem_kind: ProjectDiskFilesystemKind,
    mount_id: u64,
    block_device_bound: bool,
    whole_device: bool,
    filesystem_device_bound: bool,
}

impl TrustedProjectBlockDeviceCorrelationSummary {
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
    pub const fn block_device_bound(&self) -> bool {
        self.block_device_bound
    }

    #[must_use]
    pub const fn whole_device(&self) -> bool {
        self.whole_device
    }

    #[must_use]
    pub const fn filesystem_device_bound(&self) -> bool {
        self.filesystem_device_bound
    }
}

/// Opaque guest-local correlation between one held whole block-device node and the exact mounted
/// project filesystem device.
///
/// Raw Linux `st_rdev` and block-node inode remain private and are neither serialized nor exposed
/// through Debug. The block-node inode exists only to detect same-name device-node replacement on a
/// later reopen; filesystem role directories compare by device only.
pub struct TrustedProjectBlockDeviceCorrelation {
    summary: TrustedProjectBlockDeviceCorrelationSummary,
    block_device_rdev: u64,
    block_device_inode: u64,
}

impl TrustedProjectBlockDeviceCorrelation {
    #[must_use]
    pub const fn summary(&self) -> &TrustedProjectBlockDeviceCorrelationSummary {
        &self.summary
    }

    /// Revalidate the exact block-node identity captured by this correlation.
    #[must_use]
    pub const fn matches_block_device_identity(
        &self,
        observed_rdev: u64,
        observed_inode: u64,
    ) -> bool {
        observed_inode != 0
            && self.block_device_rdev == observed_rdev
            && self.block_device_inode == observed_inode
    }

    /// Compare another held filesystem role descriptor against the correlated whole device.
    #[must_use]
    pub const fn matches_filesystem_device(&self, observed_filesystem_device: u64) -> bool {
        self.block_device_rdev == observed_filesystem_device
    }
}

impl fmt::Debug for TrustedProjectBlockDeviceCorrelation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedProjectBlockDeviceCorrelation")
            .field("summary", &self.summary)
            .field("block_device_rdev", &REDACTED_DEVICE)
            .field("block_device_inode", &REDACTED_INODE)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustedProjectBlockDeviceCorrelationErrorKind {
    InvalidBlockDevice,
    DeviceMismatch,
    FilesystemObservationIncomplete,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TrustedProjectBlockDeviceCorrelationError {
    kind: TrustedProjectBlockDeviceCorrelationErrorKind,
    code: &'static str,
    message: &'static str,
}

impl TrustedProjectBlockDeviceCorrelationError {
    #[must_use]
    pub const fn kind(self) -> TrustedProjectBlockDeviceCorrelationErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for TrustedProjectBlockDeviceCorrelationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedProjectBlockDeviceCorrelationError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for TrustedProjectBlockDeviceCorrelationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for TrustedProjectBlockDeviceCorrelationError {}

/// Bind one freshly held whole block-device stat observation to an already-successful exact
/// project-filesystem mount observation.
///
/// `observed_rdev` is the block node's `st_rdev`, while the filesystem observation was produced from
/// the project mount root's `st_dev` and exact mountinfo major/minor row. Equality therefore closes:
///
/// ```text
/// block-device st_rdev == project-root st_dev == exact mountinfo major:minor
/// ```
///
/// # Errors
///
/// Returns a bounded refusal unless the held node is a whole block device with a nonzero inode and
/// its `st_rdev` equals the exact mounted project filesystem device.
pub fn correlate_trusted_project_block_device(
    filesystem: &TrustedProjectFilesystemGuestObservation,
    observed_rdev: u64,
    observed_inode: u64,
    observed_is_block_device: bool,
    observed_whole_device: bool,
) -> Result<TrustedProjectBlockDeviceCorrelation, TrustedProjectBlockDeviceCorrelationError> {
    if observed_inode == 0 || !observed_is_block_device || !observed_whole_device {
        return Err(invalid_block_device());
    }
    let filesystem_summary = filesystem.summary();
    if !filesystem_summary.filesystem_device_bound()
        || !filesystem_summary.mount_root_identity_bound()
        || !filesystem_summary.read_write()
    {
        return Err(filesystem_observation_incomplete());
    }
    if !filesystem.matches_filesystem_device(observed_rdev) {
        return Err(device_mismatch());
    }
    Ok(TrustedProjectBlockDeviceCorrelation {
        summary: TrustedProjectBlockDeviceCorrelationSummary {
            schema_version: TRUSTED_PROJECT_BLOCK_DEVICE_CORRELATION_SCHEMA_VERSION,
            filesystem_generation: filesystem_summary.filesystem_generation(),
            format_profile_generation: filesystem_summary.format_profile_generation(),
            filesystem_kind: filesystem_summary.filesystem_kind(),
            mount_id: filesystem_summary.mount_id(),
            block_device_bound: true,
            whole_device: true,
            filesystem_device_bound: true,
        },
        block_device_rdev: observed_rdev,
        block_device_inode: observed_inode,
    })
}

const fn error(
    kind: TrustedProjectBlockDeviceCorrelationErrorKind,
    code: &'static str,
    message: &'static str,
) -> TrustedProjectBlockDeviceCorrelationError {
    TrustedProjectBlockDeviceCorrelationError {
        kind,
        code,
        message,
    }
}

const fn invalid_block_device() -> TrustedProjectBlockDeviceCorrelationError {
    error(
        TrustedProjectBlockDeviceCorrelationErrorKind::InvalidBlockDevice,
        "project_block_device_invalid",
        "project block-device evidence is invalid",
    )
}

const fn device_mismatch() -> TrustedProjectBlockDeviceCorrelationError {
    error(
        TrustedProjectBlockDeviceCorrelationErrorKind::DeviceMismatch,
        "project_block_device_filesystem_mismatch",
        "project block device disagrees with the mounted project filesystem device",
    )
}

const fn filesystem_observation_incomplete() -> TrustedProjectBlockDeviceCorrelationError {
    error(
        TrustedProjectBlockDeviceCorrelationErrorKind::FilesystemObservationIncomplete,
        "project_block_device_filesystem_observation_incomplete",
        "project filesystem observation is incomplete for block-device correlation",
    )
}

#[cfg(test)]
mod tests {
    use super::{
        REDACTED_DEVICE, REDACTED_INODE, TrustedProjectBlockDeviceCorrelationErrorKind,
        correlate_trusted_project_block_device,
    };
    use crate::project_catalog::ProjectIdentity;
    use crate::project_disk_filesystem::{
        ProjectDiskFilesystemBinding, ProjectDiskFilesystemFormatProfileGeneration,
        ProjectDiskFilesystemGeneration, ProjectDiskFilesystemKind,
    };
    use crate::project_disk_lease::{ProjectDiskGeneration, ProjectDiskId};
    use crate::trusted_project_filesystem_guest_observation::observe_trusted_project_filesystem_guest;

    fn filesystem() -> ProjectDiskFilesystemBinding {
        ProjectDiskFilesystemBinding::new_for_project_disk(
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            ProjectDiskId::parse("disk-a").unwrap(),
            ProjectDiskGeneration::new(3).unwrap(),
            ProjectDiskFilesystemGeneration::new(7).unwrap(),
            ProjectDiskFilesystemFormatProfileGeneration::new(2).unwrap(),
            ProjectDiskFilesystemKind::Ext4,
        )
    }

    fn mounted() -> crate::trusted_project_filesystem_guest_observation::TrustedProjectFilesystemGuestObservation {
        observe_trusted_project_filesystem_guest(
            &filesystem(),
            2049,
            99,
            b"/srv/project",
            b"123 45 8:1 / /srv/project rw - ext4 /dev/vda rw\n",
        )
        .unwrap()
    }

    #[test]
    fn exact_whole_block_device_closes_device_equality() {
        let correlation =
            correlate_trusted_project_block_device(&mounted(), 2049, 77, true, true).unwrap();
        assert!(correlation.summary().block_device_bound());
        assert!(correlation.summary().whole_device());
        assert!(correlation.summary().filesystem_device_bound());
        assert_eq!(correlation.summary().mount_id(), 123);
        assert_eq!(correlation.summary().filesystem_generation().get(), 7);
        assert!(correlation.matches_block_device_identity(2049, 77));
        assert!(!correlation.matches_block_device_identity(2049, 78));
        assert!(correlation.matches_filesystem_device(2049));
    }

    #[test]
    fn block_rdev_must_equal_mounted_filesystem_device() {
        let error = correlate_trusted_project_block_device(&mounted(), 2050, 77, true, true)
            .unwrap_err();
        assert_eq!(
            error.kind(),
            TrustedProjectBlockDeviceCorrelationErrorKind::DeviceMismatch
        );
    }

    #[test]
    fn regular_or_partition_evidence_is_rejected() {
        assert_eq!(
            correlate_trusted_project_block_device(&mounted(), 2049, 77, false, true)
                .unwrap_err()
                .kind(),
            TrustedProjectBlockDeviceCorrelationErrorKind::InvalidBlockDevice
        );
        assert_eq!(
            correlate_trusted_project_block_device(&mounted(), 2049, 77, true, false)
                .unwrap_err()
                .kind(),
            TrustedProjectBlockDeviceCorrelationErrorKind::InvalidBlockDevice
        );
        assert_eq!(
            correlate_trusted_project_block_device(&mounted(), 2049, 0, true, true)
                .unwrap_err()
                .kind(),
            TrustedProjectBlockDeviceCorrelationErrorKind::InvalidBlockDevice
        );
    }

    #[test]
    fn debug_redacts_raw_block_identity() {
        let correlation =
            correlate_trusted_project_block_device(&mounted(), 2049, 77, true, true).unwrap();
        let debug = format!("{correlation:?}");
        assert!(debug.contains(REDACTED_DEVICE));
        assert!(debug.contains(REDACTED_INODE));
        assert!(!debug.contains("2049"));
        assert!(!debug.contains("77"));
    }
}
