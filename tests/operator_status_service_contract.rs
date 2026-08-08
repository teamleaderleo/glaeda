#![allow(dead_code)]

pub use smolrunner::{
    actions_runner_readiness, artifact, execution_admission, lima_observation, mac_availability,
    operator_config, operator_error, operator_status, personal_worker_queue,
    personal_worker_read_model, personal_worker_store, verification_profile,
};

#[path = "../src/operator_status_service.rs"]
mod operator_status_service;

use std::fs;
use std::path::PathBuf;

use operator_status_service::{
    OperatorStatusEvidenceReader, OperatorStatusJobEvidence, OperatorStatusService,
    OperatorStatusServiceErrorKind, OperatorStatusServiceEvidence, OperatorStatusTerminalEvidence,
    OperatorStatusWorkerEvidence,
};
use smolrunner::actions_runner_readiness::{
    ACTIONS_RUNNER_READINESS_SCHEMA_VERSION, ActionsRunnerConfiguredIdentity, ActionsRunnerName,
    ActionsRunnerReadinessReport, ActionsRunnerReadinessState,
};
use smolrunner::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use smolrunner::execution_admission::{
    DrainAcknowledgement, EpochMillis, ExecutionAdmissionIdentity, ExecutionAdmissionInput,
    ExecutionAdmissionRecord, ExecutionAdmissionState, ExecutionRequestId, ExecutionResourceLimits,
    FallbackProfileEligibility, HostCapacityObservation, ReservationEvidence,
    ReservationGeneration, ReservationId, RunnerProfileId, UnavailableReason,
};
use smolrunner::lima_observation::{
    LIMA_OBSERVATION_SCHEMA_VERSION, LimaArchitecture, LimaConfiguredInstance,
    LimaFilesystemObjectIdentity, LimaGuestObservation, LimaGuestResources, LimaInstanceName,
    LimaInstanceObservationReport, LimaObservationFreshness, LimaObservationTiming,
    LimaObservedGuest, LimaPersistentIdentity, LimaRuntimeState, LimaVmType,
};
use smolrunner::mac_availability::AvailabilityRequest;
use smolrunner::operator_config::{
    GuestWorkspacePath, OperatorConfig, OperatorIdlePolicy, OperatorOutputPreference,
    OperatorRemediationPreference, PersonalWorkerStateRoot,
};
use smolrunner::operator_error::{OperatorErrorCode, OperatorPublicError};
use smolrunner::operator_status::{
    OperatorConfigurationCompatibility, OperatorStatusDisposition, OperatorTerminalResult,
};
use smolrunner::personal_worker_queue::{
    PersonalWorkerActiveReservation, PersonalWorkerActivityEvidence, PersonalWorkerCacheAccessMode,
    PersonalWorkerCacheNamespace, PersonalWorkerCancellationState, PersonalWorkerJobRequest,
    PersonalWorkerPriority, PersonalWorkerProfile, PersonalWorkerProfileObservation,
    PersonalWorkerQueueGeneration, PersonalWorkerQueueInput, PersonalWorkerSourceIdentity,
};
use smolrunner::personal_worker_read_model::{
    PersonalWorkerJobReadRequest, PersonalWorkerJobView, personal_worker_job_view,
    personal_worker_status,
};
use smolrunner::personal_worker_store::{
    PersonalWorkerDurableCacheLease, PersonalWorkerStoreDocument, PersonalWorkerTerminalTombstone,
};
use smolrunner::verification_profile::{CacheId, VerificationProfileId};

const BASE_MILLIS: u64 = 5_000_000;
const GIB: u64 = 1_024 * 1_024 * 1_024;

fn millis(value: u64) -> EpochMillis {
    EpochMillis::new(value).expect("time")
}

fn digest(byte: &str) -> Sha256Digest {
    Sha256Digest::parse(&format!("sha256:{}", byte.repeat(64))).expect("digest")
}

fn config(workspace: &str) -> OperatorConfig {
    OperatorConfig::new(
        PersonalWorkerStateRoot::parse(PathBuf::from("/var/lib/smolrunner-test"))
            .expect("state root"),
        LimaInstanceName::parse("smolrunner").expect("instance"),
        GuestWorkspacePath::parse(workspace).expect("workspace"),
        VerificationProfileId::parse("smolrunner.required").expect("profile"),
        AvailabilityRequest::Auto,
        OperatorIdlePolicy::new(600_000, 1_800_000).expect("idle policy"),
        OperatorOutputPreference::Json,
        OperatorRemediationPreference::IncludeSuggestions,
    )
    .expect("config")
}

fn timing(started: u64, observed: u64, expires: u64) -> LimaObservationTiming {
    LimaObservationTiming {
        started_at_unix_seconds: started,
        observed_at_unix_seconds: observed,
        expires_at_unix_seconds: expires,
        duration_seconds: observed - started,
        freshness: LimaObservationFreshness::Fresh,
    }
}

fn lima(state: LimaRuntimeState) -> LimaInstanceObservationReport {
    let guest = if state == LimaRuntimeState::Running {
        LimaGuestObservation::Observed(LimaObservedGuest {
            resources: LimaGuestResources {
                architecture: LimaArchitecture::Aarch64,
                cpus: 4,
                memory_bytes: 8 * GIB,
            },
            persistent_identity: LimaPersistentIdentity {
                guest_machine_id_digest: digest("a"),
                root_filesystem: LimaFilesystemObjectIdentity {
                    device_id: 1,
                    inode: 2,
                },
                cache_directory: LimaFilesystemObjectIdentity {
                    device_id: 1,
                    inode: 3,
                },
            },
        })
    } else {
        LimaGuestObservation::NotRunning {
            runtime_state: state,
        }
    };
    LimaInstanceObservationReport {
        schema_version: LIMA_OBSERVATION_SCHEMA_VERSION,
        instance: LimaInstanceName::parse("smolrunner").expect("instance"),
        configured: LimaConfiguredInstance {
            runtime_state: state,
            vm_type: LimaVmType::Vz,
            architecture: LimaArchitecture::Aarch64,
            cpus: 4,
            memory_bytes: 8 * GIB,
            primary_disk_bytes: 100 * GIB,
        },
        guest,
        timing: timing(100, 101, 200),
    }
}

fn runner(state: ActionsRunnerReadinessState) -> ActionsRunnerReadinessReport {
    let configured_identity = matches!(
        state,
        ActionsRunnerReadinessState::IdleReady
            | ActionsRunnerReadinessState::Busy
            | ActionsRunnerReadinessState::Draining
    )
    .then(|| ActionsRunnerConfiguredIdentity {
        runner_name: ActionsRunnerName::parse("smolrunner-local").expect("runner name"),
        configuration_digest: digest("b"),
        runner_root: LimaFilesystemObjectIdentity {
            device_id: 1,
            inode: 4,
        },
    });
    ActionsRunnerReadinessReport {
        schema_version: ACTIONS_RUNNER_READINESS_SCHEMA_VERSION,
        instance: LimaInstanceName::parse("smolrunner").expect("instance"),
        runner_name: ActionsRunnerName::parse("smolrunner-local").expect("runner name"),
        state,
        configured_identity,
        timing: timing(102, 103, 200),
    }
}

fn empty_document() -> PersonalWorkerStoreDocument {
    PersonalWorkerStoreDocument::new(
        PersonalWorkerQueueInput {
            generation: PersonalWorkerQueueGeneration::new(1).expect("generation"),
            observed_at: millis(BASE_MILLIS),
            profile_observation: PersonalWorkerProfileObservation::observed(
                PersonalWorkerProfile::Stopped,
            ),
            activity_evidence: PersonalWorkerActivityEvidence::Never,
            queued: vec![],
            active: vec![],
            pending_profile_change: None,
        },
        vec![],
    )
    .expect("empty document")
}

fn limits() -> ExecutionResourceLimits {
    ExecutionResourceLimits::new(2_000, 2 * GIB, 2_048).expect("limits")
}

fn request(id: &str, repository: &str, digit: char) -> PersonalWorkerJobRequest {
    let identity = ExecutionAdmissionIdentity::new(
        ExecutionRequestId::parse(id).expect("request ID"),
        VerificationProfileId::parse("smolrunner.required").expect("verification profile"),
        RunnerProfileId::parse("personal-lima-work").expect("runner profile"),
    );
    PersonalWorkerJobRequest {
        identity,
        source: PersonalWorkerSourceIdentity::new(
            RepositoryRef::parse(repository).expect("repository"),
            CommitId::parse(&digit.to_string().repeat(40)).expect("commit"),
            GitTreeId::parse(&digit.to_string().repeat(40)).expect("tree"),
        ),
        priority: PersonalWorkerPriority::Normal,
        requested_limits: limits(),
        cache_namespace: PersonalWorkerCacheNamespace::RepositoryBuild {
            cache_id: CacheId::parse("build-cache").expect("cache ID"),
            repository: RepositoryRef::parse(repository).expect("cache repository"),
            namespace_digest: digest("c"),
        },
        cache_access: PersonalWorkerCacheAccessMode::Write,
        submitted_at: millis(BASE_MILLIS - 120_000),
        operator_deadline: None,
        cancellation: PersonalWorkerCancellationState::Active,
        fallback_eligibility: FallbackProfileEligibility::ineligible(),
    }
}

fn reservation(
    request: PersonalWorkerJobRequest,
    id: &str,
    state: ExecutionAdmissionState,
) -> (
    PersonalWorkerActiveReservation,
    PersonalWorkerDurableCacheLease,
) {
    let reservation_id = ReservationId::parse(id).expect("reservation ID");
    let generation = ReservationGeneration::new(1).expect("reservation generation");
    let reserved_at = millis(BASE_MILLIS - 30_000);
    let admission = ExecutionAdmissionRecord::from_input(ExecutionAdmissionInput {
        identity: request.identity.clone(),
        state,
        observed_at: millis(BASE_MILLIS - 10_000),
        requested_limits: request.requested_limits,
        host_capacity: Some(HostCapacityObservation::new(
            millis(BASE_MILLIS - 30_000),
            ExecutionResourceLimits::new(8_000, 10 * GIB, 4_096).expect("capacity"),
        )),
        applied_limits: Some(request.requested_limits),
        queue_position: None,
        reservation: Some(ReservationEvidence::new(
            reservation_id.clone(),
            generation,
            reserved_at,
            millis(BASE_MILLIS + 3_600_000),
        )),
        acknowledgement: None,
        fallback_eligibility: FallbackProfileEligibility::ineligible(),
        unavailable_reason: None,
    })
    .expect("active admission");
    let lease = PersonalWorkerDurableCacheLease::new(
        request.identity.request_id.clone(),
        request.cache_namespace.clone(),
        request.cache_access,
        reservation_id,
        generation,
        reserved_at,
    );
    (
        PersonalWorkerActiveReservation {
            request,
            admission,
            started_at: Some(millis(BASE_MILLIS - 20_000)),
        },
        lease,
    )
}

fn terminal(request: PersonalWorkerJobRequest) -> PersonalWorkerTerminalTombstone {
    let reservation_id = ReservationId::parse("reservation-terminal").expect("reservation ID");
    let generation = ReservationGeneration::new(2).expect("reservation generation");
    let reserved_at = millis(BASE_MILLIS - 40_000);
    let admission = ExecutionAdmissionRecord::from_input(ExecutionAdmissionInput {
        identity: request.identity.clone(),
        state: ExecutionAdmissionState::Unavailable,
        observed_at: millis(BASE_MILLIS),
        requested_limits: request.requested_limits,
        host_capacity: Some(HostCapacityObservation::new(
            millis(BASE_MILLIS - 40_000),
            ExecutionResourceLimits::new(8_000, 10 * GIB, 4_096).expect("capacity"),
        )),
        applied_limits: Some(request.requested_limits),
        queue_position: None,
        reservation: Some(ReservationEvidence::new(
            reservation_id.clone(),
            generation,
            reserved_at,
            millis(BASE_MILLIS + 3_600_000),
        )),
        acknowledgement: Some(DrainAcknowledgement::Drain),
        fallback_eligibility: FallbackProfileEligibility::ineligible(),
        unavailable_reason: Some(UnavailableReason::Drained),
    })
    .expect("terminal admission");
    let lease = PersonalWorkerDurableCacheLease::new(
        request.identity.request_id.clone(),
        request.cache_namespace.clone(),
        request.cache_access,
        reservation_id,
        generation,
        reserved_at,
    );
    PersonalWorkerTerminalTombstone::new(
        request,
        admission,
        Some(millis(BASE_MILLIS - 20_000)),
        lease,
    )
    .expect("terminal tombstone")
}

fn active_terminal_document() -> PersonalWorkerStoreDocument {
    let active_request = request("active-one", "example/active", '1');
    let (active, active_lease) = reservation(
        active_request,
        "reservation-active",
        ExecutionAdmissionState::Running,
    );
    PersonalWorkerStoreDocument::new_with_terminal_tombstones(
        PersonalWorkerQueueInput {
            generation: PersonalWorkerQueueGeneration::new(1).expect("generation"),
            observed_at: millis(BASE_MILLIS),
            profile_observation: PersonalWorkerProfileObservation::observed(
                PersonalWorkerProfile::Work,
            ),
            activity_evidence: PersonalWorkerActivityEvidence::observed(millis(BASE_MILLIS)),
            queued: vec![],
            active: vec![active],
            pending_profile_change: None,
        },
        vec![active_lease],
        vec![terminal(request("terminal-one", "example/terminal", '2'))],
    )
    .expect("active terminal document")
}

fn job_view(document: &PersonalWorkerStoreDocument, id: &str) -> PersonalWorkerJobView {
    personal_worker_job_view(
        document,
        PersonalWorkerJobReadRequest::new(
            document.revision(),
            document.queue().generation,
            ExecutionRequestId::parse(id).expect("request ID"),
        ),
    )
    .expect("job view")
}

fn evidence(
    config: OperatorConfig,
    document: &PersonalWorkerStoreDocument,
    lima: LimaInstanceObservationReport,
    runner: ActionsRunnerReadinessReport,
) -> OperatorStatusServiceEvidence {
    OperatorStatusServiceEvidence::new(
        config.clone(),
        OperatorConfigurationCompatibility::Compatible,
        OperatorStatusWorkerEvidence::new(
            config.identity().clone(),
            personal_worker_status(document).expect("status"),
            None,
            None,
        ),
        lima,
        runner,
        110,
        vec![],
    )
}

#[test]
fn idle_snapshot_is_satisfied_and_reader_is_called_exactly_once() {
    struct Reader {
        calls: u8,
        evidence: Option<OperatorStatusServiceEvidence>,
    }

    impl OperatorStatusEvidenceReader for Reader {
        fn read_evidence(&mut self) -> Result<OperatorStatusServiceEvidence, OperatorPublicError> {
            self.calls += 1;
            Ok(self.evidence.take().expect("one read"))
        }
    }

    let mut reader = Reader {
        calls: 0,
        evidence: Some(evidence(
            config("/home/lima/smolrunner-workspace"),
            &empty_document(),
            lima(LimaRuntimeState::Stopped),
            runner(ActionsRunnerReadinessState::Offline),
        )),
    };
    let report = OperatorStatusService::read(&mut reader).expect("status report");
    assert_eq!(reader.calls, 1);
    assert_eq!(report.disposition(), OperatorStatusDisposition::Satisfied);
    assert!(report.blockers().is_empty());
    assert!(report.render_human().contains("worker activity: never"));
    assert_eq!(
        serde_json::to_value(&report).expect("JSON")["disposition"],
        "satisfied"
    );
}

#[test]
fn reader_failure_is_preserved_without_fabricating_status() {
    struct Reader(u8);
    impl OperatorStatusEvidenceReader for Reader {
        fn read_evidence(&mut self) -> Result<OperatorStatusServiceEvidence, OperatorPublicError> {
            self.0 += 1;
            Err(OperatorPublicError::from_code(
                OperatorErrorCode::DurableStateBusy,
            ))
        }
    }

    let mut reader = Reader(0);
    let error = OperatorStatusService::read(&mut reader).expect_err("reader failure");
    assert_eq!(reader.0, 1);
    assert_eq!(
        error.kind(),
        OperatorStatusServiceErrorKind::EvidenceUnavailable
    );
    assert_eq!(
        error.public_error().code(),
        OperatorErrorCode::DurableStateBusy
    );
}

#[test]
fn active_and_terminal_views_project_only_from_the_exact_snapshot() {
    let config = config("/home/lima/smolrunner-workspace");
    let document = active_terminal_document();
    let bundle = OperatorStatusServiceEvidence::new(
        config.clone(),
        OperatorConfigurationCompatibility::Compatible,
        OperatorStatusWorkerEvidence::new(
            config.identity().clone(),
            personal_worker_status(&document).expect("status"),
            Some(OperatorStatusJobEvidence::new(
                config.identity().clone(),
                job_view(&document, "active-one"),
            )),
            Some(OperatorStatusTerminalEvidence::new(
                OperatorStatusJobEvidence::new(
                    config.identity().clone(),
                    job_view(&document, "terminal-one"),
                ),
                OperatorTerminalResult::Succeeded,
            )),
        ),
        lima(LimaRuntimeState::Running),
        runner(ActionsRunnerReadinessState::Busy),
        110,
        vec![],
    );

    let report = OperatorStatusService::compose(bundle).expect("active report");
    assert_eq!(
        report.disposition(),
        OperatorStatusDisposition::Continuation
    );
    assert_eq!(report.machine().runner(), ActionsRunnerReadinessState::Busy);
    assert_eq!(
        report.active_job().expect("active job").state(),
        ExecutionAdmissionState::Running
    );
    assert_eq!(
        report.latest_terminal().expect("terminal").result(),
        OperatorTerminalResult::Succeeded
    );
}

#[test]
fn missing_active_or_unretained_terminal_evidence_fails_closed() {
    let config = config("/home/lima/smolrunner-workspace");
    let active_document = active_terminal_document();
    let missing_active = OperatorStatusServiceEvidence::new(
        config.clone(),
        OperatorConfigurationCompatibility::Compatible,
        OperatorStatusWorkerEvidence::new(
            config.identity().clone(),
            personal_worker_status(&active_document).expect("status"),
            None,
            None,
        ),
        lima(LimaRuntimeState::Running),
        runner(ActionsRunnerReadinessState::Busy),
        110,
        vec![],
    );
    let error = OperatorStatusService::compose(missing_active).expect_err("missing active job");
    assert_eq!(
        error.kind(),
        OperatorStatusServiceErrorKind::InvalidActiveJob
    );
    assert_eq!(
        error.public_error().code(),
        OperatorErrorCode::DurableStateCorrupt
    );

    let empty = empty_document();
    let unretained_terminal = OperatorStatusServiceEvidence::new(
        config.clone(),
        OperatorConfigurationCompatibility::Compatible,
        OperatorStatusWorkerEvidence::new(
            config.identity().clone(),
            personal_worker_status(&empty).expect("status"),
            None,
            Some(OperatorStatusTerminalEvidence::new(
                OperatorStatusJobEvidence::new(
                    config.identity().clone(),
                    job_view(&active_document, "terminal-one"),
                ),
                OperatorTerminalResult::Succeeded,
            )),
        ),
        lima(LimaRuntimeState::Stopped),
        runner(ActionsRunnerReadinessState::Offline),
        110,
        vec![],
    );
    let error =
        OperatorStatusService::compose(unretained_terminal).expect_err("unretained terminal");
    assert_eq!(
        error.kind(),
        OperatorStatusServiceErrorKind::InvalidTerminal
    );
}

#[test]
fn configuration_and_snapshot_drift_fail_closed_in_precedence_order() {
    let accepted = config("/home/lima/accepted");
    let foreign = config("/home/lima/foreign");
    let document = active_terminal_document();
    let status = personal_worker_status(&document).expect("status");
    let active = job_view(&document, "active-one");

    let mismatched = OperatorStatusServiceEvidence::new(
        accepted.clone(),
        OperatorConfigurationCompatibility::Compatible,
        OperatorStatusWorkerEvidence::new(foreign.identity().clone(), status.clone(), None, None),
        lima(LimaRuntimeState::Stopped),
        runner(ActionsRunnerReadinessState::Offline),
        110,
        vec![],
    );
    let error = OperatorStatusService::compose(mismatched).expect_err("config drift");
    assert_eq!(
        error.kind(),
        OperatorStatusServiceErrorKind::ConfigurationMismatch
    );
    assert_eq!(
        error.public_error().code(),
        OperatorErrorCode::ConfigurationIncompatible
    );

    let advanced = document
        .advance_with_terminal_tombstones(
            PersonalWorkerQueueInput {
                generation: PersonalWorkerQueueGeneration::new(2).expect("generation"),
                observed_at: millis(BASE_MILLIS + 1),
                profile_observation: document.queue().profile_observation,
                activity_evidence: document.queue().activity_evidence,
                queued: document.queue().queued.clone(),
                active: document.queue().active.clone(),
                pending_profile_change: document.queue().pending_profile_change,
            },
            document.cache_leases().to_vec(),
            document.terminal_tombstones().to_vec(),
        )
        .expect("advance snapshot");
    let stale = OperatorStatusServiceEvidence::new(
        accepted.clone(),
        OperatorConfigurationCompatibility::Compatible,
        OperatorStatusWorkerEvidence::new(
            accepted.identity().clone(),
            personal_worker_status(&advanced).expect("advanced status"),
            Some(OperatorStatusJobEvidence::new(
                accepted.identity().clone(),
                active,
            )),
            None,
        ),
        lima(LimaRuntimeState::Running),
        runner(ActionsRunnerReadinessState::Busy),
        110,
        vec![],
    );
    let error = OperatorStatusService::compose(stale).expect_err("stale snapshot");
    assert_eq!(error.kind(), OperatorStatusServiceErrorKind::StaleRevision);
    assert_eq!(
        error.public_error().code(),
        OperatorErrorCode::DurableStateRevisionStale
    );
}

#[test]
fn stale_broken_and_incompatible_evidence_become_deduplicated_blockers() {
    let config = config("/home/lima/smolrunner-workspace");
    let document = empty_document();
    let bundle = OperatorStatusServiceEvidence::new(
        config.clone(),
        OperatorConfigurationCompatibility::Incompatible,
        OperatorStatusWorkerEvidence::new(
            config.identity().clone(),
            personal_worker_status(&document).expect("status"),
            None,
            None,
        ),
        lima(LimaRuntimeState::Broken),
        runner(ActionsRunnerReadinessState::Offline),
        201,
        vec![
            OperatorPublicError::from_code(OperatorErrorCode::LimaBroken),
            OperatorPublicError::from_code(OperatorErrorCode::LimaObservationStale),
        ],
    );

    let report = OperatorStatusService::compose(bundle).expect("blocked report");
    assert_eq!(report.disposition(), OperatorStatusDisposition::Blocked);
    let codes = report
        .blockers()
        .iter()
        .map(OperatorPublicError::code)
        .collect::<Vec<_>>();
    assert_eq!(codes.len(), 4);
    assert!(codes.contains(&OperatorErrorCode::ConfigurationIncompatible));
    assert!(codes.contains(&OperatorErrorCode::LimaBroken));
    assert!(codes.contains(&OperatorErrorCode::LimaObservationStale));
    assert!(codes.contains(&OperatorErrorCode::RunnerObservationStale));
}

#[test]
fn future_or_noncanonical_timing_and_machine_identity_fail_closed() {
    let config = config("/home/lima/smolrunner-workspace");
    let document = empty_document();
    let future = OperatorStatusServiceEvidence::new(
        config.clone(),
        OperatorConfigurationCompatibility::Compatible,
        OperatorStatusWorkerEvidence::new(
            config.identity().clone(),
            personal_worker_status(&document).expect("status"),
            None,
            None,
        ),
        lima(LimaRuntimeState::Stopped),
        runner(ActionsRunnerReadinessState::Offline),
        100,
        vec![],
    );
    let error = OperatorStatusService::compose(future).expect_err("future evidence");
    assert_eq!(error.kind(), OperatorStatusServiceErrorKind::InvalidTiming);

    let mut wrong_instance = lima(LimaRuntimeState::Stopped);
    wrong_instance.instance = LimaInstanceName::parse("foreign").expect("foreign instance");
    let error = OperatorStatusService::compose(evidence(
        config.clone(),
        &empty_document(),
        wrong_instance,
        runner(ActionsRunnerReadinessState::Offline),
    ))
    .expect_err("Lima drift");
    assert_eq!(
        error.kind(),
        OperatorStatusServiceErrorKind::LimaIdentityMismatch
    );

    let mut wrong_runner = runner(ActionsRunnerReadinessState::Offline);
    wrong_runner.configured_identity = Some(ActionsRunnerConfiguredIdentity {
        runner_name: ActionsRunnerName::parse("smolrunner-local").expect("runner name"),
        configuration_digest: digest("b"),
        runner_root: LimaFilesystemObjectIdentity {
            device_id: 1,
            inode: 4,
        },
    });
    let error = OperatorStatusService::compose(evidence(
        config,
        &empty_document(),
        lima(LimaRuntimeState::Stopped),
        wrong_runner,
    ))
    .expect_err("runner drift");
    assert_eq!(
        error.kind(),
        OperatorStatusServiceErrorKind::RunnerIdentityMismatch
    );
}

#[test]
fn debug_and_source_keep_private_authority_out_of_the_service_surface() {
    let bundle = evidence(
        config("/home/lima/private-workspace"),
        &empty_document(),
        lima(LimaRuntimeState::Stopped),
        runner(ActionsRunnerReadinessState::Offline),
    );
    let debug = format!("{bundle:?}");
    assert!(!debug.contains("private-workspace"));
    assert!(!debug.contains("/var/lib/smolrunner-test"));

    let source = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/operator_status_service.rs"
    ))
    .expect("service source");
    for forbidden in [
        "std::process",
        "Command::new",
        "limactl",
        "gh api",
        "std::fs",
        "thread::",
        "SystemTime",
        "tokio",
    ] {
        assert!(
            !source.contains(forbidden),
            "service unexpectedly owns authority token {forbidden}"
        );
    }
}
