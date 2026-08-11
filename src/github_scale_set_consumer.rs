// The exact event applier is private until the M3 service owns its live clock and bridge session.
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
use crate::github_scale_set_bridge::{
    ScaleSetBridgeEvent, ScaleSetBridgeIdentity, ScaleSetBridgeJobEvidence,
};
use crate::github_scale_set_inbox::{PendingScaleSetMessage, ScaleSetAckReceipt};
use crate::github_scale_set_protocol::ScaleSetRunnerName;

const CLAIM_DOMAIN: &[u8] = b"smolrunner.scale-set-capacity-claim.v1\0";
const MAX_REPOSITORY_COMPONENT: usize = 100;
const MAX_LABELS: usize = 32;
const DISPOSABLE_JOB_MAX_MILLIS: u64 = 6 * 60 * 60 * 1_000;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ScaleSetConsumerPolicy {
    source_identity: ScaleSetBridgeIdentity,
    scale_set_id: u32,
    repository: String,
    owner: String,
    request_labels: Vec<String>,
    resources: DisposableWorkerResources,
    prepared_template_identity: DisposablePreparedTemplateIdentity,
}

impl ScaleSetConsumerPolicy {
    pub(crate) fn new(
        source_identity: ScaleSetBridgeIdentity,
        scale_set_id: u32,
        repository: &str,
        owner: &str,
        request_labels: &[String],
        resources: DisposableWorkerResources,
        prepared_template: &DisposablePreparedTemplateManifest,
    ) -> Result<Self, ScaleSetConsumerError> {
        if scale_set_id == 0
            || !bounded_token(repository, MAX_REPOSITORY_COMPONENT)
            || !bounded_token(owner, MAX_REPOSITORY_COMPONENT)
            || request_labels.is_empty()
            || request_labels.len() > MAX_LABELS
            || resources.cpu_millis() < prepared_template.source_cpu_count().saturating_mul(1_000)
            || resources.memory_bytes() < prepared_template.source_memory_bytes()
            || resources.disk_bytes() < prepared_template.source_disk_bytes()
        {
            return Err(ScaleSetConsumerError::new("consumer_policy_invalid"));
        }
        let mut labels = request_labels.to_vec();
        labels.sort();
        if labels
            .iter()
            .any(|label| !bounded_token(label, MAX_REPOSITORY_COMPONENT))
            || labels.windows(2).any(|pair| pair[0] == pair[1])
        {
            return Err(ScaleSetConsumerError::new("consumer_policy_invalid"));
        }
        Ok(Self {
            source_identity,
            scale_set_id,
            repository: repository.to_owned(),
            owner: owner.to_owned(),
            request_labels: labels,
            resources,
            prepared_template_identity: prepared_template
                .identity()
                .map_err(|_| ScaleSetConsumerError::new("consumer_policy_invalid"))?,
        })
    }

    pub(crate) const fn source_identity(&self) -> &ScaleSetBridgeIdentity {
        &self.source_identity
    }
}

impl fmt::Debug for ScaleSetConsumerPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScaleSetConsumerPolicy")
            .field("scale_set_id", &self.scale_set_id)
            .field("repository", &"<enrolled>")
            .field("label_count", &self.request_labels.len())
            .field("resources", &self.resources)
            .field("prepared_template_identity", &"<private>")
            .finish()
    }
}

pub(crate) fn apply_scale_set_event(
    policy: &ScaleSetConsumerPolicy,
    pending: &PendingScaleSetMessage,
    event: &ScaleSetBridgeEvent,
    catalog: &DisposableAttemptCatalogDocument,
    now: EpochMillis,
) -> Result<DisposableAttemptCatalogDocument, ScaleSetConsumerError> {
    let job = event_job(event);
    validate_job(policy, job)?;
    let identities = derive_identities(policy, job)?;

    if matches!(event, ScaleSetBridgeEvent::Available(_)) {
        if now < pending.observed_at() || now > pending.not_after() {
            return Err(ScaleSetConsumerError::new("consumer_admission_stale"));
        }
        // The inbox deadline bounds whether this service message is fresh enough to create a
        // reservation. It is not the workload lifetime. Derive the one-job hard ceiling from the
        // exact persisted message deadline so crash replay produces byte-identical state.
        let attempt_not_after = pending
            .not_after()
            .get()
            .checked_add(DISPOSABLE_JOB_MAX_MILLIS)
            .and_then(|value| EpochMillis::new(value).ok())
            .ok_or_else(|| ScaleSetConsumerError::new("consumer_attempt_deadline_invalid"))?;
        let reservation = DisposableAttemptReservation::new(
            DisposableAttemptState::reserved_for_job(
                identities.attempt_id.clone(),
                identities.claim_id.clone(),
                identities.vm_id,
                identities.runner_name,
                job.job_id.clone(),
                attempt_not_after,
            ),
            policy.resources,
            policy.prepared_template_identity.clone(),
        )
        .map_err(|_| ScaleSetConsumerError::new("consumer_reservation_invalid"))?;
        if let Some(existing) = find_active_by_claim(catalog, &identities.claim_id) {
            if existing == &reservation {
                return Ok(catalog.clone());
            }
            return Err(ScaleSetConsumerError::new("consumer_claim_drift"));
        }
        if find_tombstone_by_claim(catalog, &identities.claim_id).is_some() {
            return Err(ScaleSetConsumerError::new("consumer_claim_replayed"));
        }
        if catalog
            .host_usage()
            .map_err(|_| ScaleSetConsumerError::new("consumer_capacity_invalid"))?
            .workers()
            != 0
        {
            return Err(ScaleSetConsumerError::new("consumer_capacity_unavailable"));
        }
        return catalog
            .reserve(reservation)
            .map_err(|_| ScaleSetConsumerError::new("consumer_reservation_failed"));
    }

    if let Some(active) = find_active_by_claim(catalog, &identities.claim_id) {
        return catalog
            .replace_attempt(
                active.attempt().attempt_id(),
                active.attempt().revision(),
                event_action(event)?,
            )
            .map_err(|_| ScaleSetConsumerError::new("consumer_event_conflict"));
    }
    if let Some(tombstone) = find_tombstone_by_claim(catalog, &identities.claim_id) {
        validate_tombstone_event(tombstone, event)?;
        return Ok(catalog.clone());
    }
    Err(ScaleSetConsumerError::new("consumer_claim_missing"))
}

pub(crate) fn apply_scale_set_ack_outcome(
    policy: &ScaleSetConsumerPolicy,
    receipt: &ScaleSetAckReceipt,
    catalog: &DisposableAttemptCatalogDocument,
) -> Result<DisposableAttemptCatalogDocument, ScaleSetConsumerError> {
    if receipt.outcome_applied() {
        return Err(ScaleSetConsumerError::new(
            "consumer_ack_outcome_already_applied",
        ));
    }

    let mut next = catalog.clone();
    for event in receipt.events() {
        let ScaleSetBridgeEvent::Available(job) = event else {
            continue;
        };
        validate_job(policy, job)?;
        let identities = derive_identities(policy, job)?;
        let reservation = find_active_by_claim(&next, &identities.claim_id)
            .ok_or_else(|| ScaleSetConsumerError::new("consumer_ack_claim_missing"))?;
        validate_reservation(policy, reservation, &identities)?;

        let acquired = receipt
            .acquired_request_ids()
            .binary_search(&job.runner_request_id)
            .is_ok();
        match (acquired, reservation.attempt().phase()) {
            (true, crate::disposable_worker_reconciler::DisposableAttemptPhase::Reserved)
            | (
                false,
                crate::disposable_worker_reconciler::DisposableAttemptPhase::UnprovisionedReleasing,
            ) => {}
            (false, crate::disposable_worker_reconciler::DisposableAttemptPhase::Reserved) => {
                next = next
                    .replace_attempt(
                        &identities.attempt_id,
                        reservation.attempt().revision(),
                        DisposableAttemptCatalogAction::BeginUnprovisionedRelease,
                    )
                    .map_err(|_| ScaleSetConsumerError::new("consumer_ack_release_failed"))?;
            }
            _ => return Err(ScaleSetConsumerError::new("consumer_ack_claim_drift")),
        }
    }
    Ok(next)
}

struct DerivedIdentities {
    attempt_id: DisposableAttemptId,
    claim_id: CapacityClaimId,
    vm_id: DisposableVmId,
    runner_name: ScaleSetRunnerName,
}

fn validate_reservation(
    policy: &ScaleSetConsumerPolicy,
    reservation: &DisposableAttemptReservation,
    identities: &DerivedIdentities,
) -> Result<(), ScaleSetConsumerError> {
    let attempt = reservation.attempt();
    if attempt.attempt_id() != &identities.attempt_id
        || attempt.capacity_claim_id() != &identities.claim_id
        || attempt.vm_id() != &identities.vm_id
        || attempt.runner_name() != &identities.runner_name
        || reservation.resources() != policy.resources
        || reservation.prepared_template_identity() != &policy.prepared_template_identity
    {
        return Err(ScaleSetConsumerError::new("consumer_ack_claim_drift"));
    }
    Ok(())
}

fn derive_identities(
    policy: &ScaleSetConsumerPolicy,
    job: &ScaleSetBridgeJobEvidence,
) -> Result<DerivedIdentities, ScaleSetConsumerError> {
    let mut labels = job.request_labels.clone();
    labels.sort();
    let mut hasher = Sha256::new();
    hasher.update(CLAIM_DOMAIN);
    hash_field(&mut hasher, policy.source_identity.as_str().as_bytes());
    hash_field(&mut hasher, &policy.scale_set_id.to_be_bytes());
    hash_field(&mut hasher, &job.runner_request_id.to_be_bytes());
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
            .map_err(|_| ScaleSetConsumerError::new("consumer_identity_invalid"))?,
        claim_id: CapacityClaimId::parse(&format!("claim-{digest}"))
            .map_err(|_| ScaleSetConsumerError::new("consumer_identity_invalid"))?,
        vm_id: DisposableVmId::parse(&format!("worker-{short}"))
            .map_err(|_| ScaleSetConsumerError::new("consumer_identity_invalid"))?,
        runner_name: ScaleSetRunnerName::parse(&format!("smolrunner-{short}"))
            .map_err(|_| ScaleSetConsumerError::new("consumer_identity_invalid"))?,
    })
}

fn validate_job(
    policy: &ScaleSetConsumerPolicy,
    job: &ScaleSetBridgeJobEvidence,
) -> Result<(), ScaleSetConsumerError> {
    let mut labels = job.request_labels.clone();
    labels.sort();
    if job.repository != policy.repository
        || job.owner != policy.owner
        || labels != policy.request_labels
    {
        return Err(ScaleSetConsumerError::new("consumer_job_not_enrolled"));
    }
    Ok(())
}

fn event_job(event: &ScaleSetBridgeEvent) -> &ScaleSetBridgeJobEvidence {
    match event {
        ScaleSetBridgeEvent::Available(job) | ScaleSetBridgeEvent::Assigned(job) => job,
        ScaleSetBridgeEvent::Started { job, .. } | ScaleSetBridgeEvent::Completed { job, .. } => {
            job
        }
    }
}

fn event_action(
    event: &ScaleSetBridgeEvent,
) -> Result<DisposableAttemptCatalogAction, ScaleSetConsumerError> {
    match event {
        ScaleSetBridgeEvent::Assigned(job) => Ok(DisposableAttemptCatalogAction::RecordAssigned(
            job.job_id.clone(),
        )),
        ScaleSetBridgeEvent::Started { job, runner } => {
            Ok(DisposableAttemptCatalogAction::RecordRunning {
                runner: runner.clone(),
                job_id: job.job_id.clone(),
            })
        }
        ScaleSetBridgeEvent::Completed {
            job,
            runner,
            result,
        } => Ok(DisposableAttemptCatalogAction::RecordTerminal {
            runner: runner.clone(),
            job_id: job.job_id.clone(),
            result: result.clone(),
        }),
        ScaleSetBridgeEvent::Available(_) => {
            Err(ScaleSetConsumerError::new("consumer_event_invalid"))
        }
    }
}

fn find_active_by_claim<'a>(
    catalog: &'a DisposableAttemptCatalogDocument,
    claim_id: &CapacityClaimId,
) -> Option<&'a DisposableAttemptReservation> {
    catalog
        .active()
        .iter()
        .find(|entry| entry.attempt().capacity_claim_id() == claim_id)
}

fn find_tombstone_by_claim<'a>(
    catalog: &'a DisposableAttemptCatalogDocument,
    claim_id: &CapacityClaimId,
) -> Option<&'a DisposableAttemptState> {
    catalog
        .tombstones()
        .iter()
        .find(|attempt| attempt.capacity_claim_id() == claim_id)
}

fn validate_tombstone_event(
    attempt: &DisposableAttemptState,
    event: &ScaleSetBridgeEvent,
) -> Result<(), ScaleSetConsumerError> {
    match event {
        ScaleSetBridgeEvent::Assigned(job) if attempt.github_job_id() == Some(&job.job_id) => {
            Ok(())
        }
        ScaleSetBridgeEvent::Started { job, runner }
            if attempt.github_job_id() == Some(&job.job_id)
                && attempt.runner_id() == Some(runner.id) =>
        {
            Ok(())
        }
        ScaleSetBridgeEvent::Completed {
            job,
            runner,
            result,
        } if attempt.github_job_id() == Some(&job.job_id)
            && attempt.result() == Some(result)
            && runner
                .as_ref()
                .is_none_or(|runner| attempt.runner_id() == Some(runner.id)) =>
        {
            Ok(())
        }
        _ => Err(ScaleSetConsumerError::new("consumer_event_conflict")),
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

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ScaleSetConsumerError {
    code: &'static str,
}

impl ScaleSetConsumerError {
    const fn new(code: &'static str) -> Self {
        Self { code }
    }

    pub(crate) const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for ScaleSetConsumerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScaleSetConsumerError")
            .field("code", &self.code)
            .finish()
    }
}

impl fmt::Display for ScaleSetConsumerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code)
    }
}

impl std::error::Error for ScaleSetConsumerError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disposable_attempt_catalog::DisposableAttemptCatalogDocument;
    use crate::disposable_prepared_template::current_disposable_prepared_template;
    use crate::github_scale_set_protocol::{ScaleSetJobId, ScaleSetJobResult};

    fn identity() -> ScaleSetBridgeIdentity {
        ScaleSetBridgeIdentity::parse(&format!("sha256:{}", "33".repeat(32))).unwrap()
    }

    fn policy() -> ScaleSetConsumerPolicy {
        ScaleSetConsumerPolicy::new(
            identity(),
            23,
            "project",
            "example",
            &["smolrunner".to_owned()],
            DisposableWorkerResources::new(2_000, 2 << 30, 20 << 30).unwrap(),
            &current_disposable_prepared_template().unwrap(),
        )
        .unwrap()
    }

    fn job(job_id: &str) -> ScaleSetBridgeJobEvidence {
        ScaleSetBridgeJobEvidence {
            runner_request_id: 41,
            repository: "project".to_owned(),
            owner: "example".to_owned(),
            job_id: ScaleSetJobId::parse(job_id).unwrap(),
            workflow_run_id: 99,
            request_labels: vec!["smolrunner".to_owned()],
        }
    }

    fn pending(event: ScaleSetBridgeEvent) -> PendingScaleSetMessage {
        let document = crate::github_scale_set_inbox::ScaleSetInboxDocument::empty(identity())
            .record(
                7,
                EpochMillis::new(100_000).unwrap(),
                EpochMillis::new(120_000).unwrap(),
                vec![event],
            )
            .unwrap();
        document.pending().unwrap().clone()
    }

    fn ack_receipt(acquired_request_ids: Vec<u64>) -> ScaleSetAckReceipt {
        let recorded = crate::github_scale_set_inbox::ScaleSetInboxDocument::empty(identity())
            .record(
                7,
                EpochMillis::new(100_000).unwrap(),
                EpochMillis::new(120_000).unwrap(),
                vec![ScaleSetBridgeEvent::Available(job("job-1"))],
            )
            .unwrap();
        let applied = recorded.mark_next_event_applied(7, 0).unwrap();
        let started = applied.begin_ack(7).unwrap();
        started
            .complete_ack(7, acquired_request_ids)
            .unwrap()
            .last_ack()
            .unwrap()
            .clone()
    }

    #[test]
    fn available_job_reserves_once_with_exact_durable_claim() {
        let policy = policy();
        let event = ScaleSetBridgeEvent::Available(job("job-1"));
        let pending = pending(event.clone());
        let empty = DisposableAttemptCatalogDocument::empty();
        let reserved = apply_scale_set_event(
            &policy,
            &pending,
            &event,
            &empty,
            EpochMillis::new(100_001).unwrap(),
        )
        .unwrap();
        assert_eq!(reserved.active().len(), 1);
        assert_eq!(reserved.active()[0].attempt().not_after().get(), 21_720_000);
        assert_eq!(
            apply_scale_set_event(
                &policy,
                &pending,
                &event,
                &reserved,
                EpochMillis::new(100_002).unwrap(),
            )
            .unwrap(),
            reserved
        );
    }

    #[test]
    fn changed_job_or_expired_admission_cannot_reuse_request_id() {
        let policy = policy();
        let event = ScaleSetBridgeEvent::Available(job("job-1"));
        let pending = pending(event.clone());
        let reserved = apply_scale_set_event(
            &policy,
            &pending,
            &event,
            &DisposableAttemptCatalogDocument::empty(),
            EpochMillis::new(100_001).unwrap(),
        )
        .unwrap();
        let changed = ScaleSetBridgeEvent::Assigned(job("job-2"));
        assert_eq!(
            apply_scale_set_event(
                &policy,
                &pending,
                &changed,
                &reserved,
                EpochMillis::new(100_002).unwrap(),
            )
            .unwrap_err()
            .code(),
            "consumer_claim_missing"
        );
        assert_eq!(
            apply_scale_set_event(
                &policy,
                &pending,
                &event,
                &DisposableAttemptCatalogDocument::empty(),
                EpochMillis::new(120_001).unwrap(),
            )
            .unwrap_err()
            .code(),
            "consumer_admission_stale"
        );
    }

    #[test]
    fn acknowledgement_keeps_acquired_and_releases_unacquired_reservations() {
        let policy = policy();
        let event = ScaleSetBridgeEvent::Available(job("job-1"));
        let pending = pending(event.clone());
        let reserved = apply_scale_set_event(
            &policy,
            &pending,
            &event,
            &DisposableAttemptCatalogDocument::empty(),
            EpochMillis::new(100_001).unwrap(),
        )
        .unwrap();

        assert_eq!(
            apply_scale_set_ack_outcome(&policy, &ack_receipt(vec![41]), &reserved).unwrap(),
            reserved
        );

        let released =
            apply_scale_set_ack_outcome(&policy, &ack_receipt(Vec::new()), &reserved).unwrap();
        assert_eq!(
            released.active()[0].attempt().phase(),
            crate::disposable_worker_reconciler::DisposableAttemptPhase::UnprovisionedReleasing
        );
        assert_eq!(
            apply_scale_set_ack_outcome(&policy, &ack_receipt(Vec::new()), &released).unwrap(),
            released
        );
    }

    #[test]
    fn acquired_job_can_cancel_before_clone_without_gaining_cleanup_authority() {
        let policy = policy();
        let available = ScaleSetBridgeEvent::Available(job("job-1"));
        let available_pending = pending(available.clone());
        let reserved = apply_scale_set_event(
            &policy,
            &available_pending,
            &available,
            &DisposableAttemptCatalogDocument::empty(),
            EpochMillis::new(100_001).unwrap(),
        )
        .unwrap();
        assert_eq!(
            reserved.active()[0]
                .attempt()
                .github_job_id()
                .unwrap()
                .as_str(),
            "job-1"
        );
        let acquired =
            apply_scale_set_ack_outcome(&policy, &ack_receipt(vec![41]), &reserved).unwrap();

        let assigned = ScaleSetBridgeEvent::Assigned(job("job-1"));
        let after_assigned = apply_scale_set_event(
            &policy,
            &pending(assigned.clone()),
            &assigned,
            &acquired,
            EpochMillis::new(100_002).unwrap(),
        )
        .unwrap();
        assert_eq!(after_assigned, acquired);

        let canceled = ScaleSetBridgeEvent::Completed {
            job: job("job-1"),
            runner: None,
            result: ScaleSetJobResult::parse("canceled").unwrap(),
        };
        let releasing = apply_scale_set_event(
            &policy,
            &pending(canceled.clone()),
            &canceled,
            &after_assigned,
            EpochMillis::new(100_003).unwrap(),
        )
        .unwrap();
        let attempt = releasing.active()[0].attempt();
        assert_eq!(
            attempt.phase(),
            crate::disposable_worker_reconciler::DisposableAttemptPhase::UnprovisionedReleasing
        );
        assert!(attempt.vm_identity().is_none());
        assert!(attempt.runner_id().is_none());
        assert_eq!(attempt.result().unwrap().as_str(), "canceled");
        assert!(attempt.begin_cleanup().is_err());
        let complete = attempt.complete_unprovisioned().unwrap();
        assert_eq!(
            complete.phase(),
            crate::disposable_worker_reconciler::DisposableAttemptPhase::Complete
        );
        assert_eq!(complete.result().unwrap().as_str(), "canceled");
    }

    #[test]
    fn preclone_completion_must_be_runnerless_and_canceled() {
        let policy = policy();
        let available = ScaleSetBridgeEvent::Available(job("job-1"));
        let reserved = apply_scale_set_event(
            &policy,
            &pending(available.clone()),
            &available,
            &DisposableAttemptCatalogDocument::empty(),
            EpochMillis::new(100_001).unwrap(),
        )
        .unwrap();
        let failed = ScaleSetBridgeEvent::Completed {
            job: job("job-1"),
            runner: None,
            result: ScaleSetJobResult::parse("failed").unwrap(),
        };
        assert_eq!(
            apply_scale_set_event(
                &policy,
                &pending(failed.clone()),
                &failed,
                &reserved,
                EpochMillis::new(100_002).unwrap(),
            )
            .unwrap_err()
            .code(),
            "consumer_event_conflict"
        );
    }
}
