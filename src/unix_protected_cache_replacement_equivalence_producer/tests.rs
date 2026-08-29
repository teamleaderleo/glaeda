use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use crate::personal_worker_queue::PersonalWorkerSourceIdentity;
use crate::protected_cache_replacement_equivalence::{
    ProtectedCacheReconstructionBinding, decode_protected_cache_replacement_equivalence_receipt,
};

use super::*;

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

struct Fixture {
    root: PathBuf,
    materialization_root: PathBuf,
    receipt_root: PathBuf,
    binaries: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "glaeda-protected-replacement-{name}-{}-{sequence}",
            std::process::id()
        ));
        let materialization_root = root.join("materializations");
        let receipt_root = root.join("receipts");
        let binaries = root.join("bin");
        for directory in [&root, &materialization_root, &receipt_root, &binaries] {
            fs::create_dir(directory).expect("create fixture directory");
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
                .expect("set private fixture directory");
        }
        Self {
            root,
            materialization_root,
            receipt_root,
            binaries,
        }
    }

    fn binary(&self, name: &str, candidates: &[&str]) -> PathBuf {
        let source = candidates
            .iter()
            .map(Path::new)
            .find(|candidate| candidate.is_file())
            .unwrap_or_else(|| panic!("missing fixture binary {name}"));
        let directory = self.binaries.join(name);
        fs::create_dir(&directory).expect("create fixture executable directory");
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
            .expect("set fixture executable directory mode");
        let destination = directory.join(source.file_name().expect("fixture executable basename"));
        let status = Command::new("/usr/bin/cp")
            .arg("--")
            .arg(source)
            .arg(&destination)
            .status()
            .expect("copy fixture executable");
        assert!(status.success());
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o555))
            .expect("set executable mode");
        destination
    }

    #[allow(clippy::too_many_arguments)]
    fn plan(
        &self,
        state: &str,
        candidate: &str,
        materializer_path: &Path,
        materializer_arguments: Vec<ReviewedLaunchValue>,
        validator_path: &Path,
        validator_arguments: Vec<ReviewedLaunchValue>,
        materialization_timeout: Duration,
    ) -> ProtectedCacheReplacementProductionPlan {
        let materializer_identity = identity(materializer_path);
        let validator_identity = identity(validator_path);
        let plan_digest = derive_protected_cache_replacement_program_generation_digest(
            materializer_path,
            &materializer_identity,
            &materializer_arguments,
        )
        .expect("materializer digest");
        let validator_digest = derive_protected_cache_replacement_program_generation_digest(
            validator_path,
            &validator_identity,
            &validator_arguments,
        )
        .expect("validator digest");
        let reconstruction = ProtectedCacheReconstructionBinding::new(
            PersonalWorkerSourceIdentity::new(
                RepositoryRef::parse("teamleaderleo/glaeda").unwrap(),
                CommitId::parse(&"a".repeat(40)).unwrap(),
                GitTreeId::parse(&"b".repeat(40)).unwrap(),
            ),
            digest('c'),
            plan_digest.clone(),
            validator_digest.clone(),
            digest('d'),
        );
        ProtectedCacheReplacementProductionPlan::new(
            &plan_authority(),
            ProtectedCacheGenerationFamily::CargoTargetV1,
            ProtectedCacheNamespaceIdentity::parse(digest('e').as_str()).unwrap(),
            CacheStateId::parse(state).unwrap(),
            reconstruction,
            digest('f'),
            &self.materialization_root,
            identity(&self.materialization_root),
            &self.receipt_root,
            identity(&self.receipt_root),
            candidate,
            ProtectedCacheReplacementProgram::new(
                &plan_authority(),
                materializer_path,
                materializer_identity,
                materializer_arguments,
                plan_digest,
            )
            .unwrap(),
            ProtectedCacheReplacementProgram::new(
                &plan_authority(),
                validator_path,
                validator_identity,
                validator_arguments,
                validator_digest,
            )
            .unwrap(),
            materialization_timeout,
            Duration::from_secs(5),
            Duration::from_secs(5),
        )
        .expect("production plan")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn identity(path: &Path) -> ReviewedFilesystemIdentity {
    let metadata = fs::metadata(path).expect("fixture metadata");
    ReviewedFilesystemIdentity::new(
        metadata.dev(),
        metadata.ino(),
        metadata.uid(),
        metadata.gid(),
        metadata.mode() & 0o7777,
    )
    .unwrap()
}

fn digest(character: char) -> Sha256Digest {
    Sha256Digest::parse(&format!("sha256:{}", character.to_string().repeat(64))).unwrap()
}

const fn plan_authority() -> ProtectedCacheReplacementPlanAuthority {
    ProtectedCacheReplacementPlanAuthority { _private: () }
}

fn touch_and_test_plan(
    fixture: &Fixture,
    state: &str,
    candidate: &str,
    artifact: &str,
) -> ProtectedCacheReplacementProductionPlan {
    let touch = fixture.binary(&format!("touch-{candidate}"), &["/usr/bin/touch"]);
    let test = fixture.binary(&format!("test-{candidate}"), &["/usr/bin/test"]);
    fixture.plan(
        state,
        candidate,
        &touch,
        vec![ReviewedLaunchValue::plain(artifact)],
        &test,
        vec![
            ReviewedLaunchValue::plain("-f"),
            ReviewedLaunchValue::plain(artifact),
        ],
        Duration::from_secs(5),
    )
}

#[test]
fn fresh_exact_materialization_publishes_one_canonical_receipt() {
    let fixture = Fixture::new("success");
    let plan = touch_and_test_plan(&fixture, "state-one", "candidate-one", "artifact");

    let production =
        produce_protected_cache_replacement_equivalence(&plan).expect("physical production");

    assert_eq!(
        production.authority(),
        ProtectedCacheReplacementProductionAuthority::FreshMaterializationValidatedAndPersisted
    );
    assert!(
        fixture
            .materialization_root
            .join("candidate-one/artifact")
            .is_file()
    );
    let bytes = fs::read(fixture.receipt_root.join(receipt_name(&plan.state_id))).unwrap();
    let decoded = decode_protected_cache_replacement_equivalence_receipt(&bytes).unwrap();
    assert_eq!(decoded.binding(), production.receipt().binding());
}

#[test]
fn byte_identical_second_production_accepts_exact_receipt_replay() {
    let fixture = Fixture::new("replay");
    let first = touch_and_test_plan(&fixture, "state-replay", "candidate-a", "artifact");
    let second = touch_and_test_plan(&fixture, "state-replay", "candidate-b", "artifact");

    let first = produce_protected_cache_replacement_equivalence(&first).unwrap();
    let second = produce_protected_cache_replacement_equivalence(&second).unwrap();

    assert_eq!(first.receipt().binding(), second.receipt().binding());
}

#[test]
fn validator_failure_leaves_candidate_but_no_success_receipt() {
    let fixture = Fixture::new("validator-failure");
    let touch = fixture.binary("touch", &["/usr/bin/touch"]);
    let false_program = fixture.binary("false", &["/usr/bin/false"]);
    let plan = fixture.plan(
        "state-failed-validator",
        "candidate-failed-validator",
        &touch,
        vec![ReviewedLaunchValue::plain("artifact")],
        &false_program,
        Vec::new(),
        Duration::from_secs(5),
    );

    let error = produce_protected_cache_replacement_equivalence(&plan)
        .expect_err("failed validator must not publish");

    assert_eq!(
        error.kind(),
        ProtectedCacheReplacementProductionErrorKind::Execution
    );
    assert!(
        fixture
            .materialization_root
            .join("candidate-failed-validator/artifact")
            .is_file()
    );
    assert!(
        !fixture
            .receipt_root
            .join(receipt_name(&plan.state_id))
            .exists()
    );
}

#[test]
fn preexisting_candidate_is_never_adopted_or_changed() {
    let fixture = Fixture::new("existing");
    let candidate = fixture.materialization_root.join("already-there");
    fs::create_dir(&candidate).unwrap();
    fs::set_permissions(&candidate, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(candidate.join("sentinel"), "preserve").unwrap();
    let plan = touch_and_test_plan(&fixture, "state-existing", "already-there", "new-artifact");

    let error = produce_protected_cache_replacement_equivalence(&plan)
        .expect_err("existing candidate must fail");

    assert_eq!(
        error.kind(),
        ProtectedCacheReplacementProductionErrorKind::CandidateExists
    );
    assert_eq!(
        fs::read_to_string(candidate.join("sentinel")).unwrap(),
        "preserve"
    );
    assert!(!candidate.join("new-artifact").exists());
}

#[test]
fn silent_materializer_timeout_leaves_no_receipt() {
    let fixture = Fixture::new("timeout");
    let sleep = fixture.binary("sleep", &["/usr/bin/sleep"]);
    let true_program = fixture.binary("true", &["/usr/bin/true"]);
    let plan = fixture.plan(
        "state-timeout",
        "candidate-timeout",
        &sleep,
        vec![ReviewedLaunchValue::plain("30")],
        &true_program,
        Vec::new(),
        Duration::from_millis(50),
    );

    let error = produce_protected_cache_replacement_equivalence(&plan)
        .expect_err("materializer must time out");

    assert_eq!(
        error.kind(),
        ProtectedCacheReplacementProductionErrorKind::Execution
    );
    assert!(
        fixture
            .materialization_root
            .join("candidate-timeout")
            .is_dir()
    );
    assert!(
        !fixture
            .receipt_root
            .join(receipt_name(&plan.state_id))
            .exists()
    );
}

#[test]
fn symlink_output_is_rejected_after_validator_success() {
    let fixture = Fixture::new("symlink-output");
    let ln = fixture.binary("ln", &["/usr/bin/ln"]);
    let true_program = fixture.binary("true", &["/usr/bin/true"]);
    let plan = fixture.plan(
        "state-symlink",
        "candidate-symlink",
        &ln,
        vec![
            ReviewedLaunchValue::plain("-s"),
            ReviewedLaunchValue::plain("target"),
            ReviewedLaunchValue::plain("link"),
        ],
        &true_program,
        Vec::new(),
        Duration::from_secs(5),
    );

    let error = produce_protected_cache_replacement_equivalence(&plan)
        .expect_err("symlink output must fail");

    assert_eq!(
        error.kind(),
        ProtectedCacheReplacementProductionErrorKind::OutputUnsafe
    );
    assert!(
        fixture
            .materialization_root
            .join("candidate-symlink/link")
            .is_symlink()
    );
    assert!(
        !fixture
            .receipt_root
            .join(receipt_name(&plan.state_id))
            .exists()
    );
}

#[test]
fn different_actual_output_conflicts_with_existing_state_receipt() {
    let fixture = Fixture::new("conflict");
    let first = touch_and_test_plan(&fixture, "state-conflict", "candidate-first", "first");
    let second = touch_and_test_plan(&fixture, "state-conflict", "candidate-second", "second");
    produce_protected_cache_replacement_equivalence(&first).unwrap();

    let error = produce_protected_cache_replacement_equivalence(&second)
        .expect_err("different output must conflict");

    assert_eq!(
        error.kind(),
        ProtectedCacheReplacementProductionErrorKind::ReceiptConflict
    );
    assert!(
        fixture
            .materialization_root
            .join("candidate-second/second")
            .is_file()
    );
}

#[test]
fn complete_internal_hardlink_topology_is_accepted() {
    let fixture = Fixture::new("internal-hardlinks");
    let python = fixture.binary("python", &["/usr/bin/python3"]);
    let true_program = fixture.binary("true", &["/usr/bin/true"]);
    let code = "import os; f=os.open('a',os.O_CREAT|os.O_WRONLY,0o644); os.write(f,b'x'); os.close(f); os.link('a','b')";
    let plan = fixture.plan(
        "state-internal-hardlinks",
        "candidate-internal-hardlinks",
        &python,
        vec![
            ReviewedLaunchValue::plain("-c"),
            ReviewedLaunchValue::plain(code),
        ],
        &true_program,
        Vec::new(),
        Duration::from_secs(5),
    );

    produce_protected_cache_replacement_equivalence(&plan)
        .expect("complete internal hard links must be accepted");

    let first = fs::metadata(
        fixture
            .materialization_root
            .join("candidate-internal-hardlinks/a"),
    )
    .unwrap();
    let second = fs::metadata(
        fixture
            .materialization_root
            .join("candidate-internal-hardlinks/b"),
    )
    .unwrap();
    assert_eq!(first.ino(), second.ino());
    assert_eq!(first.nlink(), 2);
}

#[test]
fn hardlink_to_state_outside_candidate_is_rejected() {
    let fixture = Fixture::new("external-hardlink");
    fs::write(fixture.materialization_root.join("external"), "outside").unwrap();
    let python = fixture.binary("python", &["/usr/bin/python3"]);
    let true_program = fixture.binary("true", &["/usr/bin/true"]);
    let plan = fixture.plan(
        "state-external-hardlink",
        "candidate-external-hardlink",
        &python,
        vec![
            ReviewedLaunchValue::plain("-c"),
            ReviewedLaunchValue::plain("import os; os.link('../external','linked')"),
        ],
        &true_program,
        Vec::new(),
        Duration::from_secs(5),
    );

    let error = produce_protected_cache_replacement_equivalence(&plan)
        .expect_err("external hard link must fail");

    assert_eq!(
        error.kind(),
        ProtectedCacheReplacementProductionErrorKind::OutputUnsafe
    );
    assert!(
        !fixture
            .receipt_root
            .join(receipt_name(&plan.state_id))
            .exists()
    );
}

#[test]
fn launcher_writable_program_is_rejected_during_review() {
    let fixture = Fixture::new("writable-program");
    let program = fixture.binary("true", &["/usr/bin/true"]);
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();

    let error = ProtectedCacheReplacementProgram::new(
        &plan_authority(),
        &program,
        identity(&program),
        Vec::new(),
        digest('0'),
    )
    .expect_err("launcher-writable executable must fail");

    assert_eq!(
        error.kind(),
        ProtectedCacheReplacementProductionErrorKind::ProgramIdentity
    );
}

#[test]
fn complete_abandoned_stage_is_recovered_before_exact_replay() {
    let fixture = Fixture::new("stage-recovery");
    let first = touch_and_test_plan(&fixture, "state-recovery", "candidate-first", "artifact");
    produce_protected_cache_replacement_equivalence(&first).unwrap();
    let final_path = fixture.receipt_root.join(receipt_name(&first.state_id));
    let stage_path = fixture.receipt_root.join(stage_name(&first.state_id));
    fs::rename(&final_path, &stage_path).unwrap();
    let second = touch_and_test_plan(&fixture, "state-recovery", "candidate-second", "artifact");

    produce_protected_cache_replacement_equivalence(&second)
        .expect("canonical abandoned stage must recover");

    assert!(final_path.is_file());
    assert!(!stage_path.exists());
}

#[test]
fn corrupt_stage_blocks_before_candidate_creation() {
    let fixture = Fixture::new("corrupt-stage");
    let state = CacheStateId::parse("state-corrupt-stage").unwrap();
    let stage_path = fixture.receipt_root.join(stage_name(&state));
    fs::write(&stage_path, b"not a canonical receipt").unwrap();
    fs::set_permissions(&stage_path, fs::Permissions::from_mode(0o600)).unwrap();
    let plan = touch_and_test_plan(
        &fixture,
        state.as_str(),
        "candidate-corrupt-stage",
        "artifact",
    );

    let error = produce_protected_cache_replacement_equivalence(&plan)
        .expect_err("corrupt stage must block");

    assert_eq!(
        error.kind(),
        ProtectedCacheReplacementProductionErrorKind::Persistence
    );
    assert!(
        !fixture
            .materialization_root
            .join("candidate-corrupt-stage")
            .exists()
    );
}

#[test]
fn nonprivate_existing_receipt_blocks_before_candidate_creation() {
    let fixture = Fixture::new("receipt-mode");
    let state = CacheStateId::parse("state-receipt-mode").unwrap();
    let receipt_path = fixture.receipt_root.join(receipt_name(&state));
    fs::write(&receipt_path, b"not trusted").unwrap();
    fs::set_permissions(&receipt_path, fs::Permissions::from_mode(0o644)).unwrap();
    let plan = touch_and_test_plan(
        &fixture,
        state.as_str(),
        "candidate-receipt-mode",
        "artifact",
    );

    let error = produce_protected_cache_replacement_equivalence(&plan)
        .expect_err("nonprivate receipt must block");

    assert_eq!(
        error.kind(),
        ProtectedCacheReplacementProductionErrorKind::Persistence
    );
    assert!(
        !fixture
            .materialization_root
            .join("candidate-receipt-mode")
            .exists()
    );
}

#[test]
fn materialized_identity_wall_clock_limit_fails_without_receipt() {
    let fixture = Fixture::new("identity-timeout");
    let mut plan = touch_and_test_plan(
        &fixture,
        "state-identity-timeout",
        "candidate-identity-timeout",
        "artifact",
    );
    plan.identity_timeout = Duration::from_nanos(1);

    let error = produce_protected_cache_replacement_equivalence(&plan)
        .expect_err("identity derivation must time out");

    assert_eq!(
        error.kind(),
        ProtectedCacheReplacementProductionErrorKind::ResourceLimit
    );
    assert!(
        !fixture
            .receipt_root
            .join(receipt_name(&plan.state_id))
            .exists()
    );
}
