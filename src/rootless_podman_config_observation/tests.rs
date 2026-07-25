use std::collections::BTreeMap;

use super::*;
use crate::rootless_podman_config_resolution::RootlessPodmanConfigAssessmentState;

#[derive(Default)]
struct FakeFilesystem {
    reads: BTreeMap<PathBuf, TrustedConfigRead>,
}

impl FakeFilesystem {
    fn with(mut self, path: impl Into<PathBuf>, read: TrustedConfigRead) -> Self {
        self.reads.insert(path.into(), read);
        self
    }
}

impl ConfigFilesystem for FakeFilesystem {
    fn read(&self, path: &Path, _expected_owner: ExpectedOwner) -> TrustedConfigRead {
        self.reads
            .get(path)
            .cloned()
            .unwrap_or(TrustedConfigRead::Missing)
    }
}

fn context() -> RootlessPodmanConfigObservationContext {
    RootlessPodmanConfigObservationContext::new(
        "/var/lib/project-runner",
        "/var/lib/project-runner/.config",
        "/var/lib/project-runner/.local/share",
        "/run/user/1001",
        1001,
        1001,
    )
    .expect("context")
}

fn paths() -> RootlessPodmanConfigObservationPaths {
    RootlessPodmanConfigObservationPaths::system_default()
}

fn policy() -> RootlessPodmanConfigPolicy {
    RootlessPodmanConfigPolicy::new(
        "overlay",
        "/var/lib/project-runner/.local/share/containers/storage",
        "/run/user/1001/containers",
        "/usr/bin/fuse-overlayfs",
        "systemd",
        "netavark",
    )
    .expect("policy")
}

#[test]
fn all_missing_sources_remain_absent() {
    let report = observe_with(&context(), &paths(), &policy(), &FakeFilesystem::default())
        .expect("observation");
    assert!(
        report
            .sources
            .iter()
            .all(|source| source.state == RootlessPodmanObservedSourceState::Missing)
    );
    assert_eq!(
        report.assessment.state,
        RootlessPodmanConfigAssessmentState::Absent
    );
}

#[test]
fn matching_sources_compose_through_parser_resolver_and_policy() {
    let filesystem = FakeFilesystem::default()
        .with(
            "/usr/share/containers/containers.conf",
            TrustedConfigRead::Present(
                b"[engine]\ncgroup_manager = \"systemd\"\n[network]\nnetwork_backend = \"netavark\"\n"
                    .to_vec(),
            ),
        )
        .with(
            "/var/lib/project-runner/.config/containers/storage.conf",
            TrustedConfigRead::Present(
                b"[storage]\ndriver = \"overlay\"\nrunroot = \"$XDG_RUNTIME_DIR/containers\"\ngraphroot = \"$XDG_DATA_HOME/containers/storage\"\n[storage.options.overlay]\nmount_program = \"/usr/bin/fuse-overlayfs\"\n"
                    .to_vec(),
            ),
        );
    let report = observe_with(&context(), &paths(), &policy(), &filesystem).expect("observation");
    assert_eq!(
        report.assessment.state,
        RootlessPodmanConfigAssessmentState::Matching
    );
}

#[test]
fn unknown_runner_source_hides_lower_precedence_without_leaking_error_text() {
    let secret = "permission denied: token=secret-value";
    let filesystem = FakeFilesystem::default()
        .with(
            "/usr/share/containers/containers.conf",
            TrustedConfigRead::Present(
                b"[engine]\ncgroup_manager = \"systemd\"\n[network]\nnetwork_backend = \"netavark\"\n"
                    .to_vec(),
            ),
        )
        .with(
            "/var/lib/project-runner/.config/containers/containers.conf",
            TrustedConfigRead::Unknown(RootlessPodmanConfigSourceProblemKind::Unreadable),
        );
    let report = observe_with(&context(), &paths(), &policy(), &filesystem).expect("observation");
    assert_eq!(
        report.assessment.state,
        RootlessPodmanConfigAssessmentState::Unknown
    );
    let json = serde_json::to_string(&report).expect("serialize report");
    assert!(!json.contains(secret));
    assert!(!json.contains("token="));
}

#[test]
fn invalid_utf8_and_malformed_relevant_values_are_unknown() {
    let filesystem = FakeFilesystem::default()
        .with(
            "/etc/containers/containers.conf",
            TrustedConfigRead::Present(vec![0xff]),
        )
        .with(
            "/etc/containers/storage.conf",
            TrustedConfigRead::Present(b"[storage]\ndriver = [\"overlay\"]\n".to_vec()),
        );
    let report = observe_with(&context(), &paths(), &policy(), &filesystem).expect("observation");
    assert_eq!(
        report.sources[1].problem,
        Some(RootlessPodmanConfigSourceProblemKind::InvalidUtf8)
    );
    assert_eq!(
        report.sources[3].problem,
        Some(RootlessPodmanConfigSourceProblemKind::InvalidReviewedSyntax)
    );
}

#[test]
fn metadata_policy_rejects_unsafe_file_shapes() {
    let valid = ConfigMetadata {
        kind: ConfigObjectKind::RegularFile,
        uid: 1001,
        gid: 1001,
        mode: 0o600,
        nlink: 1,
        size: 12,
    };
    assert!(
        validate_file_metadata(
            valid,
            ExpectedOwner::Runner {
                uid: 1001,
                gid: 1001,
            }
        )
        .is_ok()
    );

    let cases = [
        (
            ConfigMetadata {
                kind: ConfigObjectKind::Other,
                ..valid
            },
            RootlessPodmanConfigSourceProblemKind::NonRegularFile,
        ),
        (
            ConfigMetadata { nlink: 2, ..valid },
            RootlessPodmanConfigSourceProblemKind::MultipleHardLinks,
        ),
        (
            ConfigMetadata { uid: 1002, ..valid },
            RootlessPodmanConfigSourceProblemKind::WrongOwner,
        ),
        (
            ConfigMetadata {
                mode: 0o620,
                ..valid
            },
            RootlessPodmanConfigSourceProblemKind::WritableByUntrusted,
        ),
        (
            ConfigMetadata {
                size: MAX_ROOTLESS_PODMAN_CONFIG_BYTES as i64 + 1,
                ..valid
            },
            RootlessPodmanConfigSourceProblemKind::Oversized,
        ),
    ];
    for (metadata, expected) in cases {
        assert_eq!(
            validate_file_metadata(
                metadata,
                ExpectedOwner::Runner {
                    uid: 1001,
                    gid: 1001,
                }
            ),
            Err(expected)
        );
    }
}

#[test]
fn root_and_runner_ownership_are_not_interchangeable() {
    let root_file = ConfigMetadata {
        kind: ConfigObjectKind::RegularFile,
        uid: 0,
        gid: 0,
        mode: 0o644,
        nlink: 1,
        size: 12,
    };
    assert!(validate_file_metadata(root_file, ExpectedOwner::Root).is_ok());
    assert_eq!(
        validate_file_metadata(
            root_file,
            ExpectedOwner::Runner {
                uid: 1001,
                gid: 1001,
            }
        ),
        Err(RootlessPodmanConfigSourceProblemKind::WrongOwner)
    );
}

#[test]
fn context_derives_runner_sources_beneath_reviewed_xdg_config_home() {
    let context = context();
    assert_eq!(
        context.runner_containers_path(),
        Path::new("/var/lib/project-runner/.config/containers/containers.conf")
    );
    assert_eq!(
        context.runner_storage_path(),
        Path::new("/var/lib/project-runner/.config/containers/storage.conf")
    );
}

#[test]
fn unsafe_context_and_relocated_paths_are_rejected() {
    assert!(
        RootlessPodmanConfigObservationContext::new(
            "/var/lib/project-runner",
            "/etc/project-runner",
            "/var/lib/project-runner/.local/share",
            "/run/user/1001",
            1001,
            1001,
        )
        .is_err()
    );
    assert!(
        RootlessPodmanConfigObservationPaths::new(
            "/usr/share//containers/containers.conf",
            "/etc/containers/containers.conf",
            "/etc/containers/storage.conf",
        )
        .is_err()
    );
    assert!(
        RootlessPodmanConfigObservationContext::new(
            "/var/lib/project-runner",
            "/var/lib/project-runner/.config",
            "/var/lib/project-runner/.local/share",
            "/run/user/1001",
            0,
            0,
        )
        .is_err()
    );
}
