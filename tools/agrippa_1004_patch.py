from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    file = Path(path)
    text = file.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one replacement, found {count}")
    file.write_text(text.replace(old, new, 1))


replace_once(
    "src/process.rs",
    "\n#[derive(Debug, Default, Clone, Copy)]\npub struct ProcessExecutor;",
    r'''

pub trait TimedWorkingDirectoryInputCommandExecutor: TimedCommandExecutor {
    /// Execute one explicit program from one private working directory with bounded plain stdin.
    ///
    /// This composes the reviewed working-directory and plain-input boundaries: input bytes are
    /// written exactly once, the working directory never enters the execution record, output stays
    /// bounded, and timeout/process-group cleanup retains the ordinary timed-executor contract.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for an invalid working directory, timeout, or oversized input.
    /// Spawn, input-write, capture, output-limit, and timeout failures retain the existing process
    /// boundary.
    fn execute_in_directory_with_input(
        &self,
        spec: &CommandSpec,
        working_directory: &Path,
        input: &[u8],
        timeout: Duration,
    ) -> io::Result<ExecutionRecord>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessExecutor;''',
)

replace_once(
    "src/process.rs",
    "\nfn validate_timeout(timeout: Duration) -> io::Result<()> {",
    r'''

impl TimedWorkingDirectoryInputCommandExecutor for ProcessExecutor {
    fn execute_in_directory_with_input(
        &self,
        spec: &CommandSpec,
        working_directory: &Path,
        input: &[u8],
        timeout: Duration,
    ) -> io::Result<ExecutionRecord> {
        validate_timeout(timeout)?;
        validate_plain_stdin(input)?;
        validate_working_directory(working_directory)?;
        execute_process_with_working_directory_input_spawner(
            spec,
            Some(timeout),
            Some(input),
            Some(working_directory),
            &ThreadCaptureSpawner,
        )
    }
}

fn validate_timeout(timeout: Duration) -> io::Result<()> {''',
)

replace_once(
    "src/process.rs",
    "TimedWorkingDirectoryCommandExecutor, Zeroizing, execute_process_with_input_spawner,",
    "TimedWorkingDirectoryCommandExecutor, TimedWorkingDirectoryInputCommandExecutor, Zeroizing, execute_process_with_input_spawner,",
)

replace_once(
    "src/process.rs",
    "    #[test]\n    fn timed_execution_accepts_bounded_deadlines_and_completed_commands() -> io::Result<()> {",
    r'''    #[test]
    fn timed_working_directory_input_combines_private_cwd_and_bounded_stdin() -> io::Result<()> {
        let python = Path::new("/usr/bin/python3");
        if !python.is_file() {
            return Ok(());
        }
        let fixture = timeout_fixture_directory()?;
        let script = "import os,sys; sys.stdout.buffer.write(os.getcwd().encode()+b'\\n'+sys.stdin.buffer.read())";
        let record = ProcessExecutor.execute_in_directory_with_input(
            &CommandSpec::new(python).argument("-c").argument(script),
            &fixture,
            b"bounded-input",
            Duration::from_secs(1),
        )?;
        assert!(record.success);
        assert_eq!(
            record.stdout,
            format!("{}\nbounded-input", fixture.to_string_lossy())
        );

        let oversized = vec![b'x'; MAX_CAPTURED_STDIN_BYTES + 1];
        let error = ProcessExecutor
            .execute_in_directory_with_input(
                &CommandSpec::new("/absolute/program/that/must/not/exist"),
                &fixture,
                &oversized,
                Duration::from_secs(1),
            )
            .expect_err("oversized input must fail before spawn");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        fs::remove_dir(fixture)?;
        Ok(())
    }

    #[test]
    fn timed_execution_accepts_bounded_deadlines_and_completed_commands() -> io::Result<()> {''',
)

replace_once(
    "src/project_checkout_observation.rs",
    r'''    fn matches(&self, metadata: &std::fs::Metadata) -> bool {
        metadata.is_dir()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
            && metadata.uid() == self.owner
    }''',
    r'''    /// Return whether opened directory metadata is the exact observed materialization.
    #[must_use]
    pub fn matches_metadata(&self, metadata: &std::fs::Metadata) -> bool {
        metadata.is_dir()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
            && metadata.uid() == self.owner
    }''',
)

replace_once(
    "src/project_checkout_observation.rs",
    "!location_identity.matches(&final_metadata)",
    "!location_identity.matches_metadata(&final_metadata)",
)

replace_once(
    "src/bin/glaeda-local-patch-check.rs",
    "use std::fmt;\nuse std::io::{self, Read as _};\nuse std::path::{Path, PathBuf};",
    "use std::fmt;\nuse std::fs::File;\nuse std::io::{self, Read as _};\nuse std::os::fd::AsRawFd as _;\nuse std::path::{Path, PathBuf};",
)

replace_once(
    "src/bin/glaeda-local-patch-check.rs",
    "CommandSpec, MAX_CAPTURED_STDIN_BYTES, ProcessExecutor, TimedInputCommandExecutor,",
    "CommandSpec, MAX_CAPTURED_STDIN_BYTES, ProcessExecutor,\n    TimedWorkingDirectoryInputCommandExecutor,",
)

replace_once(
    "src/bin/glaeda-local-patch-check.rs",
    "#[derive(Debug, Clone, PartialEq, Eq, Serialize)]\nstruct PatchApplicabilityReport {",
    r'''struct BoundCheckout {
    _directory: File,
    working_directory: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PatchApplicabilityReport {''',
)

replace_once(
    "src/bin/glaeda-local-patch-check.rs",
    r'''    let spec = applicability_command(repository)?;
    let expected_argv = spec.displayed_argv();
    let expected_environment_keys = spec.environment.keys().cloned().collect::<Vec<_>>();
    let record = executor
        .execute_with_input(&spec, patch, CHECK_TIMEOUT)
        .map_err(|_| check_unavailable())?;''',
    r'''    let bound_checkout = bind_checkout(repository, &before)?;
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
        .map_err(|_| check_unavailable())?;''',
)

replace_once(
    "src/bin/glaeda-local-patch-check.rs",
    r'''fn applicability_command(repository: &Path) -> Result<CommandSpec, PatchCheckError> {
    let repository = repository.to_str().ok_or_else(checkout_unavailable)?;
    Ok(CommandSpec::new(GIT)''',
    r'''fn bind_checkout(
    repository: &Path,
    observation: &ProjectCheckoutObservation,
) -> Result<BoundCheckout, PatchCheckError> {
    let directory = File::open(repository).map_err(|_| source_changed())?;
    let metadata = directory.metadata().map_err(|_| source_changed())?;
    if !observation.location_identity().matches_metadata(&metadata) {
        return Err(source_changed());
    }
    let descriptor = directory.as_raw_fd();
    if descriptor < 0 {
        return Err(source_changed());
    }
    Ok(BoundCheckout {
        _directory: directory,
        working_directory: PathBuf::from(format!("/dev/fd/{descriptor}")),
    })
}

fn applicability_command() -> CommandSpec {
    CommandSpec::new(GIT)''',
)

replace_once(
    "src/bin/glaeda-local-patch-check.rs",
    r'''        .argument("diff.external=")
        .argument("-C")
        .argument(repository)
        .argument("apply")''',
    r'''        .argument("diff.external=")
        .argument("apply")''',
)

replace_once(
    "src/bin/glaeda-local-patch-check.rs",
    '        .environment("LANG", "C")\n        .environment("LC_ALL", "C"))\n}',
    '        .environment("LANG", "C")\n        .environment("LC_ALL", "C")\n}',
)

replace_once(
    "src/bin/glaeda-local-patch-check.rs",
    "    #[test]\n    fn identity_and_v1_input_limit_fail_closed() {",
    r'''    #[test]
    fn replaced_checkout_is_refused_before_descriptor_binding() {
        let fixture = fixture();
        let replacement = fixture();
        let observer = ProjectCheckoutObserver::new(GIT).expect("observer");
        let before = observer
            .observe(&fixture.root, &ProcessExecutor)
            .expect("initial observation");
        let parked = fixture.root.with_file_name(format!(
            "{}-parked",
            fixture.root.file_name().expect("fixture name").to_string_lossy()
        ));

        fs::rename(&fixture.root, &parked).expect("park original");
        fs::rename(&replacement.root, &fixture.root).expect("install replacement");
        let result = bind_checkout(&fixture.root, &before);
        fs::rename(&fixture.root, &replacement.root).expect("restore replacement path");
        fs::rename(&parked, &fixture.root).expect("restore original");

        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("replacement must fail closed before applicability"),
        };
        assert_eq!(error.kind, PatchCheckErrorKind::SourceChanged);
        assert_eq!(
            observer
                .observe(&fixture.root, &ProcessExecutor)
                .expect("restored observation"),
            before
        );
    }

    #[test]
    fn held_checkout_descriptor_survives_a_to_b_to_a_path_replacement() {
        let fixture = fixture();
        let replacement = fixture();
        fs::write(replacement.root.join("example.txt"), "replacement\n")
            .expect("dirty replacement");
        let observer = ProjectCheckoutObserver::new(GIT).expect("observer");
        let before = observer
            .observe(&fixture.root, &ProcessExecutor)
            .expect("initial observation");
        let bound = bind_checkout(&fixture.root, &before).expect("bound checkout");
        let parked = fixture.root.with_file_name(format!(
            "{}-parked",
            fixture.root.file_name().expect("fixture name").to_string_lossy()
        ));

        fs::rename(&fixture.root, &parked).expect("park original");
        fs::rename(&replacement.root, &fixture.root).expect("install replacement");
        assert!(!git_output(&fixture.root, &["status", "--porcelain=v1"]).is_empty());
        let spec = applicability_command();
        let result = ProcessExecutor.execute_in_directory_with_input(
            &spec,
            &bound.working_directory,
            PATCH,
            CHECK_TIMEOUT,
        );
        fs::rename(&fixture.root, &replacement.root).expect("restore replacement path");
        fs::rename(&parked, &fixture.root).expect("restore original");

        let record = result.expect("descriptor-bound applicability");
        assert!(record.success);
        assert_eq!(record.status, Some(0));
        assert!(!record.argv.join(" ").contains(fixture.root.to_string_lossy().as_ref()));
        assert_eq!(
            observer
                .observe(&fixture.root, &ProcessExecutor)
                .expect("restored observation"),
            before
        );
    }

    #[test]
    fn identity_and_v1_input_limit_fail_closed() {''',
)
