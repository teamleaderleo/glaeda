//! Reuse of a published reusable-state generation must pass the hot-state reuse ladder.
//!
//! `docs/REUSABLE_STATE_LIFECYCLE.md` owns the identity contract; `docs/BLAZINGLY_HOT.md` owns the
//! path-class sharing-mode policy. These tests prove the two are now one gate: every reuse that
//! `glaeda::reusable_state_lifecycle::evaluate_reusable_state_consumption` reports as a hit is also
//! an admitted hot-state candidate, and every unproven ladder dimension refuses reuse instead of
//! falling back to the narrower identity check.

use glaeda::artifact::{RepositoryRef, Sha256Digest};
use glaeda::hot_state_path_policy::{
    HotStateAdmissionMismatchField, HotStateAdmissionRefusal, HotStateCapabilityGenerationId,
    HotStateCapabilityObservation, HotStateForbiddenReason, HotStateQuarantineReason,
    HotStateSelection, HotStateSharingMode,
};
use glaeda::reusable_state_hot_state_policy::select_reusable_state_hot_state;
use glaeda::reusable_state_lifecycle::{
    ReusableStateArchitecture, ReusableStateClass, ReusableStateConsumerTrust,
    ReusableStateConsumptionDisposition, ReusableStateConsumptionMode,
    ReusableStateConsumptionPolicy, ReusableStateGeneration, ReusableStateGenerationId,
    ReusableStateIdentityContract, ReusableStateIntegrityState, ReusableStateLifecycle,
    ReusableStateMetrics, ReusableStateMissReason, ReusableStatePromotionPolicy,
    ReusableStatePublicationState, ReusableStatePublisherAuthority, ReusableStateRetentionFacts,
    evaluate_reusable_state_consumption,
};

type ContractMutation = fn(&mut ReusableStateIdentityContract);

fn digest(fill: char) -> Sha256Digest {
    Sha256Digest::parse(&format!("sha256:{}", String::from(fill).repeat(64)))
        .expect("canonical digest")
}

fn contract() -> ReusableStateIdentityContract {
    ReusableStateIdentityContract::new(
        ReusableStateClass::IncrementalBuildState,
        RepositoryRef::parse("teamleaderleo/glaeda").expect("repository"),
        ReusableStateArchitecture::parse("aarch64").expect("architecture"),
        digest('1'),
        digest('2'),
        Some(digest('3')),
        Some(digest('4')),
        Some(digest('5')),
        Some(digest('6')),
        digest('7'),
        digest('8'),
    )
}

fn metrics() -> ReusableStateMetrics {
    ReusableStateMetrics {
        lookups: 8,
        hits: 6,
        misses: 2,
        restore_duration_millis: 100,
        publication_duration_millis: 200,
        bytes_read: 1_024,
        bytes_written: 1_024,
        storage_size_bytes: 1_024_000,
        estimated_cold_work_millis: 40_000,
        estimated_warm_work_millis: 4_000,
        last_useful_hit_epoch_millis: Some(1_000),
        validation_failures: 0,
        reset_invalidation_count: 0,
        reset_invalidation_overhead_millis: 0,
        producer_successes: 4,
        successful_consumers: 4,
        semantic_mismatches: 0,
    }
}

fn retention() -> ReusableStateRetentionFacts {
    ReusableStateRetentionFacts {
        reconstructible: true,
        unique_local_work: false,
        must_retain: false,
        in_use_consumers: 0,
    }
}

fn generation_with(
    identity: ReusableStateIdentityContract,
    publication: ReusableStatePublicationState,
    integrity: ReusableStateIntegrityState,
    revalidation_required: bool,
    retention: ReusableStateRetentionFacts,
) -> ReusableStateGeneration {
    ReusableStateGeneration::candidate(
        ReusableStatePublisherAuthority::ReviewedTrustedPublisher,
        identity,
        ReusableStateGenerationId(digest('9')),
        publication,
        integrity,
        revalidation_required,
        metrics(),
        retention,
    )
    .expect("reviewed trusted publisher candidate")
}

fn validated() -> ReusableStateGeneration {
    generation_with(
        contract(),
        ReusableStatePublicationState::Complete,
        ReusableStateIntegrityState::Verified,
        false,
        retention(),
    )
    .transition(
        ReusableStateLifecycle::Validated,
        ReusableStatePromotionPolicy::conservative(),
    )
    .expect("validated generation")
}

fn capabilities(overlay: bool, private_empty: bool) -> HotStateCapabilityObservation {
    HotStateCapabilityObservation::new(
        HotStateCapabilityGenerationId::parse("reusable-state-capability-1").expect("generation"),
        overlay,
        false,
        private_empty,
        false,
        false,
    )
}

fn consumption_policy() -> ReusableStateConsumptionPolicy {
    ReusableStateConsumptionPolicy {
        low_trust_read_only_allowed: false,
    }
}

#[test]
fn exact_contract_on_an_overlay_capable_host_reuses_published_bytes() {
    let published = validated();
    let decision = select_reusable_state_hot_state(
        published.identity(),
        &published,
        &capabilities(true, true),
    )
    .expect("derivable hot-state context");

    assert_eq!(decision.refusal(), None);
    assert!(decision.reuse_admitted());
    assert!(decision.receipt().candidate_identity_match());
    assert_eq!(
        decision.receipt().selection(),
        HotStateSelection::Selected {
            mode: HotStateSharingMode::ImmutableOverlay,
            reused_state: true,
        }
    );

    assert_eq!(
        evaluate_reusable_state_consumption(
            published.identity(),
            &published,
            ReusableStateConsumerTrust::Trusted,
            consumption_policy(),
            &capabilities(true, true),
        ),
        ReusableStateConsumptionDisposition::Hit {
            mode: ReusableStateConsumptionMode::ReadOnly,
        }
    );
}

#[test]
fn every_reachable_ladder_dimension_refuses_reuse() {
    use HotStateAdmissionMismatchField as Field;

    let mutations: &[(Field, ContractMutation)] = &[
        (Field::Family, |contract| {
            contract.family_inputs_digest = digest('a');
        }),
        (Field::Binding, |contract| {
            contract.prepared_environment_generation = None;
        }),
        (Field::Project, |contract| {
            contract.repository = RepositoryRef::parse("teamleaderleo/other").expect("repository");
        }),
        (Field::PathClass, |contract| {
            contract.state_class = ReusableStateClass::CompilerCache;
        }),
        (Field::Source, |contract| {
            contract.dependency_lock_digest = Some(digest('b'));
        }),
        (Field::Toolchain, |contract| {
            contract.toolchain_generation = digest('c');
        }),
        (Field::Policy, |contract| {
            contract.build_configuration_digest = Some(digest('d'));
        }),
        (Field::Profile, |contract| {
            contract.compiler_flags_digest = Some(digest('e'));
        }),
        (Field::Validator, |contract| {
            contract.cache_schema_generation = digest('f');
        }),
        (Field::Platform, |contract| {
            contract.architecture = ReusableStateArchitecture::parse("x86-64").expect("arch");
        }),
        (Field::Platform, |contract| {
            contract.os_runtime_generation = digest('0');
        }),
    ];

    let published = validated();
    for (expected_field, mutate) in mutations {
        let mut required = contract();
        mutate(&mut required);

        let decision =
            select_reusable_state_hot_state(&required, &published, &capabilities(true, true))
                .expect("derivable hot-state context");
        assert_eq!(
            decision.refusal(),
            Some(HotStateAdmissionRefusal::Mismatch {
                field: *expected_field
            }),
            "{expected_field:?} did not refuse admission"
        );
        assert!(
            !decision.reuse_admitted(),
            "{expected_field:?} still reused published bytes"
        );
        assert!(!decision.receipt().candidate_identity_match());
        assert_eq!(
            decision.receipt().selection(),
            HotStateSelection::Selected {
                mode: HotStateSharingMode::PrivateEmpty,
                reused_state: false,
            }
        );

        assert!(
            matches!(
                evaluate_reusable_state_consumption(
                    &required,
                    &published,
                    ReusableStateConsumerTrust::Trusted,
                    consumption_policy(),
                    &capabilities(true, true),
                ),
                ReusableStateConsumptionDisposition::MissReset { .. }
            ),
            "{expected_field:?} still reported a hit"
        );
    }
}

#[test]
fn a_host_without_the_reviewed_sharing_mode_refuses_reuse_the_identity_check_would_allow() {
    let published = validated();

    let without_overlay = select_reusable_state_hot_state(
        published.identity(),
        &published,
        &capabilities(false, true),
    )
    .expect("derivable hot-state context");
    assert_eq!(without_overlay.refusal(), None);
    assert!(without_overlay.receipt().candidate_identity_match());
    assert!(!without_overlay.reuse_admitted());
    assert_eq!(
        without_overlay.receipt().selection(),
        HotStateSelection::Selected {
            mode: HotStateSharingMode::PrivateEmpty,
            reused_state: false,
        }
    );

    assert_eq!(
        evaluate_reusable_state_consumption(
            published.identity(),
            &published,
            ReusableStateConsumerTrust::Trusted,
            consumption_policy(),
            &capabilities(false, true),
        ),
        ReusableStateConsumptionDisposition::MissReset {
            reason: ReusableStateMissReason::HotStateReuseUnproven,
        }
    );

    let without_any_mode = select_reusable_state_hot_state(
        published.identity(),
        &published,
        &capabilities(false, false),
    )
    .expect("derivable hot-state context");
    assert!(!without_any_mode.reuse_admitted());
    assert_eq!(
        without_any_mode.receipt().selection(),
        HotStateSelection::Unavailable
    );
}

#[test]
fn unique_local_work_is_permanently_forbidden_from_reuse() {
    let published = generation_with(
        contract(),
        ReusableStatePublicationState::Complete,
        ReusableStateIntegrityState::Verified,
        false,
        ReusableStateRetentionFacts {
            reconstructible: false,
            unique_local_work: true,
            must_retain: true,
            in_use_consumers: 0,
        },
    );
    let decision = select_reusable_state_hot_state(
        published.identity(),
        &published,
        &capabilities(true, true),
    )
    .expect("derivable hot-state context");

    assert_eq!(
        decision.refusal(),
        Some(HotStateAdmissionRefusal::Forbidden {
            reason: HotStateForbiddenReason::UniqueLocalWork,
        })
    );
    assert!(!decision.reuse_admitted());
}

#[test]
fn incomplete_publication_and_unverified_integrity_quarantine_the_family() {
    let partial = generation_with(
        contract(),
        ReusableStatePublicationState::Partial,
        ReusableStateIntegrityState::Verified,
        false,
        retention(),
    );
    assert_eq!(
        select_reusable_state_hot_state(partial.identity(), &partial, &capabilities(true, true))
            .expect("derivable hot-state context")
            .refusal(),
        Some(HotStateAdmissionRefusal::QuarantineRequired {
            reason: HotStateQuarantineReason::IncompletePublication,
        })
    );

    let corrupt = generation_with(
        contract(),
        ReusableStatePublicationState::Complete,
        ReusableStateIntegrityState::Corrupt,
        false,
        retention(),
    );
    assert_eq!(
        select_reusable_state_hot_state(corrupt.identity(), &corrupt, &capabilities(true, true))
            .expect("derivable hot-state context")
            .refusal(),
        Some(HotStateAdmissionRefusal::QuarantineRequired {
            reason: HotStateQuarantineReason::IdentityOrIntegrityAmbiguous,
        })
    );

    let stale = generation_with(
        contract(),
        ReusableStatePublicationState::Complete,
        ReusableStateIntegrityState::Verified,
        true,
        retention(),
    );
    assert_eq!(
        select_reusable_state_hot_state(stale.identity(), &stale, &capabilities(true, true))
            .expect("derivable hot-state context")
            .refusal(),
        Some(HotStateAdmissionRefusal::QuarantineRequired {
            reason: HotStateQuarantineReason::RestoredUnobserved,
        })
    );
}

#[test]
fn a_repository_outside_the_canonical_project_form_refuses_reuse() {
    let published = validated();
    let mut required = contract();
    required.repository = RepositoryRef::parse("Teamleaderleo/Glaeda").expect("repository");

    assert!(
        select_reusable_state_hot_state(&required, &published, &capabilities(true, true)).is_err(),
        "a non-canonical project identity must not derive a reusable hot-state context"
    );
    assert!(matches!(
        evaluate_reusable_state_consumption(
            &required,
            &published,
            ReusableStateConsumerTrust::Trusted,
            consumption_policy(),
            &capabilities(true, true),
        ),
        ReusableStateConsumptionDisposition::MissReset { .. }
    ));
}

#[test]
fn the_selection_receipt_exposes_no_raw_identity_tokens() {
    let published = validated();
    let decision = select_reusable_state_hot_state(
        published.identity(),
        &published,
        &capabilities(true, true),
    )
    .expect("derivable hot-state context");
    let json = serde_json::to_string(decision.receipt()).expect("serializable receipt");

    assert!(json.contains("sha256:"));
    assert!(!json.contains("teamleaderleo/glaeda"));
    assert!(!json.contains("aarch64"));
    assert!(!json.contains(digest('1').as_str()));
    assert!(!json.contains(digest('8').as_str()));
}
