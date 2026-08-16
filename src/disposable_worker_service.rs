//! Production composition for one enrolled disposable Scale Set worker.
//!
//! Local durable recovery and the process-lifetime service lease complete before a bridge process
//! is started or a GitHub App key leaves Keychain. The service then delegates one bounded step at
//! a time to the existing coordinator and supervisor. Template readiness gates new capacity inside
//! the coordinator; it does not gate delivery recovery or cleanup of an existing attempt.

#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::fmt;
use std::io;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::disposable_attempt_catalog::DisposableAttemptCatalog;
use crate::disposable_clone_runtime::CloneRuntimeClock;
use crate::disposable_worker_coordinator::{
    DisposableWorkerCoordinator, DisposableWorkerCoordinatorDisposition,
};
use crate::disposable_worker_enrollment::{
    DisposableWorkerEnrollment, DisposableWorkerEnrollmentParts,
};
use crate::disposable_worker_supervisor::{
    DisposableWorkerSupervisorControl, DisposableWorkerSupervisorDisposition,
    DisposableWorkerSupervisorDriver, DisposableWorkerSupervisorError, supervise_disposable_worker,
};
use crate::execution_admission::EpochMillis;
use crate::github_scale_set_bridge::{ScaleSetBridgeClient, ScaleSetBridgeConfig};
use crate::lima_observation::LimaObservationClock;
use crate::personal_worker_store::PersonalWorkerStoreErrorKind;
use crate::process::ProcessExecutor;
use crate::unix_personal_worker_store::{DisposableWorkerServiceLock, UnixPersonalWorkerStore};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisposableWorkerServiceErrorKind {
    DurableState,
    Supervisor,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct DisposableWorkerServiceError {
    kind: DisposableWorkerServiceErrorKind,
    code: &'static str,
}

impl DisposableWorkerServiceError {
    pub const fn kind(self) -> DisposableWorkerServiceErrorKind {
        self.kind
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
        formatter.write_str("the disposable worker service could not continue")
    }
}

impl std::error::Error for DisposableWorkerServiceError {}

pub(crate) struct DisposableWorkerServiceDriver {
    _service_lock: DisposableWorkerServiceLock,
    bridge_config: ScaleSetBridgeConfig,
    coordinator: DisposableWorkerCoordinator,
    clone_runtime: crate::disposable_clone_runtime::DisposableCloneRuntime,
    runner_runtime: crate::disposable_runner_runtime::DisposableRunnerRuntime,
    executor: ProcessExecutor,
    clock: SystemDisposableWorkerClock,
}

/// Recover local durable state and acquire exclusive process-lifetime service ownership.
///
/// This boundary performs no Keychain read and starts no bridge, Lima, or guest process.
pub(crate) fn prepare_disposable_worker_service(
    enrollment: DisposableWorkerEnrollment,
) -> Result<DisposableWorkerServiceDriver, DisposableWorkerServiceError> {
    let parts = enrollment.into_parts();
    let neutral =
        UnixPersonalWorkerStore::open_or_create_disposable_worker_service_store(&parts.state_root)
            .map_err(|_| durable_error("disposable_worker_store_unavailable"))?;
    let service_lock = neutral
        .acquire_disposable_worker_service_lock()
        .map_err(|error| {
            if error.kind() == PersonalWorkerStoreErrorKind::Busy {
                durable_error("disposable_worker_service_busy")
            } else {
                durable_error("disposable_worker_service_lock_unavailable")
            }
        })?;

    // A live delivery owns the catalog boundary until its external transaction settles. Starting
    // the bridge is what permits that recovery, so unrelated template/catalog opens must not gate
    // startup here.
    let delivery_evidence = neutral
        .has_scale_set_delivery_recovery_evidence()
        .map_err(|_| durable_error("disposable_worker_delivery_recovery_unavailable"))?;
    drop(neutral);
    if !delivery_evidence {
        recover_local_documents(&parts.state_root)?;
    }
    UnixPersonalWorkerStore::open_or_create_scale_set_delivery_controller(&parts.state_root)
        .map_err(|_| durable_error("disposable_worker_delivery_recovery_unavailable"))?;

    Ok(build_driver(parts, service_lock))
}

fn recover_local_documents(path: &std::path::Path) -> Result<(), DisposableWorkerServiceError> {
    // One staged document can temporarily fence recovery of the other. Each successful opener
    // removes or publishes only its own classified stage, so two bounded passes are sufficient.
    for _ in 0..2 {
        let template_clean =
            UnixPersonalWorkerStore::open_or_create_disposable_template_generation(path).is_ok();
        let catalog_clean = UnixPersonalWorkerStore::open_or_create_disposable_catalog(path)
            .ok()
            .and_then(|store| {
                let mut catalog = DisposableAttemptCatalog::new(store);
                catalog.initialize().ok()
            })
            .is_some();
        if template_clean && catalog_clean {
            return Ok(());
        }
    }
    Err(durable_error("disposable_worker_recovery_required"))
}

fn build_driver(
    parts: DisposableWorkerEnrollmentParts,
    service_lock: DisposableWorkerServiceLock,
) -> DisposableWorkerServiceDriver {
    DisposableWorkerServiceDriver {
        _service_lock: service_lock,
        bridge_config: parts.bridge_config,
        coordinator: DisposableWorkerCoordinator::new(
            parts.state_root,
            parts.consumer_policy,
            Box::new(parts.host_storage),
        ),
        clone_runtime: parts.clone_runtime,
        runner_runtime: parts.runner_runtime,
        executor: ProcessExecutor,
        clock: SystemDisposableWorkerClock,
    }
}

impl DisposableWorkerSupervisorDriver for DisposableWorkerServiceDriver {
    type Session = ScaleSetBridgeClient;

    fn connect(&mut self) -> Result<Self::Session, DisposableWorkerSupervisorError> {
        #[cfg(target_os = "macos")]
        {
            ScaleSetBridgeClient::connect_from_keychain(self.bridge_config.clone())
                .map_err(|error| DisposableWorkerSupervisorError::new(error.code()))
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = &self.bridge_config;
            Err(DisposableWorkerSupervisorError::new(
                "disposable_worker_platform_unsupported",
            ))
        }
    }

    fn supervise_once(
        &mut self,
        session: &mut Self::Session,
    ) -> Result<DisposableWorkerCoordinatorDisposition, DisposableWorkerSupervisorError> {
        self.coordinator
            .supervise_once(
                session,
                &self.clone_runtime,
                &self.runner_runtime,
                &self.executor,
                &self.clock,
            )
            .map_err(|error| DisposableWorkerSupervisorError::new(error.code()))
    }
}

fn serve_disposable_worker_with_control(
    enrollment: DisposableWorkerEnrollment,
    control: &mut impl DisposableWorkerSupervisorControl,
) -> Result<DisposableWorkerSupervisorDisposition, DisposableWorkerServiceError> {
    let mut driver = prepare_disposable_worker_service(enrollment)?;
    supervise_disposable_worker(&mut driver, control)
        .map_err(|error| supervisor_error(error.code()))
}

/// Run one enrolled disposable-worker controller until the outer process supervisor terminates
/// the process.
///
/// Local durable recovery and exclusive service ownership complete before the bridge reads the
/// GitHub App key from Keychain. Retry pacing and circuit breaking remain inside the bounded
/// supervisor loop; `launchd` owns process termination and restart.
///
/// # Errors
///
/// Returns a bounded, path-free error if local durable preparation cannot begin.
#[cfg(target_os = "macos")]
pub fn serve_disposable_worker(
    enrollment: DisposableWorkerEnrollment,
) -> Result<(), DisposableWorkerServiceError> {
    let mut control = ThreadSupervisorControl;
    serve_disposable_worker_with_control(enrollment, &mut control).map(|_| ())
}

struct ThreadSupervisorControl;

impl DisposableWorkerSupervisorControl for ThreadSupervisorControl {
    fn stop_requested(&self) -> bool {
        false
    }

    fn wait_or_stop(
        &mut self,
        duration: std::time::Duration,
    ) -> Result<bool, DisposableWorkerSupervisorError> {
        thread::sleep(duration);
        Ok(false)
    }
}

#[derive(Debug, Clone, Copy)]
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

const fn durable_error(code: &'static str) -> DisposableWorkerServiceError {
    DisposableWorkerServiceError {
        kind: DisposableWorkerServiceErrorKind::DurableState,
        code,
    }
}

const fn supervisor_error(code: &'static str) -> DisposableWorkerServiceError {
    DisposableWorkerServiceError {
        kind: DisposableWorkerServiceErrorKind::Supervisor,
        code,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::artifact::Sha256Digest;
    use crate::disposable_prepared_template::current_disposable_prepared_template;
    use crate::disposable_template_generation::{
        DisposableTemplateGenerationDocument, DisposableTemplateGenerationId,
        DisposableTemplateSourceIdentity, encode_disposable_template_generation,
    };
    use crate::disposable_worker_enrollment::decode_disposable_worker_enrollment;
    use crate::lima_observation::LimaInstanceName;

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);
    const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

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
        let root = serde_json::to_string(root.to_str().unwrap()).unwrap();
        let document = format!(
            concat!(
                "{{\n",
                "  \"schema_version\": 1,\n",
                "  \"state_root\": {root},\n",
                "  \"lima\": {{\n",
                "    \"program\": \"/opt/homebrew/bin/limactl\",\n",
                "    \"home\": \"/private/var/lib/smolrunner/lima\",\n",
                "    \"source_instance\": \"smolrunner-prepared-template\"\n",
                "  }},\n",
                "  \"bridge\": {{\n",
                "    \"program_digest\": \"{DIGEST}\"\n",
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
            root = root,
            DIGEST = DIGEST,
        );
        decode_disposable_worker_enrollment(document.as_bytes()).unwrap()
    }

    #[test]
    fn preparation_recovers_state_and_excludes_a_second_service_before_bridge_start() {
        let root = TempRoot::new();
        let first = prepare_disposable_worker_service(enrollment(&root.0)).unwrap();
        let second = match prepare_disposable_worker_service(enrollment(&root.0)) {
            Ok(_) => panic!("a second service unexpectedly acquired the lifetime lease"),
            Err(error) => error,
        };
        assert_eq!(
            second.kind(),
            DisposableWorkerServiceErrorKind::DurableState
        );
        assert_eq!(second.code(), "disposable_worker_service_busy");

        drop(first);
        prepare_disposable_worker_service(enrollment(&root.0)).unwrap();
    }

    #[test]
    fn preparation_recovers_a_staged_template_before_opening_the_delivery_controller() {
        let root = TempRoot::new();
        UnixPersonalWorkerStore::open_or_create_disposable_worker_service_store(&root.0).unwrap();
        let prepared = current_disposable_prepared_template().unwrap();
        let document = DisposableTemplateGenerationDocument::runtime_initial(
            DisposableTemplateGenerationId::parse("staged-restart").unwrap(),
            prepared.identity().unwrap(),
            DisposableTemplateSourceIdentity::from_runtime_digest(
                Sha256Digest::parse(DIGEST).unwrap(),
            ),
            LimaInstanceName::parse("smolrunner-prepared-template").unwrap(),
        );
        let bytes = encode_disposable_template_generation(&document).unwrap();
        let store_directory = root.0.join("personal-worker");
        let staged = store_directory.join(".disposable-template-generation.next.json");
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&staged)
            .unwrap();
        file.write_all(&bytes).unwrap();
        file.sync_all().unwrap();
        drop(file);

        prepare_disposable_worker_service(enrollment(&root.0)).unwrap();
        assert!(!staged.exists());
        assert!(
            store_directory
                .join("disposable-template-generation.json")
                .is_file()
        );
    }
}
