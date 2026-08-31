from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    file = Path(path)
    text = file.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one replacement, found {count}")
    file.write_text(text.replace(old, new, 1))


path = Path("src/bin/glaeda-local-patch-check.rs")

replace_once(
    str(path),
    '''use std::fmt;
use std::fs::File;
use std::io::{self, Read as _};
use std::os::fd::AsRawFd as _;
use std::path::{Path, PathBuf};
use std::time::Duration;''',
    '''use std::env;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io::{self, Read as _};
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;''',
)

replace_once(
    str(path),
    '''use glaeda::project_checkout_observation::{ProjectCheckoutObservation, ProjectCheckoutObserver};''',
    '''use glaeda::project_checkout_observation::{ProjectCheckoutObservation, ProjectCheckoutObserver};
use glaeda::project_workspace_identity::{
    ProjectWorkspaceFilesystemIdentityKind, ProjectWorkspaceIdentityGeneration,
    project_workspace_filesystem_identity,
};''',
)

replace_once(
    str(path),
    '''const SHA256_PREFIX: &str = "sha256:";''',
    '''const SHA256_PREFIX: &str = "sha256:";
const INTERNAL_BOUND_APPLY: &str = "--glaeda-internal-bound-patch-apply-v1";
const INTERNAL_SOURCE_CHANGED_EXIT: i32 = 3;
const GIT_APPLY_ARGUMENTS: &[&str] = &[
    "--no-optional-locks",
    "-c",
    "credential.helper=",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "diff.external=",
    "apply",
    "--check",
    "--index",
    "--whitespace=nowarn",
    "-",
];
const GIT_ENVIRONMENT: &[(&str, &str)] = &[
    ("GIT_ASKPASS", "/bin/false"),
    ("GIT_ALLOW_PROTOCOL", ""),
    ("GIT_ATTR_NOSYSTEM", "1"),
    ("GIT_CONFIG_GLOBAL", "/dev/null"),
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_CONFIG_SYSTEM", "/dev/null"),
    ("GIT_NO_LAZY_FETCH", "1"),
    ("GIT_NO_REPLACE_OBJECTS", "1"),
    ("GIT_PROTOCOL_FROM_USER", "0"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("LANG", "C"),
    ("LC_ALL", "C"),
];''',
)

replace_once(
    str(path),
    '''struct BoundCheckout {
    _directory: File,
    working_directory: PathBuf,
}

''',
    "",
)

replace_once(
    str(path),
    '''fn main() {
    let args = Args::parse();''',
    '''fn main() {
    let mut process_args = env::args_os();
    let _program = process_args.next();
    if process_args.next().as_deref() == Some(OsStr::new(INTERNAL_BOUND_APPLY)) {
        let expected = process_args.next();
        let extra = process_args.next();
        let exit = match (expected, extra) {
            (Some(expected), None) => run_internal_bound_apply(expected.as_os_str()),
            _ => 2,
        };
        std::process::exit(exit);
    }

    let args = Args::parse();''',
)

replace_once(
    str(path),
    '''fn evaluate_patch(
    repository: &Path,
    expectation: &PatchCheckExpectation,
    patch: &[u8],
    executor: &ProcessExecutor,
) -> Result<PatchApplicabilityReport, PatchCheckError> {
    validate_patch_identity(expectation, patch)?;''',
    '''#[cfg(not(test))]
fn evaluate_patch(
    repository: &Path,
    expectation: &PatchCheckExpectation,
    patch: &[u8],
    executor: &ProcessExecutor,
) -> Result<PatchApplicabilityReport, PatchCheckError> {
    let helper_program = env::current_exe().map_err(|_| check_unavailable())?;
    evaluate_patch_with_runner(
        repository,
        expectation,
        patch,
        executor,
        Some(&helper_program),
    )
}

#[cfg(test)]
fn evaluate_patch(
    repository: &Path,
    expectation: &PatchCheckExpectation,
    patch: &[u8],
    executor: &ProcessExecutor,
) -> Result<PatchApplicabilityReport, PatchCheckError> {
    evaluate_patch_with_runner(repository, expectation, patch, executor, None)
}

fn evaluate_patch_with_runner(
    repository: &Path,
    expectation: &PatchCheckExpectation,
    patch: &[u8],
    executor: &ProcessExecutor,
    helper_program: Option<&Path>,
) -> Result<PatchApplicabilityReport, PatchCheckError> {
    validate_patch_identity(expectation, patch)?;''',
)

replace_once(
    str(path),
    '''    let bound_checkout = bind_checkout(repository, &before)?;
    let spec = applicability_command();
    let expected_argv = spec.displayed_argv();
    let expected_environment_keys = spec.environment.keys().cloned().collect::<Vec<_>>();
    let record = executor
        .execute_in_directory_with_input(
            &spec,
            &bound_checkout.working_directory,
            patch,
            CHECK_TIMEOUT,
        )
        .map_err(|_| check_unavailable())?;
    if record.argv != expected_argv || record.environment_keys != expected_environment_keys {
        return Err(check_unavailable());
    }
    let applicable = match (record.success, record.status) {
        (true, Some(0)) => true,
        (false, Some(1)) => false,
        _ => return Err(check_unavailable()),
    };''',
    '''    let spec = match helper_program {
        Some(helper_program) => {
            internal_applicability_command(helper_program, before.materialization_id())
        }
        None => applicability_command(),
    };
    let expected_argv = spec.displayed_argv();
    let expected_environment_keys = spec.environment.keys().cloned().collect::<Vec<_>>();
    let record = executor
        .execute_in_directory_with_input(&spec, repository, patch, CHECK_TIMEOUT)
        .map_err(|_| check_unavailable())?;
    if record.argv != expected_argv || record.environment_keys != expected_environment_keys {
        return Err(check_unavailable());
    }
    let applicable = match (record.success, record.status) {
        (true, Some(0)) => true,
        (false, Some(1)) => false,
        (false, Some(INTERNAL_SOURCE_CHANGED_EXIT)) if helper_program.is_some() => {
            return Err(source_changed());
        }
        _ => return Err(check_unavailable()),
    };''',
)

text = path.read_text()
start = text.find("fn bind_checkout(")
end = text.find("fn applicability_command()", start)
if start == -1 or end == -1:
    raise SystemExit("descriptor binding block anchors moved")
replacement = r'''fn run_internal_bound_apply(expected: &OsStr) -> i32 {
    let Some(expected) = expected.to_str() else {
        return 2;
    };
    let Ok(expected) = Sha256Digest::parse(expected) else {
        return 2;
    };
    let Ok(metadata) = fs::metadata(".") else {
        return 2;
    };
    if !metadata.is_dir() {
        return 2;
    }
    let Ok(actual) = project_workspace_filesystem_identity(
        ProjectWorkspaceIdentityGeneration::CURRENT,
        ProjectWorkspaceFilesystemIdentityKind::Materialization,
        metadata.dev(),
        metadata.ino(),
        metadata.uid(),
    ) else {
        return 2;
    };
    if actual != expected {
        return INTERNAL_SOURCE_CHANGED_EXIT;
    }

    let mut command = Command::new(GIT);
    command.env_clear().args(GIT_APPLY_ARGUMENTS);
    for (key, value) in GIT_ENVIRONMENT {
        command.env(key, value);
    }
    let error = command.exec();
    eprintln!("glaeda-local-patch-check internal Git exec failed: {error}");
    2
}

fn internal_applicability_command(
    program: &Path,
    materialization_id: &Sha256Digest,
) -> CommandSpec {
    CommandSpec::new(program)
        .argument(INTERNAL_BOUND_APPLY)
        .argument(materialization_id.as_str())
}

'''
path.write_text(text[:start] + replacement + text[end:])

text = path.read_text()
start = text.find("fn applicability_command() -> CommandSpec {")
end = text.find("\nfn validate_patch_identity(", start)
if start == -1 or end == -1:
    raise SystemExit("applicability command anchors moved")
replacement = r'''fn applicability_command() -> CommandSpec {
    let mut spec = CommandSpec::new(GIT);
    for argument in GIT_APPLY_ARGUMENTS {
        spec = spec.argument(*argument);
    }
    for (key, value) in GIT_ENVIRONMENT {
        spec = spec.environment(*key, *value);
    }
    spec
}
'''
path.write_text(text[:start] + replacement + text[end:])

replace_once(
    str(path),
    '''    use std::fs;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};''',
    '''    use std::fs;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};''',
)
replace_once(
    str(path),
    '''    const BAD_CONTEXT_PATCH: &[u8] = b"diff --git a/example.txt b/example.txt\\n--- a/example.txt\\n+++ b/example.txt\\n@@ -1 +1 @@\\n-missing\\n+after\\n";''',
    '''    const BAD_CONTEXT_PATCH: &[u8] = b"diff --git a/example.txt b/example.txt\\n--- a/example.txt\\n+++ b/example.txt\\n@@ -1 +1 @@\\n-missing\\n+after\\n";
    static FIXTURE_COUNTER: AtomicU64 = AtomicU64::new(0);''',
)
replace_once(
    str(path),
    '''        let root = std::env::temp_dir().join(format!(
            "glaeda-local-patch-check-{}-{nonce}",
            std::process::id()
        ));''',
    '''        let counter = FIXTURE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp_root = fs::canonicalize(std::env::temp_dir()).expect("canonical temp root");
        let root = temp_root.join(format!(
            "glaeda-local-patch-check-{}-{nonce}-{counter}",
            std::process::id()
        ));''',
)

text = path.read_text()
start = text.find("    #[test]\n    fn replaced_checkout_is_refused_before_descriptor_binding()")
end = text.find("    #[test]\n    fn identity_and_v1_input_limit_fail_closed()", start)
if start == -1 or end == -1:
    raise SystemExit("descriptor unit-test anchors moved")
path.write_text(text[:start] + text[end:])

replace_once(
    "src/project_checkout_observation.rs",
    '''    /// Return whether opened directory metadata is the exact observed materialization.
    #[must_use]
    pub fn matches_metadata(&self, metadata: &std::fs::Metadata) -> bool {''',
    '''    fn matches(&self, metadata: &std::fs::Metadata) -> bool {''',
)
replace_once(
    "src/project_checkout_observation.rs",
    "!location_identity.matches_metadata(&final_metadata)",
    "!location_identity.matches(&final_metadata)",
)

integration = r'''use std::fs;
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
    assert_eq!(git_output(&original.root, &["rev-parse", "HEAD"]), original.head);
    assert_eq!(git_output(&original.root, &["status", "--porcelain=v1"]), "");
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
    assert_eq!(git_output(&original.root, &["rev-parse", "HEAD"]), original.head);
    assert_eq!(git_output(&original.root, &["status", "--porcelain=v1"]), "");
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
'''
Path("tests").mkdir(exist_ok=True)
Path("tests/local_patch_check_binding.rs").write_text(integration)
