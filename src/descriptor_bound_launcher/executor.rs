use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};

use rustix::fs::{self, FileType, Mode, OFlags};
use rustix::io::Errno;
use rustix::process::{self, Pid, Signal};

use super::{
    CAPTURE_BUFFER_BYTES, DESCRIPTOR_BOUND_LAUNCH_SCHEMA_VERSION, DescriptorBoundLaunchError,
    DescriptorBoundLaunchReceipt, DescriptorBoundPrivateDiagnostics,
    DescriptorBoundPrivateEvidence, DescriptorBoundTermination, LaunchHooks, REDACTED,
    ReviewedFilesystemIdentity, ReviewedLaunchCredentials, ReviewedLaunchObject,
    ReviewedLaunchValue, ReviewedLinuxLaunchPlan,
};
use crate::process::MAX_CAPTURED_STREAM_BYTES;

const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const DIRECTORY_FLAGS: OFlags = OFlags::PATH
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const EXECUTABLE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// Execute one already reviewed Linux launch plan through retained cwd and executable descriptors.
///
/// The executor opens both reviewed objects through no-follow descriptor traversal from `/`, checks
/// their exact device/inode/owner/mode identities, and then supplies `/proc/self/fd/<n>` aliases for
/// the held descriptors to `Command`. The aliases select the retained objects rather than reopening
/// the original names. Both held descriptors are `CLOEXEC`, so they are available for child setup
/// and executable lookup but are closed in the new image. The child receives null stdin, bounded
/// private stdout/stderr capture, a cleared exact environment, and a new process group. No ambient
/// signal forwarding is performed. Capture failure sends `SIGKILL` to that process group.
///
/// Only direct ELF executables are supported. Scripts are refused so a shebang interpreter cannot
/// reintroduce a path-selected executable after review.
///
/// # Errors
///
/// Returns a typed public error for plan drift, unsafe or replaced filesystem objects, unavailable
/// descriptor aliases, unsupported executable format, credential mismatch, spawn/capture/status
/// failure, or output-limit exhaustion. Public errors never contain paths, descriptor numbers, raw
/// operating-system errors, or captured output.
pub fn execute_reviewed_linux_launch(
    plan: &ReviewedLinuxLaunchPlan,
) -> Result<DescriptorBoundLaunchReceipt, DescriptorBoundLaunchError> {
    execute_with_hooks(plan, &NoopLaunchHooks)
}

pub(super) fn execute_with_hooks(
    plan: &ReviewedLinuxLaunchPlan,
    hooks: &impl LaunchHooks,
) -> Result<DescriptorBoundLaunchReceipt, DescriptorBoundLaunchError> {
    validate_launcher_credentials(plan.credentials)?;
    let bound = BoundLaunchObjects::open(plan)?;
    hooks.after_descriptors_opened()?;

    let record = execute_bound_process(plan, &bound, hooks)?;
    Ok(DescriptorBoundLaunchReceipt {
        schema_version: DESCRIPTOR_BOUND_LAUNCH_SCHEMA_VERSION,
        command_id: plan.command_id.clone(),
        argument_count: plan.arguments.len(),
        environment_keys: plan.environment_keys(),
        credentials: plan.credentials,
        termination: record.termination,
        success: record.success,
        plan: plan.clone(),
        diagnostics: DescriptorBoundPrivateDiagnostics {
            stdout: record.stdout,
            stderr: record.stderr,
        },
        evidence: DescriptorBoundPrivateEvidence {
            executable: bound.executable_identity,
            working_directory: bound.working_directory_identity,
        },
    })
}

fn validate_launcher_credentials(
    credentials: ReviewedLaunchCredentials,
) -> Result<(), DescriptorBoundLaunchError> {
    let observed = (process::geteuid().as_raw(), process::getegid().as_raw());
    if observed != credentials.launcher_identity() {
        return Err(DescriptorBoundLaunchError::credentials(
            "effective launcher identity does not match the reviewed plan",
        ));
    }
    Ok(())
}

struct BoundLaunchObjects {
    _executable: File,
    _working_directory: OwnedFd,
    executable_identity: ReviewedFilesystemIdentity,
    working_directory_identity: ReviewedFilesystemIdentity,
    executable_alias: PathBuf,
    working_directory_alias: PathBuf,
}

impl BoundLaunchObjects {
    fn open(plan: &ReviewedLinuxLaunchPlan) -> Result<Self, DescriptorBoundLaunchError> {
        let working_directory = open_reviewed_directory(&plan.working_directory)?;
        let executable = open_reviewed_executable(&plan.executable)?;
        let working_directory_alias = descriptor_alias(
            working_directory.as_raw_fd(),
            &plan.working_directory.identity,
            ExpectedObjectKind::Directory,
            "working_directory",
        )?;
        let executable_alias = descriptor_alias(
            executable.as_raw_fd(),
            &plan.executable.identity,
            ExpectedObjectKind::Executable,
            "executable",
        )?;

        Ok(Self {
            _executable: executable,
            _working_directory: working_directory,
            executable_identity: plan.executable.identity.clone(),
            working_directory_identity: plan.working_directory.identity.clone(),
            executable_alias,
            working_directory_alias,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpectedObjectKind {
    Directory,
    Executable,
}

fn open_reviewed_directory(
    reviewed: &ReviewedLaunchObject,
) -> Result<OwnedFd, DescriptorBoundLaunchError> {
    let components = super::normal_components(&reviewed.logical_path)?;
    let mut current = fs::open("/", DIRECTORY_FLAGS, Mode::empty()).map_err(|_| {
        DescriptorBoundLaunchError::identity(
            "working_directory",
            "filesystem root could not be opened for descriptor traversal",
        )
    })?;
    for component in components {
        current = open_directory_component(&current, component, "working_directory")?;
    }
    let observed = inspect_descriptor(&current, ExpectedObjectKind::Directory, false)?;
    require_identity("working_directory", &reviewed.identity, &observed)?;
    Ok(current)
}

fn open_reviewed_executable(
    reviewed: &ReviewedLaunchObject,
) -> Result<File, DescriptorBoundLaunchError> {
    let components = super::normal_components(&reviewed.logical_path)?;
    let Some((file_name, parents)) = components.split_last() else {
        return Err(DescriptorBoundLaunchError::plan(
            "executable",
            "reviewed executable path has no file component",
        ));
    };
    let mut current = fs::open("/", DIRECTORY_FLAGS, Mode::empty()).map_err(|_| {
        DescriptorBoundLaunchError::identity(
            "executable",
            "filesystem root could not be opened for descriptor traversal",
        )
    })?;
    for component in parents {
        current = open_directory_component(&current, component, "executable")?;
    }

    let executable = fs::openat(current.as_fd(), *file_name, EXECUTABLE_FLAGS, Mode::empty())
        .map_err(|error| match error {
            Errno::LOOP | Errno::NOTDIR => DescriptorBoundLaunchError::identity(
                "executable",
                "reviewed executable path is symlinked or has an invalid object type",
            ),
            _ => DescriptorBoundLaunchError::identity(
                "executable",
                "reviewed executable could not be opened safely",
            ),
        })?;
    let observed = inspect_descriptor(&executable, ExpectedObjectKind::Executable, true)?;
    require_identity("executable", &reviewed.identity, &observed)?;

    let mut executable = File::from(executable);
    let mut magic = [0_u8; ELF_MAGIC.len()];
    executable.read_exact(&mut magic).map_err(|_| {
        DescriptorBoundLaunchError::unsupported(
            "reviewed executable could not be identified as a direct ELF image",
        )
    })?;
    if magic != ELF_MAGIC {
        return Err(DescriptorBoundLaunchError::unsupported(
            "reviewed executable must be a direct ELF image; scripts are unsupported",
        ));
    }
    Ok(executable)
}

fn open_directory_component(
    parent: &OwnedFd,
    component: &std::ffi::OsStr,
    stage: &'static str,
) -> Result<OwnedFd, DescriptorBoundLaunchError> {
    fs::openat(parent.as_fd(), component, DIRECTORY_FLAGS, Mode::empty()).map_err(|error| {
        match error {
            Errno::LOOP | Errno::NOTDIR => DescriptorBoundLaunchError::identity(
                stage,
                "reviewed path contains a symlink or non-directory component",
            ),
            _ => DescriptorBoundLaunchError::identity(
                stage,
                "reviewed path component could not be opened safely",
            ),
        }
    })
}

#[derive(Clone, PartialEq, Eq)]
struct ObservedDescriptorIdentity {
    identity: ReviewedFilesystemIdentity,
}

fn inspect_descriptor(
    descriptor: &impl AsFd,
    expected: ExpectedObjectKind,
    require_single_link: bool,
) -> Result<ObservedDescriptorIdentity, DescriptorBoundLaunchError> {
    let stage = match expected {
        ExpectedObjectKind::Directory => "working_directory",
        ExpectedObjectKind::Executable => "executable",
    };
    let stat = fs::fstat(descriptor).map_err(|_| {
        DescriptorBoundLaunchError::identity(
            stage,
            "held descriptor identity could not be inspected",
        )
    })?;
    let object_type = FileType::from_raw_mode(stat.st_mode);
    let type_matches = match expected {
        ExpectedObjectKind::Directory => object_type.is_dir(),
        ExpectedObjectKind::Executable => object_type.is_file(),
    };
    if !type_matches {
        return Err(DescriptorBoundLaunchError::identity(
            stage,
            "held descriptor has an unsupported object type",
        ));
    }
    if expected == ExpectedObjectKind::Executable && stat.st_mode & 0o111 == 0 {
        return Err(DescriptorBoundLaunchError::identity(
            stage,
            "held executable descriptor lacks execute permission bits",
        ));
    }
    if require_single_link && stat.st_nlink != 1 {
        return Err(DescriptorBoundLaunchError::identity(
            stage,
            "held executable descriptor must have exactly one hard link",
        ));
    }
    Ok(ObservedDescriptorIdentity {
        identity: ReviewedFilesystemIdentity {
            device: stat.st_dev,
            inode: stat.st_ino,
            owner_uid: stat.st_uid,
            owner_gid: stat.st_gid,
            mode: stat.st_mode & 0o7777,
        },
    })
}

fn require_identity(
    stage: &'static str,
    expected: &ReviewedFilesystemIdentity,
    observed: &ObservedDescriptorIdentity,
) -> Result<(), DescriptorBoundLaunchError> {
    if !observed.identity.exact_match(expected) {
        return Err(DescriptorBoundLaunchError::identity(
            stage,
            "held descriptor does not match the exact reviewed filesystem identity",
        ));
    }
    Ok(())
}

fn descriptor_alias(
    raw_fd: i32,
    expected: &ReviewedFilesystemIdentity,
    kind: ExpectedObjectKind,
    stage: &'static str,
) -> Result<PathBuf, DescriptorBoundLaunchError> {
    let alias = PathBuf::from(format!("/proc/self/fd/{raw_fd}"));
    let metadata = std::fs::metadata(&alias).map_err(|_| {
        DescriptorBoundLaunchError::alias(
            stage,
            "Linux descriptor alias is unavailable for the held launch object",
        )
    })?;
    use std::os::unix::fs::MetadataExt;
    let object_type_matches = match kind {
        ExpectedObjectKind::Directory => metadata.is_dir(),
        ExpectedObjectKind::Executable => metadata.is_file(),
    };
    let observed = ReviewedFilesystemIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        owner_uid: metadata.uid(),
        owner_gid: metadata.gid(),
        mode: metadata.mode() & 0o7777,
    };
    if !object_type_matches || !observed.exact_match(expected) {
        return Err(DescriptorBoundLaunchError::alias(
            stage,
            "Linux descriptor alias does not resolve to the held reviewed object",
        ));
    }
    Ok(alias)
}

struct BoundProcessRecord {
    termination: DescriptorBoundTermination,
    success: bool,
    stdout: String,
    stderr: String,
}

fn execute_bound_process(
    plan: &ReviewedLinuxLaunchPlan,
    bound: &BoundLaunchObjects,
    hooks: &impl LaunchHooks,
) -> Result<BoundProcessRecord, DescriptorBoundLaunchError> {
    let mut command = Command::new(&bound.executable_alias);
    command
        .arg0(&plan.executable.logical_path)
        .env_clear()
        .current_dir(&bound.working_directory_alias)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    command.args(plan.arguments.iter().map(ReviewedLaunchValue::exposed));
    for (key, value) in &plan.environment {
        command.env(key, value.exposed());
    }
    if let ReviewedLaunchCredentials::DropPrivileges {
        target_uid,
        target_gid,
        ..
    } = plan.credentials
    {
        command.gid(target_gid).uid(target_uid);
    }

    let mut child = command.spawn().map_err(|_| {
        DescriptorBoundLaunchError::spawn("descriptor-bound reviewed process could not be spawned")
    })?;
    hooks.after_spawn()?;

    let stdout = child.stdout.take().ok_or_else(|| {
        DescriptorBoundLaunchError::output_capture(
            "stdout",
            "child stdout was unavailable after requesting a pipe",
        )
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        DescriptorBoundLaunchError::output_capture(
            "stderr",
            "child stderr was unavailable after requesting a pipe",
        )
    })?;

    let (sender, receiver) = mpsc::channel();
    let stdout_reader = spawn_capture_reader(stdout, CapturedStream::Stdout, sender.clone());
    let stderr_reader = spawn_capture_reader(stderr, CapturedStream::Stderr, sender);

    let mut stdout_bytes = None;
    let mut stderr_bytes = None;
    let mut exceeded = BTreeSet::new();
    let mut capture_error = None;

    while stdout_bytes.is_none() || stderr_bytes.is_none() {
        let event = match receiver.recv() {
            Ok(event) => event,
            Err(_) => {
                let _ = terminate_process_group(&mut child);
                let _ = child.wait();
                let _ = join_capture_reader(stdout_reader);
                let _ = join_capture_reader(stderr_reader);
                return Err(DescriptorBoundLaunchError::output_capture(
                    "output",
                    "output capture workers stopped before reporting completion",
                ));
            }
        };
        match event {
            CaptureEvent::LimitExceeded(stream) => {
                exceeded.insert(stream);
                terminate_process_group(&mut child)?;
            }
            CaptureEvent::Completed(stream, result) => {
                let bytes = match result {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        if capture_error.is_none() {
                            capture_error = Some(stream);
                        }
                        terminate_process_group(&mut child)?;
                        Vec::new()
                    }
                };
                match stream {
                    CapturedStream::Stdout => stdout_bytes = Some(bytes),
                    CapturedStream::Stderr => stderr_bytes = Some(bytes),
                }
            }
        }
    }

    let status = child.wait().map_err(|_| {
        DescriptorBoundLaunchError::status("descriptor-bound process status could not be collected")
    })?;
    join_capture_reader(stdout_reader)?;
    join_capture_reader(stderr_reader)?;

    if let Some(stream) = capture_error {
        return Err(DescriptorBoundLaunchError::output_capture(
            stream.as_str(),
            "child output could not be captured",
        ));
    }
    if let Some(stream) = exceeded.into_iter().next() {
        return Err(DescriptorBoundLaunchError::output_limit(stream.as_str()));
    }

    let termination = match (status.code(), status.signal()) {
        (Some(code @ 0..=255), None) => DescriptorBoundTermination::Exited {
            code: u8::try_from(code).expect("matched exit status range"),
        },
        (None, Some(signal @ 1..=255)) => DescriptorBoundTermination::Signaled {
            signal: u8::try_from(signal).expect("matched signal range"),
        },
        _ => {
            return Err(DescriptorBoundLaunchError::status(
                "process termination evidence is outside the supported range",
            ));
        }
    };

    let stdout_bytes = stdout_bytes.expect("stdout completion recorded");
    let stderr_bytes = stderr_bytes.expect("stderr completion recorded");
    let secrets = plan
        .arguments
        .iter()
        .chain(plan.environment.values())
        .filter_map(ReviewedLaunchValue::secret_value)
        .collect::<Vec<_>>();

    Ok(BoundProcessRecord {
        termination,
        success: status.success(),
        stdout: redact(&String::from_utf8_lossy(&stdout_bytes), &secrets),
        stderr: redact(&String::from_utf8_lossy(&stderr_bytes), &secrets),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum CapturedStream {
    Stdout,
    Stderr,
}

impl CapturedStream {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

enum CaptureEvent {
    LimitExceeded(CapturedStream),
    Completed(CapturedStream, io::Result<Vec<u8>>),
}

fn spawn_capture_reader(
    reader: impl Read + Send + 'static,
    stream: CapturedStream,
    sender: Sender<CaptureEvent>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let result = capture_stream(reader, stream, &sender);
        let _ = sender.send(CaptureEvent::Completed(stream, result));
    })
}

fn capture_stream(
    mut reader: impl Read,
    stream: CapturedStream,
    sender: &Sender<CaptureEvent>,
) -> io::Result<Vec<u8>> {
    let mut captured = Vec::with_capacity(CAPTURE_BUFFER_BYTES);
    let mut buffer = [0_u8; CAPTURE_BUFFER_BYTES];
    let mut limit_reported = false;

    loop {
        let count = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if limit_reported {
            continue;
        }
        let remaining = MAX_CAPTURED_STREAM_BYTES - captured.len();
        let retained = remaining.min(count);
        captured.extend_from_slice(&buffer[..retained]);
        if retained < count {
            limit_reported = true;
            let _ = sender.send(CaptureEvent::LimitExceeded(stream));
        }
    }
    Ok(captured)
}

fn terminate_process_group(child: &mut Child) -> Result<(), DescriptorBoundLaunchError> {
    if child
        .try_wait()
        .map_err(|_| DescriptorBoundLaunchError::status("process status could not be inspected"))?
        .is_some()
    {
        return Ok(());
    }

    let pid = Pid::from_child(child);
    match process::kill_process_group(pid, Signal::KILL) {
        Ok(()) | Err(Errno::SRCH) => Ok(()),
        Err(_) => child.kill().map_err(|_| {
            DescriptorBoundLaunchError::status(
                "process group could not be terminated after output failure",
            )
        }),
    }
}

fn join_capture_reader(handle: JoinHandle<()>) -> Result<(), DescriptorBoundLaunchError> {
    handle.join().map_err(|_| {
        DescriptorBoundLaunchError::output_capture("output", "output capture worker panicked")
    })
}

fn redact(value: &str, secrets: &[&str]) -> String {
    secrets.iter().fold(value.to_owned(), |output, secret| {
        output.replace(secret, REDACTED)
    })
}

struct NoopLaunchHooks;

impl LaunchHooks for NoopLaunchHooks {}
