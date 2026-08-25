//! Pure sealed invocation planning for one purpose-typed Linux guest-control transaction.
//!
//! The protocol request is canonical claim data, not an authority capability. This module adds one
//! second seal: a verified Mac-side target binding that names the exact resident or formatter
//! lineage, reviewed `limactl` generation, private Lima home, and exact installed guest binary.
//! Product callers cannot construct that target binding in this slice; the owning read-only
//! prerequisite observers mint resident and formatter targets separately from fresh
//! descriptor/executable evidence.
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
use crate::project_catalog::ProjectIdentity;
use crate::project_disk_lease::{
    ProjectDiskGeneration, ProjectDiskId, ResidentSandboxGeneration, ResidentSandboxId,
};
use crate::trusted_guest_control_protocol::{
    TrustedGuestControlBinaryBinding, TrustedGuestControlFormatTransactionGeneration,
    TrustedGuestControlFormatterCarrierGeneration, TrustedGuestControlOperation,
    TrustedGuestControlTargetIdentity, trusted_guest_control_request_digest,
};
use crate::trusted_guest_control_transaction::{
    MAX_TRUSTED_GUEST_CONTROL_TRANSACTION_RECEIPT_BYTES,
    TRUSTED_GUEST_CONTROL_TRANSACTION_SCHEMA_VERSION, TrustedGuestControlTransaction,
    encode_trusted_guest_control_transaction, trusted_guest_control_transaction_digest,
};

pub const TRUSTED_GUEST_CONTROL_INVOCATION_PLAN_SCHEMA_VERSION: u8 = 3;
pub const TRUSTED_GUEST_CONTROL_INVOCATION_TIMEOUT: Duration = Duration::from_secs(120);
pub const MAX_TRUSTED_GUEST_CONTROL_STDOUT_BYTES: usize =
    MAX_TRUSTED_GUEST_CONTROL_TRANSACTION_RECEIPT_BYTES;
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
    target_identity: TrustedGuestControlTargetIdentity,
    limactl_generation: u64,
    limactl_digest: Sha256Digest,
    guest_binary_generation: u64,
    guest_binary_digest: Sha256Digest,
    operation: TrustedGuestControlOperation,
    request_digest: Sha256Digest,
    payload_digest: Sha256Digest,
    transaction_schema_version: u8,
    transaction_digest: Sha256Digest,
    stdin_bytes: usize,
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
    pub const fn target_identity(&self) -> &TrustedGuestControlTargetIdentity {
        &self.target_identity
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
    pub const fn payload_digest(&self) -> &Sha256Digest {
        &self.payload_digest
    }

    #[must_use]
    pub const fn transaction_schema_version(&self) -> u8 {
        self.transaction_schema_version
    }

    #[must_use]
    pub const fn transaction_digest(&self) -> &Sha256Digest {
        &self.transaction_digest
    }

    #[must_use]
    pub const fn stdin_bytes(&self) -> usize {
        self.stdin_bytes
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
/// private paths and executable identities, prove that `instance` is the named resident or
/// formatter generation, and only then call the corresponding crate-private constructor
/// immediately before planning.
pub struct TrustedGuestControlInvocationTarget {
    limactl_program: PathBuf,
    lima_home: PathBuf,
    instance: LimaInstanceName,
    target_identity: TrustedGuestControlTargetIdentity,
    limactl_generation: u64,
    limactl_digest: Sha256Digest,
    guest_binary_path: PathBuf,
    guest_binary: TrustedGuestControlBinaryBinding,
}

impl TrustedGuestControlInvocationTarget {
    #[allow(dead_code, clippy::too_many_arguments)]
    pub(crate) fn from_verified_resident(
        limactl_program: PathBuf,
        lima_home: PathBuf,
        instance: LimaInstanceName,
        project: ProjectIdentity,
        sandbox_id: ResidentSandboxId,
        sandbox_generation: ResidentSandboxGeneration,
        limactl_generation: u64,
        limactl_digest: Sha256Digest,
        guest_binary_path: PathBuf,
        guest_binary: TrustedGuestControlBinaryBinding,
    ) -> Result<Self, TrustedGuestControlInvocationPlanError> {
        Self::from_verified(
            limactl_program,
            lima_home,
            instance,
            TrustedGuestControlTargetIdentity::resident(project, sandbox_id, sandbox_generation),
            limactl_generation,
            limactl_digest,
            guest_binary_path,
            guest_binary,
        )
    }

    #[allow(dead_code, clippy::too_many_arguments)]
    pub(crate) fn from_verified_formatter(
        limactl_program: PathBuf,
        lima_home: PathBuf,
        instance: LimaInstanceName,
        project: ProjectIdentity,
        project_disk_id: ProjectDiskId,
        project_disk_generation: ProjectDiskGeneration,
        format_transaction_generation: TrustedGuestControlFormatTransactionGeneration,
        formatter_carrier_generation: TrustedGuestControlFormatterCarrierGeneration,
        limactl_generation: u64,
        limactl_digest: Sha256Digest,
        guest_binary_path: PathBuf,
        guest_binary: TrustedGuestControlBinaryBinding,
    ) -> Result<Self, TrustedGuestControlInvocationPlanError> {
        Self::from_verified(
            limactl_program,
            lima_home,
            instance,
            TrustedGuestControlTargetIdentity::formatter(
                project,
                project_disk_id,
                project_disk_generation,
                format_transaction_generation,
                formatter_carrier_generation,
            ),
            limactl_generation,
            limactl_digest,
            guest_binary_path,
            guest_binary,
        )
    }

    #[allow(dead_code, clippy::too_many_arguments)]
    fn from_verified(
        limactl_program: PathBuf,
        lima_home: PathBuf,
        instance: LimaInstanceName,
        target_identity: TrustedGuestControlTargetIdentity,
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
            target_identity,
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
            .field("target_identity", &self.target_identity)
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

/// Seal one exact one-shot command plus canonical request-transaction bytes.
///
/// The returned [`CommandSpec`] is still data. This function never executes it. The later Mac
/// adapter must freshly reconfirm the owning durable state, verified target, request digest, and
/// executable identities immediately before using a bounded stdin/stdout/stderr process boundary.
///
/// # Errors
///
/// Returns a bounded error when the verified target names another purpose-typed generation or guest
/// binary, or when the canonical request transaction cannot be encoded.
pub fn plan_trusted_guest_control_invocation(
    transaction: &TrustedGuestControlTransaction,
    target: &TrustedGuestControlInvocationTarget,
) -> Result<TrustedGuestControlInvocationPlan, TrustedGuestControlInvocationPlanError> {
    let request = transaction.request();
    if request.authority().target_identity() != &target.target_identity
        || !request
            .operation()
            .accepts_authority_kind(request.authority().kind())
    {
        return Err(authority_mismatch());
    }
    if request.binary() != &target.guest_binary {
        return Err(binary_mismatch());
    }

    let stdin =
        encode_trusted_guest_control_transaction(transaction).map_err(|_| protocol_error())?;
    let request_digest =
        trusted_guest_control_request_digest(request).map_err(|_| protocol_error())?;
    let transaction_digest =
        trusted_guest_control_transaction_digest(transaction).map_err(|_| protocol_error())?;
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
        .argument("--")
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
            target_identity: target.target_identity.clone(),
            limactl_generation: target.limactl_generation,
            limactl_digest: target.limactl_digest.clone(),
            guest_binary_generation: target.guest_binary.generation(),
            guest_binary_digest: target.guest_binary.digest().clone(),
            operation: request.operation(),
            request_digest,
            payload_digest: request.payload_digest().clone(),
            transaction_schema_version: TRUSTED_GUEST_CONTROL_TRANSACTION_SCHEMA_VERSION,
            transaction_digest,
            stdin_bytes: stdin.len(),
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
        "trusted guest-control request does not match the verified purpose-typed target",
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
        "trusted guest-control request transaction cannot be encoded for invocation",
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
        TrustedGuestControlArchitecture, TrustedGuestControlAuthority,
        TrustedGuestControlCreatedProvenanceClaim, TrustedGuestControlFormatAuthorityClaim,
        TrustedGuestControlFormatterConfigClaim, TrustedGuestControlFormatterConfigGeneration,
        TrustedGuestControlRequest, TrustedGuestControlRequestId,
    };
    use crate::trusted_guest_control_transaction::trusted_guest_control_payload_body_digest;

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
            trusted_guest_control_payload_body_digest(
                TrustedGuestControlOperation::PrepareTrustedTaskView,
                &payload_body(),
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn payload_body() -> Vec<u8> {
        br#"{"schema_version":1}"#.to_vec()
    }

    fn transaction(request: TrustedGuestControlRequest) -> TrustedGuestControlTransaction {
        TrustedGuestControlTransaction::new(request, payload_body()).unwrap()
    }

    fn target(request: &TrustedGuestControlRequest) -> TrustedGuestControlInvocationTarget {
        TrustedGuestControlInvocationTarget::from_verified_resident(
            PathBuf::from("/opt/homebrew/bin/limactl"),
            PathBuf::from("/private/var/smolrunner/lima"),
            LimaInstanceName::parse("resident-a").unwrap(),
            request.authority().project().clone(),
            request.authority().resident_sandbox_id().unwrap().clone(),
            request.authority().resident_sandbox_generation().unwrap(),
            5,
            digest('c'),
            PathBuf::from("/opt/smolrunner/bin/smolrunner"),
            request.binary().clone(),
        )
        .unwrap()
    }

    fn formatter_request() -> TrustedGuestControlRequest {
        let authority = TrustedGuestControlAuthority::formatter_project_disk(
            TrustedGuestControlTargetIdentity::formatter(
                ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
                ProjectDiskId::parse("disk-a").unwrap(),
                ProjectDiskGeneration::new(3).unwrap(),
                TrustedGuestControlFormatTransactionGeneration::new(5).unwrap(),
                TrustedGuestControlFormatterCarrierGeneration::new(7).unwrap(),
            ),
            TrustedGuestControlCreatedProvenanceClaim::new(digest('d')),
            TrustedGuestControlFormatAuthorityClaim::new(digest('e')),
            TrustedGuestControlFormatterConfigGeneration::new(9).unwrap(),
            TrustedGuestControlFormatterConfigClaim::new(digest('f')),
        )
        .unwrap();
        TrustedGuestControlRequest::new(
            TrustedGuestControlRequestId::parse("format-1").unwrap(),
            TrustedGuestControlBinaryBinding::new(
                7,
                digest('a'),
                TrustedGuestControlArchitecture::LinuxAarch64,
            )
            .unwrap(),
            authority,
            TrustedGuestControlOperation::FormatProjectFilesystem,
            trusted_guest_control_payload_body_digest(
                TrustedGuestControlOperation::FormatProjectFilesystem,
                &payload_body(),
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn formatter_target(
        request: &TrustedGuestControlRequest,
    ) -> TrustedGuestControlInvocationTarget {
        let TrustedGuestControlTargetIdentity::Formatter {
            project,
            project_disk_id,
            project_disk_generation,
            format_transaction_generation,
            formatter_carrier_generation,
        } = request.authority().target_identity()
        else {
            panic!("formatter fixture must carry formatter target");
        };
        TrustedGuestControlInvocationTarget::from_verified_formatter(
            PathBuf::from("/opt/homebrew/bin/limactl"),
            PathBuf::from("/private/var/smolrunner/lima"),
            LimaInstanceName::parse("formatter-a").unwrap(),
            project.clone(),
            project_disk_id.clone(),
            *project_disk_generation,
            *format_transaction_generation,
            *formatter_carrier_generation,
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
        let plan = plan_trusted_guest_control_invocation(&transaction(request), &target).unwrap();
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
                "--",
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
        assert_eq!(
            plan.summary.schema_version(),
            TRUSTED_GUEST_CONTROL_INVOCATION_PLAN_SCHEMA_VERSION
        );
        assert_eq!(plan.summary.timeout(), Duration::from_secs(120));
        assert_eq!(plan.summary.stdout_limit_bytes(), 8 * 1024);
        assert_eq!(plan.summary.stderr_limit_bytes(), 64 * 1024);
    }

    #[test]
    fn canonical_transaction_is_stdin_only_and_binds_summary_digests() {
        let request = request();
        let transaction = transaction(request.clone());
        let plan = plan_trusted_guest_control_invocation(&transaction, &target(&request)).unwrap();
        assert_eq!(
            plan.stdin(),
            encode_trusted_guest_control_transaction(&transaction).unwrap()
        );
        assert_eq!(
            plan.summary.request_digest(),
            &trusted_guest_control_request_digest(&request).unwrap()
        );
        assert_eq!(
            plan.summary.transaction_digest(),
            &trusted_guest_control_transaction_digest(&transaction).unwrap()
        );
        assert_eq!(plan.summary.stdin_bytes(), plan.stdin().len());
        assert_eq!(
            plan.summary.transaction_schema_version(),
            TRUSTED_GUEST_CONTROL_TRANSACTION_SCHEMA_VERSION
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
        wrong_sandbox.target_identity = TrustedGuestControlTargetIdentity::resident(
            request.authority().project().clone(),
            request.authority().resident_sandbox_id().unwrap().clone(),
            ResidentSandboxGeneration::new(12).unwrap(),
        );
        assert_eq!(
            plan_trusted_guest_control_invocation(&transaction(request.clone()), &wrong_sandbox)
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
            plan_trusted_guest_control_invocation(&transaction(request), &wrong_binary)
                .unwrap_err()
                .kind(),
            TrustedGuestControlInvocationPlanErrorKind::BinaryMismatch
        );
    }

    #[test]
    fn resident_and_formatter_targets_are_not_interchangeable() {
        let resident = request();
        let formatter = formatter_request();
        assert_eq!(
            plan_trusted_guest_control_invocation(
                &transaction(formatter.clone()),
                &target(&resident),
            )
            .unwrap_err()
            .kind(),
            TrustedGuestControlInvocationPlanErrorKind::AuthorityMismatch
        );
        assert_eq!(
            plan_trusted_guest_control_invocation(
                &transaction(resident.clone()),
                &formatter_target(&formatter),
            )
            .unwrap_err()
            .kind(),
            TrustedGuestControlInvocationPlanErrorKind::AuthorityMismatch
        );
        let plan = plan_trusted_guest_control_invocation(
            &transaction(formatter.clone()),
            &formatter_target(&formatter),
        )
        .unwrap();
        assert_eq!(
            plan.summary().target_identity(),
            formatter.authority().target_identity()
        );
    }

    #[test]
    fn same_sandbox_id_and_generation_in_another_project_is_rejected() {
        let request = request();
        let mut wrong_project = target(&request);
        wrong_project.target_identity = TrustedGuestControlTargetIdentity::resident(
            ProjectIdentity::parse("github.com/teamleaderleo/quarry").unwrap(),
            request.authority().resident_sandbox_id().unwrap().clone(),
            request.authority().resident_sandbox_generation().unwrap(),
        );
        assert_eq!(
            plan_trusted_guest_control_invocation(&transaction(request), &wrong_project)
                .unwrap_err()
                .kind(),
            TrustedGuestControlInvocationPlanErrorKind::AuthorityMismatch
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

        let plan = plan_trusted_guest_control_invocation(&transaction(request), &target).unwrap();
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
                TrustedGuestControlInvocationTarget::from_verified_resident(
                    path,
                    PathBuf::from("/private/var/smolrunner/lima"),
                    LimaInstanceName::parse("resident-a").unwrap(),
                    request.authority().project().clone(),
                    request.authority().resident_sandbox_id().unwrap().clone(),
                    request.authority().resident_sandbox_generation().unwrap(),
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
            TrustedGuestControlInvocationTarget::from_verified_resident(
                PathBuf::from("/opt/homebrew/bin/limactl"),
                PathBuf::from("/private/var/smolrunner/lima"),
                LimaInstanceName::parse("resident-a").unwrap(),
                request.authority().project().clone(),
                request.authority().resident_sandbox_id().unwrap().clone(),
                request.authority().resident_sandbox_generation().unwrap(),
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
