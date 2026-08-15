use crate::disposable_attempt_catalog::{
    DisposableAttemptCatalogDocument, DisposableAttemptReservation,
};
use crate::disposable_attempt_state::DisposableAttemptState;
use crate::github_scale_set_protocol::{ScaleSetJobId, ScaleSetRunnerRequestId};

impl DisposableAttemptCatalogDocument {
    /// Find the one active attempt that owns this exact pre-assignment runner request.
    ///
    /// Catalog validation enforces request-ID uniqueness across active attempts and replay
    /// tombstones, so service events never need to infer ownership from event order or a mutable
    /// runner name.
    #[must_use]
    pub fn find_active_by_runner_request_id(
        &self,
        request_id: ScaleSetRunnerRequestId,
    ) -> Option<&DisposableAttemptReservation> {
        self.active()
            .iter()
            .find(|reservation| reservation.attempt().runner_request_id() == request_id)
    }

    /// Find the completed replay tombstone that retained this exact runner request identity.
    #[must_use]
    pub fn find_tombstone_by_runner_request_id(
        &self,
        request_id: ScaleSetRunnerRequestId,
    ) -> Option<&DisposableAttemptState> {
        self.tombstones()
            .iter()
            .find(|attempt| attempt.runner_request_id() == request_id)
    }

    /// Find the one active attempt already bound to this exact GitHub job identity.
    ///
    /// Catalog validation enforces job-ID uniqueness across active attempts and replay tombstones,
    /// so a successful lookup identifies one durable owner without ordering or runner-name guesses.
    #[must_use]
    pub fn find_active_by_job_id(
        &self,
        job_id: &ScaleSetJobId,
    ) -> Option<&DisposableAttemptReservation> {
        self.active()
            .iter()
            .find(|reservation| reservation.attempt().github_job_id() == Some(job_id))
    }

    /// Find the one completed replay tombstone already bound to this exact GitHub job identity.
    #[must_use]
    pub fn find_tombstone_by_job_id(
        &self,
        job_id: &ScaleSetJobId,
    ) -> Option<&DisposableAttemptState> {
        self.tombstones()
            .iter()
            .find(|attempt| attempt.github_job_id() == Some(job_id))
    }
}

#[cfg(test)]
mod tests {
    use crate::disposable_attempt_catalog::{
        DisposableAttemptCatalogDocument, decode_disposable_attempt_catalog,
    };
    use crate::disposable_attempt_state::{
        DisposableAttemptState, encode_disposable_attempt_state,
    };
    use crate::disposable_prepared_template::current_disposable_prepared_template;
    use crate::disposable_worker_reconciler::{
        CapacityClaimId, DisposableAttemptId, DisposableAttemptPhase, DisposableVmId,
        DisposableVmIdentity,
    };
    use crate::execution_admission::EpochMillis;
    use crate::github_scale_set_protocol::{
        ScaleSetJobId, ScaleSetJobResult, ScaleSetRunnerId, ScaleSetRunnerName,
        ScaleSetRunnerReference, ScaleSetRunnerRequestId,
    };

    fn assigned_attempt(label: &str, job_id: &ScaleSetJobId) -> DisposableAttemptState {
        let reserved = DisposableAttemptState::reserved(
            DisposableAttemptId::parse(&format!("attempt-{label}")).expect("attempt id"),
            CapacityClaimId::parse(&format!("claim-{label}")).expect("capacity claim"),
            DisposableVmId::parse(&format!("vm-{label}")).expect("vm id"),
            ScaleSetRunnerName::parse(&format!("smol-{label}")).expect("runner name"),
            ScaleSetRunnerRequestId::new(if label == "active" { 41 } else { 42 })
                .expect("runner request id"),
            EpochMillis::new(50_000).expect("expiry"),
        );
        let authorized = reserved.authorize_clone().expect("authorize clone");
        let started = authorized
            .record_clone_started()
            .expect("record clone start");
        let identity_byte = if label == "active" { "11" } else { "22" };
        let bound = started
            .bind_vm_identity_after_clone(
                DisposableVmIdentity::parse(&format!("sha256:{}", identity_byte.repeat(32)))
                    .expect("VM identity"),
            )
            .expect("bind VM identity");
        let registering = bound.begin_registration().expect("begin registration");
        registering
            .record_assigned(job_id.clone())
            .expect("record assignment")
    }

    fn completed_attempt(label: &str, job_id: &ScaleSetJobId) -> DisposableAttemptState {
        let assigned = assigned_attempt(label, job_id);
        let runner = ScaleSetRunnerReference::new(
            ScaleSetRunnerId::new(501).expect("runner id"),
            assigned.runner_name().clone(),
        );
        let terminal = assigned
            .record_terminal(
                Some(&runner),
                job_id.clone(),
                ScaleSetJobResult::parse("succeeded").expect("job result"),
            )
            .expect("record terminal");
        let destroying = terminal.begin_cleanup().expect("begin cleanup");
        let deregistering = destroying
            .advance_cleanup(DisposableAttemptPhase::Deregistering)
            .expect("advance deregistration");
        let releasing = deregistering
            .advance_cleanup(DisposableAttemptPhase::Releasing)
            .expect("advance release");
        releasing
            .advance_cleanup(DisposableAttemptPhase::Complete)
            .expect("complete cleanup")
    }

    fn canonical_embedded_attempt_json(attempt: &DisposableAttemptState) -> String {
        let encoded = encode_disposable_attempt_state(attempt).expect("encode attempt");
        let value: serde_json::Value =
            serde_json::from_slice(&encoded).expect("parse encoded attempt value");
        serde_json::to_string(&value).expect("canonicalize embedded attempt value")
    }

    fn canonical_catalog(
        active: &DisposableAttemptState,
        tombstone: &DisposableAttemptState,
    ) -> DisposableAttemptCatalogDocument {
        // The catalog codec embeds attempt documents through `serde_json::Value`, which applies
        // the value-map key ordering before the outer catalog is serialized. Mirror that exact
        // boundary here instead of splicing the standalone attempt codec bytes directly.
        let active_json = canonical_embedded_attempt_json(active);
        let tombstone_json = canonical_embedded_attempt_json(tombstone);
        let template = current_disposable_prepared_template()
            .expect("prepared template")
            .identity()
            .expect("prepared-template identity");
        let revision = 1 + active.revision().get() + tombstone.revision().get() + 1;
        let bytes = format!(
            "{{\"schema_version\":7,\"revision\":{revision},\"active\":[{{\"attempt\":{active_json},\"resources\":{{\"cpu_millis\":2000,\"memory_bytes\":2147483648,\"disk_bytes\":21474836480}},\"prepared_template_digest\":\"{}\"}}],\"tombstones\":[{tombstone_json}]}}",
            template.as_str()
        )
        .into_bytes();
        decode_disposable_attempt_catalog(&bytes).expect("decode canonical catalog")
    }

    #[test]
    fn exact_job_lookup_distinguishes_active_attempts_from_replay_tombstones() {
        let active_job = ScaleSetJobId::parse("job-active").expect("active job id");
        let completed_job = ScaleSetJobId::parse("job-completed").expect("completed job id");
        let unknown_job = ScaleSetJobId::parse("job-unknown").expect("unknown job id");
        let active = assigned_attempt("active", &active_job);
        let tombstone = completed_attempt("completed", &completed_job);
        let document = canonical_catalog(&active, &tombstone);

        assert_eq!(
            document
                .find_active_by_job_id(&active_job)
                .expect("active job owner")
                .attempt(),
            &active
        );
        assert!(document.find_tombstone_by_job_id(&active_job).is_none());
        assert!(document.find_active_by_job_id(&completed_job).is_none());
        assert_eq!(
            document
                .find_tombstone_by_job_id(&completed_job)
                .expect("completed job owner"),
            &tombstone
        );
        assert!(document.find_active_by_job_id(&unknown_job).is_none());
        assert!(document.find_tombstone_by_job_id(&unknown_job).is_none());

        let active_request = active.runner_request_id();
        let completed_request = tombstone.runner_request_id();
        let unknown_request = ScaleSetRunnerRequestId::new(99).expect("unknown request id");
        assert_eq!(
            document
                .find_active_by_runner_request_id(active_request)
                .expect("active request owner")
                .attempt(),
            &active
        );
        assert!(
            document
                .find_tombstone_by_runner_request_id(active_request)
                .is_none()
        );
        assert!(
            document
                .find_active_by_runner_request_id(completed_request)
                .is_none()
        );
        assert_eq!(
            document
                .find_tombstone_by_runner_request_id(completed_request)
                .expect("completed request owner"),
            &tombstone
        );
        assert!(
            document
                .find_active_by_runner_request_id(unknown_request)
                .is_none()
        );
        assert!(
            document
                .find_tombstone_by_runner_request_id(unknown_request)
                .is_none()
        );
    }
}
