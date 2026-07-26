use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};

use serde::Serialize;

const REDACTED: &str = "[REDACTED]";
const CAPTURE_BUFFER_BYTES: usize = 8_192;
pub const MAX_CAPTURED_STREAM_BYTES: usize = 1_048_576;

#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(String);

impl SecretString {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(REDACTED)
    }
}

impl Serialize for SecretString {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(REDACTED)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "sensitivity", content = "value", rename_all = "snake_case")]
pub enum CommandValue {
    Plain(String),
    Secret(SecretString),
}

impl CommandValue {
    pub(crate) fn exposed(&self) -> &str {
        match self {
            Self::Plain(value) => value,
            Self::Secret(value) => value.expose(),
        }
    }

    fn displayed(&self) -> String {
        match self {
            Self::Plain(value) => value.clone(),
            Self::Secret(_) => REDACTED.to_owned(),
        }
    }

    pub(crate) fn secret(&self) -> Option<&str> {
        match self {
            Self::Plain(_) => None,
            Self::Secret(value) if value.expose().is_empty() => None,
            Self::Secret(value) => Some(value.expose()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandSpec {
    pub program: PathBuf,
    pub arguments: Vec<CommandValue>,
    pub environment: BTreeMap<String, CommandValue>,
}

impl CommandSpec {
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            arguments: Vec::new(),
            environment: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn argument(mut self, value: impl Into<String>) -> Self {
        self.arguments.push(CommandValue::Plain(value.into()));
        self
    }

    #[must_use]
    pub fn secret_argument(mut self, value: impl Into<String>) -> Self {
        self.arguments
            .push(CommandValue::Secret(SecretString::new(value)));
        self
    }

    #[must_use]
    pub fn environment(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.environment
            .insert(key.into(), CommandValue::Plain(value.into()));
        self
    }

    #[must_use]
    pub fn secret_environment(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.environment
            .insert(key.into(), CommandValue::Secret(SecretString::new(value)));
        self
    }

    #[must_use]
    pub fn displayed_argv(&self) -> Vec<String> {
        std::iter::once(self.program.display().to_string())
            .chain(self.arguments.iter().map(CommandValue::displayed))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecutionRecord {
    pub argv: Vec<String>,
    pub environment_keys: Vec<String>,
    pub status: Option<i32>,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

pub trait CommandExecutor {
    /// Execute one explicit program without an implicit shell.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the program path is unsafe, the process cannot be started, output
    /// capture fails, or either output stream exceeds the fixed capture limit.
    fn execute(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessExecutor;

impl CommandExecutor for ProcessExecutor {
    fn execute(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord> {
        ensure_absolute_program(&spec.program)?;

        let mut command = Command::new(&spec.program);
        command
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.args(spec.arguments.iter().map(CommandValue::exposed));
        for (key, value) in &spec.environment {
            command.env(key, value.exposed());
        }

        let mut child = command.spawn()?;
        let stdout = child.stdout.take().ok_or_else(|| {
            io::Error::other("child stdout was not available after requesting a pipe")
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            io::Error::other("child stderr was not available after requesting a pipe")
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
                    let termination_result = terminate_child(&mut child);
                    let wait_result = child.wait();
                    let stdout_join_result = join_capture_reader(stdout_reader);
                    let stderr_join_result = join_capture_reader(stderr_reader);
                    termination_result?;
                    wait_result?;
                    stdout_join_result?;
                    stderr_join_result?;
                    return Err(io::Error::other(
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
                        Err(error) => {
                            if capture_error.is_none() {
                                capture_error = Some(error);
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

        let status = child.wait()?;
        join_capture_reader(stdout_reader)?;
        join_capture_reader(stderr_reader)?;

        if let Some(error) = capture_error {
            return Err(error);
        }
        if !exceeded.is_empty() {
            let streams = exceeded
                .iter()
                .map(|stream| stream.as_str())
                .collect::<Vec<_>>()
                .join(" and ");
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "child {streams} exceeded the {MAX_CAPTURED_STREAM_BYTES}-byte capture limit"
                ),
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
            status: status.code(),
            success: status.success(),
            stdout: redact(&String::from_utf8_lossy(&stdout_bytes), &secrets),
            stderr: redact(&String::from_utf8_lossy(&stderr_bytes), &secrets),
        })
    }
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

fn terminate_child(child: &mut Child) -> io::Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    match child.kill() {
        Ok(()) => Ok(()),
        Err(_) if child.try_wait()?.is_some() => Ok(()),
        Err(error) => Err(error),
    }
}

fn join_capture_reader(handle: JoinHandle<()>) -> io::Result<()> {
    handle
        .join()
        .map_err(|_| io::Error::other("output capture worker panicked"))
}

fn ensure_absolute_program(program: &Path) -> io::Result<()> {
    if program.is_absolute() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "command program must be an absolute path: {}",
                program.display()
            ),
        ))
    }
}

fn redact(value: &str, secrets: &[&str]) -> String {
    secrets.iter().fold(value.to_owned(), |output, secret| {
        output.replace(secret, REDACTED)
    })
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::path::Path;

    use super::{
        CommandExecutor, CommandSpec, MAX_CAPTURED_STREAM_BYTES, ProcessExecutor, REDACTED,
    };

    #[test]
    fn serialization_and_debug_output_redact_secrets() {
        let spec = CommandSpec::new("/usr/bin/example")
            .argument("visible")
            .secret_argument("very-secret")
            .secret_environment("TOKEN", "environment-secret");

        let debug = format!("{spec:?}");
        let json = serde_json::to_string(&spec).expect("serialize command spec");
        assert!(!debug.contains("very-secret"));
        assert!(!debug.contains("environment-secret"));
        assert!(!json.contains("very-secret"));
        assert!(!json.contains("environment-secret"));
        assert!(json.contains(REDACTED));
    }

    #[test]
    fn process_output_is_redacted() -> io::Result<()> {
        let printf = Path::new("/usr/bin/printf");
        if !printf.is_file() {
            return Ok(());
        }

        let spec = CommandSpec::new(printf)
            .argument("%s")
            .secret_argument("top-secret");
        let record = ProcessExecutor.execute(&spec)?;

        assert!(record.success);
        assert_eq!(record.stdout, REDACTED);
        assert!(!record.argv.join(" ").contains("top-secret"));
        Ok(())
    }

    #[test]
    fn stdout_above_the_capture_limit_terminates_the_child() -> io::Result<()> {
        assert_stream_limit("sys.stdout.buffer.write(b'x' * size)", "stdout")
    }

    #[test]
    fn stderr_above_the_capture_limit_terminates_the_child() -> io::Result<()> {
        assert_stream_limit("sys.stderr.buffer.write(b'x' * size)", "stderr")
    }

    #[test]
    fn stdout_and_stderr_are_drained_concurrently() -> io::Result<()> {
        let python = Path::new("/usr/bin/python3");
        if !python.is_file() {
            return Ok(());
        }
        let chunk = 256 * 1_024;
        let script = format!("import os; os.write(1, b'o' * {chunk}); os.write(2, b'e' * {chunk})");
        let record =
            ProcessExecutor.execute(&CommandSpec::new(python).argument("-c").argument(script))?;

        assert!(record.success);
        assert_eq!(record.stdout.len(), chunk);
        assert_eq!(record.stderr.len(), chunk);
        Ok(())
    }

    #[test]
    fn relative_programs_are_rejected() {
        let error = ProcessExecutor
            .execute(&CommandSpec::new("printf"))
            .expect_err("relative program must fail");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    fn assert_stream_limit(script_body: &str, stream: &str) -> io::Result<()> {
        let python = Path::new("/usr/bin/python3");
        if !python.is_file() {
            return Ok(());
        }
        let script = format!(
            "import sys; size = {}; {script_body}; sys.stdout.flush(); sys.stderr.flush()",
            MAX_CAPTURED_STREAM_BYTES + 1
        );
        let error = ProcessExecutor
            .execute(&CommandSpec::new(python).argument("-c").argument(script))
            .expect_err("capture limit must fail closed");

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains(stream));
        assert!(
            error
                .to_string()
                .contains(&MAX_CAPTURED_STREAM_BYTES.to_string())
        );
        Ok(())
    }
}
