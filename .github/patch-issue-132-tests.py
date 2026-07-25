from pathlib import Path

path = Path('src/host_readiness.rs')
text = path.read_text()

old_imports = '''    use crate::lane_command::LinuxAccountName;
    use crate::runner_account_plan::{
'''
new_imports = '''    use crate::lane_command::LinuxAccountName;
    use crate::rootless_podman_config_observation::{
        ROOTLESS_PODMAN_CONFIG_OBSERVATION_SCHEMA_VERSION,
        RootlessPodmanConfigObservationReport, RootlessPodmanConfigSourceObservation,
        RootlessPodmanConfigSourceProblemKind, RootlessPodmanObservedSourceKind,
        RootlessPodmanObservedSourceState,
    };
    use crate::rootless_podman_config_resolution::{
        ROOTLESS_PODMAN_CONFIG_RESOLUTION_SCHEMA_VERSION, RootlessPodmanConfigAssessment,
        RootlessPodmanConfigAssessmentState,
    };
    use crate::rootless_podman_preflight::{
        ROOTLESS_PODMAN_PREFLIGHT_SCHEMA_VERSION, RootlessPodmanPreflightDisposition,
        RootlessPodmanPreflightObservation, RootlessPodmanPreflightState,
        RootlessPodmanStaticPreflightReport,
    };
    use crate::runner_account_plan::{
'''
if text.count(old_imports) != 1:
    raise SystemExit('test imports anchor missing or duplicated')
text = text.replace(old_imports, new_imports, 1)

old_super = '''        ExactExecutableObservation, HostObservationState, HostReadinessReport,
        RunnerAccountPolicyFile, RunnerAccountReadiness, SubordinateRangePolicy, render_human,
        without_subordinate_mapping_items,
'''
new_super = '''        ExactExecutableObservation, HostObservationState, HostReadinessReport,
        RootlessPodmanHostReadiness, RunnerAccountPolicyFile, RunnerAccountReadiness,
        SubordinateRangePolicy, render_human, without_subordinate_mapping_items,
'''
if text.count(old_super) != 1:
    raise SystemExit('test super import anchor missing or duplicated')
text = text.replace(old_super, new_super, 1)

helper_anchor = '''    fn report(
'''
helpers = '''    fn config_source(
        kind: RootlessPodmanObservedSourceKind,
        state: RootlessPodmanObservedSourceState,
        problem: Option<RootlessPodmanConfigSourceProblemKind>,
    ) -> RootlessPodmanConfigSourceObservation {
        RootlessPodmanConfigSourceObservation {
            kind,
            path: format!("/reviewed/{kind:?}.conf").into(),
            state,
            problem,
        }
    }

    fn rootless_readiness(
        assessment_state: RootlessPodmanConfigAssessmentState,
        disposition: RootlessPodmanPreflightDisposition,
        preflight_state: RootlessPodmanPreflightState,
        sources: Vec<RootlessPodmanConfigSourceObservation>,
    ) -> RootlessPodmanHostReadiness {
        let configuration = RootlessPodmanConfigObservationReport {
            schema_version: ROOTLESS_PODMAN_CONFIG_OBSERVATION_SCHEMA_VERSION,
            sources,
            assessment: RootlessPodmanConfigAssessment {
                schema_version: ROOTLESS_PODMAN_CONFIG_RESOLUTION_SCHEMA_VERSION,
                state: assessment_state,
                fields: Vec::new(),
            },
        };
        let observation = RootlessPodmanPreflightObservation {
            state: preflight_state,
            evidence: vec!["bounded composed preflight evidence".to_owned()],
        };
        RootlessPodmanHostReadiness::Observed {
            configuration: Box::new(configuration),
            preflight: Box::new(RootlessPodmanStaticPreflightReport {
                schema_version: ROOTLESS_PODMAN_PREFLIGHT_SCHEMA_VERSION,
                disposition,
                packages: observation.clone(),
                runner_account: observation.clone(),
                runtime_directory: observation.clone(),
                configuration: observation,
                executables: Vec::new(),
            }),
        }
    }

    fn ready_rootless_podman() -> RootlessPodmanHostReadiness {
        rootless_readiness(
            RootlessPodmanConfigAssessmentState::Matching,
            RootlessPodmanPreflightDisposition::ReadyForSmokeVerification,
            RootlessPodmanPreflightState::Matching,
            vec![config_source(
                RootlessPodmanObservedSourceKind::RunnerContainers,
                RootlessPodmanObservedSourceState::Present,
                None,
            )],
        )
    }

'''
if text.count(helper_anchor) != 1:
    raise SystemExit('test report helper anchor missing or duplicated')
text = text.replace(helper_anchor, helpers + helper_anchor, 1)

old_report_end = '''            runner_account: RunnerAccountReadiness::Planned {
                observations: Box::new(account_observations),
                plan: account_plan,
                subordinate_ids: Box::new(subordinate_ids),
            },
        }
'''
new_report_end = '''            runner_account: RunnerAccountReadiness::Planned {
                observations: Box::new(account_observations),
                plan: account_plan,
                subordinate_ids: Box::new(subordinate_ids),
            },
            rootless_podman: ready_rootless_podman(),
        }
'''
if text.count(old_report_end) != 1:
    raise SystemExit('primary report constructor anchor missing or duplicated')
text = text.replace(old_report_end, new_report_end, 1)

conflict_anchor = '''                plan,
            },
        };
        let json = serde_json::to_value(&report).expect("serialize report");
'''
conflict_replacement = '''                plan,
            },
            rootless_podman: RootlessPodmanHostReadiness::NeedsAccountEvidence {
                evidence: vec!["exact runner identity is unavailable".to_owned()],
            },
        };
        let json = serde_json::to_value(&report).expect("serialize report");
'''
if text.count(conflict_anchor) != 1:
    raise SystemExit('conflicting report constructor anchor missing or duplicated')
text = text.replace(conflict_anchor, conflict_replacement, 1)

clean_anchor = '''        assert_eq!(json["package_plan"]["disposition"], "ready");
'''
clean_assertions = clean_anchor + '''        assert_eq!(json["rootless_podman"]["disposition"], "observed");
        assert_eq!(
            json["rootless_podman"]["preflight"]["disposition"],
            "ready_for_smoke_verification"
        );
'''
if text.count(clean_anchor) != 1:
    raise SystemExit('clean host assertion anchor missing or duplicated')
text = text.replace(clean_anchor, clean_assertions, 1)

human_anchor = '''        assert!(render_human(&report).contains("[READY] ensure dedicated runner group"));
'''
human_assertions = human_anchor + '''        assert!(
            render_human(&report)
                .contains("Static preflight disposition: ready_for_smoke_verification")
        );
'''
if text.count(human_anchor) != 1:
    raise SystemExit('clean host human assertion anchor missing or duplicated')
text = text.replace(human_anchor, human_assertions, 1)

new_tests_anchor = '''    #[test]
    fn malformed_policy_is_rejected_before_observation() {
'''
new_tests = '''    #[test]
    fn composed_report_preserves_normalized_source_failures() {
        let mut report = report(
            Presence::Present,
            HostObservationState::Matching,
            PreparationObservationState::Matching,
        );
        report.rootless_podman = rootless_readiness(
            RootlessPodmanConfigAssessmentState::Unknown,
            RootlessPodmanPreflightDisposition::NeedsInspection,
            RootlessPodmanPreflightState::Unknown,
            vec![
                config_source(
                    RootlessPodmanObservedSourceKind::RunnerContainers,
                    RootlessPodmanObservedSourceState::Unknown,
                    Some(RootlessPodmanConfigSourceProblemKind::Unreadable),
                ),
                config_source(
                    RootlessPodmanObservedSourceKind::SystemStorage,
                    RootlessPodmanObservedSourceState::Unknown,
                    Some(RootlessPodmanConfigSourceProblemKind::InvalidReviewedSyntax),
                ),
                config_source(
                    RootlessPodmanObservedSourceKind::VendorContainers,
                    RootlessPodmanObservedSourceState::Unknown,
                    Some(RootlessPodmanConfigSourceProblemKind::UnsafeParentDirectory),
                ),
            ],
        );

        let json = serde_json::to_value(&report).expect("serialize composed report");
        let sources = json["rootless_podman"]["configuration"]["sources"]
            .as_array()
            .expect("source array");
        assert_eq!(sources[0]["state"], "unknown");
        assert_eq!(sources[0]["problem"], "unreadable");
        assert_eq!(sources[1]["problem"], "invalid_reviewed_syntax");
        assert_eq!(sources[2]["problem"], "unsafe_parent_directory");
        assert_eq!(
            json["rootless_podman"]["preflight"]["disposition"],
            "needs_inspection"
        );
        assert_eq!(
            crate::host_readiness_verdict::assess(&report).disposition,
            crate::host_readiness_verdict::HostReadinessDisposition::NeedsInspection
        );
    }

    #[test]
    fn configuration_dispositions_remain_distinct_in_composed_host_output() {
        let cases = [
            (
                RootlessPodmanConfigAssessmentState::Absent,
                RootlessPodmanPreflightDisposition::ChangesRequired,
                RootlessPodmanPreflightState::Absent,
                "absent",
                "changes_required",
                crate::host_readiness_verdict::HostReadinessDisposition::ChangesRequired,
            ),
            (
                RootlessPodmanConfigAssessmentState::Unknown,
                RootlessPodmanPreflightDisposition::NeedsInspection,
                RootlessPodmanPreflightState::Unknown,
                "unknown",
                "needs_inspection",
                crate::host_readiness_verdict::HostReadinessDisposition::NeedsInspection,
            ),
            (
                RootlessPodmanConfigAssessmentState::Conflicting,
                RootlessPodmanPreflightDisposition::Blocked,
                RootlessPodmanPreflightState::Conflicting,
                "conflicting",
                "blocked",
                crate::host_readiness_verdict::HostReadinessDisposition::Blocked,
            ),
        ];

        for (assessment_state, preflight_disposition, preflight_state, state_name, disposition_name, expected_verdict) in cases {
            let mut report = report(
                Presence::Present,
                HostObservationState::Matching,
                PreparationObservationState::Matching,
            );
            report.rootless_podman = rootless_readiness(
                assessment_state,
                preflight_disposition,
                preflight_state,
                vec![config_source(
                    RootlessPodmanObservedSourceKind::RunnerStorage,
                    if assessment_state == RootlessPodmanConfigAssessmentState::Absent {
                        RootlessPodmanObservedSourceState::Missing
                    } else {
                        RootlessPodmanObservedSourceState::Present
                    },
                    None,
                )],
            );
            let json = serde_json::to_value(&report).expect("serialize composed report");
            assert_eq!(
                json["rootless_podman"]["configuration"]["assessment"]["state"],
                state_name
            );
            assert_eq!(
                json["rootless_podman"]["preflight"]["disposition"],
                disposition_name
            );
            assert_eq!(
                crate::host_readiness_verdict::assess(&report).disposition,
                expected_verdict
            );
        }
    }

'''
if text.count(new_tests_anchor) != 1:
    raise SystemExit('new test anchor missing or duplicated')
text = text.replace(new_tests_anchor, new_tests + new_tests_anchor, 1)
path.write_text(text)
