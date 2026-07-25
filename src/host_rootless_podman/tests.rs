use std::path::PathBuf;

use crate::rootless_podman_config_observation::{
    RootlessPodmanConfigObservationReport, RootlessPodmanConfigSourceObservation,
    RootlessPodmanConfigSourceProblemKind, RootlessPodmanObservedSourceKind,
    RootlessPodmanObservedSourceState,
};
use crate::rootless_podman_config_resolution::{
    RootlessPodmanConfigAssessment, RootlessPodmanConfigAssessmentState,
};
use crate::rootless_podman_preflight::{
    RootlessPodmanPreflightDisposition, RootlessPodmanPreflightObservation,
    RootlessPodmanPreflightState, RootlessPodmanStaticPreflightReport,
};
use crate::runner_account_plan::{
    PreparationObservation, PreparationObservationState, RunnerAccountObservations,
};

use super::{HostRootlessPodmanReadiness, deferred_account_readiness, render_human};

fn observation(state: PreparationObservationState) -> PreparationObservation {
    PreparationObservation::new(state, ["bounded account evidence"]).expect("observation")
}

fn account_observations(state: PreparationObservationState) -> RunnerAccountObservations {
    RunnerAccountObservations {
        group: observation(state),
        user: observation(state),
        home: observation(state),
        subordinate_uids: observation(state),
        subordinate_gids: observation(state),
        linger: observation(state),
    }
}

fn preflight_observation(
    state: RootlessPodmanPreflightState,
) -> RootlessPodmanPreflightObservation {
    RootlessPodmanPreflightObservation {
        state,
        evidence: vec!["bounded preflight evidence".to_owned()],
    }
}

fn observed_readiness(
    source_state: RootlessPodmanObservedSourceState,
    problem: Option<RootlessPodmanConfigSourceProblemKind>,
    assessment_state: RootlessPodmanConfigAssessmentState,
    configuration_state: RootlessPodmanPreflightState,
    disposition: RootlessPodmanPreflightDisposition,
) -> HostRootlessPodmanReadiness {
    HostRootlessPodmanReadiness::Observed {
        configuration: Box::new(RootlessPodmanConfigObservationReport {
            schema_version: 1,
            sources: vec![RootlessPodmanConfigSourceObservation {
                kind: RootlessPodmanObservedSourceKind::RunnerContainers,
                path: PathBuf::from("/var/lib/project-runner/.config/containers/containers.conf"),
                state: source_state,
                problem,
            }],
            assessment: RootlessPodmanConfigAssessment {
                schema_version: 1,
                state: assessment_state,
                fields: Vec::new(),
            },
        }),
        preflight: Box::new(RootlessPodmanStaticPreflightReport {
            schema_version: 1,
            disposition,
            packages: preflight_observation(RootlessPodmanPreflightState::Matching),
            runner_account: preflight_observation(RootlessPodmanPreflightState::Matching),
            runtime_directory: preflight_observation(RootlessPodmanPreflightState::Matching),
            configuration: preflight_observation(configuration_state),
            executables: Vec::new(),
        }),
    }
}

#[test]
fn absent_account_defers_configuration_as_required() {
    let readiness = deferred_account_readiness(
        &account_observations(PreparationObservationState::Absent),
        None,
    );
    let HostRootlessPodmanReadiness::Deferred { state, evidence } = readiness else {
        panic!("deferred readiness");
    };
    assert_eq!(state, RootlessPodmanPreflightState::Absent);
    assert_eq!(evidence.len(), 1);
}

#[test]
fn unknown_and_conflicting_account_evidence_remain_distinct() {
    let unknown = deferred_account_readiness(
        &account_observations(PreparationObservationState::Unknown),
        None,
    );
    let conflicting = deferred_account_readiness(
        &account_observations(PreparationObservationState::Conflicting),
        None,
    );
    assert!(matches!(
        unknown,
        HostRootlessPodmanReadiness::Deferred {
            state: RootlessPodmanPreflightState::Unknown,
            ..
        }
    ));
    assert!(matches!(
        conflicting,
        HostRootlessPodmanReadiness::Deferred {
            state: RootlessPodmanPreflightState::Conflicting,
            ..
        }
    ));
}

#[test]
fn matching_composed_report_is_ready_for_smoke_verification() {
    let readiness = observed_readiness(
        RootlessPodmanObservedSourceState::Present,
        None,
        RootlessPodmanConfigAssessmentState::Matching,
        RootlessPodmanPreflightState::Matching,
        RootlessPodmanPreflightDisposition::ReadyForSmokeVerification,
    );
    assert_eq!(
        readiness.preflight_disposition(),
        Some(RootlessPodmanPreflightDisposition::ReadyForSmokeVerification)
    );
    let json = serde_json::to_value(readiness).expect("serialize readiness");
    assert_eq!(json["disposition"], "observed");
    assert_eq!(json["configuration"]["sources"][0]["state"], "present");
}

#[test]
fn missing_source_and_absent_assessment_remain_changes_required() {
    let readiness = observed_readiness(
        RootlessPodmanObservedSourceState::Missing,
        None,
        RootlessPodmanConfigAssessmentState::Absent,
        RootlessPodmanPreflightState::Absent,
        RootlessPodmanPreflightDisposition::ChangesRequired,
    );
    let json = serde_json::to_value(readiness).expect("serialize readiness");
    assert_eq!(json["configuration"]["sources"][0]["state"], "missing");
    assert_eq!(json["preflight"]["disposition"], "changes_required");
}

#[test]
fn unreadable_runner_override_is_normalized_unknown() {
    assert_unknown_problem(
        RootlessPodmanConfigSourceProblemKind::Unreadable,
        "unreadable",
    );
}

#[test]
fn malformed_runner_override_is_normalized_unknown() {
    assert_unknown_problem(
        RootlessPodmanConfigSourceProblemKind::InvalidReviewedSyntax,
        "invalid_reviewed_syntax",
    );
}

#[test]
fn unsafe_metadata_is_normalized_unknown() {
    assert_unknown_problem(
        RootlessPodmanConfigSourceProblemKind::UnsafeParentDirectory,
        "unsafe_parent_directory",
    );
}

#[test]
fn configuration_conflict_blocks_composed_preflight() {
    let readiness = observed_readiness(
        RootlessPodmanObservedSourceState::Present,
        None,
        RootlessPodmanConfigAssessmentState::Conflicting,
        RootlessPodmanPreflightState::Conflicting,
        RootlessPodmanPreflightDisposition::Blocked,
    );
    let json = serde_json::to_value(readiness).expect("serialize readiness");
    assert_eq!(json["configuration"]["assessment"]["state"], "conflicting");
    assert_eq!(json["preflight"]["disposition"], "blocked");
}

#[test]
fn human_output_contains_only_normalized_evidence() {
    let readiness = observed_readiness(
        RootlessPodmanObservedSourceState::Unknown,
        Some(RootlessPodmanConfigSourceProblemKind::Unreadable),
        RootlessPodmanConfigAssessmentState::Unknown,
        RootlessPodmanPreflightState::Unknown,
        RootlessPodmanPreflightDisposition::NeedsInspection,
    );
    let output = render_human(&readiness);
    assert!(output.contains("runner containers.conf"));
    assert!(output.contains("needs_inspection"));
    assert!(!output.contains("permission denied"));
    assert!(!output.contains("secret"));
}

fn assert_unknown_problem(
    problem: RootlessPodmanConfigSourceProblemKind,
    serialized_problem: &str,
) {
    let readiness = observed_readiness(
        RootlessPodmanObservedSourceState::Unknown,
        Some(problem),
        RootlessPodmanConfigAssessmentState::Unknown,
        RootlessPodmanPreflightState::Unknown,
        RootlessPodmanPreflightDisposition::NeedsInspection,
    );
    let json = serde_json::to_value(readiness).expect("serialize readiness");
    assert_eq!(json["configuration"]["sources"][0]["state"], "unknown");
    assert_eq!(
        json["configuration"]["sources"][0]["problem"],
        serialized_problem
    );
    assert_eq!(json["preflight"]["disposition"], "needs_inspection");
    let rendered = json.to_string();
    assert!(!rendered.contains("permission denied"));
    assert!(!rendered.contains("raw configuration"));
}
