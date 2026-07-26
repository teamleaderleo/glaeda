use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use crate::exact_commit_handoff::{
    CandidateAncestryEvidence, CandidateAncestryRequirement, CandidateCommitObservation,
    ExactCommitHandoffPlan, ExactCommitHandoffRequest, RepositoryPath, RunnerIdentity,
    RunnerIdentityClass, TargetRef, WorkspaceIdentity, WorktreeObservation, WorktreeState,
};
use crate::verification_profile::{
    CacheId, CacheIdentity, CacheObservation, CacheUse, CacheUseRecord, CleanupStatus,
    ConcurrencyPolicy, DirtyWorkspacePolicy, ExactBuildScope, ExactVerificationScope,
    HostResourceObservation, ImmutableRefInput, ImmutableSourceInputs,
    ImmutableVerificationIdentity, LocalCommitAuthority, LocalCommitState, MemoryPolicy, PackageId,
    PrivateVerificationEvidence, PublicationAuthority, PublicationState, RepositoryCommandContract,
    RepositoryCommandId, RepositoryCommandIdentity, RepositoryRefName, ResolvedRef,
    ResourceDefaults, RunnerInstallationId, RunnerOwnedWorkspaceIdentity, RunnerWorkspaceId,
    SourceComposition, TestedSourceIdentity, TimeoutPolicy, VerificationAuthorityPolicy,
    VerificationExecutionEvidence, VerificationPreflightObservation, VerificationProfileContract,
    VerificationProfileDefinition, VerificationProfileId, VerificationResultDisposition,
    VerificationTestOutcome, WorkspaceCleanliness, WorkspaceMutationAuthority,
    WorkspaceMutationPolicy, WorkspaceObservation, evaluate_verification_preflight,
    finalize_verification_result,
};

use super::{
    DEFAULT_MAX_GIT_OUTPUT_BYTES, DEFAULT_MAX_PACKAGE_BYTES, EXPORT_REF, RunnerExportAdapter,
    RunnerExportLimits, RunnerExportPhase, RunnerExportRefusalCode, parse_bundle_head,
    parse_single_parent, publish_package_no_replace,
};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TestRepository {
    root: PathBuf,
    repository: PathBuf,
    output: PathBuf,
    git: PathBuf,
    parent: CommitId,
    candidate: CommitId,
    tree: GitTreeId,
    changed_paths: Vec<RepositoryPath>,
}

impl TestRepository {
    fn create() -> Self {
        let git = find_git().expect("reviewed Git executable required for runner export tests");
        let counter = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "smolrunner-runner-export-{}-{counter}",
            std::process::id()
        ));
        let repository = root.join("repository");
        let output = root.join("output");
        fs::create_dir_all(&repository).expect("repository directory");
        fs::create_dir_all(&output).expect("output directory");
        run_git(&git, &repository, ["init", "--quiet"]);
        run_git(
            &git,
            &repository,
            ["config", "user.name", "SmolRunner Test"],
        );
        run_git(
            &git,
            &repository,
            ["config", "user.email", "smolrunner@example.invalid"],
        );
        fs::write(repository.join("alpha.txt"), "base\n").expect("base file");
        run_git(&git, &repository, ["add", "alpha.txt"]);
        run_git(&git, &repository, ["commit", "--quiet", "-m", "base"]);
        let parent = commit(output_line(&git, &repository, ["rev-parse", "HEAD"]));

        fs::write(repository.join("alpha.txt"), "base\ncandidate\n").expect("candidate file");
        fs::write(repository.join("beta.txt"), "candidate\n").expect("second candidate file");
        run_git(&git, &repository, ["add", "alpha.txt", "beta.txt"]);
        run_git(&git, &repository, ["commit", "--quiet", "-m", "candidate"]);
        let candidate = commit(output_line(&git, &repository, ["rev-parse", "HEAD"]));
        let tree = GitTreeId::parse(&output_line(
            &git,
            &repository,
            ["rev-parse", "HEAD^{tree}"],
        ))
        .expect("tree");
        let changed_paths = vec![path("alpha.txt"), path("beta.txt")];

        Self {
            root,
            repository,
            output,
            git,
            parent,
            candidate,
            tree,
            changed_paths,
        }
    }

    fn plan(&self) -> ExactCommitHandoffPlan {
        plan_with(
            self.parent.clone(),
            self.candidate.clone(),
            self.tree.clone(),
            self.changed_paths.clone(),
        )
    }

    fn package_path(&self, name: &str) -> PathBuf {
        self.output.join(name)
    }

    fn adapter(&self) -> RunnerExportAdapter {
        RunnerExportAdapter::new(self.git.clone(), RunnerExportLimits::default()).expect("adapter")
    }
}

impl Drop for TestRepository {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn exact_bundle_export_reobserves_and_builds_existing_receipt() {
    let repository = TestRepository::create();
    let plan = repository.plan();
    let package = repository.package_path("candidate.bundle");
    let record = repository
        .adapter()
        .execute(&plan, &repository.repository, &package)
        .expect("export");

    assert!(package.is_file());
    assert_eq!(record.exported_commit(), &repository.candidate);
    assert_eq!(record.exported_parent(), &repository.parent);
    assert_eq!(record.exported_tree(), &repository.tree);
    assert_eq!(record.changed_paths(), repository.changed_paths.as_slice());
    assert!(record.package_bytes() > 0);
    assert!(record.package_digest().as_str().starts_with("sha256:"));
    assert_eq!(record.package_path(), package);
    let receipt = record.to_handoff_receipt(&plan).expect("receipt");
    assert_eq!(receipt.package_digest(), record.package_digest());

    let heads = output_bytes(
        &repository.git,
        &repository.repository,
        [
            "bundle",
            "list-heads",
            package.to_str().expect("package path"),
        ],
    );
    assert_eq!(
        parse_bundle_head(&heads, EXPORT_REF).expect("single head"),
        repository.candidate
    );
}

#[test]
fn dirty_worktree_is_refused_before_package_creation() {
    let repository = TestRepository::create();
    fs::write(repository.repository.join("dirty.txt"), "dirty\n").expect("dirty file");
    let package = repository.package_path("dirty.bundle");
    let error = repository
        .adapter()
        .execute(&repository.plan(), &repository.repository, &package)
        .expect_err("dirty worktree");
    assert_eq!(error.code, RunnerExportRefusalCode::DirtyWorktree);
    assert!(!package.exists());
}

#[test]
fn moved_head_is_refused() {
    let repository = TestRepository::create();
    let plan = repository.plan();
    fs::write(repository.repository.join("gamma.txt"), "moved\n").expect("moved file");
    run_git(
        &repository.git,
        &repository.repository,
        ["add", "gamma.txt"],
    );
    run_git(
        &repository.git,
        &repository.repository,
        ["commit", "--quiet", "-m", "moved"],
    );
    let error = repository
        .adapter()
        .execute(
            &plan,
            &repository.repository,
            &repository.package_path("moved.bundle"),
        )
        .expect_err("moved candidate");
    assert_eq!(error.code, RunnerExportRefusalCode::MovedCandidate);
}

#[test]
fn parent_tree_and_changed_path_drift_are_typed() {
    let repository = TestRepository::create();
    let wrong_parent = commit("9".repeat(40));
    let wrong_tree = GitTreeId::parse(&"8".repeat(40)).expect("wrong tree");
    let parent_error = repository
        .adapter()
        .execute(
            &plan_with(
                wrong_parent,
                repository.candidate.clone(),
                repository.tree.clone(),
                repository.changed_paths.clone(),
            ),
            &repository.repository,
            &repository.package_path("parent.bundle"),
        )
        .expect_err("parent drift");
    assert_eq!(parent_error.code, RunnerExportRefusalCode::ParentDrift);

    let tree_error = repository
        .adapter()
        .execute(
            &plan_with(
                repository.parent.clone(),
                repository.candidate.clone(),
                wrong_tree,
                repository.changed_paths.clone(),
            ),
            &repository.repository,
            &repository.package_path("tree.bundle"),
        )
        .expect_err("tree drift");
    assert_eq!(tree_error.code, RunnerExportRefusalCode::TreeDrift);

    let paths_error = repository
        .adapter()
        .execute(
            &plan_with(
                repository.parent.clone(),
                repository.candidate.clone(),
                repository.tree.clone(),
                vec![path("alpha.txt")],
            ),
            &repository.repository,
            &repository.package_path("paths.bundle"),
        )
        .expect_err("path drift");
    assert_eq!(paths_error.code, RunnerExportRefusalCode::ChangedPathsDrift);
}

#[test]
fn package_ambiguity_and_missing_parent_evidence_are_refused() {
    let first = "1".repeat(40);
    let second = "2".repeat(40);
    let ambiguous = format!("{first} {EXPORT_REF}\n{second} refs/heads/extra\n");
    let error = parse_bundle_head(ambiguous.as_bytes(), EXPORT_REF).expect_err("ambiguous");
    assert_eq!(error.code, RunnerExportRefusalCode::PackageAmbiguous);

    let commit = commit(first);
    let missing = parse_single_parent(b"", &commit, RunnerExportPhase::SourceObservation)
        .expect_err("missing parent");
    assert_eq!(missing.code, RunnerExportRefusalCode::MissingGitEvidence);
}

#[test]
fn spawn_and_output_bounds_fail_closed() {
    let repository = TestRepository::create();
    let missing_git = RunnerExportAdapter::new(
        repository.root.join("missing-git"),
        RunnerExportLimits::default(),
    )
    .expect("adapter");
    let spawn = missing_git
        .execute(
            &repository.plan(),
            &repository.repository,
            &repository.package_path("spawn.bundle"),
        )
        .expect_err("spawn failure");
    assert_eq!(spawn.code, RunnerExportRefusalCode::GitSpawnFailed);

    let bounded = RunnerExportAdapter::new(
        repository.git.clone(),
        RunnerExportLimits {
            max_git_output_bytes: 1,
            max_package_bytes: DEFAULT_MAX_PACKAGE_BYTES,
        },
    )
    .expect("bounded adapter");
    let output = bounded
        .execute(
            &repository.plan(),
            &repository.repository,
            &repository.package_path("output.bundle"),
        )
        .expect_err("output bound");
    assert_eq!(output.code, RunnerExportRefusalCode::UnboundedGitOutput);
}

#[test]
fn package_size_bound_removes_partial_result() {
    let repository = TestRepository::create();
    let package = repository.package_path("tiny.bundle");
    let adapter = RunnerExportAdapter::new(
        repository.git.clone(),
        RunnerExportLimits {
            max_git_output_bytes: DEFAULT_MAX_GIT_OUTPUT_BYTES,
            max_package_bytes: 1,
        },
    )
    .expect("adapter");
    let error = adapter
        .execute(&repository.plan(), &repository.repository, &package)
        .expect_err("package bound");
    assert_eq!(error.code, RunnerExportRefusalCode::PackageTooLarge);
    assert!(!package.exists());
}

#[test]
fn no_replace_publication_preserves_existing_destination() {
    let repository = TestRepository::create();
    let staged = repository.output.join("staged.bundle");
    let destination = repository.output.join("existing.bundle");
    fs::write(&staged, b"reviewed-package").expect("staged package");
    fs::write(&destination, b"existing-sentinel").expect("existing destination");

    let error = publish_package_no_replace(&staged, &destination)
        .expect_err("existing destination must refuse no-replace publication");
    assert_eq!(error.code, RunnerExportRefusalCode::PackageIoFailure);
    assert_eq!(
        fs::read(&destination).expect("preserved destination"),
        b"existing-sentinel"
    );
    assert_eq!(
        fs::read(&staged).expect("preserved staged package"),
        b"reviewed-package"
    );
}

#[test]
fn public_record_excludes_private_paths() {
    let repository = TestRepository::create();
    let package = repository.package_path("private.bundle");
    let record = repository
        .adapter()
        .execute(&repository.plan(), &repository.repository, &package)
        .expect("export");
    let json = serde_json::to_string(&record).expect("json");
    let debug = format!("{record:?}");
    let private = package.display().to_string();
    assert!(!json.contains(&private));
    assert!(!debug.contains(&private));
    assert!(debug.contains("<private-package-path>"));
}

fn plan_with(
    parent: CommitId,
    candidate: CommitId,
    tree: GitTreeId,
    changed_paths: Vec<RepositoryPath>,
) -> ExactCommitHandoffPlan {
    let repository = RepositoryRef::parse("teamleaderleo/smolrunner").expect("repository");
    let verification = verification_identity(repository.clone(), candidate.clone(), tree.clone());
    let request = ExactCommitHandoffRequest::new(
        RunnerIdentity::new(RunnerIdentityClass::SelfHostedRunner, "runner-a").expect("runner"),
        WorkspaceIdentity::new("runner-a", "workspace-a").expect("workspace"),
        TargetRef::new(repository, "refs/heads/candidate").expect("target"),
        CandidateAncestryRequirement::ExactParent {
            parent: parent.clone(),
        },
        changed_paths.clone(),
        parent.clone(),
        verification.schema_version(),
        verification.receipt_digest().clone(),
        verification.command().clone(),
        verification.target_scope().clone(),
    )
    .expect("request");
    let observation = CandidateCommitObservation::new(
        candidate,
        tree,
        CandidateAncestryEvidence::new(parent.clone(), vec![parent]).expect("ancestry"),
        changed_paths,
        WorktreeObservation::new(WorktreeState::Clean, digest('c')),
        Some(verification),
    )
    .expect("observation");
    ExactCommitHandoffPlan::new(request, vec![observation]).expect("plan")
}

fn verification_identity(
    repository: RepositoryRef,
    commit: CommitId,
    tree: GitTreeId,
) -> ImmutableVerificationIdentity {
    const WORKSPACE_ROOT: &str = "/tmp/smolrunner-runner-export-workspace";
    const CACHE_PATH: &str = "/tmp/smolrunner-runner-export-workspace/cache";

    let installation_id = RunnerInstallationId::parse("runner-a").expect("installation");
    let workspace_id = RunnerWorkspaceId::parse("workspace-a").expect("workspace");
    let workspace = RunnerOwnedWorkspaceIdentity::new(
        installation_id.clone(),
        workspace_id.clone(),
        repository.clone(),
        WORKSPACE_ROOT,
    )
    .expect("workspace");
    let cache_id = CacheId::parse("cargo-target").expect("cache");
    let cache_namespace = digest('7');
    let cache = CacheIdentity::new(
        &workspace,
        cache_id.clone(),
        workspace_id.clone(),
        cache_namespace.clone(),
        CACHE_PATH,
    )
    .expect("cache");
    let tested_source = TestedSourceIdentity::Commit {
        commit: commit.clone(),
        tree,
    };
    let source = ImmutableSourceInputs::new(
        repository.clone(),
        vec![ImmutableRefInput::new(
            RepositoryRefName::parse("refs/heads/candidate").expect("ref"),
            commit.clone(),
        )],
        SourceComposition::SingleRef,
        tested_source.clone(),
    )
    .expect("source");
    let scope = ExactVerificationScope::WholePackageTests {
        package: PackageId::parse("smolrunner").expect("package"),
    };
    let command_identity = RepositoryCommandIdentity::new(
        repository.clone(),
        RepositoryCommandId::parse("canonical-tests").expect("command id"),
        digest('8'),
    );
    let command = RepositoryCommandContract::new(
        command_identity.clone(),
        scope.clone(),
        ExactBuildScope::WholePackage {
            package: PackageId::parse("smolrunner").expect("package"),
        },
        Vec::new(),
    )
    .expect("command");
    let profile = VerificationProfileContract::new(VerificationProfileDefinition {
        profile_id: VerificationProfileId::parse("runner-export").expect("profile"),
        workspace,
        source,
        required_capabilities: Vec::new(),
        optional_capabilities: Vec::new(),
        canonical_command: command,
        approved_equivalents: Vec::new(),
        resources: ResourceDefaults::new(
            MemoryPolicy::new(1_024, 0, 1_024).expect("memory"),
            ConcurrencyPolicy::new(1, 1, 1).expect("concurrency"),
        ),
        cache,
        timeout: TimeoutPolicy::new(60, Vec::new()).expect("timeout"),
        authority: VerificationAuthorityPolicy {
            workspace: WorkspaceMutationPolicy::new(
                WorkspaceMutationAuthority::ReadOnly,
                DirtyWorkspacePolicy::RequireClean,
            )
            .expect("workspace policy"),
            local_commit: LocalCommitAuthority::Forbidden,
            publication: PublicationAuthority::Forbidden,
        },
        additional_declared_deviations: Vec::new(),
    })
    .expect("profile");
    let observation = VerificationPreflightObservation {
        workspace: WorkspaceObservation::new(
            installation_id,
            workspace_id.clone(),
            repository,
            WORKSPACE_ROOT,
            WorkspaceCleanliness::Clean,
        )
        .expect("workspace observation"),
        resolved_refs: vec![ResolvedRef {
            ref_name: RepositoryRefName::parse("refs/heads/candidate").expect("ref"),
            commit: commit.clone(),
        }],
        capabilities: Vec::new(),
        resources: HostResourceObservation {
            available_memory_bytes: 1_024,
            available_swap_bytes: 0,
        },
        cache: CacheObservation {
            cache_id: cache_id.clone(),
            owner_workspace_id: workspace_id,
            namespace_digest: cache_namespace,
            present: true,
        },
        selected_command: command_identity.clone(),
        requested_authorities: BTreeSet::new(),
        private_evidence: PrivateVerificationEvidence::default(),
    };
    let preflight = evaluate_verification_preflight(&profile, observation).expect("preflight");
    let execution = VerificationExecutionEvidence {
        tested_source,
        command: command_identity,
        target_scope: scope,
        test_outcome: VerificationTestOutcome::Passed,
        phase_timings: Vec::new(),
        cache_use: CacheUseRecord {
            cache_id,
            use_state: CacheUse::Hit,
        },
        skips: Vec::new(),
        retries: Vec::new(),
        deviations: Vec::new(),
        cleanup: CleanupStatus::Complete,
        local_commit: LocalCommitState::Forbidden,
        publication: PublicationState::Forbidden,
        private_evidence: PrivateVerificationEvidence::default(),
    };
    let result = finalize_verification_result(&profile, preflight, execution).expect("result");
    assert_eq!(result.disposition(), VerificationResultDisposition::Passed);
    result.immutable_identity(digest('a'))
}

fn find_git() -> Option<PathBuf> {
    ["/usr/bin/git", "/usr/local/bin/git"]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
}

fn run_git<const N: usize>(git: &Path, cwd: &Path, arguments: [&str; N]) {
    let status = Command::new(git)
        .args(arguments)
        .current_dir(cwd)
        .status()
        .expect("run git");
    assert!(status.success());
}

fn output_line<const N: usize>(git: &Path, cwd: &Path, arguments: [&str; N]) -> String {
    String::from_utf8(output_bytes(git, cwd, arguments))
        .expect("utf8")
        .trim()
        .to_owned()
}

fn output_bytes<const N: usize>(git: &Path, cwd: &Path, arguments: [&str; N]) -> Vec<u8> {
    let output = Command::new(git)
        .args(arguments)
        .current_dir(cwd)
        .output()
        .expect("run git");
    assert!(output.status.success());
    output.stdout
}

fn commit(value: impl AsRef<str>) -> CommitId {
    CommitId::parse(value.as_ref()).expect("commit")
}

fn path(value: &str) -> RepositoryPath {
    RepositoryPath::parse(value).expect("path")
}

fn digest(character: char) -> Sha256Digest {
    Sha256Digest::parse(&format!("sha256:{}", character.to_string().repeat(64))).expect("digest")
}
