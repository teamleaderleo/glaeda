    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::fs;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::rootless_podman_config_resolution::{
        RootlessPodmanConfigAssessmentState, RootlessPodmanConfigPolicy,
    };
    use crate::runner_account_plan::{
        PreparationObservation, PreparationObservationState, RunnerAccountObservations,
    };

    use super::{
        ConfigFilesystem, ExpectedOwner, LinuxConfigFilesystem,
        ROOTLESS_PODMAN_CONFIG_OBSERVATION_SCHEMA_VERSION, RootlessPodmanConfigObservationPaths,
        RootlessPodmanConfigSourceObservationState, RootlessPodmanConfigSourceRole,
        RunnerConfigIdentity, TrustedConfigFile, TrustedConfigFileProblem, observe_with,
        render_human,
    };

    #[derive(Default)]
    struct FakeFilesystem {
        files: BTreeMap<PathBuf, TrustedConfigFile>,
        calls: RefCell<Vec<PathBuf>>,
    }

    impl ConfigFilesystem for FakeFilesystem {
        fn read_trusted(
            &self,
            path: &Path,
            _owner: ExpectedOwner,
            _max_bytes: usize,
        ) -> TrustedConfigFile {
            self.calls.borrow_mut().push(path.to_path_buf());
            self.files
                .get(path)
                .cloned()
                .unwrap_or(TrustedConfigFile::Missing)
        }
    }

    fn account_observations(state: PreparationObservationState) -> RunnerAccountObservations {
        let make = || PreparationObservation::new(state, ["bounded evidence"]).expect("observation");
        RunnerAccountObservations {
            group: make(),
            user: make(),
            home: make(),
            subordinate_uids: make(),
            subordinate_gids: make(),
            linger: make(),
        }
    }

    fn paths() -> RootlessPodmanConfigObservationPaths {
        RootlessPodmanConfigObservationPaths::new(
            "/usr/share/containers/containers.conf",
            "/etc/containers/containers.conf",
            "/etc/containers/storage.conf",
        )
        .expect("paths")
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

    fn identity() -> RunnerConfigIdentity {
        RunnerConfigIdentity {
            uid: 1001,
            gid: 1001,
            group_gid: 1001,
        }
    }

    fn source(report: &super::RootlessPodmanConfigObservationReport, role: RootlessPodmanConfigSourceRole) -> &super::RootlessPodmanConfigSourceObservation {
        report
            .sources
            .iter()
            .find(|source| source.role == role)
            .expect("source observation")
    }

    #[test]
    fn safe_sources_resolve_to_matching_policy() {
        let mut filesystem = FakeFilesystem::default();
        filesystem.files.insert(
            "/etc/containers/containers.conf".into(),
            TrustedConfigFile::Present(
                b"[engine]\ncgroup_manager = \"systemd\"\n[network]\nnetwork_backend = \"netavark\"\n"
                    .to_vec(),
            ),
        );
        filesystem.files.insert(
            "/var/lib/project-runner/.config/containers/storage.conf".into(),
            TrustedConfigFile::Present(
                b"[storage]\ndriver = \"overlay\"\nrunroot = \"$XDG_RUNTIME_DIR/containers\"\ngraphroot = \"$XDG_DATA_HOME/containers/storage\"\n[storage.options.overlay]\nmount_program = \"/usr/bin/fuse-overlayfs\"\n"
                    .to_vec(),
            ),
        );

        let report = observe_with(
            &account_observations(PreparationObservationState::Matching),
            Some(identity()),
            Path::new("/var/lib/project-runner"),
            &policy(),
            &paths(),
            &filesystem,
        )
        .expect("report");

        assert_eq!(
            report.schema_version,
            ROOTLESS_PODMAN_CONFIG_OBSERVATION_SCHEMA_VERSION
        );
        assert_eq!(
            report.assessment.state,
            RootlessPodmanConfigAssessmentState::Matching
        );
        assert_eq!(
            source(&report, RootlessPodmanConfigSourceRole::SystemContainers).state,
            RootlessPodmanConfigSourceObservationState::Present
        );
        assert_eq!(
            source(&report, RootlessPodmanConfigSourceRole::RunnerStorage).state,
            RootlessPodmanConfigSourceObservationState::Present
        );
    }

    #[test]
    fn unknown_runner_source_hides_lower_precedence_values() {
        let mut filesystem = FakeFilesystem::default();
        filesystem.files.insert(
            "/etc/containers/containers.conf".into(),
            TrustedConfigFile::Present(
                b"[engine]\ncgroup_manager = \"systemd\"\n[network]\nnetwork_backend = \"netavark\"\n"
                    .to_vec(),
            ),
        );
        filesystem.files.insert(
            "/var/lib/project-runner/.config/containers/containers.conf".into(),
            TrustedConfigFile::Unknown(TrustedConfigFileProblem::ReadFailed),
        );

        let report = observe_with(
            &account_observations(PreparationObservationState::Matching),
            Some(identity()),
            Path::new("/var/lib/project-runner"),
            &policy(),
            &paths(),
            &filesystem,
        )
        .expect("report");

        assert_eq!(
            report.assessment.state,
            RootlessPodmanConfigAssessmentState::Unknown
        );
        assert_eq!(
            source(&report, RootlessPodmanConfigSourceRole::RunnerContainers).state,
            RootlessPodmanConfigSourceObservationState::Unknown
        );
        assert!(
            report
                .resolved
                .containers
                .network_backend
                .value
                .is_none()
        );
    }

    #[test]
    fn runner_sources_are_not_read_until_identity_and_home_match() {
        let filesystem = FakeFilesystem::default();
        let report = observe_with(
            &account_observations(PreparationObservationState::Unknown),
            None,
            Path::new("/var/lib/project-runner"),
            &policy(),
            &paths(),
            &filesystem,
        )
        .expect("report");

        assert_eq!(filesystem.calls.borrow().len(), 3);
        assert_eq!(
            source(&report, RootlessPodmanConfigSourceRole::RunnerContainers).state,
            RootlessPodmanConfigSourceObservationState::Unknown
        );
        assert_eq!(
            source(&report, RootlessPodmanConfigSourceRole::RunnerStorage).state,
            RootlessPodmanConfigSourceObservationState::Unknown
        );
    }

    #[test]
    fn invalid_utf8_and_malformed_relevant_values_become_unknown() {
        let mut filesystem = FakeFilesystem::default();
        filesystem.files.insert(
            "/etc/containers/containers.conf".into(),
            TrustedConfigFile::Present(vec![0xff, 0xfe]),
        );
        filesystem.files.insert(
            "/etc/containers/storage.conf".into(),
            TrustedConfigFile::Present(b"[storage]\ndriver = overlay\n".to_vec()),
        );

        let report = observe_with(
            &account_observations(PreparationObservationState::Matching),
            Some(identity()),
            Path::new("/var/lib/project-runner"),
            &policy(),
            &paths(),
            &filesystem,
        )
        .expect("report");

        assert_eq!(
            source(&report, RootlessPodmanConfigSourceRole::SystemContainers).state,
            RootlessPodmanConfigSourceObservationState::Unknown
        );
        assert_eq!(
            source(&report, RootlessPodmanConfigSourceRole::SystemStorage).state,
            RootlessPodmanConfigSourceObservationState::Unknown
        );
    }

    #[test]
    fn reports_never_serialize_raw_configuration_or_os_errors() {
        let mut filesystem = FakeFilesystem::default();
        filesystem.files.insert(
            "/etc/containers/containers.conf".into(),
            TrustedConfigFile::Present(
                b"[containers]\nsecret_material = \"DO-NOT-LEAK\"\n".to_vec(),
            ),
        );
        filesystem.files.insert(
            "/var/lib/project-runner/.config/containers/storage.conf".into(),
            TrustedConfigFile::Unknown(TrustedConfigFileProblem::UnsafeTraversal),
        );

        let report = observe_with(
            &account_observations(PreparationObservationState::Matching),
            Some(identity()),
            Path::new("/var/lib/project-runner"),
            &policy(),
            &paths(),
            &filesystem,
        )
        .expect("report");
        let json = serde_json::to_string(&report).expect("serialize report");
        let human = render_human(&report);

        for forbidden in ["DO-NOT-LEAK", "permission denied", "raw configuration"] {
            assert!(!json.contains(forbidden), "{forbidden}");
            assert!(!human.contains(forbidden), "{forbidden}");
        }
    }

    #[test]
    fn metadata_failures_are_fail_closed() {
        let cases = [
            TrustedConfigFileProblem::UnsafeTraversal,
            TrustedConfigFileProblem::MetadataUnavailable,
            TrustedConfigFileProblem::NotRegularFile,
            TrustedConfigFileProblem::WrongOwner,
            TrustedConfigFileProblem::MultipleLinks,
            TrustedConfigFileProblem::WritableByUntrusted,
            TrustedConfigFileProblem::Oversized,
            TrustedConfigFileProblem::ReadFailed,
        ];
        for problem in cases {
            let mut filesystem = FakeFilesystem::default();
            filesystem.files.insert(
                "/etc/containers/containers.conf".into(),
                TrustedConfigFile::Unknown(problem),
            );
            let report = observe_with(
                &account_observations(PreparationObservationState::Matching),
                Some(identity()),
                Path::new("/var/lib/project-runner"),
                &policy(),
                &paths(),
                &filesystem,
            )
            .expect("report");
            assert_eq!(
                source(&report, RootlessPodmanConfigSourceRole::SystemContainers).state,
                RootlessPodmanConfigSourceObservationState::Unknown
            );
        }
    }

    #[test]
    fn linux_reader_rejects_symlinks_modes_links_size_and_wrong_owner() {
        let directory = temporary_directory("reader");
        let safe = directory.join("safe.conf");
        fs::write(&safe, b"[engine]\ncgroup_manager = \"systemd\"\n").expect("write");
        let metadata = fs::metadata(&safe).expect("metadata");
        let owner = ExpectedOwner::Runner(RunnerConfigIdentity {
            uid: metadata.uid(),
            gid: metadata.gid(),
            group_gid: metadata.gid(),
        });
        assert!(matches!(
            LinuxConfigFilesystem.read_trusted(&safe, owner, 65_536),
            TrustedConfigFile::Present(_)
        ));

        let writable = directory.join("writable.conf");
        fs::write(&writable, b"x").expect("write");
        fs::set_permissions(&writable, fs::Permissions::from_mode(0o666)).expect("permissions");
        assert_eq!(
            LinuxConfigFilesystem.read_trusted(&writable, owner, 65_536),
            TrustedConfigFile::Unknown(TrustedConfigFileProblem::WritableByUntrusted)
        );

        let linked = directory.join("linked.conf");
        fs::write(&linked, b"x").expect("write");
        fs::hard_link(&linked, directory.join("linked-copy.conf")).expect("hard link");
        assert_eq!(
            LinuxConfigFilesystem.read_trusted(&linked, owner, 65_536),
            TrustedConfigFile::Unknown(TrustedConfigFileProblem::MultipleLinks)
        );

        let oversized = directory.join("oversized.conf");
        fs::write(&oversized, vec![b'x'; 65_537]).expect("write");
        assert_eq!(
            LinuxConfigFilesystem.read_trusted(&oversized, owner, 65_536),
            TrustedConfigFile::Unknown(TrustedConfigFileProblem::Oversized)
        );

        let target = directory.join("target.conf");
        let link = directory.join("link.conf");
        fs::write(&target, b"x").expect("write");
        symlink(&target, &link).expect("symlink");
        assert_eq!(
            LinuxConfigFilesystem.read_trusted(&link, owner, 65_536),
            TrustedConfigFile::Unknown(TrustedConfigFileProblem::UnsafeTraversal)
        );

        let wrong_owner = ExpectedOwner::Runner(RunnerConfigIdentity {
            uid: metadata.uid().wrapping_add(1),
            gid: metadata.gid(),
            group_gid: metadata.gid(),
        });
        assert_eq!(
            LinuxConfigFilesystem.read_trusted(&safe, wrong_owner, 65_536),
            TrustedConfigFile::Unknown(TrustedConfigFileProblem::WrongOwner)
        );

        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn paths_reject_aliases_and_runner_paths_are_derived_from_home() {
        for path in [
            "etc/containers/containers.conf",
            "/",
            "/etc/containers/../containers.conf",
            "/etc//containers/containers.conf",
            "/etc/containers/containers.conf/",
        ] {
            assert!(
                RootlessPodmanConfigObservationPaths::new(
                    path,
                    "/etc/containers/containers.conf",
                    "/etc/containers/storage.conf",
                )
                .is_err(),
                "{path}"
            );
        }

        let filesystem = FakeFilesystem::default();
        let report = observe_with(
            &account_observations(PreparationObservationState::Unknown),
            None,
            Path::new("/srv/reviewed-runner"),
            &policy(),
            &paths(),
            &filesystem,
        )
        .expect("report");
        assert_eq!(
            source(&report, RootlessPodmanConfigSourceRole::RunnerContainers).path,
            Path::new("/srv/reviewed-runner/.config/containers/containers.conf")
        );
    }

    fn temporary_directory(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "smolrunner-podman-config-observation-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create temporary directory");
        path
    }
