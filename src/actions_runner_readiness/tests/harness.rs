use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::lima_observation::{
    LimaArchitecture, LimaConfiguredInstance, LimaGuestResources, LimaObservedGuest,
    LimaPersistentIdentity, LimaVmType,
};
use crate::process::{CommandExecutor, CommandSpec, CommandValue, ExecutionRecord};

use super::*;

const LIMA_HOME: &str = "/Users/operator/.lima";
const RUNNER_ROOT: &str = "/home/runner/actions-runner";
const DRAIN_MARKER: &str = "/home/runner/actions-runner/.smolrunner-draining";
const CONFIG_HEX: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const LISTENER_PID: u32 = 42;
const WORKER_PID: u32 = 43;

#[derive(Debug)]
enum ScriptedStep {
    Output(ScriptedOutput),
    IoError(io::ErrorKind),
}

#[derive(Debug)]
struct ScriptedOutput {
    stdout: String,
    stderr: String,
    status: Option<i32>,
    success: bool,
    argv_override: Option<Vec<String>>,
    environment_override: Option<Vec<String>>,
}

impl ScriptedOutput {
    fn success(stdout: impl Into<String>) -> Self {
        Self {
            stdout: stdout.into(),
            stderr: String::new(),
            status: Some(0),
            success: true,
            argv_override: None,
            environment_override: None,
        }
    }

    fn absent() -> Self {
        Self {
            stdout: String::new(),
            stderr: String::new(),
            status: Some(1),
            success: false,
            argv_override: None,
            environment_override: None,
        }
    }
}

#[derive(Debug, Default)]
struct ScriptedExecutor {
    steps: Mutex<VecDeque<ScriptedStep>>,
    seen: Mutex<Vec<CommandSpec>>,
}

impl ScriptedExecutor {
    fn new(steps: impl IntoIterator<Item = ScriptedStep>) -> Self {
        Self {
            steps: Mutex::new(steps.into_iter().collect()),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn seen(&self) -> Vec<CommandSpec> {
        self.seen.lock().expect("seen lock").clone()
    }

    fn remaining(&self) -> usize {
        self.steps.lock().expect("steps lock").len()
    }
}

impl CommandExecutor for ScriptedExecutor {
    fn execute(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord> {
        self.seen.lock().expect("seen lock").push(spec.clone());
        match self
            .steps
            .lock()
            .expect("steps lock")
            .pop_front()
            .expect("scripted runner readiness command")
        {
            ScriptedStep::IoError(kind) => Err(io::Error::new(kind, "private scripted failure")),
            ScriptedStep::Output(output) => Ok(ExecutionRecord {
                argv: output
                    .argv_override
                    .unwrap_or_else(|| spec.displayed_argv()),
                environment_keys: output
                    .environment_override
                    .unwrap_or_else(|| spec.environment.keys().cloned().collect::<Vec<_>>()),
                status: output.status,
                success: output.success,
                stdout: output.stdout,
                stderr: output.stderr,
            }),
        }
    }
}

#[derive(Debug)]
struct FakeClock {
    values: Mutex<VecDeque<io::Result<u64>>>,
}

impl FakeClock {
    fn new(values: impl IntoIterator<Item = u64>) -> Self {
        Self {
            values: Mutex::new(values.into_iter().map(Ok).collect()),
