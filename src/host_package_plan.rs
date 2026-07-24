use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::{Read as _, Take};
use std::path::Path;

use serde::Serialize;

use crate::debian_package_plan::{
    DebianPackagePlan, DebianPackagePlanError, MAX_OS_RELEASE_BYTES, PackagePlanDisposition,
    build_package_plan, parse_os_release,
};
use crate::debian_package_probe::{DebianPackageProbeError, DpkgQueryProbe};
use crate::host::{
    CurrentHostState, DesiredHostState, HostAction, HostPlan, HostProbe, LinuxFilesystemProbe,
    build_plan as build_host_plan,
};
use crate::manifest::Manifest;
use crate::process::CommandExecutor;

pub const HOST_PACKAGE_PLAN_SCHEMA_VERSION: u8 = 1;
pub const DEFAULT_OS_RELEASE_PATH: &str = "/etc/os-release";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostPackagePlan {
    pub schema_version: u8,
    pub desired: DesiredHostState,
    pub current: CurrentHostState,
    pub actions: Vec<HostAction>,
    pub package_plan: DebianPackagePlan,
}

impl HostPackagePlan {
    #[must_use]
    pub fn new(host: HostPlan, package_plan: DebianPackagePlan) -> Self {
        Self {
            schema_version: HOST_PACKAGE_PLAN_SCHEMA_VERSION,
            desired: host.desired,
            current: host.current,
            actions: host.actions,
            package_plan,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostPackagePlanErrorKind {
    HostProbe,
    OsRelease,
    Distribution,
    PackagePlan,
    PackageProbe,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostPackagePlanError {
    kind: HostPackagePlanErrorKind,
    public_message: String,
}

impl HostPackagePlanError {
    #[must_use]
    pub fn kind(&self) -> HostPackagePlanErrorKind {
        self.kind
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.public_message
    }

    fn new(kind: HostPackagePlanErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            public_message: message.into(),
        }
    }
}

impl fmt::Display for HostPackagePlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.public_message)
    }
}

impl std::error::Error for HostPackagePlanError {}

/// Inspect the existing host plan and Debian-family package state without making changes.
///
/// # Errors
///
/// Returns a bounded error when host observation, bounded `os-release` reading, distribution
/// validation, package-plan construction, or the exact dpkg inventory probe fails.
pub fn inspect_host_package_plan(
    manifest: &Manifest,
    os_release_path: impl AsRef<Path>,
    executor: &impl CommandExecutor,
) -> Result<HostPackagePlan, HostPackagePlanError> {
    let current = LinuxFilesystemProbe.inspect(manifest).map_err(|_| {
        HostPackagePlanError::new(
            HostPackagePlanErrorKind::HostProbe,
            "failed to inspect bounded host state",
        )
    })?;
    let host = build_host_plan(manifest, current);
    let os_release = read_bounded(os_release_path.as_ref())?;
    let distribution = parse_os_release(&os_release).map_err(|_| {
        HostPackagePlanError::new(
            HostPackagePlanErrorKind::Distribution,
            "host distribution is not a supported, valid Debian or Ubuntu identity",
        )
    })?;

    let seed = build_package_plan(distribution.clone(), &BTreeMap::new())
        .map_err(map_package_plan_error)?;
    let observation = DpkgQueryProbe::new(executor)
        .observe(&seed.required_packages)
        .map_err(map_package_probe_error)?;
    let package_plan = build_package_plan(distribution, observation.packages())
        .map_err(map_package_plan_error)?;

    Ok(HostPackagePlan::new(host, package_plan))
}

#[must_use]
pub fn render_human(plan: &HostPackagePlan) -> String {
    let mut output = format!(
        "SmolRunner host plan\n\nRepository: {}\nRunner user: {}\n\n",
        plan.desired.repository, plan.desired.runner_user
    );

    if plan.actions.is_empty() {
        output.push_str("The inspected host state already matches the desired non-package state.\n");
    } else {
        for action in &plan.actions {
            let marker = match action.disposition {
                crate::host::HostActionDisposition::Required => "REQUIRED",
                crate::host::HostActionDisposition::NeedsInspection => "INSPECT",
            };
            output.push_str(&format!("[{marker}] {}\n", action.summary));
        }
    }

    output.push_str("\nDebian package preparation\n");
    output.push_str(&format!(
        "Distribution: {} {}\n",
        plan.package_plan.distribution.id(),
        plan.package_plan.distribution.version_id()
    ));
    match plan.package_plan.disposition {
        PackagePlanDisposition::Ready => {
            output.push_str("All reviewed prerequisite packages are present.\n");
        }
        PackagePlanDisposition::NeedsInspection => {
            output.push_str("Package mutation is blocked until these packages are inspected: ");
            output.push_str(&package_list(&plan.package_plan.unknown_packages));
            output.push('\n');
        }
        PackagePlanDisposition::Required => {
            output.push_str("The following packages are proven absent: ");
            output.push_str(&package_list(&plan.package_plan.missing_packages));
            output.push('\n');
            if let Some(command) = &plan.package_plan.command {
                output.push_str("Reviewed command: ");
                output.push_str(&command.spec().displayed_argv().join(" "));
                output.push('\n');
            }
            output.push_str("Rollback class: compensating\n");
        }
    }

    output.push_str("\nNo changes were made.\n");
    output
}

fn read_bounded(path: &Path) -> Result<String, HostPackagePlanError> {
    let file = File::open(path).map_err(|_| {
        HostPackagePlanError::new(
            HostPackagePlanErrorKind::OsRelease,
            "could not open the host os-release file",
        )
    })?;
    let mut bytes = Vec::new();
    Take::new(file, (MAX_OS_RELEASE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            HostPackagePlanError::new(
                HostPackagePlanErrorKind::OsRelease,
                "could not read the bounded host os-release file",
            )
        })?;
    if bytes.len() > MAX_OS_RELEASE_BYTES {
        return Err(HostPackagePlanError::new(
            HostPackagePlanErrorKind::OsRelease,
            format!("host os-release exceeds {MAX_OS_RELEASE_BYTES} bytes"),
        ));
    }
    String::from_utf8(bytes).map_err(|_| {
        HostPackagePlanError::new(
            HostPackagePlanErrorKind::OsRelease,
            "host os-release is not valid UTF-8",
        )
    })
}

fn map_package_plan_error(_error: DebianPackagePlanError) -> HostPackagePlanError {
    HostPackagePlanError::new(
        HostPackagePlanErrorKind::PackagePlan,
        "could not construct the validated Debian package plan",
    )
}

fn map_package_probe_error(_error: DebianPackageProbeError) -> HostPackagePlanError {
    HostPackagePlanError::new(
        HostPackagePlanErrorKind::PackageProbe,
        "could not inspect the bounded Debian package inventory",
    )
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
    use std::cell::RefCell;
    use std::fs;
    use std::io;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::manifest::parse;
    use crate::process::{CommandExecutor, CommandSpec, ExecutionRecord};

    use super::{
        HostPackagePlanErrorKind, MAX_OS_RELEASE_BYTES, inspect_host_package_plan, render_human,
    };

    static NEXT_FILE: AtomicU64 = AtomicU64::new(1);

    const MANIFEST: &str = r#"
version: 1
repository: example/project
runner:
  scope: repository
  user: project-runner
  labels: [project-ci]
container:
  image: localhost/project-ci:1
  file: build/ci/Containerfile
verify:
  command: scripts/run-vps-verification.sh
  suites:
    full: full
limits:
  memory: 2GiB
  cpus: 1.5
  pids: 768
trust:
  forks: deny
  trigger: operator
"#;

    struct FakeExecutor {
        calls: RefCell<Vec<CommandSpec>>,
        receipt: RefCell<Option<ExecutionRecord>>,
    }

    impl FakeExecutor {
        fn returning(stdout: &str) -> Self {
            let command = crate::debian_package_probe::dpkg_inventory_command();
            Self {
                calls: RefCell::new(Vec::new()),
                receipt: RefCell::new(Some(ExecutionRecord {
                    argv: command.displayed_argv(),
                    environment_keys: Vec::new(),
                    status: Some(0),
                    success: true,
                    stdout: stdout.to_owned(),
                    stderr: String::new(),
                })),
            }
        }
    }

    impl CommandExecutor for FakeExecutor {
        fn execute(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord> {
            self.calls.borrow_mut().push(spec.clone());
            Ok(self
                .receipt
                .borrow_mut()
                .take()
                .expect("one fake receipt"))
        }
    }

    fn temporary_os_release(contents: &[u8]) -> TemporaryFile {
        let path = std::env::temp_dir().join(format!(
            "smolrunner-os-release-{}-{}",
            std::process::id(),
            NEXT_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&path, contents).expect("write temporary os-release");
        TemporaryFile(path)
    }

    struct TemporaryFile(PathBuf);

    impl Drop for TemporaryFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    #[test]
    fn package_plan_is_added_without_changing_top_level_host_actions() {
        let manifest = parse(MANIFEST).expect("manifest");
        let os_release = temporary_os_release(b"ID=ubuntu\nVERSION_ID=24.04\n");
        let executor = FakeExecutor::returning(
            "git\tinstalled\npodman\tnot-installed\nuidmap\tinstalled\nslirp4netns\tinstalled\nfuse-overlayfs\tinstalled\ndbus-user-session\tinstalled\n",
        );

        let report = inspect_host_package_plan(&manifest, &os_release.0, &executor)
            .expect("integrated host package plan");
        assert!(report.actions.len() >= 2);
        assert_eq!(
            report.package_plan.disposition,
            crate::debian_package_plan::PackagePlanDisposition::Required
        );
        assert_eq!(
            report.package_plan.missing_packages[0].as_str(),
            "podman"
        );
        assert_eq!(executor.calls.borrow().len(), 1);

        let json = serde_json::to_value(&report).expect("serialize report");
        assert!(json.get("actions").is_some());
        assert_eq!(json["package_plan"]["disposition"], "required");
        let human = render_human(&report);
        assert!(human.contains("Debian package preparation"));
        assert_eq!(human.matches("No changes were made.").count(), 1);
    }

    #[test]
    fn residual_configuration_remains_inspection_only_in_integrated_output() {
        let manifest = parse(MANIFEST).expect("manifest");
        let os_release = temporary_os_release(b"ID=debian\nVERSION_ID=12\n");
        let executor = FakeExecutor::returning(
            "git\tinstalled\npodman\tconfig-files\nuidmap\tinstalled\nslirp4netns\tinstalled\nfuse-overlayfs\tinstalled\ndbus-user-session\tinstalled\n",
        );
        let report = inspect_host_package_plan(&manifest, &os_release.0, &executor)
            .expect("integrated host package plan");
        assert_eq!(
            report.package_plan.disposition,
            crate::debian_package_plan::PackagePlanDisposition::NeedsInspection
        );
        assert!(report.package_plan.command.is_none());
    }

    #[test]
    fn oversized_os_release_fails_before_package_execution() {
        let manifest = parse(MANIFEST).expect("manifest");
        let os_release = temporary_os_release(&vec![b'x'; MAX_OS_RELEASE_BYTES + 1]);
        let executor = FakeExecutor::returning("git\tinstalled\n");
        let error = inspect_host_package_plan(&manifest, &os_release.0, &executor)
            .expect_err("oversized os-release");
        assert_eq!(error.kind(), HostPackagePlanErrorKind::OsRelease);
        assert!(executor.calls.borrow().is_empty());
    }
}
