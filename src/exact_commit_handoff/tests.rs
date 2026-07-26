use std::collections::BTreeSet;

use crate::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use crate::verification_profile::{
    CacheId, CacheIdentity, CacheObservation, CacheUse, CacheUseRecord, CleanupStatus,
    ConcurrencyPolicy, DirtyWorkspacePolicy, ExactBuildScope, ExactVerificationScope,
    HostResourceObservation, ImmutableRefInput, ImmutableSourceInputs,
    ImmutableVerificationIdentity, KnownTestTarget, LocalCommitAuthority, LocalCommitState,
    MemoryPolicy, PackageId, PhaseTimeout, PrivateVerificationEvidence, PublicationAuthority,
    PublicationState, RepositoryCommandContract, RepositoryCommandId, RepositoryCommandIdentity,
    RepositoryRefName, ResolvedRef, ResourceDefaults, RunnerInstallationId,
    RunnerOwnedWorkspaceIdentity, RunnerWorkspaceId, SourceComposition, TargetName, TestFilter,
    TestedSourceIdentity, TimeoutPolicy, VerificationAuthorityPolicy,
    VerificationExecutionEvidence, VerificationPhase, VerificationPreflightObservation,
    VerificationProfileContract, VerificationProfileDefinition, VerificationProfileId,
    VerificationResultDisposition, VerificationTestOutcome, WorkspaceCleanliness,
    WorkspaceMutationAuthority, WorkspaceMutationPolicy, WorkspaceObservation,
    evaluate_verification_preflight, finalize_verification_result,
};

use super::{
    CandidateAncestryEvidence, CandidateAncestryRequirement, CandidateCommitObservation,
    CleanupDisposition, CleanupResource, ExactCommitHandoffPlan, ExactCommitHandoffReport,
    ExactCommitHandoffRequest, HandoffCleanupObservation, HandoffCleanupResult,
    HandoffExportReceipt, HandoffImportObservation, HandoffImportReceipt,
    HandoffPublicationObservation, HandoffPublicationResult, HandoffPublishAuthorization,
    HandoffRefusalCode, HandoffTransferObservation, HandoffTransferReceipt, ImportDisposition,
    PublicationDisposition, PublisherIdentityClass, RepositoryPath, RunnerIdentity,
    RunnerIdentityClass, TargetRef, TransferDisposition, WorkspaceIdentity, WorktreeObservation,
    WorktreeState, render_exact_commit_handoff_human,
};

const EXPECTED_PARENT: &str = "4263facaf3c7d30b26cae33fd1e679278ac02105";
const CANDIDATE: &str = "73e5b9fc28de0815975fad3c3d70a6a0b38399b1";

fn commit(value: &str) -> CommitId {
    CommitId::parse(value).expect("commit")
}

fn tree(character: char) -> GitTreeId {
    GitTreeId::parse(&character.to_string().repeat(40)).expect("tree")
}

fn digest(character: char) -> Sha256Digest {
    Sha256Digest::parse(&format!("sha256:{}", character.to_string().repeat(64))).expect("digest")
}

fn path(value: &str) -> RepositoryPath {
    RepositoryPath::parse(value).expect("path")
}

fn changed_paths() -> Vec<RepositoryPath> {
    vec![
        path("codex-rs/core/src/tools/code_mode/mod.rs"),
        path("codex-rs/core/src/unified_exec/process_manager.rs"),
    ]
}

fn default_scope() -> ExactVerificationScope {
    ExactVerificationScope::FilteredTest {
        target: KnownTestTarget::IntegrationTestBinary {
            package: PackageId::parse("codex-core").expect("package"),
            binary: TargetName::parse("code_mode").expect("binary"),
        },
        filter: TestFilter::parse("live_session_summary").expect("filter"),
    }
}

fn alternate_scope() -> ExactVerificationScope {
    ExactVerificationScope::LibraryTests {
        package: PackageId::parse("codex-core").expect("package"),
    }
}

fn matching_build_scope(scope: &ExactVerificationScope) -> ExactBuildScope {
    match scope {
        ExactVerificationScope::LibraryTests { package } => ExactBuildScope::LibraryTarget {
            package: package.clone(),
        },
        ExactVerificationScope::IntegrationTestBinary { package, binary } => {
            ExactBuildScope::IntegrationTestBinary {
                package: package.clone(),
                binary: binary.clone(),
            }
        }
        ExactVerificationScope::FilteredTest { target, .. } => match target {
            KnownTestTarget::Library { package } => ExactBuildScope::LibraryTarget {
                package: package.clone(),
            },
            KnownTestTarget::IntegrationTestBinary { package, binary } => {
                ExactBuildScope::IntegrationTestBinary {
                    package: package.clone(),
                    binary: binary.clone(),
                }
            }
        },
        ExactVerificationScope::WholePackageTests { package } => ExactBuildScope::WholePackage {
            package: package.clone(),
        },
        ExactVerificationScope::WholeWorkspaceTests => ExactBuildScope::WholeWorkspace,
    }
}

fn verification_identity(
    repository: RepositoryRef,
    tested_source: TestedSourceIdentity,
    command_id: &str,
    scope: ExactVerificationScope,
    disposition: VerificationResultDisposition,
    receipt_digest: Sha256Digest,
) -> ImmutableVerificationIdentity {
    const WORKSPACE_ROOT: &str = "/tmp/smolrunner-agent4-workspace";
    const CACHE_PATH: &str = "/tmp/smolrunner-agent4-workspace/cache";

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
    let (refs, composition) = match &tested_source {
        TestedSourceIdentity::Commit { commit, .. } => (
            vec![ImmutableRefInput::new(
                RepositoryRefName::parse("refs/heads/candidate").expect("ref"),
                commit.clone(),
            )],
            SourceComposition::SingleRef,
        ),
        TestedSourceIdentity::SyntheticTree { .. } => (
            vec![
                ImmutableRefInput::new(
                    RepositoryRefName::parse("refs/heads/base").expect("ref"),
                    commit(&"1".repeat(40)),
                ),
                ImmutableRefInput::new(
                    RepositoryRefName::parse("refs/heads/change").expect("ref"),
                    commit(&"2".repeat(40)),
                ),
            ],
            SourceComposition::OrderedComposition,
        ),
    };
    let resolved_refs = refs
        .iter()
        .map(|input| ResolvedRef {
            ref_name: input.ref_name.clone(),
            commit: input.expected_commit.clone(),
        })
        .collect();
    let source =
        ImmutableSourceInputs::new(repository.clone(), refs, composition, tested_source.clone())
            .expect("source");
    let command_identity = RepositoryCommandIdentity::new(
        repository.clone(),
        RepositoryCommandId::parse(command_id).expect("command id"),
        digest('8'),
    );
    let command = RepositoryCommandContract::new(
        command_identity.clone(),
        scope.clone(),
        matching_build_scope(&scope),
        Vec::new(),
    )
    .expect("command");
    let profile = VerificationProfileContract::new(VerificationProfileDefinition {
        profile_id: VerificationProfileId::parse("codex-live-session").expect("profile"),
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
        timeout: TimeoutPolicy::new(
            60,
            vec![PhaseTimeout {
                phase: VerificationPhase::Test,
                seconds: 60,
            }],
        )
        .expect("timeout"),
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
        resolved_refs,
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
    let (test_outcome, cleanup) = match disposition {
        VerificationResultDisposition::Passed => {
            (VerificationTestOutcome::Passed, CleanupStatus::Complete)
        }
        VerificationResultDisposition::Failed => {
            (VerificationTestOutcome::Failed, CleanupStatus::Complete)
        }
        VerificationResultDisposition::CleanupIncomplete => {
            (VerificationTestOutcome::Passed, CleanupStatus::Incomplete)
        }
    };
    let execution = VerificationExecutionEvidence {
        tested_source,
        command: command_identity,
        target_scope: scope,
        test_outcome,
        phase_timings: Vec::new(),
        cache_use: CacheUseRecord {
            cache_id,
            use_state: CacheUse::Hit,
        },
        skips: Vec::new(),
        retries: Vec::new(),
        deviations: Vec::new(),
        cleanup,
        local_commit: LocalCommitState::Forbidden,
        publication: PublicationState::Forbidden,
        private_evidence: PrivateVerificationEvidence::default(),
    };
    let result = finalize_verification_result(&profile, preflight, execution).expect("result");
    assert_eq!(result.disposition(), disposition);
    result.immutable_identity(receipt_digest)
}

fn good_verification_identity() -> ImmutableVerificationIdentity {
    verification_identity(
        RepositoryRef::parse("teamleaderleo/codex").expect("repository"),
        TestedSourceIdentity::Commit {
            commit: commit(CANDIDATE),
            tree: tree('b'),
        },
        "codex-focused-tests",
        default_scope(),
        VerificationResultDisposition::Passed,
        digest('a'),
    )
}

fn request_with(
    expected: &ImmutableVerificationIdentity,
    expected_schema: u8,
    expected_digest: Sha256Digest,
) -> ExactCommitHandoffRequest {
    ExactCommitHandoffRequest::new(
        RunnerIdentity::new(RunnerIdentityClass::LimaGuest, "smolrunner").expect("runner"),
        WorkspaceIdentity::new("smolrunner", "codex-orphan-integration").expect("workspace"),
        TargetRef::new(
            RepositoryRef::parse("teamleaderleo/codex").expect("repository"),
            "refs/heads/fix/code-mode-live-session-summary",
        )
        .expect("target"),
        CandidateAncestryRequirement::ExactParent {
            parent: commit(EXPECTED_PARENT),
        },
        changed_paths(),
        commit(EXPECTED_PARENT),
        expected_schema,
        expected_digest,
        expected.command().clone(),
        expected.target_scope().clone(),
    )
    .expect("request")
}

fn request() -> ExactCommitHandoffRequest {
    let expected = good_verification_identity();
    request_with(&expected, 1, digest('a'))
}

fn candidate_with_verification(
    worktree: WorktreeState,
    paths: Vec<RepositoryPath>,
    verification_identity: Option<ImmutableVerificationIdentity>,
) -> CandidateCommitObservation {
    CandidateCommitObservation::new(
        commit(CANDIDATE),
        tree('b'),
        CandidateAncestryEvidence::new(commit(EXPECTED_PARENT), vec![commit(EXPECTED_PARENT)])
            .expect("ancestry"),
        paths,
        WorktreeObservation::new(worktree, digest('c')),
        verification_identity,
    )
    .expect("candidate")
}

fn candidate(
    worktree: WorktreeState,
    paths: Vec<RepositoryPath>,
    verification: bool,
) -> CandidateCommitObservation {
    candidate_with_verification(
        worktree,
        paths,
        verification.then(good_verification_identity),
    )
}

fn plan() -> ExactCommitHandoffPlan {
    ExactCommitHandoffPlan::new(
        request(),
        vec![candidate(WorktreeState::Clean, changed_paths(), true)],
    )
    .expect("plan")
}

fn successful_phases() -> (
    ExactCommitHandoffPlan,
    HandoffExportReceipt,
    HandoffTransferReceipt,
    HandoffImportReceipt,
    HandoffPublishAuthorization,
    HandoffPublicationResult,
    HandoffCleanupResult,
) {
    let plan = plan();
    let export = HandoffExportReceipt::new(&plan, digest('d'), commit(CANDIDATE), tree('b'))
        .expect("export");
    let transfer = HandoffTransferReceipt::new(
        &export,
        HandoffTransferObservation::new(
            TransferDisposition::Complete,
            digest('d'),
            commit(CANDIDATE),
            tree('b'),
        ),
    )
    .expect("transfer");
    let import = HandoffImportReceipt::new(
        &transfer,
        HandoffImportObservation::new(
            ImportDisposition::Complete,
            commit(CANDIDATE),
            commit(EXPECTED_PARENT),
            tree('b'),
            changed_paths(),
            good_verification_identity(),
        )
        .expect("import observation"),
    )
    .expect("import");
    let authorization = HandoffPublishAuthorization::new(
        &import,
        PublisherIdentityClass::CredentialedOperatorHost,
        commit(EXPECTED_PARENT),
    )
    .expect("authorization");
    let publication = HandoffPublicationResult::new(
        &authorization,
        HandoffPublicationObservation::new(
            PublicationDisposition::Published,
            commit(CANDIDATE),
            tree('b'),
        ),
    )
    .expect("publication");
    let cleanup = HandoffCleanupResult::new(
        &plan,
        HandoffCleanupObservation::new(Vec::new(), WorktreeState::Clean),
    );
    (
        plan,
        export,
        transfer,
        import,
        authorization,
        publication,
        cleanup,
    )
}

#[test]
fn recorded_git_bundle_case_preserves_exact_commit_and_tree() {
    let (plan, export, transfer, import, authorization, publication, cleanup) = successful_phases();
    let report = ExactCommitHandoffReport::new(
        &plan,
        &export,
        &transfer,
        &import,
        &authorization,
        &publication,
        &cleanup,
    )
    .expect("report");
    assert_eq!(report.candidate_commit().as_str(), CANDIDATE);
    assert_eq!(report.final_remote_commit().as_str(), CANDIDATE);
    assert_eq!(plan.identity().tree().as_str(), tree('b').as_str());
    assert_eq!(export.identity(), plan.identity());
    assert_eq!(transfer.identity(), plan.identity());
    assert_eq!(import.identity(), plan.identity());
    assert_eq!(authorization.identity(), plan.identity());
    assert_eq!(publication.identity(), plan.identity());
    assert!(authorization.fast_forward_only());
}

#[test]
fn moved_remote_ref_refuses_publish_authorization() {
    let (_, _, _, import, _, _, _) = successful_phases();
    let error = HandoffPublishAuthorization::new(
        &import,
        PublisherIdentityClass::CredentialedOperatorHost,
        commit(&"9".repeat(40)),
    )
    .expect_err("moved ref");
    assert_eq!(error.code, HandoffRefusalCode::MovedRemoteRef);
}

#[test]
fn dirty_workspace_refuses_planning() {
    let error = ExactCommitHandoffPlan::new(
        request(),
        vec![candidate(WorktreeState::Dirty, changed_paths(), true)],
    )
    .expect_err("dirty workspace");
    assert_eq!(error.code, HandoffRefusalCode::DirtyWorkspace);
}

#[test]
fn extra_changed_path_refuses_planning() {
    let mut paths = changed_paths();
    paths.push(path("Cargo.lock"));
    let error = ExactCommitHandoffPlan::new(
        request(),
        vec![candidate(WorktreeState::Clean, paths, true)],
    )
    .expect_err("extra path");
    assert_eq!(error.code, HandoffRefusalCode::ChangedPathOutsideAllowlist);
}

#[test]
fn ambiguous_candidate_refuses_planning() {
    let error = ExactCommitHandoffPlan::new(
        request(),
        vec![
            candidate(WorktreeState::Clean, changed_paths(), true),
            candidate(WorktreeState::Clean, changed_paths(), true),
        ],
    )
    .expect_err("ambiguous candidate");
    assert_eq!(error.code, HandoffRefusalCode::AmbiguousCandidate);
}

#[test]
fn altered_transfer_package_is_refused() {
    let plan = plan();
    let export = HandoffExportReceipt::new(&plan, digest('d'), commit(CANDIDATE), tree('b'))
        .expect("export");
    let error = HandoffTransferReceipt::new(
        &export,
        HandoffTransferObservation::new(
            TransferDisposition::Complete,
            digest('e'),
            commit(CANDIDATE),
            tree('b'),
        ),
    )
    .expect_err("altered package");
    assert_eq!(error.code, HandoffRefusalCode::AlteredTransferPackage);
}

#[test]
fn missing_verification_receipt_refuses_planning() {
    let error = ExactCommitHandoffPlan::new(
        request(),
        vec![candidate(WorktreeState::Clean, changed_paths(), false)],
    )
    .expect_err("missing receipt");
    assert_eq!(error.code, HandoffRefusalCode::MissingVerificationIdentity);
}

#[test]
fn mismatched_imported_tree_is_refused() {
    let plan = plan();
    let export = HandoffExportReceipt::new(&plan, digest('d'), commit(CANDIDATE), tree('b'))
        .expect("export");
    let transfer = HandoffTransferReceipt::new(
        &export,
        HandoffTransferObservation::new(
            TransferDisposition::Complete,
            digest('d'),
            commit(CANDIDATE),
            tree('b'),
        ),
    )
    .expect("transfer");
    let error = HandoffImportReceipt::new(
        &transfer,
        HandoffImportObservation::new(
            ImportDisposition::Complete,
            commit(CANDIDATE),
            commit(EXPECTED_PARENT),
            tree('f'),
            changed_paths(),
            good_verification_identity(),
        )
        .expect("observation"),
    )
    .expect_err("tree mismatch");
    assert_eq!(error.code, HandoffRefusalCode::ImportedIdentityMismatch);
}

#[test]
fn mismatched_imported_parent_is_refused() {
    let plan = plan();
    let export = HandoffExportReceipt::new(&plan, digest('d'), commit(CANDIDATE), tree('b'))
        .expect("export");
    let transfer = HandoffTransferReceipt::new(
        &export,
        HandoffTransferObservation::new(
            TransferDisposition::Complete,
            digest('d'),
            commit(CANDIDATE),
            tree('b'),
        ),
    )
    .expect("transfer");
    let error = HandoffImportReceipt::new(
        &transfer,
        HandoffImportObservation::new(
            ImportDisposition::Complete,
            commit(CANDIDATE),
            commit(&"9".repeat(40)),
            tree('b'),
            changed_paths(),
            good_verification_identity(),
        )
        .expect("observation"),
    )
    .expect_err("parent mismatch");
    assert_eq!(error.code, HandoffRefusalCode::ImportedIdentityMismatch);
}

#[test]
fn mismatched_imported_verification_identity_is_refused() {
    let plan = plan();
    let export = HandoffExportReceipt::new(&plan, digest('d'), commit(CANDIDATE), tree('b'))
        .expect("export");
    let transfer = HandoffTransferReceipt::new(
        &export,
        HandoffTransferObservation::new(
            TransferDisposition::Complete,
            digest('d'),
            commit(CANDIDATE),
            tree('b'),
        ),
    )
    .expect("transfer");
    let mismatched = verification_identity(
        RepositoryRef::parse("teamleaderleo/codex").expect("repository"),
        TestedSourceIdentity::Commit {
            commit: commit(CANDIDATE),
            tree: tree('b'),
        },
        "different-command",
        default_scope(),
        VerificationResultDisposition::Passed,
        digest('a'),
    );
    let error = HandoffImportReceipt::new(
        &transfer,
        HandoffImportObservation::new(
            ImportDisposition::Complete,
            commit(CANDIDATE),
            commit(EXPECTED_PARENT),
            tree('b'),
            changed_paths(),
            mismatched,
        )
        .expect("observation"),
    )
    .expect_err("verification identity mismatch");
    assert_eq!(error.code, HandoffRefusalCode::ImportedIdentityMismatch);
}

#[test]
fn failed_verification_refuses_planning() {
    let expected = good_verification_identity();
    let failed = verification_identity(
        RepositoryRef::parse("teamleaderleo/codex").expect("repository"),
        TestedSourceIdentity::Commit {
            commit: commit(CANDIDATE),
            tree: tree('b'),
        },
        "codex-focused-tests",
        default_scope(),
        VerificationResultDisposition::Failed,
        digest('a'),
    );
    let error = ExactCommitHandoffPlan::new(
        request_with(&expected, 1, digest('a')),
        vec![candidate_with_verification(
            WorktreeState::Clean,
            changed_paths(),
            Some(failed),
        )],
    )
    .expect_err("failed verification");
    assert_eq!(error.code, HandoffRefusalCode::VerificationFailed);
}

#[test]
fn incomplete_verification_cleanup_refuses_planning() {
    let expected = good_verification_identity();
    let incomplete = verification_identity(
        RepositoryRef::parse("teamleaderleo/codex").expect("repository"),
        TestedSourceIdentity::Commit {
            commit: commit(CANDIDATE),
            tree: tree('b'),
        },
        "codex-focused-tests",
        default_scope(),
        VerificationResultDisposition::CleanupIncomplete,
        digest('a'),
    );
    let error = ExactCommitHandoffPlan::new(
        request_with(&expected, 1, digest('a')),
        vec![candidate_with_verification(
            WorktreeState::Clean,
            changed_paths(),
            Some(incomplete),
        )],
    )
    .expect_err("incomplete cleanup");
    assert_eq!(
        error.code,
        HandoffRefusalCode::VerificationCleanupIncomplete
    );
}

#[test]
fn synthetic_tree_verification_refuses_commit_publication() {
    let expected = good_verification_identity();
    let synthetic = verification_identity(
        RepositoryRef::parse("teamleaderleo/codex").expect("repository"),
        TestedSourceIdentity::SyntheticTree { tree: tree('b') },
        "codex-focused-tests",
        default_scope(),
        VerificationResultDisposition::Passed,
        digest('a'),
    );
    let error = ExactCommitHandoffPlan::new(
        request_with(&expected, 1, digest('a')),
        vec![candidate_with_verification(
            WorktreeState::Clean,
            changed_paths(),
            Some(synthetic),
        )],
    )
    .expect_err("synthetic tree");
    assert_eq!(error.code, HandoffRefusalCode::VerificationSourceNotCommit);
}

#[test]
fn verification_schema_drift_is_refused() {
    let expected = good_verification_identity();
    let error = ExactCommitHandoffPlan::new(
        request_with(&expected, 2, digest('a')),
        vec![candidate_with_verification(
            WorktreeState::Clean,
            changed_paths(),
            Some(good_verification_identity()),
        )],
    )
    .expect_err("schema drift");
    assert_eq!(error.code, HandoffRefusalCode::VerificationSchemaMismatch);
}

#[test]
fn verification_digest_drift_is_refused() {
    let expected = good_verification_identity();
    let error = ExactCommitHandoffPlan::new(
        request_with(&expected, 1, digest('e')),
        vec![candidate_with_verification(
            WorktreeState::Clean,
            changed_paths(),
            Some(good_verification_identity()),
        )],
    )
    .expect_err("digest drift");
    assert_eq!(error.code, HandoffRefusalCode::VerificationDigestMismatch);
}

#[test]
fn verification_repository_mismatch_is_refused() {
    let expected = good_verification_identity();
    let actual = verification_identity(
        RepositoryRef::parse("teamleaderleo/other").expect("repository"),
        TestedSourceIdentity::Commit {
            commit: commit(CANDIDATE),
            tree: tree('b'),
        },
        "codex-focused-tests",
        default_scope(),
        VerificationResultDisposition::Passed,
        digest('a'),
    );
    let error = ExactCommitHandoffPlan::new(
        request_with(&expected, 1, digest('a')),
        vec![candidate_with_verification(
            WorktreeState::Clean,
            changed_paths(),
            Some(actual),
        )],
    )
    .expect_err("repository mismatch");
    assert_eq!(
        error.code,
        HandoffRefusalCode::VerificationRepositoryMismatch
    );
}

#[test]
fn verification_commit_mismatch_is_refused() {
    let expected = good_verification_identity();
    let actual = verification_identity(
        RepositoryRef::parse("teamleaderleo/codex").expect("repository"),
        TestedSourceIdentity::Commit {
            commit: commit(&"9".repeat(40)),
            tree: tree('b'),
        },
        "codex-focused-tests",
        default_scope(),
        VerificationResultDisposition::Passed,
        digest('a'),
    );
    let error = ExactCommitHandoffPlan::new(
        request_with(&expected, 1, digest('a')),
        vec![candidate_with_verification(
            WorktreeState::Clean,
            changed_paths(),
            Some(actual),
        )],
    )
    .expect_err("commit mismatch");
    assert_eq!(error.code, HandoffRefusalCode::VerificationCommitMismatch);
}

#[test]
fn verification_tree_mismatch_is_refused() {
    let expected = good_verification_identity();
    let actual = verification_identity(
        RepositoryRef::parse("teamleaderleo/codex").expect("repository"),
        TestedSourceIdentity::Commit {
            commit: commit(CANDIDATE),
            tree: tree('f'),
        },
        "codex-focused-tests",
        default_scope(),
        VerificationResultDisposition::Passed,
        digest('a'),
    );
    let error = ExactCommitHandoffPlan::new(
        request_with(&expected, 1, digest('a')),
        vec![candidate_with_verification(
            WorktreeState::Clean,
            changed_paths(),
            Some(actual),
        )],
    )
    .expect_err("tree mismatch");
    assert_eq!(error.code, HandoffRefusalCode::VerificationTreeMismatch);
}

#[test]
fn verification_command_mismatch_is_refused() {
    let expected = good_verification_identity();
    let actual = verification_identity(
        RepositoryRef::parse("teamleaderleo/codex").expect("repository"),
        TestedSourceIdentity::Commit {
            commit: commit(CANDIDATE),
            tree: tree('b'),
        },
        "different-command",
        default_scope(),
        VerificationResultDisposition::Passed,
        digest('a'),
    );
    let error = ExactCommitHandoffPlan::new(
        request_with(&expected, 1, digest('a')),
        vec![candidate_with_verification(
            WorktreeState::Clean,
            changed_paths(),
            Some(actual),
        )],
    )
    .expect_err("command mismatch");
    assert_eq!(error.code, HandoffRefusalCode::VerificationCommandMismatch);
}

#[test]
fn verification_target_scope_mismatch_is_refused() {
    let expected = good_verification_identity();
    let actual = verification_identity(
        RepositoryRef::parse("teamleaderleo/codex").expect("repository"),
        TestedSourceIdentity::Commit {
            commit: commit(CANDIDATE),
            tree: tree('b'),
        },
        "codex-focused-tests",
        alternate_scope(),
        VerificationResultDisposition::Passed,
        digest('a'),
    );
    let error = ExactCommitHandoffPlan::new(
        request_with(&expected, 1, digest('a')),
        vec![candidate_with_verification(
            WorktreeState::Clean,
            changed_paths(),
            Some(actual),
        )],
    )
    .expect_err("target scope mismatch");
    assert_eq!(
        error.code,
        HandoffRefusalCode::VerificationTargetScopeMismatch
    );
}

#[test]
fn failed_import_is_refused() {
    let plan = plan();
    let export = HandoffExportReceipt::new(&plan, digest('d'), commit(CANDIDATE), tree('b'))
        .expect("export");
    let transfer = HandoffTransferReceipt::new(
        &export,
        HandoffTransferObservation::new(
            TransferDisposition::Complete,
            digest('d'),
            commit(CANDIDATE),
            tree('b'),
        ),
    )
    .expect("transfer");
    let error = HandoffImportReceipt::new(
        &transfer,
        HandoffImportObservation::new(
            ImportDisposition::Failed,
            commit(CANDIDATE),
            commit(EXPECTED_PARENT),
            tree('b'),
            changed_paths(),
            good_verification_identity(),
        )
        .expect("observation"),
    )
    .expect_err("failed import");
    assert_eq!(error.code, HandoffRefusalCode::ImportFailed);
}

#[test]
fn incomplete_cleanup_blocks_final_report() {
    let (plan, export, transfer, import, authorization, publication, _) = successful_phases();
    let cleanup = HandoffCleanupResult::new(
        &plan,
        HandoffCleanupObservation::new(vec![CleanupResource::ExportPackage], WorktreeState::Clean),
    );
    assert_eq!(cleanup.disposition(), CleanupDisposition::Incomplete);
    let error = ExactCommitHandoffReport::new(
        &plan,
        &export,
        &transfer,
        &import,
        &authorization,
        &publication,
        &cleanup,
    )
    .expect_err("incomplete cleanup");
    assert_eq!(error.code, HandoffRefusalCode::CleanupIncomplete);
}

#[test]
fn public_reports_are_bounded_and_exclude_private_paths_and_credentials() {
    let (plan, export, transfer, import, authorization, publication, cleanup) = successful_phases();
    let report = ExactCommitHandoffReport::new(
        &plan,
        &export,
        &transfer,
        &import,
        &authorization,
        &publication,
        &cleanup,
    )
    .expect("report");
    let json = serde_json::to_string(&report).expect("json");
    let human = render_exact_commit_handoff_human(&report);
    let debug = format!("{report:?}");
    for forbidden in [
        "/home/lima/codex-orphan-integration",
        "/Users/publisher/.ssh/id_ed25519",
        "ghp_PRIVATE_TOKEN",
        "credential_helper",
    ] {
        assert!(!json.contains(forbidden));
        assert!(!human.contains(forbidden));
        assert!(!debug.contains(forbidden));
    }
    assert!(json.contains(CANDIDATE));
    assert!(human.contains(CANDIDATE));
    assert!(json.contains("codex-orphan-integration"));
    assert!(human.contains("codex-orphan-integration"));
}

#[test]
fn repository_paths_and_target_refs_reject_escape_forms() {
    assert!(RepositoryPath::parse("../secret").is_err());
    assert!(RepositoryPath::parse("/etc/passwd").is_err());
    assert!(RepositoryPath::parse("a\\b").is_err());
    let repository = RepositoryRef::parse("teamleaderleo/codex").expect("repository");
    assert!(TargetRef::new(repository.clone(), "main").is_err());
    assert!(TargetRef::new(repository.clone(), "refs/heads/../main").is_err());
    assert!(TargetRef::new(repository, "refs/heads/main.lock").is_err());
}
