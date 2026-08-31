use std::fs;
use std::io::Write as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use glaeda::project_workspace_identity::{
    ProjectWorkspaceFilesystemIdentityKind, ProjectWorkspaceIdentityGeneration,
    project_workspace_filesystem_identity,
};
use serde_json::Value;
use sha1::{Digest as _, Sha1};
use sha2::Sha256;

const GIT: &str = "/usr/bin/git";
const PATCH_CHECK: &str = env!("CARGO_BIN_EXE_glaeda-local-patch-check");
const INTERNAL_BOUND_APPLY: &str = "--glaeda-internal-bound-patch-apply-v1";
const INTERNAL_SOURCE_CHANGED_EXIT: i32 = 3;
const PATCH: &[u8] = b"diff --git a/example.txt b/example.txt\n--- a/example.txt\n+++ b/example.txt\n@@ -1 +1 @@\n-before\n+after\n";
static FIXTURE_COUNTER: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    head: String,
    tree: String,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn fixture() -> Fixture {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let counter = FIXTURE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp_root = fs::canonicalize(std::env::temp_dir()).expect("canonical temp root");
    let root = temp_root.join(format!(
        "glaeda-patch-binding-{}-{nonce}-{counter}",
        std::process::id()
    ));
    fs::create_dir(&root).expect("fixture root");
    run_git(&root, &["init", "-q"]);
    run_git(&root, &["config", "user.name", "Glaeda Test"]);
    run_git(&root, &["config", "user.email", "glaeda@example.invalid"]);
    fs::write(root.join("example.txt"), "before\n").expect("source");
    run_git(&root, &["add", "example.txt"]);
    run_git(&root, &["commit", "-qm", "base"]);
    let head = git_output(&root, &["rev-parse", "HEAD"]);
    let tree = git_output(&root, &["rev-parse", "HEAD^{tree}"]);
    Fixture { root, head, tree }
}

fn clone_dirty(source: &Fixture) -> Fixture {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let counter = FIXTURE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp_root = fs::canonicalize(std::env::temp_dir()).expect("canonical temp root");
    let root = temp_root.join(format!(
        "glaeda-patch-binding-clone-{}-{nonce}-{counter}",
        std::process::id()
    ));
    let status = Command::new(GIT)
        .env_clear()
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .arg("clone")
        .arg("-q")
        .arg(&source.root)
        .arg(&root)
        .status()
        .expect("clone");
    assert!(status.success(), "clone failed");
    fs::write(root.join("example.txt"), "replacement\n").expect("dirty replacement");
    let head = git_output(&root, &["rev-parse", "HEAD"]);
    let tree = git_output(&root, &["rev-parse", "HEAD^{tree}"]);
    assert_eq!(head, source.head);
    assert_eq!(tree, source.tree);
    Fixture { root, head, tree }
}

fn git(root: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(GIT);
    command
        .env_clear()
        .env("HOME", root)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .arg("-C")
        .arg(root)
        .args(args);
    command
}

fn run_git(root: &Path, args: &[&str]) {
    let status = git(root, args).status().expect("git");
    assert!(status.success(), "git command failed: {args:?}");
}

fn git_output(root: &Path, args: &[&str]) -> String {
    let output = git(root, args).output().expect("git");
    assert!(output.status.success(), "git command failed: {args:?}");
    String::from_utf8(output.stdout)
        .expect("utf8")
        .trim()
        .to_owned()
}

fn materialization(root: &Path) -> String {
    let metadata = fs::metadata(root).expect("metadata");
    project_workspace_filesystem_identity(
        ProjectWorkspaceIdentityGeneration::CURRENT,
        ProjectWorkspaceFilesystemIdentityKind::Materialization,
        metadata.dev(),
        metadata.ino(),
        metadata.uid(),
    )
    .expect("materialization")
    .as_str()
    .to_owned()
}

fn internal_command(root: &Path, expected_materialization: &str) -> Command {
    let mut command = Command::new(PATCH_CHECK);
    command
        .current_dir(root)
        .arg(INTERNAL_BOUND_APPLY)
        .arg(expected_materialization)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn parked_path(root: &Path, suffix: &str) -> PathBuf {
    root.with_file_name(format!(
        "{}-{suffix}",
        root.file_name().expect("fixture name").to_string_lossy()
    ))
}

fn restore_swap(original: &Fixture, replacement: &Fixture, parked: &Path) {
    fs::rename(&original.root, &replacement.root).expect("restore replacement path");
    fs::rename(parked, &original.root).expect("restore original path");
}

#[test]
fn replacement_bound_at_spawn_fails_closed() {
    let original = fixture();
    let replacement = clone_dirty(&original);
    let expected = materialization(&original.root);
    let parked = parked_path(&original.root, "parked-before-spawn");

    fs::rename(&original.root, &parked).expect("park original");
    fs::rename(&replacement.root, &original.root).expect("install replacement");
    let output = internal_command(&original.root, &expected)
        .output()
        .expect("internal helper");
    restore_swap(&original, &replacement, &parked);

    assert_eq!(output.status.code(), Some(INTERNAL_SOURCE_CHANGED_EXIT));
    assert_eq!(
        git_output(&original.root, &["rev-parse", "HEAD"]),
        original.head
    );
    assert_eq!(
        git_output(&original.root, &["status", "--porcelain=v1"]),
        ""
    );
}

#[test]
fn a_to_b_to_a_after_spawn_stays_bound_to_original_checkout() {
    let original = fixture();
    let replacement = clone_dirty(&original);
    let expected = materialization(&original.root);
    let parked = parked_path(&original.root, "parked-after-spawn");

    let mut child = internal_command(&original.root, &expected)
        .spawn()
        .expect("spawn bound helper");

    fs::rename(&original.root, &parked).expect("park original");
    fs::rename(&replacement.root, &original.root).expect("install replacement");
    assert!(!git_output(&original.root, &["status", "--porcelain=v1"]).is_empty());

    let mut stdin = child.stdin.take().expect("helper stdin");
    stdin.write_all(PATCH).expect("write patch");
    drop(stdin);
    let output = child.wait_with_output().expect("wait helper");

    restore_swap(&original, &replacement, &parked);

    assert_eq!(
        output.status.code(),
        Some(0),
        "helper stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        git_output(&original.root, &["rev-parse", "HEAD"]),
        original.head
    );
    assert_eq!(
        git_output(&original.root, &["status", "--porcelain=v1"]),
        ""
    );
    assert!(!git_output(&replacement.root, &["status", "--porcelain=v1"]).is_empty());
}

#[test]
fn public_front_door_uses_bound_helper_and_keeps_receipt_private() {
    let fixture = fixture();
    let blob = git_blob_sha1(PATCH);
    let sha256 = sha256(PATCH);
    let mut child = Command::new(PATCH_CHECK)
        .arg("--repository")
        .arg(&fixture.root)
        .arg("--expected-head")
        .arg(&fixture.head)
        .arg("--expected-tree")
        .arg(&fixture.tree)
        .arg("--git-blob-sha1")
        .arg(blob)
        .arg("--sha256")
        .arg(sha256)
        .arg("--bytes")
        .arg(PATCH.len().to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("public patch check");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(PATCH)
        .expect("write patch");
    let output = child.wait_with_output().expect("wait public patch check");
    assert!(
        output.status.success(),
        "public helper stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: Value = serde_json::from_slice(&output.stdout).expect("report json");
    assert_eq!(json["applicable"], true);
    assert_eq!(json["source_unchanged"], true);
    assert_eq!(json["contains_patch_content"], false);
    assert_eq!(json["contains_private_path"], false);
    let encoded = String::from_utf8(output.stdout).expect("utf8 report");
    assert!(!encoded.contains(fixture.root.to_string_lossy().as_ref()));
    assert!(!encoded.contains("example.txt"));
    assert_eq!(git_output(&fixture.root, &["status", "--porcelain=v1"]), "");
}

fn git_blob_sha1(bytes: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(format!("blob {}\0", bytes.len()).as_bytes());
    hasher.update(bytes);
    lower_hex(&hasher.finalize())
}

fn sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{}", lower_hex(&hasher.finalize()))
}

fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}
