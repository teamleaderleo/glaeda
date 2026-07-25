#![cfg(target_os = "linux")]

use std::env;
use std::fs;
use std::path::Path;

use smolrunner::debian_package_plan::{build_package_plan, parse_os_release};
use smolrunner::debian_package_probe::DpkgQueryProbe;
use smolrunner::lane_command::{LinuxAccountName, PackageName};
use smolrunner::process::ProcessExecutor;
use smolrunner::rootless_podman_preflight::{
    RootlessPodmanPreflightDisposition, RootlessPodmanPreflightPaths,
    RootlessPodmanPreflightState, observe_rootless_podman_static_preflight,
};
use smolrunner::runner_account_observation::{
    RunnerAccountObservationPaths, observe_runner_account,
};
use smolrunner::runner_account_plan::{
    DesiredRunnerAccount, PlannedSubordinateRange, PreparationObservationState,
};

const USERNAME: &str = "smolaccept";
const HOME: &str = "/var/lib/smolrunner-acceptance";
const SUBUID_START: u32 = 2_000_000;
const SUBGID_START: u32 = 2_100_000;
const SUBID_COUNT: u32 = 65_536;
const RELOCATED_ROOT: &str = "/var/lib/smolrunner-acceptance-observation";

fn acceptance_enabled() -> bool {
    env::var("SMOLRUNNER_LINUX_ACCEPTANCE").as_deref() == Ok("1")
}

fn desired_account() -> DesiredRunnerAccount {
    let username = LinuxAccountName::parse(USERNAME).expect("acceptance account name is valid");
    DesiredRunnerAccount::new(
        username.clone(),
        username,
        HOME,
        PlannedSubordinateRange::new(SUBUID_START, SUBID_COUNT)
            .expect("acceptance subordinate UID range is valid"),
        PlannedSubordinateRange::new(SUBGID_START, SUBID_COUNT)
            .expect("acceptance subordinate GID range is valid"),
    )
    .expect("acceptance account is valid")
}

#[test]
fn real_static_preflight_is_ready_without_invoking_podman() {
    if !acceptance_enabled() {
        return;
    }

    assert!(
        Path::new("/.dockerenv").exists(),
        "static preflight acceptance requires the disposable Docker harness"
    );

    let desired = desired_account();
    let system = observe_runner_account(
        &desired,
        &ProcessExecutor,
        &RunnerAccountObservationPaths::system_default(),
    )
    .expect("observe account prepared by the base acceptance binary");
    let identity = system.identity().expect("prepared account identity");

    let relocated_root = Path::new(RELOCATED_ROOT);
    let relocated = observe_runner_account(
        &desired,
        &ProcessExecutor,
        &RunnerAccountObservationPaths::new(
            relocated_root.join("subuid"),
            relocated_root.join("subgid"),
            relocated_root.join("linger"),
        )
        .expect("build relocated acceptance paths"),
    )
    .expect("observe protected subordinate-ID and linger fixtures");
    for observation in [
        &relocated.observations.group,
        &relocated.observations.user,
        &relocated.observations.home,
        &relocated.observations.subordinate_uids,
        &relocated.observations.subordinate_gids,
        &relocated.observations.linger,
    ] {
        assert_eq!(observation.state(), PreparationObservationState::Matching);
    }

    let packages = [
        "git",
        "podman",
        "uidmap",
        "slirp4netns",
        "fuse-overlayfs",
        "dbus-user-session",
    ]
    .into_iter()
    .map(|name| PackageName::parse(name).expect("reviewed package name"))
    .collect::<Vec<_>>();
    let package_observation = DpkgQueryProbe::new(ProcessExecutor)
        .observe(&packages)
        .expect("observe real prerequisite package inventory");
    let distribution = parse_os_release(
        &fs::read_to_string("/etc/os-release").expect("read operating-system identity"),
    )
    .expect("parse supported Debian-family identity");
    let package_plan = build_package_plan(distribution, package_observation.packages())
        .expect("build real prerequisite package plan");

    let report = observe_rootless_podman_static_preflight(
        &package_plan,
        &relocated.observations,
        Some(identity),
        &RootlessPodmanPreflightPaths::system_default(),
    );

    assert_eq!(
        report.disposition,
        RootlessPodmanPreflightDisposition::ReadyForSmokeVerification
    );
    assert_eq!(report.packages.state, RootlessPodmanPreflightState::Matching);
    assert_eq!(
        report.runner_account.state,
        RootlessPodmanPreflightState::Matching
    );
    assert_eq!(
        report.runtime_directory.state,
        RootlessPodmanPreflightState::Matching
    );
    assert_eq!(report.executables.len(), 8);
    assert!(
        report
            .executables
            .iter()
            .all(|executable| executable.state == RootlessPodmanPreflightState::Matching)
    );
}
