/// Read-only observation of one configured official Actions runner.
pub mod actions_runner_readiness;
pub mod artifact;
/// Read-only byte proof for snapshot-required objects in one persistent Blender content store.
#[cfg(unix)]
pub mod blender_content_store_observation;
/// Pure, path-free classification of explicit hot-state inventory observations.
pub mod cache_inventory;
/// Positive-only Linux process and mount reference observation for one Cargo target.
#[cfg(target_os = "linux")]
pub mod cargo_target_holder_observation;
/// Read-only, descriptor-bound observation of one Linux Cargo target tree.
#[cfg(target_os = "linux")]
pub mod cargo_target_observation;
/// Pure workload-neutral pre-admission compute request.
pub mod compute_execution_request;
/// Pure workload-family-neutral identity for declared compute semantics.
pub mod compute_workload;
#[cfg(target_os = "linux")]
pub mod debian_package_plan;
#[cfg(target_os = "linux")]
pub mod debian_package_probe;
#[cfg(target_os = "linux")]
pub mod debian_package_recovery;
/// Descriptor-bound execution of already reviewed Linux launch plans.
#[cfg(target_os = "linux")]
pub mod descriptor_bound_launcher;
/// Pure bounded multi-attempt resource ledger and atomic store contract.
pub mod disposable_attempt_catalog;
mod disposable_attempt_catalog_job_lookup;
/// Pure durable state, revisions, and codec for one disposable worker attempt.
pub mod disposable_attempt_state;
/// Same-lock execution of one authorized disposable Lima clone.
#[cfg(unix)]
pub mod disposable_clone_runtime;
#[cfg(unix)]
pub(crate) mod disposable_host_storage;
/// Exact plan plus explicitly approved macOS apply boundary for the disposable-worker LaunchAgent.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub mod disposable_launchd_service;
/// Read-only exact installed-state observation for the disposable-worker LaunchAgent.
#[cfg(target_os = "macos")]
pub mod disposable_launchd_service_status;
/// Sealed fixed Lima command plans for one durably planned disposable worker.
pub mod disposable_lima_worker;
/// Canonical supply-chain and isolation identity for the prepared disposable VM template.
pub mod disposable_prepared_template;
/// Private, secret-safe command binding for one durably registered disposable guest runner.
#[cfg(unix)]
pub(crate) mod disposable_runner_runtime;
/// Pure bounded diagnostic document for one controller-service failure.
pub mod disposable_service_failure_receipt;
pub mod disposable_template_generation;
/// Same-lock bounded Lima supervisor for the disposable source-template lifecycle.
#[cfg(unix)]
pub mod disposable_template_runtime;
#[cfg(unix)]
pub(crate) mod disposable_worker_coordinator;
/// Canonical, secret-free operator enrollment for one disposable Scale Set worker.
#[cfg(unix)]
pub mod disposable_worker_enrollment;
/// Pure capacity and lifecycle reconciliation for one-job disposable workers.
pub mod disposable_worker_reconciler;
/// Process-lifetime composition of enrollment, durable recovery, coordinator, and supervisor.
#[cfg(unix)]
pub mod disposable_worker_service;
#[cfg(unix)]
pub(crate) mod disposable_worker_supervisor;
pub mod doctor;
pub mod durable_journal;
#[cfg(target_os = "linux")]
pub mod durable_lane_execution;
#[cfg_attr(test, allow(clippy::too_many_arguments))]
pub mod execution_admission;
/// Pure product-neutral resource ownership and fail-closed capacity arithmetic.
pub mod execution_capacity;
pub mod execution_receipt;
pub mod execution_receipt_store;
/// Pure model-derived frontier-inference workload vocabulary and synthetic sensitivity fixtures.
pub mod frontier_inference_workload;
/// Pure Git index-v2 stat-cache patching for CoW task materialization.
pub mod git_index_stat_patch;
/// Private-process adapter for the pinned official Runner Scale Set bridge.
#[cfg(unix)]
pub(crate) mod github_scale_set_bridge;
/// Canonical bounded durable record of one polled Runner Scale Set delivery.
#[cfg(unix)]
pub(crate) mod github_scale_set_delivery;
/// Pure exact reconciliation of one retained Scale Set delivery into disposable-attempt state.
#[cfg(unix)]
pub(crate) mod github_scale_set_delivery_consumer;
/// Crash-safe poll, durable reconciliation, acknowledgement, and acquisition recovery.
#[cfg(unix)]
pub(crate) mod github_scale_set_delivery_controller;
/// Pure catalog settlement after conclusive Scale Set acknowledgement evidence.
#[cfg(unix)]
pub(crate) mod github_scale_set_delivery_settlement;
/// Pure crash/replay phases for one durably reconciled Scale Set delivery.
#[cfg(unix)]
pub(crate) mod github_scale_set_delivery_state;
/// Pure bounded vocabulary for GitHub Runner Scale Set job and runner identities.
pub mod github_scale_set_protocol;
/// Pure, fail-closed mapping of reviewed GitHub workflow-job evidence into typed broker intents.
pub mod github_workflow_job_mapper;
/// Pure bounded normalization of complete GitHub workflow-job reconciliation snapshots.
pub mod github_workflow_job_reconciliation;
pub mod host;
#[cfg(target_os = "linux")]
pub mod host_package_plan;
#[cfg(target_os = "linux")]
pub mod host_preparation_command;
#[cfg(target_os = "linux")]
pub mod host_preparation_execution;
#[cfg(target_os = "linux")]
pub mod host_preparation_plan;
#[cfg(target_os = "linux")]
pub mod host_preparation_receipt;
pub mod host_preparation_receipt_binding;
#[cfg(target_os = "linux")]
pub mod host_readiness;
#[cfg(target_os = "linux")]
pub mod host_readiness_verdict;
#[cfg(target_os = "linux")]
pub mod host_rootless_podman;
/// Pure bounded observation-only receipts for blazingly hot execution measurements.
pub mod hot_execution_performance;
/// Pure descriptive p50/p90 summaries around exact hot-fleet A-B-B-A comparisons.
pub mod hot_fleet_latency_summary;
/// Pure bounded contended fleet-window receipts and exact A-B-B-A comparison evidence.
pub mod hot_fleet_window;
/// Read-only, descriptor-bound observation of an explicit Linux hot-run cache root.
#[cfg(target_os = "linux")]
pub mod hot_run_cache_observation;
/// Pure path-class policy for selecting reviewed hot-state sharing mechanisms.
pub mod hot_state_path_policy;
/// Pure immutable resident Git object-pool generation and consumer-lease core.
pub mod immutable_git_object_pool;
/// Pure sealed non-task Git producer planning for immutable pool publication.
#[cfg(target_os = "linux")]
pub mod immutable_git_object_pool_admin_producer_plan;
/// Publication-time descriptor-bound audit of staged immutable Git pool candidates.
#[cfg(target_os = "linux")]
pub mod immutable_git_object_pool_generation_audit;
/// Pure fixed marker codec for immutable Git object-pool generations.
pub mod immutable_git_object_pool_marker;
/// Read-only descriptor-bound ownership observation of immutable Git object-pool generations.
#[cfg(target_os = "linux")]
pub mod immutable_git_object_pool_observation;
#[cfg(target_os = "linux")]
pub mod installation_id;
pub mod journal;
pub mod journal_document;
pub mod lane_command;
#[cfg(target_os = "linux")]
pub mod lane_executable;
#[cfg(target_os = "linux")]
pub mod lane_executor;
pub mod lease;
pub mod lease_catalog;
pub mod lease_document;
/// Descriptor-bound host identity for one reviewed Lima VZ instance and raw root disk.
#[cfg(unix)]
pub mod lima_host_identity;
/// Pure Lima policy: work while active, interactive after 10 idle minutes, stopped after 30.
pub mod lima_lifecycle;
/// Fixed direct executor for accepted personal-worker Lima lifecycle actions.
pub mod lima_lifecycle_executor;
/// Read-only, bounded exact observation of one Lima instance and running guest.
pub mod lima_observation;
/// Pure bounded parsing of the admitted glibc dynamic-loader cache.
#[cfg(target_os = "linux")]
pub mod linux_dynamic_loader_cache;
/// Pure bounded parsing of the admitted Linux dynamic-loader configuration.
#[cfg(target_os = "linux")]
pub mod linux_dynamic_loader_config;
/// Pure bounded ELF64 dependency parsing for the Linux runtime closure.
#[cfg(target_os = "linux")]
pub mod linux_elf_runtime_dependency;
/// Bounded path-private observation of one Linux operator machine.
#[cfg(target_os = "linux")]
pub mod linux_host_observation;
/// Read-only, fail-closed lookup of persisted project installations.
#[cfg(target_os = "linux")]
pub mod linux_installation_catalog;
/// Nonblocking coordination for installation-catalog discovery and creation.
#[cfg(target_os = "linux")]
pub mod linux_installation_catalog_lock;
/// Locked, race-free create-or-load orchestration for local project installations.
#[cfg(target_os = "linux")]
pub mod linux_installation_enrollment;
/// Staged, durable, no-replace publication of complete project installations.
#[cfg(target_os = "linux")]
pub mod linux_installation_publication;
/// Durable, revision-checked lease persistence beneath one installation directory.
#[cfg(target_os = "linux")]
pub mod linux_lease_store;
/// Direct command-free observation of the five account-related personal-worker runtime classes.
#[cfg(target_os = "linux")]
pub mod linux_personal_worker_runtime_account_evidence;
/// Descriptor-bound prerequisites for the fixed personal-worker runtime executables.
#[cfg(target_os = "linux")]
pub mod linux_personal_worker_runtime_executable_prerequisite;
/// Direct command-free Linux kernel and cgroup-v2 prerequisites for the personal-worker runtime.
#[cfg(target_os = "linux")]
pub mod linux_personal_worker_runtime_kernel_prerequisite;
/// Same-lock snapshot of current executable and dynamic-loader prerequisites.
#[cfg(target_os = "linux")]
pub mod linux_personal_worker_runtime_linkage_prerequisite;
/// Read-only, descriptor-bound observation of the fixed GNU dynamic-loader object.
#[cfg(target_os = "linux")]
pub mod linux_personal_worker_runtime_loader_object_prerequisite;
/// Descriptor-bound prerequisite for fixed loader configuration, cache, and preload absence.
#[cfg(target_os = "linux")]
pub mod linux_personal_worker_runtime_loader_state_prerequisite;
/// Read-only, locked discovery of one protected recorded personal-worker runtime manifest.
#[cfg(target_os = "linux")]
pub mod linux_personal_worker_runtime_manifest;
/// Native Linux research adapter for exact same-HEAD reflink task worktrees.
#[cfg(target_os = "linux")]
pub mod linux_reflink_task_materialization;
#[cfg(target_os = "linux")]
pub mod linux_state;
#[cfg(target_os = "linux")]
pub mod linux_state_prepare;
#[cfg(target_os = "linux")]
pub mod linux_state_recovery;
/// Pure child-process and downstream-project match aggregation for local agent presence.
pub mod local_agent_presence;
/// Pure fixed-command planning for source read/edit/verification actions on one local installation.
pub mod local_install_action_plan;
/// Local process-bound execution adapter for one sealed project action plan.
#[cfg(unix)]
pub mod local_install_action_runtime;
/// Read-only operator-local manifest for explicit executable admission.
#[cfg(unix)]
pub mod local_install_executable_manifest;
/// Bound launch adapter for one locally admitted execution plan.
#[cfg(unix)]
pub mod local_install_launcher;
/// Canonical generation-aware local install identity and marker codec.
pub mod local_install_generation;
/// Descriptor-bound generation loading with explicit runtime handoff.
#[cfg(unix)]
pub mod local_install_generation_store;
/// Pure exact plan and receipt for one owned local patch applicability check.
pub mod local_patch_check;
/// Process-backed execution of the fixed local patch applicability check.
#[cfg(unix)]
pub mod local_patch_check_runtime;
/// Pure local-action intent and command fingerprint for approved local project operations.
pub mod local_action_intent;
/// Direct read-only observation of one owned local agent process tree.
#[cfg(target_os = "linux")]
pub mod local_agent_process_observation;
/// Local project worker durable state and exact identity.
pub mod local_project_worker_state;
/// Read-only local project worker state observation and bounded report.
#[cfg(unix)]
pub mod local_project_worker_observation;
/// Read-only report composition for local project worker state and process evidence.
pub mod local_project_worker_report;
/// Pure bounded local project worker admission and resource planning.
pub mod local_project_worker_admission;
/// Descriptor-bound local project worker launch and settlement.
#[cfg(unix)]
pub mod local_project_worker_execution;
/// Pure local project worker launch-plan derivation.
pub mod local_project_worker_launch_plan;
/// Exact resource observation for one local project worker process tree.
#[cfg(target_os = "linux")]
pub mod local_project_worker_resource_observation;
/// Bounded owner-local workload observation for one project worker.
#[cfg(unix)]
pub mod local_project_worker_workload_observation;
/// Pure bounded project worker supervision policy.
pub mod local_project_worker_supervision;
/// Pure proof-bearing project workspace reference.
pub mod project_workspace;
/// Pure capacity reservation decision for one project workspace.
pub mod project_workspace_admission;
/// Read-only descriptor-bound project workspace observation.
#[cfg(unix)]
pub mod project_workspace_observation;
/// Pure project workspace readiness and exact source identity.
pub mod project_workspace_readiness;
/// Immutable project workspace registration and bounded catalog.
pub mod project_workspace_registry;
/// Pure project-disk attachment state and transition vocabulary.
pub mod project_disk;
/// Exact host-side project-disk observation and correlation.
#[cfg(unix)]
pub mod project_disk_host_observation;
/// Pure bounded model from host and guest evidence into project-disk attachment truth.
pub mod project_disk_observation;
/// Provider-neutral read-only project-disk attach planning.
pub mod project_disk_attach_plan;
/// Pure bounded project-disk attach receipt and settlement model.
pub mod project_disk_attach_receipt;
/// Pure project-disk lifecycle and capacity classification.
pub mod project_disk_lifecycle;
/// Pure project-disk persistent state and identity.
pub mod project_disk_state;
/// Pure bounded project-disk workload affinity and routing hint.
pub mod project_disk_workload_affinity;
/// Pure bounded resident project-state valuation.
pub mod resident_project_value;
/// Pure bounded trusted overlay task-view lifecycle.
pub mod trusted_overlay_task_view;
/// Pure descriptor-bound trusted overlay mount planning.
#[cfg(target_os = "linux")]
pub mod trusted_overlay_mount_plan;
/// Direct descriptor-bound trusted OverlayFS mount execution.
#[cfg(target_os = "linux")]
pub mod trusted_overlay_mount_execution;
/// Pure exact task-private Git clone planning from immutable object-pool generations.
#[cfg(target_os = "linux")]
pub mod task_private_git_clone_plan;