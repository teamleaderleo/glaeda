//! Private production composition for one enrolled disposable Scale Set worker.
//!
//! Durable documents are recovered before the verified bridge process starts and before the
//! GitHub App key leaves Keychain. Source-template maintenance then shares the supervised loop but
//! never gates recovery or teardown of an existing worker. `launchd` remains the outer process
//! supervisor; this module only composes the already-reviewed internal boundaries.

#![allow(dead_code)]

use std::fmt;
use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::disposable_attempt_catalog::DisposableAttemptCatalog;
use crate::disposable_clone_runtime::CloneRuntimeClock;
use crate::disposable_worker_enrollment::{
    DisposableWorkerEnrollment, DisposableWorkerEnrollmentParts,
};
use crate::execution_admission::EpochMillis;
use crate::github_scale_set_bridge::ScaleSetBridgeClient;
use crate::github_scale_set_service::{
    PreparedScaleSetService, ScaleSetService, SystemScaleSetServiceClock,
};
use crate::github_scale_set_supervisor::{
    ScaleSetSupervisor, ScaleSetSupervisorEvent, ScaleSetSupervisorEventSink,
    ScaleSetSupervisorPolicy, ThreadScaleSetSupervisorWait,
};
use crate::lima_observation::LimaObservationClock;
use crate::personal_worker_store::PersonalWorkerStoreErrorKind;
use crate::process::ProcessExecutor;
use crate::unix_personal_worker_store::DisposableWorkerServiceLock;
use crate::unix_personal_worker_store::UnixPersonalWorkerStore;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposableWorkerServiceErrorKind {
    DurableState,
    Bridge,
    Supervisor,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DisposableWorkerServiceError {
    kind: DisposableWorkerServiceErrorKind,
    code: &'static str,
}

impl DisposableWorkerServiceError {
    const fn new(kind: DisposableWorkerServiceErrorKind, code: &'static str) -> Self {
        Self { kind, code }
    }

    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for DisposableWorkerServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableWorkerServiceError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .finish()
    }
}

impl fmt::Display for DisposableWorkerServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the disposable-worker service was refused")
    }
}

impl std::error::Error for DisposableWorkerServiceError {}

struct PreparedDisposableWorkerService {
    _service_lock: DisposableWorkerServiceLock,
    scale_set: PreparedScaleSetService,
    parts: DisposableWorkerEnrollmentParts,
}

fn prepare_durable_service(
    enrollment: DisposableWorkerEnrollment,
) -> Result<PreparedDisposableWorkerService, DisposableWorkerServiceError> {
    let parts = enrollment.into_parts();
    recover_disposable_documents(&parts.state_root)?;
    let store = UnixPersonalWorkerStore::open_or_create_disposable_catalog(&parts.state_root)
        .map_err(|_| {
            DisposableWorkerServiceError::new(
                DisposableWorkerServiceErrorKind::DurableState,
                "disposable_worker_store_unavailable",
            )
        })?;
    let service_lock = store
        .acquire_disposable_worker_service_lock()
        .map_err(|error| {
            DisposableWorkerServiceError::new(
                DisposableWorkerServiceErrorKind::DurableState,
                if error.kind() == PersonalWorkerStoreErrorKind::Busy {
                    "disposable_worker_service_busy"
                } else {
                    "disposable_worker_service_lock_unavailable"
                },
            )
        })?;
    store.inspect_disposable_worker_admission().map_err(|_| {
        DisposableWorkerServiceError::new(
            DisposableWorkerServiceErrorKind::DurableState,
            "disposable_worker_admission_state_unavailable",
        )
    })?;
    let mut catalog = DisposableAttemptCatalog::new(store);
    catalog.initialize().map_err(|_| {
        DisposableWorkerServiceError::new(
            DisposableWorkerServiceErrorKind::DurableState,
            "disposable_worker_catalog_unavailable",
        )
    })?;
    let store = catalog.into_store();
    let scale_set = ScaleSetService::<ScaleSetBridgeClient, SystemScaleSetServiceClock>::prepare(
        store,
        parts.consumer_policy.clone(),
    )
    .map_err(|error| {
        DisposableWorkerServiceError::new(
            DisposableWorkerServiceErrorKind::DurableState,
            error.code(),
        )
    })?;
    Ok(PreparedDisposableWorkerService {
        _service_lock: service_lock,
        scale_set,
        parts,
    })
}

fn recover_disposable_documents(
    state_root: &std::path::Path,
) -> Result<(), DisposableWorkerServiceError> {
    // Each owner refuses another owner's unsettled stage. Try every owner in a bounded pass; one
    // successful recovery can unblock the others on the following pass. If no owner can advance,
    // preserve all evidence and fail closed rather than guessing which document is authoritative.
    let mut template_clean = false;
    let mut catalog_clean = false;
    let mut inbox_clean = false;
    for _ in 0..3 {
        let mut progressed = false;
        if !template_clean
            && UnixPersonalWorkerStore::open_or_create_disposable_template_generation(state_root)
                .is_ok()
        {
            template_clean = true;
            progressed = true;
        }
        if !catalog_clean
            && UnixPersonalWorkerStore::open_or_create_disposable_catalog(state_root).is_ok()
        {
            catalog_clean = true;
            progressed = true;
        }
        if !inbox_clean
            && UnixPersonalWorkerStore::open_or_recover_scale_set_inbox(state_root).is_ok()
        {
            inbox_clean = true;
            progressed = true;
        }
        if template_clean && catalog_clean && inbox_clean {
            return Ok(());
        }
        if !progressed {
            break;
        }
    }
    Err(DisposableWorkerServiceError::new(
        DisposableWorkerServiceErrorKind::DurableState,
        "disposable_worker_recovery_required",
    ))
}

#[cfg(target_os = "macos")]
pub fn serve_disposable_worker(
    enrollment: DisposableWorkerEnrollment,
) -> Result<(), DisposableWorkerServiceError> {
    // Complete all local durable recovery before acquiring or handing the App key to a child.
    let prepared = prepare_durable_service(enrollment)?;
    let executor = ProcessExecutor;
    let clock = SystemDisposableWorkerClock;
    let mut wait = ThreadScaleSetSupervisorWait;
    let mut events = DiscardSupervisorEvents;
    let mut supervisor = ScaleSetSupervisor::new(
        ScaleSetSupervisorPolicy::production(),
        &mut wait,
        &mut events,
    )
    .map_err(|error| {
        DisposableWorkerServiceError::new(
            DisposableWorkerServiceErrorKind::Supervisor,
            error.code(),
        )
    })?;

    // The bridge verifies its fixed executable and only now reads the App key from Keychain.
    let bridge = ScaleSetBridgeClient::connect_from_keychain(prepared.parts.bridge_config)
        .map_err(|error| {
            DisposableWorkerServiceError::new(
                DisposableWorkerServiceErrorKind::Bridge,
                error.code(),
            )
        })?;
    let mut service = ScaleSetService::start(prepared.scale_set, bridge).map_err(|error| {
        DisposableWorkerServiceError::new(DisposableWorkerServiceErrorKind::Bridge, error.code())
    })?;
    supervisor
        .serve(
            &mut service,
            &prepared.parts.template_runtime,
            &prepared.parts.clone_runtime,
            &prepared.parts.runner_runtime,
            &executor,
            &clock,
        )
        .map(|_exit| ())
        .map_err(|error| {
            DisposableWorkerServiceError::new(
                DisposableWorkerServiceErrorKind::Supervisor,
                error.code(),
            )
        })
}

#[derive(Debug, Clone, Copy, Default)]
struct SystemDisposableWorkerClock;

impl LimaObservationClock for SystemDisposableWorkerClock {
    fn unix_seconds(&self) -> io::Result<u64> {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .map_err(|_| io::Error::other("system clock precedes the Unix epoch"))
    }
}

impl CloneRuntimeClock for SystemDisposableWorkerClock {
    fn epoch_millis(&self) -> io::Result<EpochMillis> {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| io::Error::other("system clock precedes the Unix epoch"))?
            .as_millis();
        let millis = u64::try_from(millis)
            .map_err(|_| io::Error::other("system clock exceeds the supported range"))?;
        EpochMillis::new(millis)
            .map_err(|_| io::Error::other("system clock is outside the supported range"))
    }
}

struct DiscardSupervisorEvents;

impl ScaleSetSupervisorEventSink for DiscardSupervisorEvents {
    fn record(&mut self, _event: &ScaleSetSupervisorEvent) {}
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::disposable_worker_enrollment::decode_disposable_worker_enrollment;

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TempRoot(std::path::PathBuf);

    impl TempRoot {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "smolrunner-disposable-service-{}-{}",
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

    fn enrollment(root: &std::path::Path) -> DisposableWorkerEnrollment {
        let path = serde_json::to_string(root.to_str().unwrap()).unwrap();
        let document = format!(
            concat!(
                "{{\n",
                "  \"schema_version\": 1,\n",
                "  \"state_root\": {path},\n",
                "  \"lima\": {{\n",
                "    \"program\": \"/opt/homebrew/bin/limactl\",\n",
                "    \"home\": \"/private/var/lib/smolrunner/lima\",\n",
                "    \"source_instance\": \"smolrunner-prepared-template\"\n",
                "  }},\n",
                "  \"bridge\": {{\n",
                "    \"program_digest\": \"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n",
                "  }},\n",
                "  \"github\": {{\n",
                "    \"config_url\": \"https://github.com/acme\",\n",
                "    \"client_id\": \"Iv1.0123456789abcdef\",\n",
                "    \"installation_id\": 42,\n",
                "    \"keychain_service\": \"smolrunner.github-app\",\n",
                "    \"keychain_account\": \"acme-ci\"\n",
                "  }},\n",
                "  \"scale_set\": {{\n",
                "    \"id\": 17,\n",
                "    \"name\": \"smolrunner-disposable\",\n",
                "    \"runner_group_id\": 3,\n",
                "    \"owner\": \"acme\",\n",
                "    \"repository\": \"widgets\",\n",
                "    \"labels\": [\n",
                "      \"self-hosted\",\n",
                "      \"smolrunner\"\n",
                "    ]\n",
                "  }},\n",
                "  \"resources\": {{\n",
                "    \"cpu_millis\": 2000,\n",
                "    \"memory_bytes\": 2147483648,\n",
                "    \"disk_bytes\": 21474836480\n",
                "  }}\n",
                "}}\n"
            ),
            path = path
        );
        decode_disposable_worker_enrollment(document.as_bytes()).unwrap()
    }

    #[test]
    fn durable_control_is_prepared_without_keychain_or_process_authority() {
        let root = TempRoot::new();
        let prepared = prepare_durable_service(enrollment(&root.0)).unwrap();
        assert!(root.0.join("personal-worker/store.lock").is_file());
        assert!(
            root.0
                .join("personal-worker/disposable-attempt-catalog.json")
                .is_file()
        );
        assert!(
            root.0
                .join("personal-worker/github-scale-set-inbox.json")
                .is_file()
        );
        drop(prepared);
    }

    #[test]
    fn a_second_service_cannot_open_a_competing_bridge_session() {
        let root = TempRoot::new();
        let prepared = prepare_durable_service(enrollment(&root.0)).unwrap();

        let error = match prepare_durable_service(enrollment(&root.0)) {
            Ok(_) => panic!("competing service unexpectedly acquired controller ownership"),
            Err(error) => error,
        };
        assert_eq!(error.code(), "disposable_worker_service_busy");

        drop(prepared);
        drop(prepare_durable_service(enrollment(&root.0)).unwrap());
    }

    #[test]
    fn startup_recovers_each_single_document_stage_before_reinitialization() {
        let root = TempRoot::new();
        drop(prepare_durable_service(enrollment(&root.0)).unwrap());
        let directory = root.0.join("personal-worker");

        let cases = [
            (
                "disposable-attempt-catalog.json",
                ".disposable-attempt-catalog.next.json",
            ),
            (
                "github-scale-set-inbox.json",
                ".github-scale-set-inbox.next.json",
            ),
        ];
        for (current, stage) in cases {
            fs::copy(directory.join(current), directory.join(stage)).unwrap();
            drop(prepare_durable_service(enrollment(&root.0)).unwrap());
            assert!(!directory.join(stage).exists());
        }

        let parts = enrollment(&root.0).into_parts();
        let mut template_store =
            UnixPersonalWorkerStore::open_or_create_disposable_template_generation(&root.0)
                .unwrap();
        if template_store
            .load_disposable_template_generation()
            .unwrap()
            .is_none()
        {
            template_store
                .create_disposable_template_generation(&parts.template_runtime.initial_document())
                .unwrap();
        }
        drop(template_store);
        fs::copy(
            directory.join("disposable-template-generation.json"),
            directory.join(".disposable-template-generation.next.json"),
        )
        .unwrap();

        drop(prepare_durable_service(enrollment(&root.0)).unwrap());
        assert!(
            !directory
                .join(".disposable-template-generation.next.json")
                .exists()
        );
    }

    #[test]
    fn orphan_inbox_never_authorizes_fresh_catalog_history() {
        let root = TempRoot::new();
        drop(prepare_durable_service(enrollment(&root.0)).unwrap());
        let directory = root.0.join("personal-worker");
        fs::remove_file(directory.join("disposable-attempt-catalog.json")).unwrap();

        let error = match prepare_durable_service(enrollment(&root.0)) {
            Ok(_) => panic!("orphan inbox unexpectedly authorized a fresh catalog"),
            Err(error) => error,
        };
        assert_eq!(error.code(), "disposable_worker_catalog_unavailable");
        assert!(!directory.join("disposable-attempt-catalog.json").exists());
        assert!(directory.join("github-scale-set-inbox.json").exists());
    }

    #[test]
    fn unsafe_admission_marker_blocks_startup_before_bridge_authority() {
        let root = TempRoot::new();
        drop(prepare_durable_service(enrollment(&root.0)).unwrap());
        let mut store =
            UnixPersonalWorkerStore::open_or_create_disposable_catalog(&root.0).unwrap();
        store.set_disposable_worker_admission_hold(true).unwrap();
        drop(store);
        let marker = root
            .0
            .join("personal-worker/disposable-worker-admission.hold");
        fs::set_permissions(&marker, fs::Permissions::from_mode(0o640)).unwrap();

        let error = match prepare_durable_service(enrollment(&root.0)) {
            Ok(_) => panic!("unsafe admission marker unexpectedly reached service startup"),
            Err(error) => error,
        };
        assert_eq!(
            error.code(),
            "disposable_worker_admission_state_unavailable"
        );
    }

    #[test]
    fn system_clock_produces_one_coherent_positive_epoch() {
        let clock = SystemDisposableWorkerClock;
        let seconds = clock.unix_seconds().unwrap();
        let millis = clock.epoch_millis().unwrap().get();
        assert!(seconds > 0);
        assert!(millis >= seconds * 1_000);
        assert!(millis < seconds.saturating_add(2) * 1_000);
    }
}
