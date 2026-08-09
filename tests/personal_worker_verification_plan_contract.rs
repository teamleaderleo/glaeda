#![cfg(target_os = "linux")]

#[test]
fn module_remains_a_pure_non_executing_planner() {
    let source = include_str!("../src/personal_worker_verification_plan.rs");
    for forbidden in [
        "std::process",
        "CommandSpec",
        "ProcessExecutor",
        "std::fs",
        "OpenOptions",
        "TcpStream",
        "reqwest",
        "unsafe {",
        "sh -c",
    ] {
        assert!(!source.contains(forbidden), "planner contains {forbidden}");
    }
    assert!(source.contains("PersonalWorkerOperatorJobRead"));
    assert!(source.contains("PersonalWorkerRunnerReadinessObservation"));
    assert!(source.contains("RepositorySourceObservation"));
    assert!(source.contains("TrustedWorkspaceCacheReceipt"));
    assert!(source.contains("RustVerificationEnvelope"));
    assert!(source.contains("digest_rust_verification_envelope"));
}
