/// Read-only observation of one configured official Actions runner.
pub mod actions_runner_readiness;
pub mod artifact;
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
pub mod execution_receipt;
pub mod execution_receipt_store;
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
/// Pure, bounded normalization of complete GitHub workflow-job reconciliation snapshots.
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
#[cfg(target_os = "linux")]
pub mod host_preparation_receipt_binding;
#[cfg(target_os = "linux")]
pub mod host_readiness;
#[cfg(target_os = "linux")]
pub mod host_readiness_verdict;
#[cfg(target_os = "linux")]
pub mod host_rootless_podman;
/// Pure bounded observation-only receipts for blazingly hot execution measurements.
pub mod hot_execution_performance;
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
#[cfg(target_os = "linux")]
pub mod linux_state;
#[cfg(target_os = "linux")]
pub mod linux_state_prepare;
#[cfg(target_os = "linux")]
pub mod linux_state_recovery;
/// Pure fixed offline Cargo command policy for exact local self-builds.
pub mod local_install_build_command;
/// Read-only, path-private proof that the isolated self-build Cargo lookup path is config-free.
pub mod local_install_cargo_config_preflight;
/// Pure exact-source local binary generation and stable launcher planning.
pub mod local_install_plan;
/// Read-only exact checkout and Cargo.lock proof for local self-builds.
#[cfg(unix)]
pub mod local_install_source_preflight;
pub mod mac_availability;
pub mod macos_resource_observation;
pub mod manifest;
/// Pure schema-versioned personal-worker operator configuration and public identity.
pub mod operator_config;
/// Private-path-safe discovery and atomic persistence of operator configuration.
pub mod operator_config_store;
/// Closed public operator error, retry, remediation, dependency, approval, and command vocabulary.
pub mod operator_error;
/// Pure non-authorizing remediation applicability, safety, and confidence vocabulary.
pub mod operator_remediation;
/// Pure unified personal-worker operator status report and human renderer.
pub mod operator_status;
/// Typed, read-only aggregation of one coherent operator status evidence bundle.
pub mod operator_status_service;
pub mod ownership;
/// Pure composition of durable queue, Lima lifecycle, and runner-readiness evidence.
pub mod personal_worker_host_broker;
/// Same-lock durable execution of one exact personal-worker Lima lifecycle tick.
pub mod personal_worker_lima_adapter;
/// Pure, path-private durable ownership and crash-phase authority for personal-worker Lima.
pub mod personal_worker_lima_authority;
/// Read-only Mac/Lima observation composed for personal-worker planning.
pub mod personal_worker_mac_observation;
/// Config-bound ergonomic submission and queued cancellation.
pub mod personal_worker_operator_mutation;
/// Config-bound, current-snapshot status, queue, and job reads.
pub mod personal_worker_read_model;
/// Durable, lock-protected personal-worker request queue.
pub mod personal_worker_request_queue;
/// Pure durable status snapshot plus secret-free human/JSON rendering.
pub mod personal_worker_status;
/// Typed macOS bootstrap checklist for the personal worker.
pub mod personal_worker_workstation_checklist;
/// Pure bounded persistent worker identity plus canonical state path layout.
pub mod personal_worker_workspace;
#[cfg(target_os = "linux")]
pub mod podman_diagnostic;
#[cfg(target_os = "linux")]
pub mod podman_execution;
#[cfg(target_os = "linux")]
pub mod podman_network;
#[cfg(target_os = "linux")]
pub mod podman_network_gate;
#[cfg(target_os = "linux")]
pub mod podman_network_gate_activation;
#[cfg(target_os = "linux")]
pub mod podman_network_gate_execution;
#[cfg(target_os = "linux")]
pub mod podman_network_gate_observation;
#[cfg(target_os = "linux")]
pub mod podman_network_gate_receipt;
#[cfg(target_os = "linux")]
pub mod podman_network_join;
#[cfg(target_os = "linux")]
pub mod podman_plan;
#[cfg(target_os = "linux")]
pub mod podman_rootfs;
#[cfg(target_os = "linux")]
pub mod podman_runtime;
#[cfg(target_os = "linux")]
pub mod podman_storage;
pub mod project_catalog;
/// Canonical path-to-source policy for active project worktrees.
pub mod project_checkout;
/// Read-only project state observation.
#[cfg(target_os = "linux")]
pub mod project_checkout_observation;
#[cfg(unix)]
pub(crate) mod project_checkout_observation_git;
#[cfg(unix)]
pub(crate) mod project_checkout_observation_path;
/// Pure exact planning for single-writer persistent project disks.
pub mod project_disk_lease;
#[cfg(unix)]
pub mod project_reconciliation;
pub mod protocol;
pub mod queue;
pub mod queue_command;
pub mod queue_document;
pub mod queue_store;
pub mod runner_account;
pub mod runner_account_plan;
pub mod runner_account_reconciliation;
pub mod runner_config;
pub mod runner_config_generation;
pub mod runner_config_recovery;
pub mod runner_egress_readiness;
pub mod runner_identity;
pub mod runner_profile;
pub mod runner_user;
pub mod service_account;
pub mod service_account_apply;
pub mod service_account_plan;
pub mod service_account_status;
pub mod source_plan;
pub mod state_store;
/// Pure planning for trusted OverlayFS mount authority.
#[cfg(target_os = "linux")]
pub mod trusted_overlay_mount_plan;
/// Sealed all-FD trusted OverlayFS mount transaction behind exact physical correlation proof.
#[cfg(target_os = "linux")]
pub mod trusted_overlay_mount_execution;
/// Pure trusted overlay task/anchor lifecycle and cleanup planning.
pub mod trusted_overlay_task_view;
/// Canonical one-shot resident guest-control protocol envelope.
pub mod trusted_guest_control_protocol;
/// Pure sealed Mac-side invocation planning for one canonical guest-control request.
#[cfg(unix)]
pub mod trusted_guest_control_invocation_plan;
/// Pure sealed task-private Git clone planning from exact hot-path leases.
#[cfg(target_os = "linux")]
pub mod task_private_git_clone_plan;
#[cfg(unix)]
pub mod verification;
pub mod verification_envelope;
pub mod workflow_artifact;
