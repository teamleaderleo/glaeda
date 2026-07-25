from pathlib import Path

host = Path("src/host_readiness.rs")
text = host.read_text()
old_import = '''use crate::runner_account_plan::{
    DesiredRunnerAccount, PlannedSubordinateRange, RunnerAccountObservations, RunnerAccountPlan,
    RunnerAccountPlanDisposition, RunnerAccountResourceKind, build_runner_account_plan,
};
'''
new_import = '''use crate::runner_account_plan::{
    DesiredRunnerAccount, PlannedSubordinateRange, PreparationObservationState,
    RunnerAccountObservations, RunnerAccountPlan, RunnerAccountPlanDisposition,
    RunnerAccountResourceKind, build_runner_account_plan,
};
'''
if text.count(old_import) != 1:
    raise SystemExit("host readiness import anchor missing or duplicated")
text = text.replace(old_import, new_import, 1)
host.write_text(text)

observer = Path("src/rootless_podman_config_observation.rs")
text = observer.read_text()
old_resolution_import = '''use crate::rootless_podman_config_resolution::{
    RootlessPodmanConfigAssessment, RootlessPodmanConfigContext, RootlessPodmanConfigPolicy,
    RootlessPodmanConfigSource, RootlessPodmanConfigSourceState, assess_rootless_podman_config,
    resolve_rootless_podman_config,
};
'''
new_resolution_import = '''use crate::rootless_podman_config_resolution::{
    RootlessPodmanConfigAssessment, RootlessPodmanConfigAssessmentState,
    RootlessPodmanConfigContext, RootlessPodmanConfigPolicy, RootlessPodmanConfigSource,
    RootlessPodmanConfigSourceState, assess_rootless_podman_config,
    resolve_rootless_podman_config,
};
'''
if text.count(old_resolution_import) != 1:
    raise SystemExit("observer resolution import anchor missing or duplicated")
text = text.replace(old_resolution_import, new_resolution_import, 1)

observe_with_anchor = '''fn observe_with(
'''
renderer = '''/// Render normalized configuration source states without exposing raw configuration contents.
#[must_use]
pub fn render_human(report: &RootlessPodmanConfigObservationReport) -> String {
    let mut output = format!(
        "Rootless Podman configuration assessment: {}\\nSources:\\n",
        assessment_state_name(report.assessment.state)
    );
    for source in &report.sources {
        output.push_str(&format!(
            "- {} {}: {}\\n",
            observed_source_kind_name(source.kind),
            source.path.display(),
            observed_source_state_name(source.state)
        ));
        if let Some(problem) = source.problem {
            output.push_str(&format!("  - {}\\n", problem.evidence()));
        }
    }
    output
}

const fn assessment_state_name(state: RootlessPodmanConfigAssessmentState) -> &'static str {
    match state {
        RootlessPodmanConfigAssessmentState::Matching => "matching",
        RootlessPodmanConfigAssessmentState::Absent => "absent",
        RootlessPodmanConfigAssessmentState::Unknown => "unknown",
        RootlessPodmanConfigAssessmentState::Conflicting => "conflicting",
    }
}

const fn observed_source_kind_name(kind: RootlessPodmanObservedSourceKind) -> &'static str {
    match kind {
        RootlessPodmanObservedSourceKind::VendorContainers => "vendor containers",
        RootlessPodmanObservedSourceKind::SystemContainers => "system containers",
        RootlessPodmanObservedSourceKind::RunnerContainers => "runner containers",
        RootlessPodmanObservedSourceKind::SystemStorage => "system storage",
        RootlessPodmanObservedSourceKind::RunnerStorage => "runner storage",
    }
}

const fn observed_source_state_name(state: RootlessPodmanObservedSourceState) -> &'static str {
    match state {
        RootlessPodmanObservedSourceState::Missing => "missing",
        RootlessPodmanObservedSourceState::Present => "present",
        RootlessPodmanObservedSourceState::Unknown => "unknown",
    }
}

'''
if text.count(observe_with_anchor) != 1:
    raise SystemExit("observer render insertion anchor missing or duplicated")
text = text.replace(observe_with_anchor, renderer + observe_with_anchor, 1)
observer.write_text(text)
