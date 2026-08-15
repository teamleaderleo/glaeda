//! Pure installation/removal plan for the macOS disposable-worker LaunchAgent.
//!
//! This module creates no files and invokes no `launchctl` command. A domain-separated plan
//! identity binds the exact executable, enrollment, LaunchAgent path, property-list bytes, user
//! domain, and ordered compensation contract. Only the bounded path-free report is public output.

use std::fmt;
use std::path::Path;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::journal::RollbackClass;

pub const DISPOSABLE_LAUNCHD_SERVICE_PLAN_SCHEMA_VERSION: u8 = 1;
pub const DISPOSABLE_LAUNCHD_SERVICE_LABEL: &str = "io.smolrunner.disposable-worker";
const MAX_PRIVATE_PATH_BYTES: usize = 1_024;
const MAX_UID: u32 = 2_147_483_647;
const APPLY_LOCK: &str = ".io.smolrunner.disposable-worker.apply.lock";
const STAGED_PLIST_PREFIX: &str = ".io.smolrunner.disposable-worker.plist.next.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableLaunchdServiceDesiredState {
    Installed,
    Removed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableLaunchdServiceActionKind {
    PublishConfiguration,
    BootstrapService,
    BootoutService,
    RemoveConfiguration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DisposableLaunchdServiceActionReport {
    sequence: u8,
    kind: DisposableLaunchdServiceActionKind,
    summary: &'static str,
    rollback: RollbackClass,
}

impl DisposableLaunchdServiceActionReport {
    #[must_use]
    pub const fn sequence(&self) -> u8 {
        self.sequence
    }

    #[must_use]
    pub const fn kind(&self) -> DisposableLaunchdServiceActionKind {
        self.kind
    }

    #[must_use]
    pub const fn summary(&self) -> &'static str {
        self.summary
    }

    #[must_use]
    pub const fn rollback(&self) -> RollbackClass {
        self.rollback
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DisposableLaunchdServicePlanReport {
    schema_version: u8,
    desired_state: DisposableLaunchdServiceDesiredState,
    service_label: &'static str,
    service_scope: &'static str,
    launchd_domain: String,
    plan_identity: Sha256Digest,
    configuration_mode: u32,
    preconditions: Vec<&'static str>,
    actions: Vec<DisposableLaunchdServiceActionReport>,
    requires_operator_approval: bool,
}

impl DisposableLaunchdServicePlanReport {
    #[must_use]
    pub const fn desired_state(&self) -> DisposableLaunchdServiceDesiredState {
        self.desired_state
    }

    #[must_use]
    pub fn launchd_domain(&self) -> &str {
        &self.launchd_domain
    }

    #[must_use]
    pub fn plan_identity(&self) -> &Sha256Digest {
        &self.plan_identity
    }

    #[must_use]
    pub fn actions(&self) -> &[DisposableLaunchdServiceActionReport] {
        &self.actions
    }

    #[must_use]
    pub fn preconditions(&self) -> &[&'static str] {
        &self.preconditions
    }
}

pub struct DisposableLaunchdServicePlan {
    report: DisposableLaunchdServicePlanReport,
}

impl fmt::Debug for DisposableLaunchdServicePlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableLaunchdServicePlan")
            .field("report", &self.report)
            .finish()
    }
}

impl DisposableLaunchdServicePlan {
    #[must_use]
    pub const fn report(&self) -> &DisposableLaunchdServicePlanReport {
        &self.report
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableLaunchdServicePlanErrorKind {
    InvalidConfiguration,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DisposableLaunchdServicePlanError {
    kind: DisposableLaunchdServicePlanErrorKind,
    code: &'static str,
}

impl DisposableLaunchdServicePlanError {
    #[must_use]
    pub const fn kind(self) -> DisposableLaunchdServicePlanErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for DisposableLaunchdServicePlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableLaunchdServicePlanError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .finish()
    }
}

impl fmt::Display for DisposableLaunchdServicePlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("disposable-worker LaunchAgent configuration is invalid")
    }
}

impl std::error::Error for DisposableLaunchdServicePlanError {}

/// Build one non-mutating exact LaunchAgent installation or removal plan.
///
/// The future executor must refuse an existing nonmatching property list, publish atomically with
/// mode `0600`, checkpoint before `launchctl`, and compensate completed actions in reverse order.
/// Removal must boot out the exact label before deleting an exact-byte-matching property list.
///
/// # Errors
///
/// Returns a path-free error unless every private path is an explicit normalized absolute path and
/// the operator UID is a positive non-root user identity.
pub fn plan_disposable_launchd_service(
    desired_state: DisposableLaunchdServiceDesiredState,
    operator_uid: u32,
    operator_home: &Path,
    program: &Path,
    program_digest: &Sha256Digest,
    enrollment: &Path,
    enrollment_digest: &Sha256Digest,
) -> Result<DisposableLaunchdServicePlan, DisposableLaunchdServicePlanError> {
    if operator_uid == 0
        || operator_uid > MAX_UID
        || !valid_private_path(operator_home)
        || !valid_private_path(program)
        || !valid_private_path(enrollment)
    {
        return Err(invalid_configuration());
    }
    let launch_agent = operator_home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{DISPOSABLE_LAUNCHD_SERVICE_LABEL}.plist"));
    if !valid_private_path(&launch_agent) {
        return Err(invalid_configuration());
    }
    let launch_agent_directory = launch_agent.parent().ok_or_else(invalid_configuration)?;
    if [program, enrollment].iter().any(|input| {
        *input == launch_agent
            || *input == launch_agent_directory.join(APPLY_LOCK)
            || (input.parent() == Some(launch_agent_directory)
                && input
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(STAGED_PLIST_PREFIX)))
    }) {
        return Err(invalid_configuration());
    }
    let plist = canonical_plist(program, enrollment, enrollment_digest)?;
    let plan_identity = plan_identity(
        desired_state,
        operator_uid,
        &launch_agent,
        &plist,
        program_digest,
        enrollment_digest,
    )?;
    let actions = match desired_state {
        DisposableLaunchdServiceDesiredState::Installed => vec![
            DisposableLaunchdServiceActionReport {
                sequence: 1,
                kind: DisposableLaunchdServiceActionKind::PublishConfiguration,
                summary: "atomically publish the exact private LaunchAgent property list",
                rollback: RollbackClass::Reversible,
            },
            DisposableLaunchdServiceActionReport {
                sequence: 2,
                kind: DisposableLaunchdServiceActionKind::BootstrapService,
                summary: "bootstrap the exact user LaunchAgent in its GUI domain",
                rollback: RollbackClass::Compensating,
            },
        ],
        DisposableLaunchdServiceDesiredState::Removed => vec![
            DisposableLaunchdServiceActionReport {
                sequence: 1,
                kind: DisposableLaunchdServiceActionKind::BootoutService,
                summary: "boot out the exact user LaunchAgent and confirm it is no longer owned",
                rollback: RollbackClass::Compensating,
            },
            DisposableLaunchdServiceActionReport {
                sequence: 2,
                kind: DisposableLaunchdServiceActionKind::RemoveConfiguration,
                summary: "remove only the exact matching private LaunchAgent property list",
                rollback: RollbackClass::Compensating,
            },
        ],
    };
    Ok(DisposableLaunchdServicePlan {
        report: DisposableLaunchdServicePlanReport {
            schema_version: DISPOSABLE_LAUNCHD_SERVICE_PLAN_SCHEMA_VERSION,
            desired_state,
            service_label: DISPOSABLE_LAUNCHD_SERVICE_LABEL,
            service_scope: "user_launch_agent",
            launchd_domain: format!("gui/{operator_uid}"),
            plan_identity,
            configuration_mode: 0o600,
            preconditions: vec![
                "explicit operator approval names the exact plan identity",
                "the current user and GUI domain match the planned operator identity",
                "the executable is root-owned and immutable and enrollment is exact and private",
                "a foreign or nonmatching LaunchAgent configuration blocks the entire operation",
                "every completed action is durably checkpointed before the next mutation",
            ],
            actions,
            requires_operator_approval: true,
        },
    })
}

fn plan_identity(
    desired_state: DisposableLaunchdServiceDesiredState,
    operator_uid: u32,
    launch_agent: &Path,
    plist: &[u8],
    program_digest: &Sha256Digest,
    enrollment_digest: &Sha256Digest,
) -> Result<Sha256Digest, DisposableLaunchdServicePlanError> {
    let target = launch_agent
        .to_str()
        .ok_or_else(invalid_configuration)?
        .as_bytes();
    let mut hasher = Sha256::new();
    hasher.update(b"smolrunner.disposable-launchd-service-plan.v1\0");
    hasher.update([match desired_state {
        DisposableLaunchdServiceDesiredState::Installed => 1,
        DisposableLaunchdServiceDesiredState::Removed => 2,
    }]);
    hasher.update(operator_uid.to_be_bytes());
    hasher.update((target.len() as u64).to_be_bytes());
    hasher.update(target);
    hasher.update((plist.len() as u64).to_be_bytes());
    hasher.update(plist);
    hasher.update((program_digest.as_str().len() as u64).to_be_bytes());
    hasher.update(program_digest.as_str().as_bytes());
    hasher.update((enrollment_digest.as_str().len() as u64).to_be_bytes());
    hasher.update(enrollment_digest.as_str().as_bytes());
    Sha256Digest::parse(&format!("sha256:{:x}", hasher.finalize()))
        .map_err(|_| invalid_configuration())
}

fn canonical_plist(
    program: &Path,
    enrollment: &Path,
    enrollment_digest: &Sha256Digest,
) -> Result<Vec<u8>, DisposableLaunchdServicePlanError> {
    let program = program.to_str().ok_or_else(invalid_configuration)?;
    let enrollment = enrollment.to_str().ok_or_else(invalid_configuration)?;
    let program = xml_text(program);
    let enrollment = xml_text(enrollment);
    let enrollment_digest = xml_text(enrollment_digest.as_str());
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n\
<dict>\n\
  <key>Label</key>\n\
  <string>{DISPOSABLE_LAUNCHD_SERVICE_LABEL}</string>\n\
  <key>ProgramArguments</key>\n\
  <array>\n\
    <string>{program}</string>\n\
    <string>worker</string>\n\
    <string>serve</string>\n\
    <string>--enrollment</string>\n\
    <string>{enrollment}</string>\n\
    <string>--enrollment-digest</string>\n\
    <string>{enrollment_digest}</string>\n\
  </array>\n\
  <key>RunAtLoad</key>\n\
  <true/>\n\
  <key>KeepAlive</key>\n\
  <true/>\n\
  <key>ProcessType</key>\n\
  <string>Background</string>\n\
  <key>ThrottleInterval</key>\n\
  <integer>10</integer>\n\
  <key>Umask</key>\n\
  <integer>63</integer>\n\
  <key>WorkingDirectory</key>\n\
  <string>/var/empty</string>\n\
  <key>StandardOutPath</key>\n\
  <string>/dev/null</string>\n\
  <key>StandardErrorPath</key>\n\
  <string>/dev/null</string>\n\
</dict>\n\
</plist>\n"
    )
    .into_bytes())
}

fn valid_private_path(path: &Path) -> bool {
    let Some(text) = path.to_str() else {
        return false;
    };
    if text.len() > MAX_PRIVATE_PATH_BYTES
        || text == "/"
        || !text.starts_with('/')
        || text.ends_with('/')
        || text.chars().any(char::is_control)
    {
        return false;
    }
    text[1..]
        .split('/')
        .all(|component| !component.is_empty() && component != "." && component != "..")
}

fn xml_text(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            other => escaped.push(other),
        }
    }
    escaped
}

const fn invalid_configuration() -> DisposableLaunchdServicePlanError {
    DisposableLaunchdServicePlanError {
        kind: DisposableLaunchdServicePlanErrorKind::InvalidConfiguration,
        code: "disposable_launchd_service_configuration_invalid",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn install_plan() -> DisposableLaunchdServicePlan {
        plan_disposable_launchd_service(
            DisposableLaunchdServiceDesiredState::Installed,
            501,
            Path::new("/Users/operator"),
            Path::new("/opt/smolrunner/bin/smolrunner"),
            &digest('a'),
            Path::new("/Users/operator/.config/smolrunner/enrollment.json"),
            &digest('b'),
        )
        .unwrap()
    }

    fn digest(character: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", character.to_string().repeat(64))).unwrap()
    }

    #[test]
    fn install_plan_binds_exact_private_inputs_and_path_free_report() {
        let plan = install_plan();
        let launch_agent =
            Path::new("/Users/operator/Library/LaunchAgents/io.smolrunner.disposable-worker.plist");
        let plist = canonical_plist(
            Path::new("/opt/smolrunner/bin/smolrunner"),
            Path::new("/Users/operator/.config/smolrunner/enrollment.json"),
            &digest('b'),
        )
        .unwrap();
        assert_eq!(
            launch_agent,
            Path::new("/Users/operator/Library/LaunchAgents/io.smolrunner.disposable-worker.plist")
        );
        assert_eq!(
            plan.report().plan_identity(),
            &plan_identity(
                DisposableLaunchdServiceDesiredState::Installed,
                501,
                launch_agent,
                &plist,
                &digest('a'),
                &digest('b'),
            )
            .unwrap()
        );
        let plist = std::str::from_utf8(&plist).unwrap();
        assert!(plist.contains("<string>/opt/smolrunner/bin/smolrunner</string>"));
        assert!(plist.contains("<string>--enrollment</string>"));
        assert!(plist.contains("<string>--enrollment-digest</string>"));
        assert!(plist.contains(&format!("<string>{}</string>", digest('b').as_str())));
        assert!(plist.contains("<key>KeepAlive</key>"));
        assert!(!plist.contains("EnvironmentVariables"));

        let report = serde_json::to_string(plan.report()).unwrap();
        assert!(!report.contains("/Users/"));
        assert!(!report.contains("/opt/"));
        assert!(report.contains("requires_operator_approval"));
        assert_eq!(plan.report().launchd_domain(), "gui/501");
        assert_eq!(plan.report().preconditions().len(), 5);
        assert_eq!(plan.report().actions().len(), 2);
    }

    #[test]
    fn removal_is_ordered_bootout_then_exact_configuration_removal() {
        let plan = plan_disposable_launchd_service(
            DisposableLaunchdServiceDesiredState::Removed,
            501,
            Path::new("/Users/operator"),
            Path::new("/opt/smolrunner/bin/smolrunner"),
            &digest('a'),
            Path::new("/Users/operator/.config/smolrunner/enrollment.json"),
            &digest('b'),
        )
        .unwrap();
        assert_eq!(
            plan.report().desired_state(),
            DisposableLaunchdServiceDesiredState::Removed
        );
        assert_eq!(
            plan.report().actions[0].kind,
            DisposableLaunchdServiceActionKind::BootoutService
        );
        assert_eq!(
            plan.report().actions[1].kind,
            DisposableLaunchdServiceActionKind::RemoveConfiguration
        );
        assert_ne!(
            plan.report().plan_identity(),
            install_plan().report().plan_identity()
        );
    }

    #[test]
    fn aliases_controls_root_and_root_uid_are_refused() {
        for path in [
            "/Users/operator/../other",
            "/Users/operator/./config",
            "/Users/operator//config",
            "/Users/operator/config/",
            "/Users/operator/\nconfig",
            "/",
        ] {
            assert!(
                plan_disposable_launchd_service(
                    DisposableLaunchdServiceDesiredState::Installed,
                    501,
                    Path::new("/Users/operator"),
                    Path::new("/opt/smolrunner/bin/smolrunner"),
                    &digest('a'),
                    Path::new(path),
                    &digest('b'),
                )
                .is_err(),
                "accepted {path:?}"
            );
        }
        assert!(
            plan_disposable_launchd_service(
                DisposableLaunchdServiceDesiredState::Installed,
                0,
                Path::new("/Users/operator"),
                Path::new("/opt/smolrunner/bin/smolrunner"),
                &digest('a'),
                Path::new("/Users/operator/enrollment.json"),
                &digest('b'),
            )
            .is_err()
        );
    }

    #[test]
    fn launch_agent_internal_path_collisions_are_refused_before_execution() {
        for path in [
            "/Users/operator/Library/LaunchAgents/io.smolrunner.disposable-worker.plist",
            "/Users/operator/Library/LaunchAgents/.io.smolrunner.disposable-worker.apply.lock",
            "/Users/operator/Library/LaunchAgents/.io.smolrunner.disposable-worker.plist.next.0123456789abcdef",
        ] {
            for (program, enrollment) in [
                (path, "/Users/operator/.config/smolrunner/enrollment.json"),
                ("/opt/smolrunner/bin/smolrunner", path),
            ] {
                assert!(
                    plan_disposable_launchd_service(
                        DisposableLaunchdServiceDesiredState::Installed,
                        501,
                        Path::new("/Users/operator"),
                        Path::new(program),
                        &digest('a'),
                        Path::new(enrollment),
                        &digest('b'),
                    )
                    .is_err(),
                    "accepted internal path collision {path:?}"
                );
            }
        }
    }

    #[test]
    fn plist_escapes_private_path_metacharacters_without_changing_argv_shape() {
        let plist = canonical_plist(
            Path::new("/opt/smolrunner/bin/smol&runner"),
            Path::new("/Users/operator/config<one>.json"),
            &digest('b'),
        )
        .unwrap();
        let plist = std::str::from_utf8(&plist).unwrap();
        assert!(plist.contains("smol&amp;runner"));
        assert!(plist.contains("config&lt;one&gt;.json"));
    }
}
