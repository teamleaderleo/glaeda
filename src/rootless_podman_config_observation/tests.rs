use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::rootless_podman_config_resolution::RootlessPodmanConfigAssessmentState;

static NEXT_TEMP_TREE: AtomicU64 = AtomicU64::new(1);

struct TempTree(PathBuf);

impl TempTree {
    fn new() -> Self {
        let sequence = NEXT_TEMP_TREE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::current_dir()
            .expect("current directory")
            .join("target")
            .join(format!(
                "smolrunner-podman-config-observation-{}-{sequence}",
                std::process::id()
            ));
        fs::create_dir_all(&path).expect("create temporary tree");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .expect("secure temporary tree");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

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
fn linux_reader_enforces_no_follow_metadata_and_size_policy() {
    let tree = TempTree::new();
    let home = tree.path().join("home");
    let config_home = home.join(".config");
    let containers = config_home.join("containers");
    let data_home = home.join(".local/share");
    fs::create_dir_all(&containers).expect("create config directories");
    fs::create_dir_all(&data_home).expect("create data directories");
    for directory in [&home, &config_home, &containers, &data_home] {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
            .expect("secure directory");
    }

    let metadata = fs::metadata(tree.path()).expect("temporary tree metadata");
    let uid = metadata.uid();
    let gid = metadata.gid();
    if uid == 0 || gid == 0 {
        return;
    }
    let expected_owner = ExpectedOwner::Runner { uid, gid };
    let source = containers.join("containers.conf");
    let valid = b"[engine]\ncgroup_manager = \"systemd\"\n";

    assert_eq!(
        read_linux_config(&source, expected_owner),
        TrustedConfigRead::Missing
    );

    fs::write(&source, valid).expect("write safe source");
    fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).expect("secure source");
    assert_eq!(
        read_linux_config(&source, expected_owner),
        TrustedConfigRead::Present(valid.to_vec())
    );

    let wrong_uid = if uid == u32::MAX { uid - 1 } else { uid + 1 };
    assert_eq!(
        read_linux_config(
            &source,
            ExpectedOwner::Runner {
                uid: wrong_uid,
                gid,
            },
        ),
        TrustedConfigRead::Unknown(RootlessPodmanConfigSourceProblemKind::WrongOwner)
    );

    fs::set_permissions(&source, fs::Permissions::from_mode(0o620))
        .expect("make source group writable");
    assert_eq!(
        read_linux_config(&source, expected_owner),
        TrustedConfigRead::Unknown(RootlessPodmanConfigSourceProblemKind::WritableByUntrusted)
    );
    fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).expect("restore source mode");

    let hard_link = containers.join("linked.conf");
    fs::hard_link(&source, &hard_link).expect("create hard link");
    assert_eq!(
        read_linux_config(&source, expected_owner),
        TrustedConfigRead::Unknown(RootlessPodmanConfigSourceProblemKind::MultipleHardLinks)
    );
    fs::remove_file(&hard_link).expect("remove hard link");

    fs::write(&source, vec![b'x'; MAX_ROOTLESS_PODMAN_CONFIG_BYTES + 1])
        .expect("write oversized source");
    assert_eq!(
        read_linux_config(&source, expected_owner),
        TrustedConfigRead::Unknown(RootlessPodmanConfigSourceProblemKind::Oversized)
    );

    fs::remove_file(&source).expect("remove oversized source");
    fs::create_dir(&source).expect("create directory at source path");
    assert_eq!(
        read_linux_config(&source, expected_owner),
        TrustedConfigRead::Unknown(RootlessPodmanConfigSourceProblemKind::NonRegularFile)
    );
    fs::remove_dir(&source).expect("remove source directory");

    symlink("/etc/passwd", &source).expect("create final symlink");
    assert_eq!(
        read_linux_config(&source, expected_owner),
        TrustedConfigRead::Unknown(
            RootlessPodmanConfigSourceProblemKind::SymlinkOrInvalidObject
        )
    );
    fs::remove_file(&source).expect("remove final symlink");

    fs::write(&source, valid).expect("rewrite safe source");
    fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).expect("secure source");
    fs::set_permissions(&containers, fs::Permissions::from_mode(0o720))
        .expect("make parent group writable");
    assert_eq!(
        read_linux_config(&source, expected_owner),
        TrustedConfigRead::Unknown(RootlessPodmanConfigSourceProblemKind::UnsafeParentDirectory)
    );
    fs::set_permissions(&containers, fs::Permissions::from_mode(0o700))
        .expect("restore parent mode");

    let real_containers = config_home.join("real-containers");
    fs::rename(&containers, &real_containers).expect("move real directory");
    symlink(&real_containers, &containers).expect("create parent symlink");
    assert_eq!(
        read_linux_config(&source, expected_owner),
        TrustedConfigRead::Unknown(
            RootlessPodmanConfigSourceProblemKind::SymlinkOrInvalidObject
        )
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
