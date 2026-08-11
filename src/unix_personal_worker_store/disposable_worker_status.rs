use super::*;

use crate::disposable_attempt_catalog::MAX_DISPOSABLE_ATTEMPT_CATALOG_DOCUMENT_BYTES;
use crate::disposable_worker_reconciler::DisposableAttemptPhase;
use crate::github_scale_set_inbox::MAX_GITHUB_SCALE_SET_INBOX_BYTES;

const STATUS_SCHEMA_VERSION: u8 = 1;
const MAX_STATUS_BLOCKERS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableWorkerOperationalState {
    Running,
    Held,
    Stopped,
    RecoveryRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DisposableWorkerAttemptStatus {
    attempt_id: String,
    phase: DisposableAttemptPhase,
    vm_bound: bool,
    runner_bound: bool,
    job_bound: bool,
}

impl DisposableWorkerAttemptStatus {
    #[must_use]
    pub fn attempt_id(&self) -> &str {
        &self.attempt_id
    }

    #[must_use]
    pub const fn phase(&self) -> DisposableAttemptPhase {
        self.phase
    }

    #[must_use]
    pub const fn vm_bound(&self) -> bool {
        self.vm_bound
    }

    #[must_use]
    pub const fn runner_bound(&self) -> bool {
        self.runner_bound
    }

    #[must_use]
    pub const fn job_bound(&self) -> bool {
        self.job_bound
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DisposableWorkerServiceStatus {
    schema_version: u8,
    state: DisposableWorkerOperationalState,
    controller_running: bool,
    admission_held: bool,
    catalog_revision: Option<u64>,
    inbox_revision: Option<u64>,
    pending_scale_set_reconciliation: bool,
    active_attempts: Vec<DisposableWorkerAttemptStatus>,
    retained_tombstones: usize,
    blockers: Vec<&'static str>,
}

impl DisposableWorkerServiceStatus {
    #[must_use]
    pub const fn state(&self) -> DisposableWorkerOperationalState {
        self.state
    }

    #[must_use]
    pub const fn controller_running(&self) -> bool {
        self.controller_running
    }

    #[must_use]
    pub const fn admission_held(&self) -> bool {
        self.admission_held
    }

    #[must_use]
    pub fn active_attempts(&self) -> &[DisposableWorkerAttemptStatus] {
        &self.active_attempts
    }

    #[must_use]
    pub fn blockers(&self) -> &[&'static str] {
        &self.blockers
    }

    #[must_use]
    pub const fn pending_scale_set_reconciliation(&self) -> bool {
        self.pending_scale_set_reconciliation
    }

    #[must_use]
    pub const fn catalog_revision(&self) -> Option<u64> {
        self.catalog_revision
    }

    #[must_use]
    pub const fn inbox_revision(&self) -> Option<u64> {
        self.inbox_revision
    }

    #[must_use]
    pub const fn retained_tombstones(&self) -> usize {
        self.retained_tombstones
    }
}

impl UnixPersonalWorkerStore {
    /// Inspect one exact durable disposable-worker snapshot without recovering or mutating it.
    ///
    /// The report is bounded and path-free. A staged publication becomes an explicit blocker
    /// instead of being recovered by an operator read.
    pub fn inspect_disposable_worker_service_status(
        &self,
    ) -> Result<DisposableWorkerServiceStatus, PersonalWorkerStoreError> {
        let _lock = self.acquire_read_lock()?;
        let admission_held = self.disposable_worker_admission_held_locked()?;
        let controller_running = self.disposable_worker_service_running_locked()?;
        let mut blockers = Vec::new();

        let catalog_staged = self
            .read_named_bytes_bounded(
                super::disposable_attempt_catalog::STAGED_CATALOG_DOCUMENT,
                MAX_DISPOSABLE_ATTEMPT_CATALOG_DOCUMENT_BYTES,
            )?
            .is_some();
        let inbox_staged = self
            .read_named_bytes_bounded(
                super::github_scale_set_inbox::STAGED_INBOX_DOCUMENT,
                MAX_GITHUB_SCALE_SET_INBOX_BYTES,
            )?
            .is_some();
        let template_staged = self
            .read_named_bytes_bounded(
                super::disposable_template_generation::STAGED_GENERATION_DOCUMENT,
                crate::disposable_template_generation::MAX_DISPOSABLE_TEMPLATE_GENERATION_BYTES,
            )?
            .is_some();
        if catalog_staged {
            push_blocker(&mut blockers, "catalog_recovery_required")?;
        }
        if inbox_staged {
            push_blocker(&mut blockers, "scale_set_inbox_recovery_required")?;
        }
        if template_staged {
            push_blocker(&mut blockers, "template_recovery_required")?;
        }

        let catalog = if catalog_staged {
            None
        } else {
            self.load_catalog_named(super::disposable_attempt_catalog::CATALOG_DOCUMENT)
                .map_err(|_| PersonalWorkerStoreError::corrupt_state())?
        };
        if catalog.is_none() && !catalog_staged {
            push_blocker(&mut blockers, "catalog_missing")?;
        }
        let inbox = if inbox_staged {
            None
        } else {
            self.load_scale_set_inbox_named(super::github_scale_set_inbox::INBOX_DOCUMENT)
                .map_err(|_| PersonalWorkerStoreError::corrupt_state())?
        };
        if inbox.is_none() && !inbox_staged {
            push_blocker(&mut blockers, "scale_set_inbox_missing")?;
        }
        if !controller_running {
            push_blocker(&mut blockers, "service_not_running")?;
        }

        let pending_scale_set_reconciliation = inbox.as_ref().is_some_and(
            crate::github_scale_set_inbox::ScaleSetInboxDocument::requires_reconciliation,
        );
        if pending_scale_set_reconciliation {
            push_blocker(&mut blockers, "scale_set_reconciliation_pending")?;
        }
        let active_attempts = catalog
            .as_ref()
            .map(|document| {
                document
                    .active()
                    .iter()
                    .map(|reservation| {
                        let attempt = reservation.attempt();
                        DisposableWorkerAttemptStatus {
                            attempt_id: attempt.attempt_id().as_str().to_owned(),
                            phase: attempt.phase(),
                            vm_bound: attempt.vm_identity().is_some(),
                            runner_bound: attempt.runner_id().is_some(),
                            job_bound: attempt.github_job_id().is_some(),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let recovery_required = catalog_staged || inbox_staged || template_staged;
        let state = if recovery_required {
            DisposableWorkerOperationalState::RecoveryRequired
        } else if admission_held {
            DisposableWorkerOperationalState::Held
        } else if controller_running {
            DisposableWorkerOperationalState::Running
        } else {
            DisposableWorkerOperationalState::Stopped
        };
        Ok(DisposableWorkerServiceStatus {
            schema_version: STATUS_SCHEMA_VERSION,
            state,
            controller_running,
            admission_held,
            catalog_revision: catalog.as_ref().map(|document| document.revision().get()),
            inbox_revision: inbox.as_ref().map(|document| document.revision().get()),
            pending_scale_set_reconciliation,
            active_attempts,
            retained_tombstones: catalog
                .as_ref()
                .map_or(0, |document| document.tombstones().len()),
            blockers,
        })
    }

    fn disposable_worker_service_running_locked(&self) -> Result<bool, PersonalWorkerStoreError> {
        inspect_directory(
            &self.directory,
            "personal worker store directory",
            Some(self.owner),
        )?;
        let lock = match fs::openat(
            &self.directory,
            DISPOSABLE_WORKER_SERVICE_LOCK_FILE,
            EXISTING_LOCK_FLAGS,
            Mode::empty(),
        ) {
            Ok(lock) => lock,
            Err(Errno::NOENT) => return Ok(false),
            Err(error) => return Err(map_lock_open_error(error)),
        };
        inspect_private_file(&lock, self.owner, "disposable worker service lock", Some(0))?;
        match fs::flock(&lock, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {
                fs::flock(&lock, FlockOperation::Unlock).map_err(|_| {
                    store_error(
                        PersonalWorkerStoreErrorKind::Io,
                        "could not release the disposable worker service inspection lock",
                    )
                })?;
                Ok(false)
            }
            Err(Errno::AGAIN) => Ok(true),
            Err(_) => Err(store_error(
                PersonalWorkerStoreErrorKind::Io,
                "could not inspect disposable worker service ownership",
            )),
        }
    }
}

fn push_blocker(
    blockers: &mut Vec<&'static str>,
    blocker: &'static str,
) -> Result<(), PersonalWorkerStoreError> {
    if blockers.len() >= MAX_STATUS_BLOCKERS {
        return Err(PersonalWorkerStoreError::corrupt_state());
    }
    blockers.push(blocker);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::disposable_attempt_catalog::DisposableAttemptCatalog;
    use crate::github_scale_set_bridge::ScaleSetBridgeIdentity;

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TempRoot(std::path::PathBuf);

    impl TempRoot {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "smolrunner-disposable-status-{}-{}",
                std::process::id(),
                NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).unwrap();
            Self(path)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn initialized_store(root: &TempRoot) -> UnixPersonalWorkerStore {
        let store = UnixPersonalWorkerStore::open_or_create_disposable_catalog(&root.0).unwrap();
        let mut catalog = DisposableAttemptCatalog::new(store);
        catalog.initialize().unwrap();
        let mut store = catalog.into_store();
        let source = ScaleSetBridgeIdentity::parse(&format!("sha256:{}", "7a".repeat(32))).unwrap();
        store.initialize_scale_set_inbox(&source).unwrap();
        store
    }

    #[test]
    fn status_is_bounded_typed_and_observes_hold_without_mutation() {
        let root = TempRoot::new();
        let mut store = initialized_store(&root);
        let stopped = store.inspect_disposable_worker_service_status().unwrap();
        assert_eq!(stopped.state(), DisposableWorkerOperationalState::Stopped);
        assert!(!stopped.controller_running());
        assert!(!stopped.admission_held());
        assert!(stopped.active_attempts().is_empty());
        assert_eq!(stopped.blockers(), ["service_not_running"]);

        store.set_disposable_worker_admission_hold(true).unwrap();
        let held = store.inspect_disposable_worker_service_status().unwrap();
        assert_eq!(held.state(), DisposableWorkerOperationalState::Held);
        assert!(held.admission_held());
        assert_eq!(held.blockers(), ["service_not_running"]);
        let json = serde_json::to_string(&held).unwrap();
        assert!(!json.contains(root.0.to_str().unwrap()));
        assert!(!json.contains("sha256:"));
    }

    #[test]
    fn staged_catalog_is_reported_without_recovery_or_cleanup() {
        let root = TempRoot::new();
        let store = initialized_store(&root);
        let directory = root.0.join(STORE_DIRECTORY);
        let stage =
            directory.join(super::super::disposable_attempt_catalog::STAGED_CATALOG_DOCUMENT);
        fs::copy(
            directory.join(super::super::disposable_attempt_catalog::CATALOG_DOCUMENT),
            &stage,
        )
        .unwrap();

        let status = store.inspect_disposable_worker_service_status().unwrap();
        assert_eq!(
            status.state(),
            DisposableWorkerOperationalState::RecoveryRequired
        );
        assert!(status.blockers().contains(&"catalog_recovery_required"));
        assert!(stage.exists());
    }

    #[test]
    fn held_service_lock_is_reported_as_running() {
        let root = TempRoot::new();
        let store = initialized_store(&root);
        let service_lock = store.acquire_disposable_worker_service_lock().unwrap();

        let status = store.inspect_disposable_worker_service_status().unwrap();
        assert_eq!(status.state(), DisposableWorkerOperationalState::Running);
        assert!(status.controller_running());
        assert!(status.blockers().is_empty());
        drop(service_lock);
    }
}
