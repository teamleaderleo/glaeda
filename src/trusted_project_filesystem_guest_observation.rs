//! Pure Linux guest correlation of one held project-filesystem root with exact mountinfo evidence.
//!
//! This ports the device/mountinfo checks physically exercised by #618/#628 into Rust without
//! adding guest execution or mount mutation. Callers supply `st_dev`/`st_ino` from an already-held
//! exact project-root descriptor, one fixed reviewed mountpoint locator, and bounded mountinfo.

use std::fmt;

use serde::Serialize;

use crate::project_disk_filesystem::{
    ProjectDiskFilesystemBinding, ProjectDiskFilesystemFormatProfileGeneration,
    ProjectDiskFilesystemGeneration, ProjectDiskFilesystemKind,
};

pub const TRUSTED_PROJECT_FILESYSTEM_GUEST_OBSERVATION_SCHEMA_VERSION: u8 = 1;
const MAX_MOUNTINFO_BYTES: usize = 1_048_576;
const MAX_MOUNTPOINT_BYTES: usize = 1_024;
const REDACTED_DEVICE: &str = "<private-linux-filesystem-device>";
const REDACTED_INODE: &str = "<private-linux-filesystem-inode>";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrustedProjectFilesystemGuestObservationSummary {
    schema_version: u8,
    filesystem_generation: ProjectDiskFilesystemGeneration,
    format_profile_generation: ProjectDiskFilesystemFormatProfileGeneration,
    filesystem_kind: ProjectDiskFilesystemKind,
    mount_id: u64,
    filesystem_device_bound: bool,
    mount_root_identity_bound: bool,
    read_write: bool,
}

impl TrustedProjectFilesystemGuestObservationSummary {
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
    pub const fn filesystem_device_bound(&self) -> bool {
        self.filesystem_device_bound
    }

    #[must_use]
    pub const fn mount_root_identity_bound(&self) -> bool {
        self.mount_root_identity_bound
    }

    #[must_use]
    pub const fn read_write(&self) -> bool {
        self.read_write
    }
}

/// Opaque guest-local observation that one exact held project root agrees with one exact mountinfo
/// row and the expected P4 filesystem generation/kind.
///
/// Raw Linux device/inode values stay private and are neither serialized nor exposed through
/// Debug. The root inode is retained separately from the filesystem device: sibling lower/upper/
/// work descriptors legitimately have different inodes while still belonging to this filesystem.
pub struct TrustedProjectFilesystemGuestObservation {
    summary: TrustedProjectFilesystemGuestObservationSummary,
    filesystem_device: u64,
    mount_root_inode: u64,
}

impl TrustedProjectFilesystemGuestObservation {
    #[must_use]
    pub const fn summary(&self) -> &TrustedProjectFilesystemGuestObservationSummary {
        &self.summary
    }

    /// Compare another freshly held descriptor against the same exact Linux filesystem device.
    #[must_use]
    pub const fn matches_filesystem_device(&self, observed_filesystem_device: u64) -> bool {
        self.filesystem_device == observed_filesystem_device
    }

    /// Revalidate the exact project mount-root descriptor identity captured by this observation.
    ///
    /// This is intentionally stronger than `matches_filesystem_device`: it is for reopening the
    /// project mount root itself, while sibling role directories should use device equality only.
    #[must_use]
    pub const fn matches_mount_root_identity(
        &self,
        observed_filesystem_device: u64,
        observed_filesystem_inode: u64,
    ) -> bool {
        observed_filesystem_inode != 0
            && self.filesystem_device == observed_filesystem_device
            && self.mount_root_inode == observed_filesystem_inode
    }
}

impl fmt::Debug for TrustedProjectFilesystemGuestObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedProjectFilesystemGuestObservation")
            .field("summary", &self.summary)
            .field("filesystem_device", &REDACTED_DEVICE)
            .field("mount_root_inode", &REDACTED_INODE)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustedProjectFilesystemGuestObservationErrorKind {
    InvalidInput,
    MountinfoMalformed,
    MountpointAmbiguous,
    DeviceMismatch,
    FilesystemMismatch,
    MountPolicyMismatch,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TrustedProjectFilesystemGuestObservationError {
    kind: TrustedProjectFilesystemGuestObservationErrorKind,
    code: &'static str,
    message: &'static str,
}

impl TrustedProjectFilesystemGuestObservationError {
    #[must_use]
    pub const fn kind(self) -> TrustedProjectFilesystemGuestObservationErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for TrustedProjectFilesystemGuestObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedProjectFilesystemGuestObservationError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for TrustedProjectFilesystemGuestObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for TrustedProjectFilesystemGuestObservationError {}

/// Correlate one held project-root descriptor observation with one exact mountinfo row.
///
/// The Linux `dev_t` split and reviewed mountinfo escape handling intentionally match the #618
/// physical receipt validator. Exactly one row must name `expected_mountpoint`; its major/minor must
/// equal the supplied descriptor's `st_dev`, its filesystem type must match `filesystem`, and the
/// visible project filesystem must be writable. The nonzero project-root inode is retained privately
/// so a later reopen can prove the exact `(st_dev, st_ino)` root identity again.
///
/// # Errors
///
/// Returns a bounded refusal for invalid root identity, malformed/ambiguous mountinfo, device drift,
/// filesystem-kind drift, read-only mount policy, or an unsafe mountpoint locator.
pub fn observe_trusted_project_filesystem_guest(
    filesystem: &ProjectDiskFilesystemBinding,
    observed_filesystem_device: u64,
    observed_filesystem_inode: u64,
    expected_mountpoint: &[u8],
    mountinfo: &[u8],
) -> Result<TrustedProjectFilesystemGuestObservation, TrustedProjectFilesystemGuestObservationError>
{
    validate_mountpoint(expected_mountpoint)?;
    if observed_filesystem_inode == 0
        || mountinfo.is_empty()
        || mountinfo.len() > MAX_MOUNTINFO_BYTES
    {
        return Err(invalid_input());
    }

    let (expected_major, expected_minor) = linux_device_numbers(observed_filesystem_device);
    let expected_filesystem_type = match filesystem.kind() {
        ProjectDiskFilesystemKind::Ext4 => b"ext4".as_slice(),
        ProjectDiskFilesystemKind::Xfs => b"xfs".as_slice(),
    };

    let mut matched_mount_id = None;
    for line in mountinfo.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let separator = find_separator(line).ok_or_else(mountinfo_malformed)?;
        let left_fields = split_fields(&line[..separator])?;
        let right_fields = split_fields(&line[separator + 3..])?;
        if left_fields.len() < 6 || right_fields.len() < 3 {
            return Err(mountinfo_malformed());
        }

        let mountpoint = decode_mountinfo_field(left_fields[4])?;
        if mountpoint != expected_mountpoint {
            continue;
        }
        if matched_mount_id.is_some() {
            return Err(mountpoint_ambiguous());
        }

        let mount_id = parse_decimal(left_fields[0]).ok_or_else(mountinfo_malformed)?;
        if mount_id == 0 {
            return Err(mountinfo_malformed());
        }
        let (major, minor) = parse_major_minor(left_fields[2])?;
        if major != expected_major || minor != expected_minor {
            return Err(device_mismatch());
        }
        if right_fields[0] != expected_filesystem_type {
            return Err(filesystem_mismatch());
        }

        let mut read_write = false;
        let mut read_only = false;
        for option in left_fields[5].split(|byte| *byte == b',') {
            match option {
                b"rw" => read_write = true,
                b"ro" => read_only = true,
                _ => {}
            }
        }
        if !read_write || read_only {
            return Err(mount_policy_mismatch());
        }
        matched_mount_id = Some(mount_id);
    }

    let mount_id = matched_mount_id.ok_or_else(mountpoint_ambiguous)?;
    Ok(TrustedProjectFilesystemGuestObservation {
        summary: TrustedProjectFilesystemGuestObservationSummary {
            schema_version: TRUSTED_PROJECT_FILESYSTEM_GUEST_OBSERVATION_SCHEMA_VERSION,
            filesystem_generation: filesystem.filesystem_generation(),
            format_profile_generation: filesystem.format_profile_generation(),
            filesystem_kind: filesystem.kind(),
            mount_id,
            filesystem_device_bound: true,
            mount_root_identity_bound: true,
            read_write: true,
        },
        filesystem_device: observed_filesystem_device,
        mount_root_inode: observed_filesystem_inode,
    })
}

fn validate_mountpoint(
    mountpoint: &[u8],
) -> Result<(), TrustedProjectFilesystemGuestObservationError> {
    if mountpoint.len() < 2
        || mountpoint.len() > MAX_MOUNTPOINT_BYTES
        || mountpoint.first() != Some(&b'/')
        || mountpoint.contains(&0)
    {
        return Err(invalid_input());
    }
    for component in mountpoint[1..].split(|byte| *byte == b'/') {
        if component.is_empty() || component == b"." || component == b".." {
            return Err(invalid_input());
        }
    }
    Ok(())
}

fn find_separator(line: &[u8]) -> Option<usize> {
    line.windows(3).position(|window| window == b" - ")
}

fn split_fields(
    fields: &[u8],
) -> Result<Vec<&[u8]>, TrustedProjectFilesystemGuestObservationError> {
    let result = fields.split(|byte| *byte == b' ').collect::<Vec<_>>();
    if result.is_empty() || result.iter().any(|field| field.is_empty()) {
        return Err(mountinfo_malformed());
    }
    Ok(result)
}

fn parse_decimal(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() {
        return None;
    }
    let mut value = 0_u64;
    for byte in bytes {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value.checked_mul(10)?.checked_add(u64::from(byte - b'0'))?;
    }
    Some(value)
}

fn parse_major_minor(
    bytes: &[u8],
) -> Result<(u64, u64), TrustedProjectFilesystemGuestObservationError> {
    let Some(separator) = bytes.iter().position(|byte| *byte == b':') else {
        return Err(mountinfo_malformed());
    };
    if bytes[separator + 1..].contains(&b':') {
        return Err(mountinfo_malformed());
    }
    let major = parse_decimal(&bytes[..separator]).ok_or_else(mountinfo_malformed)?;
    let minor = parse_decimal(&bytes[separator + 1..]).ok_or_else(mountinfo_malformed)?;
    Ok((major, minor))
}

fn decode_mountinfo_field(
    field: &[u8],
) -> Result<Vec<u8>, TrustedProjectFilesystemGuestObservationError> {
    let mut result = Vec::with_capacity(field.len());
    let mut index = 0;
    while index < field.len() {
        if field[index] != b'\\' {
            result.push(field[index]);
            index += 1;
            continue;
        }
        if index + 3 >= field.len() {
            return Err(mountinfo_malformed());
        }
        let octal = &field[index + 1..index + 4];
        if !octal.iter().all(u8::is_ascii_digit) || octal.iter().any(|byte| *byte > b'7') {
            return Err(mountinfo_malformed());
        }
        let decoded = (octal[0] - b'0') * 64 + (octal[1] - b'0') * 8 + (octal[2] - b'0');
        if !matches!(decoded, 9 | 10 | 32 | 92) {
            return Err(mountinfo_malformed());
        }
        result.push(decoded);
        index += 4;
    }
    Ok(result)
}

/// Decode Linux's 64-bit `dev_t` layout exactly as the accepted #618 physical validator does.
const fn linux_device_numbers(device: u64) -> (u64, u64) {
    let major = ((device & 0x0000_0000_000f_ff00) >> 8) | ((device & 0xffff_f000_0000_0000) >> 32);
    let minor = (device & 0x0000_0000_0000_00ff) | ((device & 0x0000_0fff_fff0_0000) >> 12);
    (major, minor)
}

const fn invalid_input() -> TrustedProjectFilesystemGuestObservationError {
    error(
        TrustedProjectFilesystemGuestObservationErrorKind::InvalidInput,
        "project_filesystem_guest_observation_invalid_input",
        "project filesystem guest observation input is invalid",
    )
}

const fn mountinfo_malformed() -> TrustedProjectFilesystemGuestObservationError {
    error(
        TrustedProjectFilesystemGuestObservationErrorKind::MountinfoMalformed,
        "project_filesystem_guest_mountinfo_malformed",
        "project filesystem guest mountinfo evidence is malformed",
    )
}

const fn mountpoint_ambiguous() -> TrustedProjectFilesystemGuestObservationError {
    error(
        TrustedProjectFilesystemGuestObservationErrorKind::MountpointAmbiguous,
        "project_filesystem_guest_mountpoint_ambiguous",
        "project filesystem guest mountpoint evidence is absent or ambiguous",
    )
}

const fn device_mismatch() -> TrustedProjectFilesystemGuestObservationError {
    error(
        TrustedProjectFilesystemGuestObservationErrorKind::DeviceMismatch,
        "project_filesystem_guest_device_mismatch",
        "project filesystem descriptor device disagrees with exact mountinfo evidence",
    )
}

const fn filesystem_mismatch() -> TrustedProjectFilesystemGuestObservationError {
    error(
        TrustedProjectFilesystemGuestObservationErrorKind::FilesystemMismatch,
        "project_filesystem_guest_type_mismatch",
        "project filesystem type disagrees with the accepted filesystem generation",
    )
}

const fn mount_policy_mismatch() -> TrustedProjectFilesystemGuestObservationError {
    error(
        TrustedProjectFilesystemGuestObservationErrorKind::MountPolicyMismatch,
        "project_filesystem_guest_mount_policy_mismatch",
        "project filesystem mount policy is incompatible with the accepted writable attachment",
    )
}

const fn error(
    kind: TrustedProjectFilesystemGuestObservationErrorKind,
    code: &'static str,
    message: &'static str,
) -> TrustedProjectFilesystemGuestObservationError {
    TrustedProjectFilesystemGuestObservationError {
        kind,
        code,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        REDACTED_DEVICE, REDACTED_INODE, TrustedProjectFilesystemGuestObservationErrorKind,
        linux_device_numbers, observe_trusted_project_filesystem_guest,
    };
    use crate::project_catalog::ProjectIdentity;
    use crate::project_disk_filesystem::{
        ProjectDiskFilesystemBinding, ProjectDiskFilesystemFormatProfileGeneration,
        ProjectDiskFilesystemGeneration, ProjectDiskFilesystemKind,
    };
    use crate::project_disk_lease::{ProjectDiskGeneration, ProjectDiskId, ProjectDiskLeaseRecord};

    fn filesystem(kind: ProjectDiskFilesystemKind) -> ProjectDiskFilesystemBinding {
        let record = ProjectDiskLeaseRecord::new_detached(
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            ProjectDiskId::parse("disk-a").unwrap(),
            ProjectDiskGeneration::new(3).unwrap(),
        );
        ProjectDiskFilesystemBinding::new(
            &record,
            ProjectDiskFilesystemGeneration::new(7).unwrap(),
            ProjectDiskFilesystemFormatProfileGeneration::new(2).unwrap(),
            kind,
        )
    }

    const fn make_linux_device(major: u64, minor: u64) -> u64 {
        ((major & 0x0000_0fff) << 8)
            | ((major & 0xffff_f000) << 32)
            | (minor & 0x0000_00ff)
            | ((minor & 0xffff_ff00) << 12)
    }

    fn mountinfo(major: u64, minor: u64, mountpoint: &str, options: &str, fs: &str) -> Vec<u8> {
        format!("73 29 {major}:{minor} / {mountpoint} {options} - {fs} /dev/vdb rw\n").into_bytes()
    }

    #[test]
    fn exact_mount_root_and_sibling_device_checks_are_distinct() {
        let device = make_linux_device(259, 65_537);
        let info = mountinfo(259, 65_537, "/srv/quarry", "rw,nodev,nosuid", "ext4");
        let observation = observe_trusted_project_filesystem_guest(
            &filesystem(ProjectDiskFilesystemKind::Ext4),
            device,
            44,
            b"/srv/quarry",
            &info,
        )
        .unwrap();

        assert!(observation.summary().filesystem_device_bound());
        assert!(observation.summary().mount_root_identity_bound());
        assert!(observation.summary().read_write());
        assert_eq!(observation.summary().mount_id(), 73);
        assert!(observation.matches_mount_root_identity(device, 44));
        assert!(!observation.matches_mount_root_identity(device, 45));
        assert!(!observation.matches_mount_root_identity(device + 1, 44));
        assert!(observation.matches_filesystem_device(device));
        assert!(!observation.matches_filesystem_device(device + 1));
    }

    #[test]
    fn zero_inode_is_refused_and_private_root_identity_is_redacted() {
        let device = make_linux_device(8, 17);
        let info = mountinfo(8, 17, "/srv/quarry", "rw,nodev,nosuid", "ext4");
        let error = observe_trusted_project_filesystem_guest(
            &filesystem(ProjectDiskFilesystemKind::Ext4),
            device,
            0,
            b"/srv/quarry",
            &info,
        )
        .unwrap_err();
        assert_eq!(
            error.kind(),
            TrustedProjectFilesystemGuestObservationErrorKind::InvalidInput
        );

        let observation = observe_trusted_project_filesystem_guest(
            &filesystem(ProjectDiskFilesystemKind::Ext4),
            device,
            987_654,
            b"/srv/quarry",
            &info,
        )
        .unwrap();
        let debug = format!("{observation:?}");
        assert!(debug.contains(REDACTED_DEVICE));
        assert!(debug.contains(REDACTED_INODE));
        assert!(!debug.contains("987654"));
    }

    #[test]
    fn extended_linux_device_bits_match_mountinfo() {
        let major = 0x1abc;
        let minor = 0x12345;
        let device = make_linux_device(major, minor);
        assert_eq!(linux_device_numbers(device), (major, minor));
        let info = mountinfo(major, minor, "/srv/quarry", "rw", "xfs");
        let observation = observe_trusted_project_filesystem_guest(
            &filesystem(ProjectDiskFilesystemKind::Xfs),
            device,
            1,
            b"/srv/quarry",
            &info,
        )
        .unwrap();
        assert_eq!(
            observation.summary().filesystem_kind(),
            ProjectDiskFilesystemKind::Xfs
        );
    }

    #[test]
    fn device_filesystem_and_policy_mismatches_fail_closed() {
        let device = make_linux_device(8, 17);
        let wrong_device = mountinfo(8, 18, "/srv/quarry", "rw", "ext4");
        assert_eq!(
            observe_trusted_project_filesystem_guest(
                &filesystem(ProjectDiskFilesystemKind::Ext4),
                device,
                1,
                b"/srv/quarry",
                &wrong_device,
            )
            .unwrap_err()
            .kind(),
            TrustedProjectFilesystemGuestObservationErrorKind::DeviceMismatch
        );

        let wrong_fs = mountinfo(8, 17, "/srv/quarry", "rw", "xfs");
        assert_eq!(
            observe_trusted_project_filesystem_guest(
                &filesystem(ProjectDiskFilesystemKind::Ext4),
                device,
                1,
                b"/srv/quarry",
                &wrong_fs,
            )
            .unwrap_err()
            .kind(),
            TrustedProjectFilesystemGuestObservationErrorKind::FilesystemMismatch
        );

        for options in ["ro,nodev", "rw,ro"] {
            let info = mountinfo(8, 17, "/srv/quarry", options, "ext4");
            assert_eq!(
                observe_trusted_project_filesystem_guest(
                    &filesystem(ProjectDiskFilesystemKind::Ext4),
                    device,
                    1,
                    b"/srv/quarry",
                    &info,
                )
                .unwrap_err()
                .kind(),
                TrustedProjectFilesystemGuestObservationErrorKind::MountPolicyMismatch
            );
        }
    }

    #[test]
    fn absent_duplicate_and_unreviewed_mountpoint_evidence_is_refused() {
        let device = make_linux_device(8, 17);
        let one = mountinfo(8, 17, "/srv/other", "rw", "ext4");
        assert_eq!(
            observe_trusted_project_filesystem_guest(
                &filesystem(ProjectDiskFilesystemKind::Ext4),
                device,
                1,
                b"/srv/quarry",
                &one,
            )
            .unwrap_err()
            .kind(),
            TrustedProjectFilesystemGuestObservationErrorKind::MountpointAmbiguous
        );

        let mut duplicate = mountinfo(8, 17, "/srv/quarry", "rw", "ext4");
        duplicate.extend(mountinfo(8, 17, "/srv/quarry", "rw", "ext4"));
        assert_eq!(
            observe_trusted_project_filesystem_guest(
                &filesystem(ProjectDiskFilesystemKind::Ext4),
                device,
                1,
                b"/srv/quarry",
                &duplicate,
            )
            .unwrap_err()
            .kind(),
            TrustedProjectFilesystemGuestObservationErrorKind::MountpointAmbiguous
        );
    }

    #[test]
    fn reviewed_mountinfo_escapes_decode_and_other_escapes_fail() {
        let device = make_linux_device(8, 17);
        let reviewed = b"73 29 8:17 / /srv/quarry\\040data rw - ext4 /dev/vdb rw\n";
        let observation = observe_trusted_project_filesystem_guest(
            &filesystem(ProjectDiskFilesystemKind::Ext4),
            device,
            1,
            b"/srv/quarry data",
            reviewed,
        )
        .unwrap();
        assert_eq!(observation.summary().mount_id(), 73);

        let unreviewed = b"73 29 8:17 / /srv/quarry\\141data rw - ext4 /dev/vdb rw\n";
        assert_eq!(
            observe_trusted_project_filesystem_guest(
                &filesystem(ProjectDiskFilesystemKind::Ext4),
                device,
                1,
                b"/srv/quarryadata",
                unreviewed,
            )
            .unwrap_err()
            .kind(),
            TrustedProjectFilesystemGuestObservationErrorKind::MountinfoMalformed
        );
    }
}
