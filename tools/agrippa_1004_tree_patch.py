from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    file = Path(path)
    text = file.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one replacement, found {count}")
    file.write_text(text.replace(old, new, 1))


path = "src/bin/glaeda-local-patch-check.rs"

replace_once(
    path,
    "use std::fmt;\nuse std::io::{self, Read as _};\nuse std::path::{Path, PathBuf};\nuse std::time::Duration;",
    "use std::fmt;\nuse std::fs;\nuse std::io::{self, Read as _};\nuse std::path::{Path, PathBuf};\nuse std::sync::atomic::{AtomicU64, Ordering};\nuse std::time::{Duration, SystemTime, UNIX_EPOCH};",
)

replace_once(
    path,
    "    CommandSpec, MAX_CAPTURED_STDIN_BYTES, ProcessExecutor, TimedInputCommandExecutor,\n};",
    "    CommandSpec, MAX_CAPTURED_STDIN_BYTES, ProcessExecutor, TimedCommandExecutor,\n    TimedInputCommandExecutor,\n};",
)

replace_once(
    path,
    "const SHA256_PREFIX: &str = \"sha256:\";",
    "const SHA256_PREFIX: &str = \"sha256:\";\nstatic TEMP_INDEX_SEQUENCE: AtomicU64 = AtomicU64::new(0);",
)

replace_once(
    path,
    "#[derive(Debug, Clone, PartialEq, Eq, Serialize)]\nstruct PatchApplicabilityReport {",
    r'''#[derive(Debug)]
struct TemporaryGitIndex {
    directory: PathBuf,
    path: PathBuf,
}

impl TemporaryGitIndex {
    fn new() -> Result<Self, PatchCheckError> {
        let root = fs::canonicalize(std::env::temp_dir()).map_err(|_| check_unavailable())?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| check_unavailable())?
            .as_nanos();
        for _ in 0..32 {
            let sequence = TEMP_INDEX_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let directory = root.join(format!(
                "glaeda-local-patch-index-{}-{now}-{sequence}",
                std::process::id()
            ));
            match fs::create_dir(&directory) {
                Ok(()) => {
                    let path = directory.join("index");
                    return Ok(Self { directory, path });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(check_unavailable()),
            }
        }
        Err(check_unavailable())
    }
}

impl Drop for TemporaryGitIndex {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PatchApplicabilityReport {''',
)

old_evaluate = r'''fn evaluate_patch(
    repository: &Path,
    expectation: &PatchCheckExpectation,
    patch: &[u8],
    executor: &ProcessExecutor,
) -> Result<PatchApplicabilityReport, PatchCheckError> {
    validate_patch_identity(expectation, patch)?;

    let observer = ProjectCheckoutObserver::new(GIT).map_err(|_| checkout_unavailable())?;
    let before = observer
        .observe(repository, executor)
        .map_err(|_| checkout_unavailable())?;
    validate_base(&before, expectation)?;

    let spec = applicability_command(repository)?;
    let expected_argv = spec.displayed_argv();
    let expected_environment_keys = spec.environment.keys().cloned().collect::<Vec<_>>();
    let record = executor
        .execute_with_input(&spec, patch, CHECK_TIMEOUT)
        .map_err(|_| check_unavailable())?;
    if record.argv != expected_argv || record.environment_keys != expected_environment_keys {
        return Err(check_unavailable());
    }
    let applicable = match (record.success, record.status) {
        (true, Some(0)) => true,
        (false, Some(1)) => false,
        _ => return Err(check_unavailable()),
    };
'''
new_evaluate = r'''fn evaluate_patch(
    repository: &Path,
    expectation: &PatchCheckExpectation,
    patch: &[u8],
    executor: &impl TimedInputCommandExecutor,
) -> Result<PatchApplicabilityReport, PatchCheckError> {
    validate_patch_identity(expectation, patch)?;

    let observer = ProjectCheckoutObserver::new(GIT).map_err(|_| checkout_unavailable())?;
    let before = observer
        .observe(repository, executor)
        .map_err(|_| checkout_unavailable())?;
    validate_base(&before, expectation)?;

    let temporary_index = TemporaryGitIndex::new()?;
    let read_tree = read_tree_command(repository, &temporary_index.path, &expectation.expected_tree)?;
    let expected_read_tree_argv = read_tree.displayed_argv();
    let expected_read_tree_environment = read_tree.environment.keys().cloned().collect::<Vec<_>>();
    let read_tree_record = executor
        .execute_with_timeout(&read_tree, CHECK_TIMEOUT)
        .map_err(|_| check_unavailable())?;
    if read_tree_record.argv != expected_read_tree_argv
        || read_tree_record.environment_keys != expected_read_tree_environment
        || !read_tree_record.success
        || read_tree_record.status != Some(0)
    {
        return Err(check_unavailable());
    }

    let spec = applicability_command(repository, &temporary_index.path)?;
    let expected_argv = spec.displayed_argv();
    let expected_environment_keys = spec.environment.keys().cloned().collect::<Vec<_>>();
    let record = executor
        .execute_with_input(&spec, patch, CHECK_TIMEOUT)
        .map_err(|_| check_unavailable())?;
    if record.argv != expected_argv || record.environment_keys != expected_environment_keys {
        return Err(check_unavailable());
    }
    let applicable = match (record.success, record.status) {
        (true, Some(0)) => true,
        (false, Some(1)) => false,
        _ => return Err(check_unavailable()),
    };
'''
replace_once(path, old_evaluate, new_evaluate)

old_command = r'''fn applicability_command(repository: &Path) -> Result<CommandSpec, PatchCheckError> {
    let repository = repository.to_str().ok_or_else(checkout_unavailable)?;
    Ok(CommandSpec::new(GIT)
        .argument("--no-optional-locks")
        .argument("-c")
        .argument("credential.helper=")
        .argument("-c")
        .argument("core.fsmonitor=false")
        .argument("-c")
        .argument("core.hooksPath=/dev/null")
        .argument("-c")
        .argument("diff.external=")
        .argument("-C")
        .argument(repository)
        .argument("apply")
        .argument("--check")
        .argument("--index")
        .argument("--whitespace=nowarn")
        .argument("-")
        .environment("GIT_ASKPASS", "/bin/false")
        .environment("GIT_ALLOW_PROTOCOL", "")
        .environment("GIT_ATTR_NOSYSTEM", "1")
        .environment("GIT_CONFIG_GLOBAL", "/dev/null")
        .environment("GIT_CONFIG_NOSYSTEM", "1")
        .environment("GIT_CONFIG_SYSTEM", "/dev/null")
        .environment("GIT_NO_LAZY_FETCH", "1")
        .environment("GIT_NO_REPLACE_OBJECTS", "1")
        .environment("GIT_PROTOCOL_FROM_USER", "0")
        .environment("GIT_TERMINAL_PROMPT", "0")
        .environment("LANG", "C")
        .environment("LC_ALL", "C"))
}
'''
new_command = r'''fn git_command(
    repository: &Path,
    index: &Path,
) -> Result<CommandSpec, PatchCheckError> {
    let repository = repository.to_str().ok_or_else(checkout_unavailable)?;
    let index = index.to_str().ok_or_else(check_unavailable)?;
    Ok(CommandSpec::new(GIT)
        .argument("--no-optional-locks")
        .argument("-c")
        .argument("credential.helper=")
        .argument("-c")
        .argument("core.fsmonitor=false")
        .argument("-c")
        .argument("core.hooksPath=/dev/null")
        .argument("-c")
        .argument("diff.external=")
        .argument("-C")
        .argument(repository)
        .environment("GIT_ASKPASS", "/bin/false")
        .environment("GIT_ALLOW_PROTOCOL", "")
        .environment("GIT_ATTR_NOSYSTEM", "1")
        .environment("GIT_CONFIG_GLOBAL", "/dev/null")
        .environment("GIT_CONFIG_NOSYSTEM", "1")
        .environment("GIT_CONFIG_SYSTEM", "/dev/null")
        .environment("GIT_INDEX_FILE", index)
        .environment("GIT_NO_LAZY_FETCH", "1")
        .environment("GIT_NO_REPLACE_OBJECTS", "1")
        .environment("GIT_PROTOCOL_FROM_USER", "0")
        .environment("GIT_TERMINAL_PROMPT", "0")
        .environment("LANG", "C")
        .environment("LC_ALL", "C"))
}

fn read_tree_command(
    repository: &Path,
    index: &Path,
    expected_tree: &GitTreeId,
) -> Result<CommandSpec, PatchCheckError> {
    Ok(git_command(repository, index)?
        .argument("read-tree")
        .argument(expected_tree.as_str()))
}

fn applicability_command(
    repository: &Path,
    index: &Path,
) -> Result<CommandSpec, PatchCheckError> {
    Ok(git_command(repository, index)?
        .argument("apply")
        .argument("--check")
        .argument("--cached")
        .argument("--whitespace=nowarn")
        .argument("-"))
}
'''
replace_once(path, old_command, new_command)

replace_once(
    path,
    "    use std::fs;\n    use std::process::Command;\n    use std::time::{SystemTime, UNIX_EPOCH};",
    "    use std::fs;\n    use std::io;\n    use std::process::Command;\n    use std::sync::atomic::{AtomicU64, Ordering};\n    use std::time::{SystemTime, UNIX_EPOCH};\n\n    use glaeda::process::{CommandExecutor, ExecutionRecord, TimedCommandExecutor};",
)

replace_once(
    path,
    "    const BAD_CONTEXT_PATCH: &[u8] = b\"diff --git a/example.txt b/example.txt\\n--- a/example.txt\\n+++ b/example.txt\\n@@ -1 +1 @@\\n-missing\\n+after\\n\";",
    "    const BAD_CONTEXT_PATCH: &[u8] = b\"diff --git a/example.txt b/example.txt\\n--- a/example.txt\\n+++ b/example.txt\\n@@ -1 +1 @@\\n-missing\\n+after\\n\";\n    static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);",
)

old_fixture = r'''    fn fixture() -> Fixture {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "glaeda-local-patch-check-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("fixture root");'''
new_fixture = r'''    fn fixture() -> Fixture {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temp_root = fs::canonicalize(std::env::temp_dir()).expect("canonical temp root");
        let root = temp_root.join(format!(
            "glaeda-local-patch-check-{}-{nonce}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("fixture root");'''
replace_once(path, old_fixture, new_fixture)

insert_before = "    #[test]\n    fn exact_applicable_patch_is_checked_without_mutating_source() {"
replacement = r'''    struct SwapDuringPatchExecutor {
        inner: ProcessExecutor,
        root: PathBuf,
        parked: PathBuf,
        replacement: PathBuf,
    }

    impl CommandExecutor for SwapDuringPatchExecutor {
        fn execute(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord> {
            self.inner.execute(spec)
        }
    }

    impl TimedCommandExecutor for SwapDuringPatchExecutor {
        fn execute_with_timeout(
            &self,
            spec: &CommandSpec,
            timeout: Duration,
        ) -> io::Result<ExecutionRecord> {
            self.inner.execute_with_timeout(spec, timeout)
        }
    }

    impl TimedInputCommandExecutor for SwapDuringPatchExecutor {
        fn execute_with_input(
            &self,
            spec: &CommandSpec,
            input: &[u8],
            timeout: Duration,
        ) -> io::Result<ExecutionRecord> {
            fs::rename(&self.root, &self.parked)?;
            fs::rename(&self.replacement, &self.root)?;
            let result = self.inner.execute_with_input(spec, input, timeout);
            let restore_replacement = fs::rename(&self.root, &self.replacement);
            let restore_original = fs::rename(&self.parked, &self.root);
            restore_replacement?;
            restore_original?;
            result
        }
    }

    #[test]
    fn transient_same_tree_replacement_cannot_change_applicability() {
        let fixture = fixture();
        let replacement = fixture();
        assert_eq!(fixture.tree, replacement.tree);
        fs::write(replacement.root.join("example.txt"), "replacement\n")
            .expect("dirty replacement");
        let parked = fixture.root.with_file_name(format!(
            "{}-parked",
            fixture.root.file_name().expect("fixture name").to_string_lossy()
        ));
        let executor = SwapDuringPatchExecutor {
            inner: ProcessExecutor,
            root: fixture.root.clone(),
            parked,
            replacement: replacement.root.clone(),
        };

        let report = evaluate_patch(
            &fixture.root,
            &expectation(&fixture, PATCH),
            PATCH,
            &executor,
        )
        .expect("tree-bound applicability");
        assert!(report.applicable);
        assert!(report.source_unchanged);
        assert_eq!(git_output(&fixture.root, &["status", "--porcelain=v1"]), "");
        assert!(!git_output(&replacement.root, &["status", "--porcelain=v1"]).is_empty());
    }

    #[test]
    fn exact_applicable_patch_is_checked_without_mutating_source() {'''
replace_once(path, insert_before, replacement)
