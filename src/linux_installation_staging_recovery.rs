use std::fs::File;
use std::io::{Read, Take};
use std::os::fd::OwnedFd;
use std::path::Path;

use rustix::fs::{self, Dir, FileType, Mode, OFlags};
use rustix::io::Errno;
use serde::Serialize;

use crate::state::{InstallationId, STATE_ROOT};
use crate::state_document::{StateDocument, decode_state_document};
use crate::state_store::{MAX_STATE_DOCUMENT_BYTES, StateStoreError, StateStoreErrorKind};

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const STAGING_DIRECTORY: &str = "staging";
const RESOURCES_DIRECTORY: &str = "resources";
const JOURNALS_DIRECTORY: &str = "journals";
const PROJECT_FILE: &str = "project.json";
const MAX_STAGED_INSTALLATIONS: usize = 1_024;
const MAX_STAGE_ENTRIES: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StagedInstallationDisposition {
    CompleteOrphan,
    Incomplete,
    Suspicious,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StagedInstallationConcern {
    MalformedInstallationId,
    Symlink,
    NonDirectory,
    WrongMode,
    WrongOwner,
    UnexpectedEntry,
    MissingResources,
    UnsafeResources,
    MissingJournals,
    UnsafeJournals,
    MissingProject,
    UnsafeProject,
    ProjectHardLinked,
    ProjectWrongMode,
    ProjectWrongOwner,
    ProjectOversized,
    ProjectInvalidUtf8,
    ProjectInvalidDocument,
    ProjectIdMismatch,
    InspectionFailed,
}

impl StagedInstallationConcern {
    const fn is_suspicious(self) -> bool {
        matches!(
            self,
            Self::MalformedInstallationId
                | Self::Symlink
                | Self::NonDirectory
                | Self::WrongMode
                | Self::WrongOwner
                | Self::UnexpectedEntry
                | Self::UnsafeResources
                | Self::UnsafeJournals
                | Self::UnsafeProject
                | Self::ProjectHardLinked
                | Self::ProjectWrongMode
                | Self::ProjectWrongOwner
                | Self::ProjectOversized
                | Self::InspectionFailed
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StagedInstallationFinding {
    name: String,
    disposition: StagedInstallationDisposition,
    concerns: Vec<StagedInstallationConcern>,
}

impl StagedInstallationFinding {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn disposition(&self) -> StagedInstallationDisposition {
        self.disposition
    }

    #[must_use]
    pub fn concerns(&self) -> &[StagedInstallationConcern] {
        &self.concerns
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StagedInstallationRecoveryReport {
    findings: Vec<StagedInstallationFinding>,
    truncated: bool,
}

impl StagedInstallationRecoveryReport {
    #[must_use]
    pub fn findings(&self) -> &[StagedInstallationFinding] {
        &self.findings
    }

    #[must_use]
    pub const fn truncated(&self) -> bool {
        self.truncated
    }
}

/// Inspect abandoned installation-publication staging trees beneath the canonical state root.
///
/// This function is read-only. It does not publish, delete, rename, chmod, or otherwise mutate a
/// staged installation.
///
/// # Errors
///
/// Returns a bounded store error when the state root or fixed staging directory cannot be traversed
/// safely.
pub fn inspect_default_staged_installations()
-> Result<StagedInstallationRecoveryReport, StateStoreError> {
    inspect_staged_installations(STATE_ROOT)
}

/// Inspect abandoned installation-publication staging trees beneath one trusted state root.
///
/// A complete orphan has a canonical installation ID, exact owner and mode, only the expected
/// `resources`, `journals`, and `project.json` entries, and a strict project document whose embedded
/// installation ID matches its directory. Structurally trusted but incomplete content is reported
/// as incomplete. Symlinks, foreign ownership, broad modes, hard links, unexpected entries, and
/// inspection failures are suspicious. No finding is changed automatically.
///
/// # Errors
///
/// Returns a bounded store error when the state root or fixed staging directory cannot be traversed
/// safely.
pub fn inspect_staged_installations(
    root_path: impl AsRef<Path>,
) -> Result<StagedInstallationRecoveryReport, StateStoreError> {
    let root = open_directory_path(root_path.as_ref(), "state root")?;
    let root_stat = inspect_managed_directory(&root, "state root", None)?;
    let owner = (root_stat.st_uid, root_stat.st_gid);
    let Some(staging) = open_optional_directory_at(&root, STAGING_DIRECTORY, "staging directory")?
    else {
        return Ok(StagedInstallationRecoveryReport {
            findings: Vec::new(),
            truncated: false,
        });
    };
    inspect_managed_directory(&staging, "staging directory", Some(owner))?;

    let mut entries = Dir::read_from(&staging)
        .map_err(|_| io_error("could not enumerate the installation staging directory"))?;
    let mut findings = Vec::new();
    let mut truncated = false;
    for entry in &mut entries {
        let entry = entry.map_err(|_| io_error("could not read a staged installation entry"))?;
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        if findings.len() == MAX_STAGED_INSTALLATIONS {
            truncated = true;
            break;
        }
        findings.push(inspect_staged_entry(
            &staging,
            name,
            entry.file_type(),
            owner,
        ));
    }
    findings.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(StagedInstallationRecoveryReport {
        findings,
        truncated,
    })
}

fn inspect_staged_entry(
    staging: &OwnedFd,
    name_bytes: &[u8],
    hinted_type: FileType,
    owner: (u32, u32),
) -> StagedInstallationFinding {
    let mut concerns = Vec::new();
    let installation_id = parse_installation_id(name_bytes, &mut concerns);
    if hinted_type.is_symlink() {
        concerns.push(StagedInstallationConcern::Symlink);
        return finish_finding(name_bytes, concerns);
    }

    let directory = match fs::openat(staging, name_bytes, DIRECTORY_FLAGS, Mode::empty()) {
        Ok(directory) => directory,
        Err(Errno::LOOP) => {
            concerns.push(StagedInstallationConcern::Symlink);
            return finish_finding(name_bytes, concerns);
        }
        Err(Errno::NOTDIR) => {
            concerns.push(StagedInstallationConcern::NonDirectory);
            return finish_finding(name_bytes, concerns);
        }
        Err(_) => {
            concerns.push(StagedInstallationConcern::InspectionFailed);
            return finish_finding(name_bytes, concerns);
        }
    };
    inspect_entry_directory(&directory, owner, &mut concerns);
    if concerns.iter().any(|concern| concern.is_suspicious()) {
        return finish_finding(name_bytes, concerns);
    }

    inspect_stage_contents(&directory, &mut concerns);
    inspect_expected_directory(
        &directory,
        RESOURCES_DIRECTORY,
        owner,
        StagedInstallationConcern::MissingResources,
        StagedInstallationConcern::UnsafeResources,
        &mut concerns,
    );
    inspect_expected_directory(
        &directory,
        JOURNALS_DIRECTORY,
        owner,
        StagedInstallationConcern::MissingJournals,
        StagedInstallationConcern::UnsafeJournals,
        &mut concerns,
    );
    inspect_project(
        &directory,
        owner,
        installation_id.as_ref(),
        &mut concerns,
    );
    finish_finding(name_bytes, concerns)
}

fn parse_installation_id(
    name: &[u8],
    concerns: &mut Vec<StagedInstallationConcern>,
) -> Option<InstallationId> {
    let Ok(name) = std::str::from_utf8(name) else {
        concerns.push(StagedInstallationConcern::MalformedInstallationId);
        return None;
    };
    match InstallationId::parse(name) {
        Ok(installation_id) => Some(installation_id),
        Err(_) => {
            concerns.push(StagedInstallationConcern::MalformedInstallationId);
            None
        }
    }
}

fn inspect_entry_directory(
    directory: &OwnedFd,
    owner: (u32, u32),
    concerns: &mut Vec<StagedInstallationConcern>,
) {
    let stat = match fs::fstat(directory) {
        Ok(stat) => stat,
        Err(_) => {
            concerns.push(StagedInstallationConcern::InspectionFailed);
            return;
        }
    };
    if !FileType::from_raw_mode(stat.st_mode).is_dir() {
        concerns.push(StagedInstallationConcern::NonDirectory);
    }
    if stat.st_mode & 0o7777 != 0o750 {
        concerns.push(StagedInstallationConcern::WrongMode);
    }
    if owner != (stat.st_uid, stat.st_gid) {
        concerns.push(StagedInstallationConcern::WrongOwner);
    }
}

fn inspect_stage_contents(
    directory: &OwnedFd,
    concerns: &mut Vec<StagedInstallationConcern>,
) {
    let mut entries = match Dir::read_from(directory) {
        Ok(entries) => entries,
        Err(_) => {
            concerns.push(StagedInstallationConcern::InspectionFailed);
            return;
        }
    };
    let mut count = 0_usize;
    for entry in &mut entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                concerns.push(StagedInstallationConcern::InspectionFailed);
                return;
            }
        };
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        count += 1;
        if count > MAX_STAGE_ENTRIES {
            concerns.push(StagedInstallationConcern::UnexpectedEntry);
            return;
        }
        if !matches!(
            name,
            b"resources" | b"journals" | b"project.json"
        ) {
            concerns.push(StagedInstallationConcern::UnexpectedEntry);
        }
    }
}

fn inspect_expected_directory(
    parent: &OwnedFd,
    name: &str,
    owner: (u32, u32),
    missing: StagedInstallationConcern,
    unsafe_concern: StagedInstallationConcern,
    concerns: &mut Vec<StagedInstallationConcern>,
) {
    let directory = match fs::openat(parent, name, DIRECTORY_FLAGS, Mode::empty()) {
        Ok(directory) => directory,
        Err(Errno::NOENT) => {
            concerns.push(missing);
            return;
        }
        Err(_) => {
            concerns.push(unsafe_concern);
            return;
        }
    };
    let stat = match fs::fstat(&directory) {
        Ok(stat) => stat,
        Err(_) => {
            concerns.push(StagedInstallationConcern::InspectionFailed);
            return;
        }
    };
    if !FileType::from_raw_mode(stat.st_mode).is_dir()
        || stat.st_mode & 0o7777 != 0o750
        || owner != (stat.st_uid, stat.st_gid)
    {
        concerns.push(unsafe_concern);
    }
}

fn inspect_project(
    installation: &OwnedFd,
    owner: (u32, u32),
    expected_id: Option<&InstallationId>,
    concerns: &mut Vec<StagedInstallationConcern>,
) {
    let project = match fs::openat(installation, PROJECT_FILE, FILE_FLAGS, Mode::empty()) {
        Ok(project) => project,
        Err(Errno::NOENT) => {
            concerns.push(StagedInstallationConcern::MissingProject);
            return;
        }
        Err(_) => {
            concerns.push(StagedInstallationConcern::UnsafeProject);
            return;
        }
    };
    let stat = match fs::fstat(&project) {
        Ok(stat) => stat,
        Err(_) => {
            concerns.push(StagedInstallationConcern::InspectionFailed);
            return;
        }
    };
    if !FileType::from_raw_mode(stat.st_mode).is_file() {
        concerns.push(StagedInstallationConcern::UnsafeProject);
        return;
    }
    if stat.st_nlink != 1 {
        concerns.push(StagedInstallationConcern::ProjectHardLinked);
    }
    if stat.st_mode & 0o7777 != 0o600 {
        concerns.push(StagedInstallationConcern::ProjectWrongMode);
    }
    if owner != (stat.st_uid, stat.st_gid) {
        concerns.push(StagedInstallationConcern::ProjectWrongOwner);
    }
    if stat.st_size < 0 || stat.st_size as u64 > MAX_STATE_DOCUMENT_BYTES as u64 {
        concerns.push(StagedInstallationConcern::ProjectOversized);
    }
    if concerns.iter().any(|concern| concern.is_suspicious()) {
        return;
    }

    let bytes = match read_bounded(project) {
        Ok(bytes) => bytes,
        Err(_) => {
            concerns.push(StagedInstallationConcern::InspectionFailed);
            return;
        }
    };
    let input = match std::str::from_utf8(&bytes) {
        Ok(input) => input,
        Err(_) => {
            concerns.push(StagedInstallationConcern::ProjectInvalidUtf8);
            return;
        }
    };
    let document = match decode_state_document(input) {
        Ok(StateDocument::Project(document)) => document,
        Ok(StateDocument::Resource(_)) | Err(_) => {
            concerns.push(StagedInstallationConcern::ProjectInvalidDocument);
            return;
        }
    };
    if expected_id.is_none_or(|expected| document.installation_id() != expected) {
        concerns.push(StagedInstallationConcern::ProjectIdMismatch);
    }
}

fn read_bounded(file: OwnedFd) -> Result<Vec<u8>, StateStoreError> {
    let file = File::from(file);
    let mut reader: Take<File> = file.take((MAX_STATE_DOCUMENT_BYTES + 1) as u64);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|_| io_error("could not read staged project state"))?;
    if bytes.len() > MAX_STATE_DOCUMENT_BYTES {
        return Err(corrupt_error("staged project state exceeds the size limit"));
    }
    Ok(bytes)
}

fn finish_finding(
    name: &[u8],
    concerns: Vec<StagedInstallationConcern>,
) -> StagedInstallationFinding {
    let disposition = if concerns.is_empty() {
        StagedInstallationDisposition::CompleteOrphan
    } else if concerns.iter().any(|concern| concern.is_suspicious()) {
        StagedInstallationDisposition::Suspicious
    } else {
        StagedInstallationDisposition::Incomplete
    };
    StagedInstallationFinding {
        name: public_name(name),
        disposition,
        concerns,
    }
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
    inspect_managed_directory(&directory, subject, None)?;
    Ok(directory)
}

fn open_optional_directory_at(
    parent: &OwnedFd,
    name: &str,
    subject: &str,
) -> Result<Option<OwnedFd>, StateStoreError> {
    match fs::openat(parent, name, DIRECTORY_FLAGS, Mode::empty()) {
        Ok(directory) => Ok(Some(directory)),
        Err(Errno::NOENT) => Ok(None),
        Err(error) => Err(map_directory_open_error(error, subject)),
    }
}

fn inspect_managed_directory(
    directory: &OwnedFd,
    subject: &str,
    expected_owner: Option<(u32, u32)>,
) -> Result<rustix::fs::Stat, StateStoreError> {
    let stat = fs::fstat(directory)
        .map_err(|_| io_error(format!("could not inspect {subject}")))?;
    if !FileType::from_raw_mode(stat.st_mode).is_dir() {
        return Err(unsafe_error(format!("{subject} is not a directory")));
    }
    if stat.st_mode & 0o7777 != 0o750 {
        return Err(unsafe_error(format!("{subject} does not have mode 0750")));
    }
    if expected_owner.is_some_and(|owner| owner != (stat.st_uid, stat.st_gid)) {
        return Err(unsafe_error(format!(
            "{subject} has an unexpected owner or group"
        )));
    }
    Ok(stat)
}

fn map_directory_open_error(error: Errno, subject: &str) -> StateStoreError {
    match error {
        Errno::LOOP | Errno::NOTDIR => unsafe_error(format!("{subject} is symlinked or invalid")),
        Errno::NOENT => io_error(format!("{subject} does not exist")),
        _ => io_error(format!("could not open {subject}")),
    }
}

fn io_error(message: impl Into<String>) -> StateStoreError {
    StateStoreError::public(StateStoreErrorKind::Io, message)
}

fn corrupt_error(message: impl Into<String>) -> StateStoreError {
    StateStoreError::public(StateStoreErrorKind::CorruptState, message)
}

fn unsafe_error(message: impl Into<String>) -> StateStoreError {
    StateStoreError::public(StateStoreErrorKind::UnsafeFilesystem, message)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::manifest::RunnerScope;
    use crate::ownership::ProjectIdentity;
    use crate::state::InstallationId;
    use crate::state_document::{ProjectStateDocument, StateDocument, encode_state_document};
    use crate::state_store::StateStoreErrorKind;

    use super::{
        JOURNALS_DIRECTORY, PROJECT_FILE, RESOURCES_DIRECTORY, STAGING_DIRECTORY,
        StagedInstallationConcern, StagedInstallationDisposition, inspect_staged_installations,
    };

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(label: &str) -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "smolrunner-staging-recovery-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create temporary state root");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o750))
                .expect("set state-root mode");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn id(value: &str) -> InstallationId {
        InstallationId::parse(value).expect("installation ID")
    }

    fn project(repository: &str) -> ProjectIdentity {
        ProjectIdentity {
            repository: repository.to_owned(),
            runner_scope: RunnerScope::Repository,
            runner_user: "project-runner".to_owned(),
        }
    }

    fn create_complete_stage(
        root: &Path,
        directory_id: &InstallationId,
        document_id: InstallationId,
    ) -> PathBuf {
        let stage = root
            .join(STAGING_DIRECTORY)
            .join(directory_id.as_str());
        fs::create_dir_all(stage.join(RESOURCES_DIRECTORY)).expect("create resources directory");
        fs::create_dir_all(stage.join(JOURNALS_DIRECTORY)).expect("create journals directory");
        for directory in [
            root.join(STAGING_DIRECTORY),
            stage.clone(),
            stage.join(RESOURCES_DIRECTORY),
            stage.join(JOURNALS_DIRECTORY),
        ] {
            fs::set_permissions(directory, fs::Permissions::from_mode(0o750))
                .expect("set staged directory mode");
        }
        let document = ProjectStateDocument::new(document_id, project("example/project"))
            .expect("project document");
        let encoded = encode_state_document(&StateDocument::Project(document))
            .expect("encode project document");
        let project_path = stage.join(PROJECT_FILE);
        fs::write(&project_path, encoded).expect("write staged project document");
        fs::set_permissions(&project_path, fs::Permissions::from_mode(0o600))
            .expect("set staged project mode");
        stage
    }

    #[test]
    fn missing_staging_directory_returns_an_empty_report() {
        let root = TempRoot::new("missing");
        let report = inspect_staged_installations(root.path()).expect("inspect missing staging");
        assert!(report.findings().is_empty());
        assert!(!report.truncated());
    }

    #[test]
    fn complete_staged_installation_is_reported_without_mutation() {
        let root = TempRoot::new("complete");
        let installation_id = id("1111111111111111");
        let stage = create_complete_stage(root.path(), &installation_id, installation_id.clone());

        let report = inspect_staged_installations(root.path()).expect("inspect complete staging");
        assert_eq!(report.findings().len(), 1);
        let finding = &report.findings()[0];
        assert_eq!(finding.name(), installation_id.as_str());
        assert_eq!(
            finding.disposition(),
            StagedInstallationDisposition::CompleteOrphan
        );
        assert!(finding.concerns().is_empty());
        assert!(stage.exists());
    }

    #[test]
    fn trusted_missing_content_is_incomplete() {
        let root = TempRoot::new("incomplete");
        let installation_id = id("2222222222222222");
        let stage = create_complete_stage(root.path(), &installation_id, installation_id.clone());
        fs::remove_file(stage.join(PROJECT_FILE)).expect("remove staged project");
        fs::remove_dir(stage.join(RESOURCES_DIRECTORY)).expect("remove resources directory");

        let report = inspect_staged_installations(root.path()).expect("inspect incomplete staging");
        let finding = &report.findings()[0];
        assert_eq!(
            finding.disposition(),
            StagedInstallationDisposition::Incomplete
        );
        assert!(
            finding
                .concerns()
                .contains(&StagedInstallationConcern::MissingProject)
        );
        assert!(
            finding
                .concerns()
                .contains(&StagedInstallationConcern::MissingResources)
        );
    }

    #[test]
    fn project_id_mismatch_is_incomplete_and_never_promoted() {
        let root = TempRoot::new("mismatch");
        let directory_id = id("3333333333333333");
        create_complete_stage(
            root.path(),
            &directory_id,
            id("4444444444444444"),
        );

        let report = inspect_staged_installations(root.path()).expect("inspect mismatched staging");
        let finding = &report.findings()[0];
        assert_eq!(
            finding.disposition(),
            StagedInstallationDisposition::Incomplete
        );
        assert!(
            finding
                .concerns()
                .contains(&StagedInstallationConcern::ProjectIdMismatch)
        );
    }

    #[test]
    fn malformed_symlinked_or_broad_entries_are_suspicious() {
        let root = TempRoot::new("suspicious");
        let staging = root.path().join(STAGING_DIRECTORY);
        fs::create_dir(&staging).expect("create staging directory");
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o750))
            .expect("set staging mode");

        let malformed = staging.join("INVALID");
        fs::create_dir(&malformed).expect("create malformed stage");
        fs::set_permissions(&malformed, fs::Permissions::from_mode(0o750))
            .expect("set malformed stage mode");

        let broad_id = id("5555555555555555");
        let broad = staging.join(broad_id.as_str());
        fs::create_dir(&broad).expect("create broad stage");
        fs::set_permissions(&broad, fs::Permissions::from_mode(0o755))
            .expect("set broad stage mode");

        let outside = TempRoot::new("outside");
        let link_id = id("6666666666666666");
        symlink(outside.path(), staging.join(link_id.as_str())).expect("create staged symlink");

        let report = inspect_staged_installations(root.path()).expect("inspect suspicious staging");
        assert_eq!(report.findings().len(), 3);
        assert!(report.findings().iter().all(|finding| {
            finding.disposition() == StagedInstallationDisposition::Suspicious
        }));
    }

    #[test]
    fn unsafe_fixed_staging_directory_fails_closed() {
        let root = TempRoot::new("unsafe-fixed");
        let staging = root.path().join(STAGING_DIRECTORY);
        fs::create_dir(&staging).expect("create staging directory");
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o755))
            .expect("broaden staging mode");

        let error = inspect_staged_installations(root.path())
            .expect_err("broad fixed staging directory must fail");
        assert_eq!(error.kind(), StateStoreErrorKind::UnsafeFilesystem);
    }
}
