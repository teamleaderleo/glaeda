use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::debian_package_plan::{DebianPackagePlan, build_package_plan, parse_os_release};
use crate::host::Presence;
use crate::host_preparation_command::{decide_host_preparation, host_preparation_confirmation};
use crate::host_preparation_plan::{HostPreparationProposal, plan_host_preparation};
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
use crate::subordinate_id::{PodmanMigrationPlan, build_exact_subordinate_id_plan};

pub(super) fn desired() -> DesiredRunnerAccount {
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

pub(super) fn observation(
    state: PreparationObservationState,
    label: &str,
) -> PreparationObservation {
    PreparationObservation::new(state, [format!("raw execution sentinel {label}")])
        .expect("bounded observation")
}

pub(super) fn observations(
    group: PreparationObservationState,
    user: PreparationObservationState,
    home: PreparationObservationState,
    uid: PreparationObservationState,
    gid: PreparationObservationState,
    linger: PreparationObservationState,
) -> RunnerAccountObservations {
    RunnerAccountObservations {
        group: observation(group, "group"),
        user: observation(user, "user"),
        home: observation(home, "home"),
        subordinate_uids: observation(uid, "subordinate UIDs"),
        subordinate_gids: observation(gid, "subordinate GIDs"),
        linger: observation(linger, "linger"),
    }
}

pub(super) fn all(state: PreparationObservationState) -> RunnerAccountObservations {
    observations(state, state, state, state, state, state)
}

pub(super) fn packages(git: Presence) -> DebianPackagePlan {
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

pub(super) fn exact_executables(
    package_plan: &DebianPackagePlan,
) -> Vec<ExactExecutableObservation> {
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
        evidence: vec![format!("raw execution sentinel executable {name}")],
    })
    .collect()
}

pub(super) fn preflight(state: RootlessPodmanPreflightState) -> RootlessPodmanPreflightObservation {
    RootlessPodmanPreflightObservation {
        state,
        evidence: vec!["raw execution sentinel preflight".to_owned()],
    }
}

pub(super) fn rootless(package_plan: &DebianPackagePlan) -> HostRootlessPodmanReadiness {
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
                evidence: vec![format!("raw execution sentinel rootless executable {name}")],
            })
            .collect(),
        }),
    }
}

pub(super) fn report(
    repository: &str,
    package_presence: Presence,
    observations: RunnerAccountObservations,
    identity: Option<(u32, u32)>,
) -> HostReadinessReport {
    let desired = desired();
    let package_plan = packages(package_presence);
    let account_plan =
        build_runner_account_plan(desired.clone(), observations.clone()).expect("account plan");
    let subordinate_ids = build_exact_subordinate_id_plan(
        &desired,
        &observations,
        identity,
        std::path::Path::new("/etc/subuid"),
        std::path::Path::new("/etc/subgid"),
    )
    .expect("subordinate plan");
    let all_matching = [
        observations.group.state(),
        observations.user.state(),
        observations.home.state(),
        observations.subordinate_uids.state(),
        observations.subordinate_gids.state(),
        observations.linger.state(),
    ]
    .into_iter()
    .all(|state| state == PreparationObservationState::Matching);
    HostReadinessReport {
        schema_version: crate::host_readiness::HOST_READINESS_SCHEMA_VERSION,
        repository: repository.to_owned(),
        executables: exact_executables(&package_plan),
        rootless_podman: if all_matching {
            rootless(&package_plan)
        } else {
            HostRootlessPodmanReadiness::Deferred {
                state: RootlessPodmanPreflightState::Absent,
                evidence: vec!["raw execution sentinel deferred".to_owned()],
            }
        },
        package_plan,
        runner_account: RunnerAccountReadiness::Planned {
            observations: Box::new(observations),
            plan: account_plan,
            subordinate_ids: Box::new(subordinate_ids),
        },
    }
}

pub(super) fn package_proposal() -> HostPreparationProposal {
    plan_host_preparation(report(
        "owner/repository",
        Presence::Absent,
        all(PreparationObservationState::Matching),
        Some((1001, 1001)),
    ))
}

pub(super) fn mapping_proposal() -> HostPreparationProposal {
    plan_host_preparation(report(
        "owner/repository",
        Presence::Present,
        observations(
            PreparationObservationState::Matching,
            PreparationObservationState::Matching,
            PreparationObservationState::Matching,
            PreparationObservationState::Absent,
            PreparationObservationState::Matching,
            PreparationObservationState::Matching,
        ),
        Some((1001, 1001)),
    ))
}

pub(super) fn migration_proposal() -> HostPreparationProposal {
    let mut changed = report(
        "owner/repository",
        Presence::Present,
        observations(
            PreparationObservationState::Matching,
            PreparationObservationState::Matching,
            PreparationObservationState::Matching,
            PreparationObservationState::Absent,
            PreparationObservationState::Matching,
            PreparationObservationState::Matching,
        ),
        Some((1001, 1001)),
    );
    let migration = match &mut changed.runner_account {
        RunnerAccountReadiness::Planned {
            subordinate_ids, ..
        } => std::mem::replace(
            &mut subordinate_ids.podman_migration,
            PodmanMigrationPlan::NotRequired,
        ),
        RunnerAccountReadiness::NeedsConfiguration { .. } => unreachable!(),
    };
    let mut matching = report(
        "owner/repository",
        Presence::Present,
        all(PreparationObservationState::Matching),
        Some((1001, 1001)),
    );
    match &mut matching.runner_account {
        RunnerAccountReadiness::Planned {
            subordinate_ids, ..
        } => subordinate_ids.podman_migration = migration,
        RunnerAccountReadiness::NeedsConfiguration { .. } => unreachable!(),
    }
    plan_host_preparation(matching)
}

pub(super) fn confirmed(
    proposal: HostPreparationProposal,
) -> crate::host_preparation_command::HostPreparationCommandDecision {
    let confirmation = host_preparation_confirmation(&proposal)
        .expect("confirmation")
        .value()
        .to_owned();
    decide_host_preparation(proposal, Some(&confirmation)).expect("confirmed decision")
}
