use smolrunner::disposable_worker_reconciler::{
    CapacityClaimId, DisposableAttempt, DisposableAttemptId, DisposableAttemptPhase,
    DisposableVmId, DisposableWorkerAction, DisposableWorkerResources, GitHubJobConclusion,
    GitHubJobId, ScaleSetRunnerId,
};
use smolrunner::disposable_worker_store::{
    DisposableWorkerStoreDocument, DisposableWorkerStoreErrorKind,
    DisposableWorkerStoreMutationDisposition, decode_disposable_worker_store_document,
    encode_disposable_worker_store_document,
};
use smolrunner::execution_admission::EpochMillis;

fn time(value: u64) -> EpochMillis {
    EpochMillis::new(value).unwrap()
}

fn attempt(suffix: usize) -> DisposableAttempt {
    DisposableAttempt::reserved(
        DisposableAttemptId::parse(&format!("attempt-{suffix}")).unwrap(),
        CapacityClaimId::parse(&format!("claim-{suffix}")).unwrap(),
        DisposableVmId::parse(&format!("vm-{suffix}")).unwrap(),
        ScaleSetRunnerId::parse(&format!("runner-{suffix}")).unwrap(),
        DisposableWorkerResources::new(1_000, 2_000, 3_000).unwrap(),
        time(10_000),
    )
}

fn apply(
    document: &DisposableWorkerStoreDocument,
    attempt_id: &DisposableAttemptId,
    action: DisposableWorkerAction,
) -> DisposableWorkerStoreDocument {
    let mutation = document
        .checkpoint_attempt(attempt_id, &action)
        .expect("valid checkpoint");
    assert_eq!(
        mutation.disposition(),
        DisposableWorkerStoreMutationDisposition::Applied
    );
    mutation.document().clone()
}

#[test]
fn canonical_document_round_trips_and_rejects_unknown_or_alternate_bytes() {
    let initial = DisposableWorkerStoreDocument::new().unwrap();
    let mutation = initial.reserve_attempt(attempt(1)).unwrap();
    let document = mutation.document();
    let encoded = encode_disposable_worker_store_document(document).unwrap();

    assert_eq!(
        decode_disposable_worker_store_document(&encoded).unwrap(),
        *document
    );
    assert!(encoded.ends_with(b"\n"));

    let mut alternate = encoded.clone();
    alternate.pop();
    assert_eq!(
        decode_disposable_worker_store_document(&alternate)
            .unwrap_err()
            .kind(),
        DisposableWorkerStoreErrorKind::CorruptState
    );

    let mut value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    value["unknown"] = serde_json::json!(true);
    let mut unknown = serde_json::to_vec_pretty(&value).unwrap();
    unknown.push(b'\n');
    assert_eq!(
        decode_disposable_worker_store_document(&unknown)
            .unwrap_err()
            .kind(),
        DisposableWorkerStoreErrorKind::CorruptState
    );

    value.as_object_mut().unwrap().remove("unknown");
    value["schema_version"] = serde_json::json!(2);
    let mut future = serde_json::to_vec_pretty(&value).unwrap();
    future.push(b'\n');
    assert_eq!(
        decode_disposable_worker_store_document(&future)
            .unwrap_err()
            .kind(),
        DisposableWorkerStoreErrorKind::VersionIncompatible
    );
}

#[test]
fn reservation_is_replay_safe_and_all_exact_identities_are_unique() {
    let initial = DisposableWorkerStoreDocument::new().unwrap();
    let first = attempt(1);
    let reserved = initial.reserve_attempt(first.clone()).unwrap();
    assert_eq!(reserved.document().revision().get(), 2);

    let replay = reserved.document().reserve_attempt(first).unwrap();
    assert_eq!(
        replay.disposition(),
        DisposableWorkerStoreMutationDisposition::Duplicate
    );
    assert_eq!(replay.document().revision().get(), 2);

    let conflicting = DisposableAttempt::reserved(
        DisposableAttemptId::parse("attempt-2").unwrap(),
        CapacityClaimId::parse("claim-1").unwrap(),
        DisposableVmId::parse("vm-2").unwrap(),
        ScaleSetRunnerId::parse("runner-2").unwrap(),
        DisposableWorkerResources::new(1_000, 2_000, 3_000).unwrap(),
        time(10_000),
    );
    assert_eq!(
        reserved
            .document()
            .reserve_attempt(conflicting)
            .unwrap_err()
            .kind(),
        DisposableWorkerStoreErrorKind::Conflict
    );

    let usage = reserved.document().host_usage().unwrap();
    let plan = smolrunner::disposable_worker_reconciler::plan_capacity(
        time(1_000),
        smolrunner::disposable_worker_reconciler::ScaleSetDemand::new(2, 0, time(900), time(1_100))
            .unwrap(),
        smolrunner::disposable_worker_reconciler::DisposableHostBudget::new(
            2,
            DisposableWorkerResources::new(2_000, 4_000, 6_000).unwrap(),
        )
        .unwrap(),
        usage,
        DisposableWorkerResources::new(1_000, 2_000, 3_000).unwrap(),
    )
    .unwrap();
    assert_eq!(plan.additional_workers(), 1);
}

#[test]
fn every_lifecycle_checkpoint_and_capacity_release_is_durable_and_monotonic() {
    let initial = DisposableWorkerStoreDocument::new().unwrap();
    let mut document = initial
        .reserve_attempt(attempt(1))
        .unwrap()
        .document()
        .clone();
    let attempt_id = DisposableAttemptId::parse("attempt-1").unwrap();
    let job_id = GitHubJobId::new(42).unwrap();
    let actions = [
        DisposableWorkerAction::Checkpoint {
            phase: DisposableAttemptPhase::Provisioning,
        },
        DisposableWorkerAction::Checkpoint {
            phase: DisposableAttemptPhase::Registering,
        },
        DisposableWorkerAction::Checkpoint {
            phase: DisposableAttemptPhase::Waiting,
        },
        DisposableWorkerAction::RecordAssigned {
            github_job_id: job_id,
        },
        DisposableWorkerAction::RecordRunning {
            github_job_id: job_id,
        },
        DisposableWorkerAction::RecordTerminal {
            github_job_id: job_id,
            conclusion: GitHubJobConclusion::Success,
        },
        DisposableWorkerAction::Checkpoint {
            phase: DisposableAttemptPhase::Destroying,
        },
        DisposableWorkerAction::Checkpoint {
            phase: DisposableAttemptPhase::Deregistering,
        },
        DisposableWorkerAction::Checkpoint {
            phase: DisposableAttemptPhase::Releasing,
        },
        DisposableWorkerAction::Checkpoint {
            phase: DisposableAttemptPhase::Complete,
        },
    ];
    for action in actions {
        let previous = document.clone();
        document = apply(&document, &attempt_id, action);
        document.validate_successor_of(&previous).unwrap();
        let bytes = encode_disposable_worker_store_document(&document).unwrap();
        document = decode_disposable_worker_store_document(&bytes).unwrap();
    }

    assert_eq!(
        document.attempts()[0].phase(),
        DisposableAttemptPhase::Complete
    );
    assert!(!document.attempts()[0].capacity_reserved());
    assert_eq!(
        document.host_usage().unwrap(),
        smolrunner::disposable_worker_reconciler::DisposableHostUsage::zero()
    );

    let pruned = document.prune_complete_attempt(&attempt_id).unwrap();
    assert!(pruned.document().attempts().is_empty());
    assert_eq!(
        pruned.document().completed_attempt_ids(),
        std::slice::from_ref(&attempt_id)
    );
    let prune_replay = pruned
        .document()
        .prune_complete_attempt(&attempt_id)
        .unwrap();
    assert_eq!(
        prune_replay.disposition(),
        DisposableWorkerStoreMutationDisposition::Duplicate
    );
    let reservation_replay = pruned.document().reserve_attempt(attempt(1)).unwrap();
    assert_eq!(
        reservation_replay.disposition(),
        DisposableWorkerStoreMutationDisposition::Duplicate
    );
}

#[test]
fn malformed_phase_shapes_and_skipped_transitions_fail_closed() {
    let initial = DisposableWorkerStoreDocument::new().unwrap();
    let document = initial
        .reserve_attempt(attempt(1))
        .unwrap()
        .document()
        .clone();
    let encoded = encode_disposable_worker_store_document(&document).unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    value["attempts"][0]["phase"] = serde_json::json!("complete");
    let mut forged = serde_json::to_vec_pretty(&value).unwrap();
    forged.push(b'\n');
    assert_eq!(
        decode_disposable_worker_store_document(&forged)
            .unwrap_err()
            .kind(),
        DisposableWorkerStoreErrorKind::CorruptState
    );

    assert_eq!(
        document
            .checkpoint_attempt(
                &DisposableAttemptId::parse("attempt-1").unwrap(),
                &DisposableWorkerAction::Checkpoint {
                    phase: DisposableAttemptPhase::Running,
                },
            )
            .unwrap_err()
            .kind(),
        DisposableWorkerStoreErrorKind::InvalidTransition
    );
    assert_eq!(
        document
            .prune_complete_attempt(&DisposableAttemptId::parse("attempt-1").unwrap())
            .unwrap_err()
            .kind(),
        DisposableWorkerStoreErrorKind::InvalidTransition
    );
}

#[cfg(unix)]
mod unix_persistence {
    use std::fs::{self, File, OpenOptions};
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;

    use rustix::fs::{FlockOperation, flock};
    use smolrunner::disposable_worker_store::{
        DisposableWorkerStore, DisposableWorkerStoreErrorKind, DisposableWorkerStoreMutationIntent,
        DisposableWorkerStoreRecoveryDisposition, DisposableWorkerStoreWriteDisposition,
        apply_disposable_worker_store_mutation, encode_disposable_worker_store_document,
    };
    use smolrunner::unix_personal_worker_store::UnixPersonalWorkerStore;

    use super::*;

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(label: &str) -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "smolrunner-disposable-worker-store-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create temporary root");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o750))
                .expect("set private root mode");
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

    fn hold_writer_lock(store_directory: &Path) -> File {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(store_directory.join("store.lock"))
            .expect("open durable writer lock");
        flock(&lock, FlockOperation::NonBlockingLockExclusive).expect("hold writer lock");
        lock
    }

    fn write_private(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).expect("write staged fixture");
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("set staged mode");
    }

    #[test]
    fn unix_store_creates_replaces_and_enforces_lock_and_revision_cas() {
        let root = TempRoot::new("cas");
        let (mut store, recovery) =
            UnixPersonalWorkerStore::open_or_create_disposable(root.path()).unwrap();
        assert_eq!(
            recovery.disposition(),
            DisposableWorkerStoreRecoveryDisposition::Clean
        );
        assert_eq!(recovery.revision(), None);

        let initial = DisposableWorkerStoreDocument::new().unwrap();
        let lock = hold_writer_lock(&root.store_directory());
        assert_eq!(
            store.create(&initial).unwrap_err().kind(),
            DisposableWorkerStoreErrorKind::Busy
        );
        drop(lock);

        let created = store.create(&initial).unwrap();
        assert_eq!(
            created.disposition(),
            DisposableWorkerStoreWriteDisposition::Created
        );
        assert_eq!(store.load().unwrap(), Some(initial.clone()));

        let first_attempt = attempt(1);
        let replaced = apply_disposable_worker_store_mutation(
            &mut store,
            initial.revision(),
            DisposableWorkerStoreMutationIntent::ReserveAttempt {
                attempt: first_attempt.clone(),
            },
        )
        .unwrap();
        assert_eq!(
            replaced.disposition(),
            DisposableWorkerStoreMutationDisposition::Applied
        );
        let successor = store.load().unwrap().unwrap();
        assert_eq!(store.load().unwrap(), Some(successor.clone()));

        let replay = apply_disposable_worker_store_mutation(
            &mut store,
            successor.revision(),
            DisposableWorkerStoreMutationIntent::ReserveAttempt {
                attempt: first_attempt,
            },
        )
        .unwrap();
        assert_eq!(
            replay.disposition(),
            DisposableWorkerStoreMutationDisposition::Duplicate
        );
        assert_eq!(
            apply_disposable_worker_store_mutation(
                &mut store,
                initial.revision(),
                DisposableWorkerStoreMutationIntent::PruneCompleteAttempt {
                    attempt_id: DisposableAttemptId::parse("attempt-1").unwrap(),
                },
            )
            .unwrap_err()
            .kind(),
            DisposableWorkerStoreErrorKind::RevisionConflict
        );
    }

    #[test]
    fn restart_publishes_an_exact_successor_stage_and_removes_a_stale_stage() {
        let root = TempRoot::new("recovery");
        let (mut store, _) =
            UnixPersonalWorkerStore::open_or_create_disposable(root.path()).unwrap();
        let initial = DisposableWorkerStoreDocument::new().unwrap();
        store.create(&initial).unwrap();
        let successor = initial
            .reserve_attempt(attempt(1))
            .unwrap()
            .document()
            .clone();
        write_private(
            &root.store_directory().join(".disposable-next.json"),
            &encode_disposable_worker_store_document(&successor).unwrap(),
        );

        let published = store.recover().unwrap();
        assert_eq!(
            published.disposition(),
            DisposableWorkerStoreRecoveryDisposition::PublishedStaged
        );
        assert_eq!(store.load().unwrap(), Some(successor.clone()));

        write_private(
            &root.store_directory().join(".disposable-next.json"),
            &encode_disposable_worker_store_document(&initial).unwrap(),
        );
        let removed = store.recover().unwrap();
        assert_eq!(
            removed.disposition(),
            DisposableWorkerStoreRecoveryDisposition::RemovedStaleStaged
        );
        assert_eq!(store.load().unwrap(), Some(successor));
        assert!(
            !root
                .store_directory()
                .join(".disposable-next.json")
                .exists()
        );
    }

    #[test]
    fn existing_authority_without_lock_and_noncanonical_stages_fail_closed() {
        let root = TempRoot::new("unsafe");
        let directory = root.store_directory();
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o750)).unwrap();
        assert_eq!(
            UnixPersonalWorkerStore::open_or_create_disposable(root.path())
                .unwrap_err()
                .kind(),
            DisposableWorkerStoreErrorKind::UnsafeFilesystem
        );

        let root = TempRoot::new("corrupt-stage");
        let (mut store, _) =
            UnixPersonalWorkerStore::open_or_create_disposable(root.path()).unwrap();
        store
            .create(&DisposableWorkerStoreDocument::new().unwrap())
            .unwrap();
        write_private(
            &root.store_directory().join(".disposable-next.json"),
            b"not canonical JSON\n",
        );
        assert_eq!(
            store.recover().unwrap_err().kind(),
            DisposableWorkerStoreErrorKind::CorruptState
        );
        assert!(
            root.store_directory()
                .join(".disposable-next.json")
                .exists()
        );
    }

    #[test]
    fn concurrent_reservations_cannot_both_publish_from_one_revision() {
        let root = TempRoot::new("concurrent-cas");
        let (mut initializer, _) =
            UnixPersonalWorkerStore::open_or_create_disposable(root.path()).unwrap();
        let initial = DisposableWorkerStoreDocument::new().unwrap();
        initializer.create(&initial).unwrap();
        let (first_store, _) =
            UnixPersonalWorkerStore::open_or_create_disposable(root.path()).unwrap();
        let (second_store, _) =
            UnixPersonalWorkerStore::open_or_create_disposable(root.path()).unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let initial_revision = initial.revision();

        let run = |mut store: UnixPersonalWorkerStore,
                   candidate: DisposableAttempt,
                   barrier: Arc<Barrier>| {
            thread::spawn(move || {
                barrier.wait();
                apply_disposable_worker_store_mutation(
                    &mut store,
                    initial_revision,
                    DisposableWorkerStoreMutationIntent::ReserveAttempt { attempt: candidate },
                )
            })
        };
        let first = run(first_store, attempt(1), Arc::clone(&barrier));
        let second = run(second_store, attempt(2), Arc::clone(&barrier));
        barrier.wait();
        let outcomes = [first.join().unwrap(), second.join().unwrap()];
        assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
        assert!(
            outcomes
                .iter()
                .filter_map(|outcome| outcome.as_ref().err())
                .all(|error| matches!(
                    error.kind(),
                    DisposableWorkerStoreErrorKind::Busy
                        | DisposableWorkerStoreErrorKind::RevisionConflict
                ))
        );

        let (store, _) = UnixPersonalWorkerStore::open_or_create_disposable(root.path()).unwrap();
        assert_eq!(store.load().unwrap().unwrap().attempts().len(), 1);
    }
}
