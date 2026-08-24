use std::error::Error;
use std::fmt;

use clap::{Args, Parser, Subcommand};
use serde::Serialize;
use smolrunner::artifact::{CommitId, GitTreeId, Sha256Digest};
use smolrunner::hot_execution_performance::{
    HotBuildState, HotDependencyState, HotExecutionHeat, HotExecutionMilestones, HotExecutionMode,
    HotExecutionPerformanceIdentity, HotExecutionPerformanceReceipt, HotExecutionResourceObservation,
    HotExecutionResultClass, HotExecutionStorageObservation, HotIndexServiceState, HotRepositoryState,
    HotSandboxState,
};

const PLAN_SCHEMA_VERSION: u8 = 1;
const SAMPLE_SCHEMA_VERSION: u8 = 1;
const WORKLOAD_ID: &str = "quarry-agent-brief-edit-test-v1";
const PROJECT_ID: &str = "quarry";
const CONTROL_CANDIDATE_ID: &str = "trusted-mac-current";
const HOT_CANDIDATE_ID: &str = "project-disk-ext4-overlay";
const BACKEND_ID: &str = "lima-vz";

#[derive(Debug, Parser)]
#[command(about = "Plan and record the first Quarry resident-project dogfood experiment")]
struct Cli {
    #[arg(long, default_value = "human")]
    output: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Emit the frozen software-side experiment contract. Performs no host or guest mutation.
    Plan(PlanArgs),
    /// Emit one bounded observation receipt from already-owned benchmark timestamps.
    Sample(SampleArgs),
}

#[derive(Debug, Args)]
struct PlanArgs {
    #[arg(long)]
    quarry_commit: String,
    #[arg(long)]
    quarry_tree: String,
    #[arg(long)]
    smolrunner_commit: String,
}

#[derive(Debug, Args)]
struct SampleArgs {
    #[arg(long)]
    quarry_commit: String,
    #[arg(long)]
    quarry_tree: String,
    #[arg(long)]
    smolrunner_commit: String,
    #[arg(long)]
    arm: String,
    #[arg(long)]
    sample_class: String,
    #[arg(long)]
    ordinal: u16,
    #[arg(long)]
    fanout: Option<u8>,
    #[arg(long)]
    execution_mode: String,
    #[arg(long)]
    host_class: String,
    #[arg(long)]
    resource_profile: String,
    #[arg(long)]
    total_elapsed_millis: u64,
    #[arg(long)]
    sandbox_ready_millis: Option<u64>,
    #[arg(long)]
    repository_ready_millis: Option<u64>,
    #[arg(long)]
    dependency_ready_millis: Option<u64>,
    #[arg(long)]
    first_useful_command_millis: u64,
    #[arg(long)]
    edit_complete_millis: Option<u64>,
    #[arg(long)]
    focused_pytest_result_millis: Option<u64>,
    #[arg(long)]
    final_relevant_result_millis: Option<u64>,
    #[arg(long)]
    residency_transition_millis: Option<u64>,
    #[arg(long)]
    agent_brief_digest: Option<String>,
    #[arg(long)]
    guest_logical_bytes: Option<u64>,
    #[arg(long)]
    guest_filesystem_used_bytes: Option<u64>,
    #[arg(long)]
    host_backing_logical_bytes: Option<u64>,
    #[arg(long)]
    host_backing_allocated_bytes: Option<u64>,
    #[arg(long)]
    task_filesystem_used_delta_bytes: Option<u64>,
    #[arg(long)]
    peak_guest_memory_bytes: Option<u64>,
    #[arg(long)]
    host_memory_delta_bytes: Option<i64>,
    #[arg(long)]
    cpu_time_millis: Option<u64>,
    #[arg(long, default_value = "succeeded")]
    result: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum DogfoodAuthority {
    ObservationOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum PhysicalMutationPolicy {
    SealedUntilSeparatelyAuthorized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum DogfoodArm {
    Control,
    HotProject,
}

impl DogfoodArm {
    fn parse(value: &str) -> Result<Self, DogfoodError> {
        match value {
            "control" => Ok(Self::Control),
            "hot_project" => Ok(Self::HotProject),
            _ => Err(DogfoodError::new(
                "invalid_dogfood_arm",
                "dogfood arm must be control or hot_project",
            )),
        }
    }

    const fn candidate_id(self) -> &'static str {
        match self {
            Self::Control => CONTROL_CANDIDATE_ID,
            Self::HotProject => HOT_CANDIDATE_ID,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum DogfoodSampleClass {
    Singleton,
    Sequential,
    Fanout,
    Restart,
    Fallback,
}

impl DogfoodSampleClass {
    fn parse(value: &str) -> Result<Self, DogfoodError> {
        match value {
            "singleton" => Ok(Self::Singleton),
            "sequential" => Ok(Self::Sequential),
            "fanout" => Ok(Self::Fanout),
            "restart" => Ok(Self::Restart),
            "fallback" => Ok(Self::Fallback),
            _ => Err(DogfoodError::new(
                "invalid_dogfood_sample_class",
                "dogfood sample class is outside the frozen experiment vocabulary",
            )),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct StatePlacement {
    state_class: &'static str,
    placement: &'static str,
    lifetime: &'static str,
    authority: &'static str,
    invalidation: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct FrozenWorkload {
    orientation_command: [&'static str; 5],
    edit_target: &'static str,
    edit_semantics: &'static str,
    focused_test_command: [&'static str; 6],
    sequential_fresh_tasks: u8,
    fanout_widths: [u8; 3],
    restart_cycles: u8,
    cold_fallback_required: bool,
    final_cold_oracle: [&'static str; 6],
}

#[derive(Debug, Clone, Serialize)]
struct AcceptanceThresholds {
    singleton_first_command_p50_max_control_basis_points: u16,
    singleton_first_command_p90_max_control_basis_points: u16,
    edit_to_focused_pytest_p50_max_control_basis_points: u16,
    fanout_batch_min_speedup_milli: u16,
    required_restart_successes: u8,
    max_break_even_tasks: u8,
    exact_source_and_output_match_required: bool,
    unexplained_growth_per_second_cycle_allowed: bool,
    cold_oracle_pass_required: bool,
}

#[derive(Debug, Clone, Serialize)]
struct QuarryDogfoodPlan {
    schema_version: u8,
    authority: DogfoodAuthority,
    physical_mutation: PhysicalMutationPolicy,
    quarry_commit: CommitId,
    quarry_tree: GitTreeId,
    smolrunner_commit: CommitId,
    workload_id: &'static str,
    control_candidate_id: &'static str,
    hot_candidate_id: &'static str,
    backend_id: &'static str,
    state_placement: Vec<StatePlacement>,
    workload: FrozenWorkload,
    acceptance: AcceptanceThresholds,
}

#[derive(Debug, Clone, Serialize)]
struct EditTestObservation {
    edit_complete_millis: u64,
    focused_pytest_result_millis: u64,
    edit_to_focused_pytest_millis: u64,
}

#[derive(Debug, Clone, Serialize)]
struct SampleCoordinates {
    class: DogfoodSampleClass,
    ordinal: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    fanout: Option<u8>,
}

#[derive(Debug, Clone, Serialize)]
struct QuarryDogfoodSample {
    schema_version: u8,
    authority: DogfoodAuthority,
    physical_mutation: PhysicalMutationPolicy,
    arm: DogfoodArm,
    coordinates: SampleCoordinates,
    quarry_commit: CommitId,
    quarry_tree: GitTreeId,
    smolrunner_commit: CommitId,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_brief_digest: Option<Sha256Digest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    edit_test: Option<EditTestObservation>,
    performance: HotExecutionPerformanceReceipt,
}

#[derive(Debug)]
struct DogfoodError {
    code: &'static str,
    message: &'static str,
}

impl DogfoodError {
    const fn new(code: &'static str, message: &'static str) -> Self {
        Self { code, message }
    }
}

impl fmt::Display for DogfoodError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl Error for DogfoodError {}

fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    let output = cli.output.as_str();
    if output != "human" && output != "json" {
        return Err(Box::new(DogfoodError::new(
            "invalid_output_mode",
            "output must be human or json",
        )));
    }

    match cli.command {
        Command::Plan(args) => {
            let plan = build_plan(&args)?;
            if output == "json" {
                println!("{}", serde_json::to_string_pretty(&plan)?);
            } else {
                print_plan_human(&plan);
            }
        }
        Command::Sample(args) => {
            let sample = build_sample(&args)?;
            if output == "json" {
                println!("{}", serde_json::to_string_pretty(&sample)?);
            } else {
                print_sample_human(&sample);
            }
        }
    }
    Ok(())
}

fn build_plan(args: &PlanArgs) -> Result<QuarryDogfoodPlan, Box<dyn Error>> {
    Ok(QuarryDogfoodPlan {
        schema_version: PLAN_SCHEMA_VERSION,
        authority: DogfoodAuthority::ObservationOnly,
        physical_mutation: PhysicalMutationPolicy::SealedUntilSeparatelyAuthorized,
        quarry_commit: CommitId::parse(&args.quarry_commit)?,
        quarry_tree: GitTreeId::parse(&args.quarry_tree)?,
        smolrunner_commit: CommitId::parse(&args.smolrunner_commit)?,
        workload_id: WORKLOAD_ID,
        control_candidate_id: CONTROL_CANDIDATE_ID,
        hot_candidate_id: HOT_CANDIDATE_ID,
        backend_id: BACKEND_ID,
        state_placement: state_placement(),
        workload: FrozenWorkload {
            orientation_command: [
                "python",
                "-m",
                "quarry.agent_brief",
                "--format",
                "json",
            ],
            edit_target: "src/quarry/agent_brief.py",
            edit_semantics: "fixed_comment_only_patch_with_identical_program_behavior",
            focused_test_command: [
                "python",
                "-m",
                "pytest",
                "-q",
                "tests/test_agent_brief.py",
                "--disable-warnings",
            ],
            sequential_fresh_tasks: 10,
            fanout_widths: [1, 8, 32],
            restart_cycles: 3,
            cold_fallback_required: true,
            final_cold_oracle: [
                "python",
                "scripts/run_local_tests.py",
                "exact-head",
                "--base",
                "origin/main",
                "--",
            ],
        },
        acceptance: AcceptanceThresholds {
            singleton_first_command_p50_max_control_basis_points: 5_000,
            singleton_first_command_p90_max_control_basis_points: 6_500,
            edit_to_focused_pytest_p50_max_control_basis_points: 11_000,
            fanout_batch_min_speedup_milli: 2_000,
            required_restart_successes: 3,
            max_break_even_tasks: 10,
            exact_source_and_output_match_required: true,
            unexplained_growth_per_second_cycle_allowed: false,
            cold_oracle_pass_required: true,
        },
    })
}

fn state_placement() -> Vec<StatePlacement> {
    vec![
        StatePlacement {
            state_class: "git_object_pool",
            placement: "persistent_project_disk",
            lifetime: "immutable_generation",
            authority: "acceleration_only",
            invalidation: "exact_pool_generation_or_source_parent_mismatch",
        },
        StatePlacement {
            state_class: "resident_source_anchor",
            placement: "persistent_project_disk",
            lifetime: "immutable_while_leased",
            authority: "working_state_only",
            invalidation: "commit_tree_index_or_cleanliness_mismatch",
        },
        StatePlacement {
            state_class: "python_environment",
            placement: "persistent_project_disk",
            lifetime: "project_generation",
            authority: "dependency_acceleration_only",
            invalidation: "python_toolchain_or_dependency_generation_mismatch",
        },
        StatePlacement {
            state_class: "dependency_cache",
            placement: "persistent_project_disk",
            lifetime: "evictable_project_cache",
            authority: "acceleration_only",
            invalidation: "cache_identity_mismatch_or_corruption",
        },
        StatePlacement {
            state_class: "derived_indexes",
            placement: "persistent_project_disk_when_exact_parented",
            lifetime: "rebuildable_generation",
            authority: "observation_or_acceleration_only",
            invalidation: "semantic_parent_or_tool_generation_mismatch",
        },
        StatePlacement {
            state_class: "task_private_git_metadata",
            placement: "persistent_project_disk_task_area",
            lifetime: "task_generation",
            authority: "task_working_state_only",
            invalidation: "task_pool_source_or_private_git_proof_mismatch",
        },
        StatePlacement {
            state_class: "overlay_upper_work",
            placement: "persistent_project_disk_task_area",
            lifetime: "task_generation",
            authority: "task_working_state_only",
            invalidation: "task_generation_or_mount_revalidation_mismatch",
        },
        StatePlacement {
            state_class: "pytest_cache",
            placement: "task_private_upper",
            lifetime: "task_generation",
            authority: "acceleration_only",
            invalidation: "task_settlement",
        },
        StatePlacement {
            state_class: "python_bytecode_cache",
            placement: "task_private_upper_in_phase_one",
            lifetime: "task_generation",
            authority: "acceleration_only",
            invalidation: "task_settlement",
        },
        StatePlacement {
            state_class: "overlay_merged_mount",
            placement: "guest_kernel_mount_state",
            lifetime: "task_generation",
            authority: "transaction_continuity_only",
            invalidation: "mount_or_task_lifecycle_mismatch",
        },
        StatePlacement {
            state_class: "disk_lease_and_attachment_authority",
            placement: "outside_project_disk",
            lifetime: "durable_controller_state",
            authority: "lifecycle_authority",
            invalidation: "fresh_external_observation_disagrees",
        },
        StatePlacement {
            state_class: "verification_and_source_authority",
            placement: "outside_project_disk",
            lifetime: "canonical_external_or_durable_evidence",
            authority: "source_and_verification_authority",
            invalidation: "fresh_proof_required",
        },
        StatePlacement {
            state_class: "credentials_and_jit_material",
            placement: "outside_project_disk",
            lifetime: "existing_bounded_capability_lifecycle",
            authority: "capability_specific",
            invalidation: "capability_generation_or_expiry",
        },
        StatePlacement {
            state_class: "performance_receipts",
            placement: "outside_project_disk",
            lifetime: "bounded_observation_history",
            authority: "observation_only",
            invalidation: "never_used_as_execution_authority",
        },
    ]
}

fn build_sample(args: &SampleArgs) -> Result<QuarryDogfoodSample, Box<dyn Error>> {
    let arm = DogfoodArm::parse(&args.arm)?;
    let class = DogfoodSampleClass::parse(&args.sample_class)?;
    validate_coordinates(class, args.ordinal, args.fanout)?;

    let quarry_commit = CommitId::parse(&args.quarry_commit)?;
    let quarry_tree = GitTreeId::parse(&args.quarry_tree)?;
    let smolrunner_commit = CommitId::parse(&args.smolrunner_commit)?;
    let execution_mode = parse_execution_mode(&args.execution_mode)?;
    validate_mode_for_arm(arm, execution_mode)?;

    let focused_pytest_result_millis = args.focused_pytest_result_millis;
    let edit_test = match (args.edit_complete_millis, focused_pytest_result_millis) {
        (Some(edit), Some(result)) if result >= edit => Some(EditTestObservation {
            edit_complete_millis: edit,
            focused_pytest_result_millis: result,
            edit_to_focused_pytest_millis: result - edit,
        }),
        (None, None) => None,
        (Some(_), Some(_)) => {
            return Err(Box::new(DogfoodError::new(
                "dogfood_result_before_edit",
                "focused pytest result must not precede edit completion",
            )));
        }
        _ => {
            return Err(Box::new(DogfoodError::new(
                "dogfood_incomplete_edit_measurement",
                "edit completion and focused pytest result must be recorded together",
            )));
        }
    };

    let milestones = HotExecutionMilestones::new(
        args.sandbox_ready_millis,
        args.repository_ready_millis,
        args.dependency_ready_millis,
        Some(args.first_useful_command_millis),
        focused_pytest_result_millis,
        args.final_relevant_result_millis,
        args.residency_transition_millis,
    )?;
    let storage = if [
        args.guest_logical_bytes,
        args.guest_filesystem_used_bytes,
        args.host_backing_logical_bytes,
        args.host_backing_allocated_bytes,
        args.task_filesystem_used_delta_bytes,
    ]
    .iter()
    .any(Option::is_some)
    {
        Some(HotExecutionStorageObservation::new(
            args.guest_logical_bytes,
            args.guest_filesystem_used_bytes,
            args.host_backing_logical_bytes,
            args.host_backing_allocated_bytes,
            args.task_filesystem_used_delta_bytes,
        )?)
    } else {
        None
    };
    let resources = if args.peak_guest_memory_bytes.is_some()
        || args.host_memory_delta_bytes.is_some()
        || args.cpu_time_millis.is_some()
    {
        Some(HotExecutionResourceObservation::new(
            args.peak_guest_memory_bytes,
            args.host_memory_delta_bytes,
            args.cpu_time_millis,
        )?)
    } else {
        None
    };

    let source_id = format!("git:{}", quarry_commit.as_str());
    let performance = HotExecutionPerformanceReceipt::new(
        HotExecutionPerformanceIdentity::new(
            WORKLOAD_ID,
            PROJECT_ID,
            &source_id,
            arm.candidate_id(),
            BACKEND_ID,
            &args.host_class,
            &args.resource_profile,
        )?,
        execution_mode,
        args.total_elapsed_millis,
        milestones,
        heat_for(arm, execution_mode),
        storage,
        resources,
        parse_result(&args.result)?,
    )?;

    Ok(QuarryDogfoodSample {
        schema_version: SAMPLE_SCHEMA_VERSION,
        authority: DogfoodAuthority::ObservationOnly,
        physical_mutation: PhysicalMutationPolicy::SealedUntilSeparatelyAuthorized,
        arm,
        coordinates: SampleCoordinates {
            class,
            ordinal: args.ordinal,
            fanout: args.fanout,
        },
        quarry_commit,
        quarry_tree,
        smolrunner_commit,
        agent_brief_digest: args
            .agent_brief_digest
            .as_deref()
            .map(Sha256Digest::parse)
            .transpose()?,
        edit_test,
        performance,
    })
}

fn validate_coordinates(
    class: DogfoodSampleClass,
    ordinal: u16,
    fanout: Option<u8>,
) -> Result<(), DogfoodError> {
    if ordinal == 0 {
        return Err(DogfoodError::new(
            "invalid_dogfood_ordinal",
            "dogfood sample ordinal must be greater than zero",
        ));
    }
    match class {
        DogfoodSampleClass::Fanout => {
            if !matches!(fanout, Some(1 | 8 | 32)) {
                return Err(DogfoodError::new(
                    "invalid_dogfood_fanout",
                    "fanout samples are frozen to widths 1, 8, or 32",
                ));
            }
        }
        DogfoodSampleClass::Sequential if ordinal > 10 => {
            return Err(DogfoodError::new(
                "invalid_dogfood_ordinal",
                "sequential dogfood samples are frozen to ten tasks",
            ));
        }
        DogfoodSampleClass::Restart if ordinal > 3 => {
            return Err(DogfoodError::new(
                "invalid_dogfood_ordinal",
                "restart dogfood samples are frozen to three cycles",
            ));
        }
        _ if fanout.is_some() => {
            return Err(DogfoodError::new(
                "unexpected_dogfood_fanout",
                "fanout width is valid only for fanout samples",
            ));
        }
        _ => {}
    }
    Ok(())
}

fn parse_execution_mode(value: &str) -> Result<HotExecutionMode, DogfoodError> {
    match value {
        "cold_disposable" => Ok(HotExecutionMode::ColdDisposable),
        "prepared_disposable" => Ok(HotExecutionMode::PreparedDisposable),
        "resident_after_idle" => Ok(HotExecutionMode::ResidentAfterIdle),
        "resident_immediate" => Ok(HotExecutionMode::ResidentImmediate),
        "resident_task_loop" => Ok(HotExecutionMode::ResidentTaskLoop),
        _ => Err(DogfoodError::new(
            "invalid_dogfood_execution_mode",
            "execution mode is outside the Quarry dogfood vocabulary",
        )),
    }
}

fn validate_mode_for_arm(arm: DogfoodArm, mode: HotExecutionMode) -> Result<(), DogfoodError> {
    let valid = match arm {
        DogfoodArm::Control => matches!(
            mode,
            HotExecutionMode::ColdDisposable | HotExecutionMode::PreparedDisposable
        ),
        DogfoodArm::HotProject => matches!(
            mode,
            HotExecutionMode::ResidentAfterIdle
                | HotExecutionMode::ResidentImmediate
                | HotExecutionMode::ResidentTaskLoop
        ),
    };
    if valid {
        Ok(())
    } else {
        Err(DogfoodError::new(
            "dogfood_arm_mode_mismatch",
            "execution mode is incompatible with the selected dogfood arm",
        ))
    }
}

fn parse_result(value: &str) -> Result<HotExecutionResultClass, DogfoodError> {
    match value {
        "succeeded" => Ok(HotExecutionResultClass::Succeeded),
        "failed" => Ok(HotExecutionResultClass::Failed),
        "canceled" => Ok(HotExecutionResultClass::Canceled),
        "reset_required" => Ok(HotExecutionResultClass::ResetRequired),
        "unknown" => Ok(HotExecutionResultClass::Unknown),
        _ => Err(DogfoodError::new(
            "invalid_dogfood_result",
            "result is outside the bounded hot-execution result vocabulary",
        )),
    }
}

fn heat_for(arm: DogfoodArm, mode: HotExecutionMode) -> HotExecutionHeat {
    match arm {
        DogfoodArm::Control => HotExecutionHeat::new(
            HotSandboxState::Prepared,
            HotRepositoryState::CheckoutHit,
            HotDependencyState::EnvironmentHit,
            HotBuildState::Cold,
            HotIndexServiceState::Unavailable,
        ),
        DogfoodArm::HotProject => HotExecutionHeat::new(
            if mode == HotExecutionMode::ResidentAfterIdle {
                HotSandboxState::Resumed
            } else {
                HotSandboxState::ResidentHit
            },
            HotRepositoryState::TaskFork,
            HotDependencyState::EnvironmentHit,
            HotBuildState::Cold,
            HotIndexServiceState::Unavailable,
        ),
    }
}

fn print_plan_human(plan: &QuarryDogfoodPlan) {
    println!("Quarry hot-project dogfood v{}", plan.schema_version);
    println!("Quarry commit: {}", plan.quarry_commit.as_str());
    println!("Quarry tree: {}", plan.quarry_tree.as_str());
    println!("SmolRunner commit: {}", plan.smolrunner_commit.as_str());
    println!("control: {}", plan.control_candidate_id);
    println!("treatment: {}", plan.hot_candidate_id);
    println!("sequential tasks: {}", plan.workload.sequential_fresh_tasks);
    println!("fanout: 1/8/32");
    println!("restart cycles: {}", plan.workload.restart_cycles);
    println!("physical mutation: sealed until separately authorized");
}

fn print_sample_human(sample: &QuarryDogfoodSample) {
    println!("Quarry dogfood sample");
    println!("arm: {:?}", sample.arm);
    println!("class: {:?}", sample.coordinates.class);
    println!("ordinal: {}", sample.coordinates.ordinal);
    if let Some(fanout) = sample.coordinates.fanout {
        println!("fanout: {fanout}");
    }
    if let Some(edit) = &sample.edit_test {
        println!(
            "edit -> focused pytest: {} ms",
            edit.edit_to_focused_pytest_millis
        );
    }
    print!("{}", sample.performance.render_human());
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    const TREE: &str = "89abcdef0123456789abcdef0123456789abcdef";
    const DIGEST: &str =
        "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn plan_freezes_first_quarry_workload_and_keeps_physical_mutation_sealed() {
        let plan = build_plan(&PlanArgs {
            quarry_commit: COMMIT.to_owned(),
            quarry_tree: TREE.to_owned(),
            smolrunner_commit: COMMIT.to_owned(),
        })
        .expect("plan is valid");

        assert_eq!(plan.schema_version, 1);
        assert_eq!(plan.workload.sequential_fresh_tasks, 10);
        assert_eq!(plan.workload.fanout_widths, [1, 8, 32]);
        assert_eq!(plan.workload.restart_cycles, 3);
        assert!(plan.workload.cold_fallback_required);
        assert_eq!(
            plan.physical_mutation,
            PhysicalMutationPolicy::SealedUntilSeparatelyAuthorized
        );
        assert!(
            plan.state_placement
                .iter()
                .any(|item| item.state_class == "disk_lease_and_attachment_authority"
                    && item.placement == "outside_project_disk")
        );
    }

    #[test]
    fn sample_records_owned_edit_to_focused_pytest_delta() {
        let sample = build_sample(&SampleArgs {
            quarry_commit: COMMIT.to_owned(),
            quarry_tree: TREE.to_owned(),
            smolrunner_commit: COMMIT.to_owned(),
            arm: "hot_project".to_owned(),
            sample_class: "sequential".to_owned(),
            ordinal: 1,
            fanout: None,
            execution_mode: "resident_task_loop".to_owned(),
            host_class: "apple-silicon".to_owned(),
            resource_profile: "medium-4c-8g".to_owned(),
            total_elapsed_millis: 3_000,
            sandbox_ready_millis: Some(1),
            repository_ready_millis: Some(8),
            dependency_ready_millis: Some(12),
            first_useful_command_millis: 20,
            edit_complete_millis: Some(100),
            focused_pytest_result_millis: Some(2_500),
            final_relevant_result_millis: Some(2_800),
            residency_transition_millis: Some(3_000),
            agent_brief_digest: Some(DIGEST.to_owned()),
            guest_logical_bytes: None,
            guest_filesystem_used_bytes: None,
            host_backing_logical_bytes: None,
            host_backing_allocated_bytes: None,
            task_filesystem_used_delta_bytes: None,
            peak_guest_memory_bytes: None,
            host_memory_delta_bytes: None,
            cpu_time_millis: None,
            result: "succeeded".to_owned(),
        })
        .expect("sample is valid");

        let edit = sample.edit_test.expect("edit observation exists");
        assert_eq!(edit.edit_to_focused_pytest_millis, 2_400);
        assert_eq!(sample.performance.milestones().first_relevant_result_millis(), Some(2_500));
    }

    #[test]
    fn sample_refuses_partial_edit_measurement_and_unplanned_fanout() {
        let edit_error = match (Some(10_u64), None::<u64>) {
            (Some(_), Some(_)) => unreachable!(),
            (None, None) => unreachable!(),
            _ => DogfoodError::new(
                "dogfood_incomplete_edit_measurement",
                "edit completion and focused pytest result must be recorded together",
            ),
        };
        assert_eq!(edit_error.code, "dogfood_incomplete_edit_measurement");

        let fanout_error = validate_coordinates(DogfoodSampleClass::Fanout, 1, Some(4))
            .expect_err("unplanned fanout is refused");
        assert_eq!(fanout_error.code, "invalid_dogfood_fanout");
    }
}
