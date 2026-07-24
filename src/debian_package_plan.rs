use std::collections::BTreeMap;
use std::fmt;

use serde::Serialize;

use crate::host::Presence;
use crate::journal::{ExecutionLane, PlannedMutation, Preconditions, RollbackClass};
use crate::lane_command::{LaneCommand, LaneCommandError, PackageName};

pub const DEBIAN_PACKAGE_PLAN_SCHEMA_VERSION: u8 = 1;
pub const MAX_OS_RELEASE_BYTES: usize = 65_536;

const REQUIRED_PACKAGE_NAMES: [&str; 6] = [
    "git",
    "podman",
    "uidmap",
    "slirp4netns",
    "fuse-overlayfs",
    "dbus-user-session",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DebianFamilyDistribution {
    Debian,
    Ubuntu,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DistributionIdentity {
    pub distribution: DebianFamilyDistribution,
    pub id: String,
    pub version_id: String,
}

/// Parse the bounded distribution identity needed for Debian-family package planning.
///
/// This is intentionally not a shell parser. It accepts only plain or simply quoted values for the
/// exact `ID` and `VERSION_ID` fields, rejects duplicates, and ignores unrelated fields.
///
/// # Errors
///
/// Returns an error for oversized input, malformed required fields, duplicate required fields,
/// missing fields, or distributions other than exact Debian and Ubuntu IDs.
pub fn parse_os_release(input: &str) -> Result<DistributionIdentity, DebianPackagePlanError> {
    if input.len() > MAX_OS_RELEASE_BYTES {
        return Err(DebianPackagePlanError::single(format!(
            "os-release input exceeds {MAX_OS_RELEASE_BYTES} bytes"
        )));
    }
    if input.chars().any(|character| character == '\0') {
        return Err(DebianPackagePlanError::single(
            "os-release input contains a NUL byte",
        ));
    }

    let mut id = None::<String>;
    let mut version_id = None::<String>;
    for (index, raw_line) in input.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, raw_value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "ID" => set_once(
                &mut id,
                decode_os_release_value("ID", raw_value, index + 1)?,
                "ID",
            )?,
            "VERSION_ID" => set_once(
                &mut version_id,
                decode_os_release_value("VERSION_ID", raw_value, index + 1)?,
                "VERSION_ID",
            )?,
            _ => {}
        }
    }

    let id = id.ok_or_else(|| DebianPackagePlanError::single("os-release is missing ID"))?;
    let version_id = version_id
        .ok_or_else(|| DebianPackagePlanError::single("os-release is missing VERSION_ID"))?;
    validate_identity_value("ID", &id)?;
    validate_identity_value("VERSION_ID", &version_id)?;

    let distribution = match id.as_str() {
        "debian" => DebianFamilyDistribution::Debian,
        "ubuntu" => DebianFamilyDistribution::Ubuntu,
        unsupported => {
            return Err(DebianPackagePlanError::single(format!(
                "distribution ID {unsupported:?} is not supported for Debian package planning"
            )));
        }
    };

    Ok(DistributionIdentity {
        distribution,
        id,
        version_id,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackagePlanDisposition {
    Ready,
    Required,
    NeedsInspection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DebianPackagePlan {
    pub schema_version: u8,
    pub distribution: DistributionIdentity,
    pub disposition: PackagePlanDisposition,
    pub required_packages: Vec<PackageName>,
    pub present_packages: Vec<PackageName>,
    pub missing_packages: Vec<PackageName>,
    pub unknown_packages: Vec<PackageName>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mutation: Option<PlannedMutation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<LaneCommand>,
}

/// Build a conservative package preparation plan from fixed package identities and observations.
///
/// Missing observation keys are treated as unknown. Any unknown required package blocks mutation,
/// even when another package is proven absent. Only a fully observed inventory may produce the
/// reviewed root-lane `apt-get install` command.
///
/// # Errors
///
/// Returns an error only when the fixed reviewed package bundle or command cannot be constructed.
pub fn build_package_plan(
    distribution: DistributionIdentity,
    observed: &BTreeMap<String, Presence>,
) -> Result<DebianPackagePlan, DebianPackagePlanError> {
    let required_packages = required_packages()?;
    let mut present_packages = Vec::new();
    let mut missing_packages = Vec::new();
    let mut unknown_packages = Vec::new();

    for package in &required_packages {
        match observed
            .get(package.as_str())
            .copied()
            .unwrap_or(Presence::Unknown)
        {
            Presence::Present => present_packages.push(package.clone()),
            Presence::Absent => missing_packages.push(package.clone()),
            Presence::Unknown => unknown_packages.push(package.clone()),
        }
    }

    let (disposition, mutation, command) = if !unknown_packages.is_empty() {
        (PackagePlanDisposition::NeedsInspection, None, None)
    } else if missing_packages.is_empty() {
        (PackagePlanDisposition::Ready, None, None)
    } else {
        let evidence = std::iter::once(format!(
            "os-release ID={} VERSION_ID={}",
            distribution.id, distribution.version_id
        ))
        .chain(required_packages.iter().map(|package| {
            let presence = observed
                .get(package.as_str())
                .copied()
                .unwrap_or(Presence::Unknown);
            format!("package {} observed {presence:?}", package.as_str())
        }))
        .collect::<Vec<_>>();
        let summary = format!(
            "install Debian-family host prerequisites: {}",
            missing_packages
                .iter()
                .map(PackageName::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        );
        let mutation = PlannedMutation::new(
            "install-debian-host-prerequisites",
            ExecutionLane::Root,
            summary,
            RollbackClass::Compensating,
            Preconditions::new(evidence),
        );
        let command = LaneCommand::apt_install(&mutation, &missing_packages)?;
        (
            PackagePlanDisposition::Required,
            Some(mutation),
            Some(command),
        )
    };

    Ok(DebianPackagePlan {
        schema_version: DEBIAN_PACKAGE_PLAN_SCHEMA_VERSION,
        distribution,
        disposition,
        required_packages,
        present_packages,
        missing_packages,
        unknown_packages,
        mutation,
        command,
    })
}

#[must_use]
pub fn render_human(plan: &DebianPackagePlan) -> String {
    let mut output = format!(
        "SmolRunner Debian package plan\n\nDistribution: {} {}\n",
        plan.distribution.id, plan.distribution.version_id
    );
    match plan.disposition {
        PackagePlanDisposition::Ready => {
            output.push_str("All reviewed prerequisite packages are present.\n");
        }
        PackagePlanDisposition::NeedsInspection => {
            output.push_str("Package mutation is blocked until these packages are inspected: ");
            output.push_str(&package_list(&plan.unknown_packages));
            output.push('\n');
        }
        PackagePlanDisposition::Required => {
            output.push_str("The following packages are proven absent: ");
            output.push_str(&package_list(&plan.missing_packages));
            output.push('\n');
            if let Some(command) = &plan.command {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DebianPackagePlanError {
    pub problems: Vec<String>,
}

impl DebianPackagePlanError {
    fn single(problem: impl Into<String>) -> Self {
        Self {
            problems: vec![problem.into()],
        }
    }
}

impl From<LaneCommandError> for DebianPackagePlanError {
    fn from(error: LaneCommandError) -> Self {
        Self {
            problems: error.problems,
        }
    }
}

impl fmt::Display for DebianPackagePlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "Debian package plan validation failed")?;
        for problem in &self.problems {
            writeln!(formatter, "- {problem}")?;
        }
        Ok(())
    }
}

impl std::error::Error for DebianPackagePlanError {}

fn required_packages() -> Result<Vec<PackageName>, DebianPackagePlanError> {
    REQUIRED_PACKAGE_NAMES
        .into_iter()
        .map(PackageName::parse)
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn set_once(
    target: &mut Option<String>,
    value: String,
    field: &str,
) -> Result<(), DebianPackagePlanError> {
    if target.replace(value).is_some() {
        Err(DebianPackagePlanError::single(format!(
            "os-release contains duplicate {field}"
        )))
    } else {
        Ok(())
    }
}

fn decode_os_release_value(
    field: &str,
    raw_value: &str,
    line: usize,
) -> Result<String, DebianPackagePlanError> {
    if raw_value.is_empty() {
        return Err(DebianPackagePlanError::single(format!(
            "os-release {field} is empty on line {line}"
        )));
    }
    let bytes = raw_value.as_bytes();
    let value = match bytes.first().copied() {
        Some(b'\'' | b'"') => {
            let quote = bytes[0];
            if bytes.len() < 2 || bytes.last().copied() != Some(quote) {
                return Err(DebianPackagePlanError::single(format!(
                    "os-release {field} has malformed quoting on line {line}"
                )));
            }
            let inner = &raw_value[1..raw_value.len() - 1];
            if inner.bytes().any(|byte| byte == quote || byte == b'\\') {
                return Err(DebianPackagePlanError::single(format!(
                    "os-release {field} uses unsupported quoting or escapes on line {line}"
                )));
            }
            inner
        }
        Some(_) => {
            if raw_value
                .bytes()
                .any(|byte| byte.is_ascii_whitespace() || matches!(byte, b'\'' | b'"' | b'\\'))
            {
                return Err(DebianPackagePlanError::single(format!(
                    "os-release {field} must be plain or simply quoted on line {line}"
                )));
            }
            raw_value
        }
        None => unreachable!("empty value handled above"),
    };
    Ok(value.to_owned())
}

fn validate_identity_value(field: &str, value: &str) -> Result<(), DebianPackagePlanError> {
    if value.is_empty()
        || value.len() > 64
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
    {
        return Err(DebianPackagePlanError::single(format!(
            "os-release {field} must be 1 to 64 lowercase ASCII letters, digits, '.', '_', or '-'"
        )));
    }
    Ok(())
}

fn package_list(packages: &[PackageName]) -> String {
    packages
        .iter()
        .map(PackageName::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::host::Presence;
    use crate::journal::{ExecutionLane, RollbackClass};

    use super::{
        DebianFamilyDistribution, PackagePlanDisposition, REQUIRED_PACKAGE_NAMES,
        build_package_plan, parse_os_release, render_human,
    };

    fn ubuntu() -> super::DistributionIdentity {
        parse_os_release("NAME=Ubuntu\nID=ubuntu\nVERSION_ID=\"24.04\"\n").expect("Ubuntu identity")
    }

    fn inventory(default: Presence) -> BTreeMap<String, Presence> {
        REQUIRED_PACKAGE_NAMES
            .into_iter()
            .map(|package| (package.to_owned(), default))
            .collect()
    }

    #[test]
    fn exact_debian_and_ubuntu_identities_are_supported() {
        let debian = parse_os_release("ID=debian\nVERSION_ID='12'\n").expect("Debian identity");
        assert_eq!(debian.distribution, DebianFamilyDistribution::Debian);
        assert_eq!(debian.version_id, "12");

        let ubuntu = ubuntu();
        assert_eq!(ubuntu.distribution, DebianFamilyDistribution::Ubuntu);
        assert_eq!(ubuntu.version_id, "24.04");
    }

    #[test]
    fn malformed_unsupported_and_oversized_identity_fails_closed() {
        parse_os_release("ID=fedora\nVERSION_ID=42\n").expect_err("unsupported distribution");
        parse_os_release("ID=ubuntu\nID=ubuntu\nVERSION_ID=24.04\n").expect_err("duplicate ID");
        parse_os_release("ID=ubuntu\nVERSION_ID=\"24.04\n").expect_err("malformed quote");
        parse_os_release(&"x".repeat(super::MAX_OS_RELEASE_BYTES + 1))
            .expect_err("oversized identity");
    }

    #[test]
    fn fully_present_inventory_is_ready_without_a_mutation() {
        let plan = build_package_plan(ubuntu(), &inventory(Presence::Present)).expect("plan");
        assert_eq!(plan.disposition, PackagePlanDisposition::Ready);
        assert!(plan.missing_packages.is_empty());
        assert!(plan.unknown_packages.is_empty());
        assert!(plan.mutation.is_none());
        assert!(plan.command.is_none());
    }

    #[test]
    fn unknown_package_state_blocks_even_proven_absence() {
        let mut observed = inventory(Presence::Present);
        observed.insert("podman".to_owned(), Presence::Absent);
        observed.insert("uidmap".to_owned(), Presence::Unknown);
        let plan = build_package_plan(ubuntu(), &observed).expect("plan");
        assert_eq!(plan.disposition, PackagePlanDisposition::NeedsInspection);
        assert_eq!(
            plan.missing_packages
                .iter()
                .map(crate::lane_command::PackageName::as_str)
                .collect::<Vec<_>>(),
            ["podman"]
        );
        assert_eq!(
            plan.unknown_packages
                .iter()
                .map(crate::lane_command::PackageName::as_str)
                .collect::<Vec<_>>(),
            ["uidmap"]
        );
        assert!(plan.mutation.is_none());
        assert!(plan.command.is_none());
    }

    #[test]
    fn proven_absence_builds_one_compensating_root_command() {
        let mut observed = inventory(Presence::Present);
        observed.insert("podman".to_owned(), Presence::Absent);
        observed.insert("uidmap".to_owned(), Presence::Absent);
        let plan = build_package_plan(ubuntu(), &observed).expect("plan");
        assert_eq!(plan.disposition, PackagePlanDisposition::Required);
        let mutation = plan.mutation.as_ref().expect("mutation");
        assert_eq!(mutation.lane, ExecutionLane::Root);
        assert_eq!(mutation.rollback, RollbackClass::Compensating);
        assert!(
            mutation
                .preconditions
                .evidence
                .iter()
                .any(|item| item == "os-release ID=ubuntu VERSION_ID=24.04")
        );
        assert_eq!(
            plan.command
                .as_ref()
                .expect("command")
                .spec()
                .displayed_argv(),
            [
                "/usr/bin/apt-get",
                "install",
                "--yes",
                "--no-install-recommends",
                "podman",
                "uidmap",
            ]
        );
        let human = render_human(&plan);
        assert!(human.contains("Rollback class: compensating"));
        assert!(human.ends_with("No changes were made.\n"));
    }

    #[test]
    fn missing_inventory_keys_are_unknown() {
        let plan = build_package_plan(ubuntu(), &BTreeMap::new()).expect("plan");
        assert_eq!(plan.disposition, PackagePlanDisposition::NeedsInspection);
        assert_eq!(plan.unknown_packages.len(), REQUIRED_PACKAGE_NAMES.len());
    }
}
