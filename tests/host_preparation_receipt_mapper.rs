#![cfg(target_os = "linux")]

use smolrunner::artifact::Sha256Digest;
use smolrunner::debian_package_plan::PackagePlanDisposition;
use smolrunner::execution_receipt::{
    ExecutionReceiptActionOutcome, ExecutionReceiptDisposition, ExecutionReceiptOperation,
    ReceiptTimestamp, encode_execution_receipt,
};
use smolrunner::host_preparation_execution::{
    HOST_PREPARATION_EXECUTION_SCHEMA_VERSION, HostPreparationExecutionDisposition,
    HostPreparationExecutionReport,
};
use smolrunner::host_preparation_plan::{
    DeferredActionReason, DeferredHostPreparationAction, FreshObservationBarrier,
    HostReadinessSourceIdentity, SourceExecutableIdentity, SourceRootlessPodmanIdentity,
    SourceRunnerAccountIdentity,
};
use smolrunner::host_preparation_receipt::{
    HostPreparationReceiptContext, HostPreparationReceiptErrorKind,
    map_host_preparation_execution_receipt,
};
use smolrunner::host_readiness::HostObservationState;
use smolrunner::journal::{
    ActionOutcome, ExecutionJournal, ExecutionLane, JOURNAL_SCHEMA_VERSION, JournalRecord,
    PlannedMutation, Preconditions, RollbackClass,
};
use smolrunner::lane_command::LaneCommandKind;
use smolrunner::rootless_podman_preflight::RootlessPodmanPreflightState;
use smolrunner::state::JournalId;

const PRIVATE_PATH: &str = "/private/host/executable";
const PRIVATE_MESSAGE: &str = "PRIVATE journal prose token=secret";

fn source(repository: &str) -> HostReadinessSourceIdentity {
    HostReadinessSourceIdentity {
        kind: "host_readiness_report".to_owned(),
        schema_version: 1,
        repository: repository.to_owned(),
        executables: vec![SourceExecutableIdentity {
            name: "podman".to_owned(),
            path: PRIVATE_PATH.to_owned(),
            state: HostObservationState::Matching,
        }],
        package_plan_schema_version: 1,
        package_disposition: PackagePlanDisposition::Ready,
        runner_account: SourceRunnerAccountIdentity::NeedsConfiguration,
        rootless_podman: SourceRootlessPodmanIdentity::Deferred {
            state: RootlessPodmanPreflightState::Absent,
        },
    }
}

fn mutation(id: &str) -> PlannedMutation {
    PlannedMutation::new(
        id,
        ExecutionLane::Root,
        format!("private summary at {PRIVATE_PATH}"),
        RollbackClass::Irreversible,
        Preconditions::new([format!("private evidence at {PRIVATE_PATH}")]),
    )
}

fn record(id: &str, outcome: ActionOutcome, message: Option<&str>) -> JournalRecord {
    JournalRecord {
        action: mutation(id),
        outcome,
        message: message.map(str::to_owned),
    }
}

fn barrier(id: &str) -> FreshObservationBarrier {
    FreshObservationBarrier {
        id: id.to_owned(),
        after_action_ids: vec!["change-mappings".to_owned()],
        requirements: Vec::new(),
        summary: format!("private barrier at {PRIVATE_PATH}"),
    }
}

fn deferred(id: &str) -> DeferredHostPreparationAction {
    DeferredHostPreparationAction {
        id: id.to_owned(),
        lane: ExecutionLane::RunnerUser,
        command_kind: LaneCommandKind::RunnerPodmanMigrate,
        summary: format!("private deferred work at {PRIVATE_PATH}"),
        depends_on: vec!["change-mappings".to_owned()],
        reason: DeferredActionReason::FreshObservationRequired,
    }
}

fn report(
    disposition: HostPreparationExecutionDisposition,
    records: Vec<JournalRecord>,
) -> HostPreparationExecutionReport {
    HostPreparationExecutionReport {
        schema_version: HOST_PREPARATION_EXECUTION_SCHEMA_VERSION,
        source: source("teamleaderleo/smolrunner"),
        phase_id: "host-preparation-root-phase".to_owned(),
        disposition,
        journal: ExecutionJournal {
            schema_version: JOURNAL_SCHEMA_VERSION,
            records,
            stopped_after: None,
        },
        continuation_barriers: Vec::new(),
        deferred_actions: Vec::new(),
    }
}

fn context(execution_id: &str) -> HostPreparationReceiptContext {
    HostPreparationReceiptContext::new(
        JournalId::parse(execution_id).expect("journal ID"),
        Sha256Digest::parse(&format!("sha256:{}", "ab".repeat(32))).expect("source digest"),
        ReceiptTimestamp::parse("2026-07-26T18:00:00.000Z").expect("start timestamp"),
        ReceiptTimestamp::parse("2026-07-26T18:00:02.125Z").expect("terminal timestamp"),
    )
}

#[test]
fn completed_report_maps_to_the_merged_receipt_contract() {
    let report = report(
        HostPreparationExecutionDisposition::Completed,
        vec![record(
            "ensure-runner-group",
            ActionOutcome::Completed,
            Some(PRIVATE_MESSAGE),
        )],
    );
    let receipt = map_host_preparation_execution_receipt(
        &report,
        context("host-prepare-0123456789abcdef"),
    )
    .expect("receipt");

    assert_eq!(receipt.disposition(), ExecutionReceiptDisposition::Completed);
    assert_eq!(receipt.execution_id().as_str(), "host-prepare-0123456789abcdef");
    assert_eq!(receipt.summary().completed(), 1);
    assert_eq!(receipt.actions()[0].id(), "ensure-runner-group");
    assert_eq!(
        receipt.actions()[0].outcome(),
        ExecutionReceiptActionOutcome::Completed
    );
    assert_eq!(receipt.actions()[0].failure_code(), None);
    let ExecutionReceiptOperation::HostPreparation {
        repository,
        phase_id,
        ..
    } = receipt.operation();
    assert_eq!(repository.as_str(), "teamleaderleo/smolrunner");
    assert_eq!(phase_id.as_str(), "host-preparation-root-phase");

    let encoded = encode_execution_receipt(&receipt).expect("encoded receipt");
    for forbidden in [
        PRIVATE_PATH,
        PRIVATE_MESSAGE,
        "private summary",
        "private evidence",
        "executables",
        "preconditions",
        "message",
    ] {
        assert!(!encoded.contains(forbidden), "receipt leaked {forbidden}");
    }
}

#[test]
fn failed_report_maps_pending_actions_to_not_run_and_ignores_prose() {
    let mut report = report(
        HostPreparationExecutionDisposition::ActionFailed,
        vec![
            record("ensure-runner-group", ActionOutcome::Completed, None),
            record(
                "ensure-runner-user",
                ActionOutcome::Failed,
                Some(PRIVATE_MESSAGE),
            ),
            record("ensure-runner-home", ActionOutcome::Pending, None),
        ],
    );
    report.continuation_barriers = vec![barrier("unreached-barrier")];
    report.deferred_actions = vec![deferred("z-reobserve"), deferred("a-recover")];

    let receipt = map_host_preparation_execution_receipt(
        &report,
        context("host-prepare-1123456789abcdef"),
    )
    .expect("failed receipt");

    assert_eq!(
        receipt.disposition(),
        ExecutionReceiptDisposition::ActionFailed
    );
    assert_eq!(receipt.summary().completed(), 1);
    assert_eq!(receipt.summary().failed(), 1);
    assert_eq!(receipt.summary().not_run(), 1);
    assert_eq!(
        receipt.actions()[1].failure_code(),
        Some("host-preparation-action-failed")
    );
    assert_eq!(
        receipt.actions()[2].outcome(),
        ExecutionReceiptActionOutcome::NotRun
    );
    assert!(!receipt.continuation().fresh_observation_required());
    assert_eq!(
        receipt
            .continuation()
            .barriers()
            .collect::<Vec<_>>(),
        Vec::<&str>::new()
    );
    assert_eq!(
        receipt
            .continuation()
            .deferred_actions()
            .collect::<Vec<_>>(),
        ["a-recover", "z-reobserve"]
    );

    let encoded = encode_execution_receipt(&receipt).expect("encoded receipt");
    assert!(!encoded.contains(PRIVATE_MESSAGE));
    assert!(!encoded.contains("unreached-barrier"));
}

#[test]
fn fresh_observation_report_sorts_public_continuation_identities() {
    let mut report = report(
        HostPreparationExecutionDisposition::FreshObservationRequired,
        vec![record(
            "change-mappings",
            ActionOutcome::Completed,
            Some(PRIVATE_MESSAGE),
        )],
    );
    report.continuation_barriers = vec![barrier("z-runtime"), barrier("a-authority")];
    report.deferred_actions = vec![deferred("z-migration"), deferred("a-reobserve")];

    let receipt = map_host_preparation_execution_receipt(
        &report,
        context("host-prepare-2123456789abcdef"),
    )
    .expect("fresh-observation receipt");

    assert_eq!(
        receipt.disposition(),
        ExecutionReceiptDisposition::FreshObservationRequired
    );
    assert!(receipt.continuation().fresh_observation_required());
    assert_eq!(
        receipt.continuation().barriers().collect::<Vec<_>>(),
        ["a-authority", "z-runtime"]
    );
    assert_eq!(
        receipt
            .continuation()
            .deferred_actions()
            .collect::<Vec<_>>(),
        ["a-reobserve", "z-migration"]
    );

    let encoded = encode_execution_receipt(&receipt).expect("encoded receipt");
    assert!(!encoded.contains(PRIVATE_PATH));
    assert!(!encoded.contains("private barrier"));
    assert!(!encoded.contains("private deferred"));
}

#[test]
fn rollback_failure_uses_a_fixed_public_code() {
    let report = report(
        HostPreparationExecutionDisposition::ActionFailed,
        vec![record(
            "ensure-runner-user",
            ActionOutcome::RollbackFailed,
            Some(PRIVATE_MESSAGE),
        )],
    );
    let receipt = map_host_preparation_execution_receipt(
        &report,
        context("host-prepare-3123456789abcdef"),
    )
    .expect("rollback-failed receipt");

    assert_eq!(
        receipt.actions()[0].failure_code(),
        Some("host-preparation-rollback-failed")
    );
    assert!(!encode_execution_receipt(&receipt)
        .expect("encoded receipt")
        .contains(PRIVATE_MESSAGE));
}

#[test]
fn unsupported_schemas_and_invalid_repository_fail_closed() {
    let mut execution_schema = report(
        HostPreparationExecutionDisposition::Completed,
        vec![record("one", ActionOutcome::Completed, None)],
    );
    execution_schema.schema_version = 99;
    assert_eq!(
        map_host_preparation_execution_receipt(
            &execution_schema,
            context("host-prepare-4123456789abcdef")
        )
        .expect_err("execution schema")
        .kind(),
        HostPreparationReceiptErrorKind::UnsupportedExecutionSchema
    );

    let mut journal_schema = report(
        HostPreparationExecutionDisposition::Completed,
        vec![record("one", ActionOutcome::Completed, None)],
    );
    journal_schema.journal.schema_version = 99;
    assert_eq!(
        map_host_preparation_execution_receipt(
            &journal_schema,
            context("host-prepare-5123456789abcdef")
        )
        .expect_err("journal schema")
        .kind(),
        HostPreparationReceiptErrorKind::UnsupportedJournalSchema
    );

    let mut repository = report(
        HostPreparationExecutionDisposition::Completed,
        vec![record("one", ActionOutcome::Completed, None)],
    );
    repository.source = source("not a repository");
    assert_eq!(
        map_host_preparation_execution_receipt(
            &repository,
            context("host-prepare-6123456789abcdef")
        )
        .expect_err("repository identity")
        .kind(),
        HostPreparationReceiptErrorKind::InvalidRepositoryIdentity
    );
}

#[test]
fn in_progress_and_inconsistent_terminal_reports_are_rejected() {
    for outcome in [ActionOutcome::Executing, ActionOutcome::RollbackInProgress] {
        let report = report(
            HostPreparationExecutionDisposition::ActionFailed,
            vec![record("one", outcome, Some(PRIVATE_MESSAGE))],
        );
        assert_eq!(
            map_host_preparation_execution_receipt(
                &report,
                context("host-prepare-7123456789abcdef")
            )
            .expect_err("in-progress journal")
            .kind(),
            HostPreparationReceiptErrorKind::NonterminalJournal
        );
    }

    let pending_completed = report(
        HostPreparationExecutionDisposition::Completed,
        vec![record("one", ActionOutcome::Pending, None)],
    );
    assert_eq!(
        map_host_preparation_execution_receipt(
            &pending_completed,
            context("host-prepare-8123456789abcdef")
        )
        .expect_err("pending completed report")
        .kind(),
        HostPreparationReceiptErrorKind::InconsistentExecutionReport
    );

    let mut completed_with_deferred = report(
        HostPreparationExecutionDisposition::Completed,
        vec![record("one", ActionOutcome::Completed, None)],
    );
    completed_with_deferred.deferred_actions = vec![deferred("unexpected-deferred")];
    assert_eq!(
        map_host_preparation_execution_receipt(
            &completed_with_deferred,
            context("host-prepare-9123456789abcdef")
        )
        .expect_err("completed continuation")
        .kind(),
        HostPreparationReceiptErrorKind::InconsistentExecutionReport
    );
}

#[test]
fn duplicate_continuation_identity_is_rejected() {
    let mut report = report(
        HostPreparationExecutionDisposition::FreshObservationRequired,
        vec![record("one", ActionOutcome::Completed, None)],
    );
    report.continuation_barriers = vec![barrier("same"), barrier("same")];
    assert_eq!(
        map_host_preparation_execution_receipt(
            &report,
            context("host-prepare-a123456789abcdef")
        )
        .expect_err("duplicate barrier")
        .kind(),
        HostPreparationReceiptErrorKind::InconsistentExecutionReport
    );
}
