use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::debian_package_plan::{DebianPackagePlan, build_package_plan, parse_os_release};
use crate::host::Presence;
use crate::host_preparation_plan::{HostPreparationResult, plan_host_preparation};
use crate::host_readiness::{
    ExactExecutableObservation, HostObservationState, HostReadinessReport, RunnerAccountReadiness,
};
use crate::host_rootless_podman::HostRootlessPodmanReadiness;
use crate::lane_command::LinuxAccountName;
use crate::rootless_podman_config_observation::{
    ROOTLESS_PODMAN_CONFIG_OBSERVATION_SCHEMA_VERSION, RootlessPodmanConfigObservationReport,
};
use crate::rootless_podman_config_resolution::{
    ROOTLESS_PODMAN_CONFIG_RESOLUTION_SCHEMA_VERSION, RootlessPodmanConfigAssessment,
    RootlessPodmanConfigAssessmentState,
};
use crate::rootless_podman_preflight::{
    ROOTLESS_PODMAN_PREFLIGHT_SCHEMA_VERSION, RootlessPodmanExecutableObservation,
    RootlessPodmanPreflightDisposition, RootlessPodmanPreflightObservation,
    RootlessPodmanPreflightState, RootlessPodmanStaticPreflightReport,
};
use crate::runner_account_plan::{
    DesiredRunnerAccount, PlannedSubordinateRange, PreparationObservation,
    PreparationObservationState, RunnerAccountObservations, build_runner_account_plan,
};
use crate::subordinate_id::build_exact_subordinate_id_plan;

use super::{
    HOST_PREPARATION_COMMAND_SCHEMA_VERSION, HOST_PREPARATION_CONFIRMATION_PREFIX,
    HostPreparationCommandDisposition, decide_host_preparation, hex_encode,
    host_preparation_confirmation, render_human,
};

fn desired() -> DesiredRunnerAccount {
    let account = LinuxAccountName::parse("project-runner").expect("account");
    DesiredRunnerAccount::new(
        account.clone(),
        account,
        "/var/lib/project-runner",
        PlannedSubordinateRange::new(100_000, 65_536).expect("UID range"),
        PlannedSubordinateRange::new(200_000, 65_536).expect("GID range"),
    )
    .expect("desired account")
}

fn observation(state: PreparationObservationState, label: &str) -> PreparationObservation {
    PreparationObservation::new(state, [format!("raw sentinel {label}")])
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

fn packages(git: Presence) -> DebianPackagePlan {
    let inventory = [
        ("git", git),
        ("podman", Presence::Present),
        ("uidmap", Presence::Present),
        ("slirp4netns", Presence::Present),
        ("fuse-overlayfs", Presence::Present),
        ("dbus-user-session", Presence::Present),
    ]
    .into_iter()
    .map(|(name, state)| (name.to_owned(), state))
    .collect::<BTreeMap<_, _>>();
    build_package_plan(
        parse_os_release("ID=debian\nVERSION_ID=\"12\"\n").expect("distribution"),
        &inventory,
    )
    .expect("package plan")
}

fn exact_executables(package_plan: &DebianPackagePlan) -> Vec<ExactExecutableObservation> {
    [
        ("git", "/usr/bin/git"),
        ("podman", "/usr/bin/podman"),
        ("systemctl", "/usr/bin/systemctl"),
    ]
    .into_iter()
    .map(|(name, path)| ExactExecutableObservation {
        name: name.to_owned(),
        path: PathBuf::from(path),
        state: if package_plan
            .missing_packages
            .iter()
            .any(|package| package.as_str() == name)
        {
            HostObservationState::Absent
        } else {
            HostObservationState::Matching
        },
        evidence: vec![format!("raw sentinel executable {name}")],
    })
    .collect()
}

fn preflight(state: RootlessPodmanPreflightState) -> RootlessPodmanPreflightObservation {
    RootlessPodmanPreflightObservation {
        state,
        evidence: vec!["raw sentinel preflight".to_owned()],
    }
}

fn rootless(package_plan: &DebianPackagePlan) -> HostRootlessPodmanReadiness {
    let package_state = if package_plan.missing_packages.is_empty() {
        RootlessPodmanPreflightState::Matching
    } else {
        RootlessPodmanPreflightState::Absent
    };
    let disposition = if package_state == RootlessPodmanPreflightState::Matching {
        RootlessPodmanPreflightDisposition::ReadyForSmokeVerification
    } else {
        RootlessPodmanPreflightDisposition::ChangesRequired
    };
    let executable_state = |package: &str| {
        if package_plan
            .missing_packages
            .iter()
            .any(|candidate| candidate.as_str() == package)
        {
            RootlessPodmanPreflightState::Absent
        } else {
            RootlessPodmanPreflightState::Matching
        }
    };
    HostRootlessPodmanReadiness::Observed {
        configuration: Box::new(RootlessPodmanConfigObservationReport {
            schema_version: ROOTLESS_PODMAN_CONFIG_OBSERVATION_SCHEMA_VERSION,
            sources: Vec::new(),
            assessment: RootlessPodmanConfigAssessment {
                schema_version: ROOTLESS_PODMAN_CONFIG_RESOLUTION_SCHEMA_VERSION,
                state: RootlessPodmanConfigAssessmentState::Matching,
                fields: Vec::new(),
            },
        }),
        preflight: Box::new(RootlessPodmanStaticPreflightReport {
            schema_version: ROOTLESS_PODMAN_PREFLIGHT_SCHEMA_VERSION,
            disposition,
            packages: preflight(package_state),
            runner_account: preflight(RootlessPodmanPreflightState::Matching),
            runtime_directory: preflight(RootlessPodmanPreflightState::Matching),
            configuration: preflight(RootlessPodmanPreflightState::Matching),
            executables: [
                ("podman", "/usr/bin/podman", executable_state("podman")),
                (
                    "runuser",
                    "/usr/sbin/runuser",
                    RootlessPodmanPreflightState::Matching,
                ),
                (
                    "env",
                    "/usr/bin/env",
                    RootlessPodmanPreflightState::Matching,
                ),
                (
                    "systemctl",
                    "/usr/bin/systemctl",
                    RootlessPodmanPreflightState::Matching,
                ),
                (
                    "newuidmap",
                    "/usr/bin/newuidmap",
                    executable_state("uidmap"),
                ),
                (
                    "newgidmap",
                    "/usr/bin/newgidmap",
                    executable_state("uidmap"),
                ),
                (
                    "slirp4netns",
                    "/usr/bin/slirp4netns",
                    executable_state("slirp4netns"),
                ),
                (
                    "fuse-overlayfs",
                    "/usr/bin/fuse-overlayfs",
                    executable_state("fuse-overlayfs"),
                ),
            ]
            .into_iter()
            .map(|(name, path, state)| RootlessPodmanExecutableObservation {
                name: name.to_owned(),
                path: PathBuf::from(path),
                state,
                evidence: vec![format!("raw sentinel rootless executable {name}")],
            })
            .collect(),
        }),
    }
}

fn proposal(
    repository: &str,
    package_presence: Presence,
    account_state: PreparationObservationState,
) -> crate::host_preparation_plan::HostPreparationProposal {
    let desired = desired();
    let observations = observations(account_state);
    let package_plan = packages(package_presence);
    let account_plan =
        build_runner_account_plan(desired.clone(), observations.clone()).expect("account plan");
    let identity = (account_state == PreparationObservationState::Matching).then_some((1001, 1001));
    let subordinate_ids = build_exact_subordinate_id_plan(
        &desired,
        &observations,
        identity,
        std::path::Path::new("/etc/subuid"),
        std::path::Path::new("/etc/subgid"),
    )
    .expect("subordinate plan");
    plan_host_preparation(HostReadinessReport {
        schema_version: crate::host_readiness::HOST_READINESS_SCHEMA_VERSION,
        repository: repository.to_owned(),
        executables: exact_executables(&package_plan),
        rootless_podman: if account_state == PreparationObservationState::Matching {
            rootless(&package_plan)
        } else {
            HostRootlessPodmanReadiness::Deferred {
                state: RootlessPodmanPreflightState::Unknown,
                evidence: vec!["raw sentinel deferred".to_owned()],
            }
        },
        package_plan,
        runner_account: RunnerAccountReadiness::Planned {
            observations: Box::new(observations),
            plan: account_plan,
            subordinate_ids: Box::new(subordinate_ids),
        },
    })
}

#[test]
fn hexadecimal_encoding_is_injective_and_stable() {
    assert_eq!(hex_encode(b""), "");
    assert_eq!(hex_encode(b"f"), "66");
    assert_eq!(hex_encode(b"fo"), "666f");
    assert_eq!(hex_encode(b"foo"), "666f6f");
}

#[test]
fn executable_confirmation_is_stable_and_exact() {
    let first = proposal(
        "owner/repository",
        Presence::Absent,
        PreparationObservationState::Matching,
    );
    let second = proposal(
        "owner/repository",
        Presence::Absent,
        PreparationObservationState::Matching,
    );
    let first_confirmation = host_preparation_confirmation(&first).expect("confirmation");
    let second_confirmation = host_preparation_confirmation(&second).expect("confirmation");
    assert_eq!(first_confirmation, second_confirmation);
    assert_eq!(
        first_confirmation.schema_version,
        HOST_PREPARATION_COMMAND_SCHEMA_VERSION
    );
    let encoded = first_confirmation
        .value
        .strip_prefix(HOST_PREPARATION_CONFIRMATION_PREFIX)
        .expect("confirmation prefix");
    assert!(
        encoded
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    );
    assert_eq!(
        first_confirmation.value.len(),
        HOST_PREPARATION_CONFIRMATION_PREFIX.len()
            + serde_json::to_vec(&first).expect("public proposal").len() * 2
    );

    let changed = proposal(
        "owner/another-repository",
        Presence::Absent,
        PreparationObservationState::Matching,
    );
    assert_ne!(
        first_confirmation,
        host_preparation_confirmation(&changed).expect("changed confirmation")
    );
}

#[test]
fn decision_distinguishes_ready_blocked_required_mismatch_and_confirmed() {
    let ready = decide_host_preparation(
        proposal(
            "owner/repository",
            Presence::Present,
            PreparationObservationState::Matching,
        ),
        Some("ignored"),
    )
    .expect("ready decision");
    assert_eq!(ready.disposition, HostPreparationCommandDisposition::Ready);
    assert!(ready.confirmation.is_none());

    let blocked = decide_host_preparation(
        proposal(
            "owner/repository",
            Presence::Present,
            PreparationObservationState::Unknown,
        ),
        Some("ignored"),
    )
    .expect("blocked decision");
    assert_eq!(
        blocked.disposition,
        HostPreparationCommandDisposition::Blocked
    );
    assert!(blocked.confirmation.is_none());

    let executable = proposal(
        "owner/repository",
        Presence::Absent,
        PreparationObservationState::Matching,
    );
    assert!(matches!(
        executable.result,
        HostPreparationResult::Executable { .. }
    ));
    let expected = host_preparation_confirmation(&executable)
        .expect("confirmation")
        .value;
    let required = decide_host_preparation(executable.clone(), None).expect("required decision");
    assert_eq!(
        required.disposition,
        HostPreparationCommandDisposition::ConfirmationRequired
    );
    let mismatch =
        decide_host_preparation(executable.clone(), Some("wrong")).expect("mismatch decision");
    assert_eq!(
        mismatch.disposition,
        HostPreparationCommandDisposition::ConfirmationMismatch
    );
    let confirmed =
        decide_host_preparation(executable, Some(&expected)).expect("confirmed decision");
    assert_eq!(
        confirmed.disposition,
        HostPreparationCommandDisposition::Confirmed
    );
    assert!(confirmed.confirmed_phase().is_some());
    assert!(confirmed.into_confirmed_phase().is_some());
}

#[test]
fn output_contains_only_public_proposal_and_bounded_confirmation() {
    let decision = decide_host_preparation(
        proposal(
            "owner/repository",
            Presence::Absent,
            PreparationObservationState::Matching,
        ),
        None,
    )
    .expect("decision");
    let json = serde_json::to_string(&decision).expect("serialize decision");
    for forbidden in [
        "raw sentinel",
        "durable_plan",
        "\"spec\"",
        "\"program\"",
        "\"arguments\"",
        "\"environment\"",
    ] {
        assert!(!json.contains(forbidden), "JSON leaked {forbidden}");
    }
    let human = render_human(&decision);
    assert!(human.contains("Exact confirmation:"));
    assert!(!human.contains("raw sentinel"));
    assert!(!human.contains("/usr/bin/apt-get"));
}
