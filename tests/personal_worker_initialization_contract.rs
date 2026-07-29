#![cfg(unix)]

use std::fs::{self, OpenOptions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rustix::fs::{FlockOperation, flock};
use smolrunner::execution_admission::EpochMillis;
use smolrunner::personal_worker_queue::{
    PersonalWorkerProfile, PersonalWorkerQueueGeneration, PersonalWorkerQueueInput,
};
use smolrunner::personal_worker_store::{
    PersonalWorkerStore, PersonalWorkerStoreDocument, PersonalWorkerStoreErrorKind,
    PersonalWorkerStoreInitializationDisposition, decode_personal_worker_store_document,
    encode_personal_worker_store_document,
};
use smolrunner::unix_personal_worker_store::UnixPersonalWorkerStore;

static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "smolrunner-personal-worker-initialization-{label}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create temporary state root");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).expect("set state root mode");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn store_directory(&self) -> PathBuf {
        self.0.join("personal-worker")
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

fn queue(generation: u64, observed_at: u64) -> PersonalWorkerQueueInput {
    PersonalWorkerQueueInput {
        generation: PersonalWorkerQueueGeneration::new(generation).expect("queue generation"),
        observed_at: time(observed_at),
        current_profile: PersonalWorkerProfile::Interactive,
        last_activity_at: time(observed_at),
        queued: vec![],
        active: vec![],
        pending_profile_change: None,
    }
}

fn initial_document(observed_at: u64) -> PersonalWorkerStoreDocument {
    PersonalWorkerStoreDocument::new(queue(1, observed_at), vec![]).expect("initial document")
}

fn write_private(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).expect("write private fixture");
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("set private fixture mode");
}

#[test]
fn top_level_store_version_is_distinct_from_corruption() {
    let encoded = encode_personal_worker_store_document(&initial_document(1_000_000))
        .expect("encode initial document");
    let mut incompatible: serde_json::Value =
        serde_json::from_slice(&encoded).expect("parse document");
    incompatible["schema_version"] = serde_json::Value::from(2_u64);
    let incompatible_bytes =
        serde_json::to_vec(&incompatible).expect("encode incompatible document");
    assert_eq!(
        decode_personal_worker_store_document(&incompatible_bytes)
            .expect_err("unsupported top-level version")
            .kind(),
        PersonalWorkerStoreErrorKind::VersionIncompatible
    );
    assert_eq!(
        decode_personal_worker_store_document(b"not-json")
            .expect_err("malformed state")
            .kind(),
        PersonalWorkerStoreErrorKind::CorruptState
    );
}

#[test]
fn initialize_if_clean_creates_once_and_replays_without_byte_changes() {
    let root = TempRoot::new("create-replay");
    let initial = initial_document(2_000_000);
    let created = UnixPersonalWorkerStore::initialize_if_clean(root.path(), &initial)
        .expect("create initial state");
    assert_eq!(
        created.disposition(),
        PersonalWorkerStoreInitializationDisposition::Created
    );
    assert_eq!(created.revision(), Some(initial.revision()));
    assert!(created.bytes_written() > 0);

    let current = root.store_directory().join("current.json");
    let before = fs::read(&current).expect("read current bytes");
    let replay = UnixPersonalWorkerStore::initialize_if_clean(root.path(), &initial)
        .expect("replay initialisation");
    assert_eq!(
        replay.disposition(),
        PersonalWorkerStoreInitializationDisposition::AlreadyExists
    );
    assert_eq!(replay.revision(), Some(initial.revision()));
    assert_eq!(replay.bytes_written(), 0);
    assert_eq!(fs::read(&current).expect("read replay bytes"), before);

    let reopened = UnixPersonalWorkerStore::open_existing_read_only(root.path())
        .expect("open initialised store");
    assert_eq!(reopened.load().expect("load state"), Some(initial));
}

#[test]
fn initialize_if_clean_refuses_every_valid_recovery_shape_without_mutation() {
    let initial_only = TempRoot::new("staged-initial");
    let initial = initial_document(3_000_000);
    UnixPersonalWorkerStore::initialize_if_clean(initial_only.path(), &initial)
        .expect("create initial state");
    let initial_current = initial_only.store_directory().join("current.json");
    let initial_stage = initial_only.store_directory().join(".next.json");
    fs::rename(&initial_current, &initial_stage).expect("stage initial document");
    let initial_stage_bytes = fs::read(&initial_stage).expect("read initial stage");
    let receipt = UnixPersonalWorkerStore::initialize_if_clean(initial_only.path(), &initial)
        .expect("inspect staged initial");
    assert_eq!(
        receipt.disposition(),
        PersonalWorkerStoreInitializationDisposition::RecoveryRequired
    );
    assert_eq!(receipt.revision(), Some(initial.revision()));
    assert!(!initial_current.exists());
    assert_eq!(
        fs::read(&initial_stage).expect("re-read initial stage"),
        initial_stage_bytes
    );

    let successor_root = TempRoot::new("staged-successor");
    let current = initial_document(4_000_000);
    UnixPersonalWorkerStore::initialize_if_clean(successor_root.path(), &current)
        .expect("create current state");
    let successor = current
        .advance(queue(2, 4_000_001), vec![])
        .expect("successor document");
    let current_path = successor_root.store_directory().join("current.json");
    let stage_path = successor_root.store_directory().join(".next.json");
    let current_bytes = fs::read(&current_path).expect("read current bytes");
    let successor_bytes =
        encode_personal_worker_store_document(&successor).expect("encode successor");
    write_private(&stage_path, &successor_bytes);
    let receipt = UnixPersonalWorkerStore::initialize_if_clean(successor_root.path(), &current)
        .expect("inspect staged successor");
    assert_eq!(
        receipt.disposition(),
        PersonalWorkerStoreInitializationDisposition::RecoveryRequired
    );
    assert_eq!(receipt.revision(), Some(successor.revision()));
    assert_eq!(
        fs::read(&current_path).expect("re-read current"),
        current_bytes
    );
    assert_eq!(
        fs::read(&stage_path).expect("re-read successor"),
        successor_bytes
    );

    let stale_root = TempRoot::new("stale-stage");
    let stale = initial_document(5_000_000);
    UnixPersonalWorkerStore::initialize_if_clean(stale_root.path(), &stale)
        .expect("create stale current");
    let stale_current = stale_root.store_directory().join("current.json");
    let stale_stage = stale_root.store_directory().join(".next.json");
    let stale_bytes = fs::read(&stale_current).expect("read stale current");
    write_private(&stale_stage, &stale_bytes);
    let receipt = UnixPersonalWorkerStore::initialize_if_clean(stale_root.path(), &stale)
        .expect("inspect stale stage");
    assert_eq!(
        receipt.disposition(),
        PersonalWorkerStoreInitializationDisposition::RecoveryRequired
    );
    assert_eq!(receipt.revision(), Some(stale.revision()));
    assert_eq!(
        fs::read(&stale_current).expect("re-read stale current"),
        stale_bytes
    );
    assert_eq!(
        fs::read(&stale_stage).expect("re-read stale stage"),
        stale_bytes
    );
}

#[test]
fn initialize_if_clean_preserves_corrupt_stage_and_reports_busy_without_blocking() {
    let root = TempRoot::new("corrupt-stage");
    let initial = initial_document(6_000_000);
    UnixPersonalWorkerStore::initialize_if_clean(root.path(), &initial)
        .expect("create current state");
    let current_path = root.store_directory().join("current.json");
    let stage_path = root.store_directory().join(".next.json");
    let current_bytes = fs::read(&current_path).expect("read current bytes");
    write_private(&stage_path, b"not-json");
    let stage_bytes = fs::read(&stage_path).expect("read corrupt stage");
    let error = UnixPersonalWorkerStore::initialize_if_clean(root.path(), &initial)
        .expect_err("corrupt stage must fail closed");
    assert_eq!(error.kind(), PersonalWorkerStoreErrorKind::CorruptState);
    assert_eq!(
        fs::read(&current_path).expect("re-read current"),
        current_bytes
    );
    assert_eq!(
        fs::read(&stage_path).expect("re-read corrupt stage"),
        stage_bytes
    );

    fs::remove_file(&stage_path).expect("remove test stage");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.store_directory().join("store.lock"))
        .expect("open lock file");
    flock(&lock, FlockOperation::NonBlockingLockExclusive).expect("hold writer lock");
    let error = UnixPersonalWorkerStore::initialize_if_clean(root.path(), &initial)
        .expect_err("busy store must return immediately");
    assert_eq!(error.kind(), PersonalWorkerStoreErrorKind::Busy);
    assert_eq!(
        fs::read(&current_path).expect("read busy current"),
        current_bytes
    );
}
