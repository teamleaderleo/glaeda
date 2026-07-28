#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use smolrunner::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use smolrunner::execution_admission::{
    EpochMillis, ExecutionAdmissionIdentity, ExecutionRequestId, ExecutionResourceLimits,
    FallbackProfileEligibility, RunnerProfileId,
};
use smolrunner::personal_worker_queue::{
    MAX_PERSONAL_WORKER_QUEUE_ENTRIES, PersonalWorkerCacheAccessMode,
    PersonalWorkerCacheNamespace, PersonalWorkerCancellationState, PersonalWorkerJobRequest,
    PersonalWorkerPriority, PersonalWorkerProfile, PersonalWorkerQueueGeneration,
    PersonalWorkerQueueInput, PersonalWorkerSourceIdentity,
};
use smolrunner::personal_worker_store::{
    PersonalWorkerStore, PersonalWorkerStoreDocument,
};
use smolrunner::unix_personal_worker_store::UnixPersonalWorkerStore;
use smolrunner::verification_profile::{CacheId, VerificationProfileId};

const GIB: u64 = 1_024 * 1_024 * 1_024;
const BASE: u64 = 20_000_000;
static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "smolrunner-personal-worker-submit-capacity-{label}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create temporary state root");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).expect("set root mode");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn current_document(&self) -> PathBuf {
        self.0.join("personal-worker/current.json")
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn time(value: u64) -> EpochMillis {
    EpochMillis::new(value).expect("time")
}

fn request(id: &str) -> PersonalWorkerJobRequest {
    let repository = RepositoryRef::parse("example/project").expect("repository");
    PersonalWorkerJobRequest {
        identity: ExecutionAdmissionIdentity::new(
            ExecutionRequestId::parse(id).expect("request ID"),
            VerificationProfileId::parse("smolrunner.required").expect("verification profile"),
            RunnerProfileId::parse("personal-lima-work").expect("runner profile"),
        ),
        source: PersonalWorkerSourceIdentity::new(
            repository.clone(),
            CommitId::parse(&"a".repeat(40)).expect("commit"),
            GitTreeId::parse(&"b".repeat(40)).expect("tree"),
        ),
        priority: PersonalWorkerPriority::Normal,
        requested_limits: ExecutionResourceLimits::new(2_000, 2 * GIB, 2_048)
            .expect("resource limits"),
        cache_namespace: PersonalWorkerCacheNamespace::RepositoryBuild {
            cache_id: CacheId::parse("build-cache").expect("cache ID"),
            repository,
            namespace_digest: Sha256Digest::parse(&format!("sha256:{}", "ab".repeat(32)))
                .expect("namespace digest"),
        },
        cache_access: PersonalWorkerCacheAccessMode::Write,
        submitted_at: time(BASE - 1_000),
        operator_deadline: Some(time(BASE + 60_000)),
        cancellation: PersonalWorkerCancellationState::Active,
        fallback_eligibility: FallbackProfileEligibility::ineligible(),
    }
}

fn full_queue_document() -> PersonalWorkerStoreDocument {
    let queued = (0..MAX_PERSONAL_WORKER_QUEUE_ENTRIES)
        .map(|index| request(&format!("capacity-{index:03}")))
        .collect();
    PersonalWorkerStoreDocument::new(
        PersonalWorkerQueueInput {
            generation: PersonalWorkerQueueGeneration::new(1).expect("queue generation"),
            observed_at: time(BASE),
            current_profile: PersonalWorkerProfile::Interactive,
            last_activity_at: time(BASE - 1_000),
            queued,
            active: vec![],
            pending_profile_change: None,
        },
        vec![],
    )
    .expect("full queue document")
}

fn create_store(root: &TempRoot, document: &PersonalWorkerStoreDocument) {
    let (mut store, _) = UnixPersonalWorkerStore::open_or_create(root.path()).expect("open store");
    store.create(document).expect("create durable document");
}

fn submit_command(root: &TempRoot, output: &str, request_id: &str) -> Vec<OsString> {
    vec![
        OsString::from("--output"),
        OsString::from(output),
        OsString::from("queue"),
        OsString::from("submit"),
        OsString::from("--store-root"),
        root.path().as_os_str().to_owned(),
        OsString::from("--revision"),
        OsString::from("1"),
        OsString::from("--generation"),
        OsString::from("1"),
        OsString::from("--observed-at"),
        OsString::from((BASE + 1_000).to_string()),
        OsString::from("--request-id"),
        OsString::from(request_id),
        OsString::from("--verification-profile"),
        OsString::from("smolrunner.required"),
        OsString::from("--runner-profile"),
        OsString::from("personal-lima-work"),
        OsString::from("--repository"),
        OsString::from("example/project"),
        OsString::from("--commit"),
        OsString::from("a".repeat(40)),
        OsString::from("--tree"),
        OsString::from("b".repeat(40)),
        OsString::from("--priority"),
        OsString::from("normal"),
        OsString::from("--cpu-millis"),
        OsString::from("2000"),
        OsString::from("--memory-bytes"),
        OsString::from((2 * GIB).to_string()),
        OsString::from("--pids"),
        OsString::from("2048"),
        OsString::from("--cache-id"),
        OsString::from("build-cache"),
        OsString::from("--cache-namespace-digest"),
        OsString::from(format!("sha256:{}", "ab".repeat(32))),
        OsString::from("--cache-access"),
        OsString::from("write"),
        OsString::from("--submitted-at"),
        OsString::from((BASE - 1_000).to_string()),
        OsString::from("--operator-deadline"),
        OsString::from((BASE + 60_000).to_string()),
    ]
}

fn run_smolrunner(arguments: &[OsString]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_smolrunner"))
        .args(arguments)
        .output()
        .expect("run installed smolrunner binary")
}

fn public_output(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn full_queue_refuses_another_submission_without_publication_or_private_output() {
    let root = TempRoot::new("private-path-sentinel");
    create_store(&root, &full_queue_document());
    let before = fs::read(root.current_document()).expect("read current document");
    let request_id = "private-capacity-request-sentinel";

    let json = run_smolrunner(&submit_command(&root, "json", request_id));
    assert!(!json.status.success());
    let json_public = public_output(&json);
    let parsed: serde_json::Value =
        serde_json::from_slice(&json.stdout).expect("bounded JSON error envelope");
    assert_eq!(parsed["kind"], "invalid_mutation");
    assert_eq!(fs::read(root.current_document()).expect("read current document"), before);

    let human = run_smolrunner(&submit_command(&root, "human", request_id));
    assert!(!human.status.success());
    let human_public = public_output(&human);
    assert_eq!(fs::read(root.current_document()).expect("read current document"), before);

    for forbidden in [
        request_id,
        "example/project",
        "build-cache",
        root.path().to_string_lossy().as_ref(),
    ] {
        assert!(!json_public.contains(forbidden));
        assert!(!human_public.contains(forbidden));
    }
}
