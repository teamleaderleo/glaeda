//! Pure sealed invocation planning for one resident Linux guest-control transaction.
//!
//! The protocol request is already canonical and authority-free. This module adds one second seal:
//! a verified Mac-side target binding that names the exact resident sandbox, reviewed `limactl`
//! generation, private Lima home, and exact installed guest binary. Product callers cannot construct
//! that target binding in this slice; a later read-only prerequisite observer will mint it only from
//! fresh descriptor/executable evidence.
//!
//! Planning performs no process execution, filesystem I/O, Lima observation, privilege escalation,
//! guest mutation, or durable-state mutation.

use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::artifact::Sha256Digest;
use crate::lima_observation::{LIMACTL_SAFE_HOME, LIMACTL_SAFE_PATH, LimaInstanceName};
use crate::process::CommandSpec;
use crate::project_disk_lease::{ResidentSandboxGeneration, ResidentSandboxId};
use crate::trusted_guest_control_protocol::{
    MAX_TRUSTED_GUEST_CONTROL_RECEIPT_BYTES, TrustedGuestControlBinaryBinding,
    TrustedGuestControlOperation, TrustedGuestControlRequest, encode_trusted_guest_control_request,
    trusted_guest_control_request_digest,
};

pub const TRUSTED_GUEST_CONTROL_INVOCATION_PLAN_SCHEMA_VERSION: u8 = 1;
pub const TRUSTED_GUEST_CONTROL_INVOCATION_TIMEOUT: Duration = Duration::from_secs(120);
pub const MAX_TRUSTED_GUEST_CONTROL_STDOUT_BYTES: usize = MAX_TRUSTED_GUEST_CONTROL_RECEIPT_BYTES;
pub const MAX_TRUSTED_GUEST_CONTROL_STDERR_BYTES: usize = 64 * 1024;

const SUDO: &str = "/usr/bin/sudo";
const ENV: &str = "/usr/bin/env";
const GUEST_HOME: &str = "/root";
const GUEST_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
const GUEST_CONTROL_MODE: &str = "guest-control";
const GUEST_CONTROL_STDIO: &str = "--stdio";
#[allow(dead_code)]
const MAX_PRIVATE_PATH_BYTES: usize = 1_024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrustedGuestControlInvocationSummary {
    schema_version: u8,
    sandbox_id: ResidentSandboxId,
    sandbox_generation: ResidentSandboxGeneration,
    limactl_generation: u64,
    limactl_digest: Sha256Digest,
    guest_binary_generation: u64,
    guest_binary_digest: Sha256Digest,
    operation: TrustedGuestControlOperation,
    request_digest: Sha256Digest,
    timeout_seconds: u64,
    stdout_limit_bytes: usize,
    stderr_limit_bytes: usize,
}

impl TrustedGuestControlInvocationSummary {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn sandbox_id(&self) -> &ResidentSandboxId {
        &self.sandbox_id
    }

    #[must_use]
    pub const fn sandbox_generation(&self) -> ResidentSandboxGeneration {
        self.sandbox_generation
    }

    #[must_use]
    pub const fn limactl_generation(&self) -> u64 {
        self.limactl_generation
    }

    #[must_use]
    pub const fn limactl_digest(&self) -> &Sha256Digest {
        &self.limactl_digest
    }

    #[must_use]
    pub const fn guest_binary_generation(&self) -> u64 {
        self.guest_binary_generation
    }

    #[must_use]
    pub const fn guest_binary_digest(&self) -> &Sha256Digest {
        &self.guest_binary_digest
    }

    #[must_use]
    pub const fn operation(&self) -> TrustedGuestControlOperation {
        self.operation
    }

    #[must_use]
    pub const fn request_digest(&self) -> &Sha256Digest {
        &self.request_digest
    }

    #[must_use]
    pub const fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_seconds)
    }

    #[must_use]
    pub const fn stdout_limit_bytes(&self) -> usize {
        self.stdout_limit_bytes
    }

    #[must_use]
    pub const fn stderr_limit_bytes(&self) -> usize {
        self.stderr_limit_bytes
    }
}

/// Exact verified host/guest target for one later invocation.
///
/// There is intentionally no public constructor. The later P3 observer/adapter must verify these
/// private paths and executable identities, prove that `instance` is the named resident sandbox
/// generation, and only then call the crate-private constructor immediately before planning.
pub struct TrustedGuestControlInvocationTarget {
    limactl_program: PathBuf,
    lima_home: PathBuf,
    instance: LimaInstanceName,
    sandbox_id: ResidentSandboxId,
    sandbox_generation: ResidentSandboxGeneration,
    limactl_generation: u64,
    limactl_digest: Sha256Digest,
    guest_binary_path: PathBuf,
    guest_binary: TrustedGuestControlBinaryBinding,
}

impl TrustedGuestControlInvocationTarget {
    #[allow(dead_code, clippy::too_many_arguments)]
    pub(crate) fn from_verified(
        limactl_program: PathBuf,
        lima_home: PathBuf,
        instance: LimaInstanceName,
        sandbox_id: ResidentSandboxId,
        sandbox_generation: ResidentSandboxGeneration,
        limactl_generation: u64,
        limactl_digest: Sha256Digest,
        guest_binary_path: PathBuf,
        guest_binary: TrustedGuestControlBinaryBinding,
    ) -> Result<Self, TrustedGuestControlInvocationPlanError> {
        if limactl_generation == 0 {
            return Err(invalid_target());
        }
        Ok(Self {
            limactl_program: validate_private_absolute_path(limactl_program)?,
            lima_home: validate_private_absolute_path(lima_home)?,
            instance,
            sandbox_id,
            sandbox_generation,
            limactl_generation,
            limactl_digest,
            guest_binary_path: validate_private_absolute_path(guest_binary_path)?,
            guest_binary,
        })
    }
}

impl fmt::Debug for TrustedGuestControlInvocationTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedGuestControlInvocationTarget")
            .field("sandbox_id", &self.sandbox_id)
            .field("sandbox_generation", &self.sandbox_generation)
            .field("limactl_generation", &self.limactl_generation)
            .field("limactl_digest", &self.limactl_digest)
            .field("guest_binary", &self.guest_binary)
            .field("limactl_program", &"<private-reviewed-limactl>")
            .field("lima_home", &"<private-lima-home>")
            .field("instance", &"<private-lima-instance>")
            .field("guest_binary_path", &"<private-reviewed-guest-binary>")
            .finish()
    }
}

pub struct TrustedGuestControlInvocationPlan {
    summary: TrustedGuestControlInvocationSummary,
    #[allow(dead_code)]
    command: CommandSpec,
    #[allow(dead_code)]
    stdin: Vec<u8>,
}

impl TrustedGuestControlInvocationPlan {
    #[must_use]
    pub const fn summary(&self) -> &TrustedGuestControlInvocationSummary {
        &self.summary
    }

    #[must_use]
    #[allow(dead_code)]
    pub(crate) const fn command(&self) -> &CommandSpec {
        &self.command
    }

    #[must_use]
    #[allow(dead_code)]
    pub(crate) fn stdin(&self) -> &[u8] {
        &self.stdin
    }
}

impl fmt::Debug for TrustedGuestControlInvocationPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedGuestControlInvocationPlan")
            .field("summary", &self.summary)
            .field("command", &"<private-fixed-one-shot-command>")
            .field("stdin", &"<canonical-bounded-guest-request>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustedGuestControlInvocationPlanErrorKind {
    InvalidTarget,
    AuthorityMismatch,
    BinaryMismatch,
    Protocol,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TrustedGuestControlInvocationPlanError {
    kind: TrustedGuestControlInvocationPlanErrorKind,
    code: &'static str,
    message: &'static str,
}

impl TrustedGuestControlInvocationPlanError {
    #[must_use]
    pub const fn kind(self) -> TrustedGuestControlInvocationPlanErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for TrustedGuestControlInvocationPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedGuestControlInvocationPlanError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for TrustedGuestControlInvocationPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for TrustedGuestControlInvocationPlanError {}

/// Seal one exact one-shot command plus canonical request bytes.
///
/// The returned [`CommandSpec`] is still data. This function never executes it. The later Mac
/// adapter must freshly reconfirm the durable attachment, verified target, request digest, and
/// executable identities immediately before using a bounded stdin/stdout/stderr process boundary.
///
/// # Errors
///
/// Returns a bounded error when the verified target names another sandbox generation or guest
/// binary, or when the canonical protocol request cannot be encoded.
pub fn plan_trusted_guest_control_invocation(
    request: &TrustedGuestControlRequest,
    target: &TrustedGuestControlInvocationTarget,
) -> Result<TrustedGuestControlInvocationPlan, TrustedGuestControlInvocationPlanError> {
    if request.authority().resident_sandbox_id() != &target.sandbox_id
        || request.authority().resident_sandbox_generation() != target.sandbox_generation
    {
        return Err(authority_mismatch());
    }
    if request.binary() != &target.guest_binary {
        return Err(binary_mismatch());
    }

    let stdin = encode_trusted_guest_control_request(request).map_err(|_| protocol_error())?;
    let request_digest =
        trusted_guest_control_request_digest(request).map_err(|_| protocol_error())?;
    let command = CommandSpec::new(&target.limactl_program)
        .argument("--tty=false")
        .environment("HOME", LIMACTL_SAFE_HOME)
        .secret_environment(
            "LIMA_HOME",
            target
                .lima_home
                .to_str()
                .expect("validated private Lima home remains UTF-8"),
        )
        .environment("LANG", "C")
        .environment("LC_ALL", "C")
        .environment("PATH", LIMACTL_SAFE_PATH)
        .argument("shell")
        .argument(target.instance.as_str())
        .argument(SUDO)
        .argument("--non-interactive")
        .argument("--")
        .argument(ENV)
        .argument("-i")
        .argument(format!("HOME={GUEST_HOME}"))
        .argument(format!("PATH={GUEST_PATH}"))
        .argument(
            target
                .guest_binary_path
                .to_str()
                .expect("validated guest binary path remains UTF-8"),
        )
        .argument(GUEST_CONTROL_MODE)
        .argument(GUEST_CONTROL_STDIO);

    Ok(TrustedGuestControlInvocationPlan {
        summary: TrustedGuestControlInvocationSummary {
            schema_version: TRUSTED_GUEST_CONTROL_INVOCATION_PLAN_SCHEMA_VERSION,
            sandbox_id: target.sandbox_id.clone(),
            sandbox_generation: target.sandbox_generation,
            limactl_generation: target.limactl_generation,
            limactl_digest: target.limactl_digest.clone(),
            guest_binary_generation: target.guest_binary.generation(),
            guest_binary_digest: target.guest_binary.digest().clone(),
            operation: request.operation(),
            request_digest,
            timeout_seconds: TRUSTED_GUEST_CONTROL_INVOCATION_TIMEOUT.as_secs(),
            stdout_limit_bytes: MAX_TRUSTED_GUEST_CONTROL_STDOUT_BYTES,
            stderr_limit_bytes: MAX_TRUSTED_GUEST_CONTROL_STDERR_BYTES,
        },
        command,
        stdin,
    })
}

#[allow(dead_code)]
fn validate_private_absolute_path(
    path: PathBuf,
) -> Result<PathBuf, TrustedGuestControlInvocationPlanError> {
    if !path.is_absolute()
        || path == Path::new("/")
        || path.as_os_str().len() > MAX_PRIVATE_PATH_BYTES
        || path.to_str().is_none()
    {
        return Err(invalid_target());
    }
    for component in path.components() {
        if !matches!(component, Component::RootDir | Component::Normal(_)) {
            return Err(invalid_target());
        }
    }
    Ok(path)
}

const fn plan_error(
    kind: TrustedGuestControlInvocationPlanErrorKind,
    code: &'static str,
    message: &'static str,
) -> TrustedGuestControlInvocationPlanError {
    TrustedGuestControlInvocationPlanError {
        kind,
        code,
        message,
    }
}

#[allow(dead_code)]
const fn invalid_target() -> TrustedGuestControlInvocationPlanError {
    plan_error(
        TrustedGuestControlInvocationPlanErrorKind::InvalidTarget,
        "trusted_guest_control_invocation_target_invalid",
        "trusted guest-control invocation target is invalid",
    )
}

const fn authority_mismatch() -> TrustedGuestControlInvocationPlanError {
    plan_error(
        TrustedGuestControlInvocationPlanErrorKind::AuthorityMismatch,
        "trusted_guest_control_invocation_authority_mismatch",
        "trusted guest-control request does not match the verified resident sandbox",
    )
}

const fn binary_mismatch() -> TrustedGuestControlInvocationPlanError {
    plan_error(
        TrustedGuestControlInvocationPlanErrorKind::BinaryMismatch,
        "trusted_guest_control_invocation_binary_mismatch",
        "trusted guest-control request does not match the verified guest binary",
    )
}

const fn protocol_error() -> TrustedGuestControlInvocationPlanError {
    plan_error(
        TrustedGuestControlInvocationPlanErrorKind::Protocol,
        "trusted_guest_control_invocation_protocol_invalid",
        "trusted guest-control request cannot be encoded for invocation",
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::artifact::Sha256Digest;
    use crate::project_catalog::ProjectIdentity;
    use crate::project_disk_lease::{
        ProjectDiskGeneration, ProjectDiskId, ProjectDiskLockObservation, ProjectDiskObservation,
        ProjectDiskPhysicalObservation, ProjectDiskRecoverability, ProjectDiskUseObservation,
        ResidentSandboxGeneration, ResidentSandboxId,
    };
    use crate::trusted_guest_control_protocol::{
        TrustedGuestControlArchitecture, TrustedGuestControlAuthority, TrustedGuestControlRequestId,
    };

    fn digest(byte: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    fn request() -> TrustedGuestControlRequest {
        let detached = crate::project_disk_lease::ProjectDiskLeaseRecord::new_detached(
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            ProjectDiskId::parse("disk-a").unwrap(),
            ProjectDiskGeneration::new(3).unwrap(),
        );
        let plan = detached
            .plan_attach(
                ResidentSandboxId::parse("sandbox-a").unwrap(),
                ResidentSandboxGeneration::new(11).unwrap(),
                ProjectDiskObservation::new(
                    ProjectDiskPhysicalObservation::Exact,
                    ProjectDiskUseObservation::Unused,
                    ProjectDiskLockObservation::Unlocked,
                    ProjectDiskRecoverability::Rebuildable,
                ),
            )
            .unwrap();
        let attached = detached
            .record_attach_success(
                &plan,
                ProjectDiskObservation::new(
                    ProjectDiskPhysicalObservation::Exact,
                    ProjectDiskUseObservation::CurrentAttachment,
                    ProjectDiskLockObservation::CurrentAttachment,
                    ProjectDiskRecoverability::Rebuildable,
                ),
            )
            .unwrap();
        TrustedGuestControlRequest::new(
            TrustedGuestControlRequestId::parse("request-1").unwrap(),
            TrustedGuestControlBinaryBinding::new(
                7,
                digest('a'),
                TrustedGuestControlArchitecture::LinuxAarch64,
            )
            .unwrap(),
            TrustedGuestControlAuthority::from_attached_project_disk(&attached).unwrap(),
            TrustedGuestControlOperation::PrepareTrustedTaskView,
            digest('b'),
        )
    }

    fn target(request: &TrustedGuestControlRequest) -> TrustedGuestControlInvocationTarget {
        TrustedGuestControlInvocationTarget::from_verified(
            PathBuf::from("/opt/homebrew/bin/limactl"),
            PathBuf::from("/private/var/smolrunner/lima"),
            LimaInstanceName::parse("resident-a").unwrap(),
            request.authority().resident_sandbox_id().clone(),
            request.authority().resident_sandbox_generation(),
            5,
            digest('c'),
            PathBuf::from("/opt/smolrunner/bin/smolrunner"),
            request.binary().clone(),
        )
        .unwrap()
    }

    fn plain_arguments(command: &CommandSpec) -> Vec<String> {
        command
            .arguments
            .iter()
            .map(|value| match value {
                crate::process::CommandValue::Plain(value) => value.clone(),
                crate::process::CommandValue::Secret(_) => "<secret>".to_owned(),
            })
            .collect()
    }

    #[test]
    fn exact_plan_uses_one_closed_lima_sudo_env_command() {
        let request = request();
        let target = target(&request);
        let plan = plan_trusted_guest_control_invocation(&request, &target).unwrap();
        assert_eq!(
            plan.command.program,
            PathBuf::from("/opt/homebrew/bin/limactl")
        );
        assert_eq!(
            plain_arguments(&plan.command),
            vec![
                "--tty=false",
                "shell",
                "resident-a",
                SUDO,
                "--non-interactive",
                "--",
                ENV,
                "-i",
                "HOME=/root",
                "PATH=/usr/bin:/bin:/usr/sbin:/sbin",
                "/opt/smolrunner/bin/smolrunner",
                "guest-control",
                "--stdio",
            ]
        );
        assert_eq!(
            plan.command
                .environment
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                "HOME".to_owned(),
                "LANG".to_owned(),
                "LC_ALL".to_owned(),
                "LIMA_HOME".to_owned(),
                "PATH".to_owned(),
            ])
        );
        assert_eq!(plan.summary.timeout(), Duration::from_secs(120));
        assert_eq!(plan.summary.stdout_limit_bytes(), 2 * 1024);
        assert_eq!(plan.summary.stderr_limit_bytes(), 64 * 1024);
    }

    #[test]
    fn canonical_request_is_stdin_only_and_binds_summary_digest() {
        let request = request();
        let plan = plan_trusted_guest_control_invocation(&request, &target(&request)).unwrap();
        assert_eq!(
            plan.stdin(),
            encode_trusted_guest_control_request(&request).unwrap()
        );
        assert_eq!(
            plan.summary.request_digest(),
            &trusted_guest_control_request_digest(&request).unwrap()
        );
        let request_text = std::str::from_utf8(plan.stdin()).unwrap();
        assert!(
            !plan
                .command
                .displayed_argv()
                .iter()
                .any(|value| request_text.contains(value))
        );
        assert!(
            !plan
                .command
                .displayed_argv()
                .join(" ")
                .contains("request-1")
        );
    }

    #[test]
    fn sandbox_and_binary_mismatch_fail_before_command_publication() {
        let request = request();
        let mut wrong_sandbox = target(&request);
        wrong_sandbox.sandbox_generation = ResidentSandboxGeneration::new(12).unwrap();
        assert_eq!(
            plan_trusted_guest_control_invocation(&request, &wrong_sandbox)
                .unwrap_err()
                .kind(),
            TrustedGuestControlInvocationPlanErrorKind::AuthorityMismatch
        );

        let mut wrong_binary = target(&request);
        wrong_binary.guest_binary = TrustedGuestControlBinaryBinding::new(
            8,
            digest('a'),
            TrustedGuestControlArchitecture::LinuxAarch64,
        )
        .unwrap();
        assert_eq!(
            plan_trusted_guest_control_invocation(&request, &wrong_binary)
                .unwrap_err()
                .kind(),
            TrustedGuestControlInvocationPlanErrorKind::BinaryMismatch
        );
    }

    #[test]
    fn private_target_paths_stay_out_of_summary_and_debug() {
        let request = request();
        let target = target(&request);
        let debug = format!("{target:?}");
        assert!(!debug.contains("/opt/homebrew/bin/limactl"));
        assert!(!debug.contains("/private/var/smolrunner/lima"));
        assert!(!debug.contains("/opt/smolrunner/bin/smolrunner"));

        let plan = plan_trusted_guest_control_invocation(&request, &target).unwrap();
        let summary = serde_json::to_string(plan.summary()).unwrap();
        assert!(!summary.contains("/opt/homebrew"));
        assert!(!summary.contains("/private/var"));
        assert!(!summary.contains("/opt/smolrunner"));
        assert!(summary.contains("prepare_trusted_task_view"));
    }

    #[test]
    fn target_constructor_rejects_aliases_root_and_zero_generation() {
        let request = request();
        for path in [
            PathBuf::from("relative/limactl"),
            PathBuf::from("/"),
            PathBuf::from("/opt/../tmp/limactl"),
        ] {
            assert_eq!(
                TrustedGuestControlInvocationTarget::from_verified(
                    path,
                    PathBuf::from("/private/var/smolrunner/lima"),
                    LimaInstanceName::parse("resident-a").unwrap(),
                    request.authority().resident_sandbox_id().clone(),
                    request.authority().resident_sandbox_generation(),
                    5,
                    digest('c'),
                    PathBuf::from("/opt/smolrunner/bin/smolrunner"),
                    request.binary().clone(),
                )
                .unwrap_err()
                .kind(),
                TrustedGuestControlInvocationPlanErrorKind::InvalidTarget
            );
        }

        assert_eq!(
            TrustedGuestControlInvocationTarget::from_verified(
                PathBuf::from("/opt/homebrew/bin/limactl"),
                PathBuf::from("/private/var/smolrunner/lima"),
                LimaInstanceName::parse("resident-a").unwrap(),
                request.authority().resident_sandbox_id().clone(),
                request.authority().resident_sandbox_generation(),
                0,
                digest('c'),
                PathBuf::from("/opt/smolrunner/bin/smolrunner"),
                request.binary().clone(),
            )
            .unwrap_err()
            .kind(),
            TrustedGuestControlInvocationPlanErrorKind::InvalidTarget
        );
    }
}
