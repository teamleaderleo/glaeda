pub mod artifact;
#[cfg(target_os = "linux")]
pub mod debian_package_plan;
#[cfg(target_os = "linux")]
pub mod debian_package_probe;
pub mod doctor;
pub mod durable_journal;
pub mod host;
#[cfg(target_os = "linux")]
pub mod host_package_plan;
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
pub mod manifest;
pub mod ownership;
pub mod plan;
pub mod podman_preview;
pub mod podman_preview_execution;
pub mod podman_preview_inspect;
pub mod podman_preview_state;
pub mod preview;
pub mod process;
pub mod resource;
#[cfg(target_os = "linux")]
pub mod runner_account_plan;
#[cfg(target_os = "linux")]
pub mod runner_user;
pub mod state;
pub mod state_document;
pub mod state_store;

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
