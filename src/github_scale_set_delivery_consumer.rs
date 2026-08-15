#![allow(dead_code)]

use std::fmt;

use sha2::{Digest, Sha256};

use crate::disposable_attempt_catalog::{
    DisposableAttemptCatalogAction, DisposableAttemptCatalogDocument, DisposableAttemptReservation,
};
use crate::disposable_attempt_state::DisposableAttemptState;
use crate::disposable_prepared_template::{
    DisposablePreparedTemplateIdentity, DisposablePreparedTemplateManifest,
};
use crate::disposable_worker_reconciler::{
    CapacityClaimId, DisposableAttemptId, DisposableVmId, DisposableWorkerResources,
};
use crate::execution_admission::EpochMillis;
use crate::github_scale_set_delivery::{
    ScaleSetDelivery, ScaleSetDeliveryJobEvidence, ScaleSetDeliveryLifecycleEvent,
};
use crate::github_scale_set_protocol::ScaleSetRunnerName;

const ATTEMPT_IDENTITY_DOMAIN: &[u8] = b"smolrunner.scale-set-delivery-attempt.v1\0";
const MAX_REPOSITORY_COMPONENT_BYTES: usize = 100;
const MAX_LABELS: usize = 32;
const DISPOSABLE_JOB_MAX_MILLIS: u64 = 6 * 60 * 60 * 1_000;

/// Exact enrolled inputs that may turn one retained Scale Set delivery into catalog state.
///
/// This policy is crate-private and contains no credential or process authority. A later service
/// transaction will reconstruct it from the validated operator enrollment while holding the
/// canonical mutation lock.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ScaleSetDeliveryConsumerPolicy {
    scale_set_id: u32,
    repository: String,
    owner: String,
    request_labels: Vec<String>,
    resources: DisposableWorkerResources,
    prepared_template_identity: DisposablePreparedTemplateIdentity,
}

impl ScaleSetDeliveryConsumerPolicy {
    pub(crate) fn new(
        scale_set_id: u32,
        repository: &str,
        owner: &str,
        request_labels: &[String],
        resources: DisposableWorkerResources,
        prepared_template: &DisposablePreparedTemplateManifest,
    ) -> Result<Self, ScaleSetDeliveryConsumerError> {
        let source_cpu_millis = prepared_template
            .source_cpu_count()
            .checked_mul(1_000)
            .ok_or_else(invalid_policy)?;
        if scale_set_id == 0
            || !bounded_token(repository, MAX_REPOSITORY_COMPONENT_BYTES)
            || !bounded_token(owner, MAX_REPOSITORY_COMPONENT_BYTES)
            || request_labels.is_empty()
            || request_labels.len() > MAX_LABELS
            || resources.cpu_millis() < source_cpu_millis
            || resources.memory_bytes() < prepared_template.source_memory_bytes()
            || resources.disk_bytes() < prepared_template.source_disk_bytes()
        {
            return Err(invalid_policy());
        }

        let mut labels = request_labels.to_vec();
        labels.sort();
        if labels
            .iter()
            .any(|label| !bounded_token(label, MAX_REPOSITORY_COMPONENT_BYTES))
            || labels.windows(2).any(|pair| pair[0] == pair[1])
        {
            return Err(invalid_policy());
        }

        Ok(Self {
            scale_set_id,
            repository: repository.to_owned(),
            owner: owner.to_owned(),
            request_labels: labels,
            resources,
            prepared_template_identity: prepared_template
                .identity()
                .map_err(|_| invalid_policy())?,
        })
    }
}

impl fmt::Debug for ScaleSetDeliveryConsumerPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScaleSetDeliveryConsumerPolicy")
            .field("scale_set_id", &self.scale_set_id)
            .field("repository", &"<enrolled>")
            .field("label_count", &self.request_labels.len())
            .field("resources", &self.resources)
            .field("prepared_template_identity", &"<private>")
            .finish()
    }
}

/// Apply every retained event in order to a private catalog snapshot.
///
/// This function performs no persistence or external action. Returning an error leaves the caller's
/// catalog unchanged. A later paired transaction is responsible for durably publishing the exact
/// returned successor before acknowledgement or acquisition.
pub(crate) fn reconcile_scale_set_delivery(
    policy: &ScaleSetDeliveryConsumerPolicy,
    delivery: &ScaleSetDelivery,
    catalog: &DisposableAttemptCatalogDocument,
    observed_at: EpochMillis,
) -> Result<DisposableAttemptCatalogDocument, ScaleSetDeliveryConsumerError> {
    let events = delivery
        .retained_events()
        .map_err(|_| consumer_error("delivery_consumer_evidence_invalid"))?;
    events
        .into_iter()
        .try_fold(catalog.clone(), |current, event| {
            reconcile_event(policy, &event, &current, observed_at)
        })
}

fn reconcile_event(
    policy: &ScaleSetDeliveryConsumerPolicy,
    event: &ScaleSetDeliveryLifecycleEvent,
    catalog: &DisposableAttemptCatalogDocument,
    observed_at: EpochMillis,
) -> Result<DisposableAttemptCatalogDocument, ScaleSetDeliveryConsumerError> {
    let job = event_job(event);
    validate_job(policy, job)?;
    let identities = derive_identities(policy, job)?;

    if matches!(event, ScaleSetDeliveryLifecycleEvent::Available { .. }) {
        let not_after = attempt_not_after(observed_at)?;
        if let Some(existing) = catalog.find_active_by_runner_request_id(job.runner_request_id) {
            validate_reservation(policy, existing, job, &identities)?;
            if existing.attempt().not_after() != not_after {
                return Err(consumer_error("delivery_consumer_identity_drift"));
            }
            return Ok(catalog.clone());
        }
        if let Some(existing) = catalog.find_tombstone_by_runner_request_id(job.runner_request_id) {
            validate_attempt(existing, job, &identities)?;
            if existing.not_after() != not_after {
                return Err(consumer_error("delivery_consumer_identity_drift"));
            }
            validate_tombstone_event(existing, event)?;
            return Ok(catalog.clone());
        }
        if catalog
            .host_usage()
            .map_err(|_| consumer_error("delivery_consumer_catalog_invalid"))?
            .workers()
            != 0
        {
            return Err(consumer_error("delivery_consumer_capacity_unavailable"));
        }
        let reservation = DisposableAttemptReservation::new(
            DisposableAttemptState::reserved(
                identities.attempt_id,
                identities.claim_id,
                identities.vm_id,
                identities.runner_name,
                job.runner_request_id,
                not_after,
            ),
            policy.resources,
            policy.prepared_template_identity.clone(),
        )
        .map_err(|_| consumer_error("delivery_consumer_reservation_invalid"))?;
        return catalog
            .reserve(reservation)
            .map_err(|_| consumer_error("delivery_consumer_reservation_failed"));
    }

    if let Some(existing) = catalog.find_active_by_runner_request_id(job.runner_request_id) {
        validate_reservation(policy, existing, job, &identities)?;
        return catalog
            .replace_attempt(
                existing.attempt().attempt_id(),
                existing.attempt().revision(),
                event_action(event)?,
            )
            .map_err(|_| consumer_error("delivery_consumer_event_conflict"));
    }
    if let Some(existing) = catalog.find_tombstone_by_runner_request_id(job.runner_request_id) {
        validate_attempt(existing, job, &identities)?;
        validate_tombstone_event(existing, event)?;
        return Ok(catalog.clone());
    }
    Err(consumer_error("delivery_consumer_attempt_missing"))
}

fn attempt_not_after(
    observed_at: EpochMillis,
) -> Result<EpochMillis, ScaleSetDeliveryConsumerError> {
    observed_at
        .get()
        .checked_add(DISPOSABLE_JOB_MAX_MILLIS)
        .and_then(|value| EpochMillis::new(value).ok())
        .ok_or_else(|| consumer_error("delivery_consumer_deadline_invalid"))
}

struct DerivedIdentities {
    attempt_id: DisposableAttemptId,
    claim_id: CapacityClaimId,
    vm_id: DisposableVmId,
    runner_name: ScaleSetRunnerName,
}

fn derive_identities(
    policy: &ScaleSetDeliveryConsumerPolicy,
    job: &ScaleSetDeliveryJobEvidence,
) -> Result<DerivedIdentities, ScaleSetDeliveryConsumerError> {
    let mut labels = job.request_labels.clone();
    labels.sort();
    let mut hasher = Sha256::new();
    hasher.update(ATTEMPT_IDENTITY_DOMAIN);
    hash_field(&mut hasher, &policy.scale_set_id.to_be_bytes());
    hash_field(&mut hasher, &policy.resources.cpu_millis().to_be_bytes());
    hash_field(&mut hasher, &policy.resources.memory_bytes().to_be_bytes());
    hash_field(&mut hasher, &policy.resources.disk_bytes().to_be_bytes());
    hash_field(
        &mut hasher,
        policy.prepared_template_identity.as_str().as_bytes(),
    );
    hash_field(&mut hasher, &job.runner_request_id.get().to_be_bytes());
    hash_field(&mut hasher, job.repository.as_bytes());
    hash_field(&mut hasher, job.owner.as_bytes());
    hash_field(&mut hasher, job.job_id.as_str().as_bytes());
    hash_field(&mut hasher, &job.workflow_run_id.to_be_bytes());
    for label in labels {
        hash_field(&mut hasher, label.as_bytes());
    }
    let digest = format!("{:x}", hasher.finalize());
    let short = &digest[..32];
    Ok(DerivedIdentities {
        attempt_id: DisposableAttemptId::parse(&format!("attempt-{short}"))
            .map_err(|_| consumer_error("delivery_consumer_identity_invalid"))?,
        claim_id: CapacityClaimId::parse(&format!("claim-{digest}"))
            .map_err(|_| consumer_error("delivery_consumer_identity_invalid"))?,
        vm_id: DisposableVmId::parse(&format!("worker-{short}"))
            .map_err(|_| consumer_error("delivery_consumer_identity_invalid"))?,
        runner_name: ScaleSetRunnerName::parse(&format!("smolrunner-{short}"))
            .map_err(|_| consumer_error("delivery_consumer_identity_invalid"))?,
    })
}

fn validate_job(
    policy: &ScaleSetDeliveryConsumerPolicy,
    job: &ScaleSetDeliveryJobEvidence,
) -> Result<(), ScaleSetDeliveryConsumerError> {
    let mut labels = job.request_labels.clone();
    labels.sort();
    if job.repository != policy.repository
        || job.owner != policy.owner
        || labels != policy.request_labels
    {
        return Err(consumer_error("delivery_consumer_job_not_enrolled"));
    }
    Ok(())
}

fn validate_reservation(
    policy: &ScaleSetDeliveryConsumerPolicy,
    reservation: &DisposableAttemptReservation,
    job: &ScaleSetDeliveryJobEvidence,
    identities: &DerivedIdentities,
) -> Result<(), ScaleSetDeliveryConsumerError> {
    validate_attempt(reservation.attempt(), job, identities)?;
    if reservation.resources() != policy.resources
        || reservation.prepared_template_identity() != &policy.prepared_template_identity
    {
        return Err(consumer_error("delivery_consumer_identity_drift"));
    }
    Ok(())
}

fn validate_attempt(
    attempt: &DisposableAttemptState,
    job: &ScaleSetDeliveryJobEvidence,
    identities: &DerivedIdentities,
) -> Result<(), ScaleSetDeliveryConsumerError> {
    if attempt.attempt_id() != &identities.attempt_id
        || attempt.capacity_claim_id() != &identities.claim_id
        || attempt.vm_id() != &identities.vm_id
        || attempt.runner_name() != &identities.runner_name
        || attempt.runner_request_id() != job.runner_request_id
        || attempt
            .github_job_id()
            .is_some_and(|job_id| job_id != &job.job_id)
    {
        return Err(consumer_error("delivery_consumer_identity_drift"));
    }
    Ok(())
}

fn event_job(event: &ScaleSetDeliveryLifecycleEvent) -> &ScaleSetDeliveryJobEvidence {
    match event {
        ScaleSetDeliveryLifecycleEvent::Available { job }
        | ScaleSetDeliveryLifecycleEvent::Assigned { job }
        | ScaleSetDeliveryLifecycleEvent::Started { job, .. }
        | ScaleSetDeliveryLifecycleEvent::Completed { job, .. } => job,
    }
}

fn event_action(
    event: &ScaleSetDeliveryLifecycleEvent,
) -> Result<DisposableAttemptCatalogAction, ScaleSetDeliveryConsumerError> {
    match event {
        ScaleSetDeliveryLifecycleEvent::Assigned { job } => Ok(
            DisposableAttemptCatalogAction::RecordAssigned(job.job_id.clone()),
        ),
        ScaleSetDeliveryLifecycleEvent::Started { job, runner } => {
            Ok(DisposableAttemptCatalogAction::RecordRunning {
                runner: runner.clone(),
                job_id: job.job_id.clone(),
            })
        }
        ScaleSetDeliveryLifecycleEvent::Completed {
            job,
            runner,
            result,
        } => Ok(DisposableAttemptCatalogAction::RecordTerminal {
            runner: runner.clone(),
            job_id: job.job_id.clone(),
            result: result.clone(),
        }),
        ScaleSetDeliveryLifecycleEvent::Available { .. } => {
            Err(consumer_error("delivery_consumer_event_invalid"))
        }
    }
}

fn validate_tombstone_event(
    attempt: &DisposableAttemptState,
    event: &ScaleSetDeliveryLifecycleEvent,
) -> Result<(), ScaleSetDeliveryConsumerError> {
    let valid = match event {
        ScaleSetDeliveryLifecycleEvent::Available { .. } => true,
        ScaleSetDeliveryLifecycleEvent::Assigned { job } => {
            attempt.github_job_id() == Some(&job.job_id)
        }
        ScaleSetDeliveryLifecycleEvent::Started { job, runner } => {
            attempt.github_job_id() == Some(&job.job_id)
                && attempt.runner_id() == Some(runner.id)
                && attempt.runner_name() == &runner.name
        }
        ScaleSetDeliveryLifecycleEvent::Completed {
            job,
            runner,
            result,
        } => {
            attempt.github_job_id() == Some(&job.job_id)
                && attempt.result() == Some(result)
                && runner.as_ref().is_none_or(|runner| {
                    attempt.runner_id() == Some(runner.id) && attempt.runner_name() == &runner.name
                })
        }
    };
    if valid {
        Ok(())
    } else {
        Err(consumer_error("delivery_consumer_event_conflict"))
    }
}

fn hash_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(value);
}

fn bounded_token(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScaleSetDeliveryConsumerError {
    code: &'static str,
}

impl ScaleSetDeliveryConsumerError {
    pub(crate) const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for ScaleSetDeliveryConsumerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScaleSetDeliveryConsumerError")
            .field("code", &self.code)
            .finish()
    }
}

impl fmt::Display for ScaleSetDeliveryConsumerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code)
    }
}

impl std::error::Error for ScaleSetDeliveryConsumerError {}

const fn consumer_error(code: &'static str) -> ScaleSetDeliveryConsumerError {
    ScaleSetDeliveryConsumerError { code }
}

const fn invalid_policy() -> ScaleSetDeliveryConsumerError {
    consumer_error("delivery_consumer_policy_invalid")
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::disposable_prepared_template::current_disposable_prepared_template;
    use crate::disposable_worker_reconciler::{DisposableAttemptPhase, DisposableVmIdentity};
    use crate::github_scale_set_bridge::{
        ScaleSetBridgeEvent, ScaleSetBridgeJobEvidence, ScaleSetBridgePoll, ScaleSetStatistics,
    };
    use crate::github_scale_set_protocol::{
        ScaleSetJobId, ScaleSetJobResult, ScaleSetRunnerId, ScaleSetRunnerReference,
    };

    fn policy() -> ScaleSetDeliveryConsumerPolicy {
        ScaleSetDeliveryConsumerPolicy::new(
            23,
            "project",
            "example",
            &["smolrunner".to_owned()],
            DisposableWorkerResources::new(2_000, 2 << 30, 20 << 30).unwrap(),
            &current_disposable_prepared_template().unwrap(),
        )
        .unwrap()
    }

    fn job(request_id: u64, job_id: &str) -> ScaleSetBridgeJobEvidence {
        ScaleSetBridgeJobEvidence {
            runner_request_id: request_id,
            repository: "project".to_owned(),
            owner: "example".to_owned(),
            job_id: ScaleSetJobId::parse(job_id).unwrap(),
            workflow_run_id: 99,
            request_labels: vec!["smolrunner".to_owned()],
        }
    }

    fn delivery(events: Vec<ScaleSetBridgeEvent>) -> ScaleSetDelivery {
        ScaleSetDelivery::from_bridge_poll(&ScaleSetBridgePoll::Message {
            message_id: 7,
            statistics: ScaleSetStatistics {
                available_jobs: 1,
                acquired_jobs: 0,
                assigned_jobs: 0,
                running_jobs: 0,
                registered_runners: 0,
                busy_runners: 0,
                idle_runners: 0,
            },
            events,
        })
        .unwrap()
        .unwrap()
    }

    fn observed_at() -> EpochMillis {
        EpochMillis::new(100_000).unwrap()
    }

    fn reserve() -> DisposableAttemptCatalogDocument {
        reconcile_scale_set_delivery(
            &policy(),
            &delivery(vec![ScaleSetBridgeEvent::Available(job(41, "job-1"))]),
            &DisposableAttemptCatalogDocument::empty(),
            observed_at(),
        )
        .unwrap()
    }

    #[test]
    fn available_delivery_reserves_once_with_exact_request_and_deadline() {
        let delivery = delivery(vec![ScaleSetBridgeEvent::Available(job(41, "job-1"))]);
        let reserved = reconcile_scale_set_delivery(
            &policy(),
            &delivery,
            &DisposableAttemptCatalogDocument::empty(),
            observed_at(),
        )
        .unwrap();
        assert_eq!(reserved.active().len(), 1);
        assert_eq!(reserved.active()[0].attempt().runner_request_id().get(), 41);
        assert_eq!(reserved.active()[0].attempt().not_after().get(), 21_700_000);
        assert_eq!(
            reconcile_scale_set_delivery(&policy(), &delivery, &reserved, observed_at()).unwrap(),
            reserved
        );
        assert_eq!(
            reconcile_scale_set_delivery(
                &policy(),
                &delivery,
                &reserved,
                EpochMillis::new(100_001).unwrap(),
            )
            .unwrap_err()
            .code(),
            "delivery_consumer_identity_drift"
        );
    }

    #[test]
    fn exact_request_binding_rejects_changed_job_evidence() {
        let reserved = reserve();
        let changed = delivery(vec![ScaleSetBridgeEvent::Assigned(job(41, "job-2"))]);
        assert_eq!(
            reconcile_scale_set_delivery(&policy(), &changed, &reserved, observed_at())
                .unwrap_err()
                .code(),
            "delivery_consumer_identity_drift"
        );
    }

    #[test]
    fn unknown_assignment_cannot_select_an_attempt() {
        let assigned = delivery(vec![ScaleSetBridgeEvent::Assigned(job(41, "job-1"))]);
        assert_eq!(
            reconcile_scale_set_delivery(
                &policy(),
                &assigned,
                &DisposableAttemptCatalogDocument::empty(),
                observed_at(),
            )
            .unwrap_err()
            .code(),
            "delivery_consumer_attempt_missing"
        );
    }

    #[test]
    fn ordered_lifecycle_events_advance_the_exact_request_owner() {
        let mut catalog = reserve();
        let attempt_id = catalog.active()[0].attempt().attempt_id().clone();
        let mut attempt_revision = catalog.active()[0].attempt().revision();
        for action in [
            DisposableAttemptCatalogAction::AuthorizeClone,
            DisposableAttemptCatalogAction::RecordCloneStarted,
        ] {
            catalog = catalog
                .replace_attempt(&attempt_id, attempt_revision, action)
                .unwrap();
            attempt_revision = catalog.active()[0].attempt().revision();
        }
        catalog = catalog
            .bind_vm_identity_after_clone(
                &attempt_id,
                attempt_revision,
                DisposableVmIdentity::parse(&format!("sha256:{}", "11".repeat(32))).unwrap(),
            )
            .unwrap();
        attempt_revision = catalog.active()[0].attempt().revision();
        catalog = catalog
            .replace_attempt(
                &attempt_id,
                attempt_revision,
                DisposableAttemptCatalogAction::BeginRegistration,
            )
            .unwrap();

        let runner = ScaleSetRunnerReference::new(
            ScaleSetRunnerId::new(501).unwrap(),
            catalog.active()[0].attempt().runner_name().clone(),
        );
        let lifecycle = delivery(vec![
            ScaleSetBridgeEvent::Assigned(job(41, "job-1")),
            ScaleSetBridgeEvent::Started {
                job: job(41, "job-1"),
                runner: runner.clone(),
            },
            ScaleSetBridgeEvent::Completed {
                job: job(41, "job-1"),
                runner: Some(runner),
                result: ScaleSetJobResult::parse("succeeded").unwrap(),
            },
        ]);
        let terminal = reconcile_scale_set_delivery(
            &policy(),
            &lifecycle,
            &catalog,
            EpochMillis::new(200_000).unwrap(),
        )
        .unwrap();
        let attempt = terminal.active()[0].attempt();
        assert_eq!(attempt.phase(), DisposableAttemptPhase::Terminal);
        assert_eq!(attempt.github_job_id().unwrap().as_str(), "job-1");
        assert_eq!(attempt.result().unwrap().as_str(), "succeeded");
        assert_eq!(attempt.not_after().get(), 21_700_000);
    }

    #[test]
    fn tombstone_replay_requires_the_exact_runner_name_for_start_and_completion() {
        let mut catalog = reserve();
        let attempt_id = catalog.active()[0].attempt().attempt_id().clone();
        let mut attempt_revision = catalog.active()[0].attempt().revision();
        for action in [
            DisposableAttemptCatalogAction::AuthorizeClone,
            DisposableAttemptCatalogAction::RecordCloneStarted,
        ] {
            catalog = catalog
                .replace_attempt(&attempt_id, attempt_revision, action)
                .unwrap();
            attempt_revision = catalog.active()[0].attempt().revision();
        }
        catalog = catalog
            .bind_vm_identity_after_clone(
                &attempt_id,
                attempt_revision,
                DisposableVmIdentity::parse(&format!("sha256:{}", "11".repeat(32))).unwrap(),
            )
            .unwrap();
        attempt_revision = catalog.active()[0].attempt().revision();
        catalog = catalog
            .replace_attempt(
                &attempt_id,
                attempt_revision,
                DisposableAttemptCatalogAction::BeginRegistration,
            )
            .unwrap();
        let runner = ScaleSetRunnerReference::new(
            ScaleSetRunnerId::new(501).unwrap(),
            catalog.active()[0].attempt().runner_name().clone(),
        );
        let terminal_delivery = delivery(vec![ScaleSetBridgeEvent::Completed {
            job: job(41, "job-1"),
            runner: Some(runner.clone()),
            result: ScaleSetJobResult::parse("succeeded").unwrap(),
        }]);
        catalog = reconcile_scale_set_delivery(
            &policy(),
            &terminal_delivery,
            &catalog,
            EpochMillis::new(200_000).unwrap(),
        )
        .unwrap();
        for action in [
            DisposableAttemptCatalogAction::BeginCleanup,
            DisposableAttemptCatalogAction::AdvanceCleanup(DisposableAttemptPhase::Deregistering),
            DisposableAttemptCatalogAction::AdvanceCleanup(DisposableAttemptPhase::Releasing),
            DisposableAttemptCatalogAction::AdvanceCleanup(DisposableAttemptPhase::Complete),
        ] {
            attempt_revision = catalog.active()[0].attempt().revision();
            catalog = catalog
                .replace_attempt(&attempt_id, attempt_revision, action)
                .unwrap();
        }
        attempt_revision = catalog.active()[0].attempt().revision();
        catalog = catalog
            .retire_complete(&attempt_id, attempt_revision)
            .unwrap();

        let changed_runner = ScaleSetRunnerReference::new(
            runner.id,
            ScaleSetRunnerName::parse("smolrunner-replacement").unwrap(),
        );
        let conflicting_start = delivery(vec![ScaleSetBridgeEvent::Started {
            job: job(41, "job-1"),
            runner: changed_runner.clone(),
        }]);
        let conflicting_completion = delivery(vec![ScaleSetBridgeEvent::Completed {
            job: job(41, "job-1"),
            runner: Some(changed_runner),
            result: ScaleSetJobResult::parse("succeeded").unwrap(),
        }]);
        for conflicting in [&conflicting_start, &conflicting_completion] {
            assert_eq!(
                reconcile_scale_set_delivery(
                    &policy(),
                    conflicting,
                    &catalog,
                    EpochMillis::new(300_000).unwrap(),
                )
                .unwrap_err()
                .code(),
                "delivery_consumer_event_conflict"
            );
        }
        assert_eq!(
            reconcile_scale_set_delivery(
                &policy(),
                &terminal_delivery,
                &catalog,
                EpochMillis::new(300_000).unwrap(),
            )
            .unwrap(),
            catalog
        );
    }

    #[test]
    fn a_second_available_job_fails_atomically_at_one_worker_capacity() {
        let two = delivery(vec![
            ScaleSetBridgeEvent::Available(job(41, "job-1")),
            ScaleSetBridgeEvent::Available(job(42, "job-2")),
        ]);
        let empty = DisposableAttemptCatalogDocument::empty();
        assert_eq!(
            reconcile_scale_set_delivery(&policy(), &two, &empty, observed_at())
                .unwrap_err()
                .code(),
            "delivery_consumer_capacity_unavailable"
        );
        assert!(empty.active().is_empty());
    }
}
