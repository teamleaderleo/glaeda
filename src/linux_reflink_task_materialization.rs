//! Research-only same-HEAD task materialization on Linux reflink filesystems.
//!
//! This adapter mutates only one caller-selected absent target worktree. It deliberately carries
//! no project lease, source authority, task adoption, cache, execution, cleanup, or product-default
//! authority. Ordinary Git materialization remains the complete fallback.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};
use std::time::Instant;

use rustix::fs::ioctl_ficlone;
use serde::Serialize;

use crate::git_index_stat_patch::{GitIndexStat, GitIndexStatUpdate, patch_git_index_v2_stats};

pub const REFLINK_TASK_MATERIALIZATION_SCHEMA_VERSION: u8 = 1;
const MAX_GIT_OUTPUT_BYTES: usize = 32 * 1024 * 1024;
const MAX_TRACKED_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_TRACKED_FILES: usize = 200_000;
const MAX_PATH_BYTES: usize = 4_096;
const MAX_GITDIR_FILE_BYTES: u64 = 8_192;
const GIT_OID_HEX_BYTES: usize = 40;
const MAX_FANOUT_TASKS: usize = 32;
const REFLINK_FILE_WORKERS: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReflinkTaskMaterializationMode {
    Ordinary,
    ReflinkWithFallback,
}

impl ReflinkTaskMaterializationMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Ordinary => "ordinary",
            Self::ReflinkWithFallback => "reflink_with_fallback",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReflinkTaskMaterializationDisposition {
    Ordinary,
    Reflinked,
    OrdinaryFallback,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflinkTaskMaterializationRequest {
    git: PathBuf,
    source: PathBuf,
    target: PathBuf,
    commit: String,
    mode: ReflinkTaskMaterializationMode,
}

impl ReflinkTaskMaterializationRequest {
    /// Construct one bounded research request.
    ///
    /// # Errors
    ///
    /// Returns a path-free error when the executable, paths, or SHA-1 commit identity are outside
    /// the reviewed research boundary.
    pub fn new(
        git: impl Into<PathBuf>,
        source: impl Into<PathBuf>,
        target: impl Into<PathBuf>,
        commit: impl Into<String>,
        mode: ReflinkTaskMaterializationMode,
    ) -> Result<Self, ReflinkTaskMaterializationError> {
        let request = Self {
            git: git.into(),
            source: source.into(),
            target: target.into(),
            commit: commit.into(),
            mode,
        };
        validate_request(&request)?;
        Ok(request)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflinkTaskFanoutRequest {
    tasks: Vec<ReflinkTaskMaterializationRequest>,
}

impl ReflinkTaskFanoutRequest {
    /// Construct one bounded shared-source fan-out request.
    ///
    /// # Errors
    ///
    /// Returns a path-free error unless 1-32 distinct targets share one exact Git executable,
    /// source, commit, and materialization mode.
    pub fn new(
        git: impl Into<PathBuf>,
        source: impl Into<PathBuf>,
        targets: impl IntoIterator<Item = PathBuf>,
        commit: impl Into<String>,
        mode: ReflinkTaskMaterializationMode,
    ) -> Result<Self, ReflinkTaskMaterializationError> {
        let git = git.into();
        let source = source.into();
        let commit = commit.into();
        let targets = targets.into_iter().collect::<Vec<_>>();
        if targets.is_empty() || targets.len() > MAX_FANOUT_TASKS {
            return Err(request_invalid());
        }
        let mut unique = BTreeSet::new();
        let mut tasks = Vec::with_capacity(targets.len());
        for target in targets {
            if !unique.insert(target.clone()) {
                return Err(request_invalid());
            }
            tasks.push(ReflinkTaskMaterializationRequest::new(
                git.clone(),
                source.clone(),
                target,
                commit.clone(),
                mode,
            )?);
        }
        Ok(Self { tasks })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReflinkTaskMaterializationTimings {
    total_microseconds: u64,
    worktree_microseconds: u64,
    file_materialization_microseconds: u64,
    index_patch_microseconds: u64,
    fallback_microseconds: u64,
    proof_microseconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReflinkTaskMaterializationReport {
    schema_version: u8,
    document_type: &'static str,
    authority: &'static str,
    requested_mode: &'static str,
    disposition: ReflinkTaskMaterializationDisposition,
    fallback_reason: Option<&'static str>,
    commit: String,
    tree: String,
    tracked_regular_files: u32,
    logical_bytes: u64,
    timings: ReflinkTaskMaterializationTimings,
    final_git_proof: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReflinkTaskFanoutTimings {
    total_microseconds: u64,
    source_proof_microseconds: u64,
    parallel_preparation_microseconds: u64,
    parallel_finalization_microseconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReflinkTaskFanoutReport {
    schema_version: u8,
    document_type: &'static str,
    authority: &'static str,
    requested_mode: &'static str,
    task_count: u32,
    ordinary_tasks: u32,
    reflinked_tasks: u32,
    ordinary_fallback_tasks: u32,
    fallback_reasons: std::collections::BTreeMap<&'static str, u32>,
    commit: String,
    tree: String,
    tracked_regular_files_per_task: u32,
    logical_bytes_per_task: u64,
    timings: ReflinkTaskFanoutTimings,
    final_git_proof: &'static str,
}

impl ReflinkTaskFanoutReport {
    #[must_use]
    pub const fn task_count(&self) -> u32 {
        self.task_count
    }

    #[must_use]
    pub const fn reflinked_tasks(&self) -> u32 {
        self.reflinked_tasks
    }

    #[must_use]
    pub const fn ordinary_fallback_tasks(&self) -> u32 {
        self.ordinary_fallback_tasks
    }
}

impl ReflinkTaskMaterializationReport {
    #[must_use]
    pub const fn disposition(&self) -> ReflinkTaskMaterializationDisposition {
        self.disposition
    }

    #[must_use]
    pub const fn fallback_reason(&self) -> Option<&'static str> {
        self.fallback_reason
    }

    #[must_use]
    pub const fn tracked_regular_files(&self) -> u32 {
        self.tracked_regular_files
    }

    #[must_use]
    pub const fn logical_bytes(&self) -> u64 {
        self.logical_bytes
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflinkTaskMaterializationError {
    code: &'static str,
    message: &'static str,
}

impl ReflinkTaskMaterializationError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for ReflinkTaskMaterializationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ReflinkTaskMaterializationError {}

#[derive(Debug, Clone)]
struct TreeEntry {
    path: Vec<u8>,
    mode: u32,
}

#[derive(Debug, Clone)]
struct TreeInventory {
    regular_entries: Vec<TreeEntry>,
    candidate_supported: bool,
}

#[derive(Debug, Clone, Copy)]
struct CandidateFailure {
    code: &'static str,
}

#[derive(Debug, Default)]
struct PhaseTimings {
    worktree_microseconds: u64,
    file_materialization_microseconds: u64,
    index_patch_microseconds: u64,
    fallback_microseconds: u64,
    proof_microseconds: u64,
}

/// Materialize one exact detached task worktree.
///
/// Reflink mode brackets a held-stable exact clean same-HEAD source with Git observations, checks
/// every open source file remains stable across its clone window, patches only target index
/// stat-cache words through the existing bounded index-v2 patcher, and ends in non-mutating
/// `diff-files` plus `diff-index` proofs. Candidate evidence or capability failure resets the owned
/// target through ordinary Git and reports a fixed fallback reason.
///
/// # Errors
///
/// Returns a path-free error when request validation, ordinary Git fallback, final Git proof, or
/// bounded report construction fails. A returned error may leave the exact requested target as
/// discardable recovery state; directory presence alone never grants task readiness.
pub fn materialize_reflink_task(
    request: &ReflinkTaskMaterializationRequest,
) -> Result<ReflinkTaskMaterializationReport, ReflinkTaskMaterializationError> {
    validate_request(request)?;
    let started = Instant::now();
    let mut timings = PhaseTimings::default();
    let mut worktree_created = false;
    let mut fallback_reason = None;

    let inventory = list_tree(request)?;
    let expected_tree = commit_tree(request)?;

    let mut disposition = match request.mode {
        ReflinkTaskMaterializationMode::Ordinary => {
            let worktree_started = Instant::now();
            add_worktree(request, false).map_err(|_| ordinary_materialization_failed())?;
            timings.worktree_microseconds = elapsed_microseconds(worktree_started);
            worktree_created = true;
            ReflinkTaskMaterializationDisposition::Ordinary
        }
        ReflinkTaskMaterializationMode::ReflinkWithFallback => {
            match materialize_candidate(request, &inventory, &mut timings, &mut worktree_created) {
                Ok(()) => ReflinkTaskMaterializationDisposition::Reflinked,
                Err(failure) => {
                    fallback_reason = Some(failure.code);
                    let fallback_started = Instant::now();
                    ordinary_fallback(request, worktree_created)
                        .map_err(|_| ordinary_fallback_failed())?;
                    timings.fallback_microseconds = elapsed_microseconds(fallback_started);
                    worktree_created = true;
                    ReflinkTaskMaterializationDisposition::OrdinaryFallback
                }
            }
        }
    };

    debug_assert!(worktree_created);
    let proof_started = Instant::now();
    let tree = match final_git_proof(request, &expected_tree) {
        Ok(tree) => tree,
        Err(_) if disposition == ReflinkTaskMaterializationDisposition::Reflinked => {
            fallback_reason = Some("candidate_final_git_proof_failed");
            let fallback_started = Instant::now();
            ordinary_fallback(request, true).map_err(|_| ordinary_fallback_failed())?;
            timings.fallback_microseconds = elapsed_microseconds(fallback_started);
            disposition = ReflinkTaskMaterializationDisposition::OrdinaryFallback;
            final_git_proof(request, &expected_tree)?
        }
        Err(error) => return Err(error),
    };
    timings.proof_microseconds = elapsed_microseconds(proof_started);
    let logical_bytes = logical_bytes(&request.target, &inventory.regular_entries)
        .map_err(|_| final_git_proof_failed())?;

    Ok(ReflinkTaskMaterializationReport {
        schema_version: REFLINK_TASK_MATERIALIZATION_SCHEMA_VERSION,
        document_type: "glaeda-reflink-task-materialization",
        authority: "research_materialization_only",
        requested_mode: request.mode.as_str(),
        disposition,
        fallback_reason,
        commit: request.commit.clone(),
        tree,
        tracked_regular_files: u32::try_from(inventory.regular_entries.len())
            .map_err(|_| tree_inventory_invalid())?,
        logical_bytes,
        timings: ReflinkTaskMaterializationTimings {
            total_microseconds: elapsed_microseconds(started),
            worktree_microseconds: timings.worktree_microseconds,
            file_materialization_microseconds: timings.file_materialization_microseconds,
            index_patch_microseconds: timings.index_patch_microseconds,
            fallback_microseconds: timings.fallback_microseconds,
            proof_microseconds: timings.proof_microseconds,
        },
        final_git_proof: "head_tree_diff_files_diff_index",
    })
}

#[must_use]
pub fn render_reflink_task_materialization_human(
    report: &ReflinkTaskMaterializationReport,
) -> String {
    format!(
        "Glaeda reflink task materialization\nauthority: {}\nrequested mode: {}\ndisposition: {:?}\nfallback reason: {}\ncommit: {}\ntree: {}\ntracked regular files: {}\nlogical bytes: {}\ntotal: {} us\nworktree: {} us\nfile materialization: {} us\nindex patch: {} us\nfallback: {} us\nproof: {} us\nfinal Git proof: {}\n",
        report.authority,
        report.requested_mode,
        report.disposition,
        report.fallback_reason.unwrap_or("none"),
        report.commit,
        report.tree,
        report.tracked_regular_files,
        report.logical_bytes,
        report.timings.total_microseconds,
        report.timings.worktree_microseconds,
        report.timings.file_materialization_microseconds,
        report.timings.index_patch_microseconds,
        report.timings.fallback_microseconds,
        report.timings.proof_microseconds,
        report.final_git_proof,
    )
}

/// Materialize 1-32 exact task worktrees while sharing one source inventory and clean-source
/// observation window across the complete fan-out.
///
/// # Errors
///
/// Returns a path-free error if any ordinary fallback or final Git proof fails. A returned error
/// may leave only the exact requested targets as discardable recovery state.
pub fn materialize_reflink_task_fanout(
    request: &ReflinkTaskFanoutRequest,
) -> Result<ReflinkTaskFanoutReport, ReflinkTaskMaterializationError> {
    let started = Instant::now();
    let first = request.tasks.first().ok_or_else(request_invalid)?;
    let inventory = list_tree(first)?;
    let expected_tree = commit_tree(first)?;
    let mut source_proof_microseconds = 0_u64;

    let preparation_started = Instant::now();
    let mut preparations = match first.mode {
        ReflinkTaskMaterializationMode::Ordinary => parallel_ordinary_add(&request.tasks)?,
        ReflinkTaskMaterializationMode::ReflinkWithFallback => {
            let source_proof_started = Instant::now();
            let initial_source = observe_exact_clean_source(first);
            source_proof_microseconds = source_proof_microseconds
                .saturating_add(elapsed_microseconds(source_proof_started));
            if let Err(failure) = initial_source {
                request
                    .tasks
                    .iter()
                    .map(|_| PreparedTask::failed(false, failure))
                    .collect()
            } else if !inventory.candidate_supported {
                request
                    .tasks
                    .iter()
                    .map(|_| PreparedTask::failed(false, candidate("tree_inventory_unsupported")))
                    .collect()
            } else {
                parallel_reflink_prepare(&request.tasks, &inventory.regular_entries)?
            }
        }
    };
    let parallel_preparation_microseconds = elapsed_microseconds(preparation_started);

    if first.mode == ReflinkTaskMaterializationMode::ReflinkWithFallback
        && preparations.iter().any(PreparedTask::is_candidate)
    {
        let source_proof_started = Instant::now();
        let final_source = observe_exact_clean_source(first);
        source_proof_microseconds =
            source_proof_microseconds.saturating_add(elapsed_microseconds(source_proof_started));
        if final_source.is_err() {
            for preparation in &mut preparations {
                if preparation.is_candidate() {
                    preparation.result = Err(candidate("source_changed_during_fanout"));
                }
            }
        }
    }

    let finalization_started = Instant::now();
    let finalized = parallel_finalize(&request.tasks, preparations, &expected_tree)?;
    let parallel_finalization_microseconds = elapsed_microseconds(finalization_started);
    let tree = common_final_tree(&finalized)?;
    let logical_bytes = logical_bytes(&first.target, &inventory.regular_entries)
        .map_err(|_| final_git_proof_failed())?;

    let mut ordinary_tasks = 0_u32;
    let mut reflinked_tasks = 0_u32;
    let mut ordinary_fallback_tasks = 0_u32;
    let mut fallback_reasons = std::collections::BTreeMap::new();
    for task in &finalized {
        match task.disposition {
            ReflinkTaskMaterializationDisposition::Ordinary => {
                ordinary_tasks = ordinary_tasks.saturating_add(1);
            }
            ReflinkTaskMaterializationDisposition::Reflinked => {
                reflinked_tasks = reflinked_tasks.saturating_add(1);
            }
            ReflinkTaskMaterializationDisposition::OrdinaryFallback => {
                ordinary_fallback_tasks = ordinary_fallback_tasks.saturating_add(1);
                let reason = task.fallback_reason.ok_or_else(fanout_failed)?;
                let count = fallback_reasons.entry(reason).or_insert(0_u32);
                *count = count.saturating_add(1);
            }
        }
    }

    Ok(ReflinkTaskFanoutReport {
        schema_version: REFLINK_TASK_MATERIALIZATION_SCHEMA_VERSION,
        document_type: "glaeda-reflink-task-fanout",
        authority: "research_materialization_only",
        requested_mode: first.mode.as_str(),
        task_count: u32::try_from(request.tasks.len()).map_err(|_| request_invalid())?,
        ordinary_tasks,
        reflinked_tasks,
        ordinary_fallback_tasks,
        fallback_reasons,
        commit: first.commit.clone(),
        tree,
        tracked_regular_files_per_task: u32::try_from(inventory.regular_entries.len())
            .map_err(|_| tree_inventory_invalid())?,
        logical_bytes_per_task: logical_bytes,
        timings: ReflinkTaskFanoutTimings {
            total_microseconds: elapsed_microseconds(started),
            source_proof_microseconds,
            parallel_preparation_microseconds,
            parallel_finalization_microseconds,
        },
        final_git_proof: "head_tree_diff_files_diff_index_per_task",
    })
}

#[must_use]
pub fn render_reflink_task_fanout_human(report: &ReflinkTaskFanoutReport) -> String {
    format!(
        "Glaeda reflink task fan-out\nauthority: {}\nrequested mode: {}\ntasks: {}\nordinary: {}\nreflinked: {}\nordinary fallback: {}\ncommit: {}\ntree: {}\ntracked regular files per task: {}\nlogical bytes per task: {}\ntotal: {} us\nsource proof: {} us\nparallel preparation: {} us\nparallel finalization: {} us\nfinal Git proof: {}\n",
        report.authority,
        report.requested_mode,
        report.task_count,
        report.ordinary_tasks,
        report.reflinked_tasks,
        report.ordinary_fallback_tasks,
        report.commit,
        report.tree,
        report.tracked_regular_files_per_task,
        report.logical_bytes_per_task,
        report.timings.total_microseconds,
        report.timings.source_proof_microseconds,
        report.timings.parallel_preparation_microseconds,
        report.timings.parallel_finalization_microseconds,
        report.final_git_proof,
    )
}

struct PreparedTask {
    worktree_created: bool,
    result: Result<Option<Vec<GitIndexStatUpdate>>, CandidateFailure>,
}

impl PreparedTask {
    const fn failed(worktree_created: bool, failure: CandidateFailure) -> Self {
        Self {
            worktree_created,
            result: Err(failure),
        }
    }

    fn is_candidate(&self) -> bool {
        matches!(self.result, Ok(Some(_)))
    }
}

struct FinalizedTask {
    disposition: ReflinkTaskMaterializationDisposition,
    fallback_reason: Option<&'static str>,
    tree: String,
}

fn parallel_ordinary_add(
    tasks: &[ReflinkTaskMaterializationRequest],
) -> Result<Vec<PreparedTask>, ReflinkTaskMaterializationError> {
    std::thread::scope(|scope| {
        let handles = tasks
            .iter()
            .map(|task| {
                scope.spawn(move || {
                    add_worktree(task, false)?;
                    Ok(PreparedTask {
                        worktree_created: true,
                        result: Ok(None),
                    })
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| handle.join().map_err(|_| fanout_failed())?)
            .collect()
    })
}

fn parallel_reflink_prepare(
    tasks: &[ReflinkTaskMaterializationRequest],
    entries: &[TreeEntry],
) -> Result<Vec<PreparedTask>, ReflinkTaskMaterializationError> {
    let mut preparations = std::thread::scope(|scope| {
        let handles = tasks
            .iter()
            .map(|task| scope.spawn(move || prepare_reflink_worktree(task, entries.len())))
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| handle.join().map_err(|_| fanout_failed()))
            .collect::<Result<Vec<_>, _>>()
    })?;
    reflink_entries_across_tasks(tasks, entries, &mut preparations);
    Ok(preparations)
}

fn prepare_reflink_worktree(
    request: &ReflinkTaskMaterializationRequest,
    entry_count: usize,
) -> PreparedTask {
    let mut worktree_created = false;
    let result = (|| {
        same_filesystem(&request.source, &request.target)?;
        add_worktree(request, true).map_err(|_| candidate("candidate_worktree_failed"))?;
        worktree_created = true;
        run_git(
            request,
            &request.target,
            &[OsStr::new("read-tree"), OsStr::new(&request.commit)],
        )
        .map_err(|_| candidate("candidate_read_tree_failed"))?;
        Ok(Some(Vec::with_capacity(entry_count)))
    })();
    PreparedTask {
        worktree_created,
        result,
    }
}

fn reflink_entries_across_tasks(
    tasks: &[ReflinkTaskMaterializationRequest],
    entries: &[TreeEntry],
    preparations: &mut [PreparedTask],
) {
    let Some(first) = tasks.first() else {
        fail_open_candidates(preparations, candidate("candidate_fanout_mismatch"));
        return;
    };
    if tasks.len() != preparations.len() {
        fail_open_candidates(preparations, candidate("candidate_fanout_mismatch"));
        return;
    }

    let parents = entries
        .iter()
        .filter_map(|entry| {
            PathBuf::from(OsString::from_vec(entry.path.clone()))
                .parent()
                .map(Path::to_owned)
        })
        .collect::<BTreeSet<_>>();
    for (task, preparation) in tasks.iter().zip(preparations.iter_mut()) {
        if preparation.is_candidate()
            && parents
                .iter()
                .try_for_each(|parent| fs::create_dir_all(task.target.join(parent)))
                .is_err()
        {
            preparation.result = Err(candidate("candidate_target_create_failed"));
        }
    }
    if !preparations.iter().any(PreparedTask::is_candidate) {
        return;
    }

    let candidate_indices = preparations
        .iter()
        .enumerate()
        .filter_map(|(index, preparation)| preparation.is_candidate().then_some(index))
        .collect::<Vec<_>>();
    let candidate_tasks = candidate_indices
        .iter()
        .map(|index| &tasks[*index])
        .collect::<Vec<_>>();
    let worker_count = REFLINK_FILE_WORKERS.min(entries.len()).max(1);
    let chunk_size = entries.len().div_ceil(worker_count).max(1);
    let chunks = std::thread::scope(|scope| {
        let handles = entries
            .chunks(chunk_size)
            .map(|chunk| scope.spawn(|| reflink_entry_chunk(first, &candidate_tasks, chunk)))
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .map_err(|_| candidate("candidate_worker_failed"))?
            })
            .collect::<Result<Vec<_>, _>>()
    });
    let chunks = match chunks {
        Ok(chunks) => chunks,
        Err(failure) => {
            fail_open_candidates(preparations, failure);
            return;
        }
    };
    for chunk in chunks {
        if chunk.len() != candidate_indices.len() {
            fail_open_candidates(preparations, candidate("candidate_fanout_mismatch"));
            return;
        }
        for (index, updates) in candidate_indices.iter().zip(chunk) {
            let Ok(Some(combined)) = &mut preparations[*index].result else {
                continue;
            };
            match updates {
                Ok(mut updates) => combined.append(&mut updates),
                Err(failure) => preparations[*index].result = Err(failure),
            }
        }
    }
}

fn reflink_entry_chunk(
    first: &ReflinkTaskMaterializationRequest,
    tasks: &[&ReflinkTaskMaterializationRequest],
    entries: &[TreeEntry],
) -> Result<Vec<Result<Vec<GitIndexStatUpdate>, CandidateFailure>>, CandidateFailure> {
    let mut results = tasks
        .iter()
        .map(|_| Ok(Vec::with_capacity(entries.len())))
        .collect::<Vec<_>>();
    for entry in entries {
        let relative = PathBuf::from(OsString::from_vec(entry.path.clone()));
        let source = OpenOptions::new()
            .read(true)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(first.source.join(&relative))
            .map_err(|_| candidate("candidate_source_open_failed"))?;
        let source_metadata = source
            .metadata()
            .map_err(|_| candidate("candidate_source_stat_failed"))?;
        if !source_metadata.is_file()
            || source_metadata.len() > MAX_TRACKED_FILE_BYTES
            || source_metadata.mode() & 0o170777 != entry.mode
        {
            return Err(candidate("candidate_source_mismatch"));
        }

        for (task, result) in tasks.iter().zip(results.iter_mut()) {
            let Ok(updates) = result else {
                continue;
            };
            match reflink_one_target(&source, task.target.join(&relative), entry) {
                Ok(update) => updates.push(update),
                Err(failure) => *result = Err(failure),
            }
        }

        let source_after = source
            .metadata()
            .map_err(|_| candidate("candidate_source_stat_failed"))?;
        if !same_file_snapshot(&source_metadata, &source_after) {
            return Err(candidate("candidate_source_moved"));
        }
    }
    Ok(results)
}

fn reflink_one_target(
    source: &fs::File,
    target_path: PathBuf,
    entry: &TreeEntry,
) -> Result<GitIndexStatUpdate, CandidateFailure> {
    let target = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(entry.mode & 0o777)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(target_path)
        .map_err(|_| candidate("candidate_target_create_failed"))?;
    ioctl_ficlone(&target, source).map_err(|_| candidate("reflink_unavailable"))?;
    target
        .set_permissions(fs::Permissions::from_mode(entry.mode & 0o777))
        .map_err(|_| candidate("candidate_target_mode_failed"))?;
    let metadata = target
        .metadata()
        .map_err(|_| candidate("candidate_target_stat_failed"))?;
    let stat = git_index_stat(&metadata)?;
    GitIndexStatUpdate::new(entry.path.clone(), stat)
        .map_err(|_| candidate("candidate_target_stat_failed"))
}

fn fail_open_candidates(preparations: &mut [PreparedTask], failure: CandidateFailure) {
    for preparation in preparations {
        if preparation.is_candidate() {
            preparation.result = Err(failure);
        }
    }
}

fn parallel_finalize(
    tasks: &[ReflinkTaskMaterializationRequest],
    preparations: Vec<PreparedTask>,
    expected_tree: &str,
) -> Result<Vec<FinalizedTask>, ReflinkTaskMaterializationError> {
    if tasks.len() != preparations.len() {
        return Err(fanout_failed());
    }
    std::thread::scope(|scope| {
        let handles = tasks
            .iter()
            .zip(preparations)
            .map(|(task, preparation)| {
                scope.spawn(move || finalize_task(task, preparation, expected_tree))
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| handle.join().map_err(|_| fanout_failed())?)
            .collect()
    })
}

fn finalize_task(
    request: &ReflinkTaskMaterializationRequest,
    preparation: PreparedTask,
    expected_tree: &str,
) -> Result<FinalizedTask, ReflinkTaskMaterializationError> {
    match preparation.result {
        Ok(None) => Ok(FinalizedTask {
            disposition: ReflinkTaskMaterializationDisposition::Ordinary,
            fallback_reason: None,
            tree: final_git_proof(request, expected_tree)?,
        }),
        Ok(Some(updates)) => {
            if patch_target_index(request, &updates).is_ok()
                && let Ok(tree) = final_git_proof(request, expected_tree)
            {
                return Ok(FinalizedTask {
                    disposition: ReflinkTaskMaterializationDisposition::Reflinked,
                    fallback_reason: None,
                    tree,
                });
            }
            ordinary_fallback(request, true).map_err(|_| ordinary_fallback_failed())?;
            Ok(FinalizedTask {
                disposition: ReflinkTaskMaterializationDisposition::OrdinaryFallback,
                fallback_reason: Some("candidate_finalization_failed"),
                tree: final_git_proof(request, expected_tree)?,
            })
        }
        Err(failure) => {
            ordinary_fallback(request, preparation.worktree_created)
                .map_err(|_| ordinary_fallback_failed())?;
            Ok(FinalizedTask {
                disposition: ReflinkTaskMaterializationDisposition::OrdinaryFallback,
                fallback_reason: Some(failure.code),
                tree: final_git_proof(request, expected_tree)?,
            })
        }
    }
}

fn common_final_tree(tasks: &[FinalizedTask]) -> Result<String, ReflinkTaskMaterializationError> {
    let first = tasks.first().ok_or_else(fanout_failed)?.tree.clone();
    if tasks.iter().all(|task| task.tree == first) {
        Ok(first)
    } else {
        Err(fanout_failed())
    }
}

fn materialize_candidate(
    request: &ReflinkTaskMaterializationRequest,
    inventory: &TreeInventory,
    timings: &mut PhaseTimings,
    worktree_created: &mut bool,
) -> Result<(), CandidateFailure> {
    if !inventory.candidate_supported {
        return Err(candidate("tree_inventory_unsupported"));
    }
    observe_exact_clean_source(request)?;
    same_filesystem(&request.source, &request.target)?;

    let worktree_started = Instant::now();
    add_worktree(request, true).map_err(|_| candidate("candidate_worktree_failed"))?;
    *worktree_created = true;
    run_git(
        request,
        &request.target,
        &[OsStr::new("read-tree"), OsStr::new(&request.commit)],
    )
    .map_err(|_| candidate("candidate_read_tree_failed"))?;
    timings.worktree_microseconds = elapsed_microseconds(worktree_started);

    let files_started = Instant::now();
    let updates = reflink_entries(request, &inventory.regular_entries)?;
    timings.file_materialization_microseconds = elapsed_microseconds(files_started);

    observe_exact_clean_source(request)?;

    let index_started = Instant::now();
    patch_target_index(request, &updates)?;
    timings.index_patch_microseconds = elapsed_microseconds(index_started);
    Ok(())
}

fn ordinary_fallback(
    request: &ReflinkTaskMaterializationRequest,
    worktree_created: bool,
) -> Result<(), ReflinkTaskMaterializationError> {
    if worktree_created {
        remove_index_lock(request);
        run_git(
            request,
            &request.target,
            &[
                OsStr::new("reset"),
                OsStr::new("--hard"),
                OsStr::new("--quiet"),
                OsStr::new(&request.commit),
            ],
        )?;
    } else {
        add_worktree(request, false)?;
    }
    Ok(())
}

fn add_worktree(
    request: &ReflinkTaskMaterializationRequest,
    no_checkout: bool,
) -> Result<(), ReflinkTaskMaterializationError> {
    let mut arguments = vec![
        OsString::from("worktree"),
        OsString::from("add"),
        OsString::from("--detach"),
    ];
    if no_checkout {
        arguments.push(OsString::from("--no-checkout"));
    }
    arguments.push(OsString::from("--"));
    arguments.push(request.target.as_os_str().to_owned());
    arguments.push(OsString::from(&request.commit));
    let borrowed = arguments
        .iter()
        .map(OsString::as_os_str)
        .collect::<Vec<_>>();
    run_git(request, &request.source, &borrowed).map(|_| ())
}

fn observe_exact_clean_source(
    request: &ReflinkTaskMaterializationRequest,
) -> Result<(), CandidateFailure> {
    let root = git_stdout(
        request,
        &request.source,
        &[OsStr::new("rev-parse"), OsStr::new("--show-toplevel")],
    )
    .map_err(|_| candidate("source_git_observation_failed"))?;
    let observed_root = PathBuf::from(OsString::from_vec(trim_one_newline(root)));
    let canonical_root =
        fs::canonicalize(observed_root).map_err(|_| candidate("source_git_observation_failed"))?;
    if canonical_root != request.source {
        return Err(candidate("source_root_mismatch"));
    }
    let head = git_text_line(
        request,
        &request.source,
        &[OsStr::new("rev-parse"), OsStr::new("HEAD")],
    )
    .map_err(|_| candidate("source_git_observation_failed"))?;
    if head != request.commit {
        return Err(candidate("source_head_mismatch"));
    }
    let diff = run_git_status(
        request,
        &request.source,
        &[
            OsStr::new("diff"),
            OsStr::new("--no-ext-diff"),
            OsStr::new("--quiet"),
            OsStr::new("--ignore-submodules=none"),
            OsStr::new("HEAD"),
            OsStr::new("--"),
        ],
    )
    .map_err(|_| candidate("source_git_observation_failed"))?;
    if diff != 0 {
        return Err(candidate("source_not_clean"));
    }
    let untracked = git_stdout(
        request,
        &request.source,
        &[
            OsStr::new("ls-files"),
            OsStr::new("--others"),
            OsStr::new("--exclude-standard"),
            OsStr::new("-z"),
        ],
    )
    .map_err(|_| candidate("source_git_observation_failed"))?;
    if !untracked.is_empty() {
        return Err(candidate("source_not_clean"));
    }
    Ok(())
}

fn list_tree(
    request: &ReflinkTaskMaterializationRequest,
) -> Result<TreeInventory, ReflinkTaskMaterializationError> {
    let output = git_stdout(
        request,
        &request.source,
        &[
            OsStr::new("ls-tree"),
            OsStr::new("-r"),
            OsStr::new("-z"),
            OsStr::new("--full-tree"),
            OsStr::new(&request.commit),
        ],
    )?;
    parse_tree_entries(&output)
}

fn parse_tree_entries(output: &[u8]) -> Result<TreeInventory, ReflinkTaskMaterializationError> {
    if output.len() > MAX_GIT_OUTPUT_BYTES {
        return Err(git_output_invalid());
    }
    let mut entries = Vec::new();
    let mut paths = BTreeSet::new();
    let mut candidate_supported = true;
    for record in output.split(|byte| *byte == 0) {
        if record.is_empty() {
            continue;
        }
        if entries.len() >= MAX_TRACKED_FILES {
            return Err(tree_inventory_invalid());
        }
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(tree_inventory_invalid)?;
        let metadata = std::str::from_utf8(&record[..tab]).map_err(|_| tree_inventory_invalid())?;
        let mut fields = metadata.split(' ');
        let mode = fields.next().ok_or_else(tree_inventory_invalid)?;
        let kind = fields.next().ok_or_else(tree_inventory_invalid)?;
        let oid = fields.next().ok_or_else(tree_inventory_invalid)?;
        if fields.next().is_some() {
            return Err(tree_inventory_invalid());
        }
        let path = record[tab + 1..].to_vec();
        validate_relative_path(&path)?;
        if !paths.insert(path.clone()) {
            return Err(tree_inventory_invalid());
        }
        let parsed_mode = u32::from_str_radix(mode, 8).map_err(|_| tree_inventory_invalid())?;
        parse_sha1(oid.as_bytes())?;
        if matches!(mode, "100644" | "100755") && kind == "blob" {
            entries.push(TreeEntry {
                path,
                mode: parsed_mode,
            });
        } else if matches!((mode, kind), ("120000", "blob") | ("160000", "commit")) {
            candidate_supported = false;
        } else {
            return Err(tree_inventory_invalid());
        }
    }
    Ok(TreeInventory {
        regular_entries: entries,
        candidate_supported,
    })
}

fn reflink_entries(
    request: &ReflinkTaskMaterializationRequest,
    entries: &[TreeEntry],
) -> Result<Vec<GitIndexStatUpdate>, CandidateFailure> {
    let mut updates = Vec::with_capacity(entries.len());
    let mut created_parents = BTreeSet::new();
    for entry in entries {
        let relative = PathBuf::from(OsString::from_vec(entry.path.clone()));
        let source_path = request.source.join(&relative);
        let target_path = request.target.join(&relative);
        let parent = target_path
            .parent()
            .ok_or_else(|| candidate("candidate_path_invalid"))?;
        if created_parents.insert(parent.to_owned()) {
            fs::create_dir_all(parent).map_err(|_| candidate("candidate_target_create_failed"))?;
        }

        let source = OpenOptions::new()
            .read(true)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(&source_path)
            .map_err(|_| candidate("candidate_source_open_failed"))?;
        let source_metadata = source
            .metadata()
            .map_err(|_| candidate("candidate_source_stat_failed"))?;
        if !source_metadata.is_file()
            || source_metadata.len() > MAX_TRACKED_FILE_BYTES
            || source_metadata.mode() & 0o170777 != entry.mode
        {
            return Err(candidate("candidate_source_mismatch"));
        }

        let target = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(entry.mode & 0o777)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(&target_path)
            .map_err(|_| candidate("candidate_target_create_failed"))?;
        ioctl_ficlone(&target, &source).map_err(|_| candidate("reflink_unavailable"))?;
        target
            .set_permissions(fs::Permissions::from_mode(entry.mode & 0o777))
            .map_err(|_| candidate("candidate_target_mode_failed"))?;
        let source_after = source
            .metadata()
            .map_err(|_| candidate("candidate_source_stat_failed"))?;
        if !same_file_snapshot(&source_metadata, &source_after) {
            return Err(candidate("candidate_source_moved"));
        }
        let metadata = target
            .metadata()
            .map_err(|_| candidate("candidate_target_stat_failed"))?;
        let stat = git_index_stat(&metadata)?;
        updates.push(
            GitIndexStatUpdate::new(entry.path.clone(), stat)
                .map_err(|_| candidate("candidate_target_stat_failed"))?,
        );
    }
    Ok(updates)
}

fn git_index_stat(metadata: &fs::Metadata) -> Result<GitIndexStat, CandidateFailure> {
    Ok(GitIndexStat::new(
        u32::try_from(metadata.ctime()).map_err(|_| candidate("candidate_stat_out_of_range"))?,
        u32::try_from(metadata.ctime_nsec())
            .map_err(|_| candidate("candidate_stat_out_of_range"))?,
        u32::try_from(metadata.mtime()).map_err(|_| candidate("candidate_stat_out_of_range"))?,
        u32::try_from(metadata.mtime_nsec())
            .map_err(|_| candidate("candidate_stat_out_of_range"))?,
        metadata.dev() as u32,
        metadata.ino() as u32,
        metadata.mode(),
        metadata.uid(),
        metadata.gid(),
        u32::try_from(metadata.size()).map_err(|_| candidate("candidate_stat_out_of_range"))?,
    ))
}

fn same_file_snapshot(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.ctime() == right.ctime()
        && left.ctime_nsec() == right.ctime_nsec()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
        && left.mode() == right.mode()
        && left.uid() == right.uid()
        && left.gid() == right.gid()
        && left.size() == right.size()
}

fn patch_target_index(
    request: &ReflinkTaskMaterializationRequest,
    updates: &[GitIndexStatUpdate],
) -> Result<(), CandidateFailure> {
    let index = target_index_path(request)?;
    let bytes = fs::read(&index).map_err(|_| candidate("candidate_index_read_failed"))?;
    let patch = patch_git_index_v2_stats(&bytes, updates)
        .map_err(|_| candidate("candidate_index_patch_refused"))?;
    let lock = index.with_extension("lock");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&lock)
        .map_err(|_| candidate("candidate_index_lock_failed"))?;
    let result = file
        .write_all(patch.bytes())
        .and_then(|()| file.flush())
        .and_then(|()| fs::rename(&lock, &index));
    if result.is_err() {
        let _ = fs::remove_file(&lock);
        return Err(candidate("candidate_index_publish_failed"));
    }
    Ok(())
}

fn target_index_path(
    request: &ReflinkTaskMaterializationRequest,
) -> Result<PathBuf, CandidateFailure> {
    Ok(target_git_directory(request)?.join("index"))
}

fn target_git_directory(
    request: &ReflinkTaskMaterializationRequest,
) -> Result<PathBuf, CandidateFailure> {
    let dot_git = request.target.join(".git");
    let document = read_bounded_regular_nofollow(&dot_git, MAX_GITDIR_FILE_BYTES)
        .map_err(|_| candidate("candidate_index_path_failed"))?;
    let line = trim_one_newline(document);
    let git_directory = line
        .strip_prefix(b"gitdir: ")
        .ok_or_else(|| candidate("candidate_index_path_failed"))?;
    if git_directory.is_empty()
        || git_directory.contains(&0)
        || git_directory.contains(&b'\n')
        || git_directory.contains(&b'\r')
    {
        return Err(candidate("candidate_index_path_failed"));
    }
    let git_directory = PathBuf::from(OsString::from_vec(git_directory.to_vec()));
    if !is_normalized_absolute(&git_directory)
        || fs::canonicalize(&git_directory).map_err(|_| candidate("candidate_index_path_failed"))?
            != git_directory
        || !fs::metadata(&git_directory)
            .map_err(|_| candidate("candidate_index_path_failed"))?
            .is_dir()
    {
        return Err(candidate("candidate_index_path_failed"));
    }
    let backlink =
        read_bounded_regular_nofollow(&git_directory.join("gitdir"), MAX_GITDIR_FILE_BYTES)
            .map_err(|_| candidate("candidate_index_path_failed"))?;
    if trim_one_newline(backlink) != dot_git.as_os_str().as_bytes() {
        return Err(candidate("candidate_index_path_failed"));
    }
    Ok(git_directory)
}

fn read_bounded_regular_nofollow(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(std::io::Error::other("bounded regular file required"));
    }
    let mut document = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or_default());
    std::io::Read::by_ref(&mut file)
        .take(limit.saturating_add(1))
        .read_to_end(&mut document)?;
    if u64::try_from(document.len()).unwrap_or(u64::MAX) > limit {
        return Err(std::io::Error::other("bounded regular file required"));
    }
    Ok(document)
}

fn remove_index_lock(request: &ReflinkTaskMaterializationRequest) {
    if let Ok(index) = target_index_path(request) {
        let _ = fs::remove_file(index.with_extension("lock"));
    }
}

fn final_git_proof(
    request: &ReflinkTaskMaterializationRequest,
    expected_tree: &str,
) -> Result<String, ReflinkTaskMaterializationError> {
    let git_directory = target_git_directory(request).map_err(|_| final_git_proof_failed())?;
    let head = trim_one_newline(
        read_bounded_regular_nofollow(&git_directory.join("HEAD"), 64)
            .map_err(|_| final_git_proof_failed())?,
    );
    if head != request.commit.as_bytes() {
        return Err(final_git_proof_failed());
    }
    for arguments in [
        vec![
            OsStr::new("diff-files"),
            OsStr::new("--quiet"),
            OsStr::new("--"),
        ],
        vec![
            OsStr::new("diff-index"),
            OsStr::new("--cached"),
            OsStr::new("--quiet"),
            OsStr::new(&request.commit),
            OsStr::new("--"),
        ],
    ] {
        if run_git_status(request, &request.target, &arguments)? != 0 {
            return Err(final_git_proof_failed());
        }
    }
    Ok(expected_tree.to_owned())
}

fn commit_tree(
    request: &ReflinkTaskMaterializationRequest,
) -> Result<String, ReflinkTaskMaterializationError> {
    git_text_line(
        request,
        &request.source,
        &[
            OsStr::new("rev-parse"),
            OsStr::new(&format!("{}^{{tree}}", request.commit)),
        ],
    )
}

fn logical_bytes(source: &Path, entries: &[TreeEntry]) -> Result<u64, CandidateFailure> {
    let mut total = 0_u64;
    for entry in entries {
        let relative = PathBuf::from(OsString::from_vec(entry.path.clone()));
        let metadata = fs::metadata(source.join(relative))
            .map_err(|_| candidate("source_inventory_unavailable"))?;
        total = total
            .checked_add(metadata.len())
            .ok_or_else(|| candidate("source_inventory_unavailable"))?;
    }
    Ok(total)
}

fn same_filesystem(source: &Path, target: &Path) -> Result<(), CandidateFailure> {
    let source_device = fs::metadata(source)
        .map_err(|_| candidate("source_filesystem_unavailable"))?
        .dev();
    let parent = target
        .parent()
        .ok_or_else(|| candidate("target_parent_unavailable"))?;
    let target_device = fs::metadata(parent)
        .map_err(|_| candidate("target_parent_unavailable"))?
        .dev();
    if source_device != target_device {
        return Err(candidate("reflink_filesystem_mismatch"));
    }
    Ok(())
}

fn validate_request(
    request: &ReflinkTaskMaterializationRequest,
) -> Result<(), ReflinkTaskMaterializationError> {
    if !is_normalized_absolute(&request.git)
        || !is_normalized_absolute(&request.source)
        || !is_normalized_absolute(&request.target)
        || request.target.starts_with(&request.source)
        || request.source == request.target
    {
        return Err(request_invalid());
    }
    let git = fs::metadata(&request.git).map_err(|_| request_invalid())?;
    if !git.is_file() || git.mode() & 0o111 == 0 {
        return Err(request_invalid());
    }
    let source = fs::canonicalize(&request.source).map_err(|_| request_invalid())?;
    if source != request.source {
        return Err(request_invalid());
    }
    let parent = request.target.parent().ok_or_else(request_invalid)?;
    if fs::canonicalize(parent).map_err(|_| request_invalid())? != parent
        || request.target.exists()
        || request.commit.len() != GIT_OID_HEX_BYTES
        || !request
            .commit
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(request_invalid());
    }
    Ok(())
}

fn is_normalized_absolute(path: &Path) -> bool {
    let bytes = path.as_os_str().as_bytes();
    path.is_absolute()
        && bytes.len() <= MAX_PATH_BYTES
        && !bytes.contains(&b'\n')
        && !bytes.contains(&b'\r')
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

fn validate_relative_path(path: &[u8]) -> Result<(), ReflinkTaskMaterializationError> {
    if path.is_empty() || path.len() > MAX_PATH_BYTES || path.contains(&0) {
        return Err(tree_inventory_invalid());
    }
    let parsed = PathBuf::from(OsString::from_vec(path.to_vec()));
    if parsed.is_absolute()
        || parsed
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(tree_inventory_invalid());
    }
    Ok(())
}

fn parse_sha1(value: &[u8]) -> Result<[u8; 20], ReflinkTaskMaterializationError> {
    if value.len() != GIT_OID_HEX_BYTES {
        return Err(tree_inventory_invalid());
    }
    let mut result = [0_u8; 20];
    for (slot, pair) in result.iter_mut().zip(value.chunks_exact(2)) {
        *slot = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
    }
    Ok(result)
}

fn hex_nibble(value: u8) -> Result<u8, ReflinkTaskMaterializationError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(tree_inventory_invalid()),
    }
}

fn git_text_line(
    request: &ReflinkTaskMaterializationRequest,
    cwd: &Path,
    arguments: &[&OsStr],
) -> Result<String, ReflinkTaskMaterializationError> {
    let output = git_stdout(request, cwd, arguments)?;
    let line = std::str::from_utf8(&trim_one_newline(output))
        .map_err(|_| git_output_invalid())?
        .to_owned();
    if line.is_empty() || line.contains('\n') || line.contains('\r') {
        return Err(git_output_invalid());
    }
    Ok(line)
}

fn git_stdout(
    request: &ReflinkTaskMaterializationRequest,
    cwd: &Path,
    arguments: &[&OsStr],
) -> Result<Vec<u8>, ReflinkTaskMaterializationError> {
    let output = run_git(request, cwd, arguments)?;
    if output.stdout.len() > MAX_GIT_OUTPUT_BYTES || !output.stderr.is_empty() {
        return Err(git_output_invalid());
    }
    Ok(output.stdout)
}

fn run_git(
    request: &ReflinkTaskMaterializationRequest,
    cwd: &Path,
    arguments: &[&OsStr],
) -> Result<Output, ReflinkTaskMaterializationError> {
    let output = git_command(request, cwd, arguments)
        .output()
        .map_err(|_| git_execution_failed())?;
    if !output.status.success() {
        return Err(git_execution_failed());
    }
    Ok(output)
}

fn run_git_status(
    request: &ReflinkTaskMaterializationRequest,
    cwd: &Path,
    arguments: &[&OsStr],
) -> Result<i32, ReflinkTaskMaterializationError> {
    let output = git_command(request, cwd, arguments)
        .output()
        .map_err(|_| git_execution_failed())?;
    if output.stdout.len() > MAX_GIT_OUTPUT_BYTES || output.stderr.len() > MAX_GIT_OUTPUT_BYTES {
        return Err(git_output_invalid());
    }
    output.status.code().ok_or_else(git_execution_failed)
}

fn git_command(
    request: &ReflinkTaskMaterializationRequest,
    cwd: &Path,
    arguments: &[&OsStr],
) -> Command {
    let mut command = Command::new(&request.git);
    command
        .env_clear()
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .current_dir(cwd)
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(cwd)
        .args(arguments);
    command
}

fn trim_one_newline(mut value: Vec<u8>) -> Vec<u8> {
    if value.ends_with(b"\n") {
        value.pop();
    }
    value
}

fn elapsed_microseconds(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

const fn candidate(code: &'static str) -> CandidateFailure {
    CandidateFailure { code }
}

const fn error(code: &'static str, message: &'static str) -> ReflinkTaskMaterializationError {
    ReflinkTaskMaterializationError { code, message }
}

const fn request_invalid() -> ReflinkTaskMaterializationError {
    error(
        "reflink_task_request_invalid",
        "reflink task request is outside the reviewed research boundary",
    )
}

const fn tree_inventory_invalid() -> ReflinkTaskMaterializationError {
    error(
        "reflink_task_tree_inventory_invalid",
        "Git tree inventory is malformed or outside the reviewed bound",
    )
}

const fn git_execution_failed() -> ReflinkTaskMaterializationError {
    error(
        "reflink_task_git_execution_failed",
        "reviewed Git command did not complete successfully",
    )
}

const fn git_output_invalid() -> ReflinkTaskMaterializationError {
    error(
        "reflink_task_git_output_invalid",
        "reviewed Git output is malformed, noisy, or outside the bounded contract",
    )
}

const fn ordinary_materialization_failed() -> ReflinkTaskMaterializationError {
    error(
        "reflink_task_ordinary_materialization_failed",
        "ordinary Git task materialization failed",
    )
}

const fn ordinary_fallback_failed() -> ReflinkTaskMaterializationError {
    error(
        "reflink_task_ordinary_fallback_failed",
        "ordinary Git fallback failed after the reflink candidate was refused",
    )
}

const fn final_git_proof_failed() -> ReflinkTaskMaterializationError {
    error(
        "reflink_task_final_git_proof_failed",
        "final non-mutating Git proof refused the materialized task",
    )
}

const fn fanout_failed() -> ReflinkTaskMaterializationError {
    error(
        "reflink_task_fanout_failed",
        "bounded task fan-out did not complete every requested target",
    )
}

#[cfg(test)]
mod tests {
    use super::{
        ReflinkTaskMaterializationMode, ReflinkTaskMaterializationRequest, parse_sha1,
        parse_tree_entries, render_reflink_task_materialization_human,
    };
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::Path;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fixture() -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "glaeda-reflink-task-test-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("fixture root");
        root
    }

    fn git(root: &Path, arguments: &[&str]) -> String {
        let output = Command::new("/usr/bin/git")
            .env_clear()
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("LC_ALL", "C")
            .current_dir(root)
            .args(arguments)
            .output()
            .expect("Git executes");
        assert!(output.status.success(), "Git failed");
        String::from_utf8(output.stdout)
            .expect("utf8")
            .trim()
            .to_owned()
    }

    #[test]
    fn parses_only_exact_regular_tree_entries() {
        let output = b"100644 blob 1111111111111111111111111111111111111111\ta.txt\0\
100755 blob 2222222222222222222222222222222222222222\tdir/run\0";
        let inventory = parse_tree_entries(output).expect("valid inventory");
        assert!(inventory.candidate_supported);
        assert_eq!(inventory.regular_entries.len(), 2);
        assert_eq!(inventory.regular_entries[0].path, b"a.txt");
        assert_eq!(inventory.regular_entries[1].mode, 0o100755);

        let unsupported = b"120000 blob 1111111111111111111111111111111111111111\tlink\0";
        let unsupported = parse_tree_entries(unsupported).expect("valid fallback inventory");
        assert!(!unsupported.candidate_supported);
        assert!(unsupported.regular_entries.is_empty());
        assert!(parse_sha1(b"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").is_err());
    }

    #[test]
    fn ordinary_mode_materializes_and_proves_one_exact_detached_worktree() {
        let root = fixture();
        let source = root.join("source");
        let target_parent = root.join("tasks");
        let target = target_parent.join("task");
        fs::create_dir(&source).expect("source");
        fs::create_dir(&target_parent).expect("tasks");
        git(&source, &["init", "--quiet"]);
        fs::write(source.join("file.txt"), "payload\n").expect("source file");
        git(&source, &["add", "file.txt"]);
        git(
            &source,
            &[
                "-c",
                "user.name=Glaeda Test",
                "-c",
                "user.email=glaeda@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "fixture",
            ],
        );
        let commit = git(&source, &["rev-parse", "HEAD"]);
        let request = ReflinkTaskMaterializationRequest::new(
            "/usr/bin/git",
            source.clone(),
            target.clone(),
            commit,
            ReflinkTaskMaterializationMode::Ordinary,
        )
        .expect("request");
        let report = super::materialize_reflink_task(&request).expect("materialize");
        assert_eq!(report.tracked_regular_files(), 1);
        assert_eq!(report.logical_bytes(), 8);
        assert!(render_reflink_task_materialization_human(&report).contains("Ordinary"));
        assert_eq!(
            fs::read(target.join("file.txt")).expect("target"),
            b"payload\n"
        );
        let index = super::target_index_path(&request).expect("linked index");
        assert!(index.is_file());
        let git_directory = index.parent().expect("Git directory").to_owned();
        let dot_git = target.join(".git");
        let dot_git_document = fs::read(&dot_git).expect("dot Git document");
        fs::remove_file(&dot_git).expect("remove dot Git document");
        symlink("/dev/null", &dot_git).expect("replace dot Git with symlink");
        assert_eq!(
            super::target_index_path(&request)
                .expect_err("symlink must fail")
                .code,
            "candidate_index_path_failed"
        );
        fs::remove_file(&dot_git).expect("remove symlink");
        fs::write(&dot_git, dot_git_document).expect("restore dot Git document");

        let backlink = git_directory.join("gitdir");
        let backlink_document = fs::read(&backlink).expect("backlink");
        fs::write(&backlink, b"/not/the/requested/task/.git\n").expect("replace backlink");
        assert_eq!(
            super::target_index_path(&request)
                .expect_err("wrong backlink must fail")
                .code,
            "candidate_index_path_failed"
        );
        fs::write(backlink, backlink_document).expect("restore backlink");
        git(
            &source,
            &["worktree", "remove", "--force", target.to_str().unwrap()],
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn dirty_source_refuses_candidate_and_completes_ordinary_fallback() {
        let root = fixture();
        let source = root.join("source");
        let target_parent = root.join("tasks");
        let target = target_parent.join("task");
        fs::create_dir(&source).expect("source");
        fs::create_dir(&target_parent).expect("tasks");
        git(&source, &["init", "--quiet"]);
        fs::write(source.join("file.txt"), "committed\n").expect("source file");
        git(&source, &["add", "file.txt"]);
        git(
            &source,
            &[
                "-c",
                "user.name=Glaeda Test",
                "-c",
                "user.email=glaeda@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "fixture",
            ],
        );
        let commit = git(&source, &["rev-parse", "HEAD"]);
        fs::write(source.join("file.txt"), "dirty\n").expect("dirty source");
        let request = ReflinkTaskMaterializationRequest::new(
            "/usr/bin/git",
            source.clone(),
            target.clone(),
            commit,
            ReflinkTaskMaterializationMode::ReflinkWithFallback,
        )
        .expect("request");
        let report = super::materialize_reflink_task(&request).expect("ordinary fallback");
        assert_eq!(
            report.disposition(),
            super::ReflinkTaskMaterializationDisposition::OrdinaryFallback
        );
        assert_eq!(report.fallback_reason(), Some("source_not_clean"));
        assert_eq!(
            fs::read(target.join("file.txt")).expect("target"),
            b"committed\n"
        );
        git(
            &source,
            &["worktree", "remove", "--force", target.to_str().unwrap()],
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn fanout_shares_dirty_source_refusal_and_completes_every_fallback() {
        let root = fixture();
        let source = root.join("source");
        let target_parent = root.join("tasks");
        let targets = [target_parent.join("one"), target_parent.join("two")];
        fs::create_dir(&source).expect("source");
        fs::create_dir(&target_parent).expect("tasks");
        git(&source, &["init", "--quiet"]);
        fs::write(source.join("file.txt"), "committed\n").expect("source file");
        git(&source, &["add", "file.txt"]);
        git(
            &source,
            &[
                "-c",
                "user.name=Glaeda Test",
                "-c",
                "user.email=glaeda@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "fixture",
            ],
        );
        let commit = git(&source, &["rev-parse", "HEAD"]);
        fs::write(source.join("file.txt"), "dirty\n").expect("dirty source");
        let request = super::ReflinkTaskFanoutRequest::new(
            "/usr/bin/git",
            source.clone(),
            targets.iter().cloned(),
            commit,
            ReflinkTaskMaterializationMode::ReflinkWithFallback,
        )
        .expect("request");
        let report = super::materialize_reflink_task_fanout(&request).expect("fanout fallback");
        assert_eq!(report.task_count(), 2);
        assert_eq!(report.reflinked_tasks(), 0);
        assert_eq!(report.ordinary_fallback_tasks(), 2);
        for target in &targets {
            assert_eq!(
                fs::read(target.join("file.txt")).expect("target"),
                b"committed\n"
            );
            git(
                &source,
                &["worktree", "remove", "--force", target.to_str().unwrap()],
            );
        }
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn invalid_requests_and_private_values_have_path_free_errors() {
        let error = ReflinkTaskMaterializationRequest::new(
            "git",
            "/private/source",
            "/private/source/task",
            "not-a-commit",
            ReflinkTaskMaterializationMode::ReflinkWithFallback,
        )
        .expect_err("invalid request");
        assert_eq!(error.code(), "reflink_task_request_invalid");
        assert!(!error.to_string().contains("private"));

        let error = ReflinkTaskMaterializationRequest::new(
            "/usr/bin/git",
            "/private/source\nsecond-record",
            "/private/task",
            "1111111111111111111111111111111111111111",
            ReflinkTaskMaterializationMode::Ordinary,
        )
        .expect_err("line-delimited control paths are outside the boundary");
        assert_eq!(error.code(), "reflink_task_request_invalid");
    }
}
