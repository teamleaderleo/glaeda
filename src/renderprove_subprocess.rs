use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};

use serde::Serialize;

use crate::process::{CommandSpec, CommandValue, ExecutionRecord, MAX_CAPTURED_STREAM_BYTES};
use crate::renderprove_execution::{RenderproveCommand, RenderproveExecutionObservation};

const CAPTURE_BUFFER_BYTES: usize = 8_192;
const REDACTED: &str = "[REDACTED]";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderproveSubprocessErrorKind {
    Identity,
    Spawn,
    Status,
    OutputCapture,
    OutputLimit,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct RenderproveSubprocessError {
    kind: RenderproveSubprocessErrorKind,
    stage: &'static str,
    public_message: String,
}

impl RenderproveSubprocessError {
    #[must_use]
    pub const fn kind(&self) -> RenderproveSubprocessErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn stage(&self) -> &'static str {
        self.stage
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.public_message
    }

    fn new(
        kind: RenderproveSubprocessErrorKind,
        stage: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            stage,
            public_message: message.into(),
        }
    }

    fn identity(stage: &'static str, message: impl Into<String>) -> Self {
        Self::new(RenderproveSubprocessErrorKind::Identity, stage, message)
    }

    fn spawn(message: impl Into<String>) -> Self {
        Self::new(RenderproveSubprocessErrorKind::Spawn, "spawn", message)
    }

    fn status(message: impl Into<String>) -> Self {
        Self::new(RenderproveSubprocessErrorKind::Status, "status", message)
    }

    fn output_capture(stage: &'static str, message: impl Into<String>) -> Self {
        Self::new(
            RenderproveSubprocessErrorKind::OutputCapture,
            stage,
            message,
        )
    }

    fn output_limit(stage: &'static str) -> Self {
        Self::new(
            RenderproveSubprocessErrorKind::OutputLimit,
            stage,
            format!("child {stage} exceeded the {MAX_CAPTURED_STREAM_BYTES}-byte capture limit"),
        )
    }
}

impl fmt::Debug for RenderproveSubprocessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderproveSubprocessError")
            .field("kind", &self.kind)
            .field("stage", &self.stage)
            .field("message", &self.public_message)
            .finish()
    }
}

impl fmt::Display for RenderproveSubprocessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.public_message)
    }
}

impl std::error::Error for RenderproveSubprocessError {}

/// Execute one already reviewed Renderprove command in its exact reviewed working directory.
///
/// This is the only public execution entry point in this adapter. It accepts no program, argument,
/// environment, shell, or working-directory override. The exact working directory and every
/// required executable are observed before and after execution. The process environment is cleared,
/// stdout and stderr are captured privately with fixed limits, and the resulting typed observation
/// retains the same private command specification that was executed.
///
/// # Errors
///
/// Returns a typed error before or instead of an observation when filesystem identity is unsafe or
/// changes, the process cannot be spawned, status evidence is unrepresentable, output capture fails,
/// or either output stream exceeds the fixed limit.
pub fn execute_renderprove_command(
    command: &RenderproveCommand,
) -> Result<RenderproveExecutionObservation, RenderproveSubprocessError> {
    execute_with_runtime(command, &SystemRenderproveRuntime)
}

fn execute_with_runtime(
    command: &RenderproveCommand,
    runtime: &impl RenderproveRuntime,
) -> Result<RenderproveExecutionObservation, RenderproveSubprocessError> {
    let working_directory = runtime.observe_directory(command.working_directory())?;
    if working_directory.physical_path != command.working_directory() {
        return Err(RenderproveSubprocessError::identity(
            "working_directory",
            "reviewed working directory does not resolve to its exact physical path",
        ));
    }

    let required_programs = command.required_programs();
    let mut executable_identities = Vec::with_capacity(required_programs.len());
    for program in required_programs {
        let identity = runtime.observe_executable(program)?;
        if identity.physical_path != program {
            return Err(RenderproveSubprocessError::identity(
                "required_program",
                "reviewed executable does not resolve to its exact physical path",
            ));
        }
        executable_identities.push((program.to_path_buf(), identity));
    }

    let record = runtime.execute(command.spec(), &working_directory.physical_path)?;
    validate_record(command.spec(), &record)?;

    let final_working_directory = runtime.observe_directory(command.working_directory())?;
    if final_working_directory != working_directory {
        return Err(RenderproveSubprocessError::identity(
            "working_directory",
            "reviewed working-directory identity changed during execution",
        ));
    }
    for (program, expected) in executable_identities {
        let observed = runtime.observe_executable(&program)?;
        if observed != expected {
            return Err(RenderproveSubprocessError::identity(
                "required_program",
                "reviewed executable identity changed during execution",
            ));
        }
    }

    RenderproveExecutionObservation::new(
        record,
        working_directory.physical_path,
        command.spec().clone(),
    )
    .map_err(|_| {
        RenderproveSubprocessError::identity(
            "process_evidence",
            "process evidence could not be represented as a Renderprove execution observation",
        )
    })
}

fn validate_record(
    spec: &CommandSpec,
    record: &ExecutionRecord,
) -> Result<(), RenderproveSubprocessError> {
    if record.argv != spec.displayed_argv()
        || record.environment_keys != spec.environment.keys().cloned().collect::<Vec<_>>()
    {
        return Err(RenderproveSubprocessError::identity(
            "process_evidence",
            "process evidence does not describe the exact reviewed command",
        ));
    }
    match (record.status, record.success) {
        (Some(0), true) | (Some(1..=255), false) => Ok(()),
        (None, _) => Err(RenderproveSubprocessError::status(
            "process did not provide a representable exit status",
        )),
        _ => Err(RenderproveSubprocessError::status(
            "process success and exit-status evidence are inconsistent or out of range",
        )),
    }
}

#[derive(Clone, PartialEq, Eq)]
struct FilesystemIdentity {
    physical_path: PathBuf,
    device: u64,
    inode: u64,
    owner_uid: u32,
    mode: u32,
}

trait RenderproveRuntime {
    fn observe_directory(
        &self,
        path: &Path,
    ) -> Result<FilesystemIdentity, RenderproveSubprocessError>;

    fn observe_executable(
        &self,
        path: &Path,
    ) -> Result<FilesystemIdentity, RenderproveSubprocessError>;

    fn execute(
        &self,
        spec: &CommandSpec,
        working_directory: &Path,
    ) -> Result<ExecutionRecord, RenderproveSubprocessError>;
}

#[derive(Debug, Clone, Copy)]
struct SystemRenderproveRuntime;

impl RenderproveRuntime for SystemRenderproveRuntime {
    fn observe_directory(
        &self,
        path: &Path,
    ) -> Result<FilesystemIdentity, RenderproveSubprocessError> {
        observe_filesystem_identity(path, ExpectedObjectKind::Directory, "working_directory")
    }

    fn observe_executable(
        &self,
        path: &Path,
    ) -> Result<FilesystemIdentity, RenderproveSubprocessError> {
        observe_filesystem_identity(path, ExpectedObjectKind::Executable, "required_program")
    }

    fn execute(
        &self,
        spec: &CommandSpec,
        working_directory: &Path,
    ) -> Result<ExecutionRecord, RenderproveSubprocessError> {
        execute_exact_process(spec, working_directory)
    }
}

#[derive(Debug, Clone, Copy)]
enum ExpectedObjectKind {
    Directory,
    Executable,
}

fn observe_filesystem_identity(
    path: &Path,
    expected: ExpectedObjectKind,
    stage: &'static str,
) -> Result<FilesystemIdentity, RenderproveSubprocessError> {
    if !path.is_absolute() {
        return Err(RenderproveSubprocessError::identity(
            stage,
            "reviewed filesystem path is not absolute",
        ));
    }
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        let message = if error.kind() == io::ErrorKind::NotFound {
            "reviewed filesystem object is missing"
        } else {
            "reviewed filesystem object could not be inspected"
        };
        RenderproveSubprocessError::identity(stage, message)
    })?;
    if metadata.file_type().is_symlink() {
        return Err(RenderproveSubprocessError::identity(
            stage,
            "reviewed filesystem object is a symlink",
        ));
    }
    match expected {
        ExpectedObjectKind::Directory if !metadata.is_dir() => {
            return Err(RenderproveSubprocessError::identity(
                stage,
                "reviewed working directory is not a directory",
            ));
        }
        ExpectedObjectKind::Executable if !metadata.is_file() => {
            return Err(RenderproveSubprocessError::identity(
                stage,
                "reviewed executable is not a regular file",
            ));
        }
        ExpectedObjectKind::Executable if metadata.mode() & 0o111 == 0 => {
            return Err(RenderproveSubprocessError::identity(
                stage,
                "reviewed executable lacks execute permission bits",
            ));
        }
        ExpectedObjectKind::Directory | ExpectedObjectKind::Executable => {}
    }

    let physical_path = fs::canonicalize(path).map_err(|_| {
        RenderproveSubprocessError::identity(
            stage,
            "reviewed filesystem object could not be resolved physically",
        )
    })?;
    let physical_metadata = fs::metadata(&physical_path).map_err(|_| {
        RenderproveSubprocessError::identity(
            stage,
            "physical filesystem identity could not be inspected",
        )
    })?;
    if metadata.dev() != physical_metadata.dev() || metadata.ino() != physical_metadata.ino() {
        return Err(RenderproveSubprocessError::identity(
            stage,
            "logical and physical filesystem identities disagree",
        ));
    }

    Ok(FilesystemIdentity {
        physical_path,
        device: physical_metadata.dev(),
        inode: physical_metadata.ino(),
        owner_uid: physical_metadata.uid(),
        mode: physical_metadata.mode() & 0o7777,
    })
}

fn execute_exact_process(
    spec: &CommandSpec,
    working_directory: &Path,
) -> Result<ExecutionRecord, RenderproveSubprocessError> {
    let mut process = build_process(spec, working_directory);
    let mut child = process.spawn().map_err(|_| {
        RenderproveSubprocessError::spawn("reviewed Renderprove process could not be spawned")
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        RenderproveSubprocessError::output_capture(
            "stdout",
            "child stdout was unavailable after requesting a pipe",
        )
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        RenderproveSubprocessError::output_capture(
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
                let _ = terminate_child(&mut child);
                let _ = child.wait();
                let _ = join_capture_reader(stdout_reader);
                let _ = join_capture_reader(stderr_reader);
                return Err(RenderproveSubprocessError::output_capture(
                    "output",
                    "output capture workers stopped before reporting completion",
                ));
            }
        };
        match event {
            CaptureEvent::LimitExceeded(stream) => {
                exceeded.insert(stream);
                terminate_child(&mut child)?;
            }
            CaptureEvent::Completed(stream, result) => {
                let bytes = match result {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        if capture_error.is_none() {
                            capture_error = Some(stream);
                        }
                        terminate_child(&mut child)?;
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
        RenderproveSubprocessError::status("reviewed process status could not be collected")
    })?;
    join_capture_reader(stdout_reader)?;
    join_capture_reader(stderr_reader)?;

    if let Some(stream) = capture_error {
        return Err(RenderproveSubprocessError::output_capture(
            stream.as_str(),
            "child output could not be captured",
        ));
    }
    if let Some(stream) = exceeded.into_iter().next() {
        return Err(RenderproveSubprocessError::output_limit(stream.as_str()));
    }

    let code = status.code().ok_or_else(|| {
        RenderproveSubprocessError::status("process terminated without a representable exit code")
    })?;
    if !(0..=255).contains(&code) {
        return Err(RenderproveSubprocessError::status(
            "process exit status is outside the supported range",
        ));
    }

    let stdout_bytes = stdout_bytes.expect("stdout completion recorded");
    let stderr_bytes = stderr_bytes.expect("stderr completion recorded");
    let secrets = spec
        .arguments
        .iter()
        .chain(spec.environment.values())
        .filter_map(CommandValue::secret)
        .collect::<Vec<_>>();

    Ok(ExecutionRecord {
        argv: spec.displayed_argv(),
        environment_keys: spec.environment.keys().cloned().collect(),
        status: Some(code),
        success: status.success(),
        stdout: redact(&String::from_utf8_lossy(&stdout_bytes), &secrets),
        stderr: redact(&String::from_utf8_lossy(&stderr_bytes), &secrets),
    })
}

fn build_process(spec: &CommandSpec, working_directory: &Path) -> Command {
    let mut process = Command::new(&spec.program);
    process
        .env_clear()
        .current_dir(working_directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    process.args(spec.arguments.iter().map(CommandValue::exposed));
    for (key, value) in &spec.environment {
        process.env(key, value.exposed());
    }
    process
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

fn terminate_child(child: &mut Child) -> Result<(), RenderproveSubprocessError> {
    if child
        .try_wait()
        .map_err(|_| RenderproveSubprocessError::status("process status could not be inspected"))?
        .is_some()
    {
        return Ok(());
    }
    match child.kill() {
        Ok(()) => Ok(()),
        Err(_) => {
            if child
                .try_wait()
                .map_err(|_| {
                    RenderproveSubprocessError::status("process status could not be inspected")
                })?
                .is_some()
            {
                Ok(())
            } else {
                Err(RenderproveSubprocessError::status(
                    "process could not be terminated after output failure",
                ))
            }
        }
    }
}

fn join_capture_reader(handle: JoinHandle<()>) -> Result<(), RenderproveSubprocessError> {
    handle.join().map_err(|_| {
        RenderproveSubprocessError::output_capture("output", "output capture worker panicked")
    })
}

fn redact(value: &str, secrets: &[&str]) -> String {
    secrets.iter().fold(value.to_owned(), |output, secret| {
        output.replace(secret, REDACTED)
    })
}

#[cfg(test)]
mod tests;
