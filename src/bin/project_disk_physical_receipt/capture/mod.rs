use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustix::fs;
use serde::Serialize;

use crate::receipt::{
    DeclaredProjectDiskBindingDocument, PROJECT_DISK_PHYSICAL_RECEIPT_SCHEMA_VERSION,
    ProjectDiskDirectoryEvidenceDocument, ProjectDiskGuestEvidenceDocument,
    ProjectDiskLimaEvidenceDocument, ProjectDiskPhysicalReceipt,
    ProjectDiskPhysicalReceiptDocument, ReceiptCommandEvidence, same_entry_binding,
    valid_git_commit, validate_absolute_path, validate_locator,
};
use smolrunner::lima_host_identity::LimaHostIdentityAdapter;
use smolrunner::lima_observation::{
    LimaArchitecture, LimaInstanceName, LimaObservationRequest, LimaVmType,
};
use smolrunner::process::{CommandSpec, ExecutionRecord, TimedCommandExecutor};
use smolrunner::project_catalog::ProjectIdentity;
use smolrunner::project_disk_lease::{
    ProjectDiskAttachmentGeneration, ProjectDiskGeneration, ProjectDiskId, ProjectDiskRevision,
    ResidentSandboxGeneration, ResidentSandboxId,
};

mod fs_evidence;

use fs_evidence::{
    capture_held_disk_directory, open_absolute_directory, open_relative_directory, snapshot,
};

const DEFAULT_OBSERVATION_AGE_SECONDS: u64 = 30;
const MAX_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, PartialEq, Eq)]
pub struct ProjectDiskPhysicalCaptureRequest {
    repo_commit: String,
    lima_home: PathBuf,
    disk_directory: PathBuf,
    disk_name: String,
    resident_sandbox_instance: LimaInstanceName,
    guest_project_mount: PathBuf,
    guest_cache_path: PathBuf,
    limactl_program: PathBuf,
    project_identity: ProjectIdentity,
    project_disk_id: ProjectDiskId,
    project_disk_generation: ProjectDiskGeneration,
    project_disk_revision: ProjectDiskRevision,
    attachment_generation: ProjectDiskAttachmentGeneration,
    resident_sandbox_id: ResidentSandboxId,
    resident_sandbox_generation: ResidentSandboxGeneration,
    command_timeout: Duration,
}

impl ProjectDiskPhysicalCaptureRequest {
    /// Define one explicit operator-Mac physical observation transaction.
    ///
    /// The disk directory is mandatory. This constructor never derives a `_disks` path from the
    /// Lima disk name, so the first physical slice cannot encode remembered standalone-disk layout.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for invalid paths, locators, generations, or command timeout.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        repo_commit: impl Into<String>,
        lima_home: impl Into<PathBuf>,
        disk_directory: impl Into<PathBuf>,
        disk_name: impl Into<String>,
        resident_sandbox_instance: impl AsRef<str>,
        guest_project_mount: impl Into<PathBuf>,
        guest_cache_path: impl Into<PathBuf>,
        limactl_program: impl Into<PathBuf>,
        project_identity: impl AsRef<str>,
        project_disk_id: impl AsRef<str>,
        project_disk_generation: u64,
        project_disk_revision: u64,
        attachment_generation: u64,
        resident_sandbox_id: impl AsRef<str>,
        resident_sandbox_generation: u64,
        command_timeout: Duration,
    ) -> Result<Self, ProjectDiskPhysicalCaptureError> {
        let repo_commit = repo_commit.into();
        if !valid_git_commit(&repo_commit) {
            return Err(invalid_request());
        }

        let lima_home = validate_path(lima_home.into())?;
        let disk_directory = validate_path(disk_directory.into())?;
        if disk_directory == lima_home || disk_directory.strip_prefix(&lima_home).is_err() {
            return Err(invalid_request());
        }
        let guest_project_mount = validate_path(guest_project_mount.into())?;
        let guest_cache_path = validate_path(guest_cache_path.into())?;
        let limactl_program = validate_path(limactl_program.into())?;

        let disk_name = disk_name.into();
        validate_locator(&disk_name, "disk_name").map_err(|_| invalid_request())?;
        let resident_sandbox_instance = LimaInstanceName::parse(resident_sandbox_instance.as_ref())
            .map_err(|_| invalid_request())?;
        let project_identity =
            ProjectIdentity::parse(project_identity.as_ref()).map_err(|_| invalid_request())?;
        let project_disk_id =
            ProjectDiskId::parse(project_disk_id.as_ref()).map_err(|_| invalid_request())?;
        let project_disk_generation =
            ProjectDiskGeneration::new(project_disk_generation).map_err(|_| invalid_request())?;
        let project_disk_revision =
            ProjectDiskRevision::new(project_disk_revision).map_err(|_| invalid_request())?;
        let attachment_generation = ProjectDiskAttachmentGeneration::new(attachment_generation)
            .map_err(|_| invalid_request())?;
        let resident_sandbox_id = ResidentSandboxId::parse(resident_sandbox_id.as_ref())
            .map_err(|_| invalid_request())?;
        let resident_sandbox_generation =
            ResidentSandboxGeneration::new(resident_sandbox_generation)
                .map_err(|_| invalid_request())?;
        if command_timeout.is_zero() || command_timeout > MAX_COMMAND_TIMEOUT {
            return Err(invalid_request());
        }

        Ok(Self {
            repo_commit,
            lima_home,
            disk_directory,
            disk_name,
            resident_sandbox_instance,
            guest_project_mount,
            guest_cache_path,
            limactl_program,
            project_identity,
            project_disk_id,
            project_disk_generation,
            project_disk_revision,
            attachment_generation,
            resident_sandbox_id,
            resident_sandbox_generation,
            command_timeout,
        })
    }
}

impl fmt::Debug for ProjectDiskPhysicalCaptureRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectDiskPhysicalCaptureRequest")
            .field("repo_commit", &self.repo_commit)
            .field("disk_name", &self.disk_name)
            .field("resident_sandbox_instance", &self.resident_sandbox_instance)
            .field("project_identity", &self.project_identity)
            .field("project_disk_id", &self.project_disk_id)
            .field("project_disk_generation", &self.project_disk_generation)
            .field("project_disk_revision", &self.project_disk_revision)
            .field("attachment_generation", &self.attachment_generation)
            .field("resident_sandbox_id", &self.resident_sandbox_id)
            .field(
                "resident_sandbox_generation",
                &self.resident_sandbox_generation,
            )
            .field(
                "private_paths",
                &"<private-project-disk-physical-capture-paths>",
            )
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDiskPhysicalCaptureErrorKind {
    InvalidRequest,
    Clock,
    LimaHostIdentity,
    Filesystem,
    Command,
    ChangedDuringObservation,
    Receipt,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiskPhysicalCaptureError {
    kind: ProjectDiskPhysicalCaptureErrorKind,
    code: &'static str,
    message: &'static str,
}

impl ProjectDiskPhysicalCaptureError {
    #[must_use]
    pub const fn kind(&self) -> ProjectDiskPhysicalCaptureErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for ProjectDiskPhysicalCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectDiskPhysicalCaptureError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for ProjectDiskPhysicalCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ProjectDiskPhysicalCaptureError {}

/// Capture one observation-only private receipt from the exact operator Mac and resident Lima VM.
///
/// The function holds and revalidates the exact Lima home, supplied standalone-disk directory, and
/// every openable direct entry while it collects fixed Lima JSON and guest kernel observations. It
/// performs no attach, detach, unlock, format, resize, delete, mount, or project-state write.
///
/// # Errors
///
/// Returns a bounded path-private error when exact evidence cannot be retained and revalidated.
pub fn capture_project_disk_physical_receipt(
    request: &ProjectDiskPhysicalCaptureRequest,
    executor: &impl TimedCommandExecutor,
) -> Result<ProjectDiskPhysicalReceipt, ProjectDiskPhysicalCaptureError> {
    let captured_at_unix_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| capture_error(ProjectDiskPhysicalCaptureErrorKind::Clock))?
        .as_millis()
        .try_into()
        .map_err(|_| capture_error(ProjectDiskPhysicalCaptureErrorKind::Clock))?;

    let lima_request = LimaObservationRequest::new(
        request.resident_sandbox_instance.clone(),
        request.lima_home.clone(),
        LimaVmType::Vz,
        LimaArchitecture::Aarch64,
        request.guest_cache_path.clone(),
        DEFAULT_OBSERVATION_AGE_SECONDS,
    )
    .map_err(|_| invalid_request())?;
    let host_identity_before = LimaHostIdentityAdapter
        .observe(&lima_request)
        .map_err(|_| capture_error(ProjectDiskPhysicalCaptureErrorKind::LimaHostIdentity))?;

    let lima_home = open_absolute_directory(&request.lima_home)?;
    let lima_home_before = snapshot(&fs::fstat(&lima_home).map_err(|_| filesystem_error())?)?;
    let relative_disk = request
        .disk_directory
        .strip_prefix(&request.lima_home)
        .map_err(|_| invalid_request())?;
    let disk_directory = open_relative_directory(&lima_home, relative_disk)?;
    let mut held_disk = capture_held_disk_directory(disk_directory)?;

    let limactl_version = execute(
        executor,
        limactl_command(request).argument("--version"),
        request.command_timeout,
    )?;
    let disk_list_json = execute(
        executor,
        limactl_command(request)
            .argument("disk")
            .argument("list")
            .argument("--json"),
        request.command_timeout,
    )?;
    let instance_list_json = execute(
        executor,
        limactl_command(request)
            .argument("--tty=false")
            .argument("list")
            .argument("--format=json")
            .argument("--all-fields")
            .argument(request.resident_sandbox_instance.as_str()),
        request.command_timeout,
    )?;
    let mountinfo = execute(
        executor,
        guest_command(request)
            .argument("/usr/bin/cat")
            .argument("/proc/self/mountinfo"),
        request.command_timeout,
    )?;
    let block_devices_json = execute(
        executor,
        guest_command(request)
            .argument("/usr/bin/lsblk")
            .argument("--json")
            .argument("--bytes")
            .argument("--output")
            .argument("NAME,KNAME,MAJ:MIN,TYPE,SIZE,MOUNTPOINTS"),
        request.command_timeout,
    )?;

    let host_identity_after = LimaHostIdentityAdapter
        .observe(&lima_request)
        .map_err(|_| capture_error(ProjectDiskPhysicalCaptureErrorKind::LimaHostIdentity))?;
    if host_identity_before.identity() != host_identity_after.identity() {
        return Err(changed());
    }
    let disk_directory_evidence = held_disk.finish()?;
    let lima_home_after = snapshot(&fs::fstat(&lima_home).map_err(|_| filesystem_error())?)?;
    if !same_entry_binding(&lima_home_before, &lima_home_after) {
        return Err(changed());
    }
    let rebound_lima_home = open_absolute_directory(&request.lima_home)?;
    let rebound_lima_home_snapshot =
        snapshot(&fs::fstat(&rebound_lima_home).map_err(|_| filesystem_error())?)?;
    if !same_entry_binding(&lima_home_before, &rebound_lima_home_snapshot) {
        return Err(changed());
    }
    let rebound_disk = open_relative_directory(&rebound_lima_home, relative_disk)?;
    let rebound_disk_snapshot =
        snapshot(&fs::fstat(&rebound_disk).map_err(|_| filesystem_error())?)?;
    if !same_entry_binding(&disk_directory_evidence.after, &rebound_disk_snapshot) {
        return Err(changed());
    }

    let document = ProjectDiskPhysicalReceiptDocument {
        schema_version: PROJECT_DISK_PHYSICAL_RECEIPT_SCHEMA_VERSION,
        repo_commit: request.repo_commit.clone(),
        captured_at_unix_millis,
        declared_binding: DeclaredProjectDiskBindingDocument {
            project_identity: request.project_identity.as_str().to_owned(),
            project_disk_id: request.project_disk_id.as_str().to_owned(),
            project_disk_generation: request.project_disk_generation.get(),
            project_disk_revision: request.project_disk_revision.get(),
            attachment_generation: request.attachment_generation.get(),
            resident_sandbox_id: request.resident_sandbox_id.as_str().to_owned(),
            resident_sandbox_generation: request.resident_sandbox_generation.get(),
        },
        lima: ProjectDiskLimaEvidenceDocument {
            lima_home: path_string(&request.lima_home)?.to_owned(),
            disk_name: request.disk_name.clone(),
            resident_sandbox_instance: request.resident_sandbox_instance.as_str().to_owned(),
            host_identity_digest: host_identity_after.identity().digest().as_str().to_owned(),
            limactl_version: command_evidence(limactl_version),
            disk_list_json: command_evidence(disk_list_json),
            instance_list_json: command_evidence(instance_list_json),
        },
        disk_directory: ProjectDiskDirectoryEvidenceDocument {
            path: path_string(&request.disk_directory)?.to_owned(),
            ..disk_directory_evidence
        },
        guest: ProjectDiskGuestEvidenceDocument {
            project_mount: path_string(&request.guest_project_mount)?.to_owned(),
            mountinfo: command_evidence(mountinfo),
            block_devices_json: command_evidence(block_devices_json),
        },
    };
    let bytes = serde_json::to_vec(&document)
        .map_err(|_| capture_error(ProjectDiskPhysicalCaptureErrorKind::Receipt))?;
    ProjectDiskPhysicalReceipt::decode_private_json(&bytes)
        .map_err(|_| capture_error(ProjectDiskPhysicalCaptureErrorKind::Receipt))
}

fn limactl_command(request: &ProjectDiskPhysicalCaptureRequest) -> CommandSpec {
    CommandSpec::new(&request.limactl_program)
        .environment("HOME", "/var/empty")
        .environment(
            "LIMA_HOME",
            request
                .lima_home
                .to_str()
                .expect("validated Lima home remains UTF-8"),
        )
        .environment("LANG", "C")
        .environment("LC_ALL", "C")
        .environment("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
}

fn guest_command(request: &ProjectDiskPhysicalCaptureRequest) -> CommandSpec {
    limactl_command(request)
        .argument("shell")
        .argument(request.resident_sandbox_instance.as_str())
        .argument("--")
}

fn execute(
    executor: &impl TimedCommandExecutor,
    command: CommandSpec,
    timeout: Duration,
) -> Result<ExecutionRecord, ProjectDiskPhysicalCaptureError> {
    let record = executor
        .execute_with_timeout(&command, timeout)
        .map_err(|_| capture_error(ProjectDiskPhysicalCaptureErrorKind::Command))?;
    if !record.success || record.status != Some(0) {
        return Err(capture_error(ProjectDiskPhysicalCaptureErrorKind::Command));
    }
    Ok(record)
}

fn command_evidence(record: ExecutionRecord) -> ReceiptCommandEvidence {
    ReceiptCommandEvidence {
        argv: record.argv,
        environment_keys: record.environment_keys,
        status: record.status,
        success: record.success,
        stdout: record.stdout,
        stderr: record.stderr,
    }
}

fn validate_path(path: PathBuf) -> Result<PathBuf, ProjectDiskPhysicalCaptureError> {
    let Some(value) = path.to_str() else {
        return Err(invalid_request());
    };
    validate_absolute_path(value, "path").map_err(|_| invalid_request())?;
    Ok(path)
}

fn path_string(path: &Path) -> Result<&str, ProjectDiskPhysicalCaptureError> {
    path.to_str().ok_or_else(invalid_request)
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(char::from(HEX[usize::from(byte >> 4)]));
        result.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    result
}

const fn capture_error(
    kind: ProjectDiskPhysicalCaptureErrorKind,
) -> ProjectDiskPhysicalCaptureError {
    let (code, message) = match kind {
        ProjectDiskPhysicalCaptureErrorKind::InvalidRequest => (
            "project_disk_physical_capture_invalid_request",
            "project disk physical capture request is outside the reviewed contract",
        ),
        ProjectDiskPhysicalCaptureErrorKind::Clock => (
            "project_disk_physical_capture_clock",
            "project disk physical capture time is unavailable",
        ),
        ProjectDiskPhysicalCaptureErrorKind::LimaHostIdentity => (
            "project_disk_physical_capture_lima_identity",
            "exact Lima host identity could not be retained",
        ),
        ProjectDiskPhysicalCaptureErrorKind::Filesystem => (
            "project_disk_physical_capture_filesystem",
            "project disk physical filesystem evidence is unavailable",
        ),
        ProjectDiskPhysicalCaptureErrorKind::Command => (
            "project_disk_physical_capture_command",
            "project disk physical observation command failed",
        ),
        ProjectDiskPhysicalCaptureErrorKind::ChangedDuringObservation => (
            "project_disk_physical_capture_changed",
            "project disk physical evidence changed identity during observation",
        ),
        ProjectDiskPhysicalCaptureErrorKind::Receipt => (
            "project_disk_physical_capture_receipt",
            "project disk physical receipt could not be validated",
        ),
    };
    ProjectDiskPhysicalCaptureError {
        kind,
        code,
        message,
    }
}

const fn invalid_request() -> ProjectDiskPhysicalCaptureError {
    capture_error(ProjectDiskPhysicalCaptureErrorKind::InvalidRequest)
}

const fn filesystem_error() -> ProjectDiskPhysicalCaptureError {
    capture_error(ProjectDiskPhysicalCaptureErrorKind::Filesystem)
}

const fn changed() -> ProjectDiskPhysicalCaptureError {
    capture_error(ProjectDiskPhysicalCaptureErrorKind::ChangedDuringObservation)
}
