from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    if text.count(old) != 1:
        raise SystemExit(f"{label}: expected one anchor, found {text.count(old)}")
    return text.replace(old, new, 1)


resolution = Path("src/rootless_podman_config_resolution.rs")
text = resolution.read_text()
text = replace_once(
    text,
    "pub const ROOTLESS_PODMAN_CONFIG_RESOLUTION_SCHEMA_VERSION: u8 = 1;\n",
    "pub const ROOTLESS_PODMAN_CONFIG_RESOLUTION_SCHEMA_VERSION: u8 = 1;\n"
    "const MAX_ROOTLESS_PODMAN_CONFIG_EVIDENCE_BYTES: usize = 512;\n",
    "evidence constant",
)
text = replace_once(
    text,
    "    /// Returns an error unless the source path is a canonical non-root absolute path.\n",
    "    /// Returns an error unless the source path is canonical and unknown evidence is bounded,\n"
    "    /// single-line, and free of control characters.\n",
    "source documentation",
)
text = replace_once(
    text,
    """        Ok(Self {
            path: canonical_absolute_path(path.into())?,
            state,
        })
""",
    """        let state = match state {
            RootlessPodmanConfigSourceState::Unknown { evidence } => {
                RootlessPodmanConfigSourceState::Unknown {
                    evidence: reviewed_evidence(evidence)?,
                }
            }
            state => state,
        };
        Ok(Self {
            path: canonical_absolute_path(path.into())?,
            state,
        })
""",
    "source constructor",
)
text = replace_once(
    text,
    """        || value.ends_with('/')
        || value.chars().any(char::is_control)
""",
    """        || value.ends_with('/')
        || value.contains("//")
        || value.chars().any(char::is_control)
""",
    "canonical separator check",
)
text = replace_once(
    text,
    "fn reviewed_identifier(label: &str, value: String) -> Result<String, String> {\n",
    """fn reviewed_evidence(value: String) -> Result<String, String> {
    if value.is_empty()
        || value.len() > MAX_ROOTLESS_PODMAN_CONFIG_EVIDENCE_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(format!(
            "configuration source evidence must be one nonempty line of at most {MAX_ROOTLESS_PODMAN_CONFIG_EVIDENCE_BYTES} bytes"
        ));
    }
    Ok(value)
}

fn reviewed_identifier(label: &str, value: String) -> Result<String, String> {
""",
    "evidence validator",
)
text = replace_once(
    text,
    """    #[test]
    fn source_and_policy_paths_must_be_canonical() {
""",
    """    #[test]
    fn unknown_source_evidence_is_bounded_and_single_line() {
        for evidence in [
            String::new(),
            "permission denied\nraw configuration follows".to_owned(),
            "x".repeat(MAX_ROOTLESS_PODMAN_CONFIG_EVIDENCE_BYTES + 1),
        ] {
            assert!(
                RootlessPodmanConfigSource::<RootlessPodmanContainersConfig>::new(
                    "/etc/containers/containers.conf",
                    RootlessPodmanConfigSourceState::Unknown { evidence },
                )
                .is_err()
            );
        }
    }

    #[test]
    fn canonical_paths_reject_repeated_separator_aliases() {
        assert!(
            RootlessPodmanConfigSource::<RootlessPodmanContainersConfig>::new(
                "/etc//containers/containers.conf",
                RootlessPodmanConfigSourceState::Missing,
            )
            .is_err()
        );
    }

    #[test]
    fn source_and_policy_paths_must_be_canonical() {
""",
    "resolution regression tests",
)
resolution.write_text(text)

preflight = Path("src/rootless_podman_preflight.rs")
text = preflight.read_text()
text = replace_once(
    text,
    """fn classify_configuration(
    assessment: &RootlessPodmanConfigAssessment,
) -> RootlessPodmanPreflightObservation {
    let state = match assessment.state {
""",
    """fn classify_configuration(
    assessment: &RootlessPodmanConfigAssessment,
) -> RootlessPodmanPreflightObservation {
    let derived_state = assessment.fields.iter().map(|field| field.state).max();
    if derived_state.is_some_and(|state| state != assessment.state) {
        return observation(
            RootlessPodmanPreflightState::Conflicting,
            "rootless Podman configuration assessment summary conflicts with its field results",
        );
    }
    let state = match assessment.state {
""",
    "preflight assessment consistency",
)
preflight.write_text(text)

preflight_tests = Path("src/rootless_podman_preflight/tests.rs")
text = preflight_tests.read_text()
text = replace_once(
    text,
    """    ROOTLESS_PODMAN_CONFIG_RESOLUTION_SCHEMA_VERSION, RootlessPodmanConfigAssessment,
    RootlessPodmanConfigAssessmentState,
""",
    """    ROOTLESS_PODMAN_CONFIG_RESOLUTION_SCHEMA_VERSION, RootlessPodmanConfigAssessment,
    RootlessPodmanConfigAssessmentState, RootlessPodmanConfigField,
    RootlessPodmanConfigFieldAssessment,
""",
    "preflight test imports",
)
text = replace_once(
    text,
    """#[test]
fn unsafe_runtime_root_is_rejected() {
""",
    """#[test]
fn inconsistent_configuration_assessment_blocks_preflight() {
    let assessment = RootlessPodmanConfigAssessment {
        schema_version: ROOTLESS_PODMAN_CONFIG_RESOLUTION_SCHEMA_VERSION,
        state: RootlessPodmanConfigAssessmentState::Matching,
        fields: vec![RootlessPodmanConfigFieldAssessment {
            field: RootlessPodmanConfigField::NetworkBackend,
            state: RootlessPodmanConfigAssessmentState::Conflicting,
            expected: "netavark".to_owned(),
            observed: Some("cni".to_owned()),
            evidence: vec!["bounded test evidence".to_owned()],
        }],
    };
    let report = observe_with(
        &package_plan(Presence::Present),
        &account_observations(PreparationObservationState::Matching),
        Some(RuntimeIdentity { uid: 1001 }),
        &assessment,
        &RootlessPodmanPreflightPaths::system_default(),
        &FakeFilesystem {
            observation: RuntimePathObservation::Present(RuntimePathMetadata {
                kind: RuntimePathKind::Directory,
                uid: 1001,
                mode: 0o700,
            }),
        },
        &matching_executable,
    );

    assert_eq!(
        report.configuration.state,
        RootlessPodmanPreflightState::Conflicting
    );
    assert_eq!(
        report.disposition,
        RootlessPodmanPreflightDisposition::Blocked
    );
}

#[test]
fn unsafe_runtime_root_is_rejected() {
""",
    "preflight consistency regression test",
)
preflight_tests.write_text(text)
