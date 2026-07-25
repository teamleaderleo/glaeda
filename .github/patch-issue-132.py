from pathlib import Path

host = Path("src/host_readiness.rs")
text = host.read_text()

old_imports = '''use crate::process::CommandExecutor;
use crate::runner_account_observation::{RunnerAccountObservationPaths, observe_runner_account};
'''
new_imports = '''use crate::process::CommandExecutor;
use crate::rootless_podman_config_observation::{
    RootlessPodmanConfigObservationContext, RootlessPodmanConfigObservationPaths,
    RootlessPodmanConfigObservationReport, observe_rootless_podman_config,
};
use crate::rootless_podman_config_resolution::RootlessPodmanConfigPolicy;
use crate::rootless_podman_preflight::{
    RootlessPodmanPreflightDisposition, RootlessPodmanPreflightPaths,
    RootlessPodmanStaticPreflightReport, observe_rootless_podman_static_preflight,
};
use crate::runner_account_observation::{
    RunnerAccountObservationPaths, RunnerAccountObservationReport, observe_runner_account,
};
'''
if text.count(old_imports) != 1:
    raise SystemExit("host imports anchor missing or duplicated")
text = text.replace(old_imports, new_imports, 1)

type_anchor = '''#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostReadinessReport {
'''
new_type = '''#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "disposition", rename_all = "snake_case")]
pub enum RootlessPodmanHostReadiness {
    NeedsAccountEvidence {
        evidence: Vec<String>,
    },
    Observed {
        configuration: Box<RootlessPodmanConfigObservationReport>,
        preflight: Box<RootlessPodmanStaticPreflightReport>,
    },
}

'''
if text.count(type_anchor) != 1:
    raise SystemExit("host report type anchor missing or duplicated")
text = text.replace(type_anchor, new_type + type_anchor, 1)

old_report = '''    pub package_plan: DebianPackagePlan,
    pub runner_account: RunnerAccountReadiness,
}
'''
new_report = '''    pub package_plan: DebianPackagePlan,
    pub runner_account: RunnerAccountReadiness,
    pub rootless_podman: RootlessPodmanHostReadiness,
}
'''
if text.count(old_report) != 1:
    raise SystemExit("host report fields anchor missing or duplicated")
text = text.replace(old_report, new_report, 1)

old_error = '''    RunnerAccountPlan,
    SubordinateIdPlan,
}
'''
new_error = '''    RunnerAccountPlan,
    SubordinateIdPlan,
    RootlessPodmanObservation,
}
'''
if text.count(old_error) != 1:
    raise SystemExit("host error kind anchor missing or duplicated")
text = text.replace(old_error, new_error, 1)

old_block = '''    let runner_account = match policy {
        Some(policy) => {
            let desired = policy.desired_account(manifest)?;
            let report = observe_runner_account(
                &desired,
                executor,
                &RunnerAccountObservationPaths::system_default(),
            )
            .map_err(|_| {
                HostReadinessError::new(
                    HostReadinessErrorKind::RunnerAccountObservation,
                    "failed to classify bounded runner account observations",
                )
            })?;
            let identity = report
                .identity()
                .map(|identity| (identity.uid(), identity.primary_gid()));
            let observations = report.observations;
            let subordinate_ids = build_exact_subordinate_id_plan(
                &desired,
                &observations,
                identity,
                Path::new("/etc/subuid"),
                Path::new("/etc/subgid"),
            )
            .map_err(|_| {
                HostReadinessError::new(
                    HostReadinessErrorKind::SubordinateIdPlan,
                    "failed to build a dependency-safe subordinate-ID reconciliation plan",
                )
            })?;
            let plan = without_subordinate_mapping_items(
                build_runner_account_plan(desired, observations.clone()).map_err(|_| {
                    HostReadinessError::new(
                        HostReadinessErrorKind::RunnerAccountPlan,
                        "failed to build a dependency-safe runner account plan",
                    )
                })?,
            );
            RunnerAccountReadiness::Planned {
                observations: Box::new(observations),
                plan,
                subordinate_ids: Box::new(subordinate_ids),
            }
        }
        None => RunnerAccountReadiness::NeedsConfiguration {
            evidence: vec![format!(
                "runner account policy is missing at {}; exact home and subordinate-ID ranges remain unconfigured",
                policy_path.display()
            )],
        },
    };

    Ok(HostReadinessReport {
        schema_version: HOST_READINESS_SCHEMA_VERSION,
        repository: manifest.repository.clone(),
        executables: observe_required_executables(),
        package_plan,
        runner_account,
    })
}
'''
new_block = '''    let (runner_account, rootless_podman) = match policy {
        Some(policy) => {
            let desired = policy.desired_account(manifest)?;
            let account_report = observe_runner_account(
                &desired,
                executor,
                &RunnerAccountObservationPaths::system_default(),
            )
            .map_err(|_| {
                HostReadinessError::new(
                    HostReadinessErrorKind::RunnerAccountObservation,
                    "failed to classify bounded runner account observations",
                )
            })?;
            let rootless_podman = inspect_rootless_podman_readiness(
                &desired,
                &account_report,
                &package_plan,
            )?;
            let identity = account_report
                .identity()
                .map(|identity| (identity.uid(), identity.primary_gid()));
            let observations = account_report.observations;
            let subordinate_ids = build_exact_subordinate_id_plan(
                &desired,
                &observations,
                identity,
                Path::new("/etc/subuid"),
                Path::new("/etc/subgid"),
            )
            .map_err(|_| {
                HostReadinessError::new(
                    HostReadinessErrorKind::SubordinateIdPlan,
                    "failed to build a dependency-safe subordinate-ID reconciliation plan",
                )
            })?;
            let plan = without_subordinate_mapping_items(
                build_runner_account_plan(desired, observations.clone()).map_err(|_| {
                    HostReadinessError::new(
                        HostReadinessErrorKind::RunnerAccountPlan,
                        "failed to build a dependency-safe runner account plan",
                    )
                })?,
            );
            (
                RunnerAccountReadiness::Planned {
                    observations: Box::new(observations),
                    plan,
                    subordinate_ids: Box::new(subordinate_ids),
                },
                rootless_podman,
            )
        }
        None => {
            let evidence = format!(
                "runner account policy is missing at {}; exact home and subordinate-ID ranges remain unconfigured",
                policy_path.display()
            );
            (
                RunnerAccountReadiness::NeedsConfiguration {
                    evidence: vec![evidence.clone()],
                },
                RootlessPodmanHostReadiness::NeedsAccountEvidence {
                    evidence: vec![evidence],
                },
            )
        }
    };

    Ok(HostReadinessReport {
        schema_version: HOST_READINESS_SCHEMA_VERSION,
        repository: manifest.repository.clone(),
        executables: observe_required_executables(),
        package_plan,
        runner_account,
        rootless_podman,
    })
}
'''
if text.count(old_block) != 1:
    raise SystemExit("host planning composition block missing or duplicated")
text = text.replace(old_block, new_block, 1)

helper_anchor = '''fn without_subordinate_mapping_items(mut plan: RunnerAccountPlan) -> RunnerAccountPlan {
'''
helper = '''fn inspect_rootless_podman_readiness(
    desired: &DesiredRunnerAccount,
    account_report: &RunnerAccountObservationReport,
    package_plan: &DebianPackagePlan,
) -> Result<RootlessPodmanHostReadiness, HostReadinessError> {
    let Some(identity) = account_report.identity() else {
        return Ok(RootlessPodmanHostReadiness::NeedsAccountEvidence {
            evidence: vec![
                "rootless Podman configuration observation is blocked until the exact runner identity is proven"
                    .to_owned(),
            ],
        });
    };
    if account_report.observations.home.state() != PreparationObservationState::Matching {
        return Ok(RootlessPodmanHostReadiness::NeedsAccountEvidence {
            evidence: vec![
                "rootless Podman configuration observation is blocked until the reviewed runner home matches policy"
                    .to_owned(),
            ],
        });
    }
    if identity.uid() == 0
        || identity.primary_gid() == 0
        || identity.primary_gid() != identity.group_gid()
    {
        return Ok(RootlessPodmanHostReadiness::NeedsAccountEvidence {
            evidence: vec![
                "rootless Podman configuration observation requires one exact non-root runner UID and primary GID"
                    .to_owned(),
            ],
        });
    }

    let home = PathBuf::from(desired.home());
    let xdg_config_home = home.join(".config");
    let xdg_data_home = home.join(".local/share");
    let xdg_runtime_dir = PathBuf::from(format!("/run/user/{}", identity.uid()));
    let context = RootlessPodmanConfigObservationContext::new(
        home.clone(),
        xdg_config_home,
        xdg_data_home.clone(),
        xdg_runtime_dir.clone(),
        identity.uid(),
        identity.primary_gid(),
    )
    .map_err(|_| {
        HostReadinessError::new(
            HostReadinessErrorKind::RootlessPodmanObservation,
            "failed to construct the reviewed rootless Podman observation context",
        )
    })?;
    let policy = RootlessPodmanConfigPolicy::new(
        "overlay",
        xdg_data_home.join("containers/storage"),
        xdg_runtime_dir.join("containers"),
        "/usr/bin/fuse-overlayfs",
        "systemd",
        "netavark",
    )
    .map_err(|_| {
        HostReadinessError::new(
            HostReadinessErrorKind::RootlessPodmanObservation,
            "failed to construct the explicit rootless Podman host policy",
        )
    })?;
    let configuration = observe_rootless_podman_config(
        &context,
        &RootlessPodmanConfigObservationPaths::system_default(),
        &policy,
    )
    .map_err(|_| {
        HostReadinessError::new(
            HostReadinessErrorKind::RootlessPodmanObservation,
            "failed to represent trusted rootless Podman configuration observations",
        )
    })?;
    let preflight = observe_rootless_podman_static_preflight(
        package_plan,
        &account_report.observations,
        Some(identity),
        &configuration.assessment,
        &RootlessPodmanPreflightPaths::system_default(),
    );

    Ok(RootlessPodmanHostReadiness::Observed {
        configuration: Box::new(configuration),
        preflight: Box::new(preflight),
    })
}

'''
if text.count(helper_anchor) != 1:
    raise SystemExit("host helper anchor missing or duplicated")
text = text.replace(helper_anchor, helper + helper_anchor, 1)

render_anchor = '''    output.push_str("\nNo changes were made.\n");
'''
render_block = '''    output.push_str("\nRootless Podman static preflight\n");
    match &report.rootless_podman {
        RootlessPodmanHostReadiness::NeedsAccountEvidence { evidence } => {
            output.push_str("[BLOCKED] Exact runner identity and home evidence are required.\n");
            for item in evidence {
                output.push_str(&format!("  {item}\n"));
            }
        }
        RootlessPodmanHostReadiness::Observed {
            configuration,
            preflight,
        } => {
            output.push_str(&crate::rootless_podman_config_observation::render_human(
                configuration,
            ));
            output.push_str(&format!(
                "Static preflight disposition: {}\n",
                rootless_preflight_disposition_name(preflight.disposition)
            ));
            for item in &preflight.configuration.evidence {
                output.push_str(&format!("  {item}\n"));
            }
        }
    }

    output.push_str("\nNo changes were made.\n");
'''
if text.count(render_anchor) != 1:
    raise SystemExit("host render anchor missing or duplicated")
text = text.replace(render_anchor, render_block, 1)

read_anchor = '''fn read_account_policy(
'''
name_helper = '''fn rootless_preflight_disposition_name(
    disposition: RootlessPodmanPreflightDisposition,
) -> &'static str {
    match disposition {
        RootlessPodmanPreflightDisposition::ReadyForSmokeVerification => {
            "ready_for_smoke_verification"
        }
        RootlessPodmanPreflightDisposition::ChangesRequired => "changes_required",
        RootlessPodmanPreflightDisposition::NeedsInspection => "needs_inspection",
        RootlessPodmanPreflightDisposition::Blocked => "blocked",
    }
}

'''
if text.count(read_anchor) != 1:
    raise SystemExit("host read policy anchor missing or duplicated")
text = text.replace(read_anchor, name_helper + read_anchor, 1)
host.write_text(text)

verdict = Path("src/host_readiness_verdict.rs")
text = verdict.read_text()
old_verdict_import = '''use crate::host_readiness::{HostObservationState, HostReadinessReport, RunnerAccountReadiness};
'''
new_verdict_import = '''use crate::host_readiness::{
    HostObservationState, HostReadinessReport, RootlessPodmanHostReadiness,
    RunnerAccountReadiness,
};
use crate::rootless_podman_preflight::RootlessPodmanPreflightDisposition;
'''
if text.count(old_verdict_import) != 1:
    raise SystemExit("verdict import anchor missing or duplicated")
text = text.replace(old_verdict_import, new_verdict_import, 1)

assessment_anchor = '''    HostReadinessAssessment {
        schema_version: HOST_READINESS_VERDICT_SCHEMA_VERSION,
'''
rootless_findings = '''    match &report.rootless_podman {
        RootlessPodmanHostReadiness::NeedsAccountEvidence { evidence } => {
            findings.push(HostReadinessFinding {
                id: "rootless-podman-static-preflight".to_owned(),
                domain: HostReadinessDomain::RootlessPodman,
                disposition: HostReadinessDisposition::Blocked,
                summary: if evidence.is_empty() {
                    "rootless Podman static preflight requires exact runner account evidence"
                        .to_owned()
                } else {
                    evidence.join("; ")
                },
            });
        }
        RootlessPodmanHostReadiness::Observed { preflight, .. } => {
            let finding = match preflight.disposition {
                RootlessPodmanPreflightDisposition::ReadyForSmokeVerification => None,
                RootlessPodmanPreflightDisposition::ChangesRequired => Some((
                    HostReadinessDisposition::ChangesRequired,
                    "rootless Podman static preflight requires reviewed host changes",
                )),
                RootlessPodmanPreflightDisposition::NeedsInspection => Some((
                    HostReadinessDisposition::NeedsInspection,
                    "rootless Podman static preflight requires additional inspection",
                )),
                RootlessPodmanPreflightDisposition::Blocked => Some((
                    HostReadinessDisposition::Blocked,
                    "rootless Podman static preflight is blocked by conflicting evidence",
                )),
            };
            if let Some((disposition, summary)) = finding {
                findings.push(HostReadinessFinding {
                    id: "rootless-podman-static-preflight".to_owned(),
                    domain: HostReadinessDomain::RootlessPodman,
                    disposition,
                    summary: summary.to_owned(),
                });
            }
        }
    }

'''
if text.count(assessment_anchor) != 1:
    raise SystemExit("verdict assessment anchor missing or duplicated")
text = text.replace(assessment_anchor, rootless_findings + assessment_anchor, 1)
verdict.write_text(text)
