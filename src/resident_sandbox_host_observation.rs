//! Read-only host observation for one exact durable resident-sandbox generation.
//!
//! This first slice proves planned-instance absence for the exact authorized materialization
//! attempt. It executes only strict `limactl list` observations, retains the validated shared Lima
//! source descriptor, and grants no Lima lifecycle, guest, project-disk, or durable binding
//! authority. A later #711 transaction may consume this process-local lineage; restart destroys it.

use std::fmt;
use std::path::Path;

use serde::Serialize;

use crate::artifact::Sha256Digest;
use crate::lima_observation::{
    LimaInstanceAbsenceObservation, LimaInstanceName, LimaObservationAdapter, LimaObservationClock,
    LimaObservationRefusalCode, LimaObservationRequest, LimaObservationTiming,
};
use crate::process::CommandExecutor;
use crate::project_catalog::ProjectIdentity;
use crate::project_disk_host_observation::{
    HeldProjectDiskLimaSource, ProjectDiskHostObservationErrorKind, ProjectDiskLimaSourceIdentity,
};
use crate::project_disk_lease::{ResidentSandboxGeneration, ResidentSandboxId};
use crate::resident_sandbox_catalog::{
    ResidentSandboxActiveOperation, ResidentSandboxArchitecture, ResidentSandboxBackend,
    ResidentSandboxOperationGeneration, ResidentSandboxOperationPhase,
    ResidentSandboxPhysicalState, ResidentSandboxRecord, ResidentSandboxRecordRevision,
};

const RESIDENT_OBSERVATION_MAX_AGE_SECONDS: u64 = 30;
const RESIDENT_GUEST_CACHE_PATH: &str = "/var/lib/smolrunner-runner/.cache";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResidentSandboxHostObservationErrorKind {
    InvalidRecord,
    SourceMismatch,
    InstancePresent,
    SourceChanged,
    ObservationUnavailable,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct ResidentSandboxHostObservationError {
    pub kind: ResidentSandboxHostObservationErrorKind,
    pub code: &'static str,
    pub message: &'static str,
}

impl fmt::Debug for ResidentSandboxHostObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResidentSandboxHostObservationError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for ResidentSandboxHostObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ResidentSandboxHostObservationError {}

/// Process-local proof that the exact authorized resident locator is absent beneath one held
/// validated Lima source and from two strict named external observations.
///
/// This value is deliberately neither cloneable nor serializable. Its durable-looking fields are
/// equality lineage only; the retained source descriptor is what prevents restart or a copied
/// digest from recreating first-binding provenance.
pub struct ResidentSandboxAbsenceObservation {
    project: ProjectIdentity,
    sandbox_id: ResidentSandboxId,
    generation: ResidentSandboxGeneration,
    record_revision: ResidentSandboxRecordRevision,
    source_identity: ProjectDiskLimaSourceIdentity,
    locator: LimaInstanceName,
    config_digest: Sha256Digest,
    materialize_generation: ResidentSandboxOperationGeneration,
    materialize_policy_identity: Sha256Digest,
    source: HeldProjectDiskLimaSource,
    request: LimaObservationRequest,
    inventory_observation: LimaInstanceAbsenceObservation,
}

impl ResidentSandboxAbsenceObservation {
    #[must_use]
    pub const fn project(&self) -> &ProjectIdentity {
        &self.project
    }

    #[must_use]
    pub const fn sandbox_id(&self) -> &ResidentSandboxId {
        &self.sandbox_id
    }

    #[must_use]
    pub const fn generation(&self) -> ResidentSandboxGeneration {
        self.generation
    }

    #[must_use]
    pub const fn record_revision(&self) -> ResidentSandboxRecordRevision {
        self.record_revision
    }

    #[must_use]
    pub const fn source_identity(&self) -> &ProjectDiskLimaSourceIdentity {
        &self.source_identity
    }

    #[must_use]
    pub const fn config_digest(&self) -> &Sha256Digest {
        &self.config_digest
    }

    #[must_use]
    pub const fn materialize_generation(&self) -> ResidentSandboxOperationGeneration {
        self.materialize_generation
    }

    #[must_use]
    pub const fn materialize_policy_identity(&self) -> &Sha256Digest {
        &self.materialize_policy_identity
    }

    #[must_use]
    pub const fn timing(&self) -> &LimaObservationTiming {
        self.inventory_observation.timing()
    }

    /// Re-prove local child absence and strict external named absence while retaining the same
    /// source descriptor. This performs no mutation and cannot extend the durable record revision.
    pub fn confirm(
        &mut self,
        adapter: &LimaObservationAdapter,
        executor: &impl CommandExecutor,
        clock: &impl LimaObservationClock,
    ) -> Result<(), ResidentSandboxHostObservationError> {
        confirm_local_absence(&self.source, &self.locator)?;
        let observation = adapter
            .observe_named_absence(&self.request, executor, clock)
            .map_err(map_lima_observation_error)?;
        confirm_local_absence(&self.source, &self.locator)?;
        self.inventory_observation = observation;
        Ok(())
    }
}

impl fmt::Debug for ResidentSandboxAbsenceObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResidentSandboxAbsenceObservation")
            .field("project", &self.project)
            .field("sandbox_id", &self.sandbox_id)
            .field("generation", &self.generation)
            .field("record_revision", &self.record_revision)
            .field("source_identity", &self.source_identity)
            .field("locator", &self.locator)
            .field("config_digest", &self.config_digest)
            .field("materialize_generation", &self.materialize_generation)
            .field(
                "materialize_policy_identity",
                &self.materialize_policy_identity,
            )
            .field("private_source", &"<held-lima-source>")
            .field("inventory_observation", &self.inventory_observation)
            .finish()
    }
}

/// Prove planned absence for the exact currently authorized materialization attempt.
///
/// The request derives its source, instance locator, backend, architecture, guest cache path, and
/// freshness window internally. Callers can supply neither a Lima home/path nor a raw locator,
/// generation, config/host digest, inventory document, or observation callback.
pub fn observe_planned_resident_sandbox_absence(
    record: &ResidentSandboxRecord,
    expected_record_revision: ResidentSandboxRecordRevision,
    source: HeldProjectDiskLimaSource,
    adapter: &LimaObservationAdapter,
    executor: &impl CommandExecutor,
    clock: &impl LimaObservationClock,
) -> Result<ResidentSandboxAbsenceObservation, ResidentSandboxHostObservationError> {
    if record.revision() != expected_record_revision {
        return Err(invalid_record("resident sandbox record revision is stale"));
    }
    if record.source_identity() != source.identity() {
        return Err(source_mismatch());
    }
    if record.config().backend() != ResidentSandboxBackend::Vz
        || record.config().architecture() != ResidentSandboxArchitecture::Aarch64
    {
        return Err(invalid_record(
            "resident sandbox backend or architecture is not the reviewed VZ/aarch64 profile",
        ));
    }
    if !matches!(
        record.physical(),
        ResidentSandboxPhysicalState::Unmaterialized
    ) {
        return Err(invalid_record(
            "planned resident absence requires an unmaterialized generation",
        ));
    }
    let (materialize_generation, materialize_policy_identity) = match record.active_operation() {
        ResidentSandboxActiveOperation::Materialize {
            generation,
            policy_identity,
            phase: ResidentSandboxOperationPhase::Authorized,
        } => (*generation, policy_identity.clone()),
        _ => {
            return Err(invalid_record(
                "planned resident absence requires the exact authorized materialization attempt",
            ));
        }
    };

    let locator = LimaInstanceName::parse(record.locator().as_str()).map_err(|_| {
        invalid_record("resident sandbox locator is not one valid Lima instance name")
    })?;
    let request = source
        .resident_observation_request(
            locator.clone(),
            Path::new(RESIDENT_GUEST_CACHE_PATH),
            RESIDENT_OBSERVATION_MAX_AGE_SECONDS,
        )
        .map_err(|_| source_changed())?;

    confirm_local_absence(&source, &locator)?;
    let inventory_observation = adapter
        .observe_named_absence(&request, executor, clock)
        .map_err(map_lima_observation_error)?;
    confirm_local_absence(&source, &locator)?;

    Ok(ResidentSandboxAbsenceObservation {
        project: record.project().clone(),
        sandbox_id: record.sandbox_id().clone(),
        generation: record.generation(),
        record_revision: record.revision(),
        source_identity: record.source_identity().clone(),
        locator,
        config_digest: record.config_digest().clone(),
        materialize_generation,
        materialize_policy_identity,
        source,
        request,
        inventory_observation,
    })
}

fn confirm_local_absence(
    source: &HeldProjectDiskLimaSource,
    locator: &LimaInstanceName,
) -> Result<(), ResidentSandboxHostObservationError> {
    source
        .confirm_resident_instance_absent(locator)
        .map_err(|error| {
            if error.kind() == ProjectDiskHostObservationErrorKind::Present {
                instance_present()
            } else {
                source_changed()
            }
        })
}

fn map_lima_observation_error(
    error: crate::lima_observation::LimaObservationFailure,
) -> ResidentSandboxHostObservationError {
    if error.code == LimaObservationRefusalCode::UnexpectedInstanceEvidence {
        return instance_present();
    }
    ResidentSandboxHostObservationError {
        kind: ResidentSandboxHostObservationErrorKind::ObservationUnavailable,
        code: "resident_sandbox_observation_unavailable",
        message: "strict Lima absence observation was unavailable or conflicting",
    }
}

const fn invalid_record(message: &'static str) -> ResidentSandboxHostObservationError {
    ResidentSandboxHostObservationError {
        kind: ResidentSandboxHostObservationErrorKind::InvalidRecord,
        code: "resident_sandbox_observation_record_invalid",
        message,
    }
}

const fn source_mismatch() -> ResidentSandboxHostObservationError {
    ResidentSandboxHostObservationError {
        kind: ResidentSandboxHostObservationErrorKind::SourceMismatch,
        code: "resident_sandbox_observation_source_mismatch",
        message: "resident sandbox record and held Lima source identity differ",
    }
}

const fn instance_present() -> ResidentSandboxHostObservationError {
    ResidentSandboxHostObservationError {
        kind: ResidentSandboxHostObservationErrorKind::InstancePresent,
        code: "resident_sandbox_instance_present",
        message: "the exact planned resident Lima instance is already present",
    }
}

const fn source_changed() -> ResidentSandboxHostObservationError {
    ResidentSandboxHostObservationError {
        kind: ResidentSandboxHostObservationErrorKind::SourceChanged,
        code: "resident_sandbox_observation_source_changed",
        message: "held resident Lima source or planned child changed during observation",
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    use crate::process::{CommandSpec, ExecutionRecord};
    use crate::project_disk_host_observation::ConfiguredProjectDiskLimaSource;
    use crate::resident_sandbox_catalog::{
        ResidentCredentialPolicyGeneration, ResidentGuestControlPolicyGeneration,
        ResidentGuestPrivilegePolicy, ResidentGuestPrivilegePolicyGeneration,
        ResidentLimaLayoutGeneration, ResidentLocatorPolicyGeneration,
        ResidentNetworkPolicyGeneration, ResidentPreparedTemplateGeneration,
        ResidentProjectIntegrationPolicyGeneration, ResidentResourceDeclaration,
        ResidentResourceGeneration, ResidentSandboxAcceptanceRequest,
        ResidentSandboxAcceptanceRequestId, ResidentSandboxCatalog, ResidentSandboxCheckpoint,
        ResidentSandboxConfig, ResidentSandboxConfigGeneration,
    };

    use super::*;

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

    struct Fixture {
        root: PathBuf,
        lima_home: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "smolrunner-resident-absence-{}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
            let lima_home = root.join("lima-home");
            fs::create_dir(&root).expect("create fixture root");
            fs::create_dir(&lima_home).expect("create Lima home");
            fs::set_permissions(&lima_home, fs::Permissions::from_mode(0o700))
                .expect("private Lima home");
            Self { root, lima_home }
        }

        fn source(&self) -> ConfiguredProjectDiskLimaSource {
            ConfiguredProjectDiskLimaSource::new(&self.lima_home).expect("configured source")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[derive(Default)]
    struct MissingExecutor {
        calls: AtomicUsize,
    }

    impl MissingExecutor {
        fn calls(&self) -> usize {
            self.calls.load(Ordering::Relaxed)
        }
    }

    impl CommandExecutor for MissingExecutor {
        fn execute(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let argv = spec.displayed_argv();
            let instance = argv.last().expect("named list request").clone();
            Ok(ExecutionRecord {
                argv,
                environment_keys: spec.environment.keys().cloned().collect(),
                status: Some(1),
                success: false,
                stdout: String::new(),
                stderr: format!(
                    "time=\"2026-08-16T14:39:23+08:00\" level=warning msg=\"No instance matching {instance} found.\"\n\
                     time=\"2026-08-16T14:39:23+08:00\" level=fatal msg=\"unmatched instances\"\n"
                ),
            })
        }
    }

    struct FixedClock {
        value: AtomicU64,
    }

    impl FixedClock {
        const fn new(value: u64) -> Self {
            Self {
                value: AtomicU64::new(value),
            }
        }
    }

    impl LimaObservationClock for FixedClock {
        fn unix_seconds(&self) -> io::Result<u64> {
            Ok(self.value.fetch_add(1, Ordering::Relaxed))
        }
    }

    #[test]
    fn authorized_planned_absence_retains_exact_lineage_and_reconfirms() {
        let fixture = Fixture::new();
        let source = fixture.source();
        let record = record(source.identity(), true);
        let executor = MissingExecutor::default();
        let mut absence = observe_planned_resident_sandbox_absence(
            &record,
            record.revision(),
            source.hold().expect("held source"),
            &adapter(),
            &executor,
            &FixedClock::new(100),
        )
        .expect("planned absence");

        assert_eq!(absence.project(), record.project());
        assert_eq!(absence.sandbox_id(), record.sandbox_id());
        assert_eq!(absence.generation(), record.generation());
        assert_eq!(absence.record_revision(), record.revision());
        assert_eq!(absence.source_identity(), record.source_identity());
        assert_eq!(absence.config_digest(), record.config_digest());
        assert_eq!(executor.calls(), 2);
        assert!(!format!("{absence:?}").contains(fixture.lima_home.to_str().unwrap()));

        absence
            .confirm(&adapter(), &executor, &FixedClock::new(200))
            .expect("confirmed absence");
        assert_eq!(executor.calls(), 4);
    }

    #[test]
    fn unstarted_or_stale_attempt_cannot_mint_absence() {
        let fixture = Fixture::new();
        let source = fixture.source();
        let unstarted = record(source.identity(), false);
        let executor = MissingExecutor::default();
        let error = observe_planned_resident_sandbox_absence(
            &unstarted,
            unstarted.revision(),
            source.hold().expect("held source"),
            &adapter(),
            &executor,
            &FixedClock::new(100),
        )
        .expect_err("materialize attempt not authorized");
        assert_eq!(
            error.kind,
            ResidentSandboxHostObservationErrorKind::InvalidRecord
        );
        assert_eq!(executor.calls(), 0);

        let source = fixture.source();
        let authorized = record(source.identity(), true);
        let stale = ResidentSandboxRecordRevision::new(authorized.revision().get() + 1)
            .expect("stale revision");
        let error = observe_planned_resident_sandbox_absence(
            &authorized,
            stale,
            source.hold().expect("held source"),
            &adapter(),
            &executor,
            &FixedClock::new(100),
        )
        .expect_err("stale expected revision");
        assert_eq!(
            error.kind,
            ResidentSandboxHostObservationErrorKind::InvalidRecord
        );
        assert_eq!(executor.calls(), 0);
    }

    #[test]
    fn wrong_source_existing_child_and_rebound_parent_fail_before_inventory() {
        let fixture = Fixture::new();
        let other = Fixture::new();
        let source = fixture.source();
        let record = record(source.identity(), true);
        let executor = MissingExecutor::default();

        let wrong_source = other.source();
        let error = observe_planned_resident_sandbox_absence(
            &record,
            record.revision(),
            wrong_source.hold().expect("other held source"),
            &adapter(),
            &executor,
            &FixedClock::new(100),
        )
        .expect_err("wrong source");
        assert_eq!(
            error.kind,
            ResidentSandboxHostObservationErrorKind::SourceMismatch
        );

        fs::create_dir(fixture.lima_home.join(record.locator().as_str()))
            .expect("same-name instance child");
        let error = observe_planned_resident_sandbox_absence(
            &record,
            record.revision(),
            source.hold().expect("held source"),
            &adapter(),
            &executor,
            &FixedClock::new(100),
        )
        .expect_err("existing child");
        assert_eq!(
            error.kind,
            ResidentSandboxHostObservationErrorKind::InstancePresent
        );

        fs::remove_dir(fixture.lima_home.join(record.locator().as_str()))
            .expect("remove child fixture");
        let source = fixture.source();
        let held = source.hold().expect("held source before replacement");
        fs::rename(&fixture.lima_home, fixture.root.join("old-lima-home"))
            .expect("replace Lima home");
        fs::create_dir(&fixture.lima_home).expect("new Lima home");
        fs::set_permissions(&fixture.lima_home, fs::Permissions::from_mode(0o700))
            .expect("private replacement");
        let error = observe_planned_resident_sandbox_absence(
            &record,
            record.revision(),
            held,
            &adapter(),
            &executor,
            &FixedClock::new(100),
        )
        .expect_err("rebound held source");
        assert_eq!(
            error.kind,
            ResidentSandboxHostObservationErrorKind::SourceChanged
        );
        assert_eq!(executor.calls(), 0);
    }

    fn adapter() -> LimaObservationAdapter {
        LimaObservationAdapter::new("/opt/homebrew/bin/limactl").expect("adapter")
    }

    fn record(
        source_identity: &ProjectDiskLimaSourceIdentity,
        authorize_materialize: bool,
    ) -> ResidentSandboxRecord {
        let config = ResidentSandboxConfig::reviewed(
            ResidentPreparedTemplateGeneration::new(1).unwrap(),
            ResidentSandboxConfigGeneration::new(1).unwrap(),
            ResidentLimaLayoutGeneration::new(1).unwrap(),
            ResidentResourceDeclaration::new(
                ResidentResourceGeneration::new(1).unwrap(),
                2_000,
                2 * 1024 * 1024 * 1024,
                20 * 1024 * 1024 * 1024,
            )
            .unwrap(),
            ResidentNetworkPolicyGeneration::new(1).unwrap(),
            ResidentCredentialPolicyGeneration::new(1).unwrap(),
            ResidentGuestControlPolicyGeneration::new(1).unwrap(),
            ResidentGuestPrivilegePolicy::reviewed(
                ResidentGuestPrivilegePolicyGeneration::new(1).unwrap(),
            ),
            ResidentProjectIntegrationPolicyGeneration::new(1).unwrap(),
            None,
        )
        .unwrap();
        let request = ResidentSandboxAcceptanceRequest::new(
            ResidentSandboxAcceptanceRequestId::parse("resident-absence-request").unwrap(),
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            ResidentSandboxId::parse("resident-a").unwrap(),
            source_identity.clone(),
            ResidentLocatorPolicyGeneration::new(1).unwrap(),
            config,
        );
        let (catalog, receipt) = ResidentSandboxCatalog::empty().accept(request).unwrap();
        let catalog = if authorize_materialize {
            catalog
                .checkpoint(
                    receipt.key(),
                    catalog.find(receipt.key()).unwrap().revision(),
                    ResidentSandboxCheckpoint::MaterializeAuthorized,
                )
                .unwrap()
        } else {
            catalog
        };
        catalog.find(receipt.key()).unwrap().clone()
    }
}
