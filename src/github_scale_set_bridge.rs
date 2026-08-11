// The adapter is intentionally private until the durable M3 consumer owns its lifecycle. Landing
// the complete process/credential boundary first keeps the future service from inventing a second
// protocol path; remove this allowance when that consumer is connected.
#![allow(dead_code)]

use std::collections::BTreeSet;
use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::github_scale_set_protocol::{
    ScaleSetJobId, ScaleSetJobResult, ScaleSetRunnerId, ScaleSetRunnerName, ScaleSetRunnerReference,
};
use crate::{disposable_worker_reconciler::ScaleSetDemand, execution_admission::EpochMillis};

const PROTOCOL_VERSION: u8 = 1;
const MAX_PROTOCOL_LINE_BYTES: usize = 128 * 1024;
const MAX_PRIVATE_KEY_BYTES: usize = 64 * 1024;
const MAX_JIT_CONFIG_BYTES: usize = 64 * 1024;
const MAX_EVENTS: usize = 50;
const MAX_LABELS: usize = 32;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct GitHubAppKeychainConfig {
    github_config_url: String,
    client_id: String,
    installation_id: u64,
    service: String,
    account: String,
}

impl GitHubAppKeychainConfig {
    pub(crate) fn new(
        github_config_url: &str,
        client_id: &str,
        installation_id: u64,
        service: &str,
        account: &str,
    ) -> Result<Self, ScaleSetBridgeError> {
        if !valid_github_config_url(github_config_url)
            || !bounded_token(client_id, 100)
            || installation_id == 0
            || !bounded_token(service, 128)
            || !bounded_token(account, 128)
        {
            return Err(ScaleSetBridgeError::new("invalid_github_app_identity"));
        }
        Ok(Self {
            github_config_url: github_config_url.to_owned(),
            client_id: client_id.to_owned(),
            installation_id,
            service: service.to_owned(),
            account: account.to_owned(),
        })
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ScaleSetBridgeTarget {
    id: u32,
    name: String,
    runner_group_id: u32,
    labels: Vec<String>,
    owner: String,
    max_capacity: u16,
}

impl ScaleSetBridgeTarget {
    pub(crate) fn new(
        id: u32,
        name: &str,
        runner_group_id: u32,
        labels: &[String],
        owner: &str,
        max_capacity: u16,
    ) -> Result<Self, ScaleSetBridgeError> {
        if id == 0
            || runner_group_id == 0
            || max_capacity != 1
            || !bounded_token(name, 100)
            || !bounded_token(owner, 100)
            || labels.is_empty()
            || labels.len() > 8
        {
            return Err(ScaleSetBridgeError::new("invalid_scale_set_target"));
        }
        let mut seen = BTreeSet::new();
        for label in labels {
            if !bounded_token(label, 100) || !seen.insert(label.as_str()) {
                return Err(ScaleSetBridgeError::new("invalid_scale_set_target"));
            }
        }
        Ok(Self {
            id,
            name: name.to_owned(),
            runner_group_id,
            labels: labels.to_vec(),
            owner: owner.to_owned(),
            max_capacity,
        })
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ScaleSetBridgeConfig {
    program: PathBuf,
    github_app: GitHubAppKeychainConfig,
    target: ScaleSetBridgeTarget,
}

impl ScaleSetBridgeConfig {
    pub(crate) fn new(
        program: &Path,
        github_app: GitHubAppKeychainConfig,
        target: ScaleSetBridgeTarget,
    ) -> Result<Self, ScaleSetBridgeError> {
        if !canonical_absolute_path(program) {
            return Err(ScaleSetBridgeError::new("invalid_bridge_program"));
        }
        Ok(Self {
            program: program.to_path_buf(),
            github_app,
            target,
        })
    }
}

struct GitHubAppPrivateKey(Vec<u8>);

impl GitHubAppPrivateKey {
    fn parse(bytes: Vec<u8>) -> Result<Self, ScaleSetBridgeError> {
        if bytes.is_empty()
            || bytes.len() > MAX_PRIVATE_KEY_BYTES
            || std::str::from_utf8(&bytes).is_err()
        {
            return Err(ScaleSetBridgeError::new("invalid_github_app_private_key"));
        }
        Ok(Self(bytes))
    }

    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("private key was validated as UTF-8")
    }
}

impl ScaleSetStatistics {
    pub(crate) fn demand(
        self,
        observed_at: EpochMillis,
        expires_at: EpochMillis,
    ) -> Result<ScaleSetDemand, ScaleSetBridgeError> {
        ScaleSetDemand::new(
            u16::try_from(self.assigned_jobs)
                .map_err(|_| ScaleSetBridgeError::new("invalid_bridge_demand"))?,
            u16::try_from(self.running_jobs)
                .map_err(|_| ScaleSetBridgeError::new("invalid_bridge_demand"))?,
            observed_at,
            expires_at,
        )
        .map_err(|_| ScaleSetBridgeError::new("invalid_bridge_demand"))
    }
}

impl Drop for GitHubAppPrivateKey {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

impl fmt::Debug for GitHubAppPrivateKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

#[cfg(target_os = "macos")]
fn load_keychain_private_key(
    config: &GitHubAppKeychainConfig,
) -> Result<GitHubAppPrivateKey, ScaleSetBridgeError> {
    let bytes =
        security_framework::passwords::get_generic_password(&config.service, &config.account)
            .map_err(|_| ScaleSetBridgeError::new("keychain_credential_unavailable"))?;
    GitHubAppPrivateKey::parse(bytes)
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct ScaleSetBridgeError {
    code: &'static str,
}

impl ScaleSetBridgeError {
    const fn new(code: &'static str) -> Self {
        Self { code }
    }

    pub(crate) const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for ScaleSetBridgeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScaleSetBridgeError")
            .field("code", &self.code)
            .finish()
    }
}

impl fmt::Display for ScaleSetBridgeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code)
    }
}

impl std::error::Error for ScaleSetBridgeError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScaleSetStatistics {
    pub(crate) available_jobs: u32,
    pub(crate) acquired_jobs: u32,
    pub(crate) assigned_jobs: u32,
    pub(crate) running_jobs: u32,
    pub(crate) registered_runners: u32,
    pub(crate) busy_runners: u32,
    pub(crate) idle_runners: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScaleSetBridgeJobEvidence {
    pub(crate) runner_request_id: u64,
    pub(crate) repository: String,
    pub(crate) owner: String,
    pub(crate) job_id: ScaleSetJobId,
    pub(crate) workflow_run_id: u64,
    pub(crate) request_labels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScaleSetBridgeEvent {
    Available(ScaleSetBridgeJobEvidence),
    Assigned(ScaleSetBridgeJobEvidence),
    Started {
        job: ScaleSetBridgeJobEvidence,
        runner: ScaleSetRunnerReference,
    },
    Completed {
        job: ScaleSetBridgeJobEvidence,
        runner: Option<ScaleSetRunnerReference>,
        result: ScaleSetJobResult,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScaleSetBridgePoll {
    Idle {
        statistics: ScaleSetStatistics,
    },
    Message {
        message_id: u32,
        statistics: ScaleSetStatistics,
        events: Vec<ScaleSetBridgeEvent>,
    },
}

pub(crate) struct EncodedJitConfig(Vec<u8>);

impl EncodedJitConfig {
    pub(crate) fn expose_to_guest_handoff(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for EncodedJitConfig {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

impl fmt::Debug for EncodedJitConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

pub(crate) struct ScaleSetJitReceipt {
    pub(crate) runner: ScaleSetRunnerReference,
    pub(crate) config: EncodedJitConfig,
}

trait BridgeTransport {
    fn exchange(&mut self, request: &BridgeRequest<'_>) -> Result<Vec<u8>, ScaleSetBridgeError>;
}

struct ChildBridgeTransport {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    poisoned: bool,
}

impl ChildBridgeTransport {
    fn spawn(program: &Path) -> Result<Self, ScaleSetBridgeError> {
        let mut command = Command::new(program);
        command
            .env_clear()
            .current_dir("/")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0);
        let mut child = command
            .spawn()
            .map_err(|_| ScaleSetBridgeError::new("bridge_spawn_failed"))?;
        let input = match child.stdin.take() {
            Some(input) => input,
            None => {
                terminate_child(&mut child);
                return Err(ScaleSetBridgeError::new("bridge_pipe_unavailable"));
            }
        };
        let output = match child.stdout.take() {
            Some(output) => output,
            None => {
                terminate_child(&mut child);
                return Err(ScaleSetBridgeError::new("bridge_pipe_unavailable"));
            }
        };
        Ok(Self {
            child,
            input,
            output: BufReader::new(output),
            poisoned: false,
        })
    }

    fn exchange_inner(
        &mut self,
        request: &BridgeRequest<'_>,
    ) -> Result<Vec<u8>, ScaleSetBridgeError> {
        if self.poisoned {
            return Err(ScaleSetBridgeError::new("bridge_session_poisoned"));
        }
        serde_json::to_writer(&mut self.input, request)
            .map_err(|_| ScaleSetBridgeError::new("bridge_request_failed"))?;
        self.input
            .write_all(b"\n")
            .and_then(|_| self.input.flush())
            .map_err(|_| ScaleSetBridgeError::new("bridge_request_failed"))?;

        let mut response = Vec::with_capacity(4_096);
        let mut bounded = self
            .output
            .by_ref()
            .take((MAX_PROTOCOL_LINE_BYTES + 1) as u64);
        let read = bounded
            .read_until(b'\n', &mut response)
            .map_err(|_| ScaleSetBridgeError::new("bridge_response_failed"))?;
        if read == 0 || response.len() > MAX_PROTOCOL_LINE_BYTES || response.last() != Some(&b'\n')
        {
            response.fill(0);
            return Err(ScaleSetBridgeError::new("bridge_response_invalid"));
        }
        response.pop();
        if response.last() == Some(&b'\r') {
            response.fill(0);
            return Err(ScaleSetBridgeError::new("bridge_response_invalid"));
        }
        Ok(response)
    }

    fn terminate(&mut self) {
        terminate_child(&mut self.child);
    }
}

impl BridgeTransport for ChildBridgeTransport {
    fn exchange(&mut self, request: &BridgeRequest<'_>) -> Result<Vec<u8>, ScaleSetBridgeError> {
        let result = self.exchange_inner(request);
        if result.is_err() {
            self.poisoned = true;
            self.terminate();
        }
        result
    }
}

impl Drop for ChildBridgeTransport {
    fn drop(&mut self) {
        self.terminate();
    }
}

fn terminate_child(child: &mut Child) {
    match child.try_wait() {
        Ok(Some(_)) => {}
        Ok(None) | Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub(crate) struct ScaleSetBridgeClient {
    transport: Box<dyn BridgeTransport>,
    target: ScaleSetBridgeTarget,
}

impl ScaleSetBridgeClient {
    #[cfg(target_os = "macos")]
    pub(crate) fn connect_from_keychain(
        config: ScaleSetBridgeConfig,
    ) -> Result<Self, ScaleSetBridgeError> {
        let private_key = load_keychain_private_key(&config.github_app)?;
        let transport = ChildBridgeTransport::spawn(&config.program)?;
        Self::connect_with_transport(config, private_key, Box::new(transport))
    }

    fn connect_with_transport(
        config: ScaleSetBridgeConfig,
        private_key: GitHubAppPrivateKey,
        mut transport: Box<dyn BridgeTransport>,
    ) -> Result<Self, ScaleSetBridgeError> {
        let request = BridgeRequest::start(&config, &private_key);
        let response = exchange_and_decode(transport.as_mut(), &request)?;
        expect_ready(&response, &config.target)?;
        Ok(Self {
            transport,
            target: config.target,
        })
    }

    pub(crate) fn poll(&mut self) -> Result<ScaleSetBridgePoll, ScaleSetBridgeError> {
        let mut response = exchange_and_decode(self.transport.as_mut(), &BridgeRequest::poll())?;
        match response.response_type.as_str() {
            "idle" => {
                response.require_idle_shape()?;
                Ok(ScaleSetBridgePoll::Idle {
                    statistics: response.require_statistics()?,
                })
            }
            "message" => {
                response.require_message_shape()?;
                let message_id = positive_u32(response.message_id, "invalid_bridge_message")?;
                if response.events.len() > MAX_EVENTS {
                    return Err(ScaleSetBridgeError::new("invalid_bridge_message"));
                }
                let statistics = response.require_statistics()?;
                let events = std::mem::take(&mut response.events)
                    .into_iter()
                    .map(|event| normalize_event(event, self.target.id))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(ScaleSetBridgePoll::Message {
                    message_id,
                    statistics,
                    events,
                })
            }
            "error" => Err(response.bridge_error()?),
            _ => Err(ScaleSetBridgeError::new("invalid_bridge_response")),
        }
    }

    pub(crate) fn ack(&mut self, message_id: u32) -> Result<Vec<u64>, ScaleSetBridgeError> {
        let mut response =
            exchange_and_decode(self.transport.as_mut(), &BridgeRequest::ack(message_id))?;
        if response.response_type == "error" {
            return Err(response.bridge_error()?);
        }
        if response.response_type != "acked"
            || response.message_id != Some(u64::from(message_id))
            || response.code.is_some()
            || response.scale_set_id.is_some()
            || response.statistics.is_some()
            || !response.events.is_empty()
            || response.runner.is_some()
            || response.encoded_jit_config.is_some()
        {
            return Err(ScaleSetBridgeError::new("invalid_bridge_response"));
        }
        let mut seen = BTreeSet::new();
        for id in &response.acquired_requests {
            if *id == 0 || !seen.insert(*id) {
                return Err(ScaleSetBridgeError::new("invalid_bridge_response"));
            }
        }
        Ok(std::mem::take(&mut response.acquired_requests))
    }

    pub(crate) fn generate_jit(
        &mut self,
        runner_name: &ScaleSetRunnerName,
    ) -> Result<ScaleSetJitReceipt, ScaleSetBridgeError> {
        let mut response = exchange_and_decode(
            self.transport.as_mut(),
            &BridgeRequest::runner("generate_jit", runner_name.as_str(), None, Some("_work")),
        )?;
        if response.response_type == "error" {
            return Err(response.bridge_error()?);
        }
        if response.response_type != "jit"
            || response.code.is_some()
            || response.scale_set_id.is_some()
            || response.message_id.is_some()
            || response.statistics.is_some()
            || !response.events.is_empty()
            || !response.acquired_requests.is_empty()
        {
            return Err(ScaleSetBridgeError::new("invalid_bridge_response"));
        }
        let runner = normalize_runner(
            response
                .runner
                .take()
                .ok_or_else(|| ScaleSetBridgeError::new("invalid_bridge_response"))?,
            self.target.id,
            Some(runner_name.as_str()),
        )?;
        let mut config = response
            .encoded_jit_config
            .take()
            .ok_or_else(|| ScaleSetBridgeError::new("invalid_bridge_response"))?
            .into_bytes();
        if config.is_empty() || config.len() > MAX_JIT_CONFIG_BYTES {
            config.fill(0);
            return Err(ScaleSetBridgeError::new("invalid_bridge_response"));
        }
        Ok(ScaleSetJitReceipt {
            runner,
            config: EncodedJitConfig(config),
        })
    }

    pub(crate) fn observe_runner(
        &mut self,
        runner_name: &ScaleSetRunnerName,
    ) -> Result<ScaleSetRunnerReference, ScaleSetBridgeError> {
        let mut response = exchange_and_decode(
            self.transport.as_mut(),
            &BridgeRequest::runner("observe_runner", runner_name.as_str(), None, None),
        )?;
        if response.response_type == "error" {
            return Err(response.bridge_error()?);
        }
        if response.response_type != "runner"
            || response.code.is_some()
            || response.scale_set_id.is_some()
            || response.message_id.is_some()
            || response.statistics.is_some()
            || !response.events.is_empty()
            || !response.acquired_requests.is_empty()
            || response.encoded_jit_config.is_some()
        {
            return Err(ScaleSetBridgeError::new("invalid_bridge_response"));
        }
        normalize_runner(
            response
                .runner
                .take()
                .ok_or_else(|| ScaleSetBridgeError::new("invalid_bridge_response"))?,
            self.target.id,
            Some(runner_name.as_str()),
        )
    }

    pub(crate) fn remove_runner(
        &mut self,
        runner: &ScaleSetRunnerReference,
    ) -> Result<(), ScaleSetBridgeError> {
        let mut response = exchange_and_decode(
            self.transport.as_mut(),
            &BridgeRequest::runner(
                "remove_runner",
                runner.name.as_str(),
                Some(runner.id.get()),
                None,
            ),
        )?;
        if response.response_type == "error" {
            return Err(response.bridge_error()?);
        }
        if response.response_type != "removed"
            || response.code.is_some()
            || response.scale_set_id.is_some()
            || response.message_id.is_some()
            || response.statistics.is_some()
            || !response.events.is_empty()
            || !response.acquired_requests.is_empty()
            || response.encoded_jit_config.is_some()
            || normalize_runner(
                response
                    .runner
                    .take()
                    .ok_or_else(|| ScaleSetBridgeError::new("invalid_bridge_response"))?,
                self.target.id,
                Some(runner.name.as_str()),
            )? != *runner
        {
            return Err(ScaleSetBridgeError::new("invalid_bridge_response"));
        }
        Ok(())
    }
}

fn exchange_and_decode(
    transport: &mut dyn BridgeTransport,
    request: &BridgeRequest<'_>,
) -> Result<BridgeResponse, ScaleSetBridgeError> {
    let mut bytes = transport.exchange(request)?;
    let result = decode_response(&bytes);
    bytes.fill(0);
    result
}

fn decode_response(bytes: &[u8]) -> Result<BridgeResponse, ScaleSetBridgeError> {
    if bytes.is_empty() || bytes.len() > MAX_PROTOCOL_LINE_BYTES {
        return Err(ScaleSetBridgeError::new("invalid_bridge_response"));
    }
    let response: BridgeResponse = serde_json::from_slice(bytes)
        .map_err(|_| ScaleSetBridgeError::new("invalid_bridge_response"))?;
    if response.version != PROTOCOL_VERSION {
        return Err(ScaleSetBridgeError::new("invalid_bridge_response"));
    }
    Ok(response)
}

#[derive(Serialize)]
struct BridgeRequest<'a> {
    version: u8,
    operation: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    start: Option<StartRequest<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runner_name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runner_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    work_folder: Option<&'a str>,
}

#[derive(Serialize)]
struct StartRequest<'a> {
    github_config_url: &'a str,
    client_id: &'a str,
    installation_id: u64,
    private_key: &'a str,
    scale_set_id: u32,
    scale_set_name: &'a str,
    runner_group_id: u32,
    labels: &'a [String],
    owner: &'a str,
    max_capacity: u16,
}

impl<'a> BridgeRequest<'a> {
    fn start(config: &'a ScaleSetBridgeConfig, key: &'a GitHubAppPrivateKey) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            operation: "start",
            start: Some(StartRequest {
                github_config_url: &config.github_app.github_config_url,
                client_id: &config.github_app.client_id,
                installation_id: config.github_app.installation_id,
                private_key: key.as_str(),
                scale_set_id: config.target.id,
                scale_set_name: &config.target.name,
                runner_group_id: config.target.runner_group_id,
                labels: &config.target.labels,
                owner: &config.target.owner,
                max_capacity: config.target.max_capacity,
            }),
            message_id: None,
            runner_name: None,
            runner_id: None,
            work_folder: None,
        }
    }

    const fn poll() -> Self {
        Self {
            version: PROTOCOL_VERSION,
            operation: "poll",
            start: None,
            message_id: None,
            runner_name: None,
            runner_id: None,
            work_folder: None,
        }
    }

    const fn ack(message_id: u32) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            operation: "ack",
            start: None,
            message_id: Some(message_id),
            runner_name: None,
            runner_id: None,
            work_folder: None,
        }
    }

    const fn runner(
        operation: &'a str,
        runner_name: &'a str,
        runner_id: Option<u64>,
        work_folder: Option<&'a str>,
    ) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            operation,
            start: None,
            message_id: None,
            runner_name: Some(runner_name),
            runner_id,
            work_folder,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BridgeResponse {
    version: u8,
    #[serde(rename = "type")]
    response_type: String,
    code: Option<String>,
    scale_set_id: Option<u64>,
    message_id: Option<u64>,
    statistics: Option<StatisticsWire>,
    #[serde(default)]
    events: Vec<EventWire>,
    #[serde(default)]
    acquired_requests: Vec<u64>,
    runner: Option<RunnerWire>,
    encoded_jit_config: Option<String>,
}

impl Drop for BridgeResponse {
    fn drop(&mut self) {
        if let Some(secret) = self.encoded_jit_config.take() {
            let mut bytes = secret.into_bytes();
            bytes.fill(0);
        }
    }
}

impl BridgeResponse {
    fn require_idle_shape(&self) -> Result<(), ScaleSetBridgeError> {
        if self.code.is_some()
            || self.scale_set_id.is_some()
            || self.message_id.is_some()
            || self.statistics.is_none()
            || !self.events.is_empty()
            || !self.acquired_requests.is_empty()
            || self.runner.is_some()
            || self.encoded_jit_config.is_some()
        {
            return Err(ScaleSetBridgeError::new("invalid_bridge_response"));
        }
        Ok(())
    }

    fn require_message_shape(&self) -> Result<(), ScaleSetBridgeError> {
        if self.code.is_some()
            || self.scale_set_id.is_some()
            || self.message_id.is_none()
            || self.statistics.is_none()
            || !self.acquired_requests.is_empty()
            || self.runner.is_some()
            || self.encoded_jit_config.is_some()
        {
            return Err(ScaleSetBridgeError::new("invalid_bridge_response"));
        }
        Ok(())
    }

    fn require_statistics(&self) -> Result<ScaleSetStatistics, ScaleSetBridgeError> {
        let wire = self
            .statistics
            .as_ref()
            .ok_or_else(|| ScaleSetBridgeError::new("invalid_bridge_response"))?;
        let statistics = ScaleSetStatistics {
            available_jobs: u32::try_from(wire.available_jobs)
                .map_err(|_| ScaleSetBridgeError::new("invalid_bridge_response"))?,
            acquired_jobs: u32::try_from(wire.acquired_jobs)
                .map_err(|_| ScaleSetBridgeError::new("invalid_bridge_response"))?,
            assigned_jobs: u32::try_from(wire.assigned_jobs)
                .map_err(|_| ScaleSetBridgeError::new("invalid_bridge_response"))?,
            running_jobs: u32::try_from(wire.running_jobs)
                .map_err(|_| ScaleSetBridgeError::new("invalid_bridge_response"))?,
            registered_runners: u32::try_from(wire.registered_runners)
                .map_err(|_| ScaleSetBridgeError::new("invalid_bridge_response"))?,
            busy_runners: u32::try_from(wire.busy_runners)
                .map_err(|_| ScaleSetBridgeError::new("invalid_bridge_response"))?,
            idle_runners: u32::try_from(wire.idle_runners)
                .map_err(|_| ScaleSetBridgeError::new("invalid_bridge_response"))?,
        };
        let classified_runners = statistics
            .busy_runners
            .checked_add(statistics.idle_runners)
            .ok_or_else(|| ScaleSetBridgeError::new("invalid_bridge_response"))?;
        if statistics.running_jobs > statistics.assigned_jobs
            || statistics.busy_runners > statistics.registered_runners
            || statistics.idle_runners > statistics.registered_runners
            || classified_runners > statistics.registered_runners
        {
            return Err(ScaleSetBridgeError::new("invalid_bridge_response"));
        }
        Ok(statistics)
    }

    fn bridge_error(&self) -> Result<ScaleSetBridgeError, ScaleSetBridgeError> {
        if self.response_type != "error"
            || self.scale_set_id.is_some()
            || self.message_id.is_some()
            || self.statistics.is_some()
            || !self.events.is_empty()
            || !self.acquired_requests.is_empty()
            || self.runner.is_some()
            || self.encoded_jit_config.is_some()
        {
            return Err(ScaleSetBridgeError::new("invalid_bridge_response"));
        }
        let code = self
            .code
            .as_deref()
            .and_then(known_bridge_error)
            .ok_or_else(|| ScaleSetBridgeError::new("invalid_bridge_response"))?;
        Ok(ScaleSetBridgeError::new(code))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatisticsWire {
    available_jobs: u64,
    acquired_jobs: u64,
    assigned_jobs: u64,
    running_jobs: u64,
    registered_runners: u64,
    busy_runners: u64,
    idle_runners: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventWire {
    kind: String,
    runner_request_id: u64,
    repository: String,
    owner: String,
    job_id: String,
    workflow_run_id: u64,
    request_labels: Vec<String>,
    runner_id: Option<u64>,
    runner_name: Option<String>,
    result: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunnerWire {
    id: u64,
    name: String,
    scale_set_id: u64,
}

fn expect_ready(
    response: &BridgeResponse,
    target: &ScaleSetBridgeTarget,
) -> Result<(), ScaleSetBridgeError> {
    if response.response_type == "error" {
        return Err(response.bridge_error()?);
    }
    if response.response_type != "ready"
        || response.scale_set_id != Some(u64::from(target.id))
        || response.code.is_some()
        || response.message_id.is_some()
        || response.statistics.is_none()
        || !response.events.is_empty()
        || !response.acquired_requests.is_empty()
        || response.runner.is_some()
        || response.encoded_jit_config.is_some()
    {
        return Err(ScaleSetBridgeError::new("invalid_bridge_response"));
    }
    response.require_statistics().map(|_| ())
}

fn normalize_event(
    wire: EventWire,
    scale_set_id: u32,
) -> Result<ScaleSetBridgeEvent, ScaleSetBridgeError> {
    if wire.runner_request_id == 0
        || wire.workflow_run_id == 0
        || !bounded_token(&wire.repository, 100)
        || !bounded_token(&wire.owner, 100)
        || wire.request_labels.len() > MAX_LABELS
        || wire
            .request_labels
            .iter()
            .any(|label| !bounded_token(label, 100))
    {
        return Err(ScaleSetBridgeError::new("invalid_bridge_event"));
    }
    let job = ScaleSetBridgeJobEvidence {
        runner_request_id: wire.runner_request_id,
        repository: wire.repository,
        owner: wire.owner,
        job_id: ScaleSetJobId::parse(&wire.job_id)
            .map_err(|_| ScaleSetBridgeError::new("invalid_bridge_event"))?,
        workflow_run_id: wire.workflow_run_id,
        request_labels: wire.request_labels,
    };
    let runner = match (wire.runner_id, wire.runner_name) {
        (Some(id), Some(name)) => Some(normalize_runner(
            RunnerWire {
                id,
                name,
                scale_set_id: u64::from(scale_set_id),
            },
            scale_set_id,
            None,
        )?),
        (None, None) => None,
        _ => return Err(ScaleSetBridgeError::new("invalid_bridge_event")),
    };
    match wire.kind.as_str() {
        "available" if runner.is_none() && wire.result.is_none() => {
            Ok(ScaleSetBridgeEvent::Available(job))
        }
        "assigned" if runner.is_none() && wire.result.is_none() => {
            Ok(ScaleSetBridgeEvent::Assigned(job))
        }
        "started" if runner.is_some() && wire.result.is_none() => {
            Ok(ScaleSetBridgeEvent::Started {
                job,
                runner: runner.expect("runner presence checked"),
            })
        }
        "completed" => {
            let result = ScaleSetJobResult::parse(
                wire.result
                    .as_deref()
                    .ok_or_else(|| ScaleSetBridgeError::new("invalid_bridge_event"))?,
            )
            .map_err(|_| ScaleSetBridgeError::new("invalid_bridge_event"))?;
            if runner.is_none() && result.as_str() != "canceled" {
                return Err(ScaleSetBridgeError::new("invalid_bridge_event"));
            }
            Ok(ScaleSetBridgeEvent::Completed {
                job,
                runner,
                result,
            })
        }
        _ => Err(ScaleSetBridgeError::new("invalid_bridge_event")),
    }
}

fn normalize_runner(
    wire: RunnerWire,
    scale_set_id: u32,
    expected_name: Option<&str>,
) -> Result<ScaleSetRunnerReference, ScaleSetBridgeError> {
    if wire.scale_set_id != u64::from(scale_set_id)
        || expected_name.is_some_and(|expected| expected != wire.name)
    {
        return Err(ScaleSetBridgeError::new("invalid_bridge_runner"));
    }
    Ok(ScaleSetRunnerReference::new(
        ScaleSetRunnerId::new(wire.id)
            .map_err(|_| ScaleSetBridgeError::new("invalid_bridge_runner"))?,
        ScaleSetRunnerName::parse(&wire.name)
            .map_err(|_| ScaleSetBridgeError::new("invalid_bridge_runner"))?,
    ))
}

fn positive_u32(value: Option<u64>, code: &'static str) -> Result<u32, ScaleSetBridgeError> {
    value
        .filter(|value| *value > 0)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| ScaleSetBridgeError::new(code))
}

fn known_bridge_error(code: &str) -> Option<&'static str> {
    Some(match code {
        "unsupported_operation" => "unsupported_operation",
        "already_started" => "already_started",
        "invalid_start" => "invalid_start",
        "start_failed" => "start_failed",
        "not_started" => "not_started",
        "ack_required" => "ack_required",
        "poll_failed" => "poll_failed",
        "invalid_message" => "invalid_message",
        "message_mismatch" => "message_mismatch",
        "ack_failed" => "ack_failed",
        "acquire_failed" => "acquire_failed",
        "invalid_acquisition" => "invalid_acquisition",
        "invalid_runner" => "invalid_runner",
        "scale_set_drift" => "scale_set_drift",
        "jit_failed" => "jit_failed",
        "runner_unavailable" => "runner_unavailable",
        "remove_failed" => "remove_failed",
        "response_too_large" => "response_too_large",
        _ => return None,
    })
}

fn valid_github_config_url(value: &str) -> bool {
    let Some(path) = value.strip_prefix("https://github.com/") else {
        return false;
    };
    let parts = path.split('/').collect::<Vec<_>>();
    (parts.len() == 1 || parts.len() == 2) && parts.iter().all(|part| bounded_token(part, 100))
}

fn bounded_token(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

fn canonical_absolute_path(path: &Path) -> bool {
    let Some(raw) = path.to_str() else {
        return false;
    };
    raw.starts_with('/')
        && raw.len() > 1
        && !raw.ends_with('/')
        && raw
            .split('/')
            .skip(1)
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    struct ScriptedTransport {
        responses: VecDeque<Vec<u8>>,
        requests: Vec<Vec<u8>>,
    }

    impl ScriptedTransport {
        fn new(responses: &[&str]) -> Self {
            Self {
                responses: responses
                    .iter()
                    .map(|response| response.as_bytes().to_vec())
                    .collect(),
                requests: Vec::new(),
            }
        }
    }

    impl BridgeTransport for ScriptedTransport {
        fn exchange(
            &mut self,
            request: &BridgeRequest<'_>,
        ) -> Result<Vec<u8>, ScaleSetBridgeError> {
            self.requests.push(
                serde_json::to_vec(request)
                    .map_err(|_| ScaleSetBridgeError::new("script_encode_failed"))?,
            );
            self.responses
                .pop_front()
                .ok_or_else(|| ScaleSetBridgeError::new("script_exhausted"))
        }
    }

    fn config() -> ScaleSetBridgeConfig {
        ScaleSetBridgeConfig::new(
            Path::new("/opt/smolrunner/bin/scaleset-bridge"),
            GitHubAppKeychainConfig::new(
                "https://github.com/example/project",
                "Iv1.example",
                17,
                "dev.smolrunner.github-app",
                "example-project",
            )
            .unwrap(),
            ScaleSetBridgeTarget::new(
                23,
                "smolrunner",
                1,
                &["smolrunner".to_owned()],
                "smolrunner-host",
                1,
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn keychain_and_program_configuration_fail_closed() {
        assert!(
            GitHubAppKeychainConfig::new(
                "http://github.com/example",
                "Iv1.example",
                17,
                "service",
                "account"
            )
            .is_err()
        );
        assert!(
            ScaleSetBridgeConfig::new(
                Path::new("/opt/./bridge"),
                config().github_app,
                config().target
            )
            .is_err()
        );
        assert!(
            ScaleSetBridgeTarget::new(
                23,
                "smolrunner",
                1,
                &["smolrunner".to_owned()],
                "smolrunner-host",
                0,
            )
            .is_err()
        );
        assert!(
            ScaleSetBridgeTarget::new(
                23,
                "smolrunner",
                1,
                &["smolrunner".to_owned()],
                "smolrunner-host",
                2,
            )
            .is_err()
        );
        let key = GitHubAppPrivateKey::parse(b"private-key".to_vec()).unwrap();
        assert_eq!(format!("{key:?}"), "[REDACTED]");
    }

    #[test]
    fn session_maps_demand_events_and_exact_ack() {
        let transport = ScriptedTransport::new(&[
            r#"{"version":1,"type":"ready","scale_set_id":23,"statistics":{"available_jobs":0,"acquired_jobs":0,"assigned_jobs":0,"running_jobs":0,"registered_runners":0,"busy_runners":0,"idle_runners":0}}"#,
            r#"{"version":1,"type":"message","message_id":7,"statistics":{"available_jobs":1,"acquired_jobs":0,"assigned_jobs":1,"running_jobs":0,"registered_runners":0,"busy_runners":0,"idle_runners":0},"events":[{"kind":"available","runner_request_id":41,"repository":"project","owner":"example","job_id":"job-1","workflow_run_id":99,"request_labels":["smolrunner"]}]}"#,
            r#"{"version":1,"type":"acked","message_id":7,"acquired_requests":[41]}"#,
        ]);
        let mut client = ScaleSetBridgeClient::connect_with_transport(
            config(),
            GitHubAppPrivateKey::parse(b"private-key".to_vec()).unwrap(),
            Box::new(transport),
        )
        .unwrap();
        let ScaleSetBridgePoll::Message {
            message_id,
            statistics,
            events,
        } = client.poll().unwrap()
        else {
            panic!("expected message")
        };
        assert_eq!(message_id, 7);
        assert_eq!(statistics.assigned_jobs, 1);
        statistics
            .demand(
                EpochMillis::new(1_000).unwrap(),
                EpochMillis::new(1_030).unwrap(),
            )
            .unwrap();
        assert!(matches!(
            events.as_slice(),
            [ScaleSetBridgeEvent::Available(_)]
        ));
        assert_eq!(client.ack(7).unwrap(), vec![41]);
    }

    #[test]
    fn jit_secret_is_redacted_and_runner_is_exact() {
        let transport = ScriptedTransport::new(&[
            r#"{"version":1,"type":"ready","scale_set_id":23,"statistics":{"available_jobs":0,"acquired_jobs":0,"assigned_jobs":0,"running_jobs":0,"registered_runners":0,"busy_runners":0,"idle_runners":0}}"#,
            r#"{"version":1,"type":"jit","runner":{"id":81,"name":"smolrunner-job-1","scale_set_id":23},"encoded_jit_config":"one-time-secret"}"#,
        ]);
        let mut client = ScaleSetBridgeClient::connect_with_transport(
            config(),
            GitHubAppPrivateKey::parse(b"private-key".to_vec()).unwrap(),
            Box::new(transport),
        )
        .unwrap();
        let receipt = client
            .generate_jit(&ScaleSetRunnerName::parse("smolrunner-job-1").unwrap())
            .unwrap();
        assert_eq!(receipt.runner.id.get(), 81);
        assert_eq!(format!("{:?}", receipt.config), "[REDACTED]");
        assert_eq!(receipt.config.expose_to_guest_handoff(), b"one-time-secret");
    }

    #[test]
    fn unknown_response_fields_and_runnerless_non_cancel_fail_closed() {
        let unknown_error = match decode_response(br#"{"version":1,"type":"idle","unknown":true}"#)
        {
            Ok(_) => panic!("unknown response field was accepted"),
            Err(error) => error,
        };
        assert_eq!(unknown_error.code(), "invalid_bridge_response");
        let mut response = decode_response(
            br#"{"version":1,"type":"message","message_id":7,"statistics":{"available_jobs":0,"acquired_jobs":0,"assigned_jobs":0,"running_jobs":0,"registered_runners":0,"busy_runners":0,"idle_runners":0},"events":[{"kind":"completed","runner_request_id":41,"repository":"project","owner":"example","job_id":"job-1","workflow_run_id":99,"request_labels":["smolrunner"],"result":"failed"}]}"#,
        )
        .unwrap();
        assert_eq!(
            normalize_event(
                std::mem::take(&mut response.events)
                    .into_iter()
                    .next()
                    .unwrap(),
                23,
            )
            .unwrap_err()
            .code(),
            "invalid_bridge_event"
        );
    }
}
