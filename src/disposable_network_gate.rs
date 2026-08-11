//! Boot-volatile root receipt for the macOS hostile-CI network gate.
//!
//! A later root-owned one-shot LaunchDaemon publishes this receipt only after enabling PF and
//! loading the exact anchor. The unprivileged worker treats absence or any mismatch as an admission
//! hold. Receipt bytes alone are not authority: production observation additionally requires the
//! fixed `/private/var/run` location and root-owned filesystem policy.

use std::fmt;

#[cfg(target_os = "macos")]
use std::fs::{File, Metadata};
#[cfg(target_os = "macos")]
use std::io::{Read, Seek, SeekFrom};
#[cfg(target_os = "macos")]
use std::os::unix::fs::MetadataExt;

use serde::{Deserialize, Serialize};

use crate::disposable_network_policy::{
    DISPOSABLE_NETWORK_PF_ANCHOR, DisposableNetworkPolicyBackend, DisposableNetworkPolicyPlan,
};

#[cfg(target_os = "macos")]
use rustix::fs::{self as rustix_fs, Mode, OFlags};

pub const DISPOSABLE_NETWORK_GATE_SCHEMA_VERSION: u8 = 1;
pub const DISPOSABLE_NETWORK_GATE_RECEIPT_PATH: &str =
    "/private/var/run/smolrunner/network-gate-v1.json";
pub const MAX_DISPOSABLE_NETWORK_GATE_RECEIPT_BYTES: usize = 4 * 1024;

#[cfg(target_os = "macos")]
const GATE_DIRECTORY: &str = "/private/var/run/smolrunner";
#[cfg(target_os = "macos")]
const GATE_RECEIPT_NAME: &str = "network-gate-v1.json";

#[cfg(target_os = "macos")]
const ROOT_UID: u32 = 0;
#[cfg(target_os = "macos")]
const ROOT_GID: u32 = 0;

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableNetworkGateObservationErrorKind {
    Missing,
    UnsafeFilesystem,
    Changed,
    InvalidReceipt,
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DisposableNetworkGateObservationError {
    kind: DisposableNetworkGateObservationErrorKind,
    code: &'static str,
}

#[cfg(target_os = "macos")]
impl DisposableNetworkGateObservationError {
    #[must_use]
    pub const fn kind(self) -> DisposableNetworkGateObservationErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

#[cfg(target_os = "macos")]
impl fmt::Debug for DisposableNetworkGateObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableNetworkGateObservationError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .finish()
    }
}

#[cfg(target_os = "macos")]
impl fmt::Display for DisposableNetworkGateObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the disposable-network gate is unavailable")
    }
}

#[cfg(target_os = "macos")]
impl std::error::Error for DisposableNetworkGateObservationError {}

/// Root-protected, boot-volatile observation of the exact enrolled network gate.
///
/// The type has no constructor, `Clone`, serialization, path, or raw-receipt accessor.
#[cfg(target_os = "macos")]
pub struct ObservedDisposableNetworkGate {
    receipt: DisposableNetworkGateReceipt,
}

#[cfg(target_os = "macos")]
impl fmt::Debug for ObservedDisposableNetworkGate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ObservedDisposableNetworkGate")
            .field("service_uid", &self.receipt.service_uid())
            .field("enforcement", &"observed_root_boot_receipt")
            .finish()
    }
}

/// Observe the fixed root-owned boot receipt for one exact policy.
///
/// The call first requires the process effective UID to equal the dedicated policy UID. It then
/// validates the fixed root hierarchy and boot-volatile gate directory before reading one
/// no-follow, nonblocking, root-owned, single-link, mode-0444 regular file twice. Root remains the
/// trusted administrator; this boundary prevents the unprivileged service or a guest from minting
/// its own admission signal.
///
/// # Errors
///
/// Returns a bounded, path-free error for absence, unsafe metadata, drift, or receipt mismatch.
#[cfg(target_os = "macos")]
pub fn observe_disposable_network_gate(
    expected: &DisposableNetworkPolicyPlan,
) -> Result<ObservedDisposableNetworkGate, DisposableNetworkGateObservationError> {
    if rustix::process::geteuid().as_raw() != expected.report().service_uid() {
        return Err(observation_error(
            DisposableNetworkGateObservationErrorKind::UnsafeFilesystem,
            "disposable_network_gate_service_identity_mismatch",
        ));
    }
    for path in ["/", "/private", "/private/var"] {
        verify_directory(path, ROOT_UID, ROOT_GID, false)?;
    }
    verify_directory("/private/var/run", ROOT_UID, 1, true)?;
    observe_gate_at(GATE_DIRECTORY, ROOT_UID, ROOT_GID, expected)
}

#[cfg(target_os = "macos")]
fn observe_gate_at(
    directory: &str,
    owner: u32,
    group: u32,
    expected: &DisposableNetworkPolicyPlan,
) -> Result<ObservedDisposableNetworkGate, DisposableNetworkGateObservationError> {
    verify_directory(directory, owner, group, false)?;
    let path = std::path::Path::new(directory).join(GATE_RECEIPT_NAME);
    let path_before = std::fs::symlink_metadata(&path).map_err(|error| {
        observation_error(
            if error.kind() == std::io::ErrorKind::NotFound {
                DisposableNetworkGateObservationErrorKind::Missing
            } else {
                DisposableNetworkGateObservationErrorKind::UnsafeFilesystem
            },
            "disposable_network_gate_receipt_unavailable",
        )
    })?;
    verify_receipt_metadata(&path_before, owner, group)?;
    let fd = rustix_fs::open(
        &path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| {
        observation_error(
            DisposableNetworkGateObservationErrorKind::UnsafeFilesystem,
            "disposable_network_gate_receipt_unavailable",
        )
    })?;
    let mut file = File::from(fd);
    let held_before = file.metadata().map_err(|_| {
        observation_error(
            DisposableNetworkGateObservationErrorKind::Changed,
            "disposable_network_gate_receipt_changed",
        )
    })?;
    verify_receipt_metadata(&held_before, owner, group)?;
    require_same_file(&path_before, &held_before)?;
    let first = read_bounded(&mut file)?;
    file.seek(SeekFrom::Start(0)).map_err(|_| {
        observation_error(
            DisposableNetworkGateObservationErrorKind::Changed,
            "disposable_network_gate_receipt_changed",
        )
    })?;
    let second = read_bounded(&mut file)?;
    if first != second {
        return Err(observation_error(
            DisposableNetworkGateObservationErrorKind::Changed,
            "disposable_network_gate_receipt_changed",
        ));
    }
    let held_after = file.metadata().map_err(|_| {
        observation_error(
            DisposableNetworkGateObservationErrorKind::Changed,
            "disposable_network_gate_receipt_changed",
        )
    })?;
    let path_after = std::fs::symlink_metadata(&path).map_err(|_| {
        observation_error(
            DisposableNetworkGateObservationErrorKind::Changed,
            "disposable_network_gate_receipt_changed",
        )
    })?;
    verify_receipt_metadata(&held_after, owner, group)?;
    verify_receipt_metadata(&path_after, owner, group)?;
    require_same_file(&path_before, &held_after)?;
    require_same_file(&path_before, &path_after)?;
    verify_directory(directory, owner, group, false)?;
    let receipt = decode_disposable_network_gate_receipt(&first, expected).map_err(|_| {
        observation_error(
            DisposableNetworkGateObservationErrorKind::InvalidReceipt,
            "disposable_network_gate_receipt_invalid",
        )
    })?;
    Ok(ObservedDisposableNetworkGate { receipt })
}

#[cfg(target_os = "macos")]
fn verify_directory(
    path: &str,
    owner: u32,
    group: u32,
    allow_group_write: bool,
) -> Result<(), DisposableNetworkGateObservationError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        observation_error(
            if error.kind() == std::io::ErrorKind::NotFound {
                DisposableNetworkGateObservationErrorKind::Missing
            } else {
                DisposableNetworkGateObservationErrorKind::UnsafeFilesystem
            },
            "disposable_network_gate_directory_unavailable",
        )
    })?;
    let forbidden_write = if allow_group_write { 0o002 } else { 0o022 };
    if !metadata.file_type().is_dir()
        || metadata.uid() != owner
        || metadata.gid() != group
        || metadata.mode() & forbidden_write != 0
    {
        return Err(observation_error(
            DisposableNetworkGateObservationErrorKind::UnsafeFilesystem,
            "disposable_network_gate_directory_unsafe",
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn verify_receipt_metadata(
    metadata: &Metadata,
    owner: u32,
    group: u32,
) -> Result<(), DisposableNetworkGateObservationError> {
    if !metadata.file_type().is_file()
        || metadata.uid() != owner
        || metadata.gid() != group
        || metadata.mode() & 0o7777 != 0o444
        || metadata.nlink() != 1
        || metadata.len() == 0
        || metadata.len() > MAX_DISPOSABLE_NETWORK_GATE_RECEIPT_BYTES as u64
    {
        return Err(observation_error(
            DisposableNetworkGateObservationErrorKind::UnsafeFilesystem,
            "disposable_network_gate_receipt_unsafe",
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn require_same_file(
    expected: &Metadata,
    actual: &Metadata,
) -> Result<(), DisposableNetworkGateObservationError> {
    if expected.dev() != actual.dev()
        || expected.ino() != actual.ino()
        || expected.uid() != actual.uid()
        || expected.gid() != actual.gid()
        || expected.mode() != actual.mode()
        || expected.nlink() != actual.nlink()
        || expected.len() != actual.len()
        || expected.mtime() != actual.mtime()
        || expected.mtime_nsec() != actual.mtime_nsec()
        || expected.ctime() != actual.ctime()
        || expected.ctime_nsec() != actual.ctime_nsec()
    {
        return Err(observation_error(
            DisposableNetworkGateObservationErrorKind::Changed,
            "disposable_network_gate_receipt_changed",
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn read_bounded(file: &mut File) -> Result<Vec<u8>, DisposableNetworkGateObservationError> {
    let mut bytes = Vec::with_capacity(MAX_DISPOSABLE_NETWORK_GATE_RECEIPT_BYTES);
    file.take((MAX_DISPOSABLE_NETWORK_GATE_RECEIPT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            observation_error(
                DisposableNetworkGateObservationErrorKind::Changed,
                "disposable_network_gate_receipt_changed",
            )
        })?;
    if bytes.len() > MAX_DISPOSABLE_NETWORK_GATE_RECEIPT_BYTES {
        return Err(observation_error(
            DisposableNetworkGateObservationErrorKind::UnsafeFilesystem,
            "disposable_network_gate_receipt_unsafe",
        ));
    }
    Ok(bytes)
}

#[cfg(target_os = "macos")]
const fn observation_error(
    kind: DisposableNetworkGateObservationErrorKind,
    code: &'static str,
) -> DisposableNetworkGateObservationError {
    DisposableNetworkGateObservationError { kind, code }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableNetworkGateErrorKind {
    VersionIncompatible,
    InvalidReceipt,
    NonCanonical,
    PolicyMismatch,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DisposableNetworkGateError {
    kind: DisposableNetworkGateErrorKind,
    code: &'static str,
    message: &'static str,
}

impl DisposableNetworkGateError {
    #[must_use]
    pub const fn kind(self) -> DisposableNetworkGateErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for DisposableNetworkGateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableNetworkGateError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .finish()
    }
}

impl fmt::Display for DisposableNetworkGateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for DisposableNetworkGateError {}

/// Validated receipt content. It deliberately has no constructor, `Clone`, serialization, path,
/// or raw policy-byte accessor.
pub struct DisposableNetworkGateReceipt {
    wire: NetworkGateWire,
}

impl fmt::Debug for DisposableNetworkGateReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableNetworkGateReceipt")
            .field("schema_version", &self.wire.schema_version)
            .field("backend", &self.wire.backend)
            .field("service_uid", &self.wire.service_uid)
            .field("enforcement", &"recorded_not_observed")
            .finish()
    }
}

impl DisposableNetworkGateReceipt {
    #[must_use]
    pub const fn service_uid(&self) -> u32 {
        self.wire.service_uid
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct NetworkGateWire {
    schema_version: u8,
    backend: DisposableNetworkPolicyBackend,
    anchor: String,
    service_uid: u32,
    lima_control_port: u16,
    policy_identity: String,
    enforcement: String,
}

#[derive(Deserialize)]
struct VersionWire {
    schema_version: u8,
}

/// Encode the canonical receipt bytes that the future root gate publishes after enforcement.
///
/// This function does not write a file or claim that PF is active.
///
/// # Errors
///
/// Returns a bounded error only if the fixed receipt cannot be canonically encoded.
pub fn encode_disposable_network_gate_receipt(
    plan: &DisposableNetworkPolicyPlan,
) -> Result<Vec<u8>, DisposableNetworkGateError> {
    canonical_bytes(&wire_for(plan))
}

/// Decode canonical receipt bytes and require their exact policy binding.
///
/// This validates content only. Production admission must additionally use the root-owned,
/// boot-volatile filesystem observer; decoding caller bytes is never enforcement evidence.
///
/// # Errors
///
/// Returns a bounded refusal for oversize, malformed, noncanonical, unsupported, or mismatched
/// content.
pub fn decode_disposable_network_gate_receipt(
    bytes: &[u8],
    expected: &DisposableNetworkPolicyPlan,
) -> Result<DisposableNetworkGateReceipt, DisposableNetworkGateError> {
    if bytes.len() > MAX_DISPOSABLE_NETWORK_GATE_RECEIPT_BYTES {
        return Err(gate_error(
            DisposableNetworkGateErrorKind::InvalidReceipt,
            "disposable_network_gate_receipt_too_large",
            "the disposable-network gate receipt exceeds the reviewed byte limit",
        ));
    }
    let version: VersionWire = serde_json::from_slice(bytes).map_err(|_| invalid_receipt())?;
    if version.schema_version != DISPOSABLE_NETWORK_GATE_SCHEMA_VERSION {
        return Err(gate_error(
            DisposableNetworkGateErrorKind::VersionIncompatible,
            "disposable_network_gate_version_incompatible",
            "the disposable-network gate receipt version is unsupported",
        ));
    }
    let wire: NetworkGateWire = serde_json::from_slice(bytes).map_err(|_| invalid_receipt())?;
    if canonical_bytes(&wire)? != bytes {
        return Err(gate_error(
            DisposableNetworkGateErrorKind::NonCanonical,
            "disposable_network_gate_noncanonical",
            "the disposable-network gate receipt is not canonical",
        ));
    }
    if wire != wire_for(expected) {
        return Err(gate_error(
            DisposableNetworkGateErrorKind::PolicyMismatch,
            "disposable_network_gate_policy_mismatch",
            "the disposable-network gate receipt does not match the enrolled policy",
        ));
    }
    Ok(DisposableNetworkGateReceipt { wire })
}

fn wire_for(plan: &DisposableNetworkPolicyPlan) -> NetworkGateWire {
    NetworkGateWire {
        schema_version: DISPOSABLE_NETWORK_GATE_SCHEMA_VERSION,
        backend: DisposableNetworkPolicyBackend::MacosPfDedicatedUid,
        anchor: DISPOSABLE_NETWORK_PF_ANCHOR.to_owned(),
        service_uid: plan.report().service_uid(),
        lima_control_port: plan.report().lima_control_port(),
        policy_identity: plan.report().policy_identity().as_str().to_owned(),
        enforcement: "pf_enabled_anchor_loaded_this_boot".to_owned(),
    }
}

fn canonical_bytes(wire: &NetworkGateWire) -> Result<Vec<u8>, DisposableNetworkGateError> {
    let mut bytes = serde_json::to_vec_pretty(wire).map_err(|_| invalid_receipt())?;
    bytes.push(b'\n');
    Ok(bytes)
}

const fn invalid_receipt() -> DisposableNetworkGateError {
    gate_error(
        DisposableNetworkGateErrorKind::InvalidReceipt,
        "disposable_network_gate_receipt_invalid",
        "the disposable-network gate receipt is invalid",
    )
}

const fn gate_error(
    kind: DisposableNetworkGateErrorKind,
    code: &'static str,
    message: &'static str,
) -> DisposableNetworkGateError {
    DisposableNetworkGateError {
        kind,
        code,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disposable_network_policy::plan_disposable_network_policy;
    use crate::disposable_prepared_template::current_disposable_prepared_template;

    #[cfg(target_os = "macos")]
    use std::os::unix::fs::{PermissionsExt, symlink};
    #[cfg(target_os = "macos")]
    use std::sync::atomic::{AtomicU64, Ordering};

    fn plan(service_uid: u32) -> DisposableNetworkPolicyPlan {
        plan_disposable_network_policy(
            service_uid,
            &current_disposable_prepared_template().unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn canonical_receipt_is_exactly_policy_bound_and_not_observed() {
        let expected = plan(502);
        #[cfg(target_os = "macos")]
        assert_eq!(
            std::path::Path::new(GATE_DIRECTORY).join(GATE_RECEIPT_NAME),
            std::path::Path::new(DISPOSABLE_NETWORK_GATE_RECEIPT_PATH)
        );
        let bytes = encode_disposable_network_gate_receipt(&expected).unwrap();
        let receipt = decode_disposable_network_gate_receipt(&bytes, &expected).unwrap();
        assert_eq!(receipt.service_uid(), 502);
        assert!(format!("{receipt:?}").contains("recorded_not_observed"));
        assert!(!format!("{receipt:?}").contains("a6eb142b"));

        let document: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(document["anchor"], DISPOSABLE_NETWORK_PF_ANCHOR);
        assert_eq!(document["lima_control_port"], 61_922);
        assert_eq!(
            document["enforcement"],
            "pf_enabled_anchor_loaded_this_boot"
        );
    }

    #[test]
    fn another_uid_policy_unknown_fields_and_noncanonical_bytes_are_refused() {
        let expected = plan(502);
        let other = encode_disposable_network_gate_receipt(&plan(503)).unwrap();
        assert_eq!(
            decode_disposable_network_gate_receipt(&other, &expected)
                .unwrap_err()
                .kind(),
            DisposableNetworkGateErrorKind::PolicyMismatch
        );

        let canonical = encode_disposable_network_gate_receipt(&expected).unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&canonical).unwrap();
        value["unexpected"] = serde_json::json!(true);
        assert_eq!(
            decode_disposable_network_gate_receipt(
                &serde_json::to_vec_pretty(&value).unwrap(),
                &expected,
            )
            .unwrap_err()
            .kind(),
            DisposableNetworkGateErrorKind::InvalidReceipt
        );
        assert_eq!(
            decode_disposable_network_gate_receipt(
                &serde_json::to_vec(
                    &serde_json::from_slice::<serde_json::Value>(&canonical).unwrap()
                )
                .unwrap(),
                &expected,
            )
            .unwrap_err()
            .kind(),
            DisposableNetworkGateErrorKind::NonCanonical
        );
    }

    #[test]
    fn version_precedes_current_fields_and_size_precedes_parsing() {
        let expected = plan(502);
        for version in [0, 2] {
            let bytes = format!("{{\"schema_version\":{version}}}");
            assert_eq!(
                decode_disposable_network_gate_receipt(bytes.as_bytes(), &expected)
                    .unwrap_err()
                    .kind(),
                DisposableNetworkGateErrorKind::VersionIncompatible
            );
        }
        let bytes = vec![b' '; MAX_DISPOSABLE_NETWORK_GATE_RECEIPT_BYTES + 1];
        assert_eq!(
            decode_disposable_network_gate_receipt(&bytes, &expected)
                .unwrap_err()
                .code(),
            "disposable_network_gate_receipt_too_large"
        );
    }

    #[cfg(target_os = "macos")]
    struct GateFixture {
        directory: std::path::PathBuf,
        plan: DisposableNetworkPolicyPlan,
    }

    #[cfg(target_os = "macos")]
    impl GateFixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(1);
            let directory = std::env::temp_dir().join(format!(
                "smolrunner-network-gate-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&directory).unwrap();
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
            let plan = plan(rustix::process::geteuid().as_raw());
            let receipt = encode_disposable_network_gate_receipt(&plan).unwrap();
            let path = directory.join("network-gate-v1.json");
            std::fs::write(&path, receipt).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
            Self { directory, plan }
        }

        fn observe(
            &self,
        ) -> Result<ObservedDisposableNetworkGate, DisposableNetworkGateObservationError> {
            observe_gate_at(
                self.directory.to_str().unwrap(),
                rustix::process::geteuid().as_raw(),
                rustix::process::getegid().as_raw(),
                &self.plan,
            )
        }
    }

    #[cfg(target_os = "macos")]
    impl Drop for GateFixture {
        fn drop(&mut self) {
            if let Ok(metadata) =
                std::fs::symlink_metadata(self.directory.join("network-gate-v1.json"))
                && metadata.file_type().is_file()
            {
                let _ = std::fs::set_permissions(
                    self.directory.join("network-gate-v1.json"),
                    std::fs::Permissions::from_mode(0o600),
                );
            }
            let _ = std::fs::remove_file(self.directory.join("network-gate-v1.json"));
            let _ = std::fs::remove_dir(&self.directory);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn protected_boot_receipt_observes_and_unsafe_entries_refuse() {
        let fixture = GateFixture::new();
        let observed = fixture.observe().unwrap();
        assert!(format!("{observed:?}").contains("observed_root_boot_receipt"));

        let receipt = fixture.directory.join("network-gate-v1.json");
        std::fs::set_permissions(&receipt, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            fixture.observe().unwrap_err().kind(),
            DisposableNetworkGateObservationErrorKind::UnsafeFilesystem
        );
        std::fs::remove_file(&receipt).unwrap();
        symlink("outside", &receipt).unwrap();
        assert_eq!(
            fixture.observe().unwrap_err().kind(),
            DisposableNetworkGateObservationErrorKind::UnsafeFilesystem
        );
    }
}
