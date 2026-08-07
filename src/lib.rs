/// Read-only observation of one configured official Actions runner.
pub mod actions_runner_readiness;
pub mod artifact;
#[cfg(target_os = "linux")]
pub mod debian_package_plan;
#[cfg(target_os = "linux")]
pub mod debian_package_probe;
#[cfg(target_os = "linux")]
pub mod debian_package_recovery;
/// Descriptor-bound execution of already reviewed Linux launch plans.
#[cfg(target_os = "linux")]
pub mod descriptor_bound_launcher;
pub mod doctor;
pub mod durable_journal;
#[cfg(target_os = "linux")]
pub mod durable_lane_execution;
/// Pure exact-commit runner-to-publisher handoff contracts and reports.
pub mod exact_commit_handoff;
#[cfg_attr(test, allow(clippy::too_many_arguments))]
pub mod execution_admission;
pub mod execution_receipt;
pub mod execution_receipt_store;
/// Pure, fail-closed mapping of reviewed GitHub workflow-job evidence into typed broker intents.
pub mod github_workflow_job_mapper;
/// Pure, bounded normalization of complete GitHub workflow-job reconciliation snapshots.
pub mod github_workflow_job_reconciliation;
pub mod host;
#[cfg(target_os = "linux")]
pub mod host_package_plan;
#[cfg(target_os = "linux")]
pub mod host_preparation_command;
#[cfg(target_os = "linux")]
pub mod host_preparation_execution;
#[cfg(target_os = "linux")]
pub mod host_preparation_plan;
#[cfg(target_os = "linux")]
pub mod host_preparation_receipt;
pub mod host_preparation_receipt_binding;
#[cfg(target_os = "linux")]
pub mod host_readiness;
#[cfg(target_os = "linux")]
pub mod host_readiness_verdict;
#[cfg(target_os = "linux")]
pub mod host_rootless_podman;
#[cfg(target_os = "linux")]
pub mod installation_id;
pub mod journal;
pub mod journal_document;
pub mod lane_command;
#[cfg(target_os = "linux")]
pub mod lane_executable;
#[cfg(target_os = "linux")]
pub mod lane_executor;
pub mod lease;
pub mod lease_catalog;
pub mod lease_document;
/// Pure Lima policy: work while active, interactive after 10 idle minutes, stopped after 30.
pub mod lima_lifecycle;
/// Fixed direct executor for accepted personal-worker Lima lifecycle actions.
pub mod lima_lifecycle_executor;
/// Read-only, bounded exact observation of one Lima instance and running guest.
pub mod lima_observation;
/// Read-only, fail-closed lookup of persisted project installations.
#[cfg(target_os = "linux")]
pub mod linux_installation_catalog;
/// Nonblocking coordination for installation-catalog discovery and creation.
#[cfg(target_os = "linux")]
pub mod linux_installation_catalog_lock;
/// Locked, race-free create-or-load orchestration for local project installations.
#[cfg(target_os = "linux")]
pub mod linux_installation_enrollment;
/// Staged, durable, no-replace publication of complete project installations.
#[cfg(target_os = "linux")]
pub mod linux_installation_publication;
/// Durable, revision-checked lease persistence beneath one installation directory.
#[cfg(target_os = "linux")]
pub mod linux_lease_store;
#[cfg(target_os = "linux")]
pub mod linux_state;
#[cfg(target_os = "linux")]
pub mod linux_state_prepare;
#[cfg(target_os = "linux")]
pub mod linux_state_recovery;
pub mod mac_auto_availability;
pub mod mac_auto_controller;
pub mod mac_availability;
pub mod macos_operator_activity;
pub mod macos_resource_observation;
pub mod manifest;
/// Pure schema-versioned personal-worker operator configuration and public identity.
pub mod operator_config;
/// Closed public operator error, retry, remediation, dependency, approval, and command vocabulary.
pub mod operator_error;
/// Pure unified personal-worker operator status report and human renderer.
pub mod operator_status;
pub mod ownership;
/// Pure composition of durable queue, Lima lifecycle, and runner-readiness evidence.
pub mod personal_worker_host_broker;
pub mod personal_worker_queue;
/// Pure bounded projection of durable personal-worker status, queue pages, and job state.
pub mod personal_worker_read_model;
pub mod personal_worker_store;
pub mod personal_worker_store_transaction;
pub mod plan;
pub mod podman_preview;
pub mod podman_preview_execution;
pub mod podman_preview_inspect;
pub mod podman_preview_state;
pub mod preview;
pub mod process;
pub mod renderprove_artifact_binding;
pub mod renderprove_execution;
#[cfg(target_os = "linux")]
pub mod renderprove_native_probe;
/// Descriptor-bound protected project/evidence mount lease for native Renderprove probes.
#[cfg(target_os = "linux")]
pub mod renderprove_protected_mount;
pub mod renderprove_verification;
pub mod renderprove_vision_profile;
pub mod renderprove_vision_result;
pub mod resource;
#[cfg(target_os = "linux")]
pub mod rootless_podman_config;
/// Strict, bounded, nonblocking, descriptor-relative, identity-bound observation of reviewed Podman sources.
#[cfg(target_os = "linux")]
pub mod rootless_podman_config_observation;
/// Pure, bounded, fail-closed precedence resolution and static-preflight assessment of Podman config.
#[cfg(target_os = "linux")]
pub mod rootless_podman_config_resolution;
#[cfg(target_os = "linux")]
pub mod rootless_podman_preflight;
#[cfg(target_os = "linux")]
pub mod runner_account_observation;
#[cfg(target_os = "linux")]
pub mod runner_account_plan;
/// Credentialless, bounded exact-commit Git bundle export execution and records.
pub mod runner_export;
#[cfg(target_os = "linux")]
pub mod runner_user;
#[cfg(target_os = "linux")]
pub mod runner_user_observation;
/// Pure classification of trusted bounded Rust memory-pressure observations.
pub mod rust_memory_diagnostic;
/// Pure repository-declared Rust build-scope and bounded resource-envelope contracts.
pub mod rust_verification_envelope;
/// Canonical digest binding for reviewed Rust verification envelopes.
pub mod rust_verification_envelope_digest;
pub mod state;
pub mod state_document;
pub mod state_store;
#[cfg(target_os = "linux")]
pub mod subordinate_id;
/// Descriptor-relative trusted producer for runner workspace and cache identity receipts.
#[cfg(target_os = "linux")]
pub mod trusted_workspace_receipt;
#[cfg(unix)]
pub mod unix_personal_worker_store;
pub mod verification_profile;
pub mod verification_profile_preflight_adapter;
pub mod verification_profile_registry;

use serde::Serialize;

pub const REPORT_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

impl CheckStatus {
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::Pass => 0,
            Self::Warn => 1,
            Self::Fail => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub id: String,
    pub status: CheckStatus,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Check {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        status: CheckStatus,
        summary: impl Into<String>,
        detail: Option<String>,
    ) -> Self {
        Self {
            id: id.into(),
            status,
            summary: summary.into(),
            detail,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorReport {
    pub schema_version: u8,
    pub overall: CheckStatus,
    pub checks: Vec<Check>,
}

impl DoctorReport {
    #[must_use]
    pub fn from_checks(checks: Vec<Check>) -> Self {
        let overall = checks
            .iter()
            .map(|check| check.status)
            .max_by_key(|status| status.rank())
            .unwrap_or(CheckStatus::Pass);

        Self {
            schema_version: REPORT_SCHEMA_VERSION,
            overall,
            checks,
        }
    }

    #[must_use]
    pub fn has_failures(&self) -> bool {
        self.overall == CheckStatus::Fail
    }

    #[must_use]
    pub fn has_warnings(&self) -> bool {
        self.checks
            .iter()
            .any(|check| check.status == CheckStatus::Warn)
    }
}

#[cfg(test)]
mod tests {
    use super::{Check, CheckStatus, DoctorReport};

    #[test]
    fn report_uses_most_severe_status() {
        let report = DoctorReport::from_checks(vec![
            Check::new("ok", CheckStatus::Pass, "ok", None),
            Check::new("warning", CheckStatus::Warn, "warning", None),
            Check::new("failure", CheckStatus::Fail, "failure", None),
        ]);

        assert_eq!(report.overall, CheckStatus::Fail);
        assert!(report.has_failures());
        assert!(report.has_warnings());
    }
}
