use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::artifact::{CommitId, GitTreeId, Sha256Digest};
use crate::exact_commit_handoff::{ExactCommitHandoffPlan, HandoffExportReceipt, RepositoryPath};

pub const RUNNER_EXPORT_SCHEMA_VERSION: u8 = 1;
pub const DEFAULT_MAX_GIT_OUTPUT_BYTES: usize = 1_048_576;
pub const DEFAULT_MAX_PACKAGE_BYTES: u64 = 536_870_912;
const CAPTURE_BUFFER_BYTES: usize = 8_192;
const EXPORT_REF: &str = "refs/smolrunner/export";
const INSPECTION_REF: &str = "refs/smolrunner/inspect";
static TEMP_DIRECTORY_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerExportRefusalCode {
    InvalidInput,
    DirtyWorktree,
    MovedCandidate,
    ParentDrift,
    TreeDrift,
    ChangedPathsDrift,
    PackageAmbiguous,
    MissingGitEvidence,
    GitSpawnFailed,
    GitCommandFailed,
    UnboundedGitOutput,
    PackageTooLarge,
    PackageIoFailure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerExportPhase {
    InputValidation,
    SourceObservation,
    PackageCreation,
    PackageHashing,
    PackageInspection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunnerExportError {
    pub code: RunnerExportRefusalCode,
    pub phase: RunnerExportPhase,
    pub public_message: String,
}

impl RunnerExportError {
    fn new(
        code: RunnerExportRefusalCode,
        phase: RunnerExportPhase,
        public_message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            phase,
            public_message: public_message.into(),
        }
    }

    fn io(phase: RunnerExportPhase, message: &'static str) -> Self {
        Self::new(RunnerExportRefusalCode::PackageIoFailure, phase, message)
    }
}

impl fmt::Display for RunnerExportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.public_message)
    }
}

impl std::error::Error for RunnerExportError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RunnerExportLimits {
    pub max_git_output_bytes: usize,
    pub max_package_bytes: u64,
}

impl Default for RunnerExportLimits {
    fn default() -> Self {
        Self {
            max_git_output_bytes: DEFAULT_MAX_GIT_OUTPUT_BYTES,
            max_package_bytes: DEFAULT_MAX_PACKAGE_BYTES,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct RunnerExportAdapter {
    git_program: PathBuf,
    limits: RunnerExportLimits,
}

impl fmt::Debug for RunnerExportAdapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RunnerExportAdapter")
            .field("git_program", &"<reviewed-absolute-git-program>")
            .field("limits", &self.limits)
            .finish()
    }
}

impl RunnerExportAdapter {
    /// Construct one narrow Git export adapter.
    ///
    /// # Errors
    ///
    /// Returns an error unless the Git executable is absolute and both output bounds are nonzero.
    pub fn new(
        git_program: impl Into<PathBuf>,
        limits: RunnerExportLimits,
    ) -> Result<Self, RunnerExportError> {
        let git_program = git_program.into();
        if !git_program.is_absolute() {
            return Err(RunnerExportError::new(
                RunnerExportRefusalCode::InvalidInput,
                RunnerExportPhase::InputValidation,
                "the reviewed Git executable path must be absolute",
            ));
        }
        if limits.max_git_output_bytes == 0 || limits.max_package_bytes == 0 {
            return Err(RunnerExportError::new(
                RunnerExportRefusalCode::InvalidInput,
                RunnerExportPhase::InputValidation,
                "runner export output bounds must be nonzero",
            ));
        }
        Ok(Self {
            git_program,
            limits,
        })
    }

    /// Re-observe and export exactly the candidate already accepted by `plan`.
    ///
    /// The adapter invokes only the reviewed Git executable with fixed argument shapes. It does not
    /// accept a command specification, shell fragment, credentials, remote mutation, or transfer
    /// authority.
    ///
    /// # Errors
    ///
    /// Returns a typed refusal for invalid private paths, dirty or moved source state, identity
    /// drift, missing or ambiguous Git evidence, process failure, or exceeded output bounds.
    pub fn execute(
        &self,
        plan: &ExactCommitHandoffPlan,
        repository_root: &Path,
        package_path: &Path,
    ) -> Result<RunnerExportExecutionRecord, RunnerExportError> {
        let paths = ValidatedExportPaths::new(repository_root, package_path)?;
        let planned = PlannedIdentity::from_plan(plan);

        self.observe_source(&paths.repository_root, &planned)?;

        let staging = TemporaryDirectory::create(&paths.package_parent)?;
        let export_repository = staging.path().join("export.git");
        let inspection_repository = staging.path().join("inspection.git");
        let staged_package = staging.path().join("candidate.bundle");

        self.init_bare_repository(
            &paths.package_parent,
            &export_repository,
            RunnerExportPhase::PackageCreation,
        )?;
        self.fetch_candidate(
            &paths.package_parent,
            &export_repository,
            &paths.repository_root,
            planned.commit.as_str(),
            EXPORT_REF,
            RunnerExportPhase::PackageCreation,
        )?;
        self.create_bundle(&paths.package_parent, &export_repository, &staged_package)?;

        // A second observation closes the race between initial validation and package publication.
        self.observe_source(&paths.repository_root, &planned)?;

        publish_package_no_replace(&staged_package, &paths.package_path)?;
        let mut package_guard = PublishedPackageGuard::new(paths.package_path.clone());

        let (package_digest, package_bytes) =
            hash_package(&paths.package_path, self.limits.max_package_bytes)?;

        self.init_bare_repository(
            &paths.package_parent,
            &inspection_repository,
            RunnerExportPhase::PackageInspection,
        )?;
        let listed_commit =
            self.inspect_bundle_head(&inspection_repository, &paths.package_path, EXPORT_REF)?;
        if listed_commit != planned.commit {
            return Err(RunnerExportError::new(
                RunnerExportRefusalCode::MovedCandidate,
                RunnerExportPhase::PackageInspection,
                "the finished package advertises a different candidate commit",
            ));
        }
        self.fetch_candidate(
            &paths.package_parent,
            &inspection_repository,
            &paths.package_path,
            EXPORT_REF,
            INSPECTION_REF,
            RunnerExportPhase::PackageInspection,
        )?;
        let inspected = self.observe_commit(
            &inspection_repository,
            INSPECTION_REF,
            RunnerExportPhase::PackageInspection,
        )?;
        compare_observation(&planned, &inspected, RunnerExportPhase::PackageInspection)?;

        let (final_digest, final_bytes) =
            hash_package(&paths.package_path, self.limits.max_package_bytes)?;
        if final_digest != package_digest || final_bytes != package_bytes {
            return Err(RunnerExportError::new(
                RunnerExportRefusalCode::PackageIoFailure,
                RunnerExportPhase::PackageHashing,
                "the export package changed while it was being inspected",
            ));
        }

        package_guard.disarm();
        Ok(RunnerExportExecutionRecord {
            schema_version: RUNNER_EXPORT_SCHEMA_VERSION,
            package_digest,
            package_bytes,
            exported_commit: inspected.commit,
            exported_parent: inspected.parent,
            exported_tree: inspected.tree,
            changed_paths: inspected.changed_paths,
            package_path: paths.package_path,
        })
    }

    fn observe_source(
        &self,
        repository_root: &Path,
        planned: &PlannedIdentity,
    ) -> Result<(), RunnerExportError> {
        let top_level = self.run_git(
            repository_root,
            RunnerExportPhase::SourceObservation,
            [OsStr::new("rev-parse"), OsStr::new("--show-toplevel")],
        )?;
        let observed_root =
            parse_single_utf8_line(&top_level, RunnerExportPhase::SourceObservation)?;
        let observed_root = fs::canonicalize(observed_root).map_err(|_| {
            RunnerExportError::new(
                RunnerExportRefusalCode::MissingGitEvidence,
                RunnerExportPhase::SourceObservation,
                "Git did not report a canonical worktree root",
            )
        })?;
        if observed_root != repository_root {
            return Err(RunnerExportError::new(
                RunnerExportRefusalCode::MissingGitEvidence,
                RunnerExportPhase::SourceObservation,
                "Git worktree evidence names a different repository root",
            ));
        }

        let status = self.run_git(
            repository_root,
            RunnerExportPhase::SourceObservation,
            [
                OsStr::new("status"),
                OsStr::new("--porcelain=v1"),
                OsStr::new("-z"),
                OsStr::new("--untracked-files=all"),
            ],
        )?;
        if !status.is_empty() {
            return Err(RunnerExportError::new(
                RunnerExportRefusalCode::DirtyWorktree,
                RunnerExportPhase::SourceObservation,
                "the runner worktree is not clean at export time",
            ));
        }

        let observed = self.observe_commit(
            repository_root,
            "HEAD",
            RunnerExportPhase::SourceObservation,
        )?;
        compare_observation(planned, &observed, RunnerExportPhase::SourceObservation)
    }

    fn observe_commit(
        &self,
        repository: &Path,
        revision: &str,
        phase: RunnerExportPhase,
    ) -> Result<ObservedIdentity, RunnerExportError> {
        let commit_expression = format!("{revision}^{{commit}}");
        let commit = self.run_git(
            repository,
            phase,
            [
                OsStr::new("rev-parse"),
                OsStr::new("--verify"),
                OsStr::new(&commit_expression),
            ],
        )?;
        let commit = CommitId::parse(parse_single_utf8_line(&commit, phase)?).map_err(|_| {
            missing_git_evidence(phase, "Git did not report one complete candidate commit")
        })?;

        let parents = self.run_git(
            repository,
            phase,
            [
                OsStr::new("rev-list"),
                OsStr::new("--parents"),
                OsStr::new("-n"),
                OsStr::new("1"),
                OsStr::new(revision),
            ],
        )?;
        let parent = parse_single_parent(&parents, &commit, phase)?;

        let tree_expression = format!("{revision}^{{tree}}");
        let tree = self.run_git(
            repository,
            phase,
            [
                OsStr::new("rev-parse"),
                OsStr::new("--verify"),
                OsStr::new(&tree_expression),
            ],
        )?;
        let tree = GitTreeId::parse(parse_single_utf8_line(&tree, phase)?).map_err(|_| {
            missing_git_evidence(phase, "Git did not report one complete candidate tree")
        })?;

        let changed_paths = self.run_git(
            repository,
            phase,
            [
                OsStr::new("diff-tree"),
                OsStr::new("--no-commit-id"),
                OsStr::new("--no-renames"),
                OsStr::new("--no-ext-diff"),
                OsStr::new("--no-textconv"),
                OsStr::new("--name-only"),
                OsStr::new("-r"),
                OsStr::new("-z"),
                OsStr::new(parent.as_str()),
                OsStr::new(commit.as_str()),
            ],
        )?;
        let changed_paths = parse_changed_paths(&changed_paths, phase)?;

        Ok(ObservedIdentity {
            commit,
            parent,
            tree,
            changed_paths,
        })
    }

    fn init_bare_repository(
        &self,
        cwd: &Path,
        repository: &Path,
        phase: RunnerExportPhase,
    ) -> Result<(), RunnerExportError> {
        self.run_git(
            cwd,
            phase,
            [
                OsStr::new("init"),
                OsStr::new("--quiet"),
                OsStr::new("--bare"),
                repository.as_os_str(),
            ],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn fetch_candidate(
        &self,
        cwd: &Path,
        repository: &Path,
        source: &Path,
        source_revision: &str,
        destination_ref: &str,
        phase: RunnerExportPhase,
    ) -> Result<(), RunnerExportError> {
        let refspec = format!("{source_revision}:{destination_ref}");
        self.run_git(
            cwd,
            phase,
            [
                OsStr::new("-C"),
                repository.as_os_str(),
                OsStr::new("fetch"),
                OsStr::new("--quiet"),
                OsStr::new("--no-tags"),
                OsStr::new("--no-write-fetch-head"),
                OsStr::new("--force"),
                source.as_os_str(),
                OsStr::new(&refspec),
            ],
        )?;
        Ok(())
    }

    fn create_bundle(
        &self,
        cwd: &Path,
        repository: &Path,
        package: &Path,
    ) -> Result<(), RunnerExportError> {
        let arguments = [
            OsStr::new("-C").to_os_string(),
            repository.as_os_str().to_os_string(),
            OsStr::new("bundle").to_os_string(),
            OsStr::new("create").to_os_string(),
            OsStr::new("-").to_os_string(),
            OsStr::new(EXPORT_REF).to_os_string(),
        ];
        let mut command = self.configured_git_command(cwd, &arguments);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|_| {
            RunnerExportError::new(
                RunnerExportRefusalCode::GitSpawnFailed,
                RunnerExportPhase::PackageCreation,
                "the reviewed Git executable could not be started",
            )
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            RunnerExportError::new(
                RunnerExportRefusalCode::GitSpawnFailed,
                RunnerExportPhase::PackageCreation,
                "Git package output was unavailable after requesting bounded capture",
            )
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            RunnerExportError::new(
                RunnerExportRefusalCode::GitSpawnFailed,
                RunnerExportPhase::PackageCreation,
                "Git stderr was unavailable after requesting bounded capture",
            )
        })?;

        let (sender, receiver) = mpsc::channel();
        let package_writer = spawn_package_writer(
            stdout,
            package.to_path_buf(),
            self.limits.max_package_bytes,
            sender.clone(),
        );
        let stderr_reader =
            spawn_bundle_stderr_reader(stderr, self.limits.max_git_output_bytes, sender);
        let mut package_completed = false;
        let mut stderr_completed = false;
        let mut refusal = None;

        while !package_completed || !stderr_completed {
            let event = receiver.recv().map_err(|_| {
                RunnerExportError::new(
                    RunnerExportRefusalCode::GitCommandFailed,
                    RunnerExportPhase::PackageCreation,
                    "bounded Git bundle capture stopped unexpectedly",
                )
            })?;
            match event {
                BundleEvent::PackageLimitExceeded => {
                    refusal.get_or_insert_with(|| {
                        RunnerExportError::new(
                            RunnerExportRefusalCode::PackageTooLarge,
                            RunnerExportPhase::PackageCreation,
                            "the export package exceeded the reviewed byte bound while Git produced it",
                        )
                    });
                    let _ = terminate_child(&mut child);
                }
                BundleEvent::GitOutputLimitExceeded => {
                    refusal.get_or_insert_with(|| {
                        RunnerExportError::new(
                            RunnerExportRefusalCode::UnboundedGitOutput,
                            RunnerExportPhase::PackageCreation,
                            "Git output exceeded the reviewed capture bound",
                        )
                    });
                    let _ = terminate_child(&mut child);
                }
                BundleEvent::PackageCompleted(result) => {
                    package_completed = true;
                    if result.is_err() {
                        refusal.get_or_insert_with(|| {
                            RunnerExportError::io(
                                RunnerExportPhase::PackageCreation,
                                "the bounded export package could not be written",
                            )
                        });
                        let _ = terminate_child(&mut child);
                    }
                }
                BundleEvent::StderrCompleted(result) => {
                    stderr_completed = true;
                    if result.is_err() {
                        refusal.get_or_insert_with(|| {
                            RunnerExportError::new(
                                RunnerExportRefusalCode::GitCommandFailed,
                                RunnerExportPhase::PackageCreation,
                                "bounded Git stderr capture failed",
                            )
                        });
                        let _ = terminate_child(&mut child);
                    }
                }
            }
        }

        let status = child.wait().map_err(|_| {
            RunnerExportError::new(
                RunnerExportRefusalCode::GitCommandFailed,
                RunnerExportPhase::PackageCreation,
                "the Git bundle process could not be reaped",
            )
        })?;
        join_capture_reader(package_writer, RunnerExportPhase::PackageCreation)?;
        join_capture_reader(stderr_reader, RunnerExportPhase::PackageCreation)?;
        if let Some(error) = refusal {
            return Err(error);
        }
        if !status.success() {
            return Err(RunnerExportError::new(
                RunnerExportRefusalCode::GitCommandFailed,
                RunnerExportPhase::PackageCreation,
                "Git could not create the exact reviewed bundle",
            ));
        }
        Ok(())
    }

    fn inspect_bundle_head(
        &self,
        inspection_repository: &Path,
        package: &Path,
        expected_ref: &str,
    ) -> Result<CommitId, RunnerExportError> {
        self.run_git(
            inspection_repository,
            RunnerExportPhase::PackageInspection,
            [
                OsStr::new("bundle"),
                OsStr::new("verify"),
                package.as_os_str(),
            ],
        )?;
        let heads = self.run_git(
            inspection_repository,
            RunnerExportPhase::PackageInspection,
            [
                OsStr::new("bundle"),
                OsStr::new("list-heads"),
                package.as_os_str(),
            ],
        )?;
        parse_bundle_head(&heads, expected_ref)
    }

    fn run_git<I, S>(
        &self,
        cwd: &Path,
        phase: RunnerExportPhase,
        arguments: I,
    ) -> Result<Vec<u8>, RunnerExportError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let arguments = arguments
            .into_iter()
            .map(|argument| argument.as_ref().to_os_string())
            .collect::<Vec<OsString>>();
        let mut command = self.configured_git_command(cwd, &arguments);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command.spawn().map_err(|_| {
            RunnerExportError::new(
                RunnerExportRefusalCode::GitSpawnFailed,
                phase,
                "the reviewed Git executable could not be started",
            )
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            RunnerExportError::new(
                RunnerExportRefusalCode::GitSpawnFailed,
                phase,
                "Git stdout was unavailable after requesting bounded capture",
            )
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            RunnerExportError::new(
                RunnerExportRefusalCode::GitSpawnFailed,
                phase,
                "Git stderr was unavailable after requesting bounded capture",
            )
        })?;

        let (sender, receiver) = mpsc::channel();
        let stdout_reader = spawn_capture_reader(
            stdout,
            CapturedStream::Stdout,
            self.limits.max_git_output_bytes,
            sender.clone(),
        );
        let stderr_reader = spawn_capture_reader(
            stderr,
            CapturedStream::Stderr,
            self.limits.max_git_output_bytes,
            sender,
        );
        let mut stdout_bytes = None;
        let mut stderr_bytes = None;
        let mut exceeded = false;
        let mut capture_failed = false;

        while stdout_bytes.is_none() || stderr_bytes.is_none() {
            let event = receiver.recv().map_err(|_| {
                RunnerExportError::new(
                    RunnerExportRefusalCode::GitCommandFailed,
                    phase,
                    "bounded Git output capture stopped unexpectedly",
                )
            })?;
            match event {
                CaptureEvent::LimitExceeded => {
                    exceeded = true;
                    let _ = terminate_child(&mut child);
                }
                CaptureEvent::Completed(stream, result) => {
                    let bytes = match result {
                        Ok(bytes) => bytes,
                        Err(_) => {
                            capture_failed = true;
                            let _ = terminate_child(&mut child);
                            Vec::new()
                        }
                    };
                    match stream {
                        CapturedStream::Stdout => stdout_bytes = Some(bytes),
                        CapturedStream::Stderr => stderr_bytes = Some(bytes),
                    }
                }
            }
        }

        let status = child.wait().map_err(|_| {
            RunnerExportError::new(
                RunnerExportRefusalCode::GitCommandFailed,
                phase,
                "the Git process could not be reaped",
            )
        })?;
        join_capture_reader(stdout_reader, phase)?;
        join_capture_reader(stderr_reader, phase)?;

        if exceeded {
            return Err(RunnerExportError::new(
                RunnerExportRefusalCode::UnboundedGitOutput,
                phase,
                "Git output exceeded the reviewed capture bound",
            ));
        }
        if capture_failed {
            return Err(RunnerExportError::new(
                RunnerExportRefusalCode::GitCommandFailed,
                phase,
                "bounded Git output capture failed",
            ));
        }
        if !status.success() {
            return Err(RunnerExportError::new(
                RunnerExportRefusalCode::GitCommandFailed,
                phase,
                "Git could not produce the required reviewed evidence",
            ));
        }

        Ok(stdout_bytes.expect("stdout completion recorded"))
    }

    fn configured_git_command(&self, cwd: &Path, arguments: &[OsString]) -> Command {
        let mut command = Command::new(&self.git_program);
        command
            .arg("-c")
            .arg("credential.helper=")
            .arg("-c")
            .arg("core.fsmonitor=false")
            .args(arguments)
            .current_dir(cwd)
            .env_clear()
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", "/bin/false")
            .env("GIT_ALLOW_PROTOCOL", "file")
            .env("GIT_PROTOCOL_FROM_USER", "0")
            .env("LC_ALL", "C")
            .env("LANG", "C");
        command
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct RunnerExportExecutionRecord {
    schema_version: u8,
    package_digest: Sha256Digest,
    package_bytes: u64,
    exported_commit: CommitId,
    exported_parent: CommitId,
    exported_tree: GitTreeId,
    changed_paths: Vec<RepositoryPath>,
    #[serde(skip)]
    package_path: PathBuf,
}

impl fmt::Debug for RunnerExportExecutionRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RunnerExportExecutionRecord")
            .field("schema_version", &self.schema_version)
            .field("package_digest", &self.package_digest)
            .field("package_bytes", &self.package_bytes)
            .field("exported_commit", &self.exported_commit)
            .field("exported_parent", &self.exported_parent)
            .field("exported_tree", &self.exported_tree)
            .field("changed_paths", &self.changed_paths)
            .field("package_path", &"<private-package-path>")
            .finish()
    }
}

impl RunnerExportExecutionRecord {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn package_digest(&self) -> &Sha256Digest {
        &self.package_digest
    }

    #[must_use]
    pub const fn package_bytes(&self) -> u64 {
        self.package_bytes
    }

    #[must_use]
    pub const fn exported_commit(&self) -> &CommitId {
        &self.exported_commit
    }

    #[must_use]
    pub const fn exported_parent(&self) -> &CommitId {
        &self.exported_parent
    }

    #[must_use]
    pub const fn exported_tree(&self) -> &GitTreeId {
        &self.exported_tree
    }

    #[must_use]
    pub fn changed_paths(&self) -> &[RepositoryPath] {
        &self.changed_paths
    }

    #[must_use]
    pub fn package_path(&self) -> &Path {
        &self.package_path
    }

    /// Bind this execution record into the existing pure export receipt contract.
    ///
    /// # Errors
    ///
    /// Returns the existing handoff refusal if the caller supplies a different plan identity.
    pub fn to_handoff_receipt(
        &self,
        plan: &ExactCommitHandoffPlan,
    ) -> Result<HandoffExportReceipt, crate::exact_commit_handoff::ExactCommitHandoffError> {
        HandoffExportReceipt::new(
            plan,
            self.package_digest.clone(),
            self.exported_commit.clone(),
            self.exported_tree.clone(),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedIdentity {
    commit: CommitId,
    parent: CommitId,
    tree: GitTreeId,
    changed_paths: Vec<RepositoryPath>,
}

impl PlannedIdentity {
    fn from_plan(plan: &ExactCommitHandoffPlan) -> Self {
        Self {
            commit: plan.identity().candidate_commit().clone(),
            parent: plan.identity().candidate_parent().clone(),
            tree: plan.identity().tree().clone(),
            changed_paths: plan.identity().changed_paths().to_vec(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObservedIdentity {
    commit: CommitId,
    parent: CommitId,
    tree: GitTreeId,
    changed_paths: Vec<RepositoryPath>,
}

fn compare_observation(
    planned: &PlannedIdentity,
    observed: &ObservedIdentity,
    phase: RunnerExportPhase,
) -> Result<(), RunnerExportError> {
    if observed.commit != planned.commit {
        return Err(RunnerExportError::new(
            RunnerExportRefusalCode::MovedCandidate,
            phase,
            "the runner candidate commit moved after planning",
        ));
    }
    if observed.parent != planned.parent {
        return Err(RunnerExportError::new(
            RunnerExportRefusalCode::ParentDrift,
            phase,
            "the candidate direct parent differs from the planned identity",
        ));
    }
    if observed.tree != planned.tree {
        return Err(RunnerExportError::new(
            RunnerExportRefusalCode::TreeDrift,
            phase,
            "the candidate tree differs from the planned identity",
        ));
    }
    if observed.changed_paths != planned.changed_paths {
        return Err(RunnerExportError::new(
            RunnerExportRefusalCode::ChangedPathsDrift,
            phase,
            "the complete changed-path set differs from the planned identity",
        ));
    }
    Ok(())
}

struct ValidatedExportPaths {
    repository_root: PathBuf,
    package_parent: PathBuf,
    package_path: PathBuf,
}

impl ValidatedExportPaths {
    fn new(repository_root: &Path, package_path: &Path) -> Result<Self, RunnerExportError> {
        if !repository_root.is_absolute() || !package_path.is_absolute() {
            return Err(RunnerExportError::new(
                RunnerExportRefusalCode::InvalidInput,
                RunnerExportPhase::InputValidation,
                "repository and package paths must be absolute",
            ));
        }
        let repository_root = fs::canonicalize(repository_root).map_err(|_| {
            RunnerExportError::new(
                RunnerExportRefusalCode::InvalidInput,
                RunnerExportPhase::InputValidation,
                "the runner repository root must exist and be canonical",
            )
        })?;
        if !repository_root.is_dir() {
            return Err(RunnerExportError::new(
                RunnerExportRefusalCode::InvalidInput,
                RunnerExportPhase::InputValidation,
                "the runner repository root must be a directory",
            ));
        }
        let package_parent = package_path.parent().ok_or_else(|| {
            RunnerExportError::new(
                RunnerExportRefusalCode::InvalidInput,
                RunnerExportPhase::InputValidation,
                "the package path must have an existing parent directory",
            )
        })?;
        let package_parent = fs::canonicalize(package_parent).map_err(|_| {
            RunnerExportError::new(
                RunnerExportRefusalCode::InvalidInput,
                RunnerExportPhase::InputValidation,
                "the package parent directory must exist and be canonical",
            )
        })?;
        if !package_parent.is_dir() || package_parent.starts_with(&repository_root) {
            return Err(RunnerExportError::new(
                RunnerExportRefusalCode::InvalidInput,
                RunnerExportPhase::InputValidation,
                "the package parent must be a directory outside the runner worktree",
            ));
        }
        let file_name = package_path.file_name().ok_or_else(|| {
            RunnerExportError::new(
                RunnerExportRefusalCode::InvalidInput,
                RunnerExportPhase::InputValidation,
                "the package path must include a file name",
            )
        })?;
        let package_path = package_parent.join(file_name);
        if package_path.exists() {
            return Err(RunnerExportError::new(
                RunnerExportRefusalCode::InvalidInput,
                RunnerExportPhase::InputValidation,
                "the final package path must not already exist",
            ));
        }
        Ok(Self {
            repository_root,
            package_parent,
            package_path,
        })
    }
}

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn create(parent: &Path) -> Result<Self, RunnerExportError> {
        for _ in 0..128 {
            let counter = TEMP_DIRECTORY_COUNTER.fetch_add(1, Ordering::Relaxed);
            let candidate = parent.join(format!(
                ".smolrunner-export-{}-{counter}",
                std::process::id()
            ));
            match fs::create_dir(&candidate) {
                Ok(()) => return Ok(Self(candidate)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(_) => {
                    return Err(RunnerExportError::io(
                        RunnerExportPhase::PackageCreation,
                        "the isolated export namespace could not be created",
                    ));
                }
            }
        }
        Err(RunnerExportError::io(
            RunnerExportPhase::PackageCreation,
            "a unique isolated export namespace could not be allocated",
        ))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct PublishedPackageGuard {
    path: PathBuf,
    armed: bool,
}

impl PublishedPackageGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PublishedPackageGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn publish_package_no_replace(
    staged_package: &Path,
    package_path: &Path,
) -> Result<(), RunnerExportError> {
    fs::hard_link(staged_package, package_path).map_err(|_| {
        RunnerExportError::io(
            RunnerExportPhase::PackageCreation,
            "the finished export package could not be published atomically without replacing an existing file",
        )
    })?;
    let _ = fs::remove_file(staged_package);
    Ok(())
}

fn hash_package(
    package_path: &Path,
    max_package_bytes: u64,
) -> Result<(Sha256Digest, u64), RunnerExportError> {
    let mut file = File::open(package_path).map_err(|_| {
        RunnerExportError::io(
            RunnerExportPhase::PackageHashing,
            "the finished package could not be opened for hashing",
        )
    })?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|_| {
            RunnerExportError::io(
                RunnerExportPhase::PackageHashing,
                "the finished package could not be read completely",
            )
        })?;
        if count == 0 {
            break;
        }
        total = total.checked_add(count as u64).ok_or_else(|| {
            RunnerExportError::new(
                RunnerExportRefusalCode::PackageTooLarge,
                RunnerExportPhase::PackageHashing,
                "the export package size exceeded the reviewed bound",
            )
        })?;
        if total > max_package_bytes {
            return Err(RunnerExportError::new(
                RunnerExportRefusalCode::PackageTooLarge,
                RunnerExportPhase::PackageHashing,
                "the export package size exceeded the reviewed bound",
            ));
        }
        hasher.update(&buffer[..count]);
    }
    let digest = format!("sha256:{:x}", hasher.finalize());
    let digest = Sha256Digest::parse(&digest).map_err(|_| {
        RunnerExportError::io(
            RunnerExportPhase::PackageHashing,
            "the package digest could not be represented canonically",
        )
    })?;
    Ok((digest, total))
}

fn parse_single_utf8_line(
    output: &[u8],
    phase: RunnerExportPhase,
) -> Result<&str, RunnerExportError> {
    let value = std::str::from_utf8(output)
        .map_err(|_| missing_git_evidence(phase, "Git evidence was not valid UTF-8"))?
        .trim_end_matches(['\r', '\n']);
    if value.is_empty()
        || value
            .chars()
            .any(|character| matches!(character, '\r' | '\n'))
    {
        return Err(missing_git_evidence(
            phase,
            "Git did not report exactly one bounded evidence line",
        ));
    }
    Ok(value)
}

fn parse_single_parent(
    output: &[u8],
    expected_commit: &CommitId,
    phase: RunnerExportPhase,
) -> Result<CommitId, RunnerExportError> {
    let line = parse_single_utf8_line(output, phase)?;
    let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
    if fields.len() != 2 || fields[0] != expected_commit.as_str() {
        return Err(missing_git_evidence(
            phase,
            "the candidate must have exactly one directly observed parent",
        ));
    }
    CommitId::parse(fields[1])
        .map_err(|_| missing_git_evidence(phase, "Git did not report one complete direct parent"))
}

fn parse_changed_paths(
    output: &[u8],
    phase: RunnerExportPhase,
) -> Result<Vec<RepositoryPath>, RunnerExportError> {
    if output.is_empty() || !output.ends_with(&[0]) {
        return Err(missing_git_evidence(
            phase,
            "Git did not report a complete NUL-terminated changed-path set",
        ));
    }
    let mut paths = output[..output.len() - 1]
        .split(|byte| *byte == 0)
        .map(|raw| {
            let value = std::str::from_utf8(raw)
                .map_err(|_| missing_git_evidence(phase, "a changed path was not valid UTF-8"))?;
            RepositoryPath::parse(value).map_err(|_| {
                missing_git_evidence(phase, "a changed path was not canonically representable")
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if paths.is_empty() {
        return Err(missing_git_evidence(
            phase,
            "Git reported no changed paths for the candidate commit",
        ));
    }
    paths.sort();
    let original_len = paths.len();
    paths.dedup();
    if paths.len() != original_len {
        return Err(missing_git_evidence(
            phase,
            "Git reported duplicate changed-path evidence",
        ));
    }
    Ok(paths)
}

fn parse_bundle_head(output: &[u8], expected_ref: &str) -> Result<CommitId, RunnerExportError> {
    let text = std::str::from_utf8(output).map_err(|_| {
        missing_git_evidence(
            RunnerExportPhase::PackageInspection,
            "bundle head evidence was not valid UTF-8",
        )
    })?;
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() != 1 {
        return Err(RunnerExportError::new(
            RunnerExportRefusalCode::PackageAmbiguous,
            RunnerExportPhase::PackageInspection,
            "the finished package must advertise exactly one reviewed head",
        ));
    }
    let mut fields = lines[0].split_ascii_whitespace();
    let commit = fields.next().unwrap_or_default();
    let reference = fields.next().unwrap_or_default();
    if fields.next().is_some() || reference != expected_ref {
        return Err(RunnerExportError::new(
            RunnerExportRefusalCode::PackageAmbiguous,
            RunnerExportPhase::PackageInspection,
            "the finished package advertises an unexpected head identity",
        ));
    }
    CommitId::parse(commit).map_err(|_| {
        missing_git_evidence(
            RunnerExportPhase::PackageInspection,
            "the package did not advertise one complete commit identity",
        )
    })
}

fn missing_git_evidence(phase: RunnerExportPhase, message: &'static str) -> RunnerExportError {
    RunnerExportError::new(RunnerExportRefusalCode::MissingGitEvidence, phase, message)
}

enum BundleEvent {
    PackageLimitExceeded,
    GitOutputLimitExceeded,
    PackageCompleted(io::Result<()>),
    StderrCompleted(io::Result<()>),
}

fn spawn_package_writer(
    reader: impl Read + Send + 'static,
    package: PathBuf,
    limit: u64,
    sender: Sender<BundleEvent>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let result = write_bounded_package(reader, &package, limit, &sender);
        let _ = sender.send(BundleEvent::PackageCompleted(result));
    })
}

fn write_bounded_package(
    mut reader: impl Read,
    package: &Path,
    limit: u64,
    sender: &Sender<BundleEvent>,
) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(package)?;
    let mut buffer = [0_u8; 64 * 1024];
    let mut written = 0_u64;
    let mut limit_reported = false;
    loop {
        let count = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if limit_reported {
            continue;
        }
        let next = written
            .checked_add(count as u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::FileTooLarge, "package size overflow"))?;
        if next > limit {
            limit_reported = true;
            let _ = sender.send(BundleEvent::PackageLimitExceeded);
            continue;
        }
        file.write_all(&buffer[..count])?;
        written = next;
    }
    file.flush()?;
    Ok(())
}

fn spawn_bundle_stderr_reader(
    reader: impl Read + Send + 'static,
    limit: usize,
    sender: Sender<BundleEvent>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let result = capture_bundle_stderr(reader, limit, &sender);
        let _ = sender.send(BundleEvent::StderrCompleted(result));
    })
}

fn capture_bundle_stderr(
    mut reader: impl Read,
    limit: usize,
    sender: &Sender<BundleEvent>,
) -> io::Result<()> {
    let mut observed = 0_usize;
    let mut buffer = [0_u8; CAPTURE_BUFFER_BYTES];
    let mut limit_reported = false;
    loop {
        let count = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if limit_reported {
            continue;
        }
        let next = observed.saturating_add(count);
        if next > limit {
            limit_reported = true;
            let _ = sender.send(BundleEvent::GitOutputLimitExceeded);
            continue;
        }
        observed = next;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CapturedStream {
    Stdout,
    Stderr,
}

enum CaptureEvent {
    LimitExceeded,
    Completed(CapturedStream, io::Result<Vec<u8>>),
}

fn spawn_capture_reader(
    reader: impl Read + Send + 'static,
    stream: CapturedStream,
    limit: usize,
    sender: Sender<CaptureEvent>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let result = capture_stream(reader, limit, &sender);
        let _ = sender.send(CaptureEvent::Completed(stream, result));
    })
}

fn capture_stream(
    mut reader: impl Read,
    limit: usize,
    sender: &Sender<CaptureEvent>,
) -> io::Result<Vec<u8>> {
    let mut captured = Vec::with_capacity(CAPTURE_BUFFER_BYTES.min(limit));
    let mut buffer = [0_u8; CAPTURE_BUFFER_BYTES];
    let mut limit_reported = false;
    loop {
        let count = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if limit_reported {
            continue;
        }
        let remaining = limit.saturating_sub(captured.len());
        let retained = remaining.min(count);
        captured.extend_from_slice(&buffer[..retained]);
        if retained < count {
            limit_reported = true;
            let _ = sender.send(CaptureEvent::LimitExceeded);
        }
    }
    Ok(captured)
}

fn terminate_child(child: &mut Child) -> io::Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    match child.kill() {
        Ok(()) => Ok(()),
        Err(_) if child.try_wait()?.is_some() => Ok(()),
        Err(error) => Err(error),
    }
}

fn join_capture_reader(
    handle: JoinHandle<()>,
    phase: RunnerExportPhase,
) -> Result<(), RunnerExportError> {
    handle.join().map_err(|_| {
        RunnerExportError::new(
            RunnerExportRefusalCode::GitCommandFailed,
            phase,
            "a bounded Git output capture worker stopped unexpectedly",
        )
    })
}

#[cfg(test)]
mod tests;
