use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::debian_package_plan::{DebianPackagePlan, PackagePlanDisposition};
use crate::host::{CurrentHostState, Presence};
use crate::host_package_plan::{DEFAULT_OS_RELEASE_PATH, inspect_host_package_plan_from_current};
use crate::lane_command::LinuxAccountName;
use crate::lane_executable::{ExecutableVerificationErrorKind, verify_executable};
use crate::manifest::Manifest;
use crate::process::CommandExecutor;
use crate::runner_account_observation::{RunnerAccountObservationPaths, observe_runner_account};
use crate::runner_account_plan::{
    DesiredRunnerAccount, PlannedSubordinateRange, RunnerAccountObservations, RunnerAccountPlan,
    RunnerAccountPlanDisposition, RunnerAccountResourceKind, build_runner_account_plan,
};
use crate::subordinate_id::{
    PodmanMigrationPlan, SubordinateIdReconciliationPlan, SubordinatePlanDisposition,
    build_exact_subordinate_id_plan,
};

pub const HOST_READINESS_SCHEMA_VERSION: u8 = 1;
const ACCOUNT_POLICY_VERSION: u8 = 1;
const MAX_ACCOUNT_POLICY_BYTES: usize = 65_536;
const REQUIRED_EXECUTABLES: [(&str, &str); 3] = [
    ("git", "/usr/bin/git"),
    ("podman", "/usr/bin/podman"),
    ("systemctl", "/usr/bin/systemctl"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostObservationState {
    Matching,
    Absent,
    Unknown,
    Conflicting,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExactExecutableObservation {
    pub name: String,
    pub path: PathBuf,
    pub state: HostObservationState,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "disposition", rename_all = "snake_case")]
pub enum RunnerAccountReadiness {
    NeedsConfiguration {
        evidence: Vec<String>,
    },
    Planned {
        observations: Box<RunnerAccountObservations>,
        plan: RunnerAccountPlan,
        subordinate_ids: SubordinateIdReconciliationPlan,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostReadinessReport {
    pub schema_version: u8,
    pub repository: String,
    pub executables: Vec<ExactExecutableObservation>,
    pub package_plan: DebianPackagePlan,
    pub runner_account: RunnerAccountReadiness,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostReadinessErrorKind {
    AccountPolicy,
    PackagePlan,
    RunnerAccountObservation,
    RunnerAccountPlan,
    SubordinateIdPlan,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostReadinessError {
    kind: HostReadinessErrorKind,
    public_message: String,
}

impl HostReadinessError {
    #[must_use]
    pub fn kind(&self) -> HostReadinessErrorKind {
        self.kind
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.public_message
    }

    fn new(kind: HostReadinessErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            public_message: message.into(),
        }
    }
}

impl fmt::Display for HostReadinessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.public_message)
    }
}

impl std::error::Error for HostReadinessError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunnerAccountPolicyFile {
    version: u8,
    primary_group: String,
    home: String,
    subordinate_uids: SubordinateRangePolicy,
    subordinate_gids: SubordinateRangePolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SubordinateRangePolicy {
    start: u32,
    count: u32,
}

impl RunnerAccountPolicyFile {
    fn desired_account(
        &self,
        manifest: &Manifest,
    ) -> Result<DesiredRunnerAccount, HostReadinessError> {
        if self.version != ACCOUNT_POLICY_VERSION {
            return Err(HostReadinessError::new(
                HostReadinessErrorKind::AccountPolicy,
                format!(
                    "unsupported runner account policy version {}; only version {ACCOUNT_POLICY_VERSION} is accepted",
                    self.version
                ),
            ));
        }
        let username = LinuxAccountName::parse(&manifest.runner.user).map_err(|_| {
            HostReadinessError::new(
                HostReadinessErrorKind::AccountPolicy,
                "manifest runner user is not a valid reviewed Linux account name",
            )
        })?;
        let primary_group = LinuxAccountName::parse(&self.primary_group).map_err(|_| {
            HostReadinessError::new(
                HostReadinessErrorKind::AccountPolicy,
                "runner account policy primary group is not a valid reviewed Linux account name",
            )
        })?;
        let subordinate_uids =
            PlannedSubordinateRange::new(self.subordinate_uids.start, self.subordinate_uids.count)
                .map_err(|_| {
                    HostReadinessError::new(
                        HostReadinessErrorKind::AccountPolicy,
                        "runner account policy subordinate UID range is invalid",
                    )
                })?;
        let subordinate_gids =
            PlannedSubordinateRange::new(self.subordinate_gids.start, self.subordinate_gids.count)
                .map_err(|_| {
                    HostReadinessError::new(
                        HostReadinessErrorKind::AccountPolicy,
                        "runner account policy subordinate GID range is invalid",
                    )
                })?;
        DesiredRunnerAccount::new(
            username,
            primary_group,
            &self.home,
            subordinate_uids,
            subordinate_gids,
        )
        .map_err(|_| {
            HostReadinessError::new(
                HostReadinessErrorKind::AccountPolicy,
                "runner account policy home path is invalid",
            )
        })
    }
}

/// Return the conventional explicit account-policy sidecar path for one manifest.
#[must_use]
pub fn default_account_policy_path(manifest_path: &Path) -> PathBuf {
    manifest_path.with_extension("account.yml")
}

/// Inspect package, exact executable, and runner-account readiness without making changes.
///
/// An explicitly supplied account policy must exist and validate. When no explicit policy is
/// supplied, a sibling `*.account.yml` file is loaded when present. A missing sibling policy becomes
/// a typed blocked result so SmolRunner never invents a home or subordinate-ID allocation.
///
/// # Errors
///
/// Returns a bounded error for invalid account policy, unsupported package state, conservative
/// account-observation failure, or an inconsistent account plan.
pub fn inspect_host_readiness(
    manifest: &Manifest,
    manifest_path: &Path,
    account_policy_path: Option<&Path>,
    executor: &impl CommandExecutor,
) -> Result<HostReadinessReport, HostReadinessError> {
    inspect_host_readiness_with_os_release(
        manifest,
        manifest_path,
        account_policy_path,
        DEFAULT_OS_RELEASE_PATH,
        executor,
    )
}

/// Inspect host readiness with an explicit bounded os-release path.
///
/// This entry point supports deterministic tests and trusted relocated host roots.
///
/// # Errors
///
/// Returns the same bounded errors as [`inspect_host_readiness`].
pub fn inspect_host_readiness_with_os_release(
    manifest: &Manifest,
    manifest_path: &Path,
    account_policy_path: Option<&Path>,
    os_release_path: impl AsRef<Path>,
    executor: &impl CommandExecutor,
) -> Result<HostReadinessReport, HostReadinessError> {
    let package_plan = inspect_host_package_plan_from_current(
        manifest,
        unknown_legacy_state(),
        os_release_path,
        executor,
    )
    .map_err(|error| {
        HostReadinessError::new(
            HostReadinessErrorKind::PackagePlan,
            format!("failed to inspect Debian package readiness: {error}"),
        )
    })?
    .package_plan;

    let policy_path = account_policy_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_account_policy_path(manifest_path));
    let policy = read_account_policy(&policy_path, account_policy_path.is_some())?;
    let runner_account = match policy {
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
                subordinate_ids,
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

fn without_subordinate_mapping_items(mut plan: RunnerAccountPlan) -> RunnerAccountPlan {
    plan.items.retain(|item| {
        !matches!(
            item.kind,
            RunnerAccountResourceKind::SubordinateUids | RunnerAccountResourceKind::SubordinateGids
        )
    });
    plan
}

#[must_use]
pub fn render_human(report: &HostReadinessReport) -> String {
    let mut output = format!(
        "SmolRunner host readiness plan\n\nRepository: {}\n\nExact executable readiness\n",
        report.repository
    );
    for executable in &report.executables {
        let marker = match executable.state {
            HostObservationState::Matching => "READY",
            HostObservationState::Absent => "REQUIRED",
            HostObservationState::Unknown => "INSPECT",
            HostObservationState::Conflicting => "BLOCKED",
        };
        output.push_str(&format!(
            "[{marker}] {} at {}\n",
            executable.name,
            executable.path.display()
        ));
        for evidence in &executable.evidence {
            output.push_str(&format!("  {evidence}\n"));
        }
    }

    output.push_str("\nDebian package preparation\n");
    output.push_str(&format!(
        "Distribution: {} {}\n",
        report.package_plan.distribution.id(),
        report.package_plan.distribution.version_id()
    ));
    match report.package_plan.disposition {
        PackagePlanDisposition::Ready => {
            output.push_str("[READY] All reviewed prerequisite packages are present.\n");
        }
        PackagePlanDisposition::NeedsInspection => {
            output.push_str(
                "[INSPECT] Package mutation is blocked until these packages are inspected: ",
            );
            output.push_str(&package_list(&report.package_plan.unknown_packages));
            output.push('\n');
        }
        PackagePlanDisposition::Required => {
            output.push_str("[REQUIRED] The following packages are proven absent: ");
            output.push_str(&package_list(&report.package_plan.missing_packages));
            output.push('\n');
            if let Some(command) = &report.package_plan.command {
                output.push_str("  Reviewed command: ");
                output.push_str(&command.spec().displayed_argv().join(" "));
                output.push('\n');
            }
        }
    }

    output.push_str("\nRunner account preparation\n");
    match &report.runner_account {
        RunnerAccountReadiness::NeedsConfiguration { evidence } => {
            output.push_str("[BLOCKED] Exact runner account policy is required.\n");
            for item in evidence {
                output.push_str(&format!("  {item}\n"));
            }
        }
        RunnerAccountReadiness::Planned {
            plan,
            subordinate_ids,
            ..
        } => {
            output.push_str(&format!(
                "Runner user: {}\nPrimary group: {}\nHome: {}\n",
                plan.desired.username().as_str(),
                plan.desired.primary_group().as_str(),
                plan.desired.home()
            ));
            for item in &plan.items {
                let marker = match item.disposition {
                    RunnerAccountPlanDisposition::Satisfied => "READY",
                    RunnerAccountPlanDisposition::Required => "REQUIRED",
                    RunnerAccountPlanDisposition::NeedsInspection => "INSPECT",
                    RunnerAccountPlanDisposition::Blocked => "BLOCKED",
                };
                output.push_str(&format!("[{marker}] {}\n", item.summary));
                if let Some(command) = &item.command {
                    output.push_str("  Reviewed command: ");
                    output.push_str(&command.spec().displayed_argv().join(" "));
                    output.push('\n');
                }
            }

            output.push_str("\nSubordinate-ID reconciliation barriers\n");
            for item in [
                &subordinate_ids.subordinate_uids,
                &subordinate_ids.subordinate_gids,
            ] {
                let marker = match item.disposition {
                    SubordinatePlanDisposition::Satisfied => "READY",
                    SubordinatePlanDisposition::Required => "REQUIRED",
                    SubordinatePlanDisposition::NeedsInspection => "INSPECT",
                    SubordinatePlanDisposition::Blocked => "BLOCKED",
                };
                output.push_str(&format!("[{marker}] {}\n", item.summary));
                if let Some(command) = &item.command {
                    output.push_str("  Reviewed command: ");
                    output.push_str(&command.spec().displayed_argv().join(" "));
                    output.push('\n');
                }
                if let Some(barrier) = &item.fresh_observation {
                    output.push_str(&format!("  Fresh observation: {}\n", barrier.summary));
                }
            }
            match &subordinate_ids.podman_migration {
                PodmanMigrationPlan::NotRequired => {
                    output.push_str("[READY] Rootless Podman migration is unnecessary.\n");
                }
                PodmanMigrationPlan::Required { command, .. } => {
                    output.push_str(
                        "[REQUIRED] Refresh rootless Podman after fresh mapping observations.\n",
                    );
                    output.push_str("  Reviewed command: ");
                    output.push_str(&command.spec().displayed_argv().join(" "));
                    output.push('\n');
                }
                PodmanMigrationPlan::Blocked { evidence } => {
                    output.push_str("[BLOCKED] Rootless Podman migration cannot be planned yet.\n");
                    for item in evidence {
                        output.push_str(&format!("  {item}\n"));
                    }
                }
            }
        }
    }

    output.push_str("\nNo changes were made.\n");
    output
}

fn read_account_policy(
    path: &Path,
    explicitly_requested: bool,
) -> Result<Option<RunnerAccountPolicyFile>, HostReadinessError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound && !explicitly_requested => {
            return Ok(None);
        }
        Err(_) => {
            return Err(HostReadinessError::new(
                HostReadinessErrorKind::AccountPolicy,
                format!("could not open runner account policy {}", path.display()),
            ));
        }
    };
    let mut bytes = Vec::new();
    file.take((MAX_ACCOUNT_POLICY_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            HostReadinessError::new(
                HostReadinessErrorKind::AccountPolicy,
                format!("could not read runner account policy {}", path.display()),
            )
        })?;
    if bytes.len() > MAX_ACCOUNT_POLICY_BYTES {
        return Err(HostReadinessError::new(
            HostReadinessErrorKind::AccountPolicy,
            format!(
                "runner account policy {} exceeds {MAX_ACCOUNT_POLICY_BYTES} bytes",
                path.display()
            ),
        ));
    }
    let contents = String::from_utf8(bytes).map_err(|_| {
        HostReadinessError::new(
            HostReadinessErrorKind::AccountPolicy,
            format!(
                "runner account policy {} is not valid UTF-8",
                path.display()
            ),
        )
    })?;
    serde_yaml::from_str(&contents).map(Some).map_err(|_| {
        HostReadinessError::new(
            HostReadinessErrorKind::AccountPolicy,
            format!("runner account policy {} is malformed", path.display()),
        )
    })
}

fn observe_required_executables() -> Vec<ExactExecutableObservation> {
    REQUIRED_EXECUTABLES
        .into_iter()
        .map(|(name, path)| observe_executable(name, Path::new(path)))
        .collect()
}

fn observe_executable(name: &str, path: &Path) -> ExactExecutableObservation {
    match verify_executable(path) {
        Ok(verified) => ExactExecutableObservation {
            name: name.to_owned(),
            path: path.to_path_buf(),
            state: HostObservationState::Matching,
            evidence: vec![format!(
                "root-owned reviewed executable has protected mode {:04o}",
                verified.mode()
            )],
        },
        Err(error) => {
            let state = match error.kind() {
                ExecutableVerificationErrorKind::Missing => HostObservationState::Absent,
                ExecutableVerificationErrorKind::Metadata => HostObservationState::Unknown,
                ExecutableVerificationErrorKind::Symlink
                | ExecutableVerificationErrorKind::NonRegularFile
                | ExecutableVerificationErrorKind::WrongOwner
                | ExecutableVerificationErrorKind::WritableByNonOwner
                | ExecutableVerificationErrorKind::NotExecutable => {
                    HostObservationState::Conflicting
                }
            };
            ExactExecutableObservation {
                name: name.to_owned(),
                path: path.to_path_buf(),
                state,
                evidence: vec![error.message().to_owned()],
            }
        }
    }
}

fn unknown_legacy_state() -> CurrentHostState {
    CurrentHostState {
        commands: BTreeMap::new(),
        runner_user: Presence::Unknown,
        subordinate_uids: Presence::Unknown,
        subordinate_gids: Presence::Unknown,
        linger: Presence::Unknown,
        container_image: Presence::Unknown,
        runner_registration: Presence::Unknown,
    }
}

fn package_list(packages: &[crate::lane_command::PackageName]) -> String {
    packages
        .iter()
        .map(crate::lane_command::PackageName::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::debian_package_plan::{build_package_plan, parse_os_release};
    use crate::host::Presence;
    use crate::lane_command::LinuxAccountName;
    use crate::runner_account_plan::{
        DesiredRunnerAccount, PlannedSubordinateRange, PreparationObservation,
        PreparationObservationState, RunnerAccountObservations, RunnerAccountPlanDisposition,
        RunnerAccountResourceKind, build_runner_account_plan,
    };
    use crate::subordinate_id::build_exact_subordinate_id_plan;

    use super::{
        ExactExecutableObservation, HostObservationState, HostReadinessReport,
        RunnerAccountPolicyFile, RunnerAccountReadiness, SubordinateRangePolicy, render_human,
        without_subordinate_mapping_items,
    };

    fn desired() -> DesiredRunnerAccount {
        DesiredRunnerAccount::new(
            LinuxAccountName::parse("project-runner").expect("username"),
            LinuxAccountName::parse("project-runner").expect("group"),
            "/var/lib/project-runner",
            PlannedSubordinateRange::new(100_000, 65_536).expect("subuids"),
            PlannedSubordinateRange::new(200_000, 65_536).expect("subgids"),
        )
        .expect("desired account")
    }

    fn observation(state: PreparationObservationState, label: &str) -> PreparationObservation {
        PreparationObservation::new(state, [format!("observed {label}")])
            .expect("bounded observation")
    }

    fn observations(state: PreparationObservationState) -> RunnerAccountObservations {
        RunnerAccountObservations {
            group: observation(state, "group"),
            user: observation(state, "user"),
            home: observation(state, "home"),
            subordinate_uids: observation(state, "subordinate UIDs"),
            subordinate_gids: observation(state, "subordinate GIDs"),
            linger: observation(state, "linger"),
        }
    }

    fn package_plan(presence: Presence) -> crate::debian_package_plan::DebianPackagePlan {
        let distribution = parse_os_release("ID=ubuntu\nVERSION_ID=24.04\n").expect("distribution");
        let seed = build_package_plan(distribution.clone(), &BTreeMap::new()).expect("seed");
        let observed = seed
            .required_packages
            .iter()
            .map(|package| (package.as_str().to_owned(), presence))
            .collect();
        build_package_plan(distribution, &observed).expect("package plan")
    }

    fn executable(state: HostObservationState) -> ExactExecutableObservation {
        ExactExecutableObservation {
            name: "git".to_owned(),
            path: "/usr/bin/git".into(),
            state,
            evidence: vec!["deterministic executable evidence".to_owned()],
        }
    }

    fn report(
        package_presence: Presence,
        executable_state: HostObservationState,
        account_state: PreparationObservationState,
    ) -> HostReadinessReport {
        let account_observations = observations(account_state);
        let account_plan = without_subordinate_mapping_items(
            build_runner_account_plan(desired(), account_observations.clone())
                .expect("account plan"),
        );
        let subordinate_ids = build_exact_subordinate_id_plan(
            &desired(),
            &account_observations,
            Some((1001, 1001)),
            std::path::Path::new("/etc/subuid"),
            std::path::Path::new("/etc/subgid"),
        )
        .expect("subordinate plan");
        HostReadinessReport {
            schema_version: 1,
            repository: "example/project".to_owned(),
            executables: vec![executable(executable_state)],
            package_plan: package_plan(package_presence),
            runner_account: RunnerAccountReadiness::Planned {
                observations: Box::new(account_observations),
                plan: account_plan,
                subordinate_ids,
            },
        }
    }

    #[test]
    fn clean_host_is_ready_across_one_typed_report() {
        let report = report(
            Presence::Present,
            HostObservationState::Matching,
            PreparationObservationState::Matching,
        );
        let json = serde_json::to_value(&report).expect("serialize report");
        assert_eq!(json["executables"][0]["state"], "matching");
        assert_eq!(json["package_plan"]["disposition"], "ready");
        assert!(
            report
                .runner_account_plan()
                .items
                .iter()
                .all(|item| { item.disposition == RunnerAccountPlanDisposition::Satisfied })
        );
        assert!(render_human(&report).contains("[READY] ensure dedicated runner group"));
    }

    #[test]
    fn host_plan_has_one_barriered_source_for_subordinate_mutations() {
        let report = report(
            Presence::Absent,
            HostObservationState::Absent,
            PreparationObservationState::Absent,
        );
        assert!(report.runner_account_plan().items.iter().all(|item| {
            !matches!(
                item.kind,
                RunnerAccountResourceKind::SubordinateUids
                    | RunnerAccountResourceKind::SubordinateGids
            )
        }));
        let RunnerAccountReadiness::Planned {
            subordinate_ids, ..
        } = &report.runner_account
        else {
            panic!("configured test report");
        };
        assert!(subordinate_ids.subordinate_uids.fresh_observation.is_some());
        assert!(subordinate_ids.subordinate_gids.fresh_observation.is_some());
    }

    #[test]
    fn missing_host_state_remains_required() {
        let report = report(
            Presence::Absent,
            HostObservationState::Absent,
            PreparationObservationState::Absent,
        );
        assert!(
            report
                .runner_account_plan()
                .items
                .iter()
                .all(|item| { item.disposition == RunnerAccountPlanDisposition::Required })
        );
        let human = render_human(&report);
        assert!(human.contains("[REQUIRED] git at /usr/bin/git"));
        assert!(human.contains("proven absent"));
    }

    #[test]
    fn conflicting_and_partially_unknown_state_stays_blocked() {
        let mut account_observations = observations(PreparationObservationState::Absent);
        account_observations.group = observation(PreparationObservationState::Conflicting, "group");
        account_observations.user = observation(PreparationObservationState::Unknown, "user");
        let plan = without_subordinate_mapping_items(
            build_runner_account_plan(desired(), account_observations.clone())
                .expect("blocked account plan"),
        );
        assert_eq!(
            plan.items[0].disposition,
            RunnerAccountPlanDisposition::Blocked
        );
        assert!(
            plan.items[1..]
                .iter()
                .all(|item| { item.disposition == RunnerAccountPlanDisposition::Blocked })
        );
        let report = HostReadinessReport {
            schema_version: 1,
            repository: "example/project".to_owned(),
            executables: vec![executable(HostObservationState::Conflicting)],
            package_plan: package_plan(Presence::Unknown),
            runner_account: RunnerAccountReadiness::Planned {
                subordinate_ids: build_exact_subordinate_id_plan(
                    &desired(),
                    &account_observations,
                    None,
                    std::path::Path::new("/etc/subuid"),
                    std::path::Path::new("/etc/subgid"),
                )
                .expect("subordinate plan"),
                observations: Box::new(account_observations),
                plan,
            },
        };
        let json = serde_json::to_value(&report).expect("serialize report");
        assert_eq!(
            json["runner_account"]["observations"]["group"]["state"],
            "conflicting"
        );
        assert_eq!(
            json["runner_account"]["observations"]["user"]["state"],
            "unknown"
        );
        assert!(render_human(&report).contains("[BLOCKED] git at /usr/bin/git"));
    }

    #[test]
    fn malformed_policy_is_rejected_before_observation() {
        let policy = RunnerAccountPolicyFile {
            version: 1,
            primary_group: "project-runner".to_owned(),
            home: "/var/lib/../root".to_owned(),
            subordinate_uids: SubordinateRangePolicy {
                start: 100_000,
                count: 65_536,
            },
            subordinate_gids: SubordinateRangePolicy {
                start: 200_000,
                count: 65_536,
            },
        };
        let manifest = crate::manifest::parse(
            "version: 1\nrepository: example/project\nrunner:\n  scope: repository\n  user: project-runner\n  labels: [project-ci]\ncontainer:\n  image: localhost/project-ci:1\n  file: build/ci/Containerfile\nverify:\n  command: scripts/run-vps-verification.sh\n  suites:\n    full: full\nlimits:\n  memory: 2GiB\n  cpus: 1.5\n  pids: 768\ntrust:\n  forks: deny\n  trigger: operator\n",
        )
        .expect("manifest");
        policy.desired_account(&manifest).expect_err("aliased home");
    }

    impl HostReadinessReport {
        fn runner_account_plan(&self) -> &crate::runner_account_plan::RunnerAccountPlan {
            match &self.runner_account {
                RunnerAccountReadiness::Planned { plan, .. } => plan,
                RunnerAccountReadiness::NeedsConfiguration { .. } => {
                    panic!("configured test report")
                }
            }
        }
    }
}
