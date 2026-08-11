//! Pure fixed macOS PF policy for the first hostile-CI disposable-worker boundary.
//!
//! This module does not inspect or mutate PF, accounts, services, or files. It renders the one
//! closed anchor that a later separately approved root installer must publish and activate. macOS
//! PF owns packet filtering; SmolRunner owns only this bounded policy identity.

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::disposable_prepared_template::DisposablePreparedTemplateManifest;

pub const DISPOSABLE_NETWORK_POLICY_SCHEMA_VERSION: u8 = 1;
pub const DISPOSABLE_NETWORK_PF_ANCHOR: &str = "io.smolrunner/disposable-worker";
const POLICY_DOMAIN: &[u8] = b"smolrunner.disposable-network-policy.macos-pf-uid.v1\0";
const MAX_SERVICE_UID: u32 = i32::MAX as u32;

const DENIED_IPV4: &str = "{ 0.0.0.0/8, 10.0.0.0/8, 100.64.0.0/10, 127.0.0.0/8, \
169.254.0.0/16, 172.16.0.0/12, 192.0.0.0/24, 192.0.2.0/24, 192.88.99.0/24, \
192.168.0.0/16, 198.18.0.0/15, 198.51.100.0/24, 203.0.113.0/24, 224.0.0.0/4, \
240.0.0.0/4 }";
const DENIED_IPV6: &str = "{ ::/128, ::1/128, ::ffff:0:0/96, 64:ff9b:1::/48, 100::/64, \
2001:2::/48, 2001:10::/28, 2001:db8::/32, fc00::/7, fe80::/10, ff00::/8 }";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableNetworkPolicyBackend {
    MacosPfDedicatedUid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DisposableNetworkPolicyReport {
    schema_version: u8,
    backend: DisposableNetworkPolicyBackend,
    anchor: &'static str,
    service_uid: u32,
    lima_control_port: u16,
    policy_identity: Sha256Digest,
    default_outbound: &'static str,
    peer_isolation: &'static str,
    enforcement_state: &'static str,
}

impl DisposableNetworkPolicyReport {
    #[must_use]
    pub const fn service_uid(&self) -> u32 {
        self.service_uid
    }

    #[must_use]
    pub const fn lima_control_port(&self) -> u16 {
        self.lima_control_port
    }

    #[must_use]
    pub const fn policy_identity(&self) -> &Sha256Digest {
        &self.policy_identity
    }
}

/// Exact immutable PF anchor input for one dedicated service identity.
///
/// The type has no filesystem or process authority. `anchor_bytes` are private so callers cannot
/// mistake a rendered plan for observed enforcement.
#[derive(Clone, PartialEq, Eq)]
pub struct DisposableNetworkPolicyPlan {
    report: DisposableNetworkPolicyReport,
    anchor_bytes: Vec<u8>,
}

impl fmt::Debug for DisposableNetworkPolicyPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableNetworkPolicyPlan")
            .field("report", &self.report)
            .finish()
    }
}

impl DisposableNetworkPolicyPlan {
    #[must_use]
    pub const fn report(&self) -> &DisposableNetworkPolicyReport {
        &self.report
    }

    /// Return the exact immutable anchor input for an explicitly approved privileged installer.
    #[must_use]
    pub fn anchor_bytes(&self) -> &[u8] {
        &self.anchor_bytes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableNetworkPolicyErrorKind {
    InvalidServiceIdentity,
    InvalidPreparedTemplate,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DisposableNetworkPolicyError {
    kind: DisposableNetworkPolicyErrorKind,
    code: &'static str,
    message: &'static str,
}

impl DisposableNetworkPolicyError {
    #[must_use]
    pub const fn kind(self) -> DisposableNetworkPolicyErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for DisposableNetworkPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableNetworkPolicyError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .finish()
    }
}

impl fmt::Display for DisposableNetworkPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for DisposableNetworkPolicyError {}

/// Build the one fixed macOS PF anchor for a dedicated unprivileged service UID.
///
/// This is planning only. The resulting report explicitly remains `not_observed`; it cannot admit
/// a worker until the future privileged installation and live startup gate validate enforcement.
///
/// # Errors
///
/// Returns a bounded refusal for root/out-of-range identities or a prepared template that does not
/// carry the reviewed fixed Lima control port.
pub fn plan_disposable_network_policy(
    service_uid: u32,
    prepared_template: &DisposablePreparedTemplateManifest,
) -> Result<DisposableNetworkPolicyPlan, DisposableNetworkPolicyError> {
    if service_uid == 0 || service_uid > MAX_SERVICE_UID {
        return Err(policy_error(
            DisposableNetworkPolicyErrorKind::InvalidServiceIdentity,
            "disposable_network_service_identity_invalid",
            "the disposable-network service identity is invalid",
        ));
    }
    let lima_control_port = prepared_template.ssh_local_port();
    if lima_control_port != 61_922 {
        return Err(policy_error(
            DisposableNetworkPolicyErrorKind::InvalidPreparedTemplate,
            "disposable_network_prepared_template_invalid",
            "the prepared template does not carry the reviewed network control input",
        ));
    }

    let anchor_bytes = render_anchor(service_uid, lima_control_port);
    let policy_identity = digest_policy(&anchor_bytes);
    Ok(DisposableNetworkPolicyPlan {
        report: DisposableNetworkPolicyReport {
            schema_version: DISPOSABLE_NETWORK_POLICY_SCHEMA_VERSION,
            backend: DisposableNetworkPolicyBackend::MacosPfDedicatedUid,
            anchor: DISPOSABLE_NETWORK_PF_ANCHOR,
            service_uid,
            lima_control_port,
            policy_identity,
            default_outbound: "public_tcp_udp_only",
            peer_isolation: "single_worker_only",
            enforcement_state: "not_observed",
        },
        anchor_bytes,
    })
}

fn render_anchor(service_uid: u32, lima_control_port: u16) -> Vec<u8> {
    format!(
        concat!(
            "# Generated by SmolRunner; load only in anchor io.smolrunner/disposable-worker.\n",
            "pass out quick inet proto tcp from any to 127.0.0.1 port = {lima_control_port} user = {service_uid} keep state\n",
            "block return out quick inet proto {{ tcp udp }} from any to {denied_ipv4} user = {service_uid}\n",
            "block return out quick inet6 proto {{ tcp udp }} from any to {denied_ipv6} user = {service_uid}\n",
            "pass out quick inet proto {{ tcp udp }} from any to any user = {service_uid} keep state\n",
            "pass out quick inet6 proto {{ tcp udp }} from any to any user = {service_uid} keep state\n"
        ),
        lima_control_port = lima_control_port,
        service_uid = service_uid,
        denied_ipv4 = DENIED_IPV4,
        denied_ipv6 = DENIED_IPV6,
    )
    .into_bytes()
}

fn digest_policy(bytes: &[u8]) -> Sha256Digest {
    let mut hasher = Sha256::new();
    hasher.update(POLICY_DOMAIN);
    hasher.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(bytes);
    Sha256Digest::parse(&format!("sha256:{:x}", hasher.finalize()))
        .expect("SHA-256 formatting is canonical")
}

const fn policy_error(
    kind: DisposableNetworkPolicyErrorKind,
    code: &'static str,
    message: &'static str,
) -> DisposableNetworkPolicyError {
    DisposableNetworkPolicyError {
        kind,
        code,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disposable_prepared_template::current_disposable_prepared_template;

    #[test]
    fn fixed_policy_binds_uid_port_denies_and_public_outbound() {
        let template = current_disposable_prepared_template().unwrap();
        let plan = plan_disposable_network_policy(502, &template).unwrap();
        let policy = std::str::from_utf8(plan.anchor_bytes()).unwrap();

        assert_eq!(plan.report().service_uid(), 502);
        assert_eq!(plan.report().lima_control_port(), 61_922);
        assert!(policy.contains("to 127.0.0.1 port = 61922 user = 502"));
        for denied in [
            "10.0.0.0/8",
            "100.64.0.0/10",
            "127.0.0.0/8",
            "169.254.0.0/16",
            "172.16.0.0/12",
            "192.168.0.0/16",
            "198.18.0.0/15",
            "224.0.0.0/4",
            "fc00::/7",
            "fe80::/10",
            "ff00::/8",
        ] {
            assert!(policy.contains(denied), "missing {denied}");
        }
        assert!(policy.contains("pass out quick inet proto { tcp udp } from any to any"));
        assert!(policy.contains("pass out quick inet6 proto { tcp udp } from any to any"));
        assert_eq!(
            plan.report().policy_identity().as_str(),
            "sha256:a6eb142b9051c724543c342f9631c79b2f829bb34709dcee84191be237b7fa9b"
        );
    }

    #[test]
    fn root_and_out_of_range_service_identities_are_refused() {
        let template = current_disposable_prepared_template().unwrap();
        for service_uid in [0, u32::MAX] {
            let error = plan_disposable_network_policy(service_uid, &template).unwrap_err();
            assert_eq!(
                error.kind(),
                DisposableNetworkPolicyErrorKind::InvalidServiceIdentity
            );
            assert_eq!(error.code(), "disposable_network_service_identity_invalid");
            assert!(!format!("{error:?}").contains("502"));
        }
    }

    #[test]
    fn service_identity_changes_policy_identity() {
        let template = current_disposable_prepared_template().unwrap();
        let first = plan_disposable_network_policy(502, &template).unwrap();
        let second = plan_disposable_network_policy(503, &template).unwrap();
        assert_ne!(
            first.report().policy_identity(),
            second.report().policy_identity()
        );
    }
}
