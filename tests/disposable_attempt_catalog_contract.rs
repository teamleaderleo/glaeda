use smolrunner::disposable_attempt_catalog::{
    DisposableAttemptCatalog, DisposableAttemptCatalogAction, DisposableAttemptCatalogDocument,
    DisposableAttemptCatalogErrorKind, DisposableAttemptCatalogStore,
    DisposableAttemptCatalogWriteDisposition, DisposableAttemptReservation,
    MAX_ACTIVE_DISPOSABLE_ATTEMPTS, MemoryDisposableAttemptCatalogStore,
};
use smolrunner::disposable_attempt_state::{DisposableAttemptRevision, DisposableAttemptState};
use smolrunner::disposable_prepared_template::{
    DisposablePreparedTemplateIdentity, current_disposable_prepared_template,
    decode_disposable_prepared_template, encode_disposable_prepared_template,
};
use smolrunner::disposable_worker_reconciler::{
    CapacityClaimId, DisposableAttemptId, DisposableAttemptPhase, DisposableVmId,
    DisposableWorkerResources,
};
use smolrunner::execution_admission::EpochMillis;
use smolrunner::github_scale_set_protocol::{
    ScaleSetJobId, ScaleSetJobResult, ScaleSetRunnerId, ScaleSetRunnerName, ScaleSetRunnerReference,
};

type Catalog = DisposableAttemptCatalog<MemoryDisposableAttemptCatalogStore>;

fn template_digest() -> DisposablePreparedTemplateIdentity {
    current_disposable_prepared_template()
        .unwrap()
        .identity()
        .unwrap()
}

fn other_template_digest() -> DisposablePreparedTemplateIdentity {
    changed_template_identity()
}

fn changed_template_identity() -> DisposablePreparedTemplateIdentity {
    let current = current_disposable_prepared_template().unwrap();
    let bytes = encode_disposable_prepared_template(&current).unwrap();
    let changed = String::from_utf8(bytes).unwrap().replacen(
        "\"recipe_revision\": 2",
        "\"recipe_revision\": 3",
        1,
    );
    decode_disposable_prepared_template(changed.as_bytes())
        .unwrap()
        .identity()
        .unwrap()
}

fn attempt(index: usize) -> DisposableAttemptState {
    DisposableAttemptState::reserved(
        DisposableAttemptId::parse(&format!("attempt-{index}")).unwrap(),
        CapacityClaimId::parse(&format!("claim-{index}")).unwrap(),
        DisposableVmId::parse(&format!("vm-{index}")).unwrap(),
        ScaleSetRunnerName::parse(&format!("smol-attempt-{index}")).unwrap(),
        EpochMillis::new(100_000 + u64::try_from(index).unwrap()).unwrap(),
    )
}

fn reservation(index: usize) -> DisposableAttemptReservation {
    reservation_with_digest(index, template_digest())
}

fn reservation_with_digest(
    index: usize,
    prepared_template_digest: DisposablePreparedTemplateIdentity,
) -> DisposableAttemptReservation {
    DisposableAttemptReservation::new(
        attempt(index),
        DisposableWorkerResources::new(1_000, 2 * 1024 * 1024, 8 * 1024 * 1024).unwrap(),
        prepared_template_digest,
    )
    .unwrap()
}

fn runner(index: usize, id: u64) -> ScaleSetRunnerReference {
    ScaleSetRunnerReference::new(
        ScaleSetRunnerId::new(id).unwrap(),
        ScaleSetRunnerName::parse(&format!("smol-attempt-{index}")).unwrap(),
    )
}

fn initialized() -> (Catalog, DisposableAttemptCatalogDocument) {
    let mut catalog = DisposableAttemptCatalog::new(MemoryDisposableAttemptCatalogStore::default());
    let (document, receipt) = catalog.initialize().unwrap();
    assert_eq!(
        receipt.disposition,
        DisposableAttemptCatalogWriteDisposition::Created
    );
    (catalog, document)
}

fn transition(
    catalog: &mut Catalog,
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

#[test]
fn initialization_and_exact_duplicate_reservation_are_idempotent() {
    let (mut catalog, empty) = initialized();
    let (same_empty, init_receipt) = catalog.initialize().unwrap();
    assert_eq!(same_empty, empty);
    assert_eq!(
        init_receipt.disposition,
        DisposableAttemptCatalogWriteDisposition::Satisfied
    );

    let first = reservation(1);
    let (reserved, reserve_receipt) = catalog.reserve(empty.revision(), first.clone()).unwrap();
    assert_eq!(reserved.active().len(), 1);
    assert_eq!(
        reserve_receipt.disposition,
        DisposableAttemptCatalogWriteDisposition::Replaced
    );
    assert_eq!(reserve_receipt.attempt_revision.unwrap().get(), 1);

    let (duplicate, duplicate_receipt) = catalog.reserve(reserved.revision(), first).unwrap();
    assert_eq!(duplicate, reserved);
    assert_eq!(
        duplicate_receipt.disposition,
        DisposableAttemptCatalogWriteDisposition::Satisfied
    );
    assert_eq!(duplicate_receipt.catalog_revision, reserved.revision());

    let changed_template = DisposableAttemptReservation::new(
        attempt(1),
        DisposableWorkerResources::new(1_000, 2 * 1024 * 1024, 8 * 1024 * 1024).unwrap(),
        other_template_digest(),
    )
    .unwrap();
    assert_eq!(
        catalog
            .reserve(reserved.revision(), changed_template)
            .unwrap_err()
            .kind(),
        DisposableAttemptCatalogErrorKind::AlreadyExists
    );
}

#[test]
fn raw_store_refuses_unrelated_revision_successors() {
    let (mut first_catalog, first_empty) = initialized();
    let (first_reserved, _) = first_catalog
        .reserve(first_empty.revision(), reservation(1))
        .unwrap();

    let (mut second_catalog, second_empty) = initialized();
    let (second_reserved, _) = second_catalog
        .reserve(second_empty.revision(), reservation(2))
        .unwrap();
    let unrelated = transition(
        &mut second_catalog,
        &second_reserved,
        2,
        DisposableAttemptCatalogAction::AuthorizeClone,
    );
    assert_eq!(
        unrelated.revision().get(),
        first_reserved.revision().get() + 1
    );

    let mut store = MemoryDisposableAttemptCatalogStore::default();
    assert_eq!(
        store
            .create(&first_reserved)
            .expect_err("raw create must accept only the exact empty catalog")
            .kind(),
        DisposableAttemptCatalogErrorKind::Conflict
    );
    store.create(&first_empty).unwrap();
    store
        .replace_if_revision(first_empty.revision(), &first_reserved)
        .unwrap();
    let error = store
        .replace_if_revision(first_reserved.revision(), &unrelated)
        .expect_err("revision alone must not authorize an unrelated catalog");
    assert_eq!(error.kind(), DisposableAttemptCatalogErrorKind::Conflict);
    assert_eq!(store.load().unwrap(), Some(first_reserved.clone()));

    let (mut rebound_catalog, rebound_empty) = initialized();
    let (rebound_reserved, _) = rebound_catalog
        .reserve(
            rebound_empty.revision(),
            reservation_with_digest(1, other_template_digest()),
        )
        .unwrap();
    let rebound = transition(
        &mut rebound_catalog,
        &rebound_reserved,
        1,
        DisposableAttemptCatalogAction::AuthorizeClone,
    );
    assert_eq!(
        store
            .replace_if_revision(first_reserved.revision(), &rebound)
            .expect_err("phase advance cannot rebind the prepared-template generation")
            .kind(),
        DisposableAttemptCatalogErrorKind::Conflict
    );
    assert_eq!(store.load().unwrap(), Some(first_reserved));
}

#[test]
fn reserved_attempt_cannot_enter_external_cleanup_through_the_public_catalog() {
    let (mut catalog, empty) = initialized();
    let (reserved, _) = catalog.reserve(empty.revision(), reservation(1)).unwrap();
    let attempt_id = DisposableAttemptId::parse("attempt-1").unwrap();
    let durable = reserved.find_active(&attempt_id).unwrap();

    let error = catalog
        .transition(
            reserved.revision(),
            durable.attempt().attempt_id(),
            durable.attempt().revision(),
            DisposableAttemptCatalogAction::BeginCleanup,
        )
        .expect_err("reserved state has no VM cleanup authority");
    assert_eq!(
        error.kind(),
        DisposableAttemptCatalogErrorKind::InvalidAction
    );
    assert_eq!(catalog.load().unwrap(), reserved);

    let error = catalog
        .transition(
            reserved.revision(),
            &attempt_id,
            durable.attempt().revision(),
            DisposableAttemptCatalogAction::RecordTerminal {
                runner: Some(runner(1, 11)),
                job_id: ScaleSetJobId::parse("preauthorization-terminal").unwrap(),
                result: ScaleSetJobResult::parse("canceled").unwrap(),
            },
        )
        .expect_err("job completion cannot manufacture pre-clone cleanup authority");
    assert_eq!(
        error.kind(),
        DisposableAttemptCatalogErrorKind::InvalidAction
    );
    assert_eq!(catalog.load().unwrap(), reserved);
}

#[test]
fn durable_active_attempts_are_the_host_resource_ledger() {
    let (mut catalog, empty) = initialized();
    let (one, _) = catalog.reserve(empty.revision(), reservation(1)).unwrap();
    let (two, _) = catalog.reserve(one.revision(), reservation(2)).unwrap();

    let usage = serde_json::to_value(two.host_usage().unwrap()).unwrap();
    assert_eq!(usage["workers"], 2);
    assert_eq!(usage["resources"]["cpu_millis"], 2_000);
    assert_eq!(usage["resources"]["memory_bytes"], 4 * 1024 * 1024);
    assert_eq!(usage["resources"]["disk_bytes"], 16 * 1024 * 1024);
}

#[test]
fn catalog_and_attempt_revisions_reject_stale_mutations() {
    let (mut catalog, empty) = initialized();
    let (reserved, _) = catalog.reserve(empty.revision(), reservation(1)).unwrap();
    let attempt_id = DisposableAttemptId::parse("attempt-1").unwrap();
    let attempt_revision = DisposableAttemptRevision::new(1).unwrap();

    let (provisioning, _) = catalog
        .transition(
            reserved.revision(),
            &attempt_id,
            attempt_revision,
            DisposableAttemptCatalogAction::AuthorizeClone,
        )
        .unwrap();

    let stale_catalog = catalog
        .transition(
            reserved.revision(),
            &attempt_id,
            DisposableAttemptRevision::new(2).unwrap(),
            DisposableAttemptCatalogAction::BeginRegistration,
        )
        .unwrap_err();
    assert_eq!(
        stale_catalog.kind(),
        DisposableAttemptCatalogErrorKind::Conflict
    );

    let stale_attempt = catalog
        .transition(
            provisioning.revision(),
            &attempt_id,
            attempt_revision,
            DisposableAttemptCatalogAction::BeginRegistration,
        )
        .unwrap_err();
    assert_eq!(
        stale_attempt.kind(),
        DisposableAttemptCatalogErrorKind::Conflict
    );
}

#[test]
fn global_ownership_identities_are_unique_across_attempts() {
    let (mut catalog, empty) = initialized();
    let (one, _) = catalog.reserve(empty.revision(), reservation(1)).unwrap();

    let duplicate_name = DisposableAttemptState::reserved(
        DisposableAttemptId::parse("attempt-2").unwrap(),
        CapacityClaimId::parse("claim-2").unwrap(),
        DisposableVmId::parse("vm-2").unwrap(),
        ScaleSetRunnerName::parse("smol-attempt-1").unwrap(),
        EpochMillis::new(100_002).unwrap(),
    );
    let conflicting = DisposableAttemptReservation::new(
        duplicate_name,
        DisposableWorkerResources::new(1_000, 2 * 1024 * 1024, 8 * 1024 * 1024).unwrap(),
        template_digest(),
    )
    .unwrap();

    let error = catalog.reserve(one.revision(), conflicting).unwrap_err();
    assert_eq!(
        error.kind(),
        DisposableAttemptCatalogErrorKind::CorruptState
    );
    assert_eq!(catalog.load().unwrap().active().len(), 1);
}

#[test]
fn duplicate_terminal_observation_is_satisfied_without_catalog_churn() {
    let (mut catalog, empty) = initialized();
    let (reserved, _) = catalog.reserve(empty.revision(), reservation(1)).unwrap();
    let authorized = transition(
        &mut catalog,
        &reserved,
        1,
        DisposableAttemptCatalogAction::AuthorizeClone,
    );
    let job = ScaleSetJobId::parse("opaque-job-1").unwrap();
    let result = ScaleSetJobResult::parse("future-service-result").unwrap();

    let terminal = transition(
        &mut catalog,
        &authorized,
        1,
        DisposableAttemptCatalogAction::RecordTerminal {
            runner: Some(runner(1, 11)),
            job_id: job.clone(),
            result: result.clone(),
        },
    );
    let attempt_id = DisposableAttemptId::parse("attempt-1").unwrap();
    let attempt_revision = terminal
        .find_active(&attempt_id)
        .unwrap()
        .attempt()
        .revision();

    let (same, receipt) = catalog
        .transition(
            terminal.revision(),
            &attempt_id,
            attempt_revision,
            DisposableAttemptCatalogAction::RecordTerminal {
                runner: Some(runner(1, 11)),
                job_id: job,
                result,
            },
        )
        .unwrap();
    assert_eq!(same, terminal);
    assert_eq!(
        receipt.disposition,
        DisposableAttemptCatalogWriteDisposition::Satisfied
    );
    assert_eq!(receipt.catalog_revision, terminal.revision());
    assert_eq!(receipt.attempt_revision, Some(attempt_revision));
}

#[test]
fn identity_drift_remains_distinct_from_an_illegal_phase_action() {
    let (mut catalog, empty) = initialized();
    let (reserved, _) = catalog.reserve(empty.revision(), reservation(1)).unwrap();
    let provisioning = transition(
        &mut catalog,
        &reserved,
        1,
        DisposableAttemptCatalogAction::AuthorizeClone,
    );
    let registering = transition(
        &mut catalog,
        &provisioning,
        1,
        DisposableAttemptCatalogAction::BeginRegistration,
    );

    let attempt_id = DisposableAttemptId::parse("attempt-1").unwrap();
    let attempt_revision = registering
        .find_active(&attempt_id)
        .unwrap()
        .attempt()
        .revision();
    let wrong_runner = ScaleSetRunnerReference::new(
        ScaleSetRunnerId::new(9).unwrap(),
        ScaleSetRunnerName::parse("smol-attempt-other").unwrap(),
    );
    let drift = catalog
        .transition(
            registering.revision(),
            &attempt_id,
            attempt_revision,
            DisposableAttemptCatalogAction::RecordRegistration(wrong_runner),
        )
        .unwrap_err();
    assert_eq!(
        drift.kind(),
        DisposableAttemptCatalogErrorKind::IdentityDrift
    );
}

#[test]
fn completed_attempt_releases_usage_then_moves_to_bounded_replay_history() {
    let (mut catalog, empty) = initialized();
    let (reserved, _) = catalog.reserve(empty.revision(), reservation(1)).unwrap();
    let authorized = transition(
        &mut catalog,
        &reserved,
        1,
        DisposableAttemptCatalogAction::AuthorizeClone,
    );
    let terminal = transition(
        &mut catalog,
        &authorized,
        1,
        DisposableAttemptCatalogAction::RecordTerminal {
            runner: Some(runner(1, 11)),
            job_id: ScaleSetJobId::parse("job-complete-1").unwrap(),
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

    let usage = serde_json::to_value(complete.host_usage().unwrap()).unwrap();
    assert_eq!(usage["workers"], 0);
    let attempt_id = DisposableAttemptId::parse("attempt-1").unwrap();
    let attempt_revision = complete
        .find_active(&attempt_id)
        .unwrap()
        .attempt()
        .revision();
    let (retired, _) = catalog
        .retire_complete(complete.revision(), &attempt_id, attempt_revision)
        .unwrap();

    assert!(retired.find_active(&attempt_id).is_none());
    assert_eq!(
        retired.find_tombstone(&attempt_id).unwrap().phase(),
        DisposableAttemptPhase::Complete
    );
    assert_eq!(retired.tombstones().len(), 1);

    let reuse = catalog
        .reserve(retired.revision(), reservation(1))
        .unwrap_err();
    assert_eq!(
        reuse.kind(),
        DisposableAttemptCatalogErrorKind::AlreadyExists
    );
}

#[test]
fn active_limit_refuses_new_work_but_keeps_exact_duplicate_reservation_idempotent() {
    let (mut catalog, mut document) = initialized();
    for index in 0..MAX_ACTIVE_DISPOSABLE_ATTEMPTS {
        (document, _) = catalog
            .reserve(document.revision(), reservation(index))
            .unwrap();
    }

    let first = reservation(0);
    let (same, receipt) = catalog.reserve(document.revision(), first).unwrap();
    assert_eq!(same, document);
    assert_eq!(
        receipt.disposition,
        DisposableAttemptCatalogWriteDisposition::Satisfied
    );

    let overflow = catalog
        .reserve(
            document.revision(),
            reservation(MAX_ACTIVE_DISPOSABLE_ATTEMPTS),
        )
        .unwrap_err();
    assert_eq!(
        overflow.kind(),
        DisposableAttemptCatalogErrorKind::LimitExceeded
    );
}

#[test]
fn exact_runner_ids_cannot_be_reused_across_concurrent_attempts() {
    let (mut catalog, empty) = initialized();
    let (one, _) = catalog.reserve(empty.revision(), reservation(1)).unwrap();
    let (two, _) = catalog.reserve(one.revision(), reservation(2)).unwrap();

    let one_provisioning = transition(
        &mut catalog,
        &two,
        1,
        DisposableAttemptCatalogAction::AuthorizeClone,
    );
    let one_registering = transition(
        &mut catalog,
        &one_provisioning,
        1,
        DisposableAttemptCatalogAction::BeginRegistration,
    );
    let one_registered = transition(
        &mut catalog,
        &one_registering,
        1,
        DisposableAttemptCatalogAction::RecordRegistration(runner(1, 77)),
    );

    let two_provisioning = transition(
        &mut catalog,
        &one_registered,
        2,
        DisposableAttemptCatalogAction::AuthorizeClone,
    );
    let two_registering = transition(
        &mut catalog,
        &two_provisioning,
        2,
        DisposableAttemptCatalogAction::BeginRegistration,
    );
    let attempt_id = DisposableAttemptId::parse("attempt-2").unwrap();
    let duplicate_runner_id = catalog
        .transition(
            two_registering.revision(),
            &attempt_id,
            two_registering
                .find_active(&attempt_id)
                .unwrap()
                .attempt()
                .revision(),
            DisposableAttemptCatalogAction::RecordRegistration(runner(2, 77)),
        )
        .unwrap_err();
    assert_eq!(
        duplicate_runner_id.kind(),
        DisposableAttemptCatalogErrorKind::CorruptState
    );
}

#[test]
fn exact_job_ids_cannot_be_reused_across_concurrent_attempts() {
    let (mut catalog, empty) = initialized();
    let (one, _) = catalog.reserve(empty.revision(), reservation(1)).unwrap();
    let (two, _) = catalog.reserve(one.revision(), reservation(2)).unwrap();
    let shared_job = ScaleSetJobId::parse("shared-job-id").unwrap();

    let one_authorized = transition(
        &mut catalog,
        &two,
        1,
        DisposableAttemptCatalogAction::AuthorizeClone,
    );
    let both_authorized = transition(
        &mut catalog,
        &one_authorized,
        2,
        DisposableAttemptCatalogAction::AuthorizeClone,
    );
    let one_terminal = transition(
        &mut catalog,
        &both_authorized,
        1,
        DisposableAttemptCatalogAction::RecordTerminal {
            runner: Some(runner(1, 11)),
            job_id: shared_job.clone(),
            result: ScaleSetJobResult::parse("canceled").unwrap(),
        },
    );
    let attempt_id = DisposableAttemptId::parse("attempt-2").unwrap();
    let duplicate_job_id = catalog
        .transition(
            one_terminal.revision(),
            &attempt_id,
            one_terminal
                .find_active(&attempt_id)
                .unwrap()
                .attempt()
                .revision(),
            DisposableAttemptCatalogAction::RecordTerminal {
                runner: Some(runner(2, 22)),
                job_id: shared_job,
                result: ScaleSetJobResult::parse("canceled").unwrap(),
            },
        )
        .unwrap_err();
    assert_eq!(
        duplicate_job_id.kind(),
        DisposableAttemptCatalogErrorKind::CorruptState
    );
}
