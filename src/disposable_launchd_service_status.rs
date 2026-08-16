//! Read-only exact installed-state observation for the disposable-worker user LaunchAgent.
//!
//! This module consumes the public v1 LaunchAgent plan identity as its expected-state commitment.
//! It never acquires the apply lock or mutates launchd/filesystem state. Exact configuration bytes
//! are proven by recomputing the installed-plan identity from the observed plist and comparing it
//! with the production planner's public identity; the loaded job is then matched against the same
//! explicit program/enrollment inputs through one bounded `launchctl print` observation.

use std::fmt;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::fd::OwnedFd;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use rustix::fs::{self as rustix_fs, AtFlags, FileType, Mode, OFlags};
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::disposable_launchd_service::{
    DISPOSABLE_LAUNCHD_SERVICE_LABEL, DisposableLaunchdServiceDesiredState,
    plan_disposable_launchd_service,
};
use crate::process::{CommandSpec, ExecutionRecord, TimedCommandExecutor};

pub const DISPOSABLE_LAUNCHD_SERVICE_STATUS_SCHEMA_VERSION: u8 = 2;
const MAX_UID: u32 = 2_147_483_647;
const MAX_PLIST_BYTES: u64 = 64 * 1024;
const LAUNCHCTL_TIMEOUT: Duration = Duration::from_secs(15);
const LAUNCHCTL_PROGRAM: &str = "/bin/launchctl";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableLaunchdServiceObservedState {
    Absent,
    ConfigurationOnly,
    LoadedExact,
    LoadedWithoutConfiguration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableLaunchdServiceRuntimeHealth {
    NotRunning,
    Unknown,
}

impl DisposableLaunchdServiceRuntimeHealth {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotRunning => "not_running",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableLaunchdServiceRemediation {
    None,
    PlanAndApplyInstall,
    InspectUnsafeState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DisposableLaunchdServiceStatusReport {
    schema_version: u8,
    state: DisposableLaunchdServiceObservedState,
    service_label: &'static str,
    service_scope: &'static str,
    launchd_domain: String,
    plan_identity: Sha256Digest,
    configuration_present: bool,
    service_loaded: bool,
    runtime_health: DisposableLaunchdServiceRuntimeHealth,
    #[serde(rename = "installation_remediation")]
    remediation: DisposableLaunchdServiceRemediation,
}

impl DisposableLaunchdServiceStatusReport {
    #[must_use]
    pub const fn state(&self) -> DisposableLaunchdServiceObservedState {
        self.state
    }

    #[must_use]
    pub fn launchd_domain(&self) -> &str {
        &self.launchd_domain
    }

    #[must_use]
    pub fn plan_identity(&self) -> &Sha256Digest {
        &self.plan_identity
    }

    #[must_use]
    pub const fn configuration_present(&self) -> bool {
        self.configuration_present
    }

    #[must_use]
    pub const fn service_loaded(&self) -> bool {
        self.service_loaded
    }

    #[must_use]
    pub const fn runtime_health(&self) -> DisposableLaunchdServiceRuntimeHealth {
        self.runtime_health
    }

    #[must_use]
    pub const fn installation_remediation(&self) -> DisposableLaunchdServiceRemediation {
        self.remediation
    }

    #[must_use]
    pub const fn remediation(&self) -> DisposableLaunchdServiceRemediation {
        self.remediation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableLaunchdServiceStatusErrorKind {
    InvalidConfiguration,
    OperatorMismatch,
    UnsafeState,
    ObservationFailed,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DisposableLaunchdServiceStatusError {
    kind: DisposableLaunchdServiceStatusErrorKind,
    code: &'static str,
}

impl DisposableLaunchdServiceStatusError {
    #[must_use]
    pub const fn kind(self) -> DisposableLaunchdServiceStatusErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for DisposableLaunchdServiceStatusError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableLaunchdServiceStatusError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .finish()
    }
}

impl fmt::Display for DisposableLaunchdServiceStatusError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("disposable-worker LaunchAgent status is unavailable")
    }
}

impl std::error::Error for DisposableLaunchdServiceStatusError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaunchdObservation {
    Absent,
    Exact,
}

/// Inspect one exact expected installed LaunchAgent without changing host state.
///
/// The expected configuration is rebuilt through the production planner. The observed plist must
/// reproduce that exact public installed-plan identity, and any loaded job must match the exact
/// path/program/argument identity. Current program and enrollment bytes are deliberately not read:
/// status remains useful while an installation is being recovered or prepared for exact removal.
///
/// # Errors
///
/// Returns a bounded path-free error when the operator identity, filesystem evidence, plan inputs,
/// launchd identity, or observation command cannot be proven exactly.
pub fn inspect_disposable_launchd_service_status(
    operator_uid: u32,
    operator_home: &Path,
    program: &Path,
    program_digest: &Sha256Digest,
    enrollment: &Path,
    enrollment_digest: &Sha256Digest,
    executor: &impl TimedCommandExecutor,
) -> Result<DisposableLaunchdServiceStatusReport, DisposableLaunchdServiceStatusError> {
    use rustix::process::{getegid, geteuid};

    if operator_uid == 0
        || operator_uid > MAX_UID
        || geteuid().is_root()
        || geteuid().as_raw() != operator_uid
    {
        return Err(status_error(
            DisposableLaunchdServiceStatusErrorKind::OperatorMismatch,
            "disposable_launchd_service_status_operator_mismatch",
        ));
    }

    let plan = plan_disposable_launchd_service(
        DisposableLaunchdServiceDesiredState::Installed,
        operator_uid,
        operator_home,
        program,
        program_digest,
        enrollment,
        enrollment_digest,
    )
    .map_err(|_| {
        status_error(
            DisposableLaunchdServiceStatusErrorKind::InvalidConfiguration,
            "disposable_launchd_service_status_configuration_invalid",
        )
    })?;
    let launch_agent = launch_agent_path(operator_home);
    let operator_gid = getegid().as_raw();
    let configuration_present = observe_exact_configuration(
        operator_uid,
        operator_gid,
        operator_home,
        &launch_agent,
        program_digest,
        enrollment_digest,
        plan.report().plan_identity(),
    )?;
    let launchd = observe_launchd_service(
        plan.report().launchd_domain(),
        &launch_agent,
        program,
        program_digest,
        enrollment,
        enrollment_digest,
        executor,
    )?;
    let service_loaded = launchd == LaunchdObservation::Exact;
    let (state, runtime_health, remediation) = classify_status(configuration_present, launchd);

    Ok(DisposableLaunchdServiceStatusReport {
        schema_version: DISPOSABLE_LAUNCHD_SERVICE_STATUS_SCHEMA_VERSION,
        state,
        service_label: DISPOSABLE_LAUNCHD_SERVICE_LABEL,
        service_scope: "user_launch_agent",
        launchd_domain: plan.report().launchd_domain().to_owned(),
        plan_identity: plan.report().plan_identity().clone(),
        configuration_present,
        service_loaded,
        runtime_health,
        remediation,
    })
}

fn classify_status(
    configuration_present: bool,
    launchd: LaunchdObservation,
) -> (
    DisposableLaunchdServiceObservedState,
    DisposableLaunchdServiceRuntimeHealth,
    DisposableLaunchdServiceRemediation,
) {
    match (configuration_present, launchd) {
        (false, LaunchdObservation::Absent) => (
            DisposableLaunchdServiceObservedState::Absent,
            DisposableLaunchdServiceRuntimeHealth::NotRunning,
            DisposableLaunchdServiceRemediation::PlanAndApplyInstall,
        ),
        (true, LaunchdObservation::Absent) => (
            DisposableLaunchdServiceObservedState::ConfigurationOnly,
            DisposableLaunchdServiceRuntimeHealth::NotRunning,
            DisposableLaunchdServiceRemediation::PlanAndApplyInstall,
        ),
        (true, LaunchdObservation::Exact) => (
            DisposableLaunchdServiceObservedState::LoadedExact,
            DisposableLaunchdServiceRuntimeHealth::Unknown,
            DisposableLaunchdServiceRemediation::None,
        ),
        (false, LaunchdObservation::Exact) => (
            DisposableLaunchdServiceObservedState::LoadedWithoutConfiguration,
            DisposableLaunchdServiceRuntimeHealth::Unknown,
            DisposableLaunchdServiceRemediation::InspectUnsafeState,
        ),
    }
}

fn launch_agent_path(operator_home: &Path) -> PathBuf {
    operator_home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{DISPOSABLE_LAUNCHD_SERVICE_LABEL}.plist"))
}

fn observe_exact_configuration(
    operator_uid: u32,
    operator_gid: u32,
    operator_home: &Path,
    launch_agent: &Path,
    program_digest: &Sha256Digest,
    enrollment_digest: &Sha256Digest,
    expected_plan_identity: &Sha256Digest,
) -> Result<bool, DisposableLaunchdServiceStatusError> {
    let Some(directory) = open_launch_agent_directory(operator_uid, operator_gid, operator_home)?
    else {
        return Ok(false);
    };
    let name = launch_agent.file_name().ok_or_else(unsafe_state)?;
    let held = match rustix_fs::openat(&directory, name, input_flags(), Mode::empty()) {
        Ok(held) => held,
        Err(rustix::io::Errno::NOENT) => return Ok(false),
        Err(_) => return Err(unsafe_state()),
    };
    let mut file = File::from(held);
    let before = rustix_fs::fstat(&file).map_err(|_| unsafe_state())?;
    if !FileType::from_raw_mode(before.st_mode).is_file()
        || before.st_nlink != 1
        || before.st_uid != operator_uid
        || before.st_gid != operator_gid
        || before.st_mode & 0o7777 != 0o600
        || before.st_size <= 0
        || u64::try_from(before.st_size)
            .ok()
            .is_none_or(|size| size > MAX_PLIST_BYTES)
    {
        return Err(unsafe_state());
    }
    let path_before = rustix_fs::statat(&directory, name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| unsafe_state())?;
    if !same_file(&before, &path_before) {
        return Err(unsafe_state());
    }
    let bytes = read_bounded(&mut file)?;
    file.seek(SeekFrom::Start(0)).map_err(|_| unsafe_state())?;
    let confirmation = read_bounded(&mut file)?;
    let after = rustix_fs::fstat(&file).map_err(|_| unsafe_state())?;
    let path_after = rustix_fs::statat(&directory, name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| unsafe_state())?;
    if confirmation != bytes || !same_file(&before, &after) || !same_file(&before, &path_after) {
        return Err(unsafe_state());
    }
    verify_launch_agent_directory(operator_uid, operator_gid, operator_home, &directory)?;

    let observed_identity = observed_installed_plan_identity(
        operator_uid,
        launch_agent,
        &bytes,
        program_digest,
        enrollment_digest,
    )?;
    if &observed_identity != expected_plan_identity {
        return Err(status_error(
            DisposableLaunchdServiceStatusErrorKind::UnsafeState,
            "disposable_launchd_service_status_configuration_mismatch",
        ));
    }
    Ok(true)
}

fn open_launch_agent_directory(
    operator_uid: u32,
    operator_gid: u32,
    operator_home: &Path,
) -> Result<Option<OwnedFd>, DisposableLaunchdServiceStatusError> {
    let path = operator_home.join("Library").join("LaunchAgents");
    let mut directory =
        rustix_fs::open("/", directory_flags(), Mode::empty()).map_err(|_| unsafe_state())?;
    inspect_directory(
        &rustix_fs::fstat(&directory).map_err(|_| unsafe_state())?,
        operator_uid,
    )?;

    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                directory =
                    match rustix_fs::openat(&directory, name, directory_flags(), Mode::empty()) {
                        Ok(directory) => directory,
                        Err(rustix::io::Errno::NOENT) => return Ok(None),
                        Err(_) => return Err(unsafe_state()),
                    };
                inspect_directory(
                    &rustix_fs::fstat(&directory).map_err(|_| unsafe_state())?,
                    operator_uid,
                )?;
            }
            _ => return Err(unsafe_state()),
        }
    }

    let stat = rustix_fs::fstat(&directory).map_err(|_| unsafe_state())?;
    if stat.st_uid != operator_uid || stat.st_gid != operator_gid || stat.st_mode & 0o022 != 0 {
        return Err(unsafe_state());
    }
    let resolved = rustix_fs::stat(&path).map_err(|_| unsafe_state())?;
    if !same_directory(&stat, &resolved) {
        return Err(unsafe_state());
    }
    Ok(Some(directory))
}

fn verify_launch_agent_directory(
    operator_uid: u32,
    operator_gid: u32,
    operator_home: &Path,
    directory: &OwnedFd,
) -> Result<(), DisposableLaunchdServiceStatusError> {
    let held = rustix_fs::fstat(directory).map_err(|_| unsafe_state())?;
    if held.st_uid != operator_uid || held.st_gid != operator_gid || held.st_mode & 0o022 != 0 {
        return Err(unsafe_state());
    }
    let path = operator_home.join("Library").join("LaunchAgents");
    let resolved = rustix_fs::stat(&path).map_err(|_| unsafe_state())?;
    if !same_directory(&held, &resolved) {
        return Err(unsafe_state());
    }
    Ok(())
}

fn inspect_directory(
    stat: &rustix_fs::Stat,
    operator_uid: u32,
) -> Result<(), DisposableLaunchdServiceStatusError> {
    if !FileType::from_raw_mode(stat.st_mode).is_dir()
        || (stat.st_uid != 0 && stat.st_uid != operator_uid)
        || stat.st_mode & 0o022 != 0
    {
        return Err(unsafe_state());
    }
    Ok(())
}

fn observe_launchd_service(
    launchd_domain: &str,
    launch_agent: &Path,
    program: &Path,
    program_digest: &Sha256Digest,
    enrollment: &Path,
    enrollment_digest: &Sha256Digest,
    executor: &impl TimedCommandExecutor,
) -> Result<LaunchdObservation, DisposableLaunchdServiceStatusError> {
    let target = format!("{launchd_domain}/{DISPOSABLE_LAUNCHD_SERVICE_LABEL}");
    let spec = CommandSpec::new(LAUNCHCTL_PROGRAM)
        .argument("print")
        .argument(&target);
    let record = executor
        .execute_with_timeout(&spec, LAUNCHCTL_TIMEOUT)
        .map_err(|_| observation_failed())?;
    classify_launchctl_print(
        record,
        &target,
        launch_agent,
        program,
        program_digest,
        enrollment,
        enrollment_digest,
    )
}

fn classify_launchctl_print(
    record: ExecutionRecord,
    target: &str,
    launch_agent: &Path,
    program: &Path,
    program_digest: &Sha256Digest,
    enrollment: &Path,
    enrollment_digest: &Sha256Digest,
) -> Result<LaunchdObservation, DisposableLaunchdServiceStatusError> {
    if record.success {
        let expected_header = format!("{target} = {{\n");
        let expected_path = format!("\n\tpath = {}\n", private_text(launch_agent)?);
        let expected_program = format!("\n\tprogram = {}\n", private_text(program)?);
        let expected_arguments = format!(
            "\n\targuments = {{\n\t\t{}\n\t\tworker\n\t\tserve\n\t\t--program-digest\n\t\t{}\n\t\t--enrollment\n\t\t{}\n\t\t--enrollment-digest\n\t\t{}\n\t}}\n",
            private_text(program)?,
            program_digest.as_str(),
            private_text(enrollment)?,
            enrollment_digest.as_str(),
        );
        if record.stdout.starts_with(&expected_header)
            && record.stdout.contains(&expected_path)
            && record.stdout.contains("\n\ttype = LaunchAgent\n")
            && record.stdout.contains(&expected_program)
            && record.stdout.contains(&expected_arguments)
        {
            return Ok(LaunchdObservation::Exact);
        }
        return Err(status_error(
            DisposableLaunchdServiceStatusErrorKind::UnsafeState,
            "disposable_launchd_service_status_identity_mismatch",
        ));
    }
    if record.status == Some(113) {
        return Ok(LaunchdObservation::Absent);
    }
    Err(observation_failed())
}

fn observed_installed_plan_identity(
    operator_uid: u32,
    launch_agent: &Path,
    plist: &[u8],
    program_digest: &Sha256Digest,
    enrollment_digest: &Sha256Digest,
) -> Result<Sha256Digest, DisposableLaunchdServiceStatusError> {
    let target = private_text(launch_agent)?.as_bytes();
    let mut hasher = Sha256::new();
    hasher.update(b"smolrunner.disposable-launchd-service-plan.v1\0");
    hasher.update([1]);
    hasher.update(operator_uid.to_be_bytes());
    hasher.update((target.len() as u64).to_be_bytes());
    hasher.update(target);
    hasher.update((plist.len() as u64).to_be_bytes());
    hasher.update(plist);
    hasher.update((program_digest.as_str().len() as u64).to_be_bytes());
    hasher.update(program_digest.as_str().as_bytes());
    hasher.update((enrollment_digest.as_str().len() as u64).to_be_bytes());
    hasher.update(enrollment_digest.as_str().as_bytes());
    Sha256Digest::parse(&format!("sha256:{:x}", hasher.finalize())).map_err(|_| unsafe_state())
}

fn read_bounded(file: &mut File) -> Result<Vec<u8>, DisposableLaunchdServiceStatusError> {
    let mut bytes = Vec::new();
    file.take(MAX_PLIST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unsafe_state())?;
    if u64::try_from(bytes.len())
        .ok()
        .is_none_or(|size| size > MAX_PLIST_BYTES)
    {
        return Err(unsafe_state());
    }
    Ok(bytes)
}

const fn directory_flags() -> OFlags {
    OFlags::RDONLY
        .union(OFlags::DIRECTORY)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC)
}

const fn input_flags() -> OFlags {
    OFlags::RDONLY
        .union(OFlags::NOFOLLOW)
        .union(OFlags::NONBLOCK)
        .union(OFlags::CLOEXEC)
}

fn private_text(path: &Path) -> Result<&str, DisposableLaunchdServiceStatusError> {
    path.to_str().ok_or_else(unsafe_state)
}

fn same_file(left: &rustix_fs::Stat, right: &rustix_fs::Stat) -> bool {
    left.st_dev == right.st_dev
        && left.st_ino == right.st_ino
        && left.st_mode == right.st_mode
        && left.st_nlink == right.st_nlink
        && left.st_uid == right.st_uid
        && left.st_gid == right.st_gid
        && left.st_size == right.st_size
        && left.st_mtime == right.st_mtime
        && left.st_mtime_nsec == right.st_mtime_nsec
        && left.st_ctime == right.st_ctime
        && left.st_ctime_nsec == right.st_ctime_nsec
}

fn same_directory(left: &rustix_fs::Stat, right: &rustix_fs::Stat) -> bool {
    left.st_dev == right.st_dev
        && left.st_ino == right.st_ino
        && left.st_mode == right.st_mode
        && left.st_uid == right.st_uid
        && left.st_gid == right.st_gid
}

const fn unsafe_state() -> DisposableLaunchdServiceStatusError {
    status_error(
        DisposableLaunchdServiceStatusErrorKind::UnsafeState,
        "disposable_launchd_service_status_unsafe_state",
    )
}

const fn observation_failed() -> DisposableLaunchdServiceStatusError {
    status_error(
        DisposableLaunchdServiceStatusErrorKind::ObservationFailed,
        "disposable_launchd_service_status_observation_failed",
    )
}

const fn status_error(
    kind: DisposableLaunchdServiceStatusErrorKind,
    code: &'static str,
) -> DisposableLaunchdServiceStatusError {
    DisposableLaunchdServiceStatusError { kind, code }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(character: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", character.to_string().repeat(64))).unwrap()
    }

    #[test]
    fn observed_identity_binds_every_public_v1_component() {
        let path =
            Path::new("/Users/operator/Library/LaunchAgents/io.smolrunner.disposable-worker.plist");
        let baseline =
            observed_installed_plan_identity(501, path, b"plist-one", &digest('a'), &digest('b'))
                .unwrap();
        assert_ne!(
            baseline,
            observed_installed_plan_identity(502, path, b"plist-one", &digest('a'), &digest('b'))
                .unwrap()
        );
        assert_ne!(
            baseline,
            observed_installed_plan_identity(
                501,
                Path::new("/Users/operator/Library/LaunchAgents/other.plist"),
                b"plist-one",
                &digest('a'),
                &digest('b'),
            )
            .unwrap()
        );
        assert_ne!(
            baseline,
            observed_installed_plan_identity(501, path, b"plist-two", &digest('a'), &digest('b'))
                .unwrap()
        );
        assert_ne!(
            baseline,
            observed_installed_plan_identity(501, path, b"plist-one", &digest('c'), &digest('b'))
                .unwrap()
        );
        assert_ne!(
            baseline,
            observed_installed_plan_identity(501, path, b"plist-one", &digest('a'), &digest('d'))
                .unwrap()
        );
    }

    #[test]
    fn loaded_identity_never_claims_runtime_health() {
        let (state, runtime_health, remediation) = classify_status(true, LaunchdObservation::Exact);
        assert_eq!(state, DisposableLaunchdServiceObservedState::LoadedExact);
        assert_eq!(
            runtime_health,
            DisposableLaunchdServiceRuntimeHealth::Unknown
        );
        assert_eq!(remediation, DisposableLaunchdServiceRemediation::None);

        let (state, runtime_health, _) = classify_status(true, LaunchdObservation::Absent);
        assert_eq!(
            state,
            DisposableLaunchdServiceObservedState::ConfigurationOnly
        );
        assert_eq!(
            runtime_health,
            DisposableLaunchdServiceRuntimeHealth::NotRunning
        );
    }

    #[test]
    fn public_report_and_errors_are_path_free() {
        let report = DisposableLaunchdServiceStatusReport {
            schema_version: DISPOSABLE_LAUNCHD_SERVICE_STATUS_SCHEMA_VERSION,
            state: DisposableLaunchdServiceObservedState::LoadedExact,
            service_label: DISPOSABLE_LAUNCHD_SERVICE_LABEL,
            service_scope: "user_launch_agent",
            launchd_domain: "gui/501".to_owned(),
            plan_identity: digest('a'),
            configuration_present: true,
            service_loaded: true,
            runtime_health: DisposableLaunchdServiceRuntimeHealth::Unknown,
            remediation: DisposableLaunchdServiceRemediation::None,
        };
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"schema_version\":2"));
        assert!(json.contains("\"state\":\"loaded_exact\""));
        assert!(json.contains("\"runtime_health\":\"unknown\""));
        assert!(json.contains("\"installation_remediation\":\"none\""));
        assert!(!json.contains("running_exact"));
        assert!(!json.contains("\"remediation\":"));
        assert!(!json.contains("/Users/"));
        assert!(!json.contains("/opt/"));
        assert!(!format!("{:?}", unsafe_state()).contains("/Users/"));
    }
}
