//! Exact, non-mutating production installation plan for the macOS disposable-worker services.
//!
//! This module fixes the small privileged boundary before implementing it: one dedicated
//! non-login account, two root-owned executables, one service-private enrollment, the reviewed PF
//! policy, and two system LaunchDaemons. Planning decodes the real enrollment and emits no command,
//! file, account, Keychain, launchd, or PF mutation.

use std::fmt;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::disposable_network_gate::DISPOSABLE_NETWORK_GATE_RECEIPT_PATH;
use crate::disposable_network_gate_activation::{
    DISPOSABLE_NETWORK_GATE_ACTIVATION_LOCK_PATH, DISPOSABLE_NETWORK_PF_ANCHOR_PATH,
    DISPOSABLE_NETWORK_PF_CONFIGURATION_PATH, plan_disposable_network_gate_activation,
};
use crate::disposable_worker_enrollment::decode_disposable_worker_enrollment;
use crate::journal::RollbackClass;
#[cfg(any(target_os = "macos", test))]
use crate::journal::{
    ActionOutcome, ExecutionJournal, ExecutionLane, JOURNAL_SCHEMA_VERSION, JournalRecord,
    PlannedMutation, Preconditions,
};
#[cfg(any(target_os = "macos", test))]
use crate::journal_document::JournalStateDocument;
use crate::state::InstallationId;
#[cfg(any(target_os = "macos", test))]
use crate::state::JournalId;

pub const DISPOSABLE_MACOS_SERVICE_INSTALLATION_SCHEMA_VERSION: u8 = 1;
pub const DISPOSABLE_MACOS_SERVICE_ACCOUNT: &str = "_smolrunner";
pub const DISPOSABLE_MACOS_SERVICE_GROUP: &str = "_smolrunner";
pub const DISPOSABLE_MACOS_PROGRAM_PATH: &str = "/opt/smolrunner/bin/smolrunner";
pub const DISPOSABLE_MACOS_BRIDGE_PATH: &str = "/opt/smolrunner/bin/scaleset-bridge";
pub const DISPOSABLE_MACOS_STATE_ROOT: &str = "/private/var/lib/smolrunner";
pub const DISPOSABLE_MACOS_ENROLLMENT_PATH: &str =
    "/private/var/lib/smolrunner/disposable-worker-enrollment-v2.json";
pub const DISPOSABLE_MACOS_NETWORK_SERVICE_LABEL: &str = "io.smolrunner.disposable-network-gate";
pub const DISPOSABLE_MACOS_WORKER_SERVICE_LABEL: &str = "io.smolrunner.disposable-worker";
pub const DISPOSABLE_MACOS_NETWORK_PLIST_PATH: &str =
    "/Library/LaunchDaemons/io.smolrunner.disposable-network-gate.plist";
pub const DISPOSABLE_MACOS_WORKER_PLIST_PATH: &str =
    "/Library/LaunchDaemons/io.smolrunner.disposable-worker.plist";
pub const DISPOSABLE_MACOS_INSTALLATION_RECORD_PATH: &str =
    "/private/var/db/smolrunner/disposable-service-installation-v1.json";

const MAX_PRIVATE_PATH_BYTES: usize = 1_024;
const INSTALLATION_DOMAIN: &[u8] = b"smolrunner.disposable-macos-service-installation.v1\0";
const ACCOUNT_IDENTITY_DOMAIN: &[u8] = b"smolrunner.disposable-macos-service-account.v1\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableMacosServiceDesiredState {
    Installed,
    Removed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableMacosServiceActionKind {
    BeginLifecycle,
    EnsureServiceAccount,
    PublishExecutables,
    PublishEnrollment,
    PublishNetworkPolicy,
    PublishLaunchDaemons,
    StartNetworkGate,
    StartWorkerService,
    StopWorkerService,
    RemoveWorkerService,
    StopNetworkGate,
    RemoveNetworkPolicy,
    RemoveEnrollment,
    RemoveExecutables,
    RemoveServiceAccount,
    CompleteLifecycle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DisposableMacosServiceActionReport {
    sequence: u8,
    kind: DisposableMacosServiceActionKind,
    summary: &'static str,
    rollback: RollbackClass,
}

impl DisposableMacosServiceActionReport {
    #[must_use]
    pub const fn sequence(&self) -> u8 {
        self.sequence
    }

    #[must_use]
    pub const fn kind(&self) -> DisposableMacosServiceActionKind {
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
pub struct DisposableMacosServicePlanReport {
    schema_version: u8,
    desired_state: DisposableMacosServiceDesiredState,
    service_scope: &'static str,
    service_account: &'static str,
    service_group: &'static str,
    installation_id: InstallationId,
    service_uid: u32,
    primary_group_id: u32,
    service_user_uuid: String,
    service_group_uuid: String,
    max_workers: u8,
    network_policy_identity: Sha256Digest,
    network_activation_identity: Sha256Digest,
    plan_identity: Sha256Digest,
    preconditions: Vec<&'static str>,
    actions: Vec<DisposableMacosServiceActionReport>,
    requires_root: bool,
    requires_operator_approval: bool,
}

impl DisposableMacosServicePlanReport {
    #[must_use]
    pub const fn desired_state(&self) -> DisposableMacosServiceDesiredState {
        self.desired_state
    }

    #[must_use]
    pub const fn service_uid(&self) -> u32 {
        self.service_uid
    }

    #[must_use]
    pub const fn primary_group_id(&self) -> u32 {
        self.primary_group_id
    }

    #[must_use]
    pub fn service_user_uuid(&self) -> &str {
        &self.service_user_uuid
    }

    #[must_use]
    pub fn service_group_uuid(&self) -> &str {
        &self.service_group_uuid
    }

    #[must_use]
    pub const fn installation_id(&self) -> &InstallationId {
        &self.installation_id
    }

    #[must_use]
    pub fn network_policy_identity(&self) -> &Sha256Digest {
        &self.network_policy_identity
    }

    #[must_use]
    pub fn network_activation_identity(&self) -> &Sha256Digest {
        &self.network_activation_identity
    }

    #[must_use]
    pub fn plan_identity(&self) -> &Sha256Digest {
        &self.plan_identity
    }

    #[must_use]
    pub fn preconditions(&self) -> &[&'static str] {
        &self.preconditions
    }

    #[must_use]
    pub fn actions(&self) -> &[DisposableMacosServiceActionReport] {
        &self.actions
    }
}

/// Exact private inputs retained for a future explicitly approved root apply transaction.
#[allow(
    dead_code,
    reason = "private material is retained for the separately reviewed apply boundary"
)]
pub struct DisposableMacosServicePlan {
    report: DisposableMacosServicePlanReport,
    program_source: PathBuf,
    program_digest: Sha256Digest,
    bridge_source: PathBuf,
    bridge_digest: Sha256Digest,
    enrollment_source: PathBuf,
    enrollment_digest: Sha256Digest,
    enrollment_bytes: Vec<u8>,
    network_anchor: Vec<u8>,
    main_pf_attachment: Vec<u8>,
    network_receipt: Vec<u8>,
    network_plist: Vec<u8>,
    worker_plist: Vec<u8>,
    installation_record: Vec<u8>,
}

/// Private exact material consumed only by the root apply transaction.
#[allow(
    dead_code,
    reason = "consumed by the in-progress production apply boundary"
)]
pub(crate) struct DisposableMacosServiceApplyParts<'a> {
    pub(crate) desired_state: DisposableMacosServiceDesiredState,
    pub(crate) installation_id: &'a InstallationId,
    pub(crate) service_uid: u32,
    pub(crate) primary_group_id: u32,
    pub(crate) service_user_uuid: &'a str,
    pub(crate) service_group_uuid: &'a str,
    pub(crate) program_source: &'a Path,
    pub(crate) program_digest: &'a Sha256Digest,
    pub(crate) bridge_source: &'a Path,
    pub(crate) bridge_digest: &'a Sha256Digest,
    pub(crate) enrollment_source: &'a Path,
    pub(crate) enrollment_digest: &'a Sha256Digest,
    pub(crate) enrollment_bytes: &'a [u8],
    pub(crate) network_anchor: &'a [u8],
    pub(crate) main_pf_attachment: &'a [u8],
    pub(crate) network_receipt: &'a [u8],
    pub(crate) network_plist: &'a [u8],
    pub(crate) worker_plist: &'a [u8],
    pub(crate) installation_record: &'a [u8],
}

impl fmt::Debug for DisposableMacosServicePlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableMacosServicePlan")
            .field("report", &self.report)
            .finish()
    }
}

impl DisposableMacosServicePlan {
    #[must_use]
    pub const fn report(&self) -> &DisposableMacosServicePlanReport {
        &self.report
    }

    #[cfg(any(target_os = "macos", test))]
    #[allow(
        dead_code,
        reason = "consumed by the in-progress production apply boundary"
    )]
    pub(crate) fn apply_parts(&self) -> DisposableMacosServiceApplyParts<'_> {
        DisposableMacosServiceApplyParts {
            desired_state: self.report.desired_state,
            installation_id: &self.report.installation_id,
            service_uid: self.report.service_uid,
            primary_group_id: self.report.primary_group_id,
            service_user_uuid: &self.report.service_user_uuid,
            service_group_uuid: &self.report.service_group_uuid,
            program_source: &self.program_source,
            program_digest: &self.program_digest,
            bridge_source: &self.bridge_source,
            bridge_digest: &self.bridge_digest,
            enrollment_source: &self.enrollment_source,
            enrollment_digest: &self.enrollment_digest,
            enrollment_bytes: &self.enrollment_bytes,
            network_anchor: &self.network_anchor,
            main_pf_attachment: &self.main_pf_attachment,
            network_receipt: &self.network_receipt,
            network_plist: &self.network_plist,
            worker_plist: &self.worker_plist,
            installation_record: &self.installation_record,
        }
    }

    #[cfg(any(target_os = "macos", test))]
    #[allow(
        dead_code,
        reason = "consumed by the in-progress production apply boundary"
    )]
    pub(crate) fn initial_journal_document(&self) -> JournalStateDocument {
        let plan_identity = self.report.plan_identity().as_str();
        let journal_id = JournalId::parse(&format!(
            "macos-service-{}",
            plan_identity
                .strip_prefix("sha256:")
                .expect("plan identity is a canonical SHA-256 digest")
        ))
        .expect("derived lifecycle journal ID is canonical");
        let records = self
            .report
            .actions()
            .iter()
            .enumerate()
            .map(|(index, action)| JournalRecord {
                action: PlannedMutation::new(
                    action_id(action.kind()),
                    ExecutionLane::Root,
                    action.summary(),
                    action.rollback(),
                    Preconditions::new([
                        format!("installation={}", self.report.installation_id().as_str()),
                        format!("plan={plan_identity}"),
                    ]),
                ),
                outcome: if index == 0 {
                    ActionOutcome::Completed
                } else {
                    ActionOutcome::Pending
                },
                message: if index == 0 {
                    Some("exact lifecycle journal published".to_owned())
                } else {
                    None
                },
            })
            .collect();
        JournalStateDocument::new(
            self.report.installation_id().clone(),
            journal_id,
            ExecutionJournal {
                schema_version: JOURNAL_SCHEMA_VERSION,
                records,
                stopped_after: None,
            },
        )
        .expect("fixed production lifecycle journal is valid")
    }

    #[cfg(any(target_os = "macos", test))]
    #[allow(
        dead_code,
        reason = "consumed by the in-progress production apply boundary"
    )]
    pub(crate) fn validate_lifecycle_journal(
        &self,
        document: &JournalStateDocument,
    ) -> Result<(), ()> {
        let initial = self.initial_journal_document();
        if document.installation_id() != initial.installation_id()
            || document.journal_id() != initial.journal_id()
            || document.journal().schema_version != JOURNAL_SCHEMA_VERSION
            || document.journal().stopped_after.is_some()
            || document.journal().records.len() != initial.journal().records.len()
        {
            return Err(());
        }

        let mut saw_executing = false;
        let mut saw_pending = false;
        for (index, (record, expected)) in document
            .journal()
            .records
            .iter()
            .zip(&initial.journal().records)
            .enumerate()
        {
            if record.action != expected.action {
                return Err(());
            }
            match record.outcome {
                ActionOutcome::Completed if !saw_executing && !saw_pending => {
                    let expected_message = if index == 0 {
                        "exact lifecycle journal published"
                    } else {
                        "action completed"
                    };
                    if record.message.as_deref() != Some(expected_message) {
                        return Err(());
                    }
                }
                ActionOutcome::Executing if !saw_executing && !saw_pending && index != 0 => {
                    saw_executing = true;
                    if record.message.is_some() {
                        return Err(());
                    }
                }
                ActionOutcome::Pending if !saw_executing => {
                    saw_pending = true;
                    if record.message.is_some() {
                        return Err(());
                    }
                }
                ActionOutcome::Pending if saw_executing => {
                    if record.message.is_some() {
                        return Err(());
                    }
                }
                _ => return Err(()),
            }
        }
        Ok(())
    }

    #[cfg(any(target_os = "macos", test))]
    #[allow(
        dead_code,
        reason = "consumed by the in-progress production apply boundary"
    )]
    pub(crate) fn begin_next_lifecycle_action(
        &self,
        current: &JournalStateDocument,
    ) -> Result<JournalStateDocument, ()> {
        self.validate_lifecycle_journal(current)?;
        if current
            .journal()
            .records
            .iter()
            .any(|record| record.outcome == ActionOutcome::Executing)
        {
            return Err(());
        }
        let mut journal = current.journal().clone();
        let next = journal
            .records
            .iter_mut()
            .find(|record| record.outcome == ActionOutcome::Pending)
            .ok_or(())?;
        next.outcome = ActionOutcome::Executing;
        JournalStateDocument::new(
            current.installation_id().clone(),
            current.journal_id().clone(),
            journal,
        )
        .map_err(|_| ())
    }

    #[cfg(any(target_os = "macos", test))]
    #[allow(
        dead_code,
        reason = "consumed by the in-progress production apply boundary"
    )]
    pub(crate) fn complete_executing_lifecycle_action(
        &self,
        current: &JournalStateDocument,
    ) -> Result<JournalStateDocument, ()> {
        self.validate_lifecycle_journal(current)?;
        let mut journal = current.journal().clone();
        let mut executing = journal
            .records
            .iter_mut()
            .filter(|record| record.outcome == ActionOutcome::Executing);
        let record = executing.next().ok_or(())?;
        if executing.next().is_some() {
            return Err(());
        }
        record.outcome = ActionOutcome::Completed;
        record.message = Some("action completed".to_owned());
        JournalStateDocument::new(
            current.installation_id().clone(),
            current.journal_id().clone(),
            journal,
        )
        .map_err(|_| ())
    }
}

#[allow(
    dead_code,
    reason = "consumed by the in-progress production apply boundary"
)]
const fn action_id(kind: DisposableMacosServiceActionKind) -> &'static str {
    match kind {
        DisposableMacosServiceActionKind::BeginLifecycle => "begin-lifecycle",
        DisposableMacosServiceActionKind::EnsureServiceAccount => "ensure-service-account",
        DisposableMacosServiceActionKind::PublishExecutables => "publish-executables",
        DisposableMacosServiceActionKind::PublishEnrollment => "publish-enrollment",
        DisposableMacosServiceActionKind::PublishNetworkPolicy => "publish-network-policy",
        DisposableMacosServiceActionKind::PublishLaunchDaemons => "publish-launch-daemons",
        DisposableMacosServiceActionKind::StartNetworkGate => "start-network-gate",
        DisposableMacosServiceActionKind::StartWorkerService => "start-worker-service",
        DisposableMacosServiceActionKind::StopWorkerService => "stop-worker-service",
        DisposableMacosServiceActionKind::RemoveWorkerService => "remove-worker-service",
        DisposableMacosServiceActionKind::StopNetworkGate => "stop-network-gate",
        DisposableMacosServiceActionKind::RemoveNetworkPolicy => "remove-network-policy",
        DisposableMacosServiceActionKind::RemoveEnrollment => "remove-enrollment",
        DisposableMacosServiceActionKind::RemoveExecutables => "remove-executables",
        DisposableMacosServiceActionKind::RemoveServiceAccount => "remove-service-account",
        DisposableMacosServiceActionKind::CompleteLifecycle => "complete-lifecycle",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableMacosServicePlanErrorKind {
    InvalidConfiguration,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DisposableMacosServicePlanError {
    kind: DisposableMacosServicePlanErrorKind,
    code: &'static str,
}

impl DisposableMacosServicePlanError {
    #[must_use]
    pub const fn kind(self) -> DisposableMacosServicePlanErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for DisposableMacosServicePlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableMacosServicePlanError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .finish()
    }
}

impl fmt::Display for DisposableMacosServicePlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the production macOS disposable-worker service plan is invalid")
    }
}

impl std::error::Error for DisposableMacosServicePlanError {}

/// Build the complete non-mutating production service installation or removal plan.
///
/// The plan consumes the canonical enrollment bytes so the service UID, PF policy, prepared
/// template, bridge digest, state root, Scale Set, and Keychain references are validated by their
/// existing owners. It deliberately performs no host observation or mutation.
///
/// # Errors
///
/// Returns one bounded path-free error unless every source path is canonical and distinct, the
/// enrollment is current and canonical, its state root is the fixed production root, and its
/// bridge digest matches the separately approved bridge input.
#[allow(clippy::too_many_arguments)]
pub fn plan_disposable_macos_service(
    desired_state: DisposableMacosServiceDesiredState,
    installation_id: &InstallationId,
    program_source: &Path,
    program_digest: &Sha256Digest,
    bridge_source: &Path,
    bridge_digest: &Sha256Digest,
    enrollment_source: &Path,
    enrollment_bytes: &[u8],
) -> Result<DisposableMacosServicePlan, DisposableMacosServicePlanError> {
    if !valid_source_path(program_source)
        || !valid_source_path(bridge_source)
        || !valid_source_path(enrollment_source)
        || program_source == bridge_source
        || program_source == enrollment_source
        || bridge_source == enrollment_source
        || source_collides_with_managed_path(program_source)
        || source_collides_with_managed_path(bridge_source)
        || source_collides_with_managed_path(enrollment_source)
    {
        return Err(invalid_configuration());
    }

    let enrollment = decode_disposable_worker_enrollment(enrollment_bytes)
        .map_err(|_| invalid_configuration())?;
    let parts = enrollment.into_parts();
    if parts.state_root != Path::new(DISPOSABLE_MACOS_STATE_ROOT)
        || parts.bridge_config.program_digest() != bridge_digest
    {
        return Err(invalid_configuration());
    }
    let service_uid = parts.network_policy.report().service_uid();
    let primary_group_id = service_uid;
    let service_user_uuid = account_uuid(installation_id, service_uid, b"user");
    let service_group_uuid = account_uuid(installation_id, primary_group_id, b"group");
    let network_policy_identity = parts.network_policy.report().policy_identity().clone();
    let activation = plan_disposable_network_gate_activation(&parts.network_policy)
        .map_err(|_| invalid_configuration())?;
    let network_activation_identity = activation.report().activation_identity().clone();
    let enrollment_digest = digest_bytes(enrollment_bytes);
    let network_plist = render_network_plist(service_uid, &network_policy_identity);
    let worker_plist = render_worker_plist(&enrollment_digest);
    let network_anchor = parts.network_policy.anchor_bytes().to_vec();
    let main_pf_attachment = activation.main_attachment().to_vec();
    let network_receipt = activation.receipt().to_vec();
    let installation_record = render_installation_record(
        installation_id,
        service_uid,
        primary_group_id,
        &service_user_uuid,
        &service_group_uuid,
        program_digest,
        bridge_digest,
        &enrollment_digest,
        &network_policy_identity,
        &network_activation_identity,
        &network_plist,
        &worker_plist,
    );
    let actions = installation_actions(desired_state);
    let preconditions = installation_preconditions(desired_state);
    let plan_identity = plan_identity(
        desired_state,
        installation_id,
        service_uid,
        primary_group_id,
        &service_user_uuid,
        &service_group_uuid,
        program_source,
        program_digest,
        bridge_source,
        bridge_digest,
        enrollment_source,
        &enrollment_digest,
        &network_policy_identity,
        &network_anchor,
        &main_pf_attachment,
        &network_receipt,
        &network_plist,
        &worker_plist,
        &installation_record,
    );

    Ok(DisposableMacosServicePlan {
        report: DisposableMacosServicePlanReport {
            schema_version: DISPOSABLE_MACOS_SERVICE_INSTALLATION_SCHEMA_VERSION,
            desired_state,
            service_scope: "system_launch_daemons",
            service_account: DISPOSABLE_MACOS_SERVICE_ACCOUNT,
            service_group: DISPOSABLE_MACOS_SERVICE_GROUP,
            installation_id: installation_id.clone(),
            service_uid,
            primary_group_id,
            service_user_uuid,
            service_group_uuid,
            max_workers: 1,
            network_policy_identity,
            network_activation_identity,
            plan_identity,
            preconditions,
            actions,
            requires_root: true,
            requires_operator_approval: true,
        },
        program_source: program_source.to_path_buf(),
        program_digest: program_digest.clone(),
        bridge_source: bridge_source.to_path_buf(),
        bridge_digest: bridge_digest.clone(),
        enrollment_source: enrollment_source.to_path_buf(),
        enrollment_digest,
        enrollment_bytes: enrollment_bytes.to_vec(),
        network_anchor,
        main_pf_attachment,
        network_receipt,
        network_plist,
        worker_plist,
        installation_record,
    })
}

fn installation_preconditions(
    desired_state: DisposableMacosServiceDesiredState,
) -> Vec<&'static str> {
    match desired_state {
        DisposableMacosServiceDesiredState::Installed => vec![
            "explicit operator approval names the exact complete plan identity",
            "a versioned root-owned lifecycle journal checkpoints every mutation boundary",
            "root observes or creates one exact non-login account and dedicated primary group",
            "the GitHub App Keychain item is service-readable without entering this plan",
            "foreign accounts files PF configuration and launchd jobs block the operation",
            "worker startup follows exact live network-gate receipt publication",
        ],
        DisposableMacosServiceDesiredState::Removed => vec![
            "explicit operator approval names the exact complete plan identity",
            "the exact installation identity and versioned lifecycle journal authorize removal",
            "root observes the exact managed account files PF configuration and launchd jobs",
            "active work is held drained and durably settled before destructive cleanup",
            "unknown foreign or ambiguous state blocks removal and remains preserved",
            "global PF remains enabled after SmolRunner policy removal",
        ],
    }
}

fn installation_actions(
    desired_state: DisposableMacosServiceDesiredState,
) -> Vec<DisposableMacosServiceActionReport> {
    let action = |sequence, kind, summary, rollback| DisposableMacosServiceActionReport {
        sequence,
        kind,
        summary,
        rollback,
    };
    match desired_state {
        DisposableMacosServiceDesiredState::Installed => vec![
            action(
                1,
                DisposableMacosServiceActionKind::BeginLifecycle,
                "durably checkpoint the exact approved installation before mutation",
                RollbackClass::Compensating,
            ),
            action(
                2,
                DisposableMacosServiceActionKind::EnsureServiceAccount,
                "ensure the exact non-login account primary group and no supplementary groups",
                RollbackClass::Compensating,
            ),
            action(
                3,
                DisposableMacosServiceActionKind::PublishExecutables,
                "publish the exact root-owned controller and Scale Set bridge",
                RollbackClass::Reversible,
            ),
            action(
                4,
                DisposableMacosServiceActionKind::PublishEnrollment,
                "publish the exact service-private credential-free enrollment",
                RollbackClass::Reversible,
            ),
            action(
                5,
                DisposableMacosServiceActionKind::PublishNetworkPolicy,
                "publish the exact root-owned PF anchor and main-ruleset attachment",
                RollbackClass::Compensating,
            ),
            action(
                6,
                DisposableMacosServiceActionKind::PublishLaunchDaemons,
                "publish the exact root and unprivileged system LaunchDaemons",
                RollbackClass::Reversible,
            ),
            action(
                7,
                DisposableMacosServiceActionKind::StartNetworkGate,
                "bootstrap the root gate and require its exact live admission receipt",
                RollbackClass::Compensating,
            ),
            action(
                8,
                DisposableMacosServiceActionKind::StartWorkerService,
                "bootstrap the unprivileged worker only after network admission",
                RollbackClass::Compensating,
            ),
            action(
                9,
                DisposableMacosServiceActionKind::CompleteLifecycle,
                "publish the exact ownership record and settle the installation journal",
                RollbackClass::Compensating,
            ),
        ],
        DisposableMacosServiceDesiredState::Removed => vec![
            action(
                1,
                DisposableMacosServiceActionKind::BeginLifecycle,
                "durably checkpoint exact drain-first removal before mutation",
                RollbackClass::Compensating,
            ),
            action(
                2,
                DisposableMacosServiceActionKind::StopWorkerService,
                "hold drain and boot out the exact unprivileged worker service",
                RollbackClass::Compensating,
            ),
            action(
                3,
                DisposableMacosServiceActionKind::RemoveWorkerService,
                "remove only the exact worker LaunchDaemon configuration",
                RollbackClass::Compensating,
            ),
            action(
                4,
                DisposableMacosServiceActionKind::StopNetworkGate,
                "boot out the exact one-shot root network-gate service",
                RollbackClass::Compensating,
            ),
            action(
                5,
                DisposableMacosServiceActionKind::RemoveNetworkPolicy,
                "remove only SmolRunner PF rules and receipt without disabling global PF",
                RollbackClass::Compensating,
            ),
            action(
                6,
                DisposableMacosServiceActionKind::RemoveEnrollment,
                "remove exact enrollment and settled owned state after drain",
                RollbackClass::Compensating,
            ),
            action(
                7,
                DisposableMacosServiceActionKind::RemoveExecutables,
                "remove only exact unreferenced installed executables",
                RollbackClass::Compensating,
            ),
            action(
                8,
                DisposableMacosServiceActionKind::RemoveServiceAccount,
                "remove only the exact empty dedicated account and primary group",
                RollbackClass::Compensating,
            ),
            action(
                9,
                DisposableMacosServiceActionKind::CompleteLifecycle,
                "retire the exact ownership record only after complete owned absence",
                RollbackClass::Compensating,
            ),
        ],
    }
}

#[allow(clippy::too_many_arguments)]
fn plan_identity(
    desired_state: DisposableMacosServiceDesiredState,
    installation_id: &InstallationId,
    service_uid: u32,
    primary_group_id: u32,
    service_user_uuid: &str,
    service_group_uuid: &str,
    program_source: &Path,
    program_digest: &Sha256Digest,
    bridge_source: &Path,
    bridge_digest: &Sha256Digest,
    enrollment_source: &Path,
    enrollment_digest: &Sha256Digest,
    network_policy_identity: &Sha256Digest,
    network_anchor: &[u8],
    main_pf_attachment: &[u8],
    network_receipt: &[u8],
    network_plist: &[u8],
    worker_plist: &[u8],
    installation_record: &[u8],
) -> Sha256Digest {
    let mut hasher = Sha256::new();
    hasher.update(INSTALLATION_DOMAIN);
    hash_field(
        &mut hasher,
        &[match desired_state {
            DisposableMacosServiceDesiredState::Installed => 1,
            DisposableMacosServiceDesiredState::Removed => 2,
        }],
    );
    hash_field(&mut hasher, installation_id.as_str().as_bytes());
    hash_field(&mut hasher, &service_uid.to_be_bytes());
    hash_field(&mut hasher, &primary_group_id.to_be_bytes());
    hash_field(&mut hasher, service_user_uuid.as_bytes());
    hash_field(&mut hasher, service_group_uuid.as_bytes());
    hash_field(&mut hasher, program_source.as_os_str().as_bytes());
    hash_field(&mut hasher, program_digest.as_str().as_bytes());
    hash_field(&mut hasher, bridge_source.as_os_str().as_bytes());
    hash_field(&mut hasher, bridge_digest.as_str().as_bytes());
    hash_field(&mut hasher, enrollment_source.as_os_str().as_bytes());
    hash_field(&mut hasher, enrollment_digest.as_str().as_bytes());
    hash_field(&mut hasher, network_policy_identity.as_str().as_bytes());
    for fixed in [
        DISPOSABLE_MACOS_SERVICE_ACCOUNT.as_bytes(),
        DISPOSABLE_MACOS_SERVICE_GROUP.as_bytes(),
        DISPOSABLE_MACOS_PROGRAM_PATH.as_bytes(),
        DISPOSABLE_MACOS_BRIDGE_PATH.as_bytes(),
        DISPOSABLE_MACOS_ENROLLMENT_PATH.as_bytes(),
        DISPOSABLE_MACOS_NETWORK_PLIST_PATH.as_bytes(),
        DISPOSABLE_MACOS_WORKER_PLIST_PATH.as_bytes(),
        DISPOSABLE_MACOS_INSTALLATION_RECORD_PATH.as_bytes(),
        network_anchor,
        main_pf_attachment,
        network_receipt,
        network_plist,
        worker_plist,
        installation_record,
    ] {
        hash_field(&mut hasher, fixed);
    }
    Sha256Digest::parse(&format!("sha256:{:x}", hasher.finalize()))
        .expect("SHA-256 output is canonical")
}

fn hash_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(value);
}

fn digest_bytes(bytes: &[u8]) -> Sha256Digest {
    Sha256Digest::parse(&format!("sha256:{:x}", Sha256::digest(bytes)))
        .expect("SHA-256 output is canonical")
}

fn account_uuid(installation_id: &InstallationId, numeric_id: u32, role: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(ACCOUNT_IDENTITY_DOMAIN);
    hash_field(&mut hasher, installation_id.as_str().as_bytes());
    hash_field(&mut hasher, &numeric_id.to_be_bytes());
    hash_field(&mut hasher, role);
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15],
    )
}

#[allow(clippy::too_many_arguments)]
fn render_installation_record(
    installation_id: &InstallationId,
    service_uid: u32,
    primary_group_id: u32,
    service_user_uuid: &str,
    service_group_uuid: &str,
    program_digest: &Sha256Digest,
    bridge_digest: &Sha256Digest,
    enrollment_digest: &Sha256Digest,
    network_policy_identity: &Sha256Digest,
    network_activation_identity: &Sha256Digest,
    network_plist: &[u8],
    worker_plist: &[u8],
) -> Vec<u8> {
    let network_plist_digest = digest_bytes(network_plist);
    let worker_plist_digest = digest_bytes(worker_plist);
    format!(
        concat!(
            "{{\n",
            "  \"schema_version\": 1,\n",
            "  \"installation_id\": \"{}\",\n",
            "  \"service_account\": \"{}\",\n",
            "  \"service_group\": \"{}\",\n",
            "  \"service_uid\": {},\n",
            "  \"primary_group_id\": {},\n",
            "  \"service_user_uuid\": \"{}\",\n",
            "  \"service_group_uuid\": \"{}\",\n",
            "  \"program_digest\": \"{}\",\n",
            "  \"bridge_digest\": \"{}\",\n",
            "  \"enrollment_digest\": \"{}\",\n",
            "  \"network_policy_identity\": \"{}\",\n",
            "  \"network_activation_identity\": \"{}\",\n",
            "  \"network_launchd_digest\": \"{}\",\n",
            "  \"worker_launchd_digest\": \"{}\"\n",
            "}}\n"
        ),
        installation_id.as_str(),
        DISPOSABLE_MACOS_SERVICE_ACCOUNT,
        DISPOSABLE_MACOS_SERVICE_GROUP,
        service_uid,
        primary_group_id,
        service_user_uuid,
        service_group_uuid,
        program_digest.as_str(),
        bridge_digest.as_str(),
        enrollment_digest.as_str(),
        network_policy_identity.as_str(),
        network_activation_identity.as_str(),
        network_plist_digest.as_str(),
        worker_plist_digest.as_str(),
    )
    .into_bytes()
}

fn valid_source_path(path: &Path) -> bool {
    let Some(text) = path.to_str() else {
        return false;
    };
    text.len() <= MAX_PRIVATE_PATH_BYTES
        && text.starts_with('/')
        && text != "/"
        && !text.ends_with('/')
        && !text.chars().any(char::is_control)
        && text[1..]
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

fn source_collides_with_managed_path(path: &Path) -> bool {
    let exact_collision = [
        DISPOSABLE_MACOS_PROGRAM_PATH,
        DISPOSABLE_MACOS_BRIDGE_PATH,
        DISPOSABLE_MACOS_ENROLLMENT_PATH,
        DISPOSABLE_MACOS_NETWORK_PLIST_PATH,
        DISPOSABLE_MACOS_WORKER_PLIST_PATH,
        DISPOSABLE_MACOS_INSTALLATION_RECORD_PATH,
        DISPOSABLE_NETWORK_PF_ANCHOR_PATH,
        DISPOSABLE_NETWORK_PF_CONFIGURATION_PATH,
        DISPOSABLE_NETWORK_GATE_RECEIPT_PATH,
        DISPOSABLE_NETWORK_GATE_ACTIVATION_LOCK_PATH,
        "/etc/pf.conf",
        "/etc/pf.anchors/io.smolrunner.disposable-worker",
    ]
    .into_iter()
    .any(|managed| path == Path::new(managed));
    exact_collision
        || [
            "/opt/smolrunner/bin",
            "/private/var/lib/smolrunner",
            "/var/lib/smolrunner",
            "/private/var/run/smolrunner",
            "/var/run/smolrunner",
            "/Library/LaunchDaemons",
            "/private/var/db/smolrunner",
            "/var/db/smolrunner",
            "/private/etc/pf.anchors",
            "/etc/pf.anchors",
        ]
        .into_iter()
        .any(|managed| path.starts_with(managed))
}

fn render_network_plist(service_uid: u32, policy_identity: &Sha256Digest) -> Vec<u8> {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n\
<dict>\n\
  <key>Label</key>\n\
  <string>{DISPOSABLE_MACOS_NETWORK_SERVICE_LABEL}</string>\n\
  <key>ProgramArguments</key>\n\
  <array>\n\
    <string>{DISPOSABLE_MACOS_PROGRAM_PATH}</string>\n\
    <string>worker</string>\n\
    <string>network-activate</string>\n\
    <string>--service-uid</string>\n\
    <string>{service_uid}</string>\n\
    <string>--policy-identity</string>\n\
    <string>{}</string>\n\
  </array>\n\
  <key>UserName</key>\n\
  <string>root</string>\n\
  <key>GroupName</key>\n\
  <string>wheel</string>\n\
  <key>RunAtLoad</key>\n\
  <true/>\n\
  <key>KeepAlive</key>\n\
  <dict>\n\
    <key>SuccessfulExit</key>\n\
    <false/>\n\
  </dict>\n\
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
</plist>\n",
        policy_identity.as_str()
    )
    .into_bytes()
}

fn render_worker_plist(enrollment_digest: &Sha256Digest) -> Vec<u8> {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n\
<dict>\n\
  <key>Label</key>\n\
  <string>{DISPOSABLE_MACOS_WORKER_SERVICE_LABEL}</string>\n\
  <key>ProgramArguments</key>\n\
  <array>\n\
    <string>{DISPOSABLE_MACOS_PROGRAM_PATH}</string>\n\
    <string>worker</string>\n\
    <string>serve</string>\n\
    <string>--enrollment</string>\n\
    <string>{DISPOSABLE_MACOS_ENROLLMENT_PATH}</string>\n\
    <string>--enrollment-digest</string>\n\
    <string>{}</string>\n\
  </array>\n\
  <key>UserName</key>\n\
  <string>{DISPOSABLE_MACOS_SERVICE_ACCOUNT}</string>\n\
  <key>GroupName</key>\n\
  <string>{DISPOSABLE_MACOS_SERVICE_GROUP}</string>\n\
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
</plist>\n",
        enrollment_digest.as_str()
    )
    .into_bytes()
}

const fn invalid_configuration() -> DisposableMacosServicePlanError {
    DisposableMacosServicePlanError {
        kind: DisposableMacosServicePlanErrorKind::InvalidConfiguration,
        code: "disposable_macos_service_plan_invalid",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST_A: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const DIGEST_B: &str =
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const NETWORK_DIGEST: &str =
        "sha256:65ceec8974086e378f216acc555724cb40b08ccc047391dedd0b6f17df72587e";

    fn installation_id() -> InstallationId {
        InstallationId::parse("smolrunner-install-0001").unwrap()
    }

    fn enrollment(bridge_digest: &str) -> Vec<u8> {
        format!(
            concat!(
                "{{\n",
                "  \"schema_version\": 2,\n",
                "  \"state_root\": \"/private/var/lib/smolrunner\",\n",
                "  \"network\": {{\n",
                "    \"backend\": \"macos_pf_dedicated_uid\",\n",
                "    \"service_uid\": 502,\n",
                "    \"policy_identity\": \"{NETWORK_DIGEST}\"\n",
                "  }},\n",
                "  \"lima\": {{\n",
                "    \"program\": \"/opt/homebrew/bin/limactl\",\n",
                "    \"home\": \"/private/var/lib/smolrunner/lima\",\n",
                "    \"source_instance\": \"smolrunner-prepared-template\"\n",
                "  }},\n",
                "  \"bridge\": {{\n",
                "    \"program_digest\": \"{bridge_digest}\"\n",
                "  }},\n",
                "  \"github\": {{\n",
                "    \"config_url\": \"https://github.com/acme\",\n",
                "    \"client_id\": \"Iv1.0123456789abcdef\",\n",
                "    \"installation_id\": 42,\n",
                "    \"keychain_service\": \"smolrunner.github-app\",\n",
                "    \"keychain_account\": \"acme-ci\"\n",
                "  }},\n",
                "  \"scale_set\": {{\n",
                "    \"id\": 17,\n",
                "    \"name\": \"smolrunner-disposable\",\n",
                "    \"runner_group_id\": 3,\n",
                "    \"owner\": \"acme\",\n",
                "    \"repository\": \"widgets\",\n",
                "    \"labels\": [\n",
                "      \"self-hosted\",\n",
                "      \"smolrunner\"\n",
                "    ]\n",
                "  }},\n",
                "  \"resources\": {{\n",
                "    \"cpu_millis\": 2000,\n",
                "    \"memory_bytes\": 2147483648,\n",
                "    \"disk_bytes\": 21474836480\n",
                "  }}\n",
                "}}\n"
            ),
            NETWORK_DIGEST = NETWORK_DIGEST,
            bridge_digest = bridge_digest
        )
        .into_bytes()
    }

    fn plan(desired: DisposableMacosServiceDesiredState) -> DisposableMacosServicePlan {
        plan_disposable_macos_service(
            desired,
            &installation_id(),
            Path::new("/Users/operator/build/smolrunner"),
            &Sha256Digest::parse(DIGEST_A).unwrap(),
            Path::new("/Users/operator/build/scaleset-bridge"),
            &Sha256Digest::parse(DIGEST_B).unwrap(),
            Path::new("/Users/operator/config/enrollment.json"),
            &enrollment(DIGEST_B),
        )
        .unwrap()
    }

    #[test]
    fn install_plan_binds_one_account_network_gate_and_worker_daemon() {
        let plan = plan(DisposableMacosServiceDesiredState::Installed);
        assert_eq!(plan.report().service_uid(), 502);
        assert_eq!(plan.report().primary_group_id(), 502);
        assert_eq!(plan.report().service_user_uuid().len(), 36);
        assert_eq!(plan.report().service_group_uuid().len(), 36);
        assert_ne!(
            plan.report().service_user_uuid(),
            plan.report().service_group_uuid()
        );
        assert_eq!(plan.report().service_user_uuid().as_bytes()[14], b'5');
        assert!(b"89AB".contains(&plan.report().service_user_uuid().as_bytes()[19]));
        assert_eq!(
            plan.report().network_policy_identity().as_str(),
            NETWORK_DIGEST
        );
        assert_eq!(plan.report().installation_id(), &installation_id());
        assert_eq!(plan.report().actions().len(), 9);
        let gate = String::from_utf8(plan.network_plist.clone()).unwrap();
        assert!(gate.contains("<string>network-activate</string>"));
        assert!(gate.contains("<string>502</string>"));
        assert!(gate.contains(NETWORK_DIGEST));
        assert!(gate.contains("<string>root</string>"));
        assert!(!gate.contains("enrollment"));
        assert!(!gate.contains("acme"));

        let worker = String::from_utf8(plan.worker_plist.clone()).unwrap();
        assert!(worker.contains("<string>worker</string>"));
        assert!(worker.contains("<string>serve</string>"));
        assert!(worker.contains(DISPOSABLE_MACOS_ENROLLMENT_PATH));
        assert!(worker.contains("<string>_smolrunner</string>"));
        assert!(!worker.contains("acme-ci"));
        assert!(!worker.contains("Iv1."));

        let ownership = String::from_utf8(plan.installation_record.clone()).unwrap();
        assert!(ownership.contains("\"installation_id\": \"smolrunner-install-0001\""));
        assert!(ownership.contains(plan.report().service_user_uuid()));
        assert!(ownership.contains(plan.report().service_group_uuid()));
        assert!(ownership.contains(DIGEST_A));
        assert!(ownership.contains(DIGEST_B));
        assert!(!ownership.contains("acme-ci"));
        assert!(!ownership.contains("Iv1."));

        let journal = plan.initial_journal_document();
        assert_eq!(journal.installation_id(), &installation_id());
        assert_eq!(journal.journal().records.len(), 9);
        assert_eq!(
            journal.journal().records[0].outcome,
            ActionOutcome::Completed
        );
        assert!(
            journal.journal().records[1..]
                .iter()
                .all(|record| record.outcome == ActionOutcome::Pending)
        );
        let encoded = crate::journal_document::encode_journal_document(&journal).unwrap();
        assert_eq!(
            crate::journal_document::decode_journal_document(&encoded).unwrap(),
            journal
        );
    }

    #[test]
    fn removal_orders_drain_before_exact_network_and_account_cleanup() {
        let plan = plan(DisposableMacosServiceDesiredState::Removed);
        let kinds = plan
            .report()
            .actions()
            .iter()
            .map(DisposableMacosServiceActionReport::kind)
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                DisposableMacosServiceActionKind::BeginLifecycle,
                DisposableMacosServiceActionKind::StopWorkerService,
                DisposableMacosServiceActionKind::RemoveWorkerService,
                DisposableMacosServiceActionKind::StopNetworkGate,
                DisposableMacosServiceActionKind::RemoveNetworkPolicy,
                DisposableMacosServiceActionKind::RemoveEnrollment,
                DisposableMacosServiceActionKind::RemoveExecutables,
                DisposableMacosServiceActionKind::RemoveServiceAccount,
                DisposableMacosServiceActionKind::CompleteLifecycle,
            ]
        );
    }

    #[test]
    fn enrollment_bridge_and_source_identity_mismatch_fail_closed() {
        let digest_a = Sha256Digest::parse(DIGEST_A).unwrap();
        let digest_b = Sha256Digest::parse(DIGEST_B).unwrap();
        let invalid = [
            plan_disposable_macos_service(
                DisposableMacosServiceDesiredState::Installed,
                &installation_id(),
                Path::new("relative/program"),
                &digest_a,
                Path::new("/source/bridge"),
                &digest_b,
                Path::new("/source/enrollment"),
                &enrollment(DIGEST_B),
            ),
            plan_disposable_macos_service(
                DisposableMacosServiceDesiredState::Installed,
                &installation_id(),
                Path::new("/source/program"),
                &digest_a,
                Path::new("/source/bridge"),
                &digest_a,
                Path::new("/source/enrollment"),
                &enrollment(DIGEST_B),
            ),
            plan_disposable_macos_service(
                DisposableMacosServiceDesiredState::Installed,
                &installation_id(),
                Path::new("/source/program"),
                &digest_a,
                Path::new("/source/program"),
                &digest_b,
                Path::new("/source/enrollment"),
                &enrollment(DIGEST_B),
            ),
            plan_disposable_macos_service(
                DisposableMacosServiceDesiredState::Installed,
                &installation_id(),
                Path::new(DISPOSABLE_MACOS_PROGRAM_PATH),
                &digest_a,
                Path::new("/source/bridge"),
                &digest_b,
                Path::new("/source/enrollment"),
                &enrollment(DIGEST_B),
            ),
            plan_disposable_macos_service(
                DisposableMacosServiceDesiredState::Installed,
                &installation_id(),
                Path::new(DISPOSABLE_NETWORK_PF_ANCHOR_PATH),
                &digest_a,
                Path::new("/source/bridge"),
                &digest_b,
                Path::new("/source/enrollment"),
                &enrollment(DIGEST_B),
            ),
        ];
        for result in invalid {
            let error = result.unwrap_err();
            assert_eq!(
                error.kind(),
                DisposableMacosServicePlanErrorKind::InvalidConfiguration
            );
            assert_eq!(error.code(), "disposable_macos_service_plan_invalid");
        }
    }

    #[test]
    fn desired_source_and_program_digest_change_identity_and_debug_is_private() {
        let base = plan(DisposableMacosServiceDesiredState::Installed);
        let removed = plan(DisposableMacosServiceDesiredState::Removed);
        assert_ne!(
            base.report().plan_identity(),
            removed.report().plan_identity()
        );

        let changed = plan_disposable_macos_service(
            DisposableMacosServiceDesiredState::Installed,
            &installation_id(),
            Path::new("/Users/operator/build/smolrunner-v2"),
            &Sha256Digest::parse(DIGEST_A).unwrap(),
            Path::new("/Users/operator/build/scaleset-bridge"),
            &Sha256Digest::parse(DIGEST_B).unwrap(),
            Path::new("/Users/operator/config/enrollment.json"),
            &enrollment(DIGEST_B),
        )
        .unwrap();
        assert_ne!(
            base.report().plan_identity(),
            changed.report().plan_identity()
        );

        let changed_digest = plan_disposable_macos_service(
            DisposableMacosServiceDesiredState::Installed,
            &installation_id(),
            Path::new("/Users/operator/build/smolrunner"),
            &Sha256Digest::parse(DIGEST_B).unwrap(),
            Path::new("/Users/operator/build/scaleset-bridge"),
            &Sha256Digest::parse(DIGEST_B).unwrap(),
            Path::new("/Users/operator/config/enrollment.json"),
            &enrollment(DIGEST_B),
        )
        .unwrap();
        assert_ne!(
            base.report().plan_identity(),
            changed_digest.report().plan_identity()
        );

        let changed_installation = plan_disposable_macos_service(
            DisposableMacosServiceDesiredState::Installed,
            &InstallationId::parse("smolrunner-install-0002").unwrap(),
            Path::new("/Users/operator/build/smolrunner"),
            &Sha256Digest::parse(DIGEST_A).unwrap(),
            Path::new("/Users/operator/build/scaleset-bridge"),
            &Sha256Digest::parse(DIGEST_B).unwrap(),
            Path::new("/Users/operator/config/enrollment.json"),
            &enrollment(DIGEST_B),
        )
        .unwrap();
        assert_ne!(
            base.report().plan_identity(),
            changed_installation.report().plan_identity()
        );

        let debug = format!("{base:?}");
        assert!(!debug.contains("/Users/operator"));
        assert!(!debug.contains("acme-ci"));
        assert!(!debug.contains("Iv1."));
        let report = serde_json::to_string(base.report()).unwrap();
        assert!(!report.contains("/Users/operator"));
        assert!(!report.contains("acme-ci"));
        assert!(!report.contains("Iv1."));
    }
}
