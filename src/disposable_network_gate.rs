//! Boot-volatile root receipt for the macOS hostile-CI network gate.
//!
//! A later root-owned one-shot LaunchDaemon publishes this receipt only after enabling PF and
//! loading the exact anchor. The unprivileged worker treats absence or any mismatch as an admission
//! hold. Receipt bytes alone are not authority: production observation additionally requires the
//! fixed `/private/var/run` location and root-owned filesystem policy.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::disposable_network_policy::{
    DISPOSABLE_NETWORK_PF_ANCHOR, DisposableNetworkPolicyBackend, DisposableNetworkPolicyPlan,
};

pub const DISPOSABLE_NETWORK_GATE_SCHEMA_VERSION: u8 = 1;
pub const DISPOSABLE_NETWORK_GATE_RECEIPT_PATH: &str =
    "/private/var/run/smolrunner/network-gate-v1.json";
pub const MAX_DISPOSABLE_NETWORK_GATE_RECEIPT_BYTES: usize = 4 * 1024;

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
}
