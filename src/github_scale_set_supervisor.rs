//! Bounded process loop around the one-transition Scale Set coordinator.
//!
//! `launchd` owns process supervision. This loop owns only in-process pacing: successful durable
//! transitions continue immediately, an idle long poll receives a small delay, step failures
//! back off exponentially, and repeated failures open a bounded cool-down circuit. Process death
//! never loses lifecycle authority because every external operation remains inside the existing
//! same-lock transactions.

#![allow(dead_code)]

use std::fmt;
use std::io;
use std::time::Duration;

use crate::disposable_clone_runtime::{CloneRuntimeClock, DisposableCloneRuntime};
use crate::disposable_runner_runtime::DisposableRunnerRuntime;
use crate::disposable_template_runtime::DisposableTemplateRuntime;
use crate::github_scale_set_service::{
    ScaleSetBridgeSession, ScaleSetRunnerBridgeSession, ScaleSetService, ScaleSetServiceClock,
    ScaleSetServiceDisposition, ScaleSetServiceError,
};
use crate::process::TimedCommandExecutor;

const MAX_FAILURES: u8 = 32;
const MAX_DELAY: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScaleSetSupervisorSignal {
    Continue,
    Stop,
}

pub(crate) trait ScaleSetSupervisorWait {
    fn wait(&mut self, delay: Duration) -> io::Result<ScaleSetSupervisorSignal>;
}

pub(crate) struct ThreadScaleSetSupervisorWait;

impl ScaleSetSupervisorWait for ThreadScaleSetSupervisorWait {
    fn wait(&mut self, delay: Duration) -> io::Result<ScaleSetSupervisorSignal> {
        std::thread::sleep(delay);
        Ok(ScaleSetSupervisorSignal::Continue)
    }
}

pub(crate) trait ScaleSetSupervisorEventSink {
    fn record(&mut self, event: &ScaleSetSupervisorEvent);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScaleSetSupervisorEvent {
    StepCompleted {
        disposition: ScaleSetServiceDisposition,
        next_delay: Duration,
    },
    RetryScheduled {
        code: &'static str,
        consecutive_failures: u8,
        next_delay: Duration,
    },
    CircuitOpen {
        code: &'static str,
        consecutive_failures: u8,
        next_delay: Duration,
    },
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScaleSetSupervisorPolicy {
    idle_delay: Duration,
    progress_yield_delay: Duration,
    max_immediate_steps: u8,
    retry_initial_delay: Duration,
    retry_max_delay: Duration,
    circuit_failure_threshold: u8,
    circuit_delay: Duration,
}

impl ScaleSetSupervisorPolicy {
    pub(crate) fn production() -> Self {
        Self {
            idle_delay: Duration::from_secs(1),
            progress_yield_delay: Duration::from_millis(10),
            max_immediate_steps: 32,
            retry_initial_delay: Duration::from_secs(1),
            retry_max_delay: Duration::from_secs(30),
            circuit_failure_threshold: 5,
            circuit_delay: Duration::from_secs(5 * 60),
        }
    }

    #[cfg(test)]
    fn new(
        idle_delay: Duration,
        progress_yield_delay: Duration,
        max_immediate_steps: u8,
        retry_initial_delay: Duration,
        retry_max_delay: Duration,
        circuit_failure_threshold: u8,
        circuit_delay: Duration,
    ) -> Result<Self, ScaleSetSupervisorError> {
        let policy = Self {
            idle_delay,
            progress_yield_delay,
            max_immediate_steps,
            retry_initial_delay,
            retry_max_delay,
            circuit_failure_threshold,
            circuit_delay,
        };
        policy.validate()?;
        Ok(policy)
    }

    fn validate(self) -> Result<(), ScaleSetSupervisorError> {
        if self.idle_delay.is_zero()
            || self.progress_yield_delay.is_zero()
            || self.max_immediate_steps == 0
            || self.max_immediate_steps > MAX_FAILURES
            || self.retry_initial_delay.is_zero()
            || self.retry_max_delay < self.retry_initial_delay
            || self.retry_max_delay > MAX_DELAY
            || self.circuit_failure_threshold == 0
            || self.circuit_failure_threshold > MAX_FAILURES
            || self.circuit_delay < self.retry_max_delay
            || self.circuit_delay > MAX_DELAY
        {
            return Err(ScaleSetSupervisorError::invalid_policy());
        }
        Ok(())
    }

    fn failure_delay(self, consecutive_failures: u8) -> Duration {
        let shifts = u32::from(consecutive_failures.saturating_sub(1)).min(31);
        self.retry_initial_delay
            .checked_mul(1_u32 << shifts)
            .unwrap_or(self.retry_max_delay)
            .min(self.retry_max_delay)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScaleSetSupervisorExit {
    Stopped,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScaleSetSupervisorError {
    code: &'static str,
}

impl ScaleSetSupervisorError {
    const fn invalid_policy() -> Self {
        Self {
            code: "scale_set_supervisor_policy_invalid",
        }
    }

    const fn wait_failed() -> Self {
        Self {
            code: "scale_set_supervisor_wait_failed",
        }
    }

    pub(crate) const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for ScaleSetSupervisorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScaleSetSupervisorError")
            .field("code", &self.code)
            .finish()
    }
}

impl fmt::Display for ScaleSetSupervisorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code)
    }
}

impl std::error::Error for ScaleSetSupervisorError {}

pub(crate) struct ScaleSetSupervisor<'a, W, S> {
    policy: ScaleSetSupervisorPolicy,
    wait: &'a mut W,
    events: &'a mut S,
}

impl<'a, W: ScaleSetSupervisorWait, S: ScaleSetSupervisorEventSink> ScaleSetSupervisor<'a, W, S> {
    pub(crate) fn new(
        policy: ScaleSetSupervisorPolicy,
        wait: &'a mut W,
        events: &'a mut S,
    ) -> Result<Self, ScaleSetSupervisorError> {
        policy.validate()?;
        Ok(Self {
            policy,
            wait,
            events,
        })
    }

    pub(crate) fn serve<B, C, E, L>(
        &mut self,
        service: &mut ScaleSetService<B, C>,
        template_runtime: &DisposableTemplateRuntime,
        clone_runtime: &DisposableCloneRuntime,
        runner_runtime: &DisposableRunnerRuntime,
        executor: &E,
        clock: &L,
    ) -> Result<ScaleSetSupervisorExit, ScaleSetSupervisorError>
    where
        B: ScaleSetBridgeSession + ScaleSetRunnerBridgeSession,
        C: ScaleSetServiceClock,
        E: TimedCommandExecutor,
        L: CloneRuntimeClock,
    {
        run_supervisor_loop(
            || {
                service.supervise_once(
                    template_runtime,
                    clone_runtime,
                    runner_runtime,
                    executor,
                    clock,
                )
            },
            self.policy,
            self.wait,
            self.events,
        )
    }
}

fn run_supervisor_loop<F, W, S>(
    mut step: F,
    policy: ScaleSetSupervisorPolicy,
    wait: &mut W,
    events: &mut S,
) -> Result<ScaleSetSupervisorExit, ScaleSetSupervisorError>
where
    F: FnMut() -> Result<ScaleSetServiceDisposition, ScaleSetServiceError>,
    W: ScaleSetSupervisorWait,
    S: ScaleSetSupervisorEventSink,
{
    policy.validate()?;
    let mut consecutive_failures = 0_u8;
    let mut immediate_steps = 0_u8;
    loop {
        let delay = match step() {
            Ok(disposition) => {
                consecutive_failures = 0;
                let next_delay = if matches!(
                    disposition,
                    ScaleSetServiceDisposition::Idle(_)
                        | ScaleSetServiceDisposition::IdleObservationRecorded { .. }
                        | ScaleSetServiceDisposition::AdmissionHeld { .. }
                ) {
                    immediate_steps = 0;
                    policy.idle_delay
                } else {
                    immediate_steps = immediate_steps.saturating_add(1);
                    if immediate_steps >= policy.max_immediate_steps {
                        immediate_steps = 0;
                        policy.progress_yield_delay
                    } else {
                        Duration::ZERO
                    }
                };
                events.record(&ScaleSetSupervisorEvent::StepCompleted {
                    disposition,
                    next_delay,
                });
                next_delay
            }
            Err(error) => {
                immediate_steps = 0;
                consecutive_failures = consecutive_failures.saturating_add(1).min(MAX_FAILURES);
                if consecutive_failures >= policy.circuit_failure_threshold {
                    events.record(&ScaleSetSupervisorEvent::CircuitOpen {
                        code: error.code(),
                        consecutive_failures,
                        next_delay: policy.circuit_delay,
                    });
                    policy.circuit_delay
                } else {
                    let next_delay = policy.failure_delay(consecutive_failures);
                    events.record(&ScaleSetSupervisorEvent::RetryScheduled {
                        code: error.code(),
                        consecutive_failures,
                        next_delay,
                    });
                    next_delay
                }
            }
        };

        if delay.is_zero() {
            continue;
        }
        match wait
            .wait(delay)
            .map_err(|_| ScaleSetSupervisorError::wait_failed())?
        {
            ScaleSetSupervisorSignal::Continue => {}
            ScaleSetSupervisorSignal::Stop => {
                events.record(&ScaleSetSupervisorEvent::Stopped);
                return Ok(ScaleSetSupervisorExit::Stopped);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::github_scale_set_bridge::ScaleSetStatistics;

    struct FakeWait {
        signals: VecDeque<io::Result<ScaleSetSupervisorSignal>>,
        delays: Vec<Duration>,
    }

    impl ScaleSetSupervisorWait for FakeWait {
        fn wait(&mut self, delay: Duration) -> io::Result<ScaleSetSupervisorSignal> {
            self.delays.push(delay);
            self.signals
                .pop_front()
                .unwrap_or(Ok(ScaleSetSupervisorSignal::Stop))
        }
    }

    #[derive(Default)]
    struct Events(Vec<ScaleSetSupervisorEvent>);

    impl ScaleSetSupervisorEventSink for Events {
        fn record(&mut self, event: &ScaleSetSupervisorEvent) {
            self.0.push(event.clone());
        }
    }

    fn policy() -> ScaleSetSupervisorPolicy {
        ScaleSetSupervisorPolicy::new(
            Duration::from_millis(10),
            Duration::from_millis(2),
            4,
            Duration::from_millis(5),
            Duration::from_millis(20),
            3,
            Duration::from_millis(50),
        )
        .unwrap()
    }

    fn idle() -> ScaleSetServiceDisposition {
        ScaleSetServiceDisposition::Idle(ScaleSetStatistics {
            available_jobs: 0,
            acquired_jobs: 0,
            assigned_jobs: 0,
            running_jobs: 0,
            registered_runners: 0,
            busy_runners: 0,
            idle_runners: 0,
        })
    }

    #[test]
    fn progress_is_immediate_then_idle_wait_can_stop_cleanly() {
        let mut results = VecDeque::from([
            Ok(ScaleSetServiceDisposition::MessagePersisted { message_id: 7 }),
            Ok(idle()),
        ]);
        let mut wait = FakeWait {
            signals: VecDeque::from([Ok(ScaleSetSupervisorSignal::Stop)]),
            delays: Vec::new(),
        };
        let mut events = Events::default();

        let exit = run_supervisor_loop(
            || results.pop_front().expect("bounded step fixture"),
            policy(),
            &mut wait,
            &mut events,
        )
        .unwrap();

        assert_eq!(exit, ScaleSetSupervisorExit::Stopped);
        assert_eq!(wait.delays, [Duration::from_millis(10)]);
        assert!(matches!(
            events.0.as_slice(),
            [
                ScaleSetSupervisorEvent::StepCompleted {
                    next_delay,
                    ..
                },
                ScaleSetSupervisorEvent::StepCompleted {
                    next_delay: idle_delay,
                    ..
                },
                ScaleSetSupervisorEvent::Stopped,
            ] if next_delay.is_zero() && *idle_delay == Duration::from_millis(10)
        ));
    }

    #[test]
    fn recorded_idle_observation_uses_the_idle_poll_delay() {
        let mut results =
            VecDeque::from([Ok(ScaleSetServiceDisposition::IdleObservationRecorded {
                attempt_id: "attempt-7".to_owned(),
            })]);
        let mut wait = FakeWait {
            signals: VecDeque::from([Ok(ScaleSetSupervisorSignal::Stop)]),
            delays: Vec::new(),
        };
        let mut events = Events::default();

        run_supervisor_loop(
            || results.pop_front().expect("bounded step fixture"),
            policy(),
            &mut wait,
            &mut events,
        )
        .unwrap();

        assert_eq!(wait.delays, [Duration::from_millis(10)]);
        assert!(matches!(
            events.0.as_slice(),
            [
                ScaleSetSupervisorEvent::StepCompleted {
                    disposition: ScaleSetServiceDisposition::IdleObservationRecorded { .. },
                    next_delay,
                },
                ScaleSetSupervisorEvent::Stopped,
            ] if *next_delay == Duration::from_millis(10)
        ));
    }

    #[test]
    fn admission_hold_uses_idle_pacing_without_opening_the_failure_circuit() {
        let mut results = VecDeque::from_iter((0..6).map(|_| {
            Ok(ScaleSetServiceDisposition::AdmissionHeld {
                attempt_id: "attempt-held".to_owned(),
            })
        }));
        let mut wait = FakeWait {
            signals: VecDeque::from([
                Ok(ScaleSetSupervisorSignal::Continue),
                Ok(ScaleSetSupervisorSignal::Continue),
                Ok(ScaleSetSupervisorSignal::Continue),
                Ok(ScaleSetSupervisorSignal::Continue),
                Ok(ScaleSetSupervisorSignal::Continue),
                Ok(ScaleSetSupervisorSignal::Stop),
            ]),
            delays: Vec::new(),
        };
        let mut events = Events::default();

        run_supervisor_loop(
            || results.pop_front().expect("bounded held-step fixture"),
            policy(),
            &mut wait,
            &mut events,
        )
        .unwrap();

        assert_eq!(wait.delays, [Duration::from_millis(10); 6]);
        assert!(!events.0.iter().any(|event| matches!(
            event,
            ScaleSetSupervisorEvent::RetryScheduled { .. }
                | ScaleSetSupervisorEvent::CircuitOpen { .. }
        )));
    }

    #[test]
    fn failures_back_off_open_the_circuit_and_success_resets_it() {
        let mut results = VecDeque::from([
            Err(ScaleSetServiceError::for_test("transient_one")),
            Err(ScaleSetServiceError::for_test("transient_two")),
            Err(ScaleSetServiceError::for_test("transient_three")),
            Ok(ScaleSetServiceDisposition::MessageAcknowledged { message_id: 8 }),
            Err(ScaleSetServiceError::for_test("transient_after_success")),
            Ok(idle()),
        ]);
        let mut wait = FakeWait {
            signals: VecDeque::from([
                Ok(ScaleSetSupervisorSignal::Continue),
                Ok(ScaleSetSupervisorSignal::Continue),
                Ok(ScaleSetSupervisorSignal::Continue),
                Ok(ScaleSetSupervisorSignal::Continue),
                Ok(ScaleSetSupervisorSignal::Stop),
            ]),
            delays: Vec::new(),
        };
        let mut events = Events::default();

        run_supervisor_loop(
            || results.pop_front().expect("bounded step fixture"),
            policy(),
            &mut wait,
            &mut events,
        )
        .unwrap();

        assert_eq!(
            wait.delays,
            [
                Duration::from_millis(5),
                Duration::from_millis(10),
                Duration::from_millis(50),
                Duration::from_millis(5),
                Duration::from_millis(10),
            ]
        );
        assert!(matches!(
            &events.0[2],
            ScaleSetSupervisorEvent::CircuitOpen {
                code: "transient_three",
                consecutive_failures: 3,
                next_delay,
            } if *next_delay == Duration::from_millis(50)
        ));
        assert!(matches!(
            &events.0[4],
            ScaleSetSupervisorEvent::RetryScheduled {
                code: "transient_after_success",
                consecutive_failures: 1,
                ..
            }
        ));
    }

    #[test]
    fn bounded_progress_bursts_yield_to_the_process_supervisor() {
        let policy = ScaleSetSupervisorPolicy::new(
            Duration::from_millis(10),
            Duration::from_millis(2),
            2,
            Duration::from_millis(5),
            Duration::from_millis(20),
            3,
            Duration::from_millis(50),
        )
        .unwrap();
        let mut next_message = 1_u32;
        let mut wait = FakeWait {
            signals: VecDeque::from([Ok(ScaleSetSupervisorSignal::Stop)]),
            delays: Vec::new(),
        };
        let mut events = Events::default();

        run_supervisor_loop(
            || {
                let message_id = next_message;
                next_message += 1;
                Ok(ScaleSetServiceDisposition::MessagePersisted { message_id })
            },
            policy,
            &mut wait,
            &mut events,
        )
        .unwrap();

        assert_eq!(next_message, 3);
        assert_eq!(wait.delays, [Duration::from_millis(2)]);
        assert!(matches!(
            events.0.last(),
            Some(ScaleSetSupervisorEvent::Stopped)
        ));
    }

    #[test]
    fn invalid_policy_and_wait_failure_are_bounded() {
        let invalid = ScaleSetSupervisorPolicy::new(
            Duration::ZERO,
            Duration::from_millis(1),
            1,
            Duration::from_secs(1),
            Duration::from_secs(1),
            1,
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert_eq!(invalid.code(), "scale_set_supervisor_policy_invalid");

        let mut wait = FakeWait {
            signals: VecDeque::from([Err(io::Error::other("private wait failure"))]),
            delays: Vec::new(),
        };
        let mut events = Events::default();
        let error =
            run_supervisor_loop(|| Ok(idle()), policy(), &mut wait, &mut events).unwrap_err();
        assert_eq!(error.code(), "scale_set_supervisor_wait_failed");
        assert!(!format!("{error:?}").contains("private wait failure"));
    }
}
