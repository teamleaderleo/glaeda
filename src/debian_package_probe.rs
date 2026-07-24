use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;

use serde::Serialize;

use crate::host::Presence;
use crate::lane_command::PackageName;
use crate::process::{CommandExecutor, CommandSpec, ExecutionRecord};

const DPKG_QUERY: &str = "/usr/bin/dpkg-query";
const DPKG_QUERY_FORMAT: &str = "${binary:Package}\\t${db:Status-Status}\\n";
const MAX_INVENTORY_RECORDS: usize = 100_000;
const MAX_INVENTORY_LINE_BYTES: usize = 512;

/// Build the one reviewed command used to inspect the complete dpkg package inventory.
#[must_use]
pub fn dpkg_inventory_command() -> CommandSpec {
    CommandSpec::new(DPKG_QUERY)
        .argument("--show")
        .argument(format!("--showformat={DPKG_QUERY_FORMAT}"))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DebianPackageObservation {
    command: CommandSpec,
    receipt: ExecutionRecord,
    packages: BTreeMap<String, Presence>,
}

impl DebianPackageObservation {
    #[must_use]
    pub fn command(&self) -> &CommandSpec {
        &self.command
    }

    #[must_use]
    pub fn receipt(&self) -> &ExecutionRecord {
        &self.receipt
    }

    #[must_use]
    pub fn packages(&self) -> &BTreeMap<String, Presence> {
        &self.packages
    }

    #[must_use]
    pub fn into_packages(self) -> BTreeMap<String, Presence> {
        self.packages
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DebianPackageProbeErrorKind {
    InvalidRequest,
    Execution,
    UnexpectedReceipt,
    InvalidOutput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DebianPackageProbeError {
    kind: DebianPackageProbeErrorKind,
    public_message: String,
}

impl DebianPackageProbeError {
    #[must_use]
    pub fn kind(&self) -> DebianPackageProbeErrorKind {
        self.kind
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.public_message
    }

    fn new(kind: DebianPackageProbeErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            public_message: message.into(),
        }
    }
}

impl fmt::Display for DebianPackageProbeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.public_message)
    }
}

impl std::error::Error for DebianPackageProbeError {}

/// Execute one bounded, read-only package inventory query and classify requested packages.
#[derive(Debug, Clone)]
pub struct DpkgQueryProbe<E> {
    executor: E,
}

impl<E> DpkgQueryProbe<E> {
    #[must_use]
    pub const fn new(executor: E) -> Self {
        Self { executor }
    }
}

impl<E: CommandExecutor> DpkgQueryProbe<E> {
    /// Inspect validated package names without invoking a shell or inheriting an environment.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for an empty or duplicate request, process failure, a receipt that
    /// does not match the reviewed command, or malformed and unbounded dpkg output.
    pub fn observe(
        &self,
        packages: &[PackageName],
    ) -> Result<DebianPackageObservation, DebianPackageProbeError> {
        validate_request(packages)?;
        let command = dpkg_inventory_command();
        let receipt = self.executor.execute(&command).map_err(map_execution_error)?;
        let observed = decode_inventory(&command, &receipt, packages)?;
        Ok(DebianPackageObservation {
            command,
            receipt,
            packages: observed,
        })
    }
}

fn validate_request(packages: &[PackageName]) -> Result<(), DebianPackageProbeError> {
    if packages.is_empty() {
        return Err(DebianPackageProbeError::new(
            DebianPackageProbeErrorKind::InvalidRequest,
            "package observation requires at least one validated package name",
        ));
    }
    let mut unique = BTreeSet::new();
    for package in packages {
        if !unique.insert(package.as_str()) {
            return Err(DebianPackageProbeError::new(
                DebianPackageProbeErrorKind::InvalidRequest,
                format!("package observation contains duplicate package {:?}", package.as_str()),
            ));
        }
    }
    Ok(())
}

fn map_execution_error(_error: io::Error) -> DebianPackageProbeError {
    DebianPackageProbeError::new(
        DebianPackageProbeErrorKind::Execution,
        "could not execute the bounded dpkg package inventory query",
    )
}

fn decode_inventory(
    command: &CommandSpec,
    receipt: &ExecutionRecord,
    requested: &[PackageName],
) -> Result<BTreeMap<String, Presence>, DebianPackageProbeError> {
    if receipt.argv != command.displayed_argv() || !receipt.environment_keys.is_empty() {
        return Err(DebianPackageProbeError::new(
            DebianPackageProbeErrorKind::UnexpectedReceipt,
            "dpkg package receipt does not match the reviewed command boundary",
        ));
    }
    if receipt.status != Some(0) || !receipt.success || !receipt.stderr.is_empty() {
        return Err(DebianPackageProbeError::new(
            DebianPackageProbeErrorKind::UnexpectedReceipt,
            "dpkg package inventory did not complete cleanly",
        ));
    }
    if receipt.stdout.is_empty() || !receipt.stdout.ends_with('\n') || receipt.stdout.contains('\0') {
        return Err(DebianPackageProbeError::new(
            DebianPackageProbeErrorKind::InvalidOutput,
            "dpkg package inventory is empty, truncated, or contains a NUL byte",
        ));
    }

    let requested_names = requested
        .iter()
        .map(PackageName::as_str)
        .collect::<BTreeSet<_>>();
    let mut relevant = BTreeMap::<String, Presence>::new();
    let mut records = 0usize;
    for line in receipt.stdout.lines() {
        records = records.checked_add(1).ok_or_else(|| {
            DebianPackageProbeError::new(
                DebianPackageProbeErrorKind::InvalidOutput,
                "dpkg package inventory record count overflowed",
            )
        })?;
        if records > MAX_INVENTORY_RECORDS {
            return Err(DebianPackageProbeError::new(
                DebianPackageProbeErrorKind::InvalidOutput,
                format!(
                    "dpkg package inventory exceeds {MAX_INVENTORY_RECORDS} records"
                ),
            ));
        }
        if line.is_empty() || line.len() > MAX_INVENTORY_LINE_BYTES {
            return Err(DebianPackageProbeError::new(
                DebianPackageProbeErrorKind::InvalidOutput,
                "dpkg package inventory contains an empty or oversized record",
            ));
        }
        let Some((package, status)) = line.split_once('\t') else {
            return Err(DebianPackageProbeError::new(
                DebianPackageProbeErrorKind::InvalidOutput,
                "dpkg package inventory record is missing its field separator",
            ));
        };
        if status.contains('\t')
            || !valid_inventory_package(package)
            || !valid_inventory_status(status)
        {
            return Err(DebianPackageProbeError::new(
                DebianPackageProbeErrorKind::InvalidOutput,
                "dpkg package inventory contains an invalid package or status field",
            ));
        }
        if requested_names.contains(package) {
            let presence = classify_status(status);
            if relevant.insert(package.to_owned(), presence).is_some() {
                return Err(DebianPackageProbeError::new(
                    DebianPackageProbeErrorKind::InvalidOutput,
                    format!("dpkg package inventory repeats requested package {package:?}"),
                ));
            }
        }
    }

    Ok(requested
        .iter()
        .map(|package| {
            let presence = relevant
                .get(package.as_str())
                .copied()
                .unwrap_or(Presence::Absent);
            (package.as_str().to_owned(), presence)
        })
        .collect())
}

fn valid_inventory_package(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'+' | b'.' | b'-' | b':')
        })
}

fn valid_inventory_status(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'-')
}

fn classify_status(status: &str) -> Presence {
    match status {
        "installed" => Presence::Present,
        "not-installed" | "config-files" => Presence::Absent,
        _ => Presence::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::io;

    use crate::host::Presence;
    use crate::lane_command::PackageName;
    use crate::process::{CommandExecutor, CommandSpec, ExecutionRecord};

    use super::{
        DPKG_QUERY, DPKG_QUERY_FORMAT, DebianPackageProbeErrorKind, DpkgQueryProbe,
        dpkg_inventory_command,
    };

    struct FakeExecutor {
        calls: RefCell<Vec<CommandSpec>>,
        result: RefCell<Option<io::Result<ExecutionRecord>>>,
    }

    impl FakeExecutor {
        fn returning(record: ExecutionRecord) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                result: RefCell::new(Some(Ok(record))),
            }
        }
    }

    impl CommandExecutor for FakeExecutor {
        fn execute(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord> {
            self.calls.borrow_mut().push(spec.clone());
            self.result
                .borrow_mut()
                .take()
                .expect("one configured fake result")
        }
    }

    fn package(name: &str) -> PackageName {
        PackageName::parse(name).expect("valid package name")
    }

    fn receipt(stdout: &str) -> ExecutionRecord {
        let command = dpkg_inventory_command();
        ExecutionRecord {
            argv: command.displayed_argv(),
            environment_keys: Vec::new(),
            status: Some(0),
            success: true,
            stdout: stdout.to_owned(),
            stderr: String::new(),
        }
    }

    #[test]
    fn reviewed_command_is_absolute_shell_free_and_environment_free() {
        let command = dpkg_inventory_command();
        assert_eq!(command.program.to_str(), Some(DPKG_QUERY));
        assert_eq!(
            command.displayed_argv(),
            [
                DPKG_QUERY,
                "--show",
                &format!("--showformat={DPKG_QUERY_FORMAT}"),
            ]
        );
        assert!(command.environment.is_empty());
    }

    #[test]
    fn clean_inventory_distinguishes_present_absent_and_transitional_state() {
        let executor = FakeExecutor::returning(receipt(
            "git\tinstalled\npodman\tconfig-files\nuidmap\tunpacked\nbase-files\tinstalled\n",
        ));
        let probe = DpkgQueryProbe::new(executor);
        let observation = probe
            .observe(&[package("git"), package("podman"), package("uidmap"), package("slirp4netns")])
            .expect("package observation");

        assert_eq!(observation.packages()["git"], Presence::Present);
        assert_eq!(observation.packages()["podman"], Presence::Absent);
        assert_eq!(observation.packages()["uidmap"], Presence::Unknown);
        assert_eq!(observation.packages()["slirp4netns"], Presence::Absent);
        assert_eq!(observation.receipt().status, Some(0));
    }

    #[test]
    fn nonzero_status_stderr_or_mismatched_command_fails_closed() {
        let mut failed = receipt("git\tinstalled\n");
        failed.status = Some(1);
        failed.success = false;
        let error = DpkgQueryProbe::new(FakeExecutor::returning(failed))
            .observe(&[package("git")])
            .expect_err("nonzero status");
        assert_eq!(error.kind(), DebianPackageProbeErrorKind::UnexpectedReceipt);

        let mut warning = receipt("git\tinstalled\n");
        warning.stderr = "unexpected warning".to_owned();
        DpkgQueryProbe::new(FakeExecutor::returning(warning))
            .observe(&[package("git")])
            .expect_err("stderr must fail closed");

        let mut mismatched = receipt("git\tinstalled\n");
        mismatched.argv.push("extra".to_owned());
        DpkgQueryProbe::new(FakeExecutor::returning(mismatched))
            .observe(&[package("git")])
            .expect_err("mismatched command");
    }

    #[test]
    fn malformed_duplicate_and_truncated_inventory_is_rejected() {
        for output in [
            "git installed\n",
            "git\tinstalled\ngit\tinstalled\n",
            "git\tinstalled",
            "git\tinstalled\ninvalid package\tinstalled\n",
        ] {
            DpkgQueryProbe::new(FakeExecutor::returning(receipt(output)))
                .observe(&[package("git")])
                .expect_err("invalid inventory must fail");
        }
    }

    #[test]
    fn empty_and_duplicate_requests_fail_before_execution() {
        let executor = FakeExecutor::returning(receipt("git\tinstalled\n"));
        let probe = DpkgQueryProbe::new(executor);
        let error = probe.observe(&[]).expect_err("empty request");
        assert_eq!(error.kind(), DebianPackageProbeErrorKind::InvalidRequest);
        assert!(probe.executor.calls.borrow().is_empty());

        let executor = FakeExecutor::returning(receipt("git\tinstalled\n"));
        let probe = DpkgQueryProbe::new(executor);
        probe
            .observe(&[package("git"), package("git")])
            .expect_err("duplicate request");
        assert!(probe.executor.calls.borrow().is_empty());
    }
}
