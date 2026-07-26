use smolrunner::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use smolrunner::execution_admission::{
    EpochMillis, ExecutionAdmissionIdentity, ExecutionAdmissionInput, ExecutionAdmissionRecord,
    ExecutionAdmissionState, ExecutionRequestId, ExecutionResourceLimits,
    FallbackProfileEligibility, HostCapacityObservation, ReservationEvidence,
    ReservationGeneration, ReservationId, RunnerProfileId,
};
use smolrunner::personal_worker_queue::{
    PersonalWorkerActiveReservation, PersonalWorkerCacheAccessMode, PersonalWorkerCacheLeaseState,
    PersonalWorkerCacheNamespace, PersonalWorkerCancellationState, PersonalWorkerJobRequest,
    PersonalWorkerPendingProfileChange, PersonalWorkerPriority, PersonalWorkerProfile,
    PersonalWorkerQueueEntryState, PersonalWorkerQueueGeneration, PersonalWorkerQueueInput,
    PersonalWorkerSourceIdentity, evaluate_personal_worker_queue,
};
use smolrunner::verification_profile::{CacheId, VerificationProfileId};

const GIB: u64 = 1_024 * 1_024 * 1_024;

fn time(value: u64) -> EpochMillis {
    EpochMillis::new(value).expect("time")
}

fn limits(cpu_millis: u32, memory_gib: u64) -> ExecutionResourceLimits {
    ExecutionResourceLimits::new(cpu_millis, memory_gib * GIB, 2_048).expect("limits")
}

fn source(repository: &str, digit: char) -> PersonalWorkerSourceIdentity {
    PersonalWorkerSourceIdentity::new(
        RepositoryRef::parse(repository).expect("repository"),
        CommitId::parse(&digit.to_string().repeat(40)).expect("commit"),
        GitTreeId::parse(&digit.to_string().repeat(40)).expect("tree"),
    )
}

fn identity(id: &str) -> ExecutionAdmissionIdentity {
    ExecutionAdmissionIdentity::new(
        ExecutionRequestId::parse(id).expect("request ID"),
        VerificationProfileId::parse("smolrunner.required").expect("profile"),
        RunnerProfileId::parse("personal-lima-work").expect("runner profile"),
    )
}

fn namespace(repository: &str) -> PersonalWorkerCacheNamespace {
    PersonalWorkerCacheNamespace::RepositoryBuild {
        cache_id: CacheId::parse("build-cache").expect("cache ID"),
        repository: RepositoryRef::parse(repository).expect("cache repository"),
        namespace_digest: Sha256Digest::parse(&format!("sha256:{}", "ab".repeat(32)))
            .expect("digest"),
    }
}

fn request(
    id: &str,
    repository: &str,
    digit: char,
    priority: PersonalWorkerPriority,
    submitted_at: u64,
    requested_limits: ExecutionResourceLimits,
    cache_access: PersonalWorkerCacheAccessMode,
) -> PersonalWorkerJobRequest {
    PersonalWorkerJobRequest {
        identity: identity(id),
        source: source(repository, digit),
        priority,
        requested_limits,
        cache_namespace: namespace(repository),
        cache_access,
        submitted_at: time(submitted_at),
        operator_deadline: None,
        cancellation: PersonalWorkerCancellationState::Active,
        fallback_eligibility: FallbackProfileEligibility::ineligible(),
    }
}

fn active(
    id: &str,
    repository: &str,
    digit: char,
    observed_at: u64,
    applied_limits: ExecutionResourceLimits,
    cache_access: PersonalWorkerCacheAccessMode,
) -> PersonalWorkerActiveReservation {
    let request = request(
        id,
        repository,
        digit,
        PersonalWorkerPriority::Normal,
        observed_at - 60_000,
        applied_limits,
        cache_access,
    );
    let admission = ExecutionAdmissionRecord::from_input(ExecutionAdmissionInput {
        identity: request.identity.clone(),
        state: ExecutionAdmissionState::Running,
        observed_at: time(observed_at),
        requested_limits: applied_limits,
        host_capacity: Some(HostCapacityObservation::new(
            time(observed_at),
            limits(8_000, 10),
        )),
        applied_limits: Some(applied_limits),
        queue_position: None,
        reservation: Some(ReservationEvidence::new(
            ReservationId::parse(&format!("reservation-{id}")).expect("reservation"),
            ReservationGeneration::new(1).expect("generation"),
            time(observed_at - 30_000),
            time(observed_at + 3_600_000),
        )),
        acknowledgement: None,
        fallback_eligibility: FallbackProfileEligibility::ineligible(),
        unavailable_reason: None,
    })
    .expect("active admission");
    PersonalWorkerActiveReservation {
        request,
        admission,
        started_at: Some(time(observed_at - 20_000)),
    }
}

fn input(
    generation: u64,
    observed_at: u64,
    queued: Vec<PersonalWorkerJobRequest>,
    active: Vec<PersonalWorkerActiveReservation>,
) -> PersonalWorkerQueueInput {
    PersonalWorkerQueueInput {
        generation: PersonalWorkerQueueGeneration::new(generation).expect("queue generation"),
        observed_at: time(observed_at),
        current_profile: if active.is_empty() {
            PersonalWorkerProfile::Interactive
        } else {
            PersonalWorkerProfile::Work
        },
        last_activity_at: time(observed_at - 1_000),
        queued,
        active,
        pending_profile_change: None,
    }
}

#[test]
fn priority_aging_promotes_old_background_work_without_losing_fifo() {
    let observed_at = 4_000_000;
    let old_background = request(
        "old-background",
        "teamleaderleo/smolrunner",
        'a',
        PersonalWorkerPriority::Background,
        observed_at - 2 * 30 * 60 * 1_000,
        limits(3_000, 3),
        PersonalWorkerCacheAccessMode::Read,
    );
    let recent_normal = request(
        "recent-normal",
        "example/other",
        'b',
        PersonalWorkerPriority::Normal,
        observed_at - 60_000,
        limits(3_000, 3),
        PersonalWorkerCacheAccessMode::Read,
    );

    let decision = evaluate_personal_worker_queue(
        &input(1, observed_at, vec![recent_normal, old_background], vec![]),
        None,
    )
    .expect("decision");

    assert_eq!(decision.selected.len(), 2);
    assert_eq!(decision.selected[0].request_id.as_str(), "old-background");
    assert_eq!(decision.selected[0].effective_priority_rank, 2);
}

#[test]
fn repository_fairness_prefers_an_unrepresented_repository() {
    let observed_at = 5_000_000;
    let running = active(
        "running-a",
        "teamleaderleo/smolrunner",
        'a',
        observed_at,
        limits(2_000, 2),
        PersonalWorkerCacheAccessMode::Read,
    );
    let same_repository = request(
        "queued-a",
        "teamleaderleo/smolrunner",
        'b',
        PersonalWorkerPriority::Normal,
        observed_at - 120_000,
        limits(2_000, 2),
        PersonalWorkerCacheAccessMode::Read,
    );
    let other_repository = request(
        "queued-b",
        "example/other",
        'c',
        PersonalWorkerPriority::Normal,
        observed_at - 60_000,
        limits(2_000, 2),
        PersonalWorkerCacheAccessMode::Read,
    );

    let decision = evaluate_personal_worker_queue(
        &input(
            1,
            observed_at,
            vec![same_repository, other_repository],
            vec![running],
        ),
        None,
    )
    .expect("decision");

    assert_eq!(decision.selected.len(), 1);
    assert_eq!(decision.selected[0].request_id.as_str(), "queued-b");
}

#[test]
fn one_heavy_job_excludes_other_work_while_two_light_jobs_can_share() {
    let observed_at = 6_000_000;
    let heavy = request(
        "heavy",
        "example/heavy",
        'a',
        PersonalWorkerPriority::Interactive,
        observed_at - 1_000,
        limits(6_000, 7),
        PersonalWorkerCacheAccessMode::Read,
    );
    let light = request(
        "light",
        "example/light",
        'b',
        PersonalWorkerPriority::Normal,
        observed_at - 2_000,
        limits(1_000, 1),
        PersonalWorkerCacheAccessMode::Read,
    );
    let heavy_decision =
        evaluate_personal_worker_queue(&input(1, observed_at, vec![light, heavy], vec![]), None)
            .expect("heavy decision");
    assert_eq!(heavy_decision.selected.len(), 1);
    assert_eq!(heavy_decision.selected[0].request_id.as_str(), "heavy");

    let light_a = request(
        "light-a",
        "example/a",
        'c',
        PersonalWorkerPriority::Normal,
        observed_at - 2_000,
        limits(3_000, 3),
        PersonalWorkerCacheAccessMode::Read,
    );
    let light_b = request(
        "light-b",
        "example/b",
        'd',
        PersonalWorkerPriority::Normal,
        observed_at - 1_000,
        limits(3_000, 3),
        PersonalWorkerCacheAccessMode::Read,
    );
    let light_decision = evaluate_personal_worker_queue(
        &input(1, observed_at, vec![light_a, light_b], vec![]),
        None,
    )
    .expect("light decision");
    assert_eq!(light_decision.selected.len(), 2);
}

#[test]
fn aggregate_reservations_preserve_the_fixed_guest_and_cache_reserve() {
    let observed_at = 7_000_000;
    let oversized = request(
        "oversized",
        "example/large",
        'a',
        PersonalWorkerPriority::Interactive,
        observed_at - 1_000,
        limits(7_001, 8),
        PersonalWorkerCacheAccessMode::Read,
    );
    let error =
        evaluate_personal_worker_queue(&input(1, observed_at, vec![oversized], vec![]), None)
            .expect_err("capacity refusal");
    assert_eq!(error.code, "personal_worker_reserve_violation");
}

#[test]
fn cache_writers_are_exclusive_but_readers_can_share() {
    let observed_at = 8_000_000;
    let writer = request(
        "writer",
        "teamleaderleo/smolrunner",
        'a',
        PersonalWorkerPriority::Interactive,
        observed_at - 2_000,
        limits(2_000, 2),
        PersonalWorkerCacheAccessMode::Write,
    );
    let reader = request(
        "reader",
        "teamleaderleo/smolrunner",
        'b',
        PersonalWorkerPriority::Normal,
        observed_at - 1_000,
        limits(2_000, 2),
        PersonalWorkerCacheAccessMode::Read,
    );
    let decision =
        evaluate_personal_worker_queue(&input(1, observed_at, vec![writer, reader], vec![]), None)
            .expect("writer decision");
    assert_eq!(decision.selected.len(), 1);
    let blocked = decision
        .visibility
        .iter()
        .find(|entry| entry.request_id.as_str() == "reader")
        .expect("reader visibility");
    assert_eq!(
        blocked.cache_lease,
        PersonalWorkerCacheLeaseState::BlockedByWriter
    );

    let read_a = request(
        "read-a",
        "teamleaderleo/smolrunner",
        'c',
        PersonalWorkerPriority::Normal,
        observed_at - 2_000,
        limits(2_000, 2),
        PersonalWorkerCacheAccessMode::Read,
    );
    let read_b = request(
        "read-b",
        "teamleaderleo/smolrunner",
        'd',
        PersonalWorkerPriority::Normal,
        observed_at - 1_000,
        limits(2_000, 2),
        PersonalWorkerCacheAccessMode::Read,
    );
    let read_decision =
        evaluate_personal_worker_queue(&input(1, observed_at, vec![read_a, read_b], vec![]), None)
            .expect("reader decision");
    assert_eq!(read_decision.selected.len(), 2);
}

#[test]
fn cancelled_work_remains_visible_and_never_receives_a_reservation() {
    let observed_at = 9_000_000;
    let mut cancelled = request(
        "cancelled",
        "example/cancelled",
        'a',
        PersonalWorkerPriority::Interactive,
        observed_at - 10_000,
        limits(2_000, 2),
        PersonalWorkerCacheAccessMode::Exclusive,
    );
    cancelled.cancellation = PersonalWorkerCancellationState::Cancelled {
        cancelled_at: time(observed_at - 1_000),
    };
    let decision =
        evaluate_personal_worker_queue(&input(1, observed_at, vec![cancelled], vec![]), None)
            .expect("cancelled decision");
    assert!(decision.selected.is_empty());
    assert_eq!(
        decision.visibility[0].state,
        PersonalWorkerQueueEntryState::Cancelled
    );
    assert_eq!(
        decision.visibility[0].cache_lease,
        PersonalWorkerCacheLeaseState::Available
    );
}

#[test]
fn queue_generation_must_advance_exactly_once() {
    let observed_at = 10_000_000;
    let first = evaluate_personal_worker_queue(&input(1, observed_at, vec![], vec![]), None)
        .expect("first generation");
    let error =
        evaluate_personal_worker_queue(&input(3, observed_at + 1, vec![], vec![]), Some(&first))
            .expect_err("generation gap");
    assert_eq!(error.code, "stale_or_skipped_queue_generation");
}

#[test]
fn desired_profile_uses_work_interactive_and_stopped_cooldowns() {
    let observed_at = 11_000_000;
    let queued = request(
        "new-work",
        "example/work",
        'a',
        PersonalWorkerPriority::Normal,
        observed_at - 1_000,
        limits(2_000, 2),
        PersonalWorkerCacheAccessMode::Read,
    );
    let mut work_input = input(1, observed_at, vec![queued], vec![]);
    work_input.current_profile = PersonalWorkerProfile::Interactive;
    work_input.pending_profile_change = Some(PersonalWorkerPendingProfileChange {
        target: PersonalWorkerProfile::Stopped,
        requested_at: time(observed_at - 500),
    });
    let work = evaluate_personal_worker_queue(&work_input, None).expect("work profile");
    assert_eq!(work.desired_profile, PersonalWorkerProfile::Work);
    assert!(work.cancel_pending_downscale);
    assert!(work.profile_change_permitted);

    let mut interactive_input = input(1, observed_at, vec![], vec![]);
    interactive_input.current_profile = PersonalWorkerProfile::Work;
    interactive_input.last_activity_at = time(observed_at - 10 * 60 * 1_000);
    let interactive =
        evaluate_personal_worker_queue(&interactive_input, None).expect("interactive profile");
    assert_eq!(
        interactive.desired_profile,
        PersonalWorkerProfile::Interactive
    );

    let mut stopped_input = input(1, observed_at, vec![], vec![]);
    stopped_input.current_profile = PersonalWorkerProfile::Interactive;
    stopped_input.last_activity_at = time(observed_at - 31 * 60 * 1_000);
    let stopped = evaluate_personal_worker_queue(&stopped_input, None).expect("stopped profile");
    assert_eq!(stopped.desired_profile, PersonalWorkerProfile::Stopped);
}

#[test]
fn visibility_is_bounded_and_contains_no_private_execution_material() {
    let observed_at = 12_000_000;
    let queued = request(
        "visible",
        "teamleaderleo/smolrunner",
        'a',
        PersonalWorkerPriority::Normal,
        observed_at - 1_000,
        limits(2_000, 2),
        PersonalWorkerCacheAccessMode::Read,
    );
    let decision =
        evaluate_personal_worker_queue(&input(1, observed_at, vec![queued], vec![]), None)
            .expect("decision");
    let encoded = serde_json::to_string(&decision).expect("JSON");
    for private in [
        "/Users/operator/private",
        "/var/lib/private-cache",
        "TOKEN=private",
        "PRIVATE_STDOUT",
        "PRIVATE_STDERR",
        "cargo test --all-targets",
    ] {
        assert!(!encoded.contains(private));
    }
    assert!(encoded.contains("teamleaderleo/smolrunner"));
    assert!(encoded.contains("smolrunner.required"));
}
