use smolrunner::disposable_attempt_catalog::{
    DisposableAttemptCatalog, DisposableAttemptCatalogAction,
    DisposableAttemptCatalogCodecErrorKind, DisposableAttemptCatalogDocument,
    DisposableAttemptReservation, MAX_DISPOSABLE_ATTEMPT_CATALOG_DOCUMENT_BYTES,
    MemoryDisposableAttemptCatalogStore, decode_disposable_attempt_catalog,
    encode_disposable_attempt_catalog,
};
use smolrunner::disposable_attempt_state::DisposableAttemptState;
use smolrunner::disposable_worker_reconciler::{
    CapacityClaimId, DisposableAttemptId, DisposableAttemptPhase, DisposableVmId,
    DisposableWorkerResources,
};
use smolrunner::execution_admission::EpochMillis;
use smolrunner::github_scale_set_protocol::{
    ScaleSetJobId, ScaleSetJobResult, ScaleSetRunnerId, ScaleSetRunnerName, ScaleSetRunnerReference,
};

fn attempt(index: usize) -> DisposableAttemptState {
    DisposableAttemptState::reserved(
        DisposableAttemptId::parse(&format!("attempt-{index}")).unwrap(),
        CapacityClaimId::parse(&format!("claim-{index}")).unwrap(),
        DisposableVmId::parse(&format!("vm-{index}")).unwrap(),
        ScaleSetRunnerName::parse(&format!("smol-attempt-{index}")).unwrap(),
        EpochMillis::new(200_000 + u64::try_from(index).unwrap()).unwrap(),
    )
}

fn reservation(index: usize) -> DisposableAttemptReservation {
    DisposableAttemptReservation::new(
        attempt(index),
        DisposableWorkerResources::new(1_500, 4 * 1024 * 1024, 16 * 1024 * 1024).unwrap(),
    )
    .unwrap()
}

fn runner(index: usize, id: u64) -> ScaleSetRunnerReference {
    ScaleSetRunnerReference::new(
        ScaleSetRunnerId::new(id).unwrap(),
        ScaleSetRunnerName::parse(&format!("smol-attempt-{index}")).unwrap(),
    )
}

fn transition(
    catalog: &mut DisposableAttemptCatalog<MemoryDisposableAttemptCatalogStore>,
    document: &DisposableAttemptCatalogDocument,
    index: usize,
    action: DisposableAttemptCatalogAction,
) -> DisposableAttemptCatalogDocument {
    let attempt_id = DisposableAttemptId::parse(&format!("attempt-{index}")).unwrap();
    let attempt_revision = document
        .find_active(&attempt_id)
        .unwrap()
        .attempt()
        .revision();
    catalog
        .transition(document.revision(), &attempt_id, attempt_revision, action)
        .unwrap()
        .0
}

fn populated_catalog() -> DisposableAttemptCatalogDocument {
    let mut catalog =
        DisposableAttemptCatalog::new(MemoryDisposableAttemptCatalogStore::default());
    let (empty, _) = catalog.initialize().unwrap();
    let (one, _) = catalog.reserve(empty.revision(), reservation(1)).unwrap();
    let (two, _) = catalog.reserve(one.revision(), reservation(2)).unwrap();

    let provisioning = transition(
        &mut catalog,
        &two,
        2,
        DisposableAttemptCatalogAction::BeginProvisioning,
    );
    let registering = transition(
        &mut catalog,
        &provisioning,
        2,
        DisposableAttemptCatalogAction::BeginRegistration,
    );
    let registered = transition(
        &mut catalog,
        &registering,
        2,
        DisposableAttemptCatalogAction::RecordRegistration(runner(2, 22)),
    );

    let terminal = transition(
        &mut catalog,
        &registered,
        1,
        DisposableAttemptCatalogAction::RecordTerminal {
            runner: None,
            job_id: ScaleSetJobId::parse("completed-job-1").unwrap(),
            result: ScaleSetJobResult::parse("succeeded").unwrap(),
        },
    );
    let destroying = transition(
        &mut catalog,
        &terminal,
        1,
        DisposableAttemptCatalogAction::BeginCleanup,
    );
    let deregistering = transition(
        &mut catalog,
        &destroying,
        1,
        DisposableAttemptCatalogAction::AdvanceCleanup(DisposableAttemptPhase::Deregistering),
    );
    let releasing = transition(
        &mut catalog,
        &deregistering,
        1,
        DisposableAttemptCatalogAction::AdvanceCleanup(DisposableAttemptPhase::Releasing),
    );
    let complete = transition(
        &mut catalog,
        &releasing,
        1,
        DisposableAttemptCatalogAction::AdvanceCleanup(DisposableAttemptPhase::Complete),
    );
    let attempt_id = DisposableAttemptId::parse("attempt-1").unwrap();
    let attempt_revision = complete
        .find_active(&attempt_id)
        .unwrap()
        .attempt()
        .revision();
    catalog
        .retire_complete(complete.revision(), &attempt_id, attempt_revision)
        .unwrap()
        .0
}

#[test]
fn canonical_codec_round_trips_progressed_active_and_tombstone_state() {
    let document = populated_catalog();
    let encoded = encode_disposable_attempt_catalog(&document).unwrap();
    let decoded = decode_disposable_attempt_catalog(&encoded).unwrap();

    assert_eq!(decoded, document);
    assert_eq!(decoded.active().len(), 1);
    assert_eq!(decoded.tombstones().len(), 1);
    assert_eq!(
        decoded.active()[0].attempt().phase(),
        DisposableAttemptPhase::Registering
    );
    assert_eq!(decoded.active()[0].attempt().runner_id().unwrap().get(), 22);
    assert_eq!(
        decoded.tombstones()[0].phase(),
        DisposableAttemptPhase::Complete
    );
}

#[test]
fn noncanonical_json_is_rejected_after_full_validation() {
    let encoded = encode_disposable_attempt_catalog(&populated_catalog()).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    let pretty = serde_json::to_vec_pretty(&value).unwrap();

    let error = decode_disposable_attempt_catalog(&pretty).unwrap_err();
    assert_eq!(
        error.kind(),
        DisposableAttemptCatalogCodecErrorKind::NonCanonical
    );
}

#[test]
fn top_level_future_schema_and_unknown_fields_fail_closed() {
    let encoded = encode_disposable_attempt_catalog(&populated_catalog()).unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();

    value["schema_version"] = serde_json::json!(2);
    let future = serde_json::to_vec(&value).unwrap();
    assert_eq!(
        decode_disposable_attempt_catalog(&future).unwrap_err().kind(),
        DisposableAttemptCatalogCodecErrorKind::VersionIncompatible
    );

    value["schema_version"] = serde_json::json!(1);
    value["unexpected"] = serde_json::json!(true);
    let unknown = serde_json::to_vec(&value).unwrap();
    assert_eq!(
        decode_disposable_attempt_catalog(&unknown).unwrap_err().kind(),
        DisposableAttemptCatalogCodecErrorKind::InvalidDocument
    );
}

#[test]
fn embedded_attempt_unknown_fields_and_invalid_resources_fail_closed() {
    let encoded = encode_disposable_attempt_catalog(&populated_catalog()).unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();

    value["active"][0]["attempt"]["unexpected"] = serde_json::json!(true);
    let invalid_attempt = serde_json::to_vec(&value).unwrap();
    assert_eq!(
        decode_disposable_attempt_catalog(&invalid_attempt)
            .unwrap_err()
            .kind(),
        DisposableAttemptCatalogCodecErrorKind::CorruptState
    );

    let mut value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    value["active"][0]["resources"]["cpu_millis"] = serde_json::json!(0);
    let invalid_resources = serde_json::to_vec(&value).unwrap();
    assert_eq!(
        decode_disposable_attempt_catalog(&invalid_resources)
            .unwrap_err()
            .kind(),
        DisposableAttemptCatalogCodecErrorKind::CorruptState
    );
}

#[test]
fn global_identity_collisions_are_revalidated_after_decode() {
    let mut catalog =
        DisposableAttemptCatalog::new(MemoryDisposableAttemptCatalogStore::default());
    let (empty, _) = catalog.initialize().unwrap();
    let (one, _) = catalog.reserve(empty.revision(), reservation(1)).unwrap();
    let (two, _) = catalog.reserve(one.revision(), reservation(2)).unwrap();
    let encoded = encode_disposable_attempt_catalog(&two).unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();

    let first_name = value["active"][0]["attempt"]["runner_name"].clone();
    value["active"][1]["attempt"]["runner_name"] = first_name;
    let collision = serde_json::to_vec(&value).unwrap();

    assert_eq!(
        decode_disposable_attempt_catalog(&collision)
            .unwrap_err()
            .kind(),
        DisposableAttemptCatalogCodecErrorKind::CorruptState
    );
}

#[test]
fn oversized_document_is_rejected_before_json_parse() {
    let oversized = vec![b' '; MAX_DISPOSABLE_ATTEMPT_CATALOG_DOCUMENT_BYTES + 1];
    assert_eq!(
        decode_disposable_attempt_catalog(&oversized)
            .unwrap_err()
            .kind(),
        DisposableAttemptCatalogCodecErrorKind::DocumentTooLarge
    );
}
