#![cfg(target_os = "linux")]

use std::process::Command;

const BINARY: &str = env!("CARGO_BIN_EXE_glaeda-protected-cache-observe");

#[test]
fn checkout_binary_refuses_before_traversing_or_echoing_the_root() {
    let private_root = "/private/operator/cache/root-do-not-print";
    let output = Command::new(BINARY)
        .args(["--root", private_root, "--output", "json"])
        .output()
        .expect("run protected observer");

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("decode path-free error");
    assert_eq!(
        error["error"]["code"],
        "protected_cache_observer_installation_invalid"
    );
    let encoded = String::from_utf8(output.stderr).expect("UTF-8 error");
    assert!(!encoded.contains(private_root));
}

#[test]
fn help_exposes_no_install_or_arbitrary_program_override() {
    let output = Command::new(BINARY)
        .arg("--help")
        .output()
        .expect("read protected observer help");

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let help = String::from_utf8(output.stdout).expect("UTF-8 help");
    assert!(help.contains("--root"));
    assert!(!help.contains("installed-program"));
    assert!(!help.contains("capability"));
    assert!(!help.contains("sudo"));
}
