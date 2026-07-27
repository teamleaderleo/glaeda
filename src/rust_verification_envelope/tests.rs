use super::*;
use crate::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use crate::verification_profile::{
    CacheId, CapabilityId, PackageId, RepositoryCommandId, RepositoryCommandIdentity, TargetName,
    VerificationProfileId,
};

const GIB: u64 = 1024 * 1024 * 1024;

fn digest(character: char) -> Sha256Digest {
    Sha256Digest::parse(&format!("sha256:{}", character.to_string().repeat(64))).expect("digest")
}

fn source() -> RustSourceIdentity {
    RustSourceIdentity::new(
        RepositoryRef::parse("teamleaderleo/codex").expect("repository"),
        CommitId::parse(&"a1".repeat(20)).expect("commit"),
        GitTreeId::parse(&"b2".repeat(20)).expect("tree"),
    )
}

fn command(repository: &str) -> RepositoryCommandIdentity {
    RepositoryCommandIdentity::new(
        RepositoryRef::parse(repository).expect("repository"),
        RepositoryCommandId::parse("codex.core.library-test.v1").expect("command"),
        digest('c'),
    )
}

fn cargo() -> RustCargoContract {
    RustCargoContract::new(
        RustTargetTriple::parse("aarch64-unknown-linux-gnu").expect("target"),
        RustCargoProfile::Test,
        RustFeatureSelection::Default,
        true,
        digest('d'),
        RustTargetCacheIdentity::new(
            CacheId::parse("codex-cargo-target").expect("cache"),
            digest('e'),
        ),
        vec![CapabilityId::parse("cargo").expect("cargo")],
    )
    .expect("cargo contract")
}

fn resources(backend: RustTestBackend) -> RustResourceEnvelope {
    RustResourceEnvelope::new(
        PersonalWorkerProfile::Work,
        4_000,
        6 * GIB,
        4 * GIB,
        2 * GIB,
        5 * GIB,
        3_600,
        RustConcurrencyEnvelope::new(2, backend, 0).expect("concurrency"),
    )
    .expect("resources")
}

fn library_scope() -> RustVerificationScope {
    RustVerificationScope::LibraryTests {
        package: PackageId::parse("codex-core").expect("package"),
        filter: Some(RustTestFilter::parse("orphan_session").expect("filter")),
    }
}

fn envelope(
    scope: RustVerificationScope,
    resources: RustResourceEnvelope,
    retry: RustRetryPolicy,
) -> Result<RustVerificationEnvelope, RustVerificationEnvelopeError> {
    RustVerificationEnvelope::new(
        VerificationProfileId::parse("codex.focused-library").expect("profile"),
        source(),
        command("teamleaderleo/codex"),
        scope,
        cargo(),
        resources,
        retry,
    )
}

#[test]
fn focused_library_scope_is_exact_and_path_free() {
    let result = envelope(
        library_scope(),
        resources(RustTestBackend::Libtest { test_threads: 1 }),
        RustRetryPolicy::None,
    )
    .expect("envelope");
    let json = serde_json::to_string(&result).expect("json");

    assert!(json.contains("library_tests"));
    assert!(json.contains("codex-core"));
    assert!(json.contains("cargo_build_jobs"));
    assert!(json.contains("test_threads"));
    assert!(!json.contains("integration_test"));
    assert_eq!(
        result.schema_version(),
        RUST_VERIFICATION_ENVELOPE_SCHEMA_VERSION
    );
    assert_eq!(result.profile_id().as_str(), "codex.focused-library");
    assert_eq!(result.source().repository.as_str(), "teamleaderleo/codex");
    assert_eq!(
        result.command().repository().as_str(),
        "teamleaderleo/codex"
    );
    assert_eq!(
        result.cargo().target_triple().as_str(),
        "aarch64-unknown-linux-gnu"
    );
    assert_eq!(
        result.resources().required_worker_profile(),
        PersonalWorkerProfile::Work
    );
    assert_eq!(result.resources().reserved_cpu_millis(), 4_000);
    assert_eq!(result.resources().reserved_memory_bytes(), 6 * GIB);
    assert_eq!(result.resources().minimum_available_memory_bytes(), 4 * GIB);
    assert_eq!(result.resources().minimum_available_swap_bytes(), 2 * GIB);
    assert_eq!(result.resources().estimated_peak_memory_bytes(), 5 * GIB);
    assert_eq!(result.resources().maximum_execution_seconds(), 3_600);
    assert!(matches!(result.retry(), RustRetryPolicy::None));
    for private in ["/home/", "CARGO_HOME", "RUSTFLAGS", "--lib", "cargo test"] {
        assert!(!json.contains(private));
    }
}

#[test]
fn exact_integration_target_remains_distinct_from_package_and_workspace() {
    let integration = envelope(
        RustVerificationScope::IntegrationTest {
            package: PackageId::parse("codex-core").expect("package"),
            target: TargetName::parse("code_mode_orphan_sessions").expect("target"),
            filter: None,
        },
        resources(RustTestBackend::Libtest { test_threads: 1 }),
        RustRetryPolicy::None,
    )
    .expect("integration");
    let package = envelope(
        RustVerificationScope::PackageTests {
            package: PackageId::parse("codex-core").expect("package"),
            targets: RustTargetSelection {
                library: true,
                binaries: RustNamedTargetSelection::None,
                integration_tests: RustNamedTargetSelection::All,
                examples: RustNamedTargetSelection::None,
                benches: RustNamedTargetSelection::None,
                doctests: false,
                build_scripts: true,
            },
        },
        resources(RustTestBackend::Nextest {
            test_threads: 2,
            filterset: None,
        }),
        RustRetryPolicy::None,
    )
    .expect("package");

    assert_ne!(integration.scope(), package.scope());
    assert!(matches!(
        integration.scope(),
        RustVerificationScope::IntegrationTest { .. }
    ));
    assert!(matches!(
        package.scope(),
        RustVerificationScope::PackageTests { .. }
    ));
}

#[test]
fn test_filter_never_invents_or_widens_target_scope() {
    let library = library_scope();
    let integration = RustVerificationScope::IntegrationTest {
        package: PackageId::parse("codex-core").expect("package"),
        target: TargetName::parse("all").expect("target"),
        filter: Some(RustTestFilter::parse("same-name").expect("filter")),
    };

    assert!(matches!(
        library,
        RustVerificationScope::LibraryTests {
            filter: Some(_),
            ..
        }
    ));
    assert_ne!(library, integration);
}

#[test]
fn duplicate_features_targets_and_capabilities_fail_closed() {
    let feature = RustFeatureName::parse("unstable").expect("feature");
    let duplicate_features = RustCargoContract::new(
        RustTargetTriple::parse("aarch64-unknown-linux-gnu").expect("target"),
        RustCargoProfile::Test,
        RustFeatureSelection::DefaultPlus {
            features: vec![feature.clone(), feature],
        },
        true,
        digest('d'),
        RustTargetCacheIdentity::new(CacheId::parse("cache").expect("cache"), digest('e')),
        vec![],
    )
    .expect_err("duplicate feature");
    assert_eq!(duplicate_features.code, "duplicate_feature");

    let target = TargetName::parse("all").expect("target");
    let duplicate_targets = RustTargetSelection {
        library: false,
        binaries: RustNamedTargetSelection::None,
        integration_tests: RustNamedTargetSelection::Named {
            targets: vec![target.clone(), target],
        },
        examples: RustNamedTargetSelection::None,
        benches: RustNamedTargetSelection::None,
        doctests: false,
        build_scripts: false,
    };
    let error = envelope(
        RustVerificationScope::PackageTests {
            package: PackageId::parse("codex-core").expect("package"),
            targets: duplicate_targets,
        },
        resources(RustTestBackend::Libtest { test_threads: 1 }),
        RustRetryPolicy::None,
    )
    .expect_err("duplicate targets");
    assert_eq!(error.code, "duplicate_named_target");

    let capability = CapabilityId::parse("cargo").expect("capability");
    let duplicate_capabilities = RustCargoContract::new(
        RustTargetTriple::parse("aarch64-unknown-linux-gnu").expect("target"),
        RustCargoProfile::Test,
        RustFeatureSelection::Default,
        true,
        digest('d'),
        RustTargetCacheIdentity::new(CacheId::parse("cache").expect("cache"), digest('e')),
        vec![capability.clone(), capability],
    )
    .expect_err("duplicate capabilities");
    assert_eq!(duplicate_capabilities.code, "duplicate_capability");
}

#[test]
fn source_command_repository_drift_is_refused() {
    let error = RustVerificationEnvelope::new(
        VerificationProfileId::parse("codex.focused-library").expect("profile"),
        source(),
        command("other/project"),
        library_scope(),
        cargo(),
        resources(RustTestBackend::Libtest { test_threads: 1 }),
        RustRetryPolicy::None,
    )
    .expect_err("repository drift");
    assert_eq!(error.code, "source_command_repository_mismatch");
}

#[test]
fn stopped_overcommitted_and_uncovered_memory_envelopes_are_refused() {
    let concurrency =
        RustConcurrencyEnvelope::new(2, RustTestBackend::Libtest { test_threads: 1 }, 0)
            .expect("concurrency");
    let stopped = RustResourceEnvelope::new(
        PersonalWorkerProfile::Stopped,
        1_000,
        2 * GIB,
        GIB,
        GIB,
        2 * GIB,
        60,
        concurrency.clone(),
    )
    .expect_err("stopped");
    assert_eq!(stopped.code, "stopped_worker_profile");

    let interactive_overcommit = RustResourceEnvelope::new(
        PersonalWorkerProfile::Interactive,
        1_000,
        2 * GIB,
        GIB,
        GIB,
        GIB,
        60,
        concurrency.clone(),
    )
    .expect_err("interactive overcommit");
    assert_eq!(interactive_overcommit.code, "worker_resource_overcommit");

    RustResourceEnvelope::new(
        PersonalWorkerProfile::Interactive,
        1_000,
        GIB,
        GIB,
        0,
        GIB,
        60,
        RustConcurrencyEnvelope::new(1, RustTestBackend::Libtest { test_threads: 1 }, 0)
            .expect("interactive concurrency"),
    )
    .expect("bounded interactive envelope");

    let overcommit = RustResourceEnvelope::new(
        PersonalWorkerProfile::Work,
        PERSONAL_WORKER_SCHEDULABLE_CPU_MILLIS + 1,
        2 * GIB,
        GIB,
        GIB,
        2 * GIB,
        60,
        concurrency.clone(),
    )
    .expect_err("overcommit");
    assert_eq!(overcommit.code, "worker_resource_overcommit");

    let concurrency_overcommit = RustResourceEnvelope::new(
        PersonalWorkerProfile::Work,
        2_000,
        4 * GIB,
        2 * GIB,
        2 * GIB,
        3 * GIB,
        60,
        RustConcurrencyEnvelope::new(3, RustTestBackend::Libtest { test_threads: 1 }, 0)
            .expect("wide concurrency"),
    )
    .expect_err("concurrency overcommit");
    assert_eq!(
        concurrency_overcommit.code,
        "concurrency_exceeds_cpu_reservation"
    );

    let uncovered = RustResourceEnvelope::new(
        PersonalWorkerProfile::Work,
        1_000,
        4 * GIB,
        GIB,
        0,
        2 * GIB,
        60,
        RustConcurrencyEnvelope::new(1, RustTestBackend::Libtest { test_threads: 1 }, 0)
            .expect("uncovered concurrency"),
    )
    .expect_err("uncovered");
    assert_eq!(uncovered.code, "uncovered_peak_memory");
}

#[test]
fn cargo_and_test_concurrency_are_independent_and_backend_bound() {
    let concurrency = RustConcurrencyEnvelope::new(
        4,
        RustTestBackend::Nextest {
            test_threads: 2,
            filterset: Some(RustNextestFilterset::parse("package(codex-core)").expect("filterset")),
        },
        1,
    )
    .expect("concurrency");
    assert_eq!(concurrency.cargo_build_jobs(), 4);
    assert_eq!(concurrency.test_backend().test_threads(), Some(2));

    let missing = envelope(
        library_scope(),
        resources(RustTestBackend::None),
        RustRetryPolicy::None,
    )
    .expect_err("missing backend");
    assert_eq!(missing.code, "test_backend_scope_mismatch");

    let non_test = envelope(
        RustVerificationScope::Check {
            packages: RustPackageSelection::One {
                package: PackageId::parse("codex-core").expect("package"),
            },
            targets: RustTargetSelection::library_only(),
        },
        resources(RustTestBackend::Libtest { test_threads: 1 }),
        RustRetryPolicy::None,
    )
    .expect_err("unexpected backend");
    assert_eq!(non_test.code, "test_backend_scope_mismatch");
}

#[test]
fn sole_retry_can_only_lower_concurrency() {
    let lower = RustRetryPolicy::OneLowerConcurrency {
        policy_id: RustRetryPolicyId::parse("codex.low-memory.v1").expect("policy"),
        concurrency: RustRetryConcurrency::new(1, Some(1), 0).expect("retry"),
    };
    let result = envelope(
        library_scope(),
        resources(RustTestBackend::Libtest { test_threads: 2 }),
        lower,
    )
    .expect("lower retry");
    let RustRetryPolicy::OneLowerConcurrency { concurrency, .. } = result.retry() else {
        panic!("lower retry policy");
    };
    assert_eq!(concurrency.cargo_build_jobs(), 1);
    assert_eq!(concurrency.test_threads(), Some(1));
    assert_eq!(concurrency.heavy_test_thread_reservations(), 0);

    let same = RustRetryPolicy::OneLowerConcurrency {
        policy_id: RustRetryPolicyId::parse("same.v1").expect("policy"),
        concurrency: RustRetryConcurrency::new(2, Some(1), 0).expect("retry"),
    };
    let error = envelope(
        library_scope(),
        resources(RustTestBackend::Libtest { test_threads: 1 }),
        same,
    )
    .expect_err("same concurrency");
    assert_eq!(error.code, "non_lower_concurrency_retry");

    let higher = RustRetryPolicy::OneLowerConcurrency {
        policy_id: RustRetryPolicyId::parse("higher.v1").expect("policy"),
        concurrency: RustRetryConcurrency::new(3, Some(1), 0).expect("retry"),
    };
    let error = envelope(
        library_scope(),
        resources(RustTestBackend::Libtest { test_threads: 1 }),
        higher,
    )
    .expect_err("higher concurrency");
    assert_eq!(error.code, "non_lower_concurrency_retry");
}

#[test]
fn non_test_retry_cannot_introduce_test_runtime_authority() {
    let retry = RustRetryPolicy::OneLowerConcurrency {
        policy_id: RustRetryPolicyId::parse("check-low.v1").expect("policy"),
        concurrency: RustRetryConcurrency::new(1, Some(1), 0).expect("retry"),
    };
    let error = envelope(
        RustVerificationScope::Check {
            packages: RustPackageSelection::One {
                package: PackageId::parse("codex-core").expect("package"),
            },
            targets: RustTargetSelection::library_only(),
        },
        resources(RustTestBackend::None),
        retry,
    )
    .expect_err("backend widening");
    assert_eq!(error.code, "retry_backend_mismatch");
}

#[test]
fn empty_broad_target_selection_is_refused() {
    let empty = RustTargetSelection {
        library: false,
        binaries: RustNamedTargetSelection::None,
        integration_tests: RustNamedTargetSelection::None,
        examples: RustNamedTargetSelection::None,
        benches: RustNamedTargetSelection::None,
        doctests: false,
        build_scripts: true,
    };
    let error = envelope(
        RustVerificationScope::WorkspaceTests { targets: empty },
        resources(RustTestBackend::Nextest {
            test_threads: 1,
            filterset: None,
        }),
        RustRetryPolicy::None,
    )
    .expect_err("empty target set");
    assert_eq!(error.code, "empty_target_selection");
}

#[test]
fn identifiers_and_filters_reject_aliases_controls_and_unbounded_values() {
    assert!(RustTargetTriple::parse("../target").is_err());
    assert!(RustFeatureName::parse("feature/name").is_err());
    assert!(RustTestFilter::parse("bad\nfilter").is_err());
    assert!(RustNextestFilterset::parse(&"x".repeat(MAX_FILTER_BYTES + 1)).is_err());
}
