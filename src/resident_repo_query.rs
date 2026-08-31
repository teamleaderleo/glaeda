//! One fixed, bounded `repo-query/v1` evidence bundle for exact candidate review.
//!
//! Git owns repository semantics. This module contributes an explicit scrubbed process boundary,
//! exact object coordinates, aggregate limits, and a compact typed result. It performs no fetch,
//! checkout, ref resolution, mutation, publication, or result-reuse decision.

use std::fmt;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::{CommitId, GitTreeId};
use crate::process::{CommandSpec, ExecutionRecord, TimedCommandExecutor};
use crate::project_catalog::{GitHubProjectSource, ProjectIdentity};

pub const RESIDENT_REPO_QUERY_SCHEMA_VERSION: u8 = 1;
pub const RESIDENT_REPO_QUERY_PROFILE_ID: &str = "repo-query/v1";
pub const DEFAULT_PATCH_BYTES: usize = 16 * 1024;
pub const MAX_PATCH_BYTES: usize = 64 * 1024;
pub const MAX_CHANGED_FILES: usize = 512;
pub const MAX_GIT_OUTPUT_BYTES: usize = 1024 * 1024;
pub const REPO_QUERY_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

const PROFILE_CONTRACT: &[u8] = concat!(
    "glaeda-repo-query-profile-v1\0",
    "exact-commit-objects\0merge-base\0ancestry\0commit-count\0",
    "numstat-no-renames\0bounded-complete-patch\0network-disabled\0",
    "checkout-and-git-directory-identity-stable\0origin-reobserved\0"
)
.as_bytes();

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResidentRepoQueryRequest {
    repository: ProjectIdentity,
    base: CommitId,
    head: CommitId,
    max_patch_bytes: usize,
}

impl ResidentRepoQueryRequest {
    /// Define one fixed exact-candidate review bundle.
    ///
    /// # Errors
    ///
    /// Returns a bounded error when the patch inclusion ceiling exceeds the v1 contract.
    pub fn new(
        repository: ProjectIdentity,
        base: CommitId,
        head: CommitId,
        max_patch_bytes: usize,
    ) -> Result<Self, ResidentRepoQueryError> {
        if max_patch_bytes > MAX_PATCH_BYTES {
            return Err(ResidentRepoQueryError::new(
                "patch_limit_invalid",
                "patch inclusion limit exceeds the repo-query/v1 ceiling",
            ));
        }
        Ok(Self {
            repository,
            base,
            head,
            max_patch_bytes,
        })
    }

    #[must_use]
    pub const fn repository(&self) -> &ProjectIdentity {
        &self.repository
    }

    #[must_use]
    pub const fn base(&self) -> &CommitId {
        &self.base
    }

    #[must_use]
    pub const fn head(&self) -> &CommitId {
        &self.head
    }

    #[must_use]
    pub const fn max_patch_bytes(&self) -> usize {
        self.max_patch_bytes
    }

    #[must_use]
    pub fn digest(&self) -> String {
        let mut bytes = Vec::new();
        append_field(&mut bytes, self.repository.as_str());
        append_field(&mut bytes, self.base.as_str());
        append_field(&mut bytes, self.head.as_str());
        append_field(&mut bytes, &self.max_patch_bytes.to_string());
        append_field(&mut bytes, &profile_generation());
        sha256_digest(&bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentRepoChangedFile {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    insertions: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    deletions: Option<u64>,
    binary: bool,
}

impl ResidentRepoChangedFile {
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    #[must_use]
    pub const fn insertions(&self) -> Option<u64> {
        self.insertions
    }

    #[must_use]
    pub const fn deletions(&self) -> Option<u64> {
        self.deletions
    }

    #[must_use]
    pub const fn binary(&self) -> bool {
        self.binary
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentRepoDiffSummary {
    files_changed: u32,
    text_files: u32,
    binary_files: u32,
    insertions: u64,
    deletions: u64,
}

impl ResidentRepoDiffSummary {
    #[must_use]
    pub const fn files_changed(&self) -> u32 {
        self.files_changed
    }

    #[must_use]
    pub const fn insertions(&self) -> u64 {
        self.insertions
    }

    #[must_use]
    pub const fn deletions(&self) -> u64 {
        self.deletions
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentRepoPatchEvidence {
    bytes: u32,
    sha256: String,
    included: bool,
    omitted_bytes: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
}

impl ResidentRepoPatchEvidence {
    #[must_use]
    pub const fn included(&self) -> bool {
        self.included
    }

    #[must_use]
    pub const fn bytes(&self) -> u32 {
        self.bytes
    }

    #[must_use]
    pub fn digest(&self) -> &str {
        &self.sha256
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentRepoQueryMetrics {
    git_processes: u16,
    git_stdout_bytes: u64,
    git_wall_microseconds: u64,
    complete_wall_microseconds: u64,
}

impl ResidentRepoQueryMetrics {
    #[must_use]
    pub const fn git_processes(&self) -> u16 {
        self.git_processes
    }

    #[must_use]
    pub const fn git_stdout_bytes(&self) -> u64 {
        self.git_stdout_bytes
    }

    #[must_use]
    pub const fn complete_wall_microseconds(&self) -> u64 {
        self.complete_wall_microseconds
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentRepoQueryReport {
    document_type: &'static str,
    schema_version: u8,
    profile_id: &'static str,
    profile_generation: String,
    authority: &'static str,
    request_digest: String,
    repository: ProjectIdentity,
    requested_base: CommitId,
    head: CommitId,
    head_tree: GitTreeId,
    merge_base: CommitId,
    base_is_ancestor: bool,
    commits_since_merge_base: u32,
    changed_files: Vec<ResidentRepoChangedFile>,
    diff_summary: ResidentRepoDiffSummary,
    patch: ResidentRepoPatchEvidence,
    metrics: ResidentRepoQueryMetrics,
}

impl ResidentRepoQueryReport {
    #[must_use]
    pub const fn repository(&self) -> &ProjectIdentity {
        &self.repository
    }

    #[must_use]
    pub const fn head(&self) -> &CommitId {
        &self.head
    }

    #[must_use]
    pub const fn head_tree(&self) -> &GitTreeId {
        &self.head_tree
    }

    #[must_use]
    pub const fn merge_base(&self) -> &CommitId {
        &self.merge_base
    }

    #[must_use]
    pub const fn base_is_ancestor(&self) -> bool {
        self.base_is_ancestor
    }

    #[must_use]
    pub fn changed_files(&self) -> &[ResidentRepoChangedFile] {
        &self.changed_files
    }

    #[must_use]
    pub const fn diff_summary(&self) -> &ResidentRepoDiffSummary {
        &self.diff_summary
    }

    #[must_use]
    pub const fn patch(&self) -> &ResidentRepoPatchEvidence {
        &self.patch
    }

    #[must_use]
    pub const fn metrics(&self) -> &ResidentRepoQueryMetrics {
        &self.metrics
    }

    #[must_use]
    pub fn render_human(&self) -> String {
        let mut output = format!(
            concat!(
                "repo query: profile={} authority={} project={}\n",
                "source: base={} head={} tree={} merge_base={} ancestor={}\n",
                "diff: files={} insertions={} deletions={} patch_bytes={} patch_included={}\n",
                "execution: git_processes={} git_stdout_bytes={} wall_us={}\n"
            ),
            self.profile_id,
            self.authority,
            self.repository.as_str(),
            self.requested_base.as_str(),
            self.head.as_str(),
            self.head_tree.as_str(),
            self.merge_base.as_str(),
            self.base_is_ancestor,
            self.diff_summary.files_changed,
            self.diff_summary.insertions,
            self.diff_summary.deletions,
            self.patch.bytes,
            self.patch.included,
            self.metrics.git_processes,
            self.metrics.git_stdout_bytes,
            self.metrics.complete_wall_microseconds,
        );
        for file in &self.changed_files {
            output.push_str(&format!("changed: {}\n", file.path));
        }
        if let Some(patch) = &self.patch.text {
            output.push_str("patch:\n");
            output.push_str(patch);
            if !patch.ends_with('\n') {
                output.push('\n');
            }
        }
        output
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResidentRepoQueryError {
    pub code: &'static str,
    pub problem: &'static str,
}

impl ResidentRepoQueryError {
    const fn new(code: &'static str, problem: &'static str) -> Self {
        Self { code, problem }
    }
}

impl fmt::Display for ResidentRepoQueryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.problem)
    }
}

impl std::error::Error for ResidentRepoQueryError {}

#[derive(Clone, PartialEq, Eq)]
struct CheckoutIdentity {
    device: u64,
    inode: u64,
    owner: u32,
}

impl CheckoutIdentity {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            owner: metadata.uid(),
        }
    }

    fn matches(&self, metadata: &std::fs::Metadata) -> bool {
        metadata.is_dir()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
            && metadata.uid() == self.owner
    }
}

impl fmt::Debug for CheckoutIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<private-checkout-identity>")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResidentRepoQueryObserver {
    git_program: PathBuf,
}

impl ResidentRepoQueryObserver {
    /// Create the fixed Git process boundary from one reviewed absolute program.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for a relative, root, or non-normalized program path.
    pub fn new(git_program: impl Into<PathBuf>) -> Result<Self, ResidentRepoQueryError> {
        let git_program = git_program.into();
        if !valid_absolute_path(&git_program) {
            return Err(unsafe_input());
        }
        Ok(Self { git_program })
    }

    /// Execute the fixed read-only bundle against exact objects already present locally.
    ///
    /// # Errors
    ///
    /// Returns a bounded path-private error for unsafe inputs, unavailable or malformed Git
    /// evidence, identity drift, missing objects, repository mismatch, or exceeded limits.
    pub fn observe(
        &self,
        checkout: &Path,
        request: &ResidentRepoQueryRequest,
        executor: &impl TimedCommandExecutor,
    ) -> Result<ResidentRepoQueryReport, ResidentRepoQueryError> {
        let complete_started = Instant::now();
        let (checkout, checkout_identity) = validate_checkout(checkout)?;
        let mut git = GitRunner::new(&self.git_program, &checkout, executor)?;

        let top_level = git.success(&["rev-parse", "--show-toplevel"])?;
        if single_line(&top_level.stdout)? != checkout.to_str().ok_or_else(unsafe_input)? {
            return Err(repository_mismatch());
        }

        let git_directory = git.success(&["rev-parse", "--absolute-git-dir"])?;
        let git_directory = validate_private_git_directory(single_line(&git_directory.stdout)?)?;
        let git_directory_metadata =
            std::fs::metadata(&git_directory).map_err(|_| source_changed())?;
        let git_directory_identity = CheckoutIdentity::from_metadata(&git_directory_metadata);

        let origin = git.success(&["config", "--no-includes", "--get", "remote.origin.url"])?;
        let origin_value = single_line(&origin.stdout)?.to_owned();
        let origin =
            GitHubProjectSource::parse(&origin_value).map_err(|_| repository_mismatch())?;
        if origin.project() != request.repository() {
            return Err(repository_mismatch());
        }

        require_commit_object(&mut git, request.base())?;
        require_commit_object(&mut git, request.head())?;

        let head_tree = git.success(&[
            "rev-parse",
            "--verify",
            &format!("{}^{{tree}}", request.head().as_str()),
        ])?;
        let head_tree =
            GitTreeId::parse(single_line(&head_tree.stdout)?).map_err(|_| invalid_output())?;

        let merge_base = git.success(&[
            "merge-base",
            request.base().as_str(),
            request.head().as_str(),
        ])?;
        let merge_base =
            CommitId::parse(single_line(&merge_base.stdout)?).map_err(|_| invalid_output())?;

        let ancestry = git.run(&[
            "merge-base",
            "--is-ancestor",
            request.base().as_str(),
            request.head().as_str(),
        ])?;
        if !ancestry.stdout.is_empty()
            || !ancestry.stderr.is_empty()
            || !matches!(ancestry.status, Some(0 | 1))
        {
            return Err(invalid_output());
        }
        let base_is_ancestor = ancestry.status == Some(0);

        let commit_count = git.success(&[
            "rev-list",
            "--count",
            &format!("{}..{}", merge_base.as_str(), request.head().as_str()),
        ])?;
        let commits_since_merge_base = single_line(&commit_count.stdout)?
            .parse::<u32>()
            .map_err(|_| invalid_output())?;

        let numstat = git.success(&[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--numstat",
            "-z",
            merge_base.as_str(),
            request.head().as_str(),
        ])?;
        let (changed_files, diff_summary) = parse_numstat(&numstat.stdout)?;

        let patch = git.success(&[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--no-color",
            "--unified=3",
            merge_base.as_str(),
            request.head().as_str(),
        ])?;
        let patch = build_patch_evidence(&patch.stdout, request.max_patch_bytes())?;

        let final_metadata = std::fs::metadata(&checkout).map_err(|_| source_changed())?;
        let final_git_directory_metadata =
            std::fs::metadata(&git_directory).map_err(|_| source_changed())?;
        let final_origin =
            git.success(&["config", "--no-includes", "--get", "remote.origin.url"])?;
        if !checkout_identity.matches(&final_metadata)
            || !git_directory_identity.matches(&final_git_directory_metadata)
            || single_line(&final_origin.stdout)? != origin_value
        {
            return Err(source_changed());
        }

        let complete_wall_microseconds = bounded_micros(complete_started.elapsed());
        Ok(ResidentRepoQueryReport {
            document_type: "glaeda-resident-repo-query",
            schema_version: RESIDENT_REPO_QUERY_SCHEMA_VERSION,
            profile_id: RESIDENT_REPO_QUERY_PROFILE_ID,
            profile_generation: profile_generation(),
            authority: "observation_only",
            request_digest: request.digest(),
            repository: request.repository.clone(),
            requested_base: request.base.clone(),
            head: request.head.clone(),
            head_tree,
            merge_base,
            base_is_ancestor,
            commits_since_merge_base,
            changed_files,
            diff_summary,
            patch,
            metrics: ResidentRepoQueryMetrics {
                git_processes: git.processes,
                git_stdout_bytes: git.stdout_bytes,
                git_wall_microseconds: git.wall_microseconds,
                complete_wall_microseconds,
            },
        })
    }
}

struct GitRunner<'a, Executor> {
    git_program: &'a Path,
    checkout: &'a Path,
    executor: &'a Executor,
    processes: u16,
    stdout_bytes: u64,
    wall_microseconds: u64,
}

impl<'a, Executor: TimedCommandExecutor> GitRunner<'a, Executor> {
    fn new(
        git_program: &'a Path,
        checkout: &'a Path,
        executor: &'a Executor,
    ) -> Result<Self, ResidentRepoQueryError> {
        if checkout.to_str().is_none() {
            return Err(unsafe_input());
        }
        Ok(Self {
            git_program,
            checkout,
            executor,
            processes: 0,
            stdout_bytes: 0,
            wall_microseconds: 0,
        })
    }

    fn success(&mut self, arguments: &[&str]) -> Result<ExecutionRecord, ResidentRepoQueryError> {
        let record = self.run(arguments)?;
        if !record.success || record.status != Some(0) || !record.stderr.is_empty() {
            return Err(query_unavailable());
        }
        Ok(record)
    }

    fn run(&mut self, arguments: &[&str]) -> Result<ExecutionRecord, ResidentRepoQueryError> {
        let checkout = self.checkout.to_str().ok_or_else(unsafe_input)?;
        let mut spec = CommandSpec::new(self.git_program)
            .argument("--no-optional-locks")
            .argument("-c")
            .argument("credential.helper=")
            .argument("-c")
            .argument("core.fsmonitor=false")
            .argument("-c")
            .argument("core.hooksPath=/dev/null")
            .argument("-c")
            .argument("diff.external=")
            .argument("-c")
            .argument("core.pager=cat")
            .argument("-c")
            .argument("color.ui=false")
            .argument("-c")
            .argument("submodule.recurse=false")
            .argument("-C")
            .argument(checkout)
            .environment("GIT_CONFIG_NOSYSTEM", "1")
            .environment("GIT_CONFIG_GLOBAL", "/dev/null")
            .environment("GIT_ATTR_NOSYSTEM", "1")
            .environment("GIT_NO_REPLACE_OBJECTS", "1")
            .environment("GIT_NO_LAZY_FETCH", "1")
            .environment("GIT_TERMINAL_PROMPT", "0")
            .environment("GIT_ASKPASS", "/bin/false")
            .environment("GIT_ALLOW_PROTOCOL", "")
            .environment("GIT_PROTOCOL_FROM_USER", "0")
            .environment("GIT_PAGER", "cat")
            .environment("GIT_EXTERNAL_DIFF", "")
            .environment("LC_ALL", "C")
            .environment("LANG", "C");
        for argument in arguments {
            spec = spec.argument(*argument);
        }
        let expected_argv = spec.displayed_argv();
        let expected_environment_keys = spec.environment.keys().cloned().collect::<Vec<_>>();
        let started = Instant::now();
        let record = self
            .executor
            .execute_with_timeout(&spec, REPO_QUERY_COMMAND_TIMEOUT)
            .map_err(|_| query_unavailable())?;
        self.processes = self.processes.checked_add(1).ok_or_else(limit_exceeded)?;
        self.stdout_bytes = self
            .stdout_bytes
            .checked_add(record.stdout.len() as u64)
            .ok_or_else(limit_exceeded)?;
        self.wall_microseconds = self
            .wall_microseconds
            .saturating_add(bounded_micros(started.elapsed()));
        if record.argv != expected_argv
            || record.environment_keys != expected_environment_keys
            || record.stdout.len() > MAX_GIT_OUTPUT_BYTES
            || record.stderr.len() > MAX_GIT_OUTPUT_BYTES
            || record.stdout.contains('\u{fffd}')
            || record.stderr.contains('\u{fffd}')
        {
            return Err(invalid_output());
        }
        Ok(record)
    }
}

fn require_commit_object(
    git: &mut GitRunner<'_, impl TimedCommandExecutor>,
    object: &CommitId,
) -> Result<(), ResidentRepoQueryError> {
    let record = git.success(&["cat-file", "-t", object.as_str()])?;
    if single_line(&record.stdout)? != "commit" {
        return Err(object_unavailable());
    }
    Ok(())
}

fn validate_checkout(
    checkout: &Path,
) -> Result<(PathBuf, CheckoutIdentity), ResidentRepoQueryError> {
    if !valid_absolute_path(checkout) || checkout.to_str().is_none() {
        return Err(unsafe_input());
    }
    let link_metadata = std::fs::symlink_metadata(checkout).map_err(|_| unsafe_input())?;
    if link_metadata.file_type().is_symlink() || !link_metadata.is_dir() {
        return Err(unsafe_input());
    }
    let canonical = std::fs::canonicalize(checkout).map_err(|_| unsafe_input())?;
    if canonical.as_path() != checkout {
        return Err(unsafe_input());
    }
    let metadata = std::fs::metadata(&canonical).map_err(|_| unsafe_input())?;
    let identity = CheckoutIdentity::from_metadata(&metadata);
    Ok((canonical, identity))
}

fn validate_private_git_directory(value: &str) -> Result<PathBuf, ResidentRepoQueryError> {
    let path = PathBuf::from(value);
    if !valid_absolute_path(&path) {
        return Err(invalid_output());
    }
    let canonical = std::fs::canonicalize(&path).map_err(|_| invalid_output())?;
    if canonical != path {
        return Err(invalid_output());
    }
    let metadata = std::fs::symlink_metadata(&canonical).map_err(|_| invalid_output())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(invalid_output());
    }
    Ok(canonical)
}

fn valid_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && path != Path::new("/")
        && path.to_str().is_some()
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

fn single_line(value: &str) -> Result<&str, ResidentRepoQueryError> {
    let line = value.strip_suffix('\n').unwrap_or(value);
    if line.is_empty() || line.contains(['\n', '\r', '\0']) {
        return Err(invalid_output());
    }
    Ok(line)
}

fn parse_numstat(
    value: &str,
) -> Result<(Vec<ResidentRepoChangedFile>, ResidentRepoDiffSummary), ResidentRepoQueryError> {
    if !value.is_empty() && !value.ends_with('\0') {
        return Err(invalid_output());
    }
    let mut files = Vec::new();
    let mut text_files = 0_u32;
    let mut binary_files = 0_u32;
    let mut insertions = 0_u64;
    let mut deletions = 0_u64;
    for entry in value.split_terminator('\0') {
        if files.len() >= MAX_CHANGED_FILES {
            return Err(limit_exceeded());
        }
        let mut fields = entry.splitn(3, '\t');
        let additions = fields.next().ok_or_else(invalid_output)?;
        let removals = fields.next().ok_or_else(invalid_output)?;
        let path = fields.next().ok_or_else(invalid_output)?;
        if !valid_repo_output_path(path) {
            return Err(invalid_output());
        }
        let binary = additions == "-" && removals == "-";
        let (file_insertions, file_deletions) = if binary {
            binary_files = binary_files.checked_add(1).ok_or_else(limit_exceeded)?;
            (None, None)
        } else {
            let file_insertions = additions.parse::<u64>().map_err(|_| invalid_output())?;
            let file_deletions = removals.parse::<u64>().map_err(|_| invalid_output())?;
            insertions = insertions
                .checked_add(file_insertions)
                .ok_or_else(limit_exceeded)?;
            deletions = deletions
                .checked_add(file_deletions)
                .ok_or_else(limit_exceeded)?;
            text_files = text_files.checked_add(1).ok_or_else(limit_exceeded)?;
            (Some(file_insertions), Some(file_deletions))
        };
        files.push(ResidentRepoChangedFile {
            path: path.to_owned(),
            insertions: file_insertions,
            deletions: file_deletions,
            binary,
        });
    }
    let files_changed = u32::try_from(files.len()).map_err(|_| limit_exceeded())?;
    Ok((
        files,
        ResidentRepoDiffSummary {
            files_changed,
            text_files,
            binary_files,
            insertions,
            deletions,
        },
    ))
}

fn valid_repo_output_path(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('/')
        && !value.contains('\r')
        && Path::new(value)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn build_patch_evidence(
    patch: &str,
    max_patch_bytes: usize,
) -> Result<ResidentRepoPatchEvidence, ResidentRepoQueryError> {
    let bytes = u32::try_from(patch.len()).map_err(|_| limit_exceeded())?;
    let included = patch.len() <= max_patch_bytes;
    Ok(ResidentRepoPatchEvidence {
        bytes,
        sha256: sha256_digest(patch.as_bytes()),
        included,
        omitted_bytes: if included { 0 } else { bytes },
        text: included.then(|| patch.to_owned()),
    })
}

fn profile_generation() -> String {
    sha256_digest(PROFILE_CONTRACT)
}

fn sha256_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(71);
    encoded.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn append_field(bytes: &mut Vec<u8>, value: &str) {
    bytes.extend_from_slice(&(value.len() as u64).to_be_bytes());
    bytes.extend_from_slice(value.as_bytes());
}

fn bounded_micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

const fn unsafe_input() -> ResidentRepoQueryError {
    ResidentRepoQueryError::new(
        "unsafe_input",
        "repo query input is not one normalized exact local coordinate",
    )
}

const fn repository_mismatch() -> ResidentRepoQueryError {
    ResidentRepoQueryError::new(
        "repository_mismatch",
        "resident checkout does not match the requested canonical project",
    )
}

const fn object_unavailable() -> ResidentRepoQueryError {
    ResidentRepoQueryError::new(
        "object_unavailable",
        "an exact requested Git commit object is unavailable locally",
    )
}

const fn query_unavailable() -> ResidentRepoQueryError {
    ResidentRepoQueryError::new(
        "query_unavailable",
        "the bounded local Git query did not complete successfully",
    )
}

const fn invalid_output() -> ResidentRepoQueryError {
    ResidentRepoQueryError::new(
        "invalid_output",
        "the bounded local Git query returned malformed evidence",
    )
}

const fn source_changed() -> ResidentRepoQueryError {
    ResidentRepoQueryError::new(
        "source_changed",
        "resident repository identity changed during observation",
    )
}

const fn limit_exceeded() -> ResidentRepoQueryError {
    ResidentRepoQueryError::new(
        "limit_exceeded",
        "repo query evidence exceeds the fixed v1 bounds",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(limit: usize) -> ResidentRepoQueryRequest {
        ResidentRepoQueryRequest::new(
            ProjectIdentity::parse("github.com/teamleaderleo/glaeda").expect("project"),
            CommitId::parse("1111111111111111111111111111111111111111").expect("base"),
            CommitId::parse("2222222222222222222222222222222222222222").expect("head"),
            limit,
        )
        .expect("request")
    }

    #[test]
    fn request_digest_binds_patch_limit_and_profile_generation() {
        assert_ne!(request(0).digest(), request(1).digest());
        assert!(request(0).digest().starts_with("sha256:"));
        assert_eq!(request(0).digest().len(), 71);
    }

    #[test]
    fn patch_is_all_or_nothing_and_always_digested() {
        let included = build_patch_evidence("abc", 3).expect("included patch");
        assert!(included.included());
        assert_eq!(included.bytes(), 3);
        assert_eq!(included.text.as_deref(), Some("abc"));

        let omitted = build_patch_evidence("abcd", 3).expect("omitted patch");
        assert!(!omitted.included());
        assert_eq!(omitted.omitted_bytes, 4);
        assert!(omitted.text.is_none());
        assert_ne!(included.digest(), omitted.digest());
    }

    #[test]
    fn numstat_is_exact_and_rejects_partial_records() {
        let (files, summary) =
            parse_numstat("3\t1\tsrc/lib.rs\0-\t-\tassets/image.bin\0").expect("numstat");
        assert_eq!(files.len(), 2);
        assert_eq!(summary.files_changed(), 2);
        assert_eq!(summary.insertions(), 3);
        assert_eq!(summary.deletions(), 1);
        assert!(files[1].binary());
        assert_eq!(
            parse_numstat("1\t0\tsrc/lib.rs")
                .expect_err("partial record")
                .code,
            "invalid_output"
        );
    }

    #[test]
    fn unsafe_paths_and_oversized_limits_are_refused_without_echo() {
        assert!(!valid_absolute_path(Path::new("relative/private")));
        assert!(!valid_repo_output_path("../private"));
        let error = ResidentRepoQueryRequest::new(
            ProjectIdentity::parse("github.com/teamleaderleo/glaeda").expect("project"),
            CommitId::parse("1111111111111111111111111111111111111111").expect("base"),
            CommitId::parse("2222222222222222222222222222222222222222").expect("head"),
            MAX_PATCH_BYTES + 1,
        )
        .expect_err("oversized limit");
        assert_eq!(error.code, "patch_limit_invalid");
    }
}
