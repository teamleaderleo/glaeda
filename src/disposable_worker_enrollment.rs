//! Canonical, secret-free operator enrollment for one disposable Scale Set worker.
//!
//! The document names the Mac-local control-plane inputs, but never contains the GitHub App
//! private key or a runner JIT value. Construction delegates policy validation to the existing
//! bridge, prepared-template, resource, Lima, and consumer boundaries instead of duplicating
//! those semantics here.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::artifact::Sha256Digest;
use crate::disposable_clone_runtime::DisposableCloneRuntime;
use crate::disposable_network_policy::{
    DisposableNetworkPolicyBackend, DisposableNetworkPolicyPlan, plan_disposable_network_policy,
};
use crate::disposable_prepared_template::current_disposable_prepared_template;
use crate::disposable_runner_runtime::DisposableRunnerRuntime;
use crate::disposable_template_runtime::DisposableTemplateRuntime;
use crate::disposable_worker_reconciler::DisposableWorkerResources;
use crate::github_scale_set_bridge::{
    GitHubAppKeychainConfig, ScaleSetBridgeConfig, ScaleSetBridgeTarget,
};
use crate::github_scale_set_consumer::ScaleSetConsumerPolicy;
use crate::lima_observation::LimaInstanceName;

pub const DISPOSABLE_WORKER_ENROLLMENT_SCHEMA_VERSION: u8 = 2;
pub const MAX_DISPOSABLE_WORKER_ENROLLMENT_BYTES: usize = 16 * 1024;
const BRIDGE_PROGRAM: &str = "/opt/smolrunner/bin/scaleset-bridge";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableWorkerEnrollmentErrorKind {
    InvalidDocument,
    VersionIncompatible,
    NonCanonical,
    InvalidConfiguration,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DisposableWorkerEnrollmentError {
    kind: DisposableWorkerEnrollmentErrorKind,
    code: &'static str,
    message: &'static str,
}

impl DisposableWorkerEnrollmentError {
    #[must_use]
    pub const fn kind(self) -> DisposableWorkerEnrollmentErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for DisposableWorkerEnrollmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableWorkerEnrollmentError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .finish()
    }
}

impl fmt::Display for DisposableWorkerEnrollmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for DisposableWorkerEnrollmentError {}

/// Validated construction inputs for one local, single-concurrency Scale Set service.
///
/// The type deliberately has no serialization, path accessor, `Clone`, or derived `Debug`.
pub struct DisposableWorkerEnrollment {
    state_root: PathBuf,
    network_policy: DisposableNetworkPolicyPlan,
    bridge_config: ScaleSetBridgeConfig,
    consumer_policy: ScaleSetConsumerPolicy,
    template_runtime: DisposableTemplateRuntime,
    clone_runtime: DisposableCloneRuntime,
    runner_runtime: DisposableRunnerRuntime,
}

impl fmt::Debug for DisposableWorkerEnrollment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableWorkerEnrollment")
            .field("state_root", &"<private-state-root>")
            .field("bridge", &"<enrolled-scale-set>")
            .field("network", &"<planned-not-observed>")
            .field("policy", &self.consumer_policy)
            .finish()
    }
}

pub(crate) struct DisposableWorkerEnrollmentParts {
    pub(crate) state_root: PathBuf,
    pub(crate) network_policy: DisposableNetworkPolicyPlan,
    pub(crate) bridge_config: ScaleSetBridgeConfig,
    pub(crate) consumer_policy: ScaleSetConsumerPolicy,
    pub(crate) template_runtime: DisposableTemplateRuntime,
    pub(crate) clone_runtime: DisposableCloneRuntime,
    pub(crate) runner_runtime: DisposableRunnerRuntime,
}

impl DisposableWorkerEnrollment {
    pub(crate) fn into_parts(self) -> DisposableWorkerEnrollmentParts {
        DisposableWorkerEnrollmentParts {
            state_root: self.state_root,
            network_policy: self.network_policy,
            bridge_config: self.bridge_config,
            consumer_policy: self.consumer_policy,
            template_runtime: self.template_runtime,
            clone_runtime: self.clone_runtime,
            runner_runtime: self.runner_runtime,
        }
    }
}

/// Strictly decode one canonical, secret-free disposable-worker enrollment document.
///
/// # Errors
///
/// Returns a bounded, path-free error for oversize, malformed, unsupported, noncanonical, or
/// policy-invalid input.
pub fn decode_disposable_worker_enrollment(
    bytes: &[u8],
) -> Result<DisposableWorkerEnrollment, DisposableWorkerEnrollmentError> {
    if bytes.len() > MAX_DISPOSABLE_WORKER_ENROLLMENT_BYTES {
        return Err(enrollment_error(
            DisposableWorkerEnrollmentErrorKind::InvalidDocument,
            "disposable_worker_enrollment_too_large",
            "the disposable-worker enrollment exceeds the reviewed byte limit",
        ));
    }
    let version: VersionWire = serde_json::from_slice(bytes).map_err(|_| invalid_document())?;
    if version.schema_version != DISPOSABLE_WORKER_ENROLLMENT_SCHEMA_VERSION {
        return Err(enrollment_error(
            DisposableWorkerEnrollmentErrorKind::VersionIncompatible,
            "disposable_worker_enrollment_version_incompatible",
            "the disposable-worker enrollment schema version is unsupported",
        ));
    }
    let wire: EnrollmentWire = serde_json::from_slice(bytes).map_err(|_| invalid_document())?;
    if canonical_bytes(&wire)? != bytes {
        return Err(enrollment_error(
            DisposableWorkerEnrollmentErrorKind::NonCanonical,
            "disposable_worker_enrollment_noncanonical",
            "the disposable-worker enrollment is not canonically encoded",
        ));
    }
    build_enrollment(wire)
}

fn build_enrollment(
    wire: EnrollmentWire,
) -> Result<DisposableWorkerEnrollment, DisposableWorkerEnrollmentError> {
    if wire
        .scale_set
        .labels
        .windows(2)
        .any(|pair| pair[0] >= pair[1])
    {
        return Err(invalid_configuration());
    }
    let state_root = PathBuf::from(&wire.state_root);
    let limactl_program = PathBuf::from(&wire.lima.program);
    let lima_home = PathBuf::from(&wire.lima.home);
    let source_instance =
        LimaInstanceName::parse(&wire.lima.source_instance).map_err(|_| invalid_configuration())?;
    let bridge_digest =
        Sha256Digest::parse(&wire.bridge.program_digest).map_err(|_| invalid_configuration())?;
    let expected_network_identity =
        Sha256Digest::parse(&wire.network.policy_identity).map_err(|_| invalid_configuration())?;
    let github_app = GitHubAppKeychainConfig::new(
        &wire.github.config_url,
        &wire.github.client_id,
        wire.github.installation_id,
        &wire.github.keychain_service,
        &wire.github.keychain_account,
    )
    .map_err(|_| invalid_configuration())?;
    let target = ScaleSetBridgeTarget::new(
        wire.scale_set.id,
        &wire.scale_set.name,
        wire.scale_set.runner_group_id,
        &wire.scale_set.labels,
        &wire.scale_set.owner,
        1,
    )
    .map_err(|_| invalid_configuration())?;
    let bridge_config =
        ScaleSetBridgeConfig::new(Path::new(BRIDGE_PROGRAM), bridge_digest, github_app, target)
            .map_err(|_| invalid_configuration())?;
    let prepared = current_disposable_prepared_template().map_err(|_| invalid_configuration())?;
    let network_policy = plan_disposable_network_policy(wire.network.service_uid, &prepared)
        .map_err(|_| invalid_configuration())?;
    if wire.network.backend != DisposableNetworkPolicyBackend::MacosPfDedicatedUid
        || network_policy.report().policy_identity() != &expected_network_identity
    {
        return Err(invalid_configuration());
    }
    let resources = DisposableWorkerResources::new(
        wire.resources.cpu_millis,
        wire.resources.memory_bytes,
        wire.resources.disk_bytes,
    )
    .map_err(|_| invalid_configuration())?;
    let consumer_policy = ScaleSetConsumerPolicy::new(
        bridge_config.identity(),
        wire.scale_set.id,
        &wire.scale_set.repository,
        &wire.scale_set.owner,
        &wire.scale_set.labels,
        resources,
        &prepared,
    )
    .map_err(|_| invalid_configuration())?;
    let template_runtime = DisposableTemplateRuntime::new(
        state_root.clone(),
        limactl_program.clone(),
        lima_home.clone(),
        source_instance.clone(),
    )
    .map_err(|_| invalid_configuration())?;
    let clone_runtime = DisposableCloneRuntime::new(
        state_root.clone(),
        limactl_program.clone(),
        lima_home.clone(),
        source_instance,
    )
    .map_err(|_| invalid_configuration())?;
    let runner_runtime = DisposableRunnerRuntime::new(limactl_program, lima_home)
        .map_err(|_| invalid_configuration())?;
    Ok(DisposableWorkerEnrollment {
        state_root,
        network_policy,
        bridge_config,
        consumer_policy,
        template_runtime,
        clone_runtime,
        runner_runtime,
    })
}

fn canonical_bytes(wire: &EnrollmentWire) -> Result<Vec<u8>, DisposableWorkerEnrollmentError> {
    let mut bytes = serde_json::to_vec_pretty(wire).map_err(|_| invalid_document())?;
    bytes.push(b'\n');
    Ok(bytes)
}

const fn invalid_document() -> DisposableWorkerEnrollmentError {
    enrollment_error(
        DisposableWorkerEnrollmentErrorKind::InvalidDocument,
        "disposable_worker_enrollment_invalid",
        "the disposable-worker enrollment document is invalid",
    )
}

const fn invalid_configuration() -> DisposableWorkerEnrollmentError {
    enrollment_error(
        DisposableWorkerEnrollmentErrorKind::InvalidConfiguration,
        "disposable_worker_enrollment_configuration_invalid",
        "the disposable-worker enrollment configuration is invalid",
    )
}

const fn enrollment_error(
    kind: DisposableWorkerEnrollmentErrorKind,
    code: &'static str,
    message: &'static str,
) -> DisposableWorkerEnrollmentError {
    DisposableWorkerEnrollmentError {
        kind,
        code,
        message,
    }
}

#[derive(Deserialize)]
struct VersionWire {
    schema_version: u8,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct EnrollmentWire {
    schema_version: u8,
    state_root: String,
    network: NetworkWire,
    lima: LimaWire,
    bridge: BridgeWire,
    github: GitHubWire,
    scale_set: ScaleSetWire,
    resources: ResourcesWire,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct NetworkWire {
    backend: DisposableNetworkPolicyBackend,
    service_uid: u32,
    policy_identity: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LimaWire {
    program: String,
    home: String,
    source_instance: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BridgeWire {
    program_digest: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GitHubWire {
    config_url: String,
    client_id: String,
    installation_id: u64,
    keychain_service: String,
    keychain_account: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ScaleSetWire {
    id: u32,
    name: String,
    runner_group_id: u32,
    owner: String,
    repository: String,
    labels: Vec<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ResourcesWire {
    cpu_millis: u32,
    memory_bytes: u64,
    disk_bytes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const NETWORK_DIGEST: &str =
        "sha256:65ceec8974086e378f216acc555724cb40b08ccc047391dedd0b6f17df72587e";

    fn canonical_document() -> Vec<u8> {
        canonical_bytes(&EnrollmentWire {
            schema_version: DISPOSABLE_WORKER_ENROLLMENT_SCHEMA_VERSION,
            state_root: "/private/var/lib/smolrunner".to_owned(),
            network: NetworkWire {
                backend: DisposableNetworkPolicyBackend::MacosPfDedicatedUid,
                service_uid: 502,
                policy_identity: NETWORK_DIGEST.to_owned(),
            },
            lima: LimaWire {
                program: "/opt/homebrew/bin/limactl".to_owned(),
                home: "/private/var/lib/smolrunner/lima".to_owned(),
                source_instance: "smolrunner-prepared-template".to_owned(),
            },
            bridge: BridgeWire {
                program_digest: DIGEST.to_owned(),
            },
            github: GitHubWire {
                config_url: "https://github.com/acme".to_owned(),
                client_id: "Iv1.0123456789abcdef".to_owned(),
                installation_id: 42,
                keychain_service: "smolrunner.github-app".to_owned(),
                keychain_account: "acme-ci".to_owned(),
            },
            scale_set: ScaleSetWire {
                id: 17,
                name: "smolrunner-disposable".to_owned(),
                runner_group_id: 3,
                owner: "acme".to_owned(),
                repository: "widgets".to_owned(),
                labels: vec!["self-hosted".to_owned(), "smolrunner".to_owned()],
            },
            resources: ResourcesWire {
                cpu_millis: 2_000,
                memory_bytes: 2 << 30,
                disk_bytes: 20 << 30,
            },
        })
        .unwrap()
    }

    #[test]
    fn canonical_enrollment_builds_all_existing_policy_boundaries() {
        let enrollment = decode_disposable_worker_enrollment(&canonical_document()).unwrap();
        let debug = format!("{enrollment:?}");
        assert!(!debug.contains("/private"));
        assert!(!debug.contains("acme-ci"));
        assert!(!debug.contains("Iv1."));
        assert!(!debug.contains(DIGEST));
        assert!(!debug.contains(NETWORK_DIGEST));

        let parts = enrollment.into_parts();
        assert_eq!(parts.state_root, Path::new("/private/var/lib/smolrunner"));
        assert_eq!(parts.network_policy.report().service_uid(), 502);
        assert_eq!(
            parts.network_policy.report().policy_identity().as_str(),
            NETWORK_DIGEST
        );
        let _ = parts.bridge_config;
        let _ = parts.consumer_policy;
        let _ = parts.template_runtime;
        let _ = parts.clone_runtime;
        let _ = parts.runner_runtime;
    }

    #[test]
    fn prior_future_unknown_and_noncanonical_documents_fail_closed() {
        let current = String::from_utf8(canonical_document()).unwrap();
        for version in [0, 1, 3] {
            let changed = current.replacen(
                "\"schema_version\": 2",
                &format!("\"schema_version\": {version}"),
                1,
            );
            let error = decode_disposable_worker_enrollment(changed.as_bytes()).unwrap_err();
            assert_eq!(
                error.kind(),
                DisposableWorkerEnrollmentErrorKind::VersionIncompatible
            );
        }

        let unknown = current.replacen(
            "\"state_root\":",
            "\"unexpected\": true,\n  \"state_root\":",
            1,
        );
        assert_eq!(
            decode_disposable_worker_enrollment(unknown.as_bytes())
                .unwrap_err()
                .kind(),
            DisposableWorkerEnrollmentErrorKind::InvalidDocument
        );

        let compact: serde_json::Value = serde_json::from_str(&current).unwrap();
        let compact = serde_json::to_vec(&compact).unwrap();
        assert_eq!(
            decode_disposable_worker_enrollment(&compact)
                .unwrap_err()
                .kind(),
            DisposableWorkerEnrollmentErrorKind::NonCanonical
        );
    }

    #[test]
    fn invalid_paths_identity_resources_and_labels_are_bounded_and_private() {
        let current = String::from_utf8(canonical_document()).unwrap();
        let cases = [
            current.replace("/opt/homebrew/bin/limactl", "relative/limactl"),
            current.replace(DIGEST, "sha256:not-a-digest"),
            current.replace(NETWORK_DIGEST, "sha256:not-a-digest"),
            current.replace("\"service_uid\": 502", "\"service_uid\": 0"),
            current.replace("\"cpu_millis\": 2000", "\"cpu_millis\": 1"),
            current.replace(
                "\"self-hosted\",\n      \"smolrunner\"",
                "\"smolrunner\",\n      \"self-hosted\"",
            ),
            current.replace(
                "\"self-hosted\",\n      \"smolrunner\"",
                "\"smolrunner\",\n      \"smolrunner\"",
            ),
        ];
        for bytes in cases {
            let error = decode_disposable_worker_enrollment(bytes.as_bytes()).unwrap_err();
            assert_eq!(
                error.kind(),
                DisposableWorkerEnrollmentErrorKind::InvalidConfiguration
            );
            assert_eq!(
                error.code(),
                "disposable_worker_enrollment_configuration_invalid"
            );
            assert!(!format!("{error:?}").contains("private"));
        }
    }

    #[test]
    fn byte_bound_precedes_json_parsing() {
        let bytes = vec![b' '; MAX_DISPOSABLE_WORKER_ENROLLMENT_BYTES + 1];
        let error = decode_disposable_worker_enrollment(&bytes).unwrap_err();
        assert_eq!(error.code(), "disposable_worker_enrollment_too_large");
    }
}
