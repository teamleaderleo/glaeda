use std::os::fd::OwnedFd;
use std::path::Path;

use rustix::fs::{self, Dir, FileType, Mode, OFlags};
use rustix::io::Errno;
use serde::Serialize;

use crate::state::{InstallationId, STATE_ROOT};
use crate::state_store::{MAX_STATE_DOCUMENT_BYTES, StateStoreError, StateStoreErrorKind};

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const ENTRY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const TEMP_FILE_PREFIX: &[u8] = b".smolrunner-tmp-";
const TEMP_RANDOM_HEX_LEN: usize = 32;
const MAX_RECOVERY_FINDINGS: usize = 1_024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryArea {
    Project,
    Resources,
    Journals,
    Receipts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryDisposition {
    RecoverableOrphan,
    Suspicious,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryConcern {
    MalformedTemporaryName,
    Symlink,
    NonRegularFile,
    WrongMode,
    WrongOwner,
    Oversized,
    InspectionFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecoveryFinding {
    area: RecoveryArea,
    name: String,
    disposition: RecoveryDisposition,
    concerns: Vec<RecoveryConcern>,
    #[serde(skip_serializing_if = "Option::is_none")]
    size_bytes: Option<u64>,
}

impl RecoveryFinding {
    #[must_use]
    pub fn area(&self) -> RecoveryArea {
        self.area
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn disposition(&self) -> RecoveryDisposition {
        self.disposition
    }

    #[must_use]
    pub fn concerns(&self) -> &[RecoveryConcern] {
        &self.concerns
    }

    #[must_use]
    pub fn size_bytes(&self) -> Option<u64> {
        self.size_bytes
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecoveryReport {
    findings: Vec<RecoveryFinding>,
    truncated: bool,
}

impl RecoveryReport {
    #[must_use]
    pub fn findings(&self) -> &[RecoveryFinding] {
        &self.findings
    }

    #[must_use]
    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

/// Inspect orphan temporary state files beneath the canonical state root.
///
/// This function is read-only. It reports recoverable and suspicious candidates without deleting,
/// renaming, chmodding, or otherwise mutating any filesystem object.
///
/// # Errors
///
/// Returns a bounded store error when the installation tree cannot be traversed safely.
pub fn inspect_default_orphans(
    installation_id: &InstallationId,
) -> Result<RecoveryReport, StateStoreError> {
    inspect_orphans(STATE_ROOT, installation_id)
}

/// Inspect orphan temporary state files beneath one trusted state root.
///
/// Exact `.smolrunner-tmp-` names followed by 32 lowercase hexadecimal characters are recoverable
/// only when they identify regular files owned by the root owner, have mode `0600`, and stay within
/// the state-document size limit. Malformed prefixes and incompatible filesystem objects are
/// surfaced as suspicious findings. Unrelated names are ignored.
///
/// # Errors
///
/// Returns a bounded store error when the installation tree cannot be traversed safely.
pub fn inspect_orphans(
    root_path: impl AsRef<Path>,
    installation_id: &InstallationId,
) -> Result<RecoveryReport, StateStoreError> {
    let root = open_directory_path(root_path.as_ref(), "state root")?;
    let root_stat = inspect_directory(&root, "state root")?;
    let root_owner = (root_stat.st_uid, root_stat.st_gid);
    let root_uid = root_owner.0;
    let installations = open_directory_at(&root, "installations", "installations directory")?;
    inspect_directory_owner(&installations, "installations directory", root_owner)?;
    let installation = open_directory_at(
        &installations,
        installation_id.as_str(),
        "installation directory",
    )?;
    inspect_directory_owner(&installation, "installation directory", root_owner)?;
    let resources = open_directory_at(&installation, "resources", "resources directory")?;
    inspect_directory_owner(&resources, "resources directory", root_owner)?;
    let journals = open_directory_at(&installation, "journals", "journals directory")?;
    inspect_directory_owner(&journals, "journals directory", root_owner)?;
    let receipts = open_directory_at(&installation, "receipts", "receipts directory")?;
    inspect_directory_owner(&receipts, "receipts directory", root_owner)?;

    let mut findings = Vec::new();
    let mut truncated = false;
    scan_directory(
        &installation,
        RecoveryArea::Project,
        root_uid,
        &mut findings,
        &mut truncated,
    )?;
    if !truncated {
        scan_directory(
            &resources,
            RecoveryArea::Resources,
            root_uid,
            &mut findings,
            &mut truncated,
        )?;
    }
    if !truncated {
        scan_directory(
            &journals,
            RecoveryArea::Journals,
            root_uid,
            &mut findings,
            &mut truncated,
        )?;
    }
    if !truncated {
        scan_directory(
            &receipts,
            RecoveryArea::Receipts,
            root_uid,
            &mut findings,
            &mut truncated,
        )?;
    }

    findings.sort_by(|left, right| {
        area_rank(left.area)
            .cmp(&area_rank(right.area))
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(RecoveryReport {
        findings,
        truncated,
    })
}

fn scan_directory(
    directory: &OwnedFd,
    area: RecoveryArea,
    expected_uid: u32,
    findings: &mut Vec<RecoveryFinding>,
    truncated: &mut bool,
) -> Result<(), StateStoreError> {
    let mut entries = Dir::read_from(directory).map_err(|_| {
        StateStoreError::public(
            StateStoreErrorKind::Io,
            "could not enumerate a state directory",
        )
    })?;
    for entry in &mut entries {
        let entry = entry.map_err(|_| {
            StateStoreError::public(
                StateStoreErrorKind::Io,
                "could not read a state-directory entry",
            )
        })?;
        let name_bytes = entry.file_name().to_bytes();
        if name_bytes == b"." || name_bytes == b".." || !name_bytes.starts_with(TEMP_FILE_PREFIX) {
            continue;
        }
        if findings.len() == MAX_RECOVERY_FINDINGS {
            *truncated = true;
            break;
        }
        findings.push(inspect_candidate(
            directory,
            area,
            name_bytes,
            entry.file_type(),
            expected_uid,
        ));
    }
    Ok(())
}

fn inspect_candidate(
    directory: &OwnedFd,
    area: RecoveryArea,
    name_bytes: &[u8],
    hinted_type: FileType,
    expected_uid: u32,
) -> RecoveryFinding {
    let mut concerns = Vec::new();
    if !is_canonical_temp_name(name_bytes) {
        concerns.push(RecoveryConcern::MalformedTemporaryName);
    }
    if hinted_type.is_symlink() {
        concerns.push(RecoveryConcern::Symlink);
        return finish_finding(area, name_bytes, concerns, None);
    }

    let opened = match fs::openat(directory, name_bytes, ENTRY_FLAGS, Mode::empty()) {
        Ok(opened) => opened,
        Err(Errno::LOOP) => {
            concerns.push(RecoveryConcern::Symlink);
            return finish_finding(area, name_bytes, concerns, None);
        }
        Err(_) => {
            concerns.push(RecoveryConcern::InspectionFailed);
            return finish_finding(area, name_bytes, concerns, None);
        }
    };
    let stat = match fs::fstat(&opened) {
        Ok(stat) => stat,
        Err(_) => {
            concerns.push(RecoveryConcern::InspectionFailed);
            return finish_finding(area, name_bytes, concerns, None);
        }
    };
    if !FileType::from_raw_mode(stat.st_mode).is_file() {
        concerns.push(RecoveryConcern::NonRegularFile);
    }
    if stat.st_mode & 0o7777 != 0o600 {
        concerns.push(RecoveryConcern::WrongMode);
    }
    if stat.st_uid != expected_uid {
        concerns.push(RecoveryConcern::WrongOwner);
    }
    if stat.st_size < 0 || stat.st_size as u64 > MAX_STATE_DOCUMENT_BYTES as u64 {
        concerns.push(RecoveryConcern::Oversized);
    }
    let size_bytes = u64::try_from(stat.st_size).ok();
    finish_finding(area, name_bytes, concerns, size_bytes)
}

fn finish_finding(
    area: RecoveryArea,
    name_bytes: &[u8],
    concerns: Vec<RecoveryConcern>,
    size_bytes: Option<u64>,
) -> RecoveryFinding {
    let disposition = if concerns.is_empty() {
        RecoveryDisposition::RecoverableOrphan
    } else {
        RecoveryDisposition::Suspicious
    };
    RecoveryFinding {
        area,
        name: public_name(name_bytes),
        disposition,
        concerns,
        size_bytes,
    }
}

fn is_canonical_temp_name(name: &[u8]) -> bool {
    let suffix = &name[TEMP_FILE_PREFIX.len()..];
    suffix.len() == TEMP_RANDOM_HEX_LEN
        && suffix
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

fn public_name(name: &[u8]) -> String {
    if name.iter().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
    }) {
        String::from_utf8(name.to_vec()).expect("safe ASCII is valid UTF-8")
    } else {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut encoded = String::with_capacity(4 + name.len() * 2);
        encoded.push_str("hex:");
        for byte in name {
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 0x0f) as usize] as char);
        }
        encoded
    }
}

fn open_directory_path(path: &Path, subject: &str) -> Result<OwnedFd, StateStoreError> {
    let directory = fs::open(path, DIRECTORY_FLAGS, Mode::empty())
        .map_err(|error| map_directory_open_error(error, subject))?;
    inspect_directory(&directory, subject)?;
    Ok(directory)
}

fn open_directory_at(
    parent: &OwnedFd,
    name: &str,
    subject: &str,
) -> Result<OwnedFd, StateStoreError> {
    let directory = fs::openat(parent, name, DIRECTORY_FLAGS, Mode::empty())
        .map_err(|error| map_directory_open_error(error, subject))?;
    inspect_directory(&directory, subject)?;
    Ok(directory)
}

fn inspect_directory(
    directory: &OwnedFd,
    subject: &str,
) -> Result<rustix::fs::Stat, StateStoreError> {
    let stat = fs::fstat(directory).map_err(|_| {
        StateStoreError::public(
            StateStoreErrorKind::Io,
            format!("could not inspect {subject}"),
        )
    })?;
    if !FileType::from_raw_mode(stat.st_mode).is_dir() {
        return Err(StateStoreError::public(
            StateStoreErrorKind::UnsafeFilesystem,
            format!("{subject} is not a directory"),
        ));
    }
    if stat.st_mode & 0o7777 != 0o750 {
        return Err(StateStoreError::public(
            StateStoreErrorKind::UnsafeFilesystem,
            format!("{subject} does not have mode 0750"),
        ));
    }
    Ok(stat)
}

fn inspect_directory_owner(
    directory: &OwnedFd,
    subject: &str,
    expected_owner: (u32, u32),
) -> Result<(), StateStoreError> {
    let stat = inspect_directory(directory, subject)?;
    if (stat.st_uid, stat.st_gid) == expected_owner {
        Ok(())
    } else {
        Err(StateStoreError::public(
            StateStoreErrorKind::UnsafeFilesystem,
            format!("{subject} has an unexpected owner or group"),
        ))
    }
}

fn map_directory_open_error(error: Errno, subject: &str) -> StateStoreError {
    match error {
        Errno::LOOP | Errno::NOTDIR => StateStoreError::public(
            StateStoreErrorKind::UnsafeFilesystem,
            format!("{subject} is symlinked or not a directory"),
        ),
        Errno::NOENT => {
            StateStoreError::public(StateStoreErrorKind::Io, format!("{subject} does not exist"))
        }
        _ => StateStoreError::public(StateStoreErrorKind::Io, format!("could not open {subject}")),
    }
}

const fn area_rank(area: RecoveryArea) -> u8 {
    match area {
        RecoveryArea::Project => 0,
        RecoveryArea::Resources => 1,
        RecoveryArea::Journals => 2,
        RecoveryArea::Receipts => 3,
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::linux_state_prepare::prepare_installation;
    use crate::state::InstallationId;
    use crate::state_store::{MAX_STATE_DOCUMENT_BYTES, StateStoreErrorKind};

    use super::{
        RecoveryArea, RecoveryConcern, RecoveryDisposition, inspect_directory,
        inspect_directory_owner, inspect_orphans, open_directory_path,
    };

    static NEXT_TEMP_ROOT: AtomicU64 = AtomicU64::new(1);
    const VALID_NAME: &str = ".smolrunner-tmp-0123456789abcdef0123456789abcdef";

    struct TempRoot {
        path: PathBuf,
    }

    impl TempRoot {
        fn new(label: &str) -> Self {
            let sequence = NEXT_TEMP_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "smolrunner-recovery-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create temporary root");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).expect("set root mode");
            prepare_installation(&path, &installation_id()).expect("prepare installation");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }

        fn installation(&self) -> PathBuf {
            self.path
                .join("installations")
                .join(installation_id().as_str())
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn installation_id() -> InstallationId {
        InstallationId::parse("0123456789abcdef").expect("installation ID")
    }

    fn private_write(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).expect("write candidate");
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("set private mode");
    }

    #[test]
    fn reports_exact_private_regular_temp_as_recoverable() {
        let root = TempRoot::new("recoverable");
        private_write(&root.installation().join(VALID_NAME), b"partial state");

        let report = inspect_orphans(root.path(), &installation_id()).expect("inspect orphans");
        assert!(!report.truncated());
        assert_eq!(report.findings().len(), 1);
        let finding = &report.findings()[0];
        assert_eq!(finding.area(), RecoveryArea::Project);
        assert_eq!(
            finding.disposition(),
            RecoveryDisposition::RecoverableOrphan
        );
        assert!(finding.concerns().is_empty());
        assert_eq!(finding.size_bytes(), Some(13));
    }

    #[test]
    fn reports_malformed_names_symlinks_modes_and_sizes_as_suspicious() {
        let root = TempRoot::new("suspicious");
        let resources = root.installation().join("resources");
        private_write(&resources.join(".smolrunner-tmp-short"), b"short");
        let broad = resources.join(VALID_NAME);
        fs::write(&broad, b"broad").expect("write broad candidate");
        fs::set_permissions(&broad, fs::Permissions::from_mode(0o644)).expect("set broad mode");

        let journals = root.installation().join("journals");
        let outside = root.path().join("outside");
        fs::write(&outside, b"foreign").expect("write symlink target");
        symlink(&outside, journals.join(VALID_NAME)).expect("create temp symlink");
        private_write(
            &journals.join(".smolrunner-tmp-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            &vec![0_u8; MAX_STATE_DOCUMENT_BYTES + 1],
        );
        let receipts = root.installation().join("receipts");
        private_write(
            &receipts.join(".smolrunner-tmp-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            b"receipt",
        );

        let report = inspect_orphans(root.path(), &installation_id()).expect("inspect orphans");
        assert_eq!(report.findings().len(), 5);
        assert!(
            report
                .findings()
                .iter()
                .all(|finding| { finding.disposition() == RecoveryDisposition::Suspicious })
        );
        assert!(report.findings().iter().any(|finding| {
            finding
                .concerns()
                .contains(&RecoveryConcern::MalformedTemporaryName)
        }));
        assert!(
            report
                .findings()
                .iter()
                .any(|finding| { finding.concerns().contains(&RecoveryConcern::WrongMode) })
        );
        assert!(
            report
                .findings()
                .iter()
                .any(|finding| { finding.concerns().contains(&RecoveryConcern::Symlink) })
        );
        assert!(
            report
                .findings()
                .iter()
                .any(|finding| { finding.concerns().contains(&RecoveryConcern::Oversized) })
        );
        assert!(
            report
                .findings()
                .iter()
                .any(|finding| finding.area() == RecoveryArea::Receipts)
        );
    }

    #[test]
    fn ignores_unrelated_files_and_safely_encodes_control_names() {
        let root = TempRoot::new("names");
        let resources = root.installation().join("resources");
        private_write(&resources.join("ordinary.json"), b"ordinary");
        let unsafe_name = OsString::from_vec(b".smolrunner-tmp-bad\nname".to_vec());
        private_write(&resources.join(unsafe_name), b"candidate");

        let report = inspect_orphans(root.path(), &installation_id()).expect("inspect orphans");
        assert_eq!(report.findings().len(), 1);
        assert!(report.findings()[0].name().starts_with("hex:"));
        assert!(
            report.findings()[0]
                .concerns()
                .contains(&RecoveryConcern::MalformedTemporaryName)
        );
    }

    #[test]
    fn mismatched_directory_group_is_rejected() {
        let root = TempRoot::new("group-mismatch");
        let directory = open_directory_path(root.path(), "state root").expect("open root");
        let stat = inspect_directory(&directory, "state root").expect("inspect root");
        let unexpected_owner = (stat.st_uid, stat.st_gid ^ 1);
        let error = inspect_directory_owner(&directory, "state root", unexpected_owner)
            .expect_err("different group must fail");
        assert_eq!(error.kind(), StateStoreErrorKind::UnsafeFilesystem);
    }
}
