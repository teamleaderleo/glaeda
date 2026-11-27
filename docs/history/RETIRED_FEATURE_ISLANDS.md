# Retired feature islands

SmolRunner keeps retired implementation history in Git rather than compiling or copying unused
source into the current product. This ledger records exact recovery coordinates and why each island
left the active tree.

## Exact-commit handoff and runner export

- Recovery base: `b2015cd32e632ae3c27b2abbcf635a16b2c8b38a`
- Former paths: `src/exact_commit_handoff.rs`, `src/runner_export.rs`, and
  `src/runner_export/tests.rs`
- Product track: deferred personal-worker B05/B06 experiment
- Authority removed: a credentialless Git subprocess adapter that could create and inspect bounded
  exact-commit bundle exports

The modules had no CLI, service, or disposable-worker caller. Their only live references were their
public library exports, their own tests, and the implementation map. SmolRunner now delegates source
checkout and job semantics to GitHub Actions inside each disposable VM, so retaining this alternate
host-side transfer path increased compile, lint, test, and review surface without advancing the
current product boundary.

If a future design needs this behavior, recover the exact reviewed source from the named base and
reintroduce it through a new product decision and normal security review. Do not treat this ledger as
execution or ownership authority.

## Rootless-Podman preview planning and execution

- Recovery base: `ff0bec8c98194c6eca3f9b4aeb4aef0b90205803`
- Former paths: `src/preview.rs`, `src/podman_preview.rs`,
  `src/podman_preview_execution.rs`, `src/podman_preview_inspect.rs`, and
  `src/podman_preview_state.rs`
- Product track: deferred local preview experiment
- Authority removed: a bounded rootless-Podman subprocess adapter plus pure preview planning,
  inspection authorization, and reconciliation types

The five modules formed one internally connected island with no CLI, service, disposable-worker, or
other production caller. Their only current-tree entry points were public library exports, and their
behavior was exercised only by module-local tests. The disposable Lima/VZ worker is now the hostile
workload boundary; host-side rootless Podman previews are explicitly outside the product critical
path. The accepted preview ADRs remain in `docs/adr/` as historical design records.

The generic lease vocabulary remains active because other durable-store code uses it independently
of the retired preview implementation. If previews return as a product requirement, recover the
reviewed source from the named base and re-evaluate it against the then-current VM, credential,
network, and ownership boundaries rather than treating old types or names as live authority.

## Twenty-four exports with no consumer (code-cut sweep, 2026-11-27)

- Recovery base: `7a488c66` (teamleaderleo/glaeda main, 2026-11-27)
- Former paths: `src/adaptive_ci_routing.rs`, `src/cmux_product_transport_adapter.rs`, `src/debian_package_recovery.rs`, `src/failure_diagnostic.rs`, `src/github_workflow_job_reconciliation.rs`, `src/hot_fleet_latency_summary.rs`, `src/immutable_artifact_distribution.rs`, `src/linux_installation_enrollment.rs`, `src/linux_lease_store.rs`, `src/linux_personal_worker_runtime_account_evidence.rs`, `src/linux_personal_worker_runtime_kernel_prerequisite.rs`, `src/linux_state_recovery.rs`, `src/operator_remediation.rs`, `src/project_discovery.rs`, `src/project_filesystem_correlation_receipt.rs`, `src/protected_cache_namespace_lease_visibility.rs`, `src/protected_cache_replacement_equivalence.rs`, `src/quarry_parallel_verification_capture.rs`, `src/renderprove_artifact_binding.rs`, `src/resident_backend_comparison.rs`, `src/rust_memory_diagnostic.rs`, `src/trusted_guest_control_binary_generation.rs`, `src/trusted_project_filesystem_full_guest_correlation.rs`, `src/verification_isolation_referee.rs`
- Product track: kernels built ahead of composition that no product path ever composed
- Authority removed: none in use. `docs/MODULE_INVENTORY.md` listed each with reach `none`: no binary,
  library module, test, bench, example, workflow or script named it outside its own files.

Each was written between 2026-07-24 and 2026-10-27 and never wired in, yet every `cargo check`, clippy
run, test build and review paid for it. Of the inventory's 35 `none` exports, three stay because a
workflow or contract test still names them (`production_recovery_adapter`,
`linux_personal_worker_runtime_linkage_prerequisite`, `personal_worker_verification_plan`). Eight more
stay for a follow-up because removing them also leaves helpers in their callee modules unused, and each
needs its own small cut: `immutable_git_object_pool_generation_audit`, `local_install_launcher`, `project_disk_attachment_state`, `renderprove_protected_mount`, `task_private_git_clone_plan`, `trusted_overlay_mount_execution`, `unix_protected_cache_generation_catalog_store`, `unix_resident_sandbox_store`.

To bring one back, recover it from the base above through a new product decision, with its first
caller in the same change, so it never lands unreferenced again.
