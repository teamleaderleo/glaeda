use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::artifact::Sha256Digest;
use crate::disposable_attempt_catalog::DisposableAttemptReservation;
use crate::disposable_worker_reconciler::{
    DisposableAttemptPhase, DisposableWorkerAction, DisposableWorkerResources,
};
use crate::execution_admission::EpochMillis;
use crate::lima_observation::{LIMACTL_SAFE_HOME, LimaInstanceName};
use crate::process::CommandSpec;

pub const DISPOSABLE_LIMA_WORKER_SCHEMA_VERSION: u8 = 1;

const GIB: u64 = 1 << 30;
const MAX_PRIVATE_PATH_BYTES: usize = 1_024;
const CLONE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const DESTROY_TIMEOUT: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableLimaWorkerCommandKind {
    Clone,
    DiscardIncomplete,
    Destroy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableLimaWorkerErrorKind {
    InvalidConfiguration,
    InvalidAction,
    UnsupportedResources,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DisposableLimaWorkerError {
    kind: DisposableLimaWorkerErrorKind,
    code: &'static str,
    message: &'static str,
}

impl DisposableLimaWorkerError {
    #[must_use]
    pub const fn kind(&self) -> DisposableLimaWorkerErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for DisposableLimaWorkerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for DisposableLimaWorkerError {}

/// Fixed command adapter for disposable Lima/VZ workers.
///
/// Lima owns VM creation, startup, process supervision, and deletion. This adapter contributes no
/// shell, arbitrary guest command, network policy, runner credential, caller-selected environment,
/// or caller-selected deadline. The source template must be prepared and validated by the trusted
/// controller before this boundary; a Lima instance name alone is not ownership or isolation
/// evidence. The clone plan removes inherited mounts and requests startup only as the final clone
/// operation; this adapter never emits Lima's create-or-start `start NAME` command. No plan is
/// executable until a separate boundary supplies sealed current evidence and durable authority.
#[derive(Clone, PartialEq, Eq)]
pub struct DisposableLimaWorkerAdapter {
    limactl_program: PathBuf,
    lima_home: PathBuf,
    source_template: LimaInstanceName,
}

impl fmt::Debug for DisposableLimaWorkerAdapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableLimaWorkerAdapter")
            .field("limactl_program", &"<private-program-path>")
            .field("lima_home", &"<private-lima-home>")
            .field("source_template", &self.source_template)
            .finish()
    }
}

impl DisposableLimaWorkerAdapter {
    /// Construct the fixed adapter for one configured prepared source template.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal unless both private paths are canonical absolute UTF-8 paths.
    pub fn new(
        limactl_program: impl Into<PathBuf>,
        lima_home: impl Into<PathBuf>,
        source_template: LimaInstanceName,
    ) -> Result<Self, DisposableLimaWorkerError> {
        let limactl_program = validate_private_path(limactl_program.into())?;
        let lima_home = validate_private_path(lima_home.into())?;
        Ok(Self {
            limactl_program,
            lima_home,
            source_template,
        })
    }

    /// Construct one closed command plan for an action selected by the durable reconciler.
    ///
    /// This is deliberately non-executing. The returned plan exposes no `CommandSpec`, private
    /// path, or execution method. A later same-lock durable service must reopen current catalog
    /// state, obtain its own time, bind a sealed exact VM observation, and then consume the private
    /// command. A command completion will still require fresh observation before phase advance.
    ///
    /// # Errors
    ///
    /// Returns when the action, durable phase, VM name, or resource granularity is outside the
    /// closed adapter policy.
    pub fn plan(
        &self,
        now: EpochMillis,
        reservation: &DisposableAttemptReservation,
        action: &DisposableWorkerAction,
    ) -> Result<DisposableLimaWorkerCommandPlan, DisposableLimaWorkerError> {
        let attempt = reservation.attempt();
        let instance = LimaInstanceName::parse(attempt.vm_id().as_str()).map_err(|_| {
            error(
                DisposableLimaWorkerErrorKind::InvalidAction,
                "invalid_vm_identity",
                "the durable VM identity is not a supported Lima instance name",
            )
        })?;
        if instance == self.source_template {
            return Err(error(
                DisposableLimaWorkerErrorKind::InvalidAction,
                "source_template_conflict",
                "the disposable VM identity conflicts with the prepared source template",
            ));
        }
        if matches!(action, DisposableWorkerAction::CloneVm) && now > attempt.not_after() {
            return Err(error(
                DisposableLimaWorkerErrorKind::InvalidAction,
                "attempt_expired",
                "the disposable attempt expired before the Lima provisioning command",
            ));
        }

        let (kind, command, timeout) = match action {
            DisposableWorkerAction::CloneVm
                if attempt.phase() == DisposableAttemptPhase::Provisioning =>
            {
                let resources = lima_resources(reservation.resources())?;
                (
                    DisposableLimaWorkerCommandKind::Clone,
                    self.base_command()
                        .argument("clone")
                        .argument(self.source_template.as_str())
                        .argument(instance.as_str())
                        .argument("--cpus")
                        .argument(resources.cpus)
                        .argument("--memory")
                        .argument(resources.memory_gib)
                        .argument("--disk")
                        .argument(resources.disk_gib)
                        .argument("--mount-none")
                        .argument("--start"),
                    CLONE_TIMEOUT,
                )
            }
            DisposableWorkerAction::DiscardIncompleteVm
                if attempt.phase() == DisposableAttemptPhase::Provisioning =>
            {
                (
                    DisposableLimaWorkerCommandKind::DiscardIncomplete,
                    self.base_command()
                        .argument("delete")
                        .argument("--force")
                        .argument(instance.as_str()),
                    DESTROY_TIMEOUT,
                )
            }
            DisposableWorkerAction::DestroyVm
                if attempt.phase() == DisposableAttemptPhase::Destroying =>
            {
                (
                    DisposableLimaWorkerCommandKind::Destroy,
                    self.base_command()
                        .argument("delete")
                        .argument("--force")
                        .argument(instance.as_str()),
                    DESTROY_TIMEOUT,
                )
            }
            DisposableWorkerAction::CloneVm
            | DisposableWorkerAction::DiscardIncompleteVm
            | DisposableWorkerAction::DestroyVm => {
                return Err(error(
                    DisposableLimaWorkerErrorKind::InvalidAction,
                    "invalid_durable_phase",
                    "the durable attempt phase does not authorize the requested Lima command",
                ));
            }
            _ => {
                return Err(error(
                    DisposableLimaWorkerErrorKind::InvalidAction,
                    "unsupported_action",
                    "the reconciler action is not a disposable Lima lifecycle command",
                ));
            }
        };
        let command_identity = command_identity(&command, timeout, &self.lima_home)?;
        Ok(DisposableLimaWorkerCommandPlan {
            schema_version: DISPOSABLE_LIMA_WORKER_SCHEMA_VERSION,
            kind,
            attempt_id: attempt.attempt_id().as_str().to_owned(),
            attempt_revision: attempt.revision().get(),
            vm_id: attempt.vm_id().as_str().to_owned(),
            command_identity,
            timeout_seconds: timeout.as_secs(),
            observation_required: true,
            #[cfg(test)]
            command,
            #[cfg(test)]
            timeout,
        })
    }

    fn base_command(&self) -> CommandSpec {
        CommandSpec::new(&self.limactl_program)
            .argument("--tty=false")
            .environment("HOME", LIMACTL_SAFE_HOME)
            .secret_environment("LIMA_HOME", exact_path(&self.lima_home))
            .environment("LANG", "C")
            .environment("LC_ALL", "C")
    }
}

pub struct DisposableLimaWorkerCommandPlan {
    schema_version: u8,
    kind: DisposableLimaWorkerCommandKind,
    attempt_id: String,
    attempt_revision: u64,
    vm_id: String,
    command_identity: Sha256Digest,
    timeout_seconds: u64,
    observation_required: bool,
    #[cfg(test)]
    command: CommandSpec,
    #[cfg(test)]
    timeout: Duration,
}

impl DisposableLimaWorkerCommandPlan {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn kind(&self) -> DisposableLimaWorkerCommandKind {
        self.kind
    }

    #[must_use]
    pub fn attempt_id(&self) -> &str {
        &self.attempt_id
    }

    #[must_use]
    pub const fn attempt_revision(&self) -> u64 {
        self.attempt_revision
    }

    #[must_use]
    pub fn vm_id(&self) -> &str {
        &self.vm_id
    }

    #[must_use]
    pub const fn command_identity(&self) -> &Sha256Digest {
        &self.command_identity
    }

    #[must_use]
    pub const fn timeout_seconds(&self) -> u64 {
        self.timeout_seconds
    }

    #[must_use]
    pub const fn observation_required(&self) -> bool {
        self.observation_required
    }
}

impl fmt::Debug for DisposableLimaWorkerCommandPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableLimaWorkerCommandPlan")
            .field("schema_version", &self.schema_version)
            .field("kind", &self.kind)
            .field("attempt_id", &self.attempt_id)
            .field("attempt_revision", &self.attempt_revision)
            .field("vm_id", &self.vm_id)
            .field("command_identity", &self.command_identity)
            .field("timeout_seconds", &self.timeout_seconds)
            .field("observation_required", &self.observation_required)
            .finish()
    }
}

#[derive(Serialize)]
struct CommandIdentityDocument<'a> {
    schema_version: u8,
    argv: Vec<String>,
    home: &'static str,
    lima_home: &'a str,
    lang: &'static str,
    lc_all: &'static str,
    timeout_seconds: u64,
}

fn command_identity(
    command: &CommandSpec,
    timeout: Duration,
    lima_home: &Path,
) -> Result<Sha256Digest, DisposableLimaWorkerError> {
    let document = CommandIdentityDocument {
        schema_version: DISPOSABLE_LIMA_WORKER_SCHEMA_VERSION,
        argv: command.displayed_argv(),
        home: LIMACTL_SAFE_HOME,
        lima_home: lima_home
            .to_str()
            .expect("validated private Lima path remains UTF-8"),
        lang: "C",
        lc_all: "C",
        timeout_seconds: timeout.as_secs(),
    };
    let bytes = serde_json::to_vec(&document).map_err(|_| invalid_configuration())?;
    let digest = Sha256::digest(bytes);
    Sha256Digest::parse(&format!("sha256:{digest:x}")).map_err(|_| invalid_configuration())
}

struct LimaResources {
    cpus: String,
    memory_gib: String,
    disk_gib: String,
}

fn lima_resources(
    resources: DisposableWorkerResources,
) -> Result<LimaResources, DisposableLimaWorkerError> {
    if !resources.cpu_millis().is_multiple_of(1_000)
        || !resources.memory_bytes().is_multiple_of(GIB)
        || !resources.disk_bytes().is_multiple_of(GIB)
    {
        return Err(error(
            DisposableLimaWorkerErrorKind::UnsupportedResources,
            "unsupported_resource_granularity",
            "Lima worker resources must use whole CPUs and whole GiB values",
        ));
    }
    Ok(LimaResources {
        cpus: (resources.cpu_millis() / 1_000).to_string(),
        memory_gib: (resources.memory_bytes() / GIB).to_string(),
        disk_gib: (resources.disk_bytes() / GIB).to_string(),
    })
}

fn validate_private_path(path: PathBuf) -> Result<PathBuf, DisposableLimaWorkerError> {
    let Some(text) = path.to_str() else {
        return Err(invalid_configuration());
    };
    if !path.is_absolute()
        || path == Path::new("/")
        || text.len() > MAX_PRIVATE_PATH_BYTES
        || text.bytes().any(|byte| byte.is_ascii_control())
        || text.contains("//")
        || text.ends_with('/')
        || text
            .split('/')
            .any(|component| matches!(component, "." | ".."))
    {
        return Err(invalid_configuration());
    }
    Ok(path)
}

fn exact_path(path: &Path) -> String {
    path.to_str()
        .expect("validated private Lima path remains UTF-8")
        .to_owned()
}

const fn invalid_configuration() -> DisposableLimaWorkerError {
    error(
        DisposableLimaWorkerErrorKind::InvalidConfiguration,
        "invalid_private_path",
        "reviewed Lima paths must be bounded canonical absolute paths",
    )
}

const fn error(
    kind: DisposableLimaWorkerErrorKind,
    code: &'static str,
    message: &'static str,
) -> DisposableLimaWorkerError {
    DisposableLimaWorkerError {
        kind,
        code,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disposable_attempt_catalog::{
        DisposableAttemptCatalog, DisposableAttemptCatalogAction,
        MemoryDisposableAttemptCatalogStore,
    };
    use crate::disposable_attempt_state::DisposableAttemptState;
    use crate::disposable_worker_reconciler::{
        CapacityClaimId, DisposableAttemptId, DisposableVmId,
    };
    use crate::execution_admission::EpochMillis;
    use crate::github_scale_set_protocol::ScaleSetRunnerName;

    fn resources() -> DisposableWorkerResources {
        DisposableWorkerResources::new(4_000, 8 * GIB, 64 * GIB).unwrap()
    }

    fn initial_reservation(resources: DisposableWorkerResources) -> DisposableAttemptReservation {
        DisposableAttemptReservation::new(
            DisposableAttemptState::reserved(
                DisposableAttemptId::parse("attempt-1").unwrap(),
                CapacityClaimId::parse("claim-1").unwrap(),
                DisposableVmId::parse("smol-worker-1").unwrap(),
                ScaleSetRunnerName::parse("runner-1").unwrap(),
                EpochMillis::new(10_000).unwrap(),
            ),
            resources,
        )
        .unwrap()
    }

    fn reservation_in_phase(phase: DisposableAttemptPhase) -> DisposableAttemptReservation {
        let initial = initial_reservation(resources());
        let attempt_id = initial.attempt().attempt_id().clone();
        let mut catalog =
            DisposableAttemptCatalog::new(MemoryDisposableAttemptCatalogStore::default());
        let (empty, _) = catalog.initialize().unwrap();
        let (reserved, _) = catalog.reserve(empty.revision(), initial).unwrap();
        let reserved_attempt = reserved.find_active(&attempt_id).unwrap().attempt();
        let (provisioning, _) = catalog
            .transition(
                reserved.revision(),
                &attempt_id,
                reserved_attempt.revision(),
                DisposableAttemptCatalogAction::BeginProvisioning,
            )
            .unwrap();
        if phase == DisposableAttemptPhase::Provisioning {
            return provisioning.find_active(&attempt_id).unwrap().clone();
        }
        let provisioning_attempt = provisioning.find_active(&attempt_id).unwrap().attempt();
        let (destroying, _) = catalog
            .transition(
                provisioning.revision(),
                &attempt_id,
                provisioning_attempt.revision(),
                DisposableAttemptCatalogAction::BeginCleanup,
            )
            .unwrap();
        assert_eq!(phase, DisposableAttemptPhase::Destroying);
        destroying.find_active(&attempt_id).unwrap().clone()
    }

    fn adapter() -> DisposableLimaWorkerAdapter {
        DisposableLimaWorkerAdapter::new(
            "/opt/homebrew/bin/limactl",
            "/Users/runner/.smolrunner/lima",
            LimaInstanceName::parse("smol-template").unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn clone_plan_is_fixed_private_and_requires_observation() {
        let reservation = reservation_in_phase(DisposableAttemptPhase::Provisioning);
        let plan = adapter()
            .plan(
                EpochMillis::new(1_000).unwrap(),
                &reservation,
                &DisposableWorkerAction::CloneVm,
            )
            .unwrap();

        assert_eq!(plan.kind(), DisposableLimaWorkerCommandKind::Clone);
        assert!(plan.observation_required());
        assert_eq!(plan.attempt_revision(), 2);
        assert_eq!(plan.timeout, CLONE_TIMEOUT);
        assert_eq!(
            plan.command.displayed_argv(),
            [
                "/opt/homebrew/bin/limactl",
                "--tty=false",
                "clone",
                "smol-template",
                "smol-worker-1",
                "--cpus",
                "4",
                "--memory",
                "8",
                "--disk",
                "64",
                "--mount-none",
                "--start",
            ]
        );
        assert_eq!(
            plan.command
                .environment
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["HOME", "LANG", "LC_ALL", "LIMA_HOME"]
        );
        assert!(!format!("{:?}", adapter()).contains("/Users/runner"));
        assert!(!format!("{plan:?}").contains("/opt/homebrew"));
        assert!(!format!("{plan:?}").contains("/Users/runner"));
    }

    #[test]
    fn incomplete_clone_discard_and_cleanup_destroy_are_distinct_fixed_plans() {
        let provisioning = reservation_in_phase(DisposableAttemptPhase::Provisioning);
        let discard = adapter()
            .plan(
                EpochMillis::new(1_000).unwrap(),
                &provisioning,
                &DisposableWorkerAction::DiscardIncompleteVm,
            )
            .unwrap();
        assert_eq!(
            discard.kind(),
            DisposableLimaWorkerCommandKind::DiscardIncomplete
        );
        assert_eq!(
            discard.command.displayed_argv(),
            [
                "/opt/homebrew/bin/limactl",
                "--tty=false",
                "delete",
                "--force",
                "smol-worker-1",
            ]
        );

        let destroying = reservation_in_phase(DisposableAttemptPhase::Destroying);
        let destroy = adapter()
            .plan(
                EpochMillis::new(20_000).unwrap(),
                &destroying,
                &DisposableWorkerAction::DestroyVm,
            )
            .unwrap();
        assert_eq!(
            destroy.command.displayed_argv(),
            [
                "/opt/homebrew/bin/limactl",
                "--tty=false",
                "delete",
                "--force",
                "smol-worker-1",
            ]
        );
    }

    #[test]
    fn invalid_phase_and_fractional_resources_fail_before_execution() {
        let invalid_path = DisposableLimaWorkerAdapter::new(
            "/opt/./homebrew/bin/limactl",
            "/Users/runner/.smolrunner/lima",
            LimaInstanceName::parse("smol-template").unwrap(),
        )
        .unwrap_err();
        assert_eq!(
            invalid_path.kind(),
            DisposableLimaWorkerErrorKind::InvalidConfiguration
        );

        let reserved = initial_reservation(resources());
        let error = adapter()
            .plan(
                EpochMillis::new(1_000).unwrap(),
                &reserved,
                &DisposableWorkerAction::CloneVm,
            )
            .unwrap_err();
        assert_eq!(error.kind(), DisposableLimaWorkerErrorKind::InvalidAction);

        let provisioning = reservation_in_phase(DisposableAttemptPhase::Provisioning);
        let error = adapter()
            .plan(
                EpochMillis::new(10_001).unwrap(),
                &provisioning,
                &DisposableWorkerAction::CloneVm,
            )
            .unwrap_err();
        assert_eq!(error.code(), "attempt_expired");

        let fractional =
            initial_reservation(DisposableWorkerResources::new(3_500, 8 * GIB, 64 * GIB).unwrap());
        let attempt_id = fractional.attempt().attempt_id().clone();
        let mut catalog =
            DisposableAttemptCatalog::new(MemoryDisposableAttemptCatalogStore::default());
        let (empty, _) = catalog.initialize().unwrap();
        let (reserved, _) = catalog.reserve(empty.revision(), fractional).unwrap();
        let attempt = reserved.find_active(&attempt_id).unwrap().attempt();
        let (provisioning, _) = catalog
            .transition(
                reserved.revision(),
                &attempt_id,
                attempt.revision(),
                DisposableAttemptCatalogAction::BeginProvisioning,
            )
            .unwrap();
        let error = adapter()
            .plan(
                EpochMillis::new(1_000).unwrap(),
                provisioning.find_active(&attempt_id).unwrap(),
                &DisposableWorkerAction::CloneVm,
            )
            .unwrap_err();
        assert_eq!(
            error.kind(),
            DisposableLimaWorkerErrorKind::UnsupportedResources
        );
    }
}
