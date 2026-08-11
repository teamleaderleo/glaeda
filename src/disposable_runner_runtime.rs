//! Secret-safe fixed guest command for one disposable GitHub Actions runner.
//!
//! This module deliberately stops before process execution. It converts one bridge-issued JIT
//! configuration into a non-cloneable, non-serializable command plan whose secret exists only in
//! a guaranteed-zeroizing standard-input value. A later same-lock service must freshly prove the
//! exact disposable target ready, durably record the returned runner ID, publish a no-replay
//! runner-start checkpoint, and only then consume the command.

#![allow(dead_code)]

use std::fmt;
use std::path::{Component, PathBuf};

use zeroize::Zeroizing;

use crate::disposable_attempt_catalog::DisposableAttemptReservation;
use crate::disposable_attempt_state::DisposableAttemptRevision;
use crate::disposable_worker_reconciler::{
    DisposableAttemptId, DisposableAttemptPhase, DisposableVmIdentity,
};
use crate::execution_admission::EpochMillis;
use crate::github_scale_set_bridge::ScaleSetJitReceipt;
use crate::github_scale_set_protocol::ScaleSetRunnerReference;
use crate::lima_observation::LIMACTL_SAFE_HOME;
use crate::process::CommandSpec;

const MAX_PRIVATE_PATH_BYTES: usize = 1_024;
const RUNNER_WORK_DIRECTORY: &str = "/opt/smolrunner/actions-runner";
const JIT_LAUNCHER: &str = "/opt/smolrunner/bin/smolrunner-jit-launcher";
const SUDO: &str = "/usr/bin/sudo";
const ENV: &str = "/usr/bin/env";
const RUNNER_USER: &str = "smolrunner-runner";
const RUNNER_HOME: &str = "/var/lib/smolrunner-runner";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DisposableRunnerRuntimeErrorKind {
    Configuration,
    State,
    JitConfiguration,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct DisposableRunnerRuntimeError {
    kind: DisposableRunnerRuntimeErrorKind,
    code: &'static str,
}

impl DisposableRunnerRuntimeError {
    pub(crate) const fn kind(self) -> DisposableRunnerRuntimeErrorKind {
        self.kind
    }

    pub(crate) const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for DisposableRunnerRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableRunnerRuntimeError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .finish()
    }
}

impl fmt::Display for DisposableRunnerRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the disposable runner handoff was refused")
    }
}

impl std::error::Error for DisposableRunnerRuntimeError {}

/// Fixed command builder for the official one-job runner inside a disposable Lima VM.
pub(crate) struct DisposableRunnerRuntime {
    limactl_program: PathBuf,
    lima_home: PathBuf,
}

impl DisposableRunnerRuntime {
    pub(crate) fn new(
        limactl_program: impl Into<PathBuf>,
        lima_home: impl Into<PathBuf>,
    ) -> Result<Self, DisposableRunnerRuntimeError> {
        Ok(Self {
            limactl_program: validate_private_path(limactl_program.into())?,
            lima_home: validate_private_path(lima_home.into())?,
        })
    }

    /// Consume one JIT response into a secret-bearing, non-executable plan.
    ///
    /// The attempt must already own an exact cloned VM and be waiting to create its first GitHub
    /// registration. The plan remains unusable by production until the later durable service adds
    /// fresh target-readiness and runner-start checkpoint authority.
    pub(crate) fn plan_launch(
        &self,
        reservation: &DisposableAttemptReservation,
        now: EpochMillis,
        jit: ScaleSetJitReceipt,
    ) -> Result<DisposableRunnerLaunchPlan, DisposableRunnerRuntimeError> {
        let attempt = reservation.attempt();
        if !matches!(
            attempt.phase(),
            DisposableAttemptPhase::Registering | DisposableAttemptPhase::Assigned
        ) || attempt.vm_identity().is_none()
            || attempt.runner_id().is_some()
            || now > attempt.not_after()
        {
            return Err(runtime_error(
                DisposableRunnerRuntimeErrorKind::State,
                "runner_launch_state_invalid",
            ));
        }
        if &jit.runner.name != attempt.runner_name() {
            return Err(runtime_error(
                DisposableRunnerRuntimeErrorKind::State,
                "runner_launch_name_mismatch",
            ));
        }

        let encoded = jit.config.into_zeroizing_string().map_err(|_| {
            runtime_error(
                DisposableRunnerRuntimeErrorKind::JitConfiguration,
                "runner_jit_encoding_invalid",
            )
        })?;
        validate_jit(&encoded)?;

        let command = self
            .base_command()
            .argument("shell")
            .argument("--workdir")
            .argument(RUNNER_WORK_DIRECTORY)
            .argument(attempt.vm_id().as_str())
            .argument(SUDO)
            .argument("--non-interactive")
            .argument("--set-home")
            .argument("--user")
            .argument(RUNNER_USER)
            .argument(ENV)
            .argument("-i")
            .argument(format!("HOME={RUNNER_HOME}"))
            .argument("PATH=/usr/bin:/bin")
            .argument(JIT_LAUNCHER)
            .zeroizing_secret_stdin_line(encoded);

        Ok(DisposableRunnerLaunchPlan {
            attempt_id: attempt.attempt_id().clone(),
            attempt_revision: attempt.revision(),
            vm_identity: attempt
                .vm_identity()
                .expect("validated disposable VM identity is present")
                .clone(),
            runner: jit.runner,
            not_after: attempt.not_after(),
            command,
        })
    }

    fn base_command(&self) -> CommandSpec {
        CommandSpec::new(&self.limactl_program)
            .argument("--tty=false")
            .environment("HOME", LIMACTL_SAFE_HOME)
            .secret_environment(
                "LIMA_HOME",
                self.lima_home
                    .to_str()
                    .expect("validated Lima home remains UTF-8"),
            )
            .environment("LANG", "C")
            .environment("LC_ALL", "C")
    }
}

impl fmt::Debug for DisposableRunnerRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableRunnerRuntime")
            .field("limactl_program", &"<private-program-path>")
            .field("lima_home", &"<private-lima-home>")
            .finish()
    }
}

/// One zeroizing command candidate, bound to the exact pre-registration durable state.
pub(crate) struct DisposableRunnerLaunchPlan {
    attempt_id: DisposableAttemptId,
    attempt_revision: DisposableAttemptRevision,
    vm_identity: DisposableVmIdentity,
    runner: ScaleSetRunnerReference,
    not_after: EpochMillis,
    command: CommandSpec,
}

impl DisposableRunnerLaunchPlan {
    pub(crate) const fn attempt_id(&self) -> &DisposableAttemptId {
        &self.attempt_id
    }

    pub(crate) const fn attempt_revision(&self) -> DisposableAttemptRevision {
        self.attempt_revision
    }

    pub(crate) const fn vm_identity(&self) -> &DisposableVmIdentity {
        &self.vm_identity
    }

    pub(crate) const fn runner(&self) -> &ScaleSetRunnerReference {
        &self.runner
    }

    pub(crate) const fn not_after(&self) -> EpochMillis {
        self.not_after
    }

    pub(crate) const fn command(&self) -> &CommandSpec {
        &self.command
    }
}

impl fmt::Debug for DisposableRunnerLaunchPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableRunnerLaunchPlan")
            .field("attempt_id", &self.attempt_id)
            .field("attempt_revision", &self.attempt_revision)
            .field("vm_identity", &self.vm_identity)
            .field("runner", &self.runner)
            .field("not_after", &self.not_after)
            .field("command", &"<fixed-secret-stdin-command>")
            .finish()
    }
}

fn validate_jit(value: &Zeroizing<String>) -> Result<(), DisposableRunnerRuntimeError> {
    if value.is_empty()
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=' | b'_' | b'-')
        })
    {
        return Err(runtime_error(
            DisposableRunnerRuntimeErrorKind::JitConfiguration,
            "runner_jit_value_invalid",
        ));
    }
    Ok(())
}

fn validate_private_path(path: PathBuf) -> Result<PathBuf, DisposableRunnerRuntimeError> {
    let raw = path.to_str().ok_or_else(invalid_configuration)?;
    if !path.is_absolute()
        || raw == "/"
        || raw.len() > MAX_PRIVATE_PATH_BYTES
        || raw.bytes().any(|byte| byte.is_ascii_control())
        || raw.contains("//")
        || raw.ends_with('/')
        || raw
            .split('/')
            .any(|component| matches!(component, "." | ".."))
        || path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(invalid_configuration());
    }
    Ok(path)
}

const fn invalid_configuration() -> DisposableRunnerRuntimeError {
    runtime_error(
        DisposableRunnerRuntimeErrorKind::Configuration,
        "runner_runtime_configuration_invalid",
    )
}

const fn runtime_error(
    kind: DisposableRunnerRuntimeErrorKind,
    code: &'static str,
) -> DisposableRunnerRuntimeError {
    DisposableRunnerRuntimeError { kind, code }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disposable_attempt_catalog::{
        DisposableAttemptCatalogAction, DisposableAttemptCatalogDocument,
        DisposableAttemptReservation,
    };
    use crate::disposable_attempt_state::DisposableAttemptState;
    use crate::disposable_prepared_template::current_disposable_prepared_template;
    use crate::disposable_worker_reconciler::{
        CapacityClaimId, DisposableVmId, DisposableVmIdentity, DisposableWorkerResources,
    };
    use crate::github_scale_set_bridge::EncodedJitConfig;
    use crate::github_scale_set_protocol::{
        ScaleSetRunnerId, ScaleSetRunnerName, ScaleSetRunnerReference,
    };

    fn registration_reservation() -> DisposableAttemptReservation {
        let attempt_id = DisposableAttemptId::parse("attempt-jit").unwrap();
        let reservation = DisposableAttemptReservation::new(
            DisposableAttemptState::reserved(
                attempt_id.clone(),
                CapacityClaimId::parse("claim-jit").unwrap(),
                DisposableVmId::parse("vm-jit").unwrap(),
                ScaleSetRunnerName::parse("runner-jit").unwrap(),
                EpochMillis::new(10_000).unwrap(),
            ),
            DisposableWorkerResources::new(2_000, 2 << 30, 20 << 30).unwrap(),
            current_disposable_prepared_template()
                .unwrap()
                .identity()
                .unwrap(),
        )
        .unwrap();
        let catalog = DisposableAttemptCatalogDocument::empty()
            .reserve(reservation)
            .unwrap();
        let catalog = catalog
            .replace_attempt(
                &attempt_id,
                DisposableAttemptRevision::new(1).unwrap(),
                DisposableAttemptCatalogAction::AuthorizeClone,
            )
            .unwrap();
        let catalog = catalog
            .checkpoint_clone_started(&attempt_id, DisposableAttemptRevision::new(2).unwrap())
            .unwrap();
        let catalog = catalog
            .bind_vm_identity_after_clone(
                &attempt_id,
                DisposableAttemptRevision::new(3).unwrap(),
                DisposableVmIdentity::parse(&format!("sha256:{}", "11".repeat(32))).unwrap(),
            )
            .unwrap();
        catalog
            .replace_attempt(
                &attempt_id,
                DisposableAttemptRevision::new(4).unwrap(),
                DisposableAttemptCatalogAction::BeginRegistration,
            )
            .unwrap()
            .find_active(&attempt_id)
            .unwrap()
            .clone()
    }

    fn jit(name: &str, value: &str) -> ScaleSetJitReceipt {
        ScaleSetJitReceipt {
            runner: ScaleSetRunnerReference::new(
                ScaleSetRunnerId::new(77).unwrap(),
                ScaleSetRunnerName::parse(name).unwrap(),
            ),
            config: EncodedJitConfig::for_test(value),
        }
    }

    #[test]
    fn plan_binds_exact_registration_and_keeps_jit_out_of_argv_and_debug() {
        let reservation = registration_reservation();
        let runtime = DisposableRunnerRuntime::new(
            "/opt/homebrew/bin/limactl",
            "/Users/operator/.lima-smolrunner",
        )
        .unwrap();
        let secret = "eyJ0b2tlbiI6InNlY3JldCJ9";
        let plan = runtime
            .plan_launch(
                &reservation,
                EpochMillis::new(2_000).unwrap(),
                jit("runner-jit", secret),
            )
            .unwrap();

        let argv = plan.command().displayed_argv();
        assert!(!argv.iter().any(|value| value.contains(secret)));
        assert_eq!(argv[0], "/opt/homebrew/bin/limactl");
        assert!(argv.windows(2).any(|pair| pair == ["--user", RUNNER_USER]));
        assert!(argv.iter().any(|value| value == JIT_LAUNCHER));
        assert!(!argv.iter().any(|value| value == "--preserve-env"));
        assert_eq!(
            plan.command()
                .environment
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            vec![
                "HOME".to_owned(),
                "LANG".to_owned(),
                "LC_ALL".to_owned(),
                "LIMA_HOME".to_owned(),
            ]
        );
        let debug = format!("{plan:?}");
        let json = serde_json::to_string(plan.command()).unwrap();
        assert!(!debug.contains(secret));
        assert!(!json.contains(secret));
        assert!(!debug.contains("/Users/operator"));
        assert!(!json.contains("/Users/operator"));
        assert!(debug.contains("<fixed-secret-stdin-command>"));
        assert!(json.contains("[REDACTED]"));
    }

    #[test]
    fn plan_refuses_wrong_runner_expiry_and_noncanonical_jit() {
        let reservation = registration_reservation();
        let runtime = DisposableRunnerRuntime::new(
            "/opt/homebrew/bin/limactl",
            "/Users/operator/.lima-smolrunner",
        )
        .unwrap();

        assert_eq!(
            runtime
                .plan_launch(
                    &reservation,
                    EpochMillis::new(2_000).unwrap(),
                    jit("runner-other", "YWJj"),
                )
                .unwrap_err()
                .code(),
            "runner_launch_name_mismatch"
        );
        assert_eq!(
            runtime
                .plan_launch(
                    &reservation,
                    EpochMillis::new(10_001).unwrap(),
                    jit("runner-jit", "YWJj"),
                )
                .unwrap_err()
                .code(),
            "runner_launch_state_invalid"
        );
        assert_eq!(
            runtime
                .plan_launch(
                    &reservation,
                    EpochMillis::new(2_000).unwrap(),
                    jit("runner-jit", "not valid"),
                )
                .unwrap_err()
                .code(),
            "runner_jit_value_invalid"
        );
    }

    #[test]
    fn runtime_refuses_path_aliases() {
        for path in [
            "/",
            "/opt/./homebrew/bin/limactl",
            "/opt/homebrew/bin/limactl/",
            "/opt/homebrew/bin/limactl\nchanged",
        ] {
            assert_eq!(
                DisposableRunnerRuntime::new(path, "/Users/operator/.lima-smolrunner")
                    .unwrap_err()
                    .code(),
                "runner_runtime_configuration_invalid"
            );
        }

        let runtime = DisposableRunnerRuntime::new(
            "/Users/operator/private/bin/limactl",
            "/Users/operator/.lima-smolrunner",
        )
        .unwrap();
        let debug = format!("{runtime:?}");
        assert!(!debug.contains("/Users/operator"));
    }
}
