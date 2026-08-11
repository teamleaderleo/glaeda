use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rustix::io::Errno;
use rustix::process::{self, Pid, Signal};
use serde::Serialize;
use zeroize::Zeroizing;

const REDACTED: &str = "[REDACTED]";
const CAPTURE_BUFFER_BYTES: usize = 8_192;
const CAPTURE_POLL_INTERVAL: Duration = Duration::from_millis(25);
pub const MAX_CAPTURED_STREAM_BYTES: usize = 1_048_576;
pub const MAX_COMMAND_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(Zeroizing<String>);

impl SecretString {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }

    pub(crate) fn from_zeroizing(value: Zeroizing<String>) -> Self {
        Self(value)
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
    fn exposed(&self) -> &str {
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

    fn secret(&self) -> Option<&str> {
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

    /// Add an already-zeroizing secret without creating a second ordinary `String` copy.
    #[must_use]
    pub(crate) fn zeroizing_secret_environment(
        mut self,
        key: impl Into<String>,
        value: Zeroizing<String>,
    ) -> Self {
        self.environment.insert(
            key.into(),
            CommandValue::Secret(SecretString::from_zeroizing(value)),
        );
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

pub trait TimedCommandExecutor: CommandExecutor {
    /// Execute one explicit program with a reviewed nonzero wall-clock deadline.
    ///
    /// The production executor starts the child in a fresh process group and sends `SIGKILL` to
    /// that group when the deadline expires. This covers the direct child and ordinary descendants;
    /// it does not claim the stronger cgroup ownership and cleanup evidence deferred to issue #205.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for a zero or implementation-exceeding timeout and `TimedOut` after
    /// terminating and reaping a command that exceeds the accepted deadline. Other failures retain
    /// the `CommandExecutor` contract.
    fn execute_with_timeout(
        &self,
        spec: &CommandSpec,
        timeout: Duration,
    ) -> io::Result<ExecutionRecord>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessExecutor;

impl CommandExecutor for ProcessExecutor {
    fn execute(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord> {
        execute_process(spec, None)
    }
}

impl TimedCommandExecutor for ProcessExecutor {
    fn execute_with_timeout(
        &self,
        spec: &CommandSpec,
        timeout: Duration,
    ) -> io::Result<ExecutionRecord> {
        if timeout.is_zero() || timeout > MAX_COMMAND_TIMEOUT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "command timeout must be within the bounded positive range",
            ));
        }
        execute_process(spec, Some(timeout))
    }
}

fn execute_process(spec: &CommandSpec, timeout: Option<Duration>) -> io::Result<ExecutionRecord> {
    execute_process_with_spawner(spec, timeout, &ThreadCaptureSpawner)
}

fn execute_process_with_spawner<S: CaptureThreadSpawner>(
    spec: &CommandSpec,
    timeout: Option<Duration>,
    spawner: &S,
) -> io::Result<ExecutionRecord> {
    ensure_absolute_program(&spec.program)?;
    let deadline = timeout
        .map(|duration| {
            Instant::now().checked_add(duration).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "command timeout exceeds the supported clock range",
                )
            })
        })
        .transpose()?;

    let mut command = Command::new(&spec.program);
    command
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    command.args(spec.arguments.iter().map(CommandValue::exposed));
    for (key, value) in &spec.environment {
        command.env(key, value.exposed());
    }

    let mut child = command.spawn()?;
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            abort_spawned_child(&mut child)?;
            return Err(io::Error::other(
                "child stdout was not available after requesting a pipe",
            ));
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            abort_spawned_child(&mut child)?;
            return Err(io::Error::other(
                "child stderr was not available after requesting a pipe",
            ));
        }
    };

    let (sender, receiver) = mpsc::channel();
    let stdout_reader = match spawner.spawn(stdout, CapturedStream::Stdout, sender.clone()) {
        Ok(reader) => reader,
        Err(error) => {
            cleanup_capture_setup_failure(&mut child, None)?;
            return Err(error);
        }
    };
    let stderr_reader = match spawner.spawn(stderr, CapturedStream::Stderr, sender) {
        Ok(reader) => reader,
        Err(error) => {
            cleanup_capture_setup_failure(&mut child, Some(stdout_reader))?;
            return Err(error);
        }
    };

    let mut stdout_bytes = None;
    let mut stderr_bytes = None;
    let mut exceeded = BTreeSet::new();
    let mut capture_error = None;
    let mut timed_out = false;

    while stdout_bytes.is_none() || stderr_bytes.is_none() {
        let abort_in_progress = timed_out || capture_error.is_some() || !exceeded.is_empty();
        let event = if abort_in_progress {
            match receiver.recv() {
                Ok(event) => event,
                Err(_) => {
                    let wait_result = child.wait();
                    let stdout_join_result = join_capture_reader(stdout_reader);
                    let stderr_join_result = join_capture_reader(stderr_reader);
                    wait_result?;
                    stdout_join_result?;
                    stderr_join_result?;
                    return Err(io::Error::other(
                        "output capture workers stopped before reporting completion",
                    ));
                }
            }
        } else if let Some(deadline) = deadline {
            let now = Instant::now();
            if now >= deadline {
                timed_out = true;
                terminate_process_group(&mut child)?;
                continue;
            }
            let wait = deadline
                .saturating_duration_since(now)
                .min(CAPTURE_POLL_INTERVAL);
            match receiver.recv_timeout(wait) {
                Ok(event) => event,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    let termination_result = terminate_process_group(&mut child);
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
            }
        } else {
            match receiver.recv() {
                Ok(event) => event,
                Err(_) => {
                    let termination_result = terminate_process_group(&mut child);
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
                    Err(error) => {
                        if capture_error.is_none() {
                            capture_error = Some(error);
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

    let status = if capture_error.is_some() || !exceeded.is_empty() {
        child.wait()?
    } else {
        wait_for_child(&mut child, deadline, &mut timed_out)?
    };
    join_capture_reader(stdout_reader)?;
    join_capture_reader(stderr_reader)?;

    if timed_out {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "child exceeded the reviewed wall-clock timeout",
        ));
    }
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
            format!("child {streams} exceeded the {MAX_CAPTURED_STREAM_BYTES}-byte capture limit"),
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

trait CaptureThreadSpawner {
    fn spawn<R: Read + Send + 'static>(
        &self,
        reader: R,
        stream: CapturedStream,
        sender: Sender<CaptureEvent>,
    ) -> io::Result<JoinHandle<()>>;
}

struct ThreadCaptureSpawner;

impl CaptureThreadSpawner for ThreadCaptureSpawner {
    fn spawn<R: Read + Send + 'static>(
        &self,
        reader: R,
        stream: CapturedStream,
        sender: Sender<CaptureEvent>,
    ) -> io::Result<JoinHandle<()>> {
        thread::Builder::new().spawn(move || {
            let result = capture_stream(reader, stream, &sender);
            let _ = sender.send(CaptureEvent::Completed(stream, result));
        })
    }
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

fn terminate_process_group(child: &mut Child) -> io::Result<()> {
    let pid = Pid::from_child(child);
    match process::kill_process_group(pid, Signal::KILL) {
        Ok(()) | Err(Errno::SRCH) => Ok(()),
        Err(_) => match child.kill() {
            Ok(()) => Ok(()),
            Err(_) if child.try_wait()?.is_some() => Ok(()),
            Err(error) => Err(error),
        },
    }
}

fn abort_spawned_child(child: &mut Child) -> io::Result<()> {
    terminate_process_group(child)?;
    child.wait()?;
    Ok(())
}

fn cleanup_capture_setup_failure(
    child: &mut Child,
    started_reader: Option<JoinHandle<()>>,
) -> io::Result<()> {
    let termination_result = terminate_process_group(child);
    let wait_result = child.wait();
    let join_result = started_reader.map(join_capture_reader).transpose();
    termination_result?;
    wait_result?;
    join_result?;
    Ok(())
}

fn wait_for_child(
    child: &mut Child,
    deadline: Option<Instant>,
    timed_out: &mut bool,
) -> io::Result<ExitStatus> {
    let Some(deadline) = deadline else {
        return child.wait();
    };
    if *timed_out {
        return child.wait();
    }

    loop {
        let now = Instant::now();
        if now >= deadline {
            *timed_out = true;
            terminate_process_group(child)?;
            return child.wait();
        }
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        thread::sleep(
            deadline
                .saturating_duration_since(now)
                .min(CAPTURE_POLL_INTERVAL),
        );
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
    use std::fs;
    use std::io::{self, Read};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::Sender;
    use std::thread;
    use std::thread::JoinHandle;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::{
        CaptureEvent, CaptureThreadSpawner, CapturedStream, CommandExecutor, CommandSpec,
        MAX_CAPTURED_STREAM_BYTES, MAX_COMMAND_TIMEOUT, ProcessExecutor, REDACTED,
        ThreadCaptureSpawner, TimedCommandExecutor, execute_process_with_spawner,
    };

    struct FailingCaptureSpawner {
        fail_on_call: usize,
        calls: AtomicUsize,
    }

    impl FailingCaptureSpawner {
        const fn new(fail_on_call: usize) -> Self {
            Self {
                fail_on_call,
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl CaptureThreadSpawner for FailingCaptureSpawner {
        fn spawn<R: Read + Send + 'static>(
            &self,
            reader: R,
            stream: CapturedStream,
            sender: Sender<CaptureEvent>,
        ) -> io::Result<JoinHandle<()>> {
            let call = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
            if call == self.fail_on_call {
                return Err(io::Error::other("injected capture thread failure"));
            }
            ThreadCaptureSpawner.spawn(reader, stream, sender)
        }
    }

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

    #[test]
    fn timed_execution_accepts_bounded_deadlines_and_completed_commands() -> io::Result<()> {
        let printf = Path::new("/usr/bin/printf");
        if !printf.is_file() {
            return Ok(());
        }

        let record = ProcessExecutor.execute_with_timeout(
            &CommandSpec::new(printf).argument("complete"),
            Duration::from_secs(1),
        )?;

        assert!(record.success);
        assert_eq!(record.stdout, "complete");
        Ok(())
    }

    #[test]
    fn timed_execution_rejects_invalid_deadlines_before_spawn() {
        let missing = CommandSpec::new("/absolute/program/that/must/not/exist");

        for timeout in [Duration::ZERO, MAX_COMMAND_TIMEOUT + Duration::from_secs(1)] {
            let error = ProcessExecutor
                .execute_with_timeout(&missing, timeout)
                .expect_err("invalid timeout must fail before spawn");
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn timeout_terminates_the_child_process_group() -> io::Result<()> {
        let python = Path::new("/usr/bin/python3");
        if !python.is_file() {
            return Ok(());
        }

        let fixture = timeout_fixture_directory()?;
        let marker = fixture.join("descendant-survived");
        let descendant = "import pathlib,sys,time; time.sleep(0.6); pathlib.Path(sys.argv[1]).write_text('survived')";
        let parent = "import subprocess,sys,time; subprocess.Popen([sys.executable, '-c', sys.argv[1], sys.argv[2]]); time.sleep(10)";
        let error = ProcessExecutor
            .execute_with_timeout(
                &CommandSpec::new(python)
                    .argument("-c")
                    .argument(parent)
                    .argument(descendant)
                    .argument(marker.to_string_lossy()),
                Duration::from_millis(100),
            )
            .expect_err("long-running process group must time out");

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        thread::sleep(Duration::from_millis(800));
        assert!(
            !marker.exists(),
            "ordinary descendant survived group timeout"
        );
        fs::remove_dir(&fixture)?;
        Ok(())
    }

    #[test]
    fn timeout_still_applies_after_the_child_closes_both_output_pipes() -> io::Result<()> {
        let python = Path::new("/usr/bin/python3");
        if !python.is_file() {
            return Ok(());
        }

        let error = ProcessExecutor
            .execute_with_timeout(
                &CommandSpec::new(python)
                    .argument("-c")
                    .argument("import os,time; os.close(1); os.close(2); time.sleep(10)"),
                Duration::from_millis(100),
            )
            .expect_err("closed output pipes must not bypass the command deadline");

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        Ok(())
    }

    #[test]
    fn timed_execution_preserves_output_limit_precedence() -> io::Result<()> {
        let yes = Path::new("/usr/bin/yes");
        if !yes.is_file() {
            return Ok(());
        }

        let error = ProcessExecutor
            .execute_with_timeout(
                &CommandSpec::new(yes).argument("x"),
                Duration::from_secs(10),
            )
            .expect_err("output exhaustion must abort before the later deadline");

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("stdout"));
        Ok(())
    }

    #[test]
    fn capture_thread_setup_failure_terminates_and_reaps_the_spawned_group() -> io::Result<()> {
        let python = Path::new("/usr/bin/python3");
        if !python.is_file() {
            return Ok(());
        }

        for fail_on_call in [1, 2] {
            let fixture = timeout_fixture_directory()?;
            let marker = fixture.join("process-survived-capture-setup-failure");
            let script = "import pathlib,sys,time; time.sleep(0.6); pathlib.Path(sys.argv[1]).write_text('survived'); time.sleep(10)";
            let error = execute_process_with_spawner(
                &CommandSpec::new(python)
                    .argument("-c")
                    .argument(script)
                    .argument(marker.to_string_lossy()),
                None,
                &FailingCaptureSpawner::new(fail_on_call),
            )
            .expect_err("capture thread setup failure must abort the spawned process group");

            assert_eq!(error.kind(), io::ErrorKind::Other);
            assert_eq!(error.to_string(), "injected capture thread failure");
            thread::sleep(Duration::from_millis(800));
            assert!(
                !marker.exists(),
                "spawned process survived capture thread setup failure"
            );
            fs::remove_dir(&fixture)?;
        }
        Ok(())
    }

    fn timeout_fixture_directory() -> io::Result<PathBuf> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "smolrunner-process-timeout-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path)?;
        Ok(path)
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
