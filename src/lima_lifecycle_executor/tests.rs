use std::collections::VecDeque;
use std::io;
use std::sync::Mutex;
use std::time::Duration;

use super::*;
use crate::artifact::Sha256Digest;
use crate::execution_admission::ReservationId;
use crate::lima_lifecycle::{
    GracefulStopAcknowledgement, LimaCacheDiskId, LimaCacheDiskIdentity, LimaDrainAcknowledgement,
    LimaInstanceId, LimaLifecycleObservationDefinition, LimaObservedResources,
};
use crate::lima_observation::{
    LimaArchitecture, LimaConfiguredInstance, LimaFilesystemObjectIdentity, LimaGuestResources,
    LimaInstanceObservationReport, LimaObservationTiming, LimaObservedGuest, LimaVmType,
};
use crate::process::CommandValue;

const PRIVATE_HOME: &str = "/Users/operator/.lima";
const LIMACTL: &str = "/opt/homebrew/bin/limactl";
const DECISION_MILLIS: u64 = 100_000;
const PRIMARY_DISK_BYTES: u64 = 80 * 1024 * 1024 * 1024;

fn epoch(value: u64) -> EpochMillis {
    EpochMillis::new(value).expect("epoch")
}

fn generation(value: u64) -> LimaProfileGeneration {
    LimaProfileGeneration::new(value).expect("generation")
}

fn digest(character: char) -> Sha256Digest {
    Sha256Digest::parse(&format!("sha256:{}", character.to_string().repeat(64))).expect("digest")
}

fn cache_disk() -> LimaCacheDiskIdentity {
    LimaCacheDiskIdentity::new(
        LimaCacheDiskId::parse("smolrunner-cache").expect("cache disk ID"),
        digest('a'),
    )
}

fn lifecycle_identity() -> LimaInstanceIdentity {
    LimaInstanceIdentity::new(
        LimaInstanceId::parse("smolrunner").expect("instance ID"),
        cache_disk(),
    )
}

fn persistent_identity(character: char) -> LimaPersistentIdentity {
    LimaPersistentIdentity {
        guest_machine_id_digest: digest(character),
        root_filesystem: LimaFilesystemObjectIdentity {
            device_id: 1,
            inode: 2,
        },
        cache_directory: LimaFilesystemObjectIdentity {
            device_id: 1,
            inode: 3,
        },
    }
}

fn lifecycle(
    state: LimaLifecycleState,
    profile: LimaResourceProfile,
    generation_value: u64,
    active_reservation: bool,
) -> LimaLifecycleObservation {
    let observed_at = DECISION_MILLIS;
    let last_activity_at = if state == LimaLifecycleState::Stopped {
        observed_at - 2
    } else {
        observed_at
    };
    let mut definition = LimaLifecycleObservationDefinition {
        identity: lifecycle_identity(),
        state,
        profile,
        profile_generation: generation(generation_value),
        observed_resources: LimaObservedResources::for_profile(profile),
        observed_at: epoch(observed_at),
        active_reservation_id: active_reservation
            .then(|| ReservationId::parse("reservation-active").expect("reservation ID")),
        last_activity_at: epoch(last_activity_at),
        idle_deadline: epoch(last_activity_at + profile.idle_deadline_offset_millis()),
        graceful_stop_acknowledgement: None,
    };
    if state == LimaLifecycleState::Stopped {
        definition.graceful_stop_acknowledgement = Some(GracefulStopAcknowledgement::new(
            epoch(observed_at - 1),
            generation(generation_value),
            cache_disk(),
            LimaDrainAcknowledgement::Completed,
        ));
    }
    LimaLifecycleObservation::new(definition).expect("lifecycle observation")
}

fn report(
    state: LimaRuntimeState,
    profile: LimaResourceProfile,
    persistent: Option<LimaPersistentIdentity>,
) -> LimaInstanceObservationReport {
    let envelope = profile.envelope();
    let guest = match state {
        LimaRuntimeState::Running => LimaGuestObservation::Observed(LimaObservedGuest {
            resources: LimaGuestResources {
                architecture: LimaArchitecture::Aarch64,
                cpus: envelope.vcpus,
                memory_bytes: envelope.memory_bytes,
            },
            persistent_identity: persistent.expect("running persistent identity"),
        }),
        _ => LimaGuestObservation::NotRunning {
            runtime_state: state,
        },
    };
    LimaInstanceObservationReport {
        schema_version: 1,
        instance: LimaInstanceName::parse("smolrunner").expect("instance name"),
        configured: LimaConfiguredInstance {
            runtime_state: state,
            vm_type: LimaVmType::Vz,
            architecture: LimaArchitecture::Aarch64,
            cpus: envelope.vcpus,
            memory_bytes: envelope.memory_bytes,
            primary_disk_bytes: PRIMARY_DISK_BYTES,
        },
        guest,
        timing: LimaObservationTiming {
            started_at_unix_seconds: 99,
            observed_at_unix_seconds: 100,
            expires_at_unix_seconds: 200,
            duration_seconds: 1,
            freshness: LimaObservationFreshness::Fresh,
        },
    }
}

fn request() -> LimaObservationRequest {
    LimaObservationRequest::new(
        LimaInstanceName::parse("smolrunner").expect("instance name"),
        PRIVATE_HOME,
        LimaVmType::Vz,
        LimaArchitecture::Aarch64,
        "/home/runner/.cache/smolrunner",
        300,
    )
    .expect("request")
}

fn executor() -> LimaLifecycleExecutor {
    LimaLifecycleExecutor::new(
        LIMACTL,
        PRIVATE_HOME,
        LimaInstanceName::parse("smolrunner").expect("instance name"),
    )
    .expect("executor")
}

fn accepted(action: HostBrokerAction) -> AcceptedLimaLifecycleAction {
    AcceptedLimaLifecycleAction {
        state_revision: HostBrokerStateRevision::new(7).expect("revision"),
        queue_generation: PersonalWorkerQueueGeneration::new(11).expect("queue generation"),
        decision_at: epoch(DECISION_MILLIS),
        action,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecutorMode {
    Match,
    IdentityMismatch,
    Failed,
    Oversized,
    TimedOut,
}

struct ScriptedExecutor {
    mode: ExecutorMode,
    calls: Mutex<Vec<(CommandSpec, Duration)>>,
}

impl ScriptedExecutor {
    fn new(mode: ExecutorMode) -> Self {
        Self {
            mode,
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<CommandSpec> {
        self.calls
            .lock()
            .expect("calls lock")
            .iter()
            .map(|(command, _)| command.clone())
            .collect()
    }

    fn timeouts(&self) -> Vec<Duration> {
        self.calls
            .lock()
            .expect("calls lock")
            .iter()
            .map(|(_, timeout)| *timeout)
            .collect()
    }
}

impl CommandExecutor for ScriptedExecutor {
    fn execute(&self, _spec: &CommandSpec) -> io::Result<ExecutionRecord> {
        Err(io::Error::other(
            "untimed Lima lifecycle mutation is forbidden",
        ))
    }
}

impl TimedCommandExecutor for ScriptedExecutor {
    fn execute_with_timeout(
        &self,
        spec: &CommandSpec,
        timeout: Duration,
    ) -> io::Result<ExecutionRecord> {
        self.calls
            .lock()
            .expect("calls lock")
            .push((spec.clone(), timeout));
        if self.mode == ExecutorMode::TimedOut {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "reviewed timeout elapsed",
            ));
        }
        let mut argv = spec.displayed_argv();
        if self.mode == ExecutorMode::IdentityMismatch {
            argv.push("unexpected".to_owned());
        }
        let failed = self.mode == ExecutorMode::Failed;
        Ok(ExecutionRecord {
            argv,
            environment_keys: spec.environment.keys().cloned().collect(),
            status: Some(if failed { 1 } else { 0 }),
            success: !failed,
            stdout: if self.mode == ExecutorMode::Oversized {
                "x".repeat(MAX_LIMA_LIFECYCLE_EXECUTOR_OUTPUT_BYTES + 1)
            } else {
                String::new()
            },
            stderr: String::new(),
        })
    }
}

struct ScriptedObservationSource {
    reports: Mutex<VecDeque<LimaInstanceObservationReport>>,
}

impl ScriptedObservationSource {
    fn new(reports: Vec<LimaInstanceObservationReport>) -> Self {
        Self {
            reports: Mutex::new(reports.into()),
        }
    }

    fn remaining(&self) -> usize {
        self.reports.lock().expect("reports lock").len()
    }
}

impl LimaLifecycleObservationSource for ScriptedObservationSource {
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
        self.reports
            .lock()
            .expect("reports lock")
            .pop_front()
            .ok_or(LimaLifecycleObservationSourceError)
    }
}

#[derive(Debug, Clone, Copy)]
struct FixedClock(u64);

impl LimaObservationClock for FixedClock {
    fn unix_seconds(&self) -> io::Result<u64> {
        Ok(self.0)
    }
}

struct RecordingJournal {
    checkpoints: Vec<LimaLifecycleExecutionCheckpoint>,
    fail_at: Option<LimaLifecycleExecutionCheckpoint>,
}

impl RecordingJournal {
    fn new(fail_at: Option<LimaLifecycleExecutionCheckpoint>) -> Self {
        Self {
            checkpoints: Vec::new(),
            fail_at,
        }
    }
}

impl LimaLifecycleExecutionJournal for RecordingJournal {
    fn checkpoint(
        &mut self,
        checkpoint: LimaLifecycleExecutionCheckpoint,
    ) -> Result<(), LimaLifecycleExecutionJournalError> {
        self.checkpoints.push(checkpoint);
        if self.fail_at == Some(checkpoint) {
            Err(LimaLifecycleExecutionJournalError)
        } else {
            Ok(())
        }
    }
}

fn input<'a>(
    accepted: &'a AcceptedLimaLifecycleAction,
    lifecycle: &'a LimaLifecycleObservation,
    current: &'a LimaInstanceObservationReport,
    persistent: &'a LimaPersistentIdentity,
    request: &'a LimaObservationRequest,
) -> LimaLifecycleExecutionInput<'a> {
    LimaLifecycleExecutionInput {
        accepted,
        current_broker_state_revision: accepted.state_revision,
        current_queue_generation: accepted.queue_generation,
        lifecycle,
        current,
        expected_persistent_identity: persistent,
        observation_request: request,
    }
}

#[test]
fn start_uses_one_fixed_command_and_verifies_running_identity() {
    let persistent = persistent_identity('b');
    let lifecycle = lifecycle(
        LimaLifecycleState::Stopped,
        LimaResourceProfile::Interactive,
        1,
        false,
    );
    let current = report(
        LimaRuntimeState::Stopped,
        LimaResourceProfile::Interactive,
        None,
    );
    let action = accepted(HostBrokerAction::Start {
        identity: lifecycle.identity().clone(),
        profile: LimaResourceProfile::Interactive,
        profile_generation: generation(1),
    });
    let request = request();
    let observations = ScriptedObservationSource::new(vec![report(
        LimaRuntimeState::Running,
        LimaResourceProfile::Interactive,
        Some(persistent.clone()),
    )]);
    let commands = ScriptedExecutor::new(ExecutorMode::Match);

    let execution = executor()
        .execute(
            input(&action, &lifecycle, &current, &persistent, &request),
            &observations,
            &commands,
            &FixedClock(100),
        )
        .expect("start execution");

    assert_eq!(execution.receipt().after_state, LimaLifecycleState::Running);
    assert_eq!(execution.receipt().after_generation, generation(1));
    assert_eq!(execution.receipt().persistent_identity, persistent);
    let calls = commands.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].displayed_argv(),
        vec![LIMACTL, "start", "smolrunner"]
    );
    assert_eq!(commands.timeouts(), vec![LIMA_LIFECYCLE_COMMAND_TIMEOUT]);
    assert_eq!(
        calls[0].environment.keys().cloned().collect::<Vec<_>>(),
        vec!["HOME", "LANG", "LC_ALL", "LIMA_HOME"]
    );
    assert_eq!(
        calls[0].environment.get("HOME"),
        Some(&CommandValue::Plain(LIMACTL_SAFE_HOME.to_owned()))
    );
    let public = serde_json::to_string(&execution).expect("public execution JSON");
    assert!(!public.contains(PRIVATE_HOME));
    assert!(!public.contains(LIMACTL));
    assert!(!public.contains("argv"));
    assert!(!format!("{execution:?}").contains(PRIVATE_HOME));
}

#[test]
fn stop_to_stopped_is_one_fixed_command_and_preserves_disk_identity() {
    let persistent = persistent_identity('b');
    let lifecycle = lifecycle(
        LimaLifecycleState::Running,
        LimaResourceProfile::Work,
        3,
        false,
    );
    let current = report(
        LimaRuntimeState::Running,
        LimaResourceProfile::Work,
        Some(persistent.clone()),
    );
    let action = accepted(HostBrokerAction::Stop {
        identity: lifecycle.identity().clone(),
        current_profile: LimaResourceProfile::Work,
        profile_generation: generation(3),
        target_after_stop: PersonalWorkerProfile::Stopped,
    });
    let request = request();
    let observations = ScriptedObservationSource::new(vec![report(
        LimaRuntimeState::Stopped,
        LimaResourceProfile::Work,
        None,
    )]);
    let commands = ScriptedExecutor::new(ExecutorMode::Match);

    let execution = executor()
        .execute(
            input(&action, &lifecycle, &current, &persistent, &request),
            &observations,
            &commands,
            &FixedClock(100),
        )
        .expect("stop execution");

    assert_eq!(execution.receipt().after_state, LimaLifecycleState::Stopped);
    assert_eq!(execution.receipt().primary_disk_bytes, PRIMARY_DISK_BYTES);
    assert_eq!(commands.calls().len(), 1);
    assert_eq!(
        commands.calls()[0].displayed_argv(),
        vec![LIMACTL, "stop", "smolrunner"]
    );
    assert_eq!(commands.timeouts(), vec![LIMA_LIFECYCLE_COMMAND_TIMEOUT]);
}

#[test]
fn change_profile_edits_fixed_values_and_remains_stopped() {
    let persistent = persistent_identity('b');
    let lifecycle = lifecycle(
        LimaLifecycleState::Stopped,
        LimaResourceProfile::Work,
        4,
        false,
    );
    let current = report(LimaRuntimeState::Stopped, LimaResourceProfile::Work, None);
    let action = accepted(HostBrokerAction::ChangeProfile {
        identity: lifecycle.identity().clone(),
        from_profile: LimaResourceProfile::Work,
        to_profile: LimaResourceProfile::Interactive,
        current_generation: generation(4),
        next_generation: generation(5),
    });
    let request = request();
    let observations = ScriptedObservationSource::new(vec![report(
        LimaRuntimeState::Stopped,
        LimaResourceProfile::Interactive,
        None,
    )]);
    let commands = ScriptedExecutor::new(ExecutorMode::Match);

    let execution = executor()
        .execute(
            input(&action, &lifecycle, &current, &persistent, &request),
            &observations,
            &commands,
            &FixedClock(100),
        )
        .expect("profile execution");

    assert_eq!(execution.receipt().after_generation, generation(5));
    assert_eq!(
        execution.receipt().after_profile,
        LimaResourceProfile::Interactive
    );
    assert_eq!(execution.receipt().after_state, LimaLifecycleState::Stopped);
    let calls = commands.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(commands.timeouts(), vec![LIMA_LIFECYCLE_COMMAND_TIMEOUT]);
    assert_eq!(
        calls[0].displayed_argv(),
        vec![
            LIMACTL,
            "edit",
            "--tty=false",
            "--cpus",
            "4",
            "--memory",
            "3",
            "smolrunner",
        ]
    );
    assert!(
        !calls
            .iter()
            .any(|call| call.displayed_argv().contains(&"start".to_owned()))
    );
}

#[test]
fn stop_to_profile_runs_the_exact_stop_edit_start_sequence() {
    let persistent = persistent_identity('b');
    let lifecycle = lifecycle(
        LimaLifecycleState::Running,
        LimaResourceProfile::Work,
        3,
        false,
    );
    let current = report(
        LimaRuntimeState::Running,
        LimaResourceProfile::Work,
        Some(persistent.clone()),
    );
    let action = accepted(HostBrokerAction::Stop {
        identity: lifecycle.identity().clone(),
        current_profile: LimaResourceProfile::Work,
        profile_generation: generation(3),
        target_after_stop: PersonalWorkerProfile::Interactive,
    });
    let observations = ScriptedObservationSource::new(vec![
        report(LimaRuntimeState::Stopped, LimaResourceProfile::Work, None),
        report(
            LimaRuntimeState::Stopped,
            LimaResourceProfile::Interactive,
            None,
        ),
        report(
            LimaRuntimeState::Running,
            LimaResourceProfile::Interactive,
            Some(persistent.clone()),
        ),
    ]);
    let commands = ScriptedExecutor::new(ExecutorMode::Match);
    let execution = executor()
        .execute(
            input(&action, &lifecycle, &current, &persistent, &request()),
            &observations,
            &commands,
            &FixedClock(100),
        )
        .expect("stop profile transition");

    assert_eq!(execution.receipt().after_state, LimaLifecycleState::Running);
    assert_eq!(
        execution.receipt().after_profile,
        LimaResourceProfile::Interactive
    );
    assert_eq!(execution.receipt().after_generation, generation(4));
    assert_eq!(
        commands
            .calls()
            .iter()
            .map(CommandSpec::displayed_argv)
            .collect::<Vec<_>>(),
        vec![
            vec![LIMACTL, "stop", "smolrunner"],
            vec![
                LIMACTL,
                "edit",
                "--tty=false",
                "--cpus",
                "4",
                "--memory",
                "3",
                "smolrunner",
            ],
            vec![LIMACTL, "start", "smolrunner"],
        ]
    );
    assert_eq!(commands.timeouts(), vec![LIMA_LIFECYCLE_COMMAND_TIMEOUT; 3]);
}

#[test]
fn durable_checkpoint_order_brackets_every_composite_command_and_final_verification() {
    let persistent = persistent_identity('b');
    let lifecycle = lifecycle(
        LimaLifecycleState::Running,
        LimaResourceProfile::Work,
        3,
        false,
    );
    let current = report(
        LimaRuntimeState::Running,
        LimaResourceProfile::Work,
        Some(persistent.clone()),
    );
    let action = accepted(HostBrokerAction::Stop {
        identity: lifecycle.identity().clone(),
        current_profile: LimaResourceProfile::Work,
        profile_generation: generation(3),
        target_after_stop: PersonalWorkerProfile::Interactive,
    });
    let observations = ScriptedObservationSource::new(vec![
        report(LimaRuntimeState::Stopped, LimaResourceProfile::Work, None),
        report(
            LimaRuntimeState::Stopped,
            LimaResourceProfile::Interactive,
            None,
        ),
        report(
            LimaRuntimeState::Running,
            LimaResourceProfile::Interactive,
            Some(persistent.clone()),
        ),
    ]);
    let commands = ScriptedExecutor::new(ExecutorMode::Match);
    let mut journal = RecordingJournal::new(None);
    executor()
        .execute_with_journal(
            input(&action, &lifecycle, &current, &persistent, &request()),
            &observations,
            &commands,
            &FixedClock(100),
            &mut journal,
        )
        .expect("checkpointed transition");
    assert_eq!(
        journal.checkpoints,
        vec![
            LimaLifecycleExecutionCheckpoint::StopStarted,
            LimaLifecycleExecutionCheckpoint::StopCompleted,
            LimaLifecycleExecutionCheckpoint::EditStarted,
            LimaLifecycleExecutionCheckpoint::EditCompleted,
            LimaLifecycleExecutionCheckpoint::StartStarted,
            LimaLifecycleExecutionCheckpoint::StartCompleted,
            LimaLifecycleExecutionCheckpoint::VerifyStarted,
        ]
    );
}

#[test]
fn durable_checkpoint_order_covers_each_single_command_sequence() {
    let persistent = persistent_identity('b');

    let stopped_lifecycle = lifecycle(
        LimaLifecycleState::Stopped,
        LimaResourceProfile::Interactive,
        1,
        false,
    );
    let stopped_current = report(
        LimaRuntimeState::Stopped,
        LimaResourceProfile::Interactive,
        None,
    );
    let start = accepted(HostBrokerAction::Start {
        identity: stopped_lifecycle.identity().clone(),
        profile: LimaResourceProfile::Interactive,
        profile_generation: generation(1),
    });
    let mut start_journal = RecordingJournal::new(None);
    executor()
        .execute_with_journal(
            input(
                &start,
                &stopped_lifecycle,
                &stopped_current,
                &persistent,
                &request(),
            ),
            &ScriptedObservationSource::new(vec![report(
                LimaRuntimeState::Running,
                LimaResourceProfile::Interactive,
                Some(persistent.clone()),
            )]),
            &ScriptedExecutor::new(ExecutorMode::Match),
            &FixedClock(100),
            &mut start_journal,
        )
        .expect("checkpointed start");
    assert_eq!(
        start_journal.checkpoints,
        vec![
            LimaLifecycleExecutionCheckpoint::StartStarted,
            LimaLifecycleExecutionCheckpoint::StartCompleted,
            LimaLifecycleExecutionCheckpoint::VerifyStarted,
        ]
    );

    let running_lifecycle = lifecycle(
        LimaLifecycleState::Running,
        LimaResourceProfile::Work,
        3,
        false,
    );
    let running_current = report(
        LimaRuntimeState::Running,
        LimaResourceProfile::Work,
        Some(persistent.clone()),
    );
    let stop = accepted(HostBrokerAction::Stop {
        identity: running_lifecycle.identity().clone(),
        current_profile: LimaResourceProfile::Work,
        profile_generation: generation(3),
        target_after_stop: PersonalWorkerProfile::Stopped,
    });
    let mut stop_journal = RecordingJournal::new(None);
    executor()
        .execute_with_journal(
            input(
                &stop,
                &running_lifecycle,
                &running_current,
                &persistent,
                &request(),
            ),
            &ScriptedObservationSource::new(vec![report(
                LimaRuntimeState::Stopped,
                LimaResourceProfile::Work,
                None,
            )]),
            &ScriptedExecutor::new(ExecutorMode::Match),
            &FixedClock(100),
            &mut stop_journal,
        )
        .expect("checkpointed stop");
    assert_eq!(
        stop_journal.checkpoints,
        vec![
            LimaLifecycleExecutionCheckpoint::StopStarted,
            LimaLifecycleExecutionCheckpoint::StopCompleted,
            LimaLifecycleExecutionCheckpoint::VerifyStarted,
        ]
    );

    let edit_lifecycle = lifecycle(
        LimaLifecycleState::Stopped,
        LimaResourceProfile::Work,
        4,
        false,
    );
    let edit_current = report(LimaRuntimeState::Stopped, LimaResourceProfile::Work, None);
    let edit = accepted(HostBrokerAction::ChangeProfile {
        identity: edit_lifecycle.identity().clone(),
        from_profile: LimaResourceProfile::Work,
        to_profile: LimaResourceProfile::Interactive,
        current_generation: generation(4),
        next_generation: generation(5),
    });
    let mut edit_journal = RecordingJournal::new(None);
    executor()
        .execute_with_journal(
            input(
                &edit,
                &edit_lifecycle,
                &edit_current,
                &persistent,
                &request(),
            ),
            &ScriptedObservationSource::new(vec![report(
                LimaRuntimeState::Stopped,
                LimaResourceProfile::Interactive,
                None,
            )]),
            &ScriptedExecutor::new(ExecutorMode::Match),
            &FixedClock(100),
            &mut edit_journal,
        )
        .expect("checkpointed edit");
    assert_eq!(
        edit_journal.checkpoints,
        vec![
            LimaLifecycleExecutionCheckpoint::EditStarted,
            LimaLifecycleExecutionCheckpoint::EditCompleted,
            LimaLifecycleExecutionCheckpoint::VerifyStarted,
        ]
    );
}

#[test]
fn durable_checkpoint_failure_blocks_before_or_immediately_after_the_command_boundary() {
    let persistent = persistent_identity('b');
    let lifecycle = lifecycle(
        LimaLifecycleState::Stopped,
        LimaResourceProfile::Interactive,
        1,
        false,
    );
    let current = report(
        LimaRuntimeState::Stopped,
        LimaResourceProfile::Interactive,
        None,
    );
    let action = accepted(HostBrokerAction::Start {
        identity: lifecycle.identity().clone(),
        profile: LimaResourceProfile::Interactive,
        profile_generation: generation(1),
    });

    let before_commands = ScriptedExecutor::new(ExecutorMode::Match);
    let mut before_journal =
        RecordingJournal::new(Some(LimaLifecycleExecutionCheckpoint::StartStarted));
    let before = executor()
        .execute_with_journal(
            input(&action, &lifecycle, &current, &persistent, &request()),
            &ScriptedObservationSource::new(Vec::new()),
            &before_commands,
            &FixedClock(100),
            &mut before_journal,
        )
        .expect_err("pre-command checkpoint failure");
    assert_eq!(
        before.code,
        LimaLifecycleExecutionRefusalCode::CheckpointFailed
    );
    assert_eq!(before.phase, LimaLifecycleExecutionPhase::Start);
    assert!(before_commands.calls().is_empty());

    let after_commands = ScriptedExecutor::new(ExecutorMode::Match);
    let mut after_journal =
        RecordingJournal::new(Some(LimaLifecycleExecutionCheckpoint::StartCompleted));
    let after = executor()
        .execute_with_journal(
            input(&action, &lifecycle, &current, &persistent, &request()),
            &ScriptedObservationSource::new(Vec::new()),
            &after_commands,
            &FixedClock(100),
            &mut after_journal,
        )
        .expect_err("post-command checkpoint failure");
    assert_eq!(
        after.code,
        LimaLifecycleExecutionRefusalCode::CheckpointFailed
    );
    assert_eq!(after.phase, LimaLifecycleExecutionPhase::Start);
    assert_eq!(after_commands.calls().len(), 1);
    assert_eq!(
        after_journal.checkpoints,
        vec![
            LimaLifecycleExecutionCheckpoint::StartStarted,
            LimaLifecycleExecutionCheckpoint::StartCompleted,
        ]
    );

    let failed_commands = ScriptedExecutor::new(ExecutorMode::Failed);
    let mut command_journal = RecordingJournal::new(None);
    let command_failure = executor()
        .execute_with_journal(
            input(&action, &lifecycle, &current, &persistent, &request()),
            &ScriptedObservationSource::new(Vec::new()),
            &failed_commands,
            &FixedClock(100),
            &mut command_journal,
        )
        .expect_err("command validation failure");
    assert_eq!(
        command_failure.code,
        LimaLifecycleExecutionRefusalCode::CommandFailed
    );
    assert_eq!(
        command_journal.checkpoints,
        vec![LimaLifecycleExecutionCheckpoint::StartStarted]
    );

    let verify_commands = ScriptedExecutor::new(ExecutorMode::Match);
    let verify_observations = ScriptedObservationSource::new(vec![report(
        LimaRuntimeState::Running,
        LimaResourceProfile::Interactive,
        Some(persistent.clone()),
    )]);
    let mut verify_journal =
        RecordingJournal::new(Some(LimaLifecycleExecutionCheckpoint::VerifyStarted));
    let verify_failure = executor()
        .execute_with_journal(
            input(&action, &lifecycle, &current, &persistent, &request()),
            &verify_observations,
            &verify_commands,
            &FixedClock(100),
            &mut verify_journal,
        )
        .expect_err("verification checkpoint failure");
    assert_eq!(
        verify_failure.code,
        LimaLifecycleExecutionRefusalCode::CheckpointFailed
    );
    assert_eq!(verify_failure.phase, LimaLifecycleExecutionPhase::Verify);
    assert_eq!(verify_commands.calls().len(), 1);
    assert_eq!(verify_observations.remaining(), 1);
}

#[test]
fn composite_intermediate_observation_failure_stops_before_the_next_command() {
    let persistent = persistent_identity('b');
    let lifecycle = lifecycle(
        LimaLifecycleState::Running,
        LimaResourceProfile::Work,
        3,
        false,
    );
    let current = report(
        LimaRuntimeState::Running,
        LimaResourceProfile::Work,
        Some(persistent.clone()),
    );
    let action = accepted(HostBrokerAction::Stop {
        identity: lifecycle.identity().clone(),
        current_profile: LimaResourceProfile::Work,
        profile_generation: generation(3),
        target_after_stop: PersonalWorkerProfile::Interactive,
    });
    let commands = ScriptedExecutor::new(ExecutorMode::Match);
    let mut journal = RecordingJournal::new(None);
    let failure = executor()
        .execute_with_journal(
            input(&action, &lifecycle, &current, &persistent, &request()),
            &ScriptedObservationSource::new(Vec::new()),
            &commands,
            &FixedClock(100),
            &mut journal,
        )
        .expect_err("missing intermediate stop observation");
    assert_eq!(
        failure.code,
        LimaLifecycleExecutionRefusalCode::VerificationFailed
    );
    assert_eq!(commands.calls().len(), 1);
    assert_eq!(
        journal.checkpoints,
        vec![
            LimaLifecycleExecutionCheckpoint::StopStarted,
            LimaLifecycleExecutionCheckpoint::StopCompleted,
        ]
    );
}

#[test]
fn invalid_stop_transition_refuses_before_the_first_checkpoint_or_command() {
    let persistent = persistent_identity('b');
    for (generation_value, target, expected) in [
        (
            3,
            PersonalWorkerProfile::Work,
            LimaLifecycleExecutionRefusalCode::ProfileMismatch,
        ),
        (
            u64::MAX,
            PersonalWorkerProfile::Interactive,
            LimaLifecycleExecutionRefusalCode::GenerationMismatch,
        ),
    ] {
        let lifecycle = lifecycle(
            LimaLifecycleState::Running,
            LimaResourceProfile::Work,
            generation_value,
            false,
        );
        let current = report(
            LimaRuntimeState::Running,
            LimaResourceProfile::Work,
            Some(persistent.clone()),
        );
        let action = accepted(HostBrokerAction::Stop {
            identity: lifecycle.identity().clone(),
            current_profile: LimaResourceProfile::Work,
            profile_generation: generation(generation_value),
            target_after_stop: target,
        });
        let commands = ScriptedExecutor::new(ExecutorMode::Match);
        let mut journal = RecordingJournal::new(None);
        let error = executor()
            .execute_with_journal(
                input(&action, &lifecycle, &current, &persistent, &request()),
                &ScriptedObservationSource::new(Vec::new()),
                &commands,
                &FixedClock(100),
                &mut journal,
            )
            .expect_err("invalid stop transition");
        assert_eq!(error.code, expected);
        assert!(journal.checkpoints.is_empty());
        assert!(commands.calls().is_empty());
    }
}

#[test]
fn active_reservation_refuses_before_any_command() {
    let persistent = persistent_identity('b');
    let lifecycle = lifecycle(
        LimaLifecycleState::Running,
        LimaResourceProfile::Work,
        1,
        true,
    );
    let current = report(
        LimaRuntimeState::Running,
        LimaResourceProfile::Work,
        Some(persistent.clone()),
    );
    let action = accepted(HostBrokerAction::Stop {
        identity: lifecycle.identity().clone(),
        current_profile: LimaResourceProfile::Work,
        profile_generation: generation(1),
        target_after_stop: PersonalWorkerProfile::Stopped,
    });
    let request = request();
    let observations = ScriptedObservationSource::new(Vec::new());
    let commands = ScriptedExecutor::new(ExecutorMode::Match);

    let error = executor()
        .execute(
            input(&action, &lifecycle, &current, &persistent, &request),
            &observations,
            &commands,
            &FixedClock(100),
        )
        .expect_err("active reservation refusal");

    assert_eq!(
        error.code,
        LimaLifecycleExecutionRefusalCode::ActiveReservation
    );
    assert!(commands.calls().is_empty());
}

#[test]
fn unsupported_action_and_stale_observation_refuse_before_mutation() {
    let persistent = persistent_identity('b');
    let lifecycle = lifecycle(
        LimaLifecycleState::Stopped,
        LimaResourceProfile::Interactive,
        1,
        false,
    );
    let mut current = report(
        LimaRuntimeState::Stopped,
        LimaResourceProfile::Interactive,
        None,
    );
    let request = request();
    let observations = ScriptedObservationSource::new(Vec::new());
    let commands = ScriptedExecutor::new(ExecutorMode::Match);

    let unsupported = accepted(HostBrokerAction::NoOp);
    let error = executor()
        .execute(
            input(&unsupported, &lifecycle, &current, &persistent, &request),
            &observations,
            &commands,
            &FixedClock(100),
        )
        .expect_err("unsupported refusal");
    assert_eq!(
        error.code,
        LimaLifecycleExecutionRefusalCode::UnsupportedAction
    );

    current.timing.expires_at_unix_seconds = 99;
    let start = accepted(HostBrokerAction::Start {
        identity: lifecycle.identity().clone(),
        profile: LimaResourceProfile::Interactive,
        profile_generation: generation(1),
    });
    let error = executor()
        .execute(
            input(&start, &lifecycle, &current, &persistent, &request),
            &observations,
            &commands,
            &FixedClock(100),
        )
        .expect_err("stale refusal");
    assert_eq!(
        error.code,
        LimaLifecycleExecutionRefusalCode::StaleObservation
    );
    assert!(commands.calls().is_empty());
}

#[test]
fn delayed_execution_refuses_expired_evidence_before_mutation() {
    let persistent = persistent_identity('b');
    let lifecycle = lifecycle(
        LimaLifecycleState::Stopped,
        LimaResourceProfile::Interactive,
        1,
        false,
    );
    let mut current = report(
        LimaRuntimeState::Stopped,
        LimaResourceProfile::Interactive,
        None,
    );
    current.timing.expires_at_unix_seconds = 101;
    let action = accepted(HostBrokerAction::Start {
        identity: lifecycle.identity().clone(),
        profile: LimaResourceProfile::Interactive,
        profile_generation: generation(1),
    });
    let request = request();
    let observations = ScriptedObservationSource::new(Vec::new());
    let commands = ScriptedExecutor::new(ExecutorMode::Match);

    let error = executor()
        .execute(
            input(&action, &lifecycle, &current, &persistent, &request),
            &observations,
            &commands,
            &FixedClock(102),
        )
        .expect_err("delayed stale refusal");

    assert_eq!(
        error.code,
        LimaLifecycleExecutionRefusalCode::StaleObservation
    );
    assert!(commands.calls().is_empty());
}

#[test]
fn old_action_refuses_with_fresh_observation_before_mutation() {
    let persistent = persistent_identity('b');
    let lifecycle = lifecycle(
        LimaLifecycleState::Stopped,
        LimaResourceProfile::Interactive,
        1,
        false,
    );
    let current = report(
        LimaRuntimeState::Stopped,
        LimaResourceProfile::Interactive,
        None,
    );
    let mut action = accepted(HostBrokerAction::Start {
        identity: lifecycle.identity().clone(),
        profile: LimaResourceProfile::Interactive,
        profile_generation: generation(1),
    });
    action.decision_at = epoch(60_000);
    let request = request();
    let observations = ScriptedObservationSource::new(Vec::new());
    let commands = ScriptedExecutor::new(ExecutorMode::Match);

    let error = executor()
        .execute(
            input(&action, &lifecycle, &current, &persistent, &request),
            &observations,
            &commands,
            &FixedClock(100),
        )
        .expect_err("expired action refusal");

    assert_eq!(error.code, LimaLifecycleExecutionRefusalCode::ExpiredAction);
    assert!(commands.calls().is_empty());
}

#[test]
fn command_identity_output_and_status_fail_closed() {
    let persistent = persistent_identity('b');
    let lifecycle = lifecycle(
        LimaLifecycleState::Stopped,
        LimaResourceProfile::Interactive,
        1,
        false,
    );
    let current = report(
        LimaRuntimeState::Stopped,
        LimaResourceProfile::Interactive,
        None,
    );
    let action = accepted(HostBrokerAction::Start {
        identity: lifecycle.identity().clone(),
        profile: LimaResourceProfile::Interactive,
        profile_generation: generation(1),
    });
    let request = request();

    for (mode, expected, evidence_count) in [
        (
            ExecutorMode::IdentityMismatch,
            LimaLifecycleExecutionRefusalCode::CommandIdentityMismatch,
            1,
        ),
        (
            ExecutorMode::Failed,
            LimaLifecycleExecutionRefusalCode::CommandFailed,
            1,
        ),
        (
            ExecutorMode::Oversized,
            LimaLifecycleExecutionRefusalCode::UnboundedOutput,
            1,
        ),
        (
            ExecutorMode::TimedOut,
            LimaLifecycleExecutionRefusalCode::CommandFailed,
            0,
        ),
    ] {
        let observations = ScriptedObservationSource::new(Vec::new());
        let commands = ScriptedExecutor::new(mode);
        let error = executor()
            .execute(
                input(&action, &lifecycle, &current, &persistent, &request),
                &observations,
                &commands,
                &FixedClock(100),
            )
            .expect_err("command refusal");
        assert_eq!(error.code, expected);
        assert_eq!(error.private_evidence().commands().len(), evidence_count);
        assert_eq!(commands.timeouts(), vec![LIMA_LIFECYCLE_COMMAND_TIMEOUT]);
        let public = serde_json::to_string(&error).expect("public error JSON");
        assert!(!public.contains(PRIVATE_HOME));
        assert!(!public.contains(LIMACTL));
    }
}

#[test]
fn resource_and_persistent_identity_drift_fail_after_exact_boundary() {
    let persistent = persistent_identity('b');
    let lifecycle = lifecycle(
        LimaLifecycleState::Stopped,
        LimaResourceProfile::Interactive,
        1,
        false,
    );
    let current = report(
        LimaRuntimeState::Stopped,
        LimaResourceProfile::Interactive,
        None,
    );
    let action = accepted(HostBrokerAction::Start {
        identity: lifecycle.identity().clone(),
        profile: LimaResourceProfile::Interactive,
        profile_generation: generation(1),
    });
    let request = request();

    let observations = ScriptedObservationSource::new(vec![report(
        LimaRuntimeState::Running,
        LimaResourceProfile::Interactive,
        Some(persistent_identity('c')),
    )]);
    let commands = ScriptedExecutor::new(ExecutorMode::Match);
    let error = executor()
        .execute(
            input(&action, &lifecycle, &current, &persistent, &request),
            &observations,
            &commands,
            &FixedClock(100),
        )
        .expect_err("persistent identity refusal");
    assert_eq!(
        error.code,
        LimaLifecycleExecutionRefusalCode::PersistentIdentityMismatch
    );
    assert_eq!(commands.calls().len(), 1);

    let mut wrong_resources = report(
        LimaRuntimeState::Running,
        LimaResourceProfile::Interactive,
        Some(persistent.clone()),
    );
    wrong_resources.configured.cpus = 8;
    let observations = ScriptedObservationSource::new(vec![wrong_resources]);
    let commands = ScriptedExecutor::new(ExecutorMode::Match);
    let error = executor()
        .execute(
            input(&action, &lifecycle, &current, &persistent, &request),
            &observations,
            &commands,
            &FixedClock(100),
        )
        .expect_err("resource refusal");
    assert_eq!(
        error.code,
        LimaLifecycleExecutionRefusalCode::ResourceMismatch
    );
}

#[test]
fn durable_revision_and_queue_generation_drift_refuse_before_mutation() {
    let persistent = persistent_identity('b');
    let lifecycle = lifecycle(
        LimaLifecycleState::Stopped,
        LimaResourceProfile::Interactive,
        1,
        false,
    );
    let current = report(
        LimaRuntimeState::Stopped,
        LimaResourceProfile::Interactive,
        None,
    );
    let action = accepted(HostBrokerAction::Start {
        identity: lifecycle.identity().clone(),
        profile: LimaResourceProfile::Interactive,
        profile_generation: generation(1),
    });
    let request = request();
    let observations = ScriptedObservationSource::new(Vec::new());

    let revision_commands = ScriptedExecutor::new(ExecutorMode::Match);
    let mut revision_input = input(&action, &lifecycle, &current, &persistent, &request);
    revision_input.current_broker_state_revision =
        HostBrokerStateRevision::new(8).expect("drifted revision");
    let revision_error = executor()
        .execute(
            revision_input,
            &observations,
            &revision_commands,
            &FixedClock(100),
        )
        .expect_err("broker revision drift");
    assert_eq!(
        revision_error.code,
        LimaLifecycleExecutionRefusalCode::BrokerStateRevisionMismatch
    );
    assert!(revision_commands.calls().is_empty());

    let generation_commands = ScriptedExecutor::new(ExecutorMode::Match);
    let mut generation_input = input(&action, &lifecycle, &current, &persistent, &request);
    generation_input.current_queue_generation =
        PersonalWorkerQueueGeneration::new(12).expect("drifted queue generation");
    let generation_error = executor()
        .execute(
            generation_input,
            &observations,
            &generation_commands,
            &FixedClock(100),
        )
        .expect_err("queue generation drift");
    assert_eq!(
        generation_error.code,
        LimaLifecycleExecutionRefusalCode::QueueGenerationMismatch
    );
    assert!(generation_commands.calls().is_empty());
}

#[test]
fn stale_lifecycle_observation_refuses_before_mutation() {
    let persistent = persistent_identity('b');
    let lifecycle = lifecycle(
        LimaLifecycleState::Stopped,
        LimaResourceProfile::Interactive,
        1,
        false,
    );
    let mut current = report(
        LimaRuntimeState::Stopped,
        LimaResourceProfile::Interactive,
        None,
    );
    current.timing.started_at_unix_seconds = 400;
    current.timing.observed_at_unix_seconds = 400;
    current.timing.expires_at_unix_seconds = 500;
    let mut action = accepted(HostBrokerAction::Start {
        identity: lifecycle.identity().clone(),
        profile: LimaResourceProfile::Interactive,
        profile_generation: generation(1),
    });
    action.decision_at = epoch(401_000);
    let request = request();
    let observations = ScriptedObservationSource::new(Vec::new());
    let commands = ScriptedExecutor::new(ExecutorMode::Match);

    let error = executor()
        .execute(
            input(&action, &lifecycle, &current, &persistent, &request),
            &observations,
            &commands,
            &FixedClock(401),
        )
        .expect_err("stale lifecycle observation");

    assert_eq!(
        error.code,
        LimaLifecycleExecutionRefusalCode::StaleObservation
    );
    assert!(commands.calls().is_empty());
}
