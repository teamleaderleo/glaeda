//! Exact root activation plan for the macOS hostile-CI PF gate.
//!
//! SmolRunner does not reproduce PF parsing or filtering semantics here. The plan binds the exact
//! root-owned inputs and fixed `pfctl` operations that its root transaction verifies and runs
//! before publishing the existing boot-volatile admission receipt.

use std::fmt;

#[cfg(target_os = "macos")]
use std::fs::File;
#[cfg(target_os = "macos")]
use std::io::{Read, Seek, SeekFrom, Write};
#[cfg(target_os = "macos")]
use std::os::fd::OwnedFd;
#[cfg(target_os = "macos")]
use std::os::unix::fs::MetadataExt;
#[cfg(target_os = "macos")]
use std::path::Path;
#[cfg(target_os = "macos")]
use std::time::Duration;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::disposable_network_gate::encode_disposable_network_gate_receipt;
use crate::disposable_network_policy::{DISPOSABLE_NETWORK_PF_ANCHOR, DisposableNetworkPolicyPlan};

#[cfg(target_os = "macos")]
use crate::process::{CommandSpec, ExecutionRecord, TimedCommandExecutor};

#[cfg(target_os = "macos")]
use rustix::fs::{self as rustix_fs, AtFlags, FileType, FlockOperation, Mode, OFlags};

pub const DISPOSABLE_NETWORK_GATE_ACTIVATION_SCHEMA_VERSION: u8 = 2;
pub const DISPOSABLE_NETWORK_PF_ANCHOR_PATH: &str =
    "/private/etc/pf.anchors/io.smolrunner.disposable-worker";
pub const DISPOSABLE_NETWORK_PF_CONFIGURATION_PATH: &str = "/etc/pf.conf";
pub const DISPOSABLE_NETWORK_GATE_ACTIVATION_LOCK_PATH: &str =
    "/private/var/run/smolrunner/.network-gate-activation-v1.lock";
const DISPOSABLE_NETWORK_PF_CANONICAL_ANCHOR_PATH: &str = DISPOSABLE_NETWORK_PF_ANCHOR_PATH;
const DISPOSABLE_NETWORK_PF_CANONICAL_CONFIGURATION_PATH: &str = "/private/etc/pf.conf";

pub(crate) const PFCTL_PROGRAM: &str = "/sbin/pfctl";
const ACTIVATION_DOMAIN: &[u8] = b"smolrunner.disposable-network-gate-activation.v2\0";
const ACTIVATION_COMMAND_CONTRACT: &[u8] = b"inspect-main-anchors-before-and-after\0inspect-main-rules-before-and-after\0inspect-pf-status-before-and-after\0load-exact-canonical-anchor-before-enable\0establish-simple-nontoken-reference\0never-disable-host-pf\0";
const PFCTL_TIMEOUT_SECONDS: u64 = 15;
#[cfg(target_os = "macos")]
const GATE_DIRECTORY_PATH: &str = "/private/var/run/smolrunner";
#[cfg(target_os = "macos")]
const GATE_RECEIPT_NAME: &str = "network-gate-v1.json";
#[cfg(target_os = "macos")]
const GATE_LOCK_NAME: &str = ".network-gate-activation-v1.lock";
#[cfg(target_os = "macos")]
const PFCTL_TIMEOUT: Duration = Duration::from_secs(PFCTL_TIMEOUT_SECONDS);
#[cfg(target_os = "macos")]
const SYSTEM_RANDOM_SOURCE: &str = "/dev/urandom";
#[cfg(target_os = "macos")]
const MAX_PF_CONFIGURATION_BYTES: usize = 256 * 1024;
#[cfg(target_os = "macos")]
const MAX_PF_ANCHOR_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DisposableNetworkGateActivationReport {
    schema_version: u8,
    backend: &'static str,
    pf_anchor: &'static str,
    activation_identity: Sha256Digest,
    policy_identity: Sha256Digest,
    service_uid: u32,
    privilege: &'static str,
    enforcement: &'static str,
    recovery: &'static str,
}

impl DisposableNetworkGateActivationReport {
    #[must_use]
    pub fn activation_identity(&self) -> &Sha256Digest {
        &self.activation_identity
    }

    #[must_use]
    pub fn policy_identity(&self) -> &Sha256Digest {
        &self.policy_identity
    }

    #[must_use]
    pub const fn service_uid(&self) -> u32 {
        self.service_uid
    }
}

/// Exact, non-executing activation input derived from one reviewed network policy.
///
/// The plan owns no filesystem or process authority. It deliberately keeps the anchor and receipt
/// bytes private so public callers cannot confuse planned bytes with observed enforcement.
#[derive(Clone, PartialEq, Eq)]
pub struct DisposableNetworkGateActivationPlan {
    report: DisposableNetworkGateActivationReport,
    network_policy: DisposableNetworkPolicyPlan,
    main_attachment: Vec<u8>,
    receipt: Vec<u8>,
}

impl fmt::Debug for DisposableNetworkGateActivationPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableNetworkGateActivationPlan")
            .field("report", &self.report)
            .finish()
    }
}

impl DisposableNetworkGateActivationPlan {
    #[must_use]
    pub const fn report(&self) -> &DisposableNetworkGateActivationReport {
        &self.report
    }

    #[cfg(any(target_os = "macos", test))]
    pub(crate) const fn network_policy(&self) -> &DisposableNetworkPolicyPlan {
        &self.network_policy
    }

    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn main_attachment(&self) -> &[u8] {
        &self.main_attachment
    }

    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn receipt(&self) -> &[u8] {
        &self.receipt
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableNetworkGateActivationPlanErrorKind {
    InvalidPolicy,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DisposableNetworkGateActivationPlanError {
    kind: DisposableNetworkGateActivationPlanErrorKind,
    code: &'static str,
}

impl DisposableNetworkGateActivationPlanError {
    #[must_use]
    pub const fn kind(self) -> DisposableNetworkGateActivationPlanErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for DisposableNetworkGateActivationPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableNetworkGateActivationPlanError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .finish()
    }
}

impl fmt::Display for DisposableNetworkGateActivationPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the disposable-network gate activation plan is invalid")
    }
}

impl std::error::Error for DisposableNetworkGateActivationPlanError {}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableNetworkGateActivationDisposition {
    Satisfied,
    Activated,
    Recovered,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DisposableNetworkGateActivationReceipt {
    schema_version: u8,
    disposition: DisposableNetworkGateActivationDisposition,
    activation_identity: Sha256Digest,
    policy_identity: Sha256Digest,
    service_uid: u32,
}

#[cfg(target_os = "macos")]
impl DisposableNetworkGateActivationReceipt {
    #[must_use]
    pub const fn disposition(&self) -> DisposableNetworkGateActivationDisposition {
        self.disposition
    }
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableNetworkGateActivationErrorKind {
    PrivilegeRequired,
    UnsafeState,
    Busy,
    CommandFailed,
    RecoveryRequired,
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DisposableNetworkGateActivationError {
    kind: DisposableNetworkGateActivationErrorKind,
    code: &'static str,
}

#[cfg(target_os = "macos")]
impl DisposableNetworkGateActivationError {
    #[must_use]
    pub const fn kind(self) -> DisposableNetworkGateActivationErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

#[cfg(target_os = "macos")]
impl fmt::Debug for DisposableNetworkGateActivationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableNetworkGateActivationError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .finish()
    }
}

#[cfg(target_os = "macos")]
impl fmt::Display for DisposableNetworkGateActivationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the disposable-network gate could not be activated")
    }
}

#[cfg(target_os = "macos")]
impl std::error::Error for DisposableNetworkGateActivationError {}

/// Bind the exact root PF inputs and receipt for one network policy without touching the host.
///
/// # Errors
///
/// Returns a bounded refusal if the fixed receipt cannot be encoded.
pub fn plan_disposable_network_gate_activation(
    network_policy: &DisposableNetworkPolicyPlan,
) -> Result<DisposableNetworkGateActivationPlan, DisposableNetworkGateActivationPlanError> {
    let main_attachment = format!(
        "anchor \"{DISPOSABLE_NETWORK_PF_ANCHOR}\"\nload anchor \"{DISPOSABLE_NETWORK_PF_ANCHOR}\" from \"{DISPOSABLE_NETWORK_PF_ANCHOR_PATH}\"\n"
    )
    .into_bytes();
    let receipt = encode_disposable_network_gate_receipt(network_policy).map_err(|_| {
        DisposableNetworkGateActivationPlanError {
            kind: DisposableNetworkGateActivationPlanErrorKind::InvalidPolicy,
            code: "disposable_network_gate_activation_policy_invalid",
        }
    })?;
    let activation_identity = activation_identity(network_policy, &main_attachment, &receipt);
    Ok(DisposableNetworkGateActivationPlan {
        report: DisposableNetworkGateActivationReport {
            schema_version: DISPOSABLE_NETWORK_GATE_ACTIVATION_SCHEMA_VERSION,
            backend: "apple_pfctl",
            pf_anchor: DISPOSABLE_NETWORK_PF_ANCHOR,
            activation_identity,
            policy_identity: network_policy.report().policy_identity().clone(),
            service_uid: network_policy.report().service_uid(),
            privilege: "root_only",
            enforcement: "delegated_to_macos_pf",
            recovery: "idempotent_simple_reference_then_receipt",
        },
        network_policy: network_policy.clone(),
        main_attachment,
        receipt,
    })
}

fn activation_identity(
    network_policy: &DisposableNetworkPolicyPlan,
    main_attachment: &[u8],
    receipt: &[u8],
) -> Sha256Digest {
    let mut hasher = Sha256::new();
    hasher.update(ACTIVATION_DOMAIN);
    hash_field(
        &mut hasher,
        network_policy
            .report()
            .policy_identity()
            .as_str()
            .as_bytes(),
    );
    hash_field(&mut hasher, DISPOSABLE_NETWORK_PF_ANCHOR_PATH.as_bytes());
    hash_field(
        &mut hasher,
        DISPOSABLE_NETWORK_PF_CONFIGURATION_PATH.as_bytes(),
    );
    hash_field(
        &mut hasher,
        DISPOSABLE_NETWORK_PF_CANONICAL_CONFIGURATION_PATH.as_bytes(),
    );
    hash_field(
        &mut hasher,
        DISPOSABLE_NETWORK_GATE_ACTIVATION_LOCK_PATH.as_bytes(),
    );
    hash_field(&mut hasher, PFCTL_PROGRAM.as_bytes());
    hash_field(&mut hasher, ACTIVATION_COMMAND_CONTRACT);
    hash_field(&mut hasher, &PFCTL_TIMEOUT_SECONDS.to_be_bytes());
    hash_field(&mut hasher, main_attachment);
    hash_field(&mut hasher, network_policy.anchor_bytes());
    hash_field(&mut hasher, receipt);
    Sha256Digest::parse(&format!("sha256:{:x}", hasher.finalize()))
        .expect("SHA-256 formatting is canonical")
}

fn hash_field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(bytes);
}

/// Activate the exact enrolled PF anchor and publish its boot-volatile root receipt.
///
/// The fixed root paths must already have been installed by the separately approved installer.
/// This call serializes activation, verifies the main-ruleset attachment and exact anchor bytes,
/// loads the exact canonical anchor before enabling PF, and publishes the public receipt last. It
/// never invokes an implicit shell and never disables the host-wide packet filter.
///
/// # Errors
///
/// Returns a bounded, path-free refusal for non-root callers, unsafe or drifting installed state,
/// contention, command failure, or an ambiguous recovery state.
#[cfg(target_os = "macos")]
pub fn activate_disposable_network_gate(
    plan: &DisposableNetworkGateActivationPlan,
    executor: &impl TimedCommandExecutor,
) -> Result<DisposableNetworkGateActivationReceipt, DisposableNetworkGateActivationError> {
    let paths = ActivationPaths {
        configuration_directory: Path::new("/private/etc"),
        anchor_directory: Path::new("/private/etc/pf.anchors"),
        configuration: Path::new(DISPOSABLE_NETWORK_PF_CANONICAL_CONFIGURATION_PATH),
        anchor: Path::new(DISPOSABLE_NETWORK_PF_CANONICAL_ANCHOR_PATH),
        gate_directory: Path::new(GATE_DIRECTORY_PATH),
    };
    activate_at(
        plan,
        executor,
        &paths,
        ActivationIdentity {
            effective_uid: rustix::process::geteuid().as_raw(),
            effective_gid: rustix::process::getegid().as_raw(),
            expected_uid: 0,
            expected_gid: 0,
            require_root: true,
        },
    )
}

#[cfg(target_os = "macos")]
struct ActivationPaths<'a> {
    configuration_directory: &'a Path,
    anchor_directory: &'a Path,
    configuration: &'a Path,
    anchor: &'a Path,
    gate_directory: &'a Path,
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy)]
struct ActivationIdentity {
    effective_uid: u32,
    effective_gid: u32,
    expected_uid: u32,
    expected_gid: u32,
    require_root: bool,
}

#[cfg(target_os = "macos")]
fn activate_at(
    plan: &DisposableNetworkGateActivationPlan,
    executor: &impl TimedCommandExecutor,
    paths: &ActivationPaths<'_>,
    identity: ActivationIdentity,
) -> Result<DisposableNetworkGateActivationReceipt, DisposableNetworkGateActivationError> {
    let ActivationIdentity {
        effective_uid,
        effective_gid,
        expected_uid,
        expected_gid,
        require_root,
    } = identity;
    if effective_uid != expected_uid
        || effective_gid != expected_gid
        || (require_root && (expected_uid != 0 || expected_gid != 0))
    {
        return Err(activation_error(
            DisposableNetworkGateActivationErrorKind::PrivilegeRequired,
            "disposable_network_gate_activation_requires_root",
        ));
    }
    let gate_directory = open_gate_directory(paths.gate_directory, expected_uid, expected_gid)?;
    let _lock = acquire_activation_lock(&gate_directory, expected_uid, expected_gid)?;
    synchronize_directory(&gate_directory)?;
    verify_directory(paths.gate_directory, expected_uid, expected_gid, 0o755)?;

    let receipt_present = read_optional_exact_file_at(
        &gate_directory,
        GATE_RECEIPT_NAME,
        expected_uid,
        expected_gid,
        0o444,
        crate::disposable_network_gate::MAX_DISPOSABLE_NETWORK_GATE_RECEIPT_BYTES,
    )?
    .map(|bytes| {
        if bytes == plan.receipt() {
            Ok(())
        } else {
            Err(unsafe_state("disposable_network_gate_receipt_mismatch"))
        }
    })
    .transpose()?
    .is_some();

    if receipt_present {
        remove_exact_file(
            &gate_directory,
            GATE_RECEIPT_NAME,
            plan.receipt(),
            expected_uid,
            expected_gid,
            0o444,
        )?;
    }

    revalidate_installed_inputs(plan, paths, expected_uid, expected_gid)?;
    let anchors = execute_pf(executor, inspect_main_anchor_spec())?;
    require_main_anchor(&anchors)?;
    let rules = execute_pf(executor, inspect_main_rules_spec())?;
    require_first_filter_rule(&rules)?;

    // Load the exact canonical root-owned anchor before enabling PF. If PF is already enabled this
    // replaces only SmolRunner's live anchor; if PF is disabled no packet is filtered until the
    // exact anchor has been loaded and revalidated.
    execute_pf(executor, load_anchor_spec())?;
    revalidate_installed_inputs(plan, paths, expected_uid, expected_gid)?;
    let anchors = execute_pf(executor, inspect_main_anchor_spec())?;
    require_main_anchor(&anchors)?;
    let rules = execute_pf(executor, inspect_main_rules_spec())?;
    require_first_filter_rule(&rules)?;

    let status = execute_pf(executor, inspect_pf_status_spec())?;
    let was_enabled = require_pf_status(&status)? == PfStatus::Enabled;
    // DIOCSTART (pfctl -e) establishes the kernel's simple non-token enable reference. Apple PF
    // makes this idempotent: when PF is already enabled solely by token holders it first adds the
    // simple reference and then reports EEXIST; when that reference already exists it only reports
    // EEXIST. This keeps PF available if other components release their tokens without creating an
    // external token-persistence transaction of our own.
    enable_pf(executor)?;

    revalidate_installed_inputs(plan, paths, expected_uid, expected_gid)?;
    let anchors = execute_pf(executor, inspect_main_anchor_spec())?;
    require_main_anchor(&anchors)?;
    let rules = execute_pf(executor, inspect_main_rules_spec())?;
    require_first_filter_rule(&rules)?;
    let status = execute_pf(executor, inspect_pf_status_spec())?;
    if require_pf_status(&status)? != PfStatus::Enabled {
        return Err(activation_error(
            DisposableNetworkGateActivationErrorKind::RecoveryRequired,
            "disposable_network_gate_pf_not_enabled",
        ));
    }

    publish_exact_file(
        &gate_directory,
        GATE_RECEIPT_NAME,
        plan.receipt(),
        0o444,
        expected_uid,
        expected_gid,
    )?;
    synchronize_directory(&gate_directory)?;
    let disposition = if receipt_present {
        DisposableNetworkGateActivationDisposition::Satisfied
    } else if was_enabled {
        DisposableNetworkGateActivationDisposition::Recovered
    } else {
        DisposableNetworkGateActivationDisposition::Activated
    };
    Ok(DisposableNetworkGateActivationReceipt {
        schema_version: DISPOSABLE_NETWORK_GATE_ACTIVATION_SCHEMA_VERSION,
        disposition,
        activation_identity: plan.report().activation_identity().clone(),
        policy_identity: plan.report().policy_identity().clone(),
        service_uid: plan.report().service_uid(),
    })
}

#[cfg(target_os = "macos")]
fn revalidate_installed_inputs(
    plan: &DisposableNetworkGateActivationPlan,
    paths: &ActivationPaths<'_>,
    expected_uid: u32,
    expected_gid: u32,
) -> Result<(), DisposableNetworkGateActivationError> {
    verify_directory(
        paths.configuration_directory,
        expected_uid,
        expected_gid,
        0o755,
    )?;
    verify_directory(paths.anchor_directory, expected_uid, expected_gid, 0o755)?;
    verify_directory(paths.gate_directory, expected_uid, expected_gid, 0o755)?;
    let configuration = read_exact_file(
        paths.configuration,
        expected_uid,
        expected_gid,
        0o644,
        MAX_PF_CONFIGURATION_BYTES,
    )?;
    validate_main_attachment(&configuration, plan.main_attachment())?;
    let anchor = read_exact_file(
        paths.anchor,
        expected_uid,
        expected_gid,
        0o644,
        MAX_PF_ANCHOR_BYTES,
    )?;
    if anchor != plan.network_policy().anchor_bytes() {
        return Err(unsafe_state("disposable_network_gate_anchor_mismatch"));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn validate_main_attachment(
    configuration: &[u8],
    attachment: &[u8],
) -> Result<(), DisposableNetworkGateActivationError> {
    let text = std::str::from_utf8(configuration)
        .map_err(|_| unsafe_state("disposable_network_gate_pf_configuration_invalid"))?;
    let expected = std::str::from_utf8(attachment)
        .map_err(|_| unsafe_state("disposable_network_gate_pf_configuration_invalid"))?;
    if text.as_bytes().contains(&0) || text.contains('\r') || text.matches(expected).count() != 1 {
        return Err(unsafe_state(
            "disposable_network_gate_pf_attachment_missing",
        ));
    }
    let anchor_mentions = text
        .lines()
        .filter(|line| line.contains(DISPOSABLE_NETWORK_PF_ANCHOR))
        .count();
    if anchor_mentions != 2 {
        return Err(unsafe_state(
            "disposable_network_gate_pf_attachment_ambiguous",
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn execute_pf(
    executor: &impl TimedCommandExecutor,
    spec: CommandSpec,
) -> Result<ExecutionRecord, DisposableNetworkGateActivationError> {
    let record = executor
        .execute_with_timeout(&spec, PFCTL_TIMEOUT)
        .map_err(|_| {
            activation_error(
                DisposableNetworkGateActivationErrorKind::CommandFailed,
                "disposable_network_gate_pf_command_failed",
            )
        })?;
    if !record.success {
        return Err(activation_error(
            DisposableNetworkGateActivationErrorKind::CommandFailed,
            "disposable_network_gate_pf_command_failed",
        ));
    }
    Ok(record)
}

#[cfg(target_os = "macos")]
fn enable_pf(
    executor: &impl TimedCommandExecutor,
) -> Result<(), DisposableNetworkGateActivationError> {
    let record = executor
        .execute_with_timeout(&enable_pf_spec(), PFCTL_TIMEOUT)
        .map_err(|_| {
            activation_error(
                DisposableNetworkGateActivationErrorKind::RecoveryRequired,
                "disposable_network_gate_pf_enable_outcome_unknown",
            )
        })?;
    if record.success
        || (record.status == Some(1)
            && record.stdout.is_empty()
            && record.stderr == "pfctl: pf already enabled\n")
    {
        return Ok(());
    }
    Err(activation_error(
        DisposableNetworkGateActivationErrorKind::CommandFailed,
        "disposable_network_gate_pf_command_failed",
    ))
}

#[cfg(target_os = "macos")]
fn require_main_anchor(
    record: &ExecutionRecord,
) -> Result<(), DisposableNetworkGateActivationError> {
    let count = record
        .stdout
        .lines()
        .filter(|line| line.trim() == DISPOSABLE_NETWORK_PF_ANCHOR)
        .count();
    if count != 1 {
        return Err(unsafe_state("disposable_network_gate_live_anchor_missing"));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn require_first_filter_rule(
    record: &ExecutionRecord,
) -> Result<(), DisposableNetworkGateActivationError> {
    let expected = format!("anchor \"{DISPOSABLE_NETWORK_PF_ANCHOR}\" all");
    let mut rules = record
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    if rules.next() != Some(expected.as_str()) || rules.any(|line| line == expected) {
        return Err(unsafe_state(
            "disposable_network_gate_live_rule_order_invalid",
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PfStatus {
    Enabled,
    Disabled,
}

#[cfg(target_os = "macos")]
fn require_pf_status(
    record: &ExecutionRecord,
) -> Result<PfStatus, DisposableNetworkGateActivationError> {
    let mut statuses = record
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("Status: "));
    let status = statuses
        .next()
        .ok_or_else(|| unsafe_state("disposable_network_gate_pf_status_invalid"))?;
    if statuses.next().is_some() {
        return Err(unsafe_state("disposable_network_gate_pf_status_invalid"));
    }
    if status == "Status: Disabled" {
        return Ok(PfStatus::Disabled);
    }
    if status == "Status: Enabled" || status.starts_with("Status: Enabled for ") {
        return Ok(PfStatus::Enabled);
    }
    Err(unsafe_state("disposable_network_gate_pf_status_invalid"))
}

#[cfg(target_os = "macos")]
const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

#[cfg(target_os = "macos")]
fn verify_directory(
    path: &Path,
    owner: u32,
    group: u32,
    mode: u32,
) -> Result<(), DisposableNetworkGateActivationError> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| unsafe_state("disposable_network_gate_directory_unsafe"))?;
    if !metadata.file_type().is_dir()
        || metadata.uid() != owner
        || metadata.gid() != group
        || metadata.mode() & 0o7777 != mode
    {
        return Err(unsafe_state("disposable_network_gate_directory_unsafe"));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn read_exact_file(
    path: &Path,
    owner: u32,
    group: u32,
    mode: u16,
    maximum: usize,
) -> Result<Vec<u8>, DisposableNetworkGateActivationError> {
    let parent = path
        .parent()
        .ok_or_else(|| unsafe_state("disposable_network_gate_file_unsafe"))?;
    let name = path
        .file_name()
        .ok_or_else(|| unsafe_state("disposable_network_gate_file_unsafe"))?;
    let directory = rustix_fs::open(parent, DIRECTORY_FLAGS, Mode::empty())
        .map_err(|_| unsafe_state("disposable_network_gate_file_unsafe"))?;
    read_exact_file_at(&directory, name, owner, group, mode, maximum)
}

#[cfg(target_os = "macos")]
fn read_exact_file_at(
    directory: &OwnedFd,
    name: &std::ffi::OsStr,
    owner: u32,
    group: u32,
    mode: u16,
    maximum: usize,
) -> Result<Vec<u8>, DisposableNetworkGateActivationError> {
    let fd = rustix_fs::openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| unsafe_state("disposable_network_gate_file_unsafe"))?;
    let mut file = File::from(fd);
    let before =
        rustix_fs::fstat(&file).map_err(|_| unsafe_state("disposable_network_gate_file_unsafe"))?;
    inspect_file(&before, owner, group, mode, maximum)?;
    let path_before = rustix_fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| unsafe_state("disposable_network_gate_file_unsafe"))?;
    require_same_file(&before, &path_before)?;
    let first = read_bounded(&mut file, maximum)?;
    file.seek(SeekFrom::Start(0))
        .map_err(|_| unsafe_state("disposable_network_gate_file_changed"))?;
    let second = read_bounded(&mut file, maximum)?;
    if first != second {
        return Err(unsafe_state("disposable_network_gate_file_changed"));
    }
    let after = rustix_fs::fstat(&file)
        .map_err(|_| unsafe_state("disposable_network_gate_file_changed"))?;
    let path_after = rustix_fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| unsafe_state("disposable_network_gate_file_changed"))?;
    require_same_snapshot(&before, &after)?;
    require_same_snapshot(&before, &path_after)?;
    Ok(first)
}

#[cfg(target_os = "macos")]
fn read_optional_exact_file_at(
    directory: &OwnedFd,
    name: &str,
    owner: u32,
    group: u32,
    mode: u16,
    maximum: usize,
) -> Result<Option<Vec<u8>>, DisposableNetworkGateActivationError> {
    match rustix_fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => {
            read_exact_file_at(directory, name.as_ref(), owner, group, mode, maximum).map(Some)
        }
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Err(_) => Err(unsafe_state("disposable_network_gate_file_unsafe")),
    }
}

#[cfg(target_os = "macos")]
fn read_bounded(
    file: &mut File,
    maximum: usize,
) -> Result<Vec<u8>, DisposableNetworkGateActivationError> {
    let mut bytes = Vec::with_capacity(maximum.min(8 * 1024));
    file.take(u64::try_from(maximum + 1).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)
        .map_err(|_| unsafe_state("disposable_network_gate_file_unsafe"))?;
    if bytes.len() > maximum {
        return Err(unsafe_state("disposable_network_gate_file_unsafe"));
    }
    Ok(bytes)
}

#[cfg(target_os = "macos")]
fn inspect_file(
    stat: &rustix_fs::Stat,
    owner: u32,
    group: u32,
    mode: u16,
    maximum: usize,
) -> Result<(), DisposableNetworkGateActivationError> {
    if !FileType::from_raw_mode(stat.st_mode).is_file()
        || stat.st_nlink != 1
        || stat.st_uid != owner
        || stat.st_gid != group
        || stat.st_mode & 0o7777 != mode
        || stat.st_size < 0
        || usize::try_from(stat.st_size)
            .ok()
            .is_none_or(|size| size > maximum)
    {
        return Err(unsafe_state("disposable_network_gate_file_unsafe"));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn require_same_file(
    left: &rustix_fs::Stat,
    right: &rustix_fs::Stat,
) -> Result<(), DisposableNetworkGateActivationError> {
    if left.st_dev != right.st_dev || left.st_ino != right.st_ino {
        return Err(unsafe_state("disposable_network_gate_file_changed"));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn require_same_snapshot(
    left: &rustix_fs::Stat,
    right: &rustix_fs::Stat,
) -> Result<(), DisposableNetworkGateActivationError> {
    if left.st_dev != right.st_dev
        || left.st_ino != right.st_ino
        || left.st_mode != right.st_mode
        || left.st_nlink != right.st_nlink
        || left.st_uid != right.st_uid
        || left.st_gid != right.st_gid
        || left.st_size != right.st_size
        || left.st_mtime != right.st_mtime
        || left.st_mtime_nsec != right.st_mtime_nsec
        || left.st_ctime != right.st_ctime
        || left.st_ctime_nsec != right.st_ctime_nsec
    {
        return Err(unsafe_state("disposable_network_gate_file_changed"));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn open_gate_directory(
    path: &Path,
    owner: u32,
    group: u32,
) -> Result<OwnedFd, DisposableNetworkGateActivationError> {
    let directory = rustix_fs::open(path, DIRECTORY_FLAGS, Mode::empty())
        .map_err(|_| unsafe_state("disposable_network_gate_directory_unsafe"))?;
    let stat = rustix_fs::fstat(&directory)
        .map_err(|_| unsafe_state("disposable_network_gate_directory_unsafe"))?;
    if !FileType::from_raw_mode(stat.st_mode).is_dir()
        || stat.st_uid != owner
        || stat.st_gid != group
        || stat.st_mode & 0o7777 != 0o755
    {
        return Err(unsafe_state("disposable_network_gate_directory_unsafe"));
    }
    let resolved = rustix_fs::stat(path)
        .map_err(|_| unsafe_state("disposable_network_gate_directory_unsafe"))?;
    require_same_file(&stat, &resolved)?;
    Ok(directory)
}

#[cfg(target_os = "macos")]
struct ActivationLock {
    fd: OwnedFd,
}

#[cfg(target_os = "macos")]
impl Drop for ActivationLock {
    fn drop(&mut self) {
        let _ = rustix_fs::flock(&self.fd, FlockOperation::Unlock);
    }
}

#[cfg(target_os = "macos")]
fn acquire_activation_lock(
    directory: &OwnedFd,
    owner: u32,
    group: u32,
) -> Result<ActivationLock, DisposableNetworkGateActivationError> {
    let flags = OFlags::RDWR | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let fd = rustix_fs::openat(directory, GATE_LOCK_NAME, flags, Mode::empty())
        .map_err(|_| unsafe_state("disposable_network_gate_lock_unsafe"))?;
    let before =
        rustix_fs::fstat(&fd).map_err(|_| unsafe_state("disposable_network_gate_lock_unsafe"))?;
    inspect_file(&before, owner, group, 0o600, 0)?;
    let path_before = rustix_fs::statat(directory, GATE_LOCK_NAME, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| unsafe_state("disposable_network_gate_lock_unsafe"))?;
    require_same_snapshot(&before, &path_before)?;
    rustix_fs::flock(&fd, FlockOperation::NonBlockingLockExclusive).map_err(|_| {
        activation_error(
            DisposableNetworkGateActivationErrorKind::Busy,
            "disposable_network_gate_activation_busy",
        )
    })?;
    let guard = ActivationLock { fd };
    let after = rustix_fs::fstat(&guard.fd)
        .map_err(|_| unsafe_state("disposable_network_gate_lock_unsafe"))?;
    let path_after = rustix_fs::statat(directory, GATE_LOCK_NAME, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| unsafe_state("disposable_network_gate_lock_unsafe"))?;
    require_same_snapshot(&before, &after)?;
    require_same_snapshot(&before, &path_after)?;
    Ok(guard)
}

#[cfg(target_os = "macos")]
fn publish_exact_file(
    directory: &OwnedFd,
    name: &str,
    bytes: &[u8],
    mode: u16,
    owner: u32,
    group: u32,
) -> Result<(), DisposableNetworkGateActivationError> {
    synchronize_directory(directory)?;
    if let Some(existing) =
        read_optional_exact_file_at(directory, name, owner, group, mode, bytes.len())?
    {
        if existing == bytes {
            return Ok(());
        }
        return Err(unsafe_state("disposable_network_gate_publication_conflict"));
    }
    let mut random = [0_u8; 16];
    File::open(SYSTEM_RANDOM_SOURCE)
        .and_then(|mut source| source.read_exact(&mut random))
        .map_err(|_| unsafe_state("disposable_network_gate_publication_failed"))?;
    let random = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let stage_name = format!(".{name}.next.{random}");
    let staged = rustix_fs::openat(
        directory,
        stage_name.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(mode),
    )
    .map_err(|_| unsafe_state("disposable_network_gate_publication_failed"))?;
    rustix_fs::fchmod(&staged, Mode::from_raw_mode(mode))
        .map_err(|_| unsafe_state("disposable_network_gate_publication_failed"))?;
    let mut stage = StagedFile {
        directory,
        name: stage_name,
        file: File::from(staged),
        armed: true,
    };
    stage
        .file
        .write_all(bytes)
        .and_then(|()| stage.file.sync_all())
        .map_err(|_| unsafe_state("disposable_network_gate_publication_failed"))?;
    let stat = rustix_fs::fstat(&stage.file)
        .map_err(|_| unsafe_state("disposable_network_gate_publication_failed"))?;
    inspect_file(&stat, owner, group, mode, bytes.len())?;
    if usize::try_from(stat.st_size).ok() != Some(bytes.len()) {
        return Err(unsafe_state("disposable_network_gate_publication_failed"));
    }
    let path_stat = rustix_fs::statat(directory, stage.name.as_str(), AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| unsafe_state("disposable_network_gate_publication_failed"))?;
    require_same_snapshot(&stat, &path_stat)?;
    rustix_fs::renameat(directory, stage.name.as_str(), directory, name)
        .map_err(|_| unsafe_state("disposable_network_gate_publication_failed"))?;
    stage.armed = false;
    synchronize_directory(directory)?;
    let published = read_exact_file_at(directory, name.as_ref(), owner, group, mode, bytes.len())?;
    if published != bytes {
        return Err(unsafe_state("disposable_network_gate_publication_failed"));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn remove_exact_file(
    directory: &OwnedFd,
    name: &str,
    expected: &[u8],
    owner: u32,
    group: u32,
    mode: u16,
) -> Result<(), DisposableNetworkGateActivationError> {
    let bytes = read_exact_file_at(directory, name.as_ref(), owner, group, mode, expected.len())?;
    if bytes != expected {
        return Err(unsafe_state("disposable_network_gate_removal_conflict"));
    }
    rustix_fs::unlinkat(directory, name, AtFlags::empty())
        .map_err(|_| unsafe_state("disposable_network_gate_removal_failed"))?;
    synchronize_directory(directory)
}

#[cfg(target_os = "macos")]
struct StagedFile<'a> {
    directory: &'a OwnedFd,
    name: String,
    file: File,
    armed: bool,
}

#[cfg(target_os = "macos")]
impl Drop for StagedFile<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = rustix_fs::unlinkat(self.directory, self.name.as_str(), AtFlags::empty());
            let _ = synchronize_directory(self.directory);
        }
    }
}

#[cfg(target_os = "macos")]
fn synchronize_directory(directory: &OwnedFd) -> Result<(), DisposableNetworkGateActivationError> {
    let duplicate = rustix::io::dup(directory)
        .map_err(|_| unsafe_state("disposable_network_gate_directory_sync_failed"))?;
    File::from(duplicate)
        .sync_all()
        .map_err(|_| unsafe_state("disposable_network_gate_directory_sync_failed"))
}

#[cfg(target_os = "macos")]
const fn unsafe_state(code: &'static str) -> DisposableNetworkGateActivationError {
    activation_error(DisposableNetworkGateActivationErrorKind::UnsafeState, code)
}

#[cfg(target_os = "macos")]
const fn activation_error(
    kind: DisposableNetworkGateActivationErrorKind,
    code: &'static str,
) -> DisposableNetworkGateActivationError {
    DisposableNetworkGateActivationError { kind, code }
}

#[cfg(target_os = "macos")]
pub(crate) fn inspect_main_anchor_spec() -> CommandSpec {
    CommandSpec::new(PFCTL_PROGRAM)
        .argument("-s")
        .argument("Anchors")
}

#[cfg(target_os = "macos")]
pub(crate) fn inspect_main_rules_spec() -> CommandSpec {
    CommandSpec::new(PFCTL_PROGRAM)
        .argument("-s")
        .argument("rules")
}

#[cfg(target_os = "macos")]
pub(crate) fn inspect_pf_status_spec() -> CommandSpec {
    CommandSpec::new(PFCTL_PROGRAM)
        .argument("-s")
        .argument("info")
}

#[cfg(target_os = "macos")]
pub(crate) fn enable_pf_spec() -> CommandSpec {
    CommandSpec::new(PFCTL_PROGRAM).argument("-e")
}

#[cfg(target_os = "macos")]
pub(crate) fn load_anchor_spec() -> CommandSpec {
    CommandSpec::new(PFCTL_PROGRAM)
        .argument("-a")
        .argument(DISPOSABLE_NETWORK_PF_ANCHOR)
        .argument("-f")
        .argument(DISPOSABLE_NETWORK_PF_ANCHOR_PATH)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disposable_network_policy::plan_disposable_network_policy;
    use crate::disposable_prepared_template::current_disposable_prepared_template;

    fn plan() -> DisposableNetworkGateActivationPlan {
        let template = current_disposable_prepared_template().unwrap();
        let network = plan_disposable_network_policy(502, &template).unwrap();
        plan_disposable_network_gate_activation(&network).unwrap()
    }

    #[test]
    fn activation_plan_is_exact_and_non_executing() {
        let plan = plan();
        assert_eq!(plan.report().service_uid(), 502);
        assert_eq!(
            plan.report().policy_identity().as_str(),
            "sha256:65ceec8974086e378f216acc555724cb40b08ccc047391dedd0b6f17df72587e"
        );
        assert_eq!(
            plan.main_attachment(),
            concat!(
                "anchor \"io.smolrunner.disposable-worker\"\n",
                "load anchor \"io.smolrunner.disposable-worker\" from \"/private/etc/pf.anchors/io.smolrunner.disposable-worker\"\n"
            )
            .as_bytes()
        );
        assert_eq!(
            plan.report().activation_identity().as_str(),
            "sha256:930a5d9c20260f282eb33ca28ed68443b258c17e7f13113c1b60153fab2f44bd"
        );
        assert!(format!("{plan:?}").contains("root_only"));
        assert!(!format!("{plan:?}").contains("/etc/pf.conf"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn pfctl_commands_and_status_response_are_fixed() {
        assert_eq!(
            inspect_main_anchor_spec().displayed_argv(),
            ["/sbin/pfctl", "-s", "Anchors"]
        );
        assert_eq!(
            inspect_main_rules_spec().displayed_argv(),
            ["/sbin/pfctl", "-s", "rules"]
        );
        assert_eq!(
            inspect_pf_status_spec().displayed_argv(),
            ["/sbin/pfctl", "-s", "info"]
        );
        assert_eq!(enable_pf_spec().displayed_argv(), ["/sbin/pfctl", "-e"]);
        assert_eq!(
            load_anchor_spec().displayed_argv(),
            [
                "/sbin/pfctl",
                "-a",
                "io.smolrunner.disposable-worker",
                "-f",
                "/private/etc/pf.anchors/io.smolrunner.disposable-worker"
            ]
        );

        let enabled = ExecutionRecord {
            argv: Vec::new(),
            environment_keys: Vec::new(),
            status: Some(0),
            success: true,
            stdout: "Status: Enabled for 0 days 00:00:01\n".to_owned(),
            stderr: String::new(),
        };
        assert_eq!(require_pf_status(&enabled).unwrap(), PfStatus::Enabled);
        let mut disabled = enabled.clone();
        disabled.stdout = "Status: Disabled\n".to_owned();
        assert_eq!(require_pf_status(&disabled).unwrap(), PfStatus::Disabled);
        for stdout in [
            "",
            "Status: Unknown\n",
            "Status: Enabled\nStatus: Disabled\n",
        ] {
            let mut invalid = enabled.clone();
            invalid.stdout = stdout.to_owned();
            assert_eq!(
                require_pf_status(&invalid).unwrap_err().code(),
                "disposable_network_gate_pf_status_invalid"
            );
        }
    }

    #[cfg(target_os = "macos")]
    mod activation {
        use std::collections::VecDeque;
        use std::io;
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        use std::path::PathBuf;
        use std::sync::Mutex;
        use std::sync::atomic::{AtomicU64, Ordering};

        use super::*;
        use crate::process::{CommandExecutor, TimedCommandExecutor};

        static FIXTURE_COUNTER: AtomicU64 = AtomicU64::new(1);

        struct Fixture {
            root: PathBuf,
            configuration_directory: PathBuf,
            anchor_directory: PathBuf,
            configuration: PathBuf,
            anchor: PathBuf,
            gate_directory: PathBuf,
            uid: u32,
            gid: u32,
        }

        impl Fixture {
            fn new(plan: &DisposableNetworkGateActivationPlan) -> Self {
                let sequence = FIXTURE_COUNTER.fetch_add(1, Ordering::Relaxed);
                let root = std::env::current_dir()
                    .unwrap()
                    .join("target/network-gate-activation-fixtures")
                    .join(format!("{}-{sequence}", std::process::id()));
                std::fs::create_dir_all(root.parent().unwrap()).unwrap();
                std::fs::create_dir(&root).unwrap();
                let configuration_directory = root.join("etc");
                let anchor_directory = configuration_directory.join("pf.anchors");
                let gate_directory = root.join("run");
                for directory in [&configuration_directory, &anchor_directory, &gate_directory] {
                    std::fs::create_dir(directory).unwrap();
                    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o755))
                        .unwrap();
                }
                let configuration = configuration_directory.join("pf.conf");
                let anchor = anchor_directory.join("io.smolrunner.disposable-worker");
                write_mode(&gate_directory.join(GATE_LOCK_NAME), b"", 0o600);
                let config = format!(
                    "# fixture\nscrub-anchor \"com.apple/*\"\n{}anchor \"com.apple/*\"\n",
                    std::str::from_utf8(plan.main_attachment()).unwrap()
                );
                write_mode(&configuration, config.as_bytes(), 0o644);
                write_mode(&anchor, plan.network_policy().anchor_bytes(), 0o644);
                let metadata = std::fs::metadata(&root).unwrap();
                Self {
                    root,
                    configuration_directory,
                    anchor_directory,
                    configuration,
                    anchor,
                    gate_directory,
                    uid: metadata.uid(),
                    gid: metadata.gid(),
                }
            }

            fn paths(&self) -> ActivationPaths<'_> {
                ActivationPaths {
                    configuration_directory: &self.configuration_directory,
                    anchor_directory: &self.anchor_directory,
                    configuration: &self.configuration,
                    anchor: &self.anchor,
                    gate_directory: &self.gate_directory,
                }
            }
        }

        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.root);
                if let Some(parent) = self.root.parent() {
                    let _ = std::fs::remove_dir(parent);
                }
            }
        }

        fn write_mode(path: &Path, bytes: &[u8], mode: u32) {
            std::fs::write(path, bytes).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        }

        #[derive(Default)]
        struct FakeExecutor {
            responses: Mutex<VecDeque<io::Result<ExecutionRecord>>>,
            calls: Mutex<Vec<Vec<String>>>,
        }

        impl FakeExecutor {
            fn new(responses: impl IntoIterator<Item = ExecutionRecord>) -> Self {
                Self {
                    responses: Mutex::new(responses.into_iter().map(Ok).collect()),
                    calls: Mutex::new(Vec::new()),
                }
            }

            fn calls(&self) -> Vec<Vec<String>> {
                self.calls.lock().unwrap().clone()
            }
        }

        impl CommandExecutor for FakeExecutor {
            fn execute(&self, _spec: &CommandSpec) -> io::Result<ExecutionRecord> {
                panic!("activation must use a timed command")
            }
        }

        impl TimedCommandExecutor for FakeExecutor {
            fn execute_with_timeout(
                &self,
                spec: &CommandSpec,
                timeout: Duration,
            ) -> io::Result<ExecutionRecord> {
                assert_eq!(timeout, PFCTL_TIMEOUT);
                self.calls.lock().unwrap().push(spec.displayed_argv());
                self.responses
                    .lock()
                    .unwrap()
                    .pop_front()
                    .expect("unexpected activation command")
            }
        }

        fn success(stdout: &str) -> ExecutionRecord {
            ExecutionRecord {
                argv: Vec::new(),
                environment_keys: Vec::new(),
                status: Some(0),
                success: true,
                stdout: stdout.to_owned(),
                stderr: String::new(),
            }
        }

        fn activate_fixture(
            plan: &DisposableNetworkGateActivationPlan,
            fixture: &Fixture,
            executor: &FakeExecutor,
        ) -> Result<DisposableNetworkGateActivationReceipt, DisposableNetworkGateActivationError>
        {
            activate_at(
                plan,
                executor,
                &fixture.paths(),
                ActivationIdentity {
                    effective_uid: fixture.uid,
                    effective_gid: fixture.gid,
                    expected_uid: fixture.uid,
                    expected_gid: fixture.gid,
                    require_root: false,
                },
            )
        }

        fn anchor_listing() -> ExecutionRecord {
            success("io.smolrunner.disposable-worker\n")
        }

        fn main_rules() -> ExecutionRecord {
            success("anchor \"io.smolrunner.disposable-worker\" all\nanchor \"com.apple/*\" all\n")
        }

        fn enabled_status() -> ExecutionRecord {
            success("Status: Enabled for 0 days 00:00:01\n")
        }

        fn disabled_status() -> ExecutionRecord {
            success("Status: Disabled\n")
        }

        fn already_enabled() -> ExecutionRecord {
            ExecutionRecord {
                argv: Vec::new(),
                environment_keys: Vec::new(),
                status: Some(1),
                success: false,
                stdout: String::new(),
                stderr: "pfctl: pf already enabled\n".to_owned(),
            }
        }

        fn enabled_activation_responses() -> Vec<ExecutionRecord> {
            vec![
                anchor_listing(),
                main_rules(),
                success(""),
                anchor_listing(),
                main_rules(),
                enabled_status(),
                already_enabled(),
                anchor_listing(),
                main_rules(),
                enabled_status(),
            ]
        }

        #[test]
        fn activation_loads_canonical_anchor_before_enable_and_recovers_idempotently() {
            let plan = plan();
            let fixture = Fixture::new(&plan);
            let first = FakeExecutor::new([
                anchor_listing(),
                main_rules(),
                success(""),
                anchor_listing(),
                main_rules(),
                disabled_status(),
                success(""),
                anchor_listing(),
                main_rules(),
                enabled_status(),
            ]);
            let receipt = activate_fixture(&plan, &fixture, &first).unwrap();
            assert_eq!(
                receipt.disposition(),
                DisposableNetworkGateActivationDisposition::Activated
            );
            assert_eq!(
                first.calls(),
                [
                    vec!["/sbin/pfctl", "-s", "Anchors"],
                    vec!["/sbin/pfctl", "-s", "rules"],
                    vec![
                        "/sbin/pfctl",
                        "-a",
                        "io.smolrunner.disposable-worker",
                        "-f",
                        "/private/etc/pf.anchors/io.smolrunner.disposable-worker"
                    ],
                    vec!["/sbin/pfctl", "-s", "Anchors"],
                    vec!["/sbin/pfctl", "-s", "rules"],
                    vec!["/sbin/pfctl", "-s", "info"],
                    vec!["/sbin/pfctl", "-e"],
                    vec!["/sbin/pfctl", "-s", "Anchors"],
                    vec!["/sbin/pfctl", "-s", "rules"],
                    vec!["/sbin/pfctl", "-s", "info"]
                ]
            );
            let public = fixture.gate_directory.join(GATE_RECEIPT_NAME);
            assert_eq!(
                std::fs::metadata(&public).unwrap().permissions().mode() & 0o7777,
                0o444
            );
            assert_eq!(std::fs::read(&public).unwrap(), plan.receipt());

            std::fs::remove_file(&public).unwrap();
            let recovery = FakeExecutor::new(enabled_activation_responses());
            assert_eq!(
                activate_fixture(&plan, &fixture, &recovery)
                    .unwrap()
                    .disposition(),
                DisposableNetworkGateActivationDisposition::Recovered
            );

            let satisfied = FakeExecutor::new(enabled_activation_responses());
            assert_eq!(
                activate_fixture(&plan, &fixture, &satisfied)
                    .unwrap()
                    .disposition(),
                DisposableNetworkGateActivationDisposition::Satisfied
            );
        }

        #[test]
        fn failed_anchor_load_or_ambiguous_enable_publishes_no_receipt() {
            let plan = plan();
            let fixture = Fixture::new(&plan);
            let mut failed = success("");
            failed.success = false;
            failed.status = Some(1);
            let load_failure = FakeExecutor::new([anchor_listing(), main_rules(), failed.clone()]);
            assert_eq!(
                activate_fixture(&plan, &fixture, &load_failure)
                    .unwrap_err()
                    .kind(),
                DisposableNetworkGateActivationErrorKind::CommandFailed
            );
            assert!(!fixture.gate_directory.join(GATE_RECEIPT_NAME).exists());

            let enable_failure = FakeExecutor::new([
                anchor_listing(),
                main_rules(),
                success(""),
                anchor_listing(),
                main_rules(),
                disabled_status(),
                failed,
            ]);
            assert_eq!(
                activate_fixture(&plan, &fixture, &enable_failure)
                    .unwrap_err()
                    .kind(),
                DisposableNetworkGateActivationErrorKind::CommandFailed
            );
            assert!(!fixture.gate_directory.join(GATE_RECEIPT_NAME).exists());

            // If the failed response hid a successful enable, retry observes the global state and
            // recovers without acquiring or leaking a private PF reference.
            let recovered = FakeExecutor::new(enabled_activation_responses());
            assert_eq!(
                activate_fixture(&plan, &fixture, &recovered)
                    .unwrap()
                    .disposition(),
                DisposableNetworkGateActivationDisposition::Recovered
            );
        }

        #[test]
        fn installed_input_drift_revokes_the_prior_receipt_before_refusal() {
            let plan = plan();
            let fixture = Fixture::new(&plan);
            let first = FakeExecutor::new(enabled_activation_responses());
            activate_fixture(&plan, &fixture, &first).unwrap();
            let public = fixture.gate_directory.join(GATE_RECEIPT_NAME);
            assert!(public.exists());

            write_mode(&fixture.anchor, b"block drop out all\n", 0o644);
            let refusal = FakeExecutor::default();
            assert_eq!(
                activate_fixture(&plan, &fixture, &refusal)
                    .unwrap_err()
                    .code(),
                "disposable_network_gate_anchor_mismatch"
            );
            assert!(!public.exists());
            assert!(refusal.calls().is_empty());
        }

        #[test]
        fn attachment_is_unique_and_pf_owns_live_rule_ordering() {
            let plan = plan();
            let duplicate = [plan.main_attachment(), plan.main_attachment()].concat();
            assert_eq!(
                validate_main_attachment(&duplicate, plan.main_attachment())
                    .unwrap_err()
                    .code(),
                "disposable_network_gate_pf_attachment_missing"
            );
            assert!(
                require_first_filter_rule(&success(
                    "anchor \"io.smolrunner.disposable-worker\" all\nanchor \"com.apple/*\" all\n"
                ))
                .is_ok()
            );
            for unsafe_rules in [
                "pass out all\nanchor \"io.smolrunner.disposable-worker\" all\n",
                "anchor \"io.smolrunner.disposable-worker\" all\nanchor \"io.smolrunner.disposable-worker\" all\n",
                "anchor \"com.apple/*\" all\n",
            ] {
                assert_eq!(
                    require_first_filter_rule(&success(unsafe_rules))
                        .unwrap_err()
                        .code(),
                    "disposable_network_gate_live_rule_order_invalid"
                );
            }
        }
    }
}
