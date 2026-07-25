use serde::Serialize;

use crate::debian_package_plan::PackagePlanDisposition;
use crate::host_readiness::{HostObservationState, HostReadinessReport, RunnerAccountReadiness};
use crate::runner_account_plan::{RunnerAccountPlanDisposition, RunnerAccountResourceKind};
use crate::subordinate_id::{
    PodmanMigrationPlan, SubordinateIdKind, SubordinatePlanDisposition,
};

pub const HOST_READINESS_VERDICT_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostReadinessDisposition {
    Ready,
    ChangesRequired,
    NeedsInspection,
    Blocked,
}

impl HostReadinessDisposition {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::ChangesRequired => "changes_required",
            Self::NeedsInspection => "needs_inspection",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostReadinessDomain {
    Executable,
    Package,
    RunnerAccount,
    SubordinateId,
    RootlessPodman,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostReadinessFinding {
    pub id: String,
    pub domain: HostReadinessDomain,
    pub disposition: HostReadinessDisposition,
    pub summary: String,
}

#[derive(Debug, Serialize)]
pub struct HostReadinessAssessment<'a> {
    pub schema_version: u8,
    pub disposition: HostReadinessDisposition,
    pub findings: Vec<HostReadinessFinding>,
    pub report: &'a HostReadinessReport,
}

#[must_use]
pub fn assess(report: &HostReadinessReport) -> HostReadinessAssessment<'_> {
    let mut findings = Vec::new();

    for executable in &report.executables {
        let disposition = match executable.state {
            HostObservationState::Matching => continue,
            HostObservationState::Absent => HostReadinessDisposition::ChangesRequired,
            HostObservationState::Unknown => HostReadinessDisposition::NeedsInspection,
            HostObservationState::Conflicting => HostReadinessDisposition::Blocked,
        };
        findings.push(HostReadinessFinding {
            id: format!("executable-{}", executable.name),
            domain: HostReadinessDomain::Executable,
            disposition,
            summary: format!(
                "reviewed executable {} at {} is {}",
                executable.name,
                executable.path.display(),
                observation_state_name(executable.state)
            ),
        });
    }

    match report.package_plan.disposition {
        PackagePlanDisposition::Ready => {}
        PackagePlanDisposition::Required => findings.push(HostReadinessFinding {
            id: "debian-packages".to_owned(),
            domain: HostReadinessDomain::Package,
            disposition: HostReadinessDisposition::ChangesRequired,
            summary: format!(
                "reviewed Debian packages are missing: {}",
                package_names(&report.package_plan.missing_packages)
            ),
        }),
        PackagePlanDisposition::NeedsInspection => findings.push(HostReadinessFinding {
            id: "debian-packages".to_owned(),
            domain: HostReadinessDomain::Package,
            disposition: HostReadinessDisposition::NeedsInspection,
            summary: format!(
                "reviewed Debian packages need inspection: {}",
                package_names(&report.package_plan.unknown_packages)
            ),
        }),
    }

    match &report.runner_account {
        RunnerAccountReadiness::NeedsConfiguration { .. } => findings.push(
            HostReadinessFinding {
                id: "runner-account-policy".to_owned(),
                domain: HostReadinessDomain::RunnerAccount,
                disposition: HostReadinessDisposition::Blocked,
                summary: "exact runner account policy is missing".to_owned(),
            },
        ),
        RunnerAccountReadiness::Planned {
            plan,
            subordinate_ids,
            ..
        } => {
            for item in &plan.items {
                let disposition = match item.disposition {
                    RunnerAccountPlanDisposition::Satisfied => continue,
                    RunnerAccountPlanDisposition::Required => {
                        HostReadinessDisposition::ChangesRequired
                    }
                    RunnerAccountPlanDisposition::NeedsInspection => {
                        HostReadinessDisposition::NeedsInspection
                    }
                    RunnerAccountPlanDisposition::Blocked => HostReadinessDisposition::Blocked,
                };
                findings.push(HostReadinessFinding {
                    id: format!("runner-account-{}", account_resource_name(item.kind)),
                    domain: HostReadinessDomain::RunnerAccount,
                    disposition,
                    summary: item.summary.clone(),
                });
            }

            for item in [
                &subordinate_ids.subordinate_uids,
                &subordinate_ids.subordinate_gids,
            ] {
                let Some(disposition) = subordinate_disposition(item.disposition) else {
                    continue;
                };
                findings.push(HostReadinessFinding {
                    id: format!("subordinate-{}-mapping", subordinate_kind_name(item.kind)),
                    domain: HostReadinessDomain::SubordinateId,
                    disposition,
                    summary: item.summary.clone(),
                });
            }

            match &subordinate_ids.podman_migration {
                PodmanMigrationPlan::NotRequired => {}
                PodmanMigrationPlan::Required { .. } => findings.push(HostReadinessFinding {
                    id: "rootless-podman-migration".to_owned(),
                    domain: HostReadinessDomain::RootlessPodman,
                    disposition: HostReadinessDisposition::ChangesRequired,
                    summary: "rootless Podman namespace migration is required after mapping reconciliation"
                        .to_owned(),
                }),
                PodmanMigrationPlan::Blocked { evidence } => findings.push(
                    HostReadinessFinding {
                        id: "rootless-podman-migration".to_owned(),
                        domain: HostReadinessDomain::RootlessPodman,
                        disposition: HostReadinessDisposition::Blocked,
                        summary: if evidence.is_empty() {
                            "rootless Podman namespace migration cannot be planned".to_owned()
                        } else {
                            evidence.join("; ")
                        },
                    },
                ),
            }
        }
    }

    HostReadinessAssessment {
        schema_version: HOST_READINESS_VERDICT_SCHEMA_VERSION,
        disposition: overall_disposition(&findings),
        findings,
        report,
    }
}

#[must_use]
pub fn render_human(assessment: &HostReadinessAssessment<'_>) -> String {
    let mut output = format!(
        "Overall host readiness: {}\n",
        assessment.disposition.as_str()
    );
    if assessment.findings.is_empty() {
        output.push_str("No readiness findings remain.\n\n");
    } else {
        output.push_str("Readiness findings:\n");
        for finding in &assessment.findings {
            output.push_str(&format!(
                "[{}] {}: {}\n",
                finding.disposition.as_str(),
                finding.id,
                finding.summary
            ));
        }
        output.push('\n');
    }
    output.push_str(&crate::host_readiness::render_human(assessment.report));
    output
}

fn overall_disposition(findings: &[HostReadinessFinding]) -> HostReadinessDisposition {
    findings
        .iter()
        .map(|finding| finding.disposition)
        .max()
        .unwrap_or(HostReadinessDisposition::Ready)
}

fn subordinate_disposition(
    disposition: SubordinatePlanDisposition,
) -> Option<HostReadinessDisposition> {
    match disposition {
        SubordinatePlanDisposition::Satisfied => None,
        SubordinatePlanDisposition::Required => Some(HostReadinessDisposition::ChangesRequired),
        SubordinatePlanDisposition::NeedsInspection => {
            Some(HostReadinessDisposition::NeedsInspection)
        }
        SubordinatePlanDisposition::Blocked => Some(HostReadinessDisposition::Blocked),
    }
}

fn observation_state_name(state: HostObservationState) -> &'static str {
    match state {
        HostObservationState::Matching => "matching",
        HostObservationState::Absent => "absent",
        HostObservationState::Unknown => "unknown",
        HostObservationState::Conflicting => "conflicting",
    }
}

fn account_resource_name(kind: RunnerAccountResourceKind) -> &'static str {
    match kind {
        RunnerAccountResourceKind::Group => "group",
        RunnerAccountResourceKind::User => "user",
        RunnerAccountResourceKind::HomeDirectory => "home-directory",
        RunnerAccountResourceKind::SubordinateUids => "subordinate-uids",
        RunnerAccountResourceKind::SubordinateGids => "subordinate-gids",
        RunnerAccountResourceKind::Linger => "linger",
    }
}

fn subordinate_kind_name(kind: SubordinateIdKind) -> &'static str {
    match kind {
        SubordinateIdKind::Uid => "uid",
        SubordinateIdKind::Gid => "gid",
    }
}

fn package_names(packages: &[crate::lane_command::PackageName]) -> String {
    packages
        .iter()
        .map(crate::lane_command::PackageName::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use crate::subordinate_id::{PodmanMigrationPlan, SubordinatePlanDisposition};

    use super::{
        HostReadinessDisposition, HostReadinessDomain, HostReadinessFinding, overall_disposition,
        subordinate_disposition,
    };

    fn finding(disposition: HostReadinessDisposition) -> HostReadinessFinding {
        HostReadinessFinding {
            id: "test".to_owned(),
            domain: HostReadinessDomain::Executable,
            disposition,
            summary: "test finding".to_owned(),
        }
    }

    #[test]
    fn empty_findings_are_ready() {
        assert_eq!(overall_disposition(&[]), HostReadinessDisposition::Ready);
    }

    #[test]
    fn most_severe_finding_controls_the_verdict() {
        let findings = [
            finding(HostReadinessDisposition::ChangesRequired),
            finding(HostReadinessDisposition::Blocked),
            finding(HostReadinessDisposition::NeedsInspection),
        ];
        assert_eq!(
            overall_disposition(&findings),
            HostReadinessDisposition::Blocked
        );
    }

    #[test]
    fn subordinate_mapping_dispositions_feed_the_verdict() {
        assert_eq!(
            subordinate_disposition(SubordinatePlanDisposition::Satisfied),
            None
        );
        assert_eq!(
            subordinate_disposition(SubordinatePlanDisposition::Required),
            Some(HostReadinessDisposition::ChangesRequired)
        );
        assert_eq!(
            subordinate_disposition(SubordinatePlanDisposition::NeedsInspection),
            Some(HostReadinessDisposition::NeedsInspection)
        );
        assert_eq!(
            subordinate_disposition(SubordinatePlanDisposition::Blocked),
            Some(HostReadinessDisposition::Blocked)
        );
    }

    #[test]
    fn blocked_podman_migration_carries_bounded_evidence() {
        let plan = PodmanMigrationPlan::Blocked {
            evidence: vec!["exact runner identity is unavailable".to_owned()],
        };
        let PodmanMigrationPlan::Blocked { evidence } = plan else {
            panic!("blocked migration plan");
        };
        assert_eq!(evidence, ["exact runner identity is unavailable"]);
    }

    #[test]
    fn serialized_dispositions_are_stable_snake_case() {
        assert_eq!(
            serde_json::to_string(&HostReadinessDisposition::ChangesRequired)
                .expect("serialize disposition"),
            "\"changes_required\""
        );
    }
}
