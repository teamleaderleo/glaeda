use super::{
    MAX_ROOTLESS_PODMAN_CONFIG_BYTES, MAX_ROOTLESS_PODMAN_CONFIG_LINE_BYTES,
    MAX_ROOTLESS_PODMAN_CONFIG_LINES, MAX_ROOTLESS_PODMAN_CONFIG_VALUE_BYTES,
    RootlessPodmanConfigErrorKind, parse_rootless_podman_containers_config,
    parse_rootless_podman_storage_config,
};

#[test]
fn parses_reviewed_storage_fields_and_ignores_unrelated_arrays() {
    let config = r#"
[storage]
driver = "overlay"
runroot = "/run/user/1001/containers" # reviewed runtime root
graphroot = '$HOME/.local/share/containers/storage'
rootless_storage_path = "/srv/smolrunner/storage"

[storage.options]
additionalimagestores = [
  "/srv/read-only-images",
]

[storage.options.overlay]
mount_program = "/usr/bin/fuse-overlayfs"
mountopt = "nodev"
"#;

    let parsed = parse_rootless_podman_storage_config(config).expect("storage config");
    assert_eq!(parsed.driver.as_deref(), Some("overlay"));
    assert_eq!(parsed.runroot.as_deref(), Some("/run/user/1001/containers"));
    assert_eq!(
        parsed.graphroot.as_deref(),
        Some("$HOME/.local/share/containers/storage")
    );
    assert_eq!(
        parsed.rootless_storage_path.as_deref(),
        Some("/srv/smolrunner/storage")
    );
    assert_eq!(
        parsed.overlay_mount_program.as_deref(),
        Some("/usr/bin/fuse-overlayfs")
    );
}

#[test]
fn parses_containers_fields_without_guessing_missing_defaults() {
    let config = r#"
[engine]
cgroup_manager = "systemd"

[network]
network_backend = "netavark"

[containers]
log_driver = "journald"
"#;

    let parsed = parse_rootless_podman_containers_config(config).expect("containers config");
    assert_eq!(parsed.cgroup_manager.as_deref(), Some("systemd"));
    assert_eq!(parsed.network_backend.as_deref(), Some("netavark"));

    let absent = parse_rootless_podman_containers_config("").expect("empty config");
    assert_eq!(absent.cgroup_manager, None);
    assert_eq!(absent.network_backend, None);
}

#[test]
fn parses_equivalent_bare_dotted_key_forms() {
    let storage = parse_rootless_podman_storage_config(
        r#"
storage . driver = "overlay"
storage.runroot = "/run/user/1001/containers"
[storage]
graphroot = "/srv/storage"
options . overlay . mount_program = "/usr/bin/fuse-overlayfs"
"#,
    )
    .expect("dotted storage keys");
    assert_eq!(storage.driver.as_deref(), Some("overlay"));
    assert_eq!(
        storage.runroot.as_deref(),
        Some("/run/user/1001/containers")
    );
    assert_eq!(storage.graphroot.as_deref(), Some("/srv/storage"));
    assert_eq!(
        storage.overlay_mount_program.as_deref(),
        Some("/usr/bin/fuse-overlayfs")
    );

    let containers = parse_rootless_podman_containers_config(
        "engine.cgroup_manager = \"systemd\"\nnetwork . network_backend = \"netavark\"\n",
    )
    .expect("dotted containers keys");
    assert_eq!(containers.cgroup_manager.as_deref(), Some("systemd"));
    assert_eq!(containers.network_backend.as_deref(), Some("netavark"));
}

#[test]
fn comments_and_equals_inside_strings_do_not_change_assignment_parsing() {
    let config = r#"
[storage]
graphroot = "/srv/containers#stable=1" # actual comment

[storage.options.overlay]
mount_program = "/usr/bin/fuse\\overlayfs"
"#;

    let parsed = parse_rootless_podman_storage_config(config).expect("storage config");
    assert_eq!(
        parsed.graphroot.as_deref(),
        Some("/srv/containers#stable=1")
    );
    assert_eq!(
        parsed.overlay_mount_program.as_deref(),
        Some(r"/usr/bin/fuse\overlayfs")
    );
}

#[test]
fn duplicate_relevant_keys_fail_closed_across_equivalent_forms() {
    for config in [
        "[storage]\ndriver = \"overlay\"\ndriver = \"vfs\"\n",
        "storage.driver = \"overlay\"\n[storage]\ndriver = \"vfs\"\n",
        "[storage.options]\noverlay.mount_program = \"/usr/bin/fuse-overlayfs\"\n[storage.options.overlay]\nmount_program = \"/tmp/other\"\n",
    ] {
        let error = parse_rootless_podman_storage_config(config)
            .expect_err("equivalent duplicate must fail");
        assert_eq!(
            error.kind(),
            RootlessPodmanConfigErrorKind::DuplicateRelevantKey
        );
    }
}

#[test]
fn malformed_relevant_values_fail_but_unknown_complex_values_are_ignored() {
    let unknown = r#"
[storage.options]
additionalimagestores = [
  "/srv/images",
]
"#;
    parse_rootless_podman_storage_config(unknown).expect("unknown arrays are outside the subset");

    for config in [
        "[storage]\ndriver = overlay\n",
        "[storage]\ndriver = \"overlay\n",
        "[storage]\ndriver = \"over\\nlay\"\n",
        "[storage]\ndriver = \"overlay\" trailing\n",
        "[storage]\ndriver = \"\"\"overlay\"\"\"\n",
    ] {
        let error = parse_rootless_podman_storage_config(config)
            .expect_err("malformed relevant value must fail");
        assert_eq!(
            error.kind(),
            RootlessPodmanConfigErrorKind::MalformedRelevantAssignment
        );
    }
}

#[test]
fn relevant_keys_with_missing_or_invalid_assignment_syntax_fail_closed() {
    for config in [
        "[storage]\ndriver \"overlay\"\n",
        "[storage]\ndriver extra = \"overlay\"\n",
        "[storage.options.overlay]\nmount_program: \"/usr/bin/fuse-overlayfs\"\n",
        "storage . driver \"overlay\"\n",
        "storage . \"driver\" = \"overlay\"\n",
        "[storage]\n\"driver\" = \"overlay\"\n",
    ] {
        let error = parse_rootless_podman_storage_config(config)
            .expect_err("malformed relevant assignment must fail");
        assert_eq!(
            error.kind(),
            RootlessPodmanConfigErrorKind::MalformedRelevantAssignment
        );
    }
}

#[test]
fn malformed_tables_fail_while_well_formed_array_tables_remain_outside_the_subset() {
    for config in [
        "[storage\ndriver = \"overlay\"\n",
        "[[storage]\ndriver = \"overlay\"\n",
        "[[]]\ndriver = \"overlay\"\n",
    ] {
        let error =
            parse_rootless_podman_storage_config(config).expect_err("malformed table must fail");
        assert_eq!(error.kind(), RootlessPodmanConfigErrorKind::MalformedTable);
        assert_eq!(error.line(), Some(1));
    }

    let parsed = parse_rootless_podman_storage_config(
        "[[storage]]\ndriver = \"vfs\"\n[storage]\ndriver = \"overlay\"\n",
    )
    .expect("well-formed array table is ignored");
    assert_eq!(parsed.driver.as_deref(), Some("overlay"));
}

#[test]
fn control_characters_fail_closed() {
    let control = parse_rootless_podman_storage_config("[storage]\ndriver = \"over\0lay\"\n")
        .expect_err("control must fail");
    assert_eq!(
        control.kind(),
        RootlessPodmanConfigErrorKind::InvalidControlCharacter
    );
}

#[test]
fn bounded_document_line_and_value_limits_are_enforced() {
    let oversized = "x".repeat(MAX_ROOTLESS_PODMAN_CONFIG_BYTES + 1);
    assert_eq!(
        parse_rootless_podman_storage_config(&oversized)
            .expect_err("oversized input")
            .kind(),
        RootlessPodmanConfigErrorKind::Oversized
    );

    let too_many_lines = "\n".repeat(MAX_ROOTLESS_PODMAN_CONFIG_LINES + 1);
    assert_eq!(
        parse_rootless_podman_storage_config(&too_many_lines)
            .expect_err("too many lines")
            .kind(),
        RootlessPodmanConfigErrorKind::TooManyLines
    );

    let long_line = format!("#{}", "x".repeat(MAX_ROOTLESS_PODMAN_CONFIG_LINE_BYTES));
    assert_eq!(
        parse_rootless_podman_storage_config(&long_line)
            .expect_err("long line")
            .kind(),
        RootlessPodmanConfigErrorKind::LineTooLong
    );

    let long_value = format!(
        "[storage]\ndriver = \"{}\"\n",
        "x".repeat(MAX_ROOTLESS_PODMAN_CONFIG_VALUE_BYTES)
    );
    assert_eq!(
        parse_rootless_podman_storage_config(&long_value)
            .expect_err("long relevant value")
            .kind(),
        RootlessPodmanConfigErrorKind::MalformedRelevantAssignment
    );
}

#[test]
fn document_kind_prevents_cross_file_key_confusion() {
    let storage = parse_rootless_podman_storage_config(
        "[engine]\ncgroup_manager = \"cgroupfs\"\n[network]\nnetwork_backend = \"cni\"\n",
    )
    .expect("storage parser");
    assert_eq!(storage.driver, None);

    let containers = parse_rootless_podman_containers_config(
        "[storage]\ndriver = \"vfs\"\n[storage.options.overlay]\nmount_program = \"/tmp/x\"\n",
    )
    .expect("containers parser");
    assert_eq!(containers.cgroup_manager, None);
    assert_eq!(containers.network_backend, None);
}
