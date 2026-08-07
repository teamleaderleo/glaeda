#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn assert_bash_syntax(path: &Path) {
    let status = Command::new("/bin/bash")
        .arg("-n")
        .arg(path)
        .status()
        .expect("run bash syntax check");
    assert!(status.success(), "bash syntax failed for {}", path.display());
}

#[test]
fn local_listener_contract_is_bounded_and_token_argv_free() {
    let helper_path = repo_path("scripts/local-actions-runner.sh");
    let bridge_path = repo_path("scripts/local-actions-token-bridge.sh");
    assert_bash_syntax(&helper_path);
    assert_bash_syntax(&bridge_path);

    let output = Command::new("/bin/bash")
        .arg(&helper_path)
        .arg("contract")
        .output()
        .expect("run listener contract");
    assert!(output.status.success());
    let contract: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("decode listener contract");

    assert_eq!(contract["schema_version"], 1);
    assert_eq!(contract["contract"], "smolrunner-local-actions-listener");
    assert_eq!(contract["user"], "smolrunner-runner");
    assert_eq!(contract["repository"], "teamleaderleo/smolrunner");
    assert_eq!(contract["runner_name"], "smolrunner-local-arm64");
    assert_eq!(contract["custom_label"], "smolrunner-local-arm64");
    assert_eq!(
        contract["default_labels"],
        serde_json::json!(["self-hosted", "linux", "ARM64"])
    );
    assert_eq!(contract["installation"]["source"], "actions/runner");
    assert_eq!(contract["installation"]["platform"], "linux-arm64");
    assert_eq!(contract["installation"]["exact_version_required"], true);
    assert_eq!(contract["installation"]["sha256_required"], true);
    assert_eq!(contract["installation"]["token_bridge_blob_pinned"], true);
    assert_eq!(contract["installation"]["token_bridge_pinned"], true);
    assert_eq!(contract["installation"]["auto_update"], false);
    assert_eq!(contract["registration"]["identity_fields_verified"], true);
    assert_eq!(
        contract["registration"]["token_source"],
        "stdin_to_installed_bridge_to_secret_environment"
    );
    assert_eq!(contract["registration"]["persistent_token"], false);
    assert_eq!(contract["registration"]["token_in_argv"], false);
    assert_eq!(contract["registration"]["service_install"], false);
    assert_eq!(contract["execution"]["environment"], "allowlist");
    assert_eq!(contract["execution"]["rootless_podman_required"], true);
    assert_eq!(contract["execution"]["privileged_groups"], false);
    assert_eq!(contract["trust"]["forks"], "deny");
    assert_eq!(contract["trust"]["trigger"], "operator");

    let helper = fs::read_to_string(&helper_path).expect("read listener helper");
    let bridge = fs::read_to_string(&bridge_path).expect("read token bridge");

    for required in [
        "expected_token_bridge_blob=\"08c6efa27c3faf40729056c4d797317054058565\"",
        "hash-object --no-filters",
        "actions/runner/releases/download/v${requested_version}/actions-runner-linux-arm64-${requested_version}.tar.gz",
        "--check --status -",
        "token_bridge_sha256=",
        "verify_registration_identity",
        "(.AgentName // .agentName // \"\") == $expected_name",
        "(.GitHubUrl // .gitHubUrl // \"\") == $expected_url",
        "(.WorkFolder // .workFolder // \"\") == $expected_work",
        "(.DisableUpdate // .disableUpdate // false) == true",
        "installed_token_bridge",
        "--labels",
        "--disableupdate",
        "clean_env=(",
        "env_bin",
        "exec \"${clean_env[@]}\" ./run.sh",
        "assert_subordinate_ids",
        "assert_no_privileged_groups",
    ] {
        assert!(helper.contains(required), "missing helper contract: {required}");
    }

    for required in [
        "umask 077",
        "expected_config=\"/home/smolrunner-runner/actions-runner/config.sh\"",
        "IFS= read -r secret_token",
        "export ACTIONS_RUNNER_INPUT_TOKEN=\"${secret_token}\"",
        "unset secret_token",
        "exec \"${config}\" \"$@\"",
    ] {
        assert!(bridge.contains(required), "missing token bridge contract: {required}");
    }

    for forbidden in [
        " --token ",
        "svc.sh",
        "--replace",
        "--no-default-labels",
        "--privileged",
        "SSH_AUTH_SOCK",
        "GITHUB_TOKEN",
        "GH_TOKEN",
        "CONTAINER_HOST=",
        "DOCKER_HOST=",
        "PODMAN_HOST=",
    ] {
        assert!(
            !helper.contains(forbidden) && !bridge.contains(forbidden),
            "forbidden listener authority found: {forbidden}"
        );
    }
}

#[test]
fn invalid_install_and_registration_inputs_fail_before_host_access() {
    let helper = repo_path("scripts/local-actions-runner.sh");

    let mutable_version = Command::new("/bin/bash")
        .arg(&helper)
        .args([
            "install",
            "--version",
            "latest",
            "--sha256",
            "0000000000000000000000000000000000000000000000000000000000000000",
        ])
        .output()
        .expect("run invalid mutable-version check");
    assert!(!mutable_version.status.success());

    let short_digest = Command::new("/bin/bash")
        .arg(&helper)
        .args(["install", "--version", "2.334.0", "--sha256", "deadbeef"])
        .output()
        .expect("run invalid digest check");
    assert!(!short_digest.status.success());

    let token_argument = Command::new("/bin/bash")
        .arg(&helper)
        .args(["register", "--token", "secret"])
        .output()
        .expect("run forbidden token-argument check");
    assert!(!token_argument.status.success());
}

#[test]
fn local_listener_manifest_keeps_trust_and_resource_limits_explicit() {
    let manifest = fs::read_to_string(repo_path("examples/local-ci-runner.yml"))
        .expect("read local listener manifest");

    for required in [
        "repository: teamleaderleo/smolrunner",
        "user: smolrunner-runner",
        "smolrunner-local-arm64",
        "memory: 2GiB",
        "cpus: 2",
        "pids: 768",
        "forks: deny",
        "trigger: operator",
    ] {
        assert!(manifest.contains(required), "missing manifest contract: {required}");
    }
}
