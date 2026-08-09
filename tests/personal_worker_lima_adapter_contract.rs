use std::collections::VecDeque;
use std::io;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use smolrunner::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use smolrunner::execution_admission::{
    EpochMillis, ExecutionRequestId, ExecutionResourceLimits, HostCapacityObservation,
    RunnerProfileId,
};
use smolrunner::lima_lifecycle::{
    LimaCacheDiskId, LimaCacheDiskIdentity, LimaInstanceId, LimaInstanceIdentity,
    LimaLifecycleObservation, LimaLifecycleObservationDefinition, LimaLifecyclePolicy,
    LimaLifecycleState, LimaObservedResources, LimaProfileGeneration, LimaResourceProfile,
};
use smolrunner::lima_lifecycle_executor::{
    LimaLifecycleExecutionAction, LimaLifecycleExecutionRefusalCode, LimaLifecycleExecutor,
    LimaLifecycleObservationSource, LimaLifecycleObservationSourceError,
};
use smolrunner::lima_observation::{
    LimaArchitecture, LimaConfiguredInstance, LimaFilesystemObjectIdentity, LimaGuestObservation,
    LimaGuestResources, LimaInstanceName, LimaInstanceObservationReport, LimaObservationAdapter,
    LimaObservationClock, LimaObservationFreshness, LimaObservationRequest, LimaObservationTiming,
    LimaObservedGuest, LimaPersistentIdentity, LimaRuntimeState, LimaVmType,
};
use smolrunner::mac_availability::{
    AvailabilityRequest, EffectiveAvailabilityMode, HostPowerSource, JobActivity,
    MacAvailabilityObservation, MemoryPressure, ObservationFreshness, VmPowerState,
};
use smolrunner::macos_resource_observation::{
    lima_process_command, memory_pressure_command, power_command, swap_command,
};
use smolrunner::operator_config::{
    GuestWorkspacePath, OperatorConfig, OperatorIdlePolicy, OperatorOutputPreference,
    OperatorRemediationPreference, PersonalWorkerStateRoot,
};
use smolrunner::personal_worker_host_broker::{HostBrokerRunnerObservation, HostBrokerRunnerState};
use smolrunner::personal_worker_lima_adapter::{
    PERSONAL_WORKER_LIMA_ADAPTER_SCHEMA_VERSION, PersonalWorkerLimaAdapter,
    PersonalWorkerLimaInput, PersonalWorkerLimaRefusalCode,
};
use smolrunner::personal_worker_mac_observation::{
    PersonalWorkerMacObservation, PersonalWorkerMacObservationAdapter,
    PersonalWorkerMacObservationClock, logical_cpu_command, vm_stat_command,
};
use smolrunner::personal_worker_queue::{
    PERSONAL_WORKER_QUEUE_SCHEMA_VERSION, PERSONAL_WORKER_SCHEDULABLE_CPU_MILLIS,
    PERSONAL_WORKER_SCHEDULABLE_MEMORY_BYTES, PersonalWorkerActivityEvidence,
    PersonalWorkerCacheAccessMode, PersonalWorkerCacheLeaseState, PersonalWorkerCacheNamespace,
    PersonalWorkerJobClass, PersonalWorkerPriority, PersonalWorkerProfile,
    PersonalWorkerProfileObservation, PersonalWorkerQueueDecision, PersonalWorkerQueueEntryState,
    PersonalWorkerQueueGeneration, PersonalWorkerQueueVisibility, PersonalWorkerSelection,
};
use smolrunner::personal_worker_store::PersonalWorkerStoreRevision;
use smolrunner::personal_worker_tick::{
    PersonalWorkerTickInput, PersonalWorkerTickPlan, PersonalWorkerTickPolicy,
};
use smolrunner::process::{CommandExecutor, CommandSpec, ExecutionRecord, TimedCommandExecutor};
use smolrunner::verification_profile::{CacheId, VerificationProfileId};

const GIB: u64 = 1_024 * 1_024 * 1_024;
const LIMA_HOME: &str = "/Users/operator/.lima";
const OTHER_LIMA_HOME: &str = "/Users/operator/.lima-other";
const LIMACTL: &str = "/opt/homebrew/bin/limactl";
const CACHE_PATH: &str = "/home/runner/.cache/cargo";
const DECISION_MILLIS: u64 = 100_001;
const PRIMARY_DISK_BYTES: u64 = 80 * GIB;

fn time(value: u64) -> EpochMillis {
    EpochMillis::new(value).expect("time")
}

fn digest(character: char) -> Sha256Digest {
    Sha256Digest::parse(&format!("sha256:{}", character.to_string().repeat(64))).expect("digest")
}

fn generation(value: u64) -> LimaProfileGeneration {
    LimaProfileGeneration::new(value).expect("profile generation")
}

fn limits(cpu_millis: u32, memory_bytes: u64, pids: u32) -> ExecutionResourceLimits {
    ExecutionResourceLimits::new(cpu_millis, memory_bytes, pids).expect("limits")
}

fn identity() -> LimaInstanceIdentity {
    LimaInstanceIdentity::new(
        LimaInstanceId::parse("smolrunner").expect("instance"),
        LimaCacheDiskIdentity::new(
            LimaCacheDiskId::parse("personal-cache").expect("cache disk"),
            digest('b'),
        ),
    )
}

fn persistent_identity(character: char) -> LimaPersistentIdentity {
    LimaPersistentIdentity {
        guest_machine_id_digest: digest(character),
        root_filesystem: LimaFilesystemObjectIdentity {
            device_id: 2049,
            inode: 2,
        },
        cache_directory: LimaFilesystemObjectIdentity {
            device_id: 2049,
            inode: 3,
        },
    }
}

fn lifecycle(
    state: LimaLifecycleState,
    profile: LimaResourceProfile,
    last_activity_at: u64,
) -> LimaLifecycleObservation {
    lifecycle_at(state, profile, 100_000, last_activity_at)
}

fn lifecycle_at(
    state: LimaLifecycleState,
    profile: LimaResourceProfile,
    observed_at: u64,
    last_activity_at: u64,
) -> LimaLifecycleObservation {
    LimaLifecycleObservation::new(LimaLifecycleObservationDefinition {
        identity: identity(),
        state,
        profile,
        profile_generation: generation(3),
        observed_resources: LimaObservedResources::for_profile(profile),
        observed_at: time(observed_at),
        active_reservation_id: None,
        last_activity_at: time(last_activity_at),
        idle_deadline: time(last_activity_at + profile.idle_deadline_offset_millis()),
        graceful_stop_acknowledgement: None,
    })
    .expect("lifecycle")
}

fn cache_namespace() -> PersonalWorkerCacheNamespace {
    PersonalWorkerCacheNamespace::RepositoryBuild {
        cache_id: CacheId::parse("cargo-build").expect("cache ID"),
        repository: RepositoryRef::parse("teamleaderleo/smolrunner").expect("repository"),
        namespace_digest: digest('c'),
    }
}

fn selection() -> PersonalWorkerSelection {
    PersonalWorkerSelection {
        request_id: ExecutionRequestId::parse("request-one").expect("request"),
        repository: RepositoryRef::parse("teamleaderleo/smolrunner").expect("repository"),
        verification_profile_id: VerificationProfileId::parse("smolrunner.required")
            .expect("profile"),
        runner_profile_id: RunnerProfileId::parse("work").expect("runner profile"),
        priority: PersonalWorkerPriority::Normal,
        effective_priority_rank: 1,
        job_class: PersonalWorkerJobClass::Light,
        reserved_limits: limits(2_000, 2 * GIB, 768),
        cache_namespace: cache_namespace(),
        cache_access: PersonalWorkerCacheAccessMode::Write,
    }
}

fn visibility() -> PersonalWorkerQueueVisibility {
    PersonalWorkerQueueVisibility {
        request_id: ExecutionRequestId::parse("request-one").expect("request"),
        repository: RepositoryRef::parse("teamleaderleo/smolrunner").expect("repository"),
        commit: CommitId::parse(&"12".repeat(20)).expect("commit"),
        tree: GitTreeId::parse(&"34".repeat(20)).expect("tree"),
        verification_profile_id: VerificationProfileId::parse("smolrunner.required")
            .expect("profile"),
        runner_profile_id: RunnerProfileId::parse("work").expect("runner profile"),
        priority: PersonalWorkerPriority::Normal,
        effective_priority_rank: 1,
        age_millis: 10,
        state: PersonalWorkerQueueEntryState::Selected,
        queue_position: None,
        requested_cpu_millis: 2_000,
        requested_memory_bytes: 2 * GIB,
        reserved_cpu_millis: None,
        reserved_memory_bytes: None,
        cache_namespace: cache_namespace(),
        cache_access: PersonalWorkerCacheAccessMode::Write,
        cache_lease: PersonalWorkerCacheLeaseState::Available,
        start_time: None,
        worker_profile: PersonalWorkerProfile::Work,
    }
}

fn queue(
    current: PersonalWorkerProfile,
    desired: PersonalWorkerProfile,
    with_work: bool,
) -> PersonalWorkerQueueDecision {
    queue_at(100_000, current, desired, with_work)
}

fn queue_at(
    observed_at: u64,
    current: PersonalWorkerProfile,
    desired: PersonalWorkerProfile,
    with_work: bool,
) -> PersonalWorkerQueueDecision {
    PersonalWorkerQueueDecision {
        schema_version: PERSONAL_WORKER_QUEUE_SCHEMA_VERSION,
        generation: PersonalWorkerQueueGeneration::new(7).expect("queue generation"),
        observed_at: time(observed_at),
        profile_observation: PersonalWorkerProfileObservation::observed(current),
        activity_evidence: PersonalWorkerActivityEvidence::observed(time(observed_at - 1)),
        desired_profile: desired,
        cancel_pending_downscale: false,
        profile_change_permitted: true,
        schedulable_cpu_millis: PERSONAL_WORKER_SCHEDULABLE_CPU_MILLIS,
        schedulable_memory_bytes: PERSONAL_WORKER_SCHEDULABLE_MEMORY_BYTES,
        selected: if with_work {
            vec![selection()]
        } else {
            Vec::new()
        },
        visibility: if with_work {
            vec![visibility()]
        } else {
            Vec::new()
        },
    }
}

fn availability(
    mode: EffectiveAvailabilityMode,
    vm_power: VmPowerState,
) -> MacAvailabilityObservation {
    MacAvailabilityObservation {
        effective_mode: mode,
        vm_power,
        job_activity: JobActivity::Idle,
        freshness: ObservationFreshness::Fresh,
        host_power: HostPowerSource::Ac,
        memory_pressure: MemoryPressure::Normal,
        operator_hold: false,
    }
}

fn tick_plan(
    queue: &PersonalWorkerQueueDecision,
    lifecycle: &LimaLifecycleObservation,
    runner: Option<&HostBrokerRunnerObservation>,
    capacity: Option<HostCapacityObservation>,
    availability_request: AvailabilityRequest,
    availability: MacAvailabilityObservation,
) -> PersonalWorkerTickPlan {
    tick_plan_at(
        queue,
        lifecycle,
        runner,
        capacity,
        availability_request,
        availability,
        DECISION_MILLIS,
    )
}

fn tick_plan_at(
    queue: &PersonalWorkerQueueDecision,
    lifecycle: &LimaLifecycleObservation,
    runner: Option<&HostBrokerRunnerObservation>,
    capacity: Option<HostCapacityObservation>,
    availability_request: AvailabilityRequest,
    availability: MacAvailabilityObservation,
    decision_at: u64,
) -> PersonalWorkerTickPlan {
    PersonalWorkerTickPolicy::new(30_000, 30_000, 30_000)
        .expect("tick policy")
        .plan(PersonalWorkerTickInput {
            store_revision: PersonalWorkerStoreRevision::new(11).expect("store revision"),
            decision_at: time(decision_at),
            queue,
            lifecycle_policy: &LimaLifecyclePolicy::new(30_000).expect("lifecycle policy"),
            lifecycle: Some(lifecycle),
            runner,
            capacity,
            availability_request,
            availability: Some(availability),
        })
        .expect("tick plan")
}

fn change_profile_plan(lifecycle: &LimaLifecycleObservation) -> PersonalWorkerTickPlan {
    tick_plan(
        &queue(
            PersonalWorkerProfile::Stopped,
            PersonalWorkerProfile::Work,
            true,
        ),
        lifecycle,
        None,
        None,
        AvailabilityRequest::Active,
        availability(EffectiveAvailabilityMode::Off, VmPowerState::Stopped),
    )
}

fn start_plan(lifecycle: &LimaLifecycleObservation) -> PersonalWorkerTickPlan {
    tick_plan(
        &queue(
            PersonalWorkerProfile::Stopped,
            PersonalWorkerProfile::Work,
            true,
        ),
        lifecycle,
        None,
        Some(HostCapacityObservation::new(
            time(100_000),
            limits(8_000, 12 * GIB, 4_096),
        )),
        AvailabilityRequest::Active,
        availability(EffectiveAvailabilityMode::Off, VmPowerState::Stopped),
    )
}

fn stop_plan(
    lifecycle: &LimaLifecycleObservation,
    runner: &HostBrokerRunnerObservation,
) -> PersonalWorkerTickPlan {
    tick_plan_at(
        &queue_at(
            1_900_000,
            PersonalWorkerProfile::Work,
            PersonalWorkerProfile::Stopped,
            false,
        ),
        lifecycle,
        Some(runner),
        None,
        AvailabilityRequest::Active,
        availability(EffectiveAvailabilityMode::Active, VmPowerState::Running),
        1_900_001,
    )
}

fn config() -> OperatorConfig {
    OperatorConfig::new(
        PersonalWorkerStateRoot::parse(Path::new("/private/var/lib/smolrunner-personal"))
            .expect("state root"),
        LimaInstanceName::parse("smolrunner").expect("instance"),
        GuestWorkspacePath::parse("/home/runner/workspace").expect("workspace"),
        VerificationProfileId::parse("smolrunner.required").expect("profile"),
        AvailabilityRequest::Active,
        OperatorIdlePolicy::new(600_000, 1_800_000).expect("idle policy"),
        OperatorOutputPreference::Json,
        OperatorRemediationPreference::IncludeSuggestions,
    )
    .expect("operator config")
}

fn observation_request(lima_home: &str) -> LimaObservationRequest {
    LimaObservationRequest::new(
        LimaInstanceName::parse("smolrunner").expect("instance"),
        lima_home,
        LimaVmType::Vz,
        LimaArchitecture::Aarch64,
        CACHE_PATH,
        300,
    )
    .expect("observation request")
}

fn sealed_mac(
    state: LimaRuntimeState,
    profile: LimaResourceProfile,
    lima_home: &str,
) -> PersonalWorkerMacObservation {
    sealed_mac_at(state, profile, lima_home, 100_000)
}

fn sealed_mac_at(
    state: LimaRuntimeState,
    profile: LimaResourceProfile,
    lima_home: &str,
    observed_at_millis: u64,
) -> PersonalWorkerMacObservation {
    PersonalWorkerMacObservationAdapter::new(30_000, Duration::from_secs(5))
        .expect("Mac adapter")
        .observe(
            &config(),
            &observation_request(lima_home),
            &LimaObservationAdapter::new(LIMACTL).expect("Lima observer"),
            &MacExecutor {
                state,
                profile,
                lima_home: lima_home.to_owned(),
            },
            &MacClock::new([
                observed_at_millis - 10_000,
                observed_at_millis - 9_000,
                observed_at_millis - 1_000,
                observed_at_millis,
            ]),
        )
        .expect("sealed Mac observation")
}

struct MacClock(Mutex<VecDeque<u64>>);

impl MacClock {
    fn new(values: impl IntoIterator<Item = u64>) -> Self {
        Self(Mutex::new(values.into_iter().collect()))
    }
}

impl PersonalWorkerMacObservationClock for MacClock {
    fn unix_millis(&self) -> io::Result<u64> {
        self.0
            .lock()
            .expect("clock lock")
            .pop_front()
            .ok_or_else(|| io::Error::other("private clock fixture exhausted"))
    }
}

struct MacExecutor {
    state: LimaRuntimeState,
    profile: LimaResourceProfile,
    lima_home: String,
}

impl CommandExecutor for MacExecutor {
    fn execute(&self, _spec: &CommandSpec) -> io::Result<ExecutionRecord> {
        Err(io::Error::other("untimed Mac observation is forbidden"))
    }
}

impl TimedCommandExecutor for MacExecutor {
    fn execute_with_timeout(
        &self,
        spec: &CommandSpec,
        _timeout: Duration,
    ) -> io::Result<ExecutionRecord> {
        let argv = spec.displayed_argv();
        let stdout = self.output_for(&argv)?;
        Ok(ExecutionRecord {
            argv,
            environment_keys: spec.environment.keys().cloned().collect(),
            status: Some(0),
            success: true,
            stdout,
            stderr: String::new(),
        })
    }
}

impl MacExecutor {
    fn output_for(&self, argv: &[String]) -> io::Result<String> {
        let envelope = self.profile.envelope();
        if argv == memory_pressure_command().displayed_argv() {
            return Ok("1\n".to_owned());
        }
        if argv == swap_command().displayed_argv() {
            return Ok("total = 4096.00M used = 1024.00M free = 3072.00M (encrypted)\n".to_owned());
        }
        if argv == power_command().displayed_argv() {
            return Ok(
                "Now drawing from 'AC Power'\n -InternalBattery-0\t100%; charged;\n".to_owned(),
            );
        }
        if argv == lima_process_command().displayed_argv() {
            return Ok(" 100 1 1.0 2048 00:05 /opt/homebrew/bin/limactl\n".to_owned());
        }
        if argv == vm_stat_command().displayed_argv() {
            return Ok(concat!(
                "Mach Virtual Memory Statistics: (page size of 16384 bytes)\n",
                "Pages free: 100000.\n",
                "Pages active: 200000.\n",
                "Pages inactive: 300000.\n",
                "Pages speculative: 100000.\n",
                "Pages purgeable: 50000.\n",
            )
            .to_owned());
        }
        if argv == logical_cpu_command().displayed_argv() {
            return Ok("10\n".to_owned());
        }
        if argv.get(2).map(String::as_str) == Some("list") {
            let state = match self.state {
                LimaRuntimeState::Uninitialized => "Uninitialized",
                LimaRuntimeState::Installing => "Installing",
                LimaRuntimeState::Broken => "Broken",
                LimaRuntimeState::Stopped => "Stopped",
                LimaRuntimeState::Running => "Running",
            };
            return Ok(format!(
                "{{\"name\":\"smolrunner\",\"status\":\"{state}\",\"dir\":\"{}/smolrunner\",\"vmType\":\"vz\",\"arch\":\"aarch64\",\"cpus\":{},\"memory\":{},\"disk\":{},\"errors\":[]}}\n",
                self.lima_home, envelope.vcpus, envelope.memory_bytes, PRIMARY_DISK_BYTES,
            ));
        }
        match argv.get(5).map(String::as_str) {
            Some("/usr/bin/uname") => Ok("aarch64\n".to_owned()),
            Some("/usr/bin/getconf")
                if argv.get(6).map(String::as_str) == Some("_NPROCESSORS_ONLN") =>
            {
                Ok(format!("{}\n", envelope.vcpus))
            }
            Some("/usr/bin/getconf") if argv.get(6).map(String::as_str) == Some("PAGE_SIZE") => {
                Ok("4096\n".to_owned())
            }
            Some("/usr/bin/getconf") if argv.get(6).map(String::as_str) == Some("_PHYS_PAGES") => {
                Ok(format!("{}\n", envelope.memory_bytes / 4096))
            }
            Some("/usr/bin/sha256sum") => Ok(format!("{}  /etc/machine-id\n", "a".repeat(64))),
            Some("/usr/bin/stat") if argv.last().map(String::as_str) == Some("/") => {
                Ok("2049:2\n".to_owned())
            }
            Some("/usr/bin/stat") => Ok("2049:3\n".to_owned()),
            _ => Err(io::Error::new(
                io::ErrorKind::NotFound,
                "private Mac command fixture missing",
            )),
        }
    }
}

fn report(
    state: LimaRuntimeState,
    profile: LimaResourceProfile,
    persistent: Option<LimaPersistentIdentity>,
) -> LimaInstanceObservationReport {
    report_at(state, profile, persistent, 101)
}

fn report_at(
    state: LimaRuntimeState,
    profile: LimaResourceProfile,
    persistent: Option<LimaPersistentIdentity>,
    observed_at_unix_seconds: u64,
) -> LimaInstanceObservationReport {
    let envelope = profile.envelope();
    LimaInstanceObservationReport {
        schema_version: 1,
        instance: LimaInstanceName::parse("smolrunner").expect("instance"),
        configured: LimaConfiguredInstance {
            runtime_state: state,
            vm_type: LimaVmType::Vz,
            architecture: LimaArchitecture::Aarch64,
            cpus: envelope.vcpus,
            memory_bytes: envelope.memory_bytes,
            primary_disk_bytes: PRIMARY_DISK_BYTES,
        },
        guest: if state == LimaRuntimeState::Running {
            LimaGuestObservation::Observed(LimaObservedGuest {
                resources: LimaGuestResources {
                    architecture: LimaArchitecture::Aarch64,
                    cpus: envelope.vcpus,
                    memory_bytes: envelope.memory_bytes,
                },
                persistent_identity: persistent.expect("running identity"),
            })
        } else {
            LimaGuestObservation::NotRunning {
                runtime_state: state,
            }
        },
        timing: LimaObservationTiming {
            started_at_unix_seconds: observed_at_unix_seconds,
            observed_at_unix_seconds,
            expires_at_unix_seconds: observed_at_unix_seconds + 300,
            duration_seconds: 0,
            freshness: LimaObservationFreshness::Fresh,
        },
    }
}

struct ObservationSource(Mutex<VecDeque<LimaInstanceObservationReport>>);

impl ObservationSource {
    fn new(reports: Vec<LimaInstanceObservationReport>) -> Self {
        Self(Mutex::new(reports.into()))
    }
}

impl LimaLifecycleObservationSource for ObservationSource {
    fn observe<E, C>(
        &self,
        _request: &LimaObservationRequest,
        _executor: &E,
        _clock: &C,
    ) -> Result<LimaInstanceObservationReport, LimaLifecycleObservationSourceError>
    where
        E: CommandExecutor,
        C: LimaObservationClock,
    {
        self.0
            .lock()
            .expect("observation lock")
            .pop_front()
            .ok_or(LimaLifecycleObservationSourceError)
    }
}

#[derive(Default)]
struct LifecycleCommands(Mutex<Vec<CommandSpec>>);

impl LifecycleCommands {
    fn argv(&self) -> Vec<Vec<String>> {
        self.0
            .lock()
            .expect("command lock")
            .iter()
            .map(CommandSpec::displayed_argv)
            .collect()
    }
}

impl CommandExecutor for LifecycleCommands {
    fn execute(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord> {
        self.0.lock().expect("command lock").push(spec.clone());
        Ok(ExecutionRecord {
            argv: spec.displayed_argv(),
            environment_keys: spec.environment.keys().cloned().collect(),
            status: Some(0),
            success: true,
            stdout: String::new(),
            stderr: String::new(),
        })
    }
}

struct FixedClock(u64);

impl LimaObservationClock for FixedClock {
    fn unix_seconds(&self) -> io::Result<u64> {
        Ok(self.0)
    }
}

fn execute(
    plan: &PersonalWorkerTickPlan,
    lifecycle: &LimaLifecycleObservation,
    mac: &PersonalWorkerMacObservation,
    expected: &LimaPersistentIdentity,
    request: &LimaObservationRequest,
    reports: Vec<LimaInstanceObservationReport>,
    commands: &LifecycleCommands,
) -> Result<
    smolrunner::personal_worker_lima_adapter::PersonalWorkerLimaExecution,
    smolrunner::personal_worker_lima_adapter::PersonalWorkerLimaFailure,
> {
    execute_at(
        plan, lifecycle, mac, expected, request, reports, commands, 101,
    )
}

#[allow(clippy::too_many_arguments)]
fn execute_at(
    plan: &PersonalWorkerTickPlan,
    lifecycle: &LimaLifecycleObservation,
    mac: &PersonalWorkerMacObservation,
    expected: &LimaPersistentIdentity,
    request: &LimaObservationRequest,
    reports: Vec<LimaInstanceObservationReport>,
    commands: &LifecycleCommands,
    execution_unix_seconds: u64,
) -> Result<
    smolrunner::personal_worker_lima_adapter::PersonalWorkerLimaExecution,
    smolrunner::personal_worker_lima_adapter::PersonalWorkerLimaFailure,
> {
    PersonalWorkerLimaAdapter.execute(
        PersonalWorkerLimaInput {
            plan,
            current_store_revision: PersonalWorkerStoreRevision::new(11).expect("store revision"),
            current_queue_generation: PersonalWorkerQueueGeneration::new(7)
                .expect("queue generation"),
            lifecycle,
            mac,
            expected_persistent_identity: expected,
            observation_request: request,
        },
        &LimaLifecycleExecutor::new(
            LIMACTL,
            LIMA_HOME,
            LimaInstanceName::parse("smolrunner").expect("instance"),
        )
        .expect("lifecycle executor"),
        &ObservationSource::new(reports),
        commands,
        &FixedClock(execution_unix_seconds),
    )
}

#[test]
fn sealed_tick_variants_delegate_only_fixed_lifecycle_sequences() {
    let expected = persistent_identity('a');
    let request = observation_request(LIMA_HOME);

    let profile_lifecycle = lifecycle(
        LimaLifecycleState::Stopped,
        LimaResourceProfile::Interactive,
        99_999,
    );
    let profile_plan = change_profile_plan(&profile_lifecycle);
    let profile_mac = sealed_mac(
        LimaRuntimeState::Stopped,
        LimaResourceProfile::Interactive,
        LIMA_HOME,
    );
    let profile_commands = LifecycleCommands::default();
    let profile_execution = execute(
        &profile_plan,
        &profile_lifecycle,
        &profile_mac,
        &expected,
        &request,
        vec![
            report(LimaRuntimeState::Stopped, LimaResourceProfile::Work, None),
            report(
                LimaRuntimeState::Running,
                LimaResourceProfile::Work,
                Some(expected.clone()),
            ),
        ],
        &profile_commands,
    )
    .expect("profile execution");
    assert_eq!(
        profile_commands.argv(),
        vec![
            vec![
                LIMACTL.to_owned(),
                "edit".to_owned(),
                "--tty=false".to_owned(),
                "--cpus".to_owned(),
                "8".to_owned(),
                "--memory".to_owned(),
                "10".to_owned(),
                "smolrunner".to_owned(),
            ],
            vec![
                LIMACTL.to_owned(),
                "start".to_owned(),
                "smolrunner".to_owned(),
            ],
        ]
    );
    assert_eq!(
        profile_execution.lifecycle().receipt().action,
        LimaLifecycleExecutionAction::ChangeProfile
    );

    let start_lifecycle = lifecycle(
        LimaLifecycleState::Stopped,
        LimaResourceProfile::Work,
        99_999,
    );
    let start_plan = start_plan(&start_lifecycle);
    let start_mac = sealed_mac(
        LimaRuntimeState::Stopped,
        LimaResourceProfile::Work,
        LIMA_HOME,
    );
    let start_commands = LifecycleCommands::default();
    let start_execution = execute(
        &start_plan,
        &start_lifecycle,
        &start_mac,
        &expected,
        &request,
        vec![report(
            LimaRuntimeState::Running,
            LimaResourceProfile::Work,
            Some(expected.clone()),
        )],
        &start_commands,
    )
    .expect("start execution");
    assert_eq!(
        start_commands.argv(),
        vec![vec![
            LIMACTL.to_owned(),
            "start".to_owned(),
            "smolrunner".to_owned(),
        ]]
    );
    assert_eq!(
        start_execution.lifecycle().receipt().action,
        LimaLifecycleExecutionAction::Start
    );

    let stop_lifecycle = lifecycle_at(
        LimaLifecycleState::Running,
        LimaResourceProfile::Work,
        1_900_000,
        1,
    );
    let runner = HostBrokerRunnerObservation::new(
        LimaInstanceId::parse("smolrunner").expect("instance"),
        generation(3),
        time(1_900_000),
        HostBrokerRunnerState::IdleReady,
    );
    let stop_plan = stop_plan(&stop_lifecycle, &runner);
    let stop_mac = sealed_mac_at(
        LimaRuntimeState::Running,
        LimaResourceProfile::Work,
        LIMA_HOME,
        1_900_000,
    );
    let stop_commands = LifecycleCommands::default();
    let stop_execution = execute_at(
        &stop_plan,
        &stop_lifecycle,
        &stop_mac,
        &expected,
        &request,
        vec![report_at(
            LimaRuntimeState::Stopped,
            LimaResourceProfile::Work,
            None,
            1_901,
        )],
        &stop_commands,
        1_901,
    )
    .expect("stop execution");
    assert_eq!(
        stop_commands.argv(),
        vec![vec![
            LIMACTL.to_owned(),
            "stop".to_owned(),
            "smolrunner".to_owned(),
        ]]
    );
    assert!(matches!(
        stop_execution.lifecycle().receipt().action,
        LimaLifecycleExecutionAction::Stop {
            target_after_stop: PersonalWorkerProfile::Stopped
        }
    ));
    assert_eq!(
        stop_execution.schema_version(),
        PERSONAL_WORKER_LIMA_ADAPTER_SCHEMA_VERSION
    );
    assert_eq!(stop_execution.store_revision().get(), 11);
}

#[test]
fn unsupported_tick_and_durable_drift_execute_zero_commands() {
    let lifecycle = lifecycle(
        LimaLifecycleState::Stopped,
        LimaResourceProfile::Work,
        99_999,
    );
    let blocked = tick_plan(
        &queue(
            PersonalWorkerProfile::Stopped,
            PersonalWorkerProfile::Work,
            true,
        ),
        &lifecycle,
        None,
        Some(HostCapacityObservation::new(
            time(100_000),
            limits(8_000, 12 * GIB, 4_096),
        )),
        AvailabilityRequest::Off,
        availability(EffectiveAvailabilityMode::Active, VmPowerState::Stopped),
    );
    let mac = sealed_mac(
        LimaRuntimeState::Stopped,
        LimaResourceProfile::Work,
        LIMA_HOME,
    );
    let request = observation_request(LIMA_HOME);
    let commands = LifecycleCommands::default();
    let unsupported = execute(
        &blocked,
        &lifecycle,
        &mac,
        &persistent_identity('a'),
        &request,
        Vec::new(),
        &commands,
    )
    .expect_err("unsupported tick");
    assert_eq!(
        unsupported.lifecycle_code,
        Some(LimaLifecycleExecutionRefusalCode::UnsupportedAction)
    );
    assert!(commands.argv().is_empty());

    let plan = start_plan(&lifecycle);
    let drift_commands = LifecycleCommands::default();
    let drift = PersonalWorkerLimaAdapter
        .execute(
            PersonalWorkerLimaInput {
                plan: &plan,
                current_store_revision: PersonalWorkerStoreRevision::new(12)
                    .expect("drifted revision"),
                current_queue_generation: PersonalWorkerQueueGeneration::new(7)
                    .expect("queue generation"),
                lifecycle: &lifecycle,
                mac: &mac,
                expected_persistent_identity: &persistent_identity('a'),
                observation_request: &request,
            },
            &LimaLifecycleExecutor::new(
                LIMACTL,
                LIMA_HOME,
                LimaInstanceName::parse("smolrunner").expect("instance"),
            )
            .expect("lifecycle executor"),
            &ObservationSource::new(Vec::new()),
            &drift_commands,
            &FixedClock(101),
        )
        .expect_err("stale store revision");
    assert_eq!(
        drift.lifecycle_code,
        Some(LimaLifecycleExecutionRefusalCode::BrokerStateRevisionMismatch)
    );
    assert!(drift_commands.argv().is_empty());
}

#[test]
fn source_and_persistent_identity_drift_refuse_before_mutation_and_stay_private() {
    let stopped = lifecycle(
        LimaLifecycleState::Stopped,
        LimaResourceProfile::Work,
        99_999,
    );
    let plan = start_plan(&stopped);
    let mac = sealed_mac(
        LimaRuntimeState::Stopped,
        LimaResourceProfile::Work,
        OTHER_LIMA_HOME,
    );
    let commands = LifecycleCommands::default();
    let source = execute(
        &plan,
        &stopped,
        &mac,
        &persistent_identity('a'),
        &observation_request(LIMA_HOME),
        Vec::new(),
        &commands,
    )
    .expect_err("source drift");
    assert_eq!(
        source.code,
        PersonalWorkerLimaRefusalCode::LimaSourceMismatch
    );
    assert!(commands.argv().is_empty());
    let debug = format!("{source:?}");
    assert!(!debug.contains(LIMA_HOME));
    assert!(!debug.contains(OTHER_LIMA_HOME));

    let running = lifecycle_at(
        LimaLifecycleState::Running,
        LimaResourceProfile::Work,
        1_900_000,
        1,
    );
    let runner = HostBrokerRunnerObservation::new(
        LimaInstanceId::parse("smolrunner").expect("instance"),
        generation(3),
        time(1_900_000),
        HostBrokerRunnerState::IdleReady,
    );
    let stop = stop_plan(&running, &runner);
    let running_mac = sealed_mac_at(
        LimaRuntimeState::Running,
        LimaResourceProfile::Work,
        LIMA_HOME,
        1_900_000,
    );
    let identity_commands = LifecycleCommands::default();
    let identity = execute_at(
        &stop,
        &running,
        &running_mac,
        &persistent_identity('d'),
        &observation_request(LIMA_HOME),
        Vec::new(),
        &identity_commands,
        1_901,
    )
    .expect_err("persistent identity drift");
    assert_eq!(
        identity.code,
        PersonalWorkerLimaRefusalCode::PersistentIdentityMismatch
    );
    assert!(identity_commands.argv().is_empty());
}

#[test]
fn module_adds_no_command_or_general_host_authority() {
    let source = include_str!("../src/personal_worker_lima_adapter.rs");
    for forbidden in [
        "CommandSpec",
        "std::process",
        "std::fs",
        "std::net",
        "sh -c",
        "secret_environment",
        "argument(",
        "limactl",
    ] {
        assert!(!source.contains(forbidden), "adapter contains {forbidden}");
    }
    assert!(source.contains("AcceptedLimaLifecycleAction::from_personal_worker_tick"));
    assert!(source.contains("LimaLifecycleExecutionInput"));
}
