use std::fmt;

use serde::Serialize;

use crate::mac_auto_availability::OperatorActivityState;
use crate::mac_availability::ObservationFreshness;
use crate::process::{CommandExecutor, CommandSpec, ExecutionRecord};

pub const MACOS_OPERATOR_ACTIVITY_SCHEMA_VERSION: u8 = 1;
pub const DEFAULT_OPERATOR_ACTIVITY_FRESHNESS_MILLIS: u64 = 30_000;
pub const DEFAULT_OPERATOR_ACTIVE_WINDOW_MILLIS: u64 = 30_000;
pub const MAX_REPORTED_OPERATOR_IDLE_MILLIS: u64 = 7 * 24 * 60 * 60 * 1_000;
const MAX_IOREG_OUTPUT_BYTES: usize = 65_536;
const IOREG_PROGRAM: &str = "/usr/sbin/ioreg";
const IDLE_MARKER: &str = "\"HIDIdleTime\" =";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MacOperatorActivityProblemKind {
    IdleObservationUnavailable,
    IdleValueCapped,
    StaleObservation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MacOperatorActivityReport {
    pub schema_version: u8,
    pub observed_at_millis: u64,
    pub freshness: ObservationFreshness,
    pub activity: OperatorActivityState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_millis: Option<u64>,
    pub problems: Vec<MacOperatorActivityProblemKind>,
}

pub struct MacOperatorActivityObservation {
    report: MacOperatorActivityReport,
    private_evidence: Option<ExecutionRecord>,
}

impl MacOperatorActivityObservation {
    #[must_use]
    pub const fn report(&self) -> &MacOperatorActivityReport {
        &self.report
    }

    #[must_use]
    pub fn into_report(self) -> MacOperatorActivityReport {
        self.report
    }
}

impl fmt::Debug for MacOperatorActivityObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MacOperatorActivityObservation")
            .field("report", &self.report)
            .field("private_evidence", &"[REDACTED]")
            .field("captured", &self.private_evidence.is_some())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MacOperatorActivityError {
    pub field: &'static str,
    pub code: &'static str,
    pub message: &'static str,
}

impl MacOperatorActivityError {
    const fn new(field: &'static str, code: &'static str, message: &'static str) -> Self {
        Self {
            field,
            code,
            message,
        }
    }
}

impl fmt::Display for MacOperatorActivityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for MacOperatorActivityError {}

#[must_use]
pub fn operator_idle_command() -> CommandSpec {
    CommandSpec::new(IOREG_PROGRAM)
        .argument("-c")
        .argument("IOHIDSystem")
        .argument("-d")
        .argument("1")
}

/// Observe only the coarse macOS input-idle duration used by automatic local admission.
///
/// The adapter reads no foreground application name, browser state, window title, history,
/// filesystem path, credential, clipboard, or keyboard content. Raw `ioreg` evidence stays private.
///
/// # Errors
///
/// Returns a bounded error only for invalid caller-supplied time windows. Command and parse
/// failures become explicit unknown activity evidence.
pub fn observe_macos_operator_activity(
    executor: &impl CommandExecutor,
    observed_at_millis: u64,
    now_millis: u64,
    freshness_window_millis: u64,
    active_window_millis: u64,
) -> Result<MacOperatorActivityObservation, MacOperatorActivityError> {
    if observed_at_millis == 0 {
        return Err(MacOperatorActivityError::new(
            "observed_at_millis",
            "invalid_observation_time",
            "operator activity observation time must be greater than zero",
        ));
    }
    if freshness_window_millis == 0 {
        return Err(MacOperatorActivityError::new(
            "freshness_window_millis",
            "invalid_freshness_window",
            "operator activity freshness window must be greater than zero",
        ));
    }
    if active_window_millis == 0 {
        return Err(MacOperatorActivityError::new(
            "active_window_millis",
            "invalid_active_window",
            "operator active window must be greater than zero",
        ));
    }
    let age = now_millis.checked_sub(observed_at_millis).ok_or_else(|| {
        MacOperatorActivityError::new(
            "now_millis",
            "observation_time_reversal",
            "comparison time cannot precede the operator activity observation",
        )
    })?;

    let command = operator_idle_command();
    let receipt = executor.execute(&command).ok();
    let mut problems = Vec::new();
    let freshness = if age <= freshness_window_millis {
        ObservationFreshness::Fresh
    } else {
        problems.push(MacOperatorActivityProblemKind::StaleObservation);
        ObservationFreshness::Stale
    };

    let parsed_idle = receipt
        .as_ref()
        .and_then(|record| parse_idle_receipt(&command, record));
    let (activity, idle_millis) = match parsed_idle {
        Some(raw_idle_millis) => {
            let capped = raw_idle_millis.min(MAX_REPORTED_OPERATOR_IDLE_MILLIS);
            if raw_idle_millis > MAX_REPORTED_OPERATOR_IDLE_MILLIS {
                problems.push(MacOperatorActivityProblemKind::IdleValueCapped);
            }
            let activity = if capped <= active_window_millis {
                OperatorActivityState::Active
            } else {
                OperatorActivityState::Idle
            };
            (activity, Some(capped))
        }
        None => {
            problems.push(MacOperatorActivityProblemKind::IdleObservationUnavailable);
            (OperatorActivityState::Unknown, None)
        }
    };

    Ok(MacOperatorActivityObservation {
        report: MacOperatorActivityReport {
            schema_version: MACOS_OPERATOR_ACTIVITY_SCHEMA_VERSION,
            observed_at_millis,
            freshness,
            activity,
            idle_millis,
            problems,
        },
        private_evidence: receipt,
    })
}

fn parse_idle_receipt(command: &CommandSpec, receipt: &ExecutionRecord) -> Option<u64> {
    if receipt.argv != command.displayed_argv()
        || !receipt.environment_keys.is_empty()
        || !receipt.success
        || receipt.status != Some(0)
        || !receipt.stderr.is_empty()
        || receipt.stdout.len() > MAX_IOREG_OUTPUT_BYTES
        || receipt.stdout.contains('\0')
    {
        return None;
    }
    parse_idle_output(&receipt.stdout)
}

fn parse_idle_output(input: &str) -> Option<u64> {
    let mut parsed = None;
    for line in input.lines() {
        let Some(index) = line.find(IDLE_MARKER) else {
            continue;
        };
        let value = line[index + IDLE_MARKER.len()..].trim();
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let idle_nanos = value.parse::<u128>().ok()?;
        let idle_millis = u64::try_from(idle_nanos / 1_000_000).ok()?;
        match parsed {
            Some(existing) if existing != idle_millis => return None,
            Some(_) => {}
            None => parsed = Some(idle_millis),
        }
    }
    parsed
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::io;

    use super::*;

    struct FakeExecutor {
        receipt: RefCell<Option<ExecutionRecord>>,
    }

    impl FakeExecutor {
        fn success(stdout: impl Into<String>) -> Self {
            let command = operator_idle_command();
            Self {
                receipt: RefCell::new(Some(ExecutionRecord {
                    argv: command.displayed_argv(),
                    environment_keys: Vec::new(),
                    status: Some(0),
                    success: true,
                    stdout: stdout.into(),
                    stderr: String::new(),
                })),
            }
        }

        fn missing() -> Self {
            Self {
                receipt: RefCell::new(None),
            }
        }
    }

    impl CommandExecutor for FakeExecutor {
        fn execute(&self, _spec: &CommandSpec) -> io::Result<ExecutionRecord> {
            self.receipt
                .borrow_mut()
                .take()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "fixture missing"))
        }
    }

    fn output_with_idle_nanos(nanos: u128) -> String {
        format!(
            "+-o IOHIDSystem  <class IOHIDSystem>\n  |   \"HIDIdleTime\" = {nanos}\n"
        )
    }

    #[test]
    fn command_is_absolute_fixed_and_environment_free() {
        let command = operator_idle_command();
        assert_eq!(
            command.displayed_argv(),
            vec!["/usr/sbin/ioreg", "-c", "IOHIDSystem", "-d", "1"]
        );
        assert!(command.environment.is_empty());
    }

    #[test]
    fn recent_input_is_active_without_application_identity() {
        let observation = observe_macos_operator_activity(
            &FakeExecutor::success(output_with_idle_nanos(5_000_000_000)),
            1_000,
            1_001,
            30_000,
            30_000,
        )
        .expect("observation");
        let report = observation.report();

        assert_eq!(report.activity, OperatorActivityState::Active);
        assert_eq!(report.idle_millis, Some(5_000));
        assert!(report.problems.is_empty());
        let json = serde_json::to_string(report).expect("serialize");
        assert!(!json.contains("Safari"));
        assert!(!json.contains("Chrome"));
        assert!(!json.contains("/Users/"));
    }

    #[test]
    fn sustained_input_idle_is_idle() {
        let report = observe_macos_operator_activity(
            &FakeExecutor::success(output_with_idle_nanos(120_000_000_000)),
            1_000,
            1_001,
            30_000,
            30_000,
        )
        .expect("observation")
        .into_report();

        assert_eq!(report.activity, OperatorActivityState::Idle);
        assert_eq!(report.idle_millis, Some(120_000));
    }

    #[test]
    fn conflicting_idle_values_fail_closed() {
        let output = concat!(
            "  |   \"HIDIdleTime\" = 1000000\n",
            "  |   \"HIDIdleTime\" = 2000000\n",
        );
        let report = observe_macos_operator_activity(
            &FakeExecutor::success(output),
            1_000,
            1_001,
            30_000,
            30_000,
        )
        .expect("observation")
        .into_report();

        assert_eq!(report.activity, OperatorActivityState::Unknown);
        assert_eq!(report.idle_millis, None);
        assert!(
            report
                .problems
                .contains(&MacOperatorActivityProblemKind::IdleObservationUnavailable)
        );
    }

    #[test]
    fn command_failure_becomes_unknown_evidence() {
        let report = observe_macos_operator_activity(
            &FakeExecutor::missing(),
            1_000,
            1_001,
            30_000,
            30_000,
        )
        .expect("observation")
        .into_report();

        assert_eq!(report.activity, OperatorActivityState::Unknown);
        assert_eq!(report.idle_millis, None);
    }

    #[test]
    fn stale_observation_is_explicit() {
        let report = observe_macos_operator_activity(
            &FakeExecutor::success(output_with_idle_nanos(5_000_000_000)),
            1_000,
            40_001,
            30_000,
            30_000,
        )
        .expect("observation")
        .into_report();

        assert_eq!(report.freshness, ObservationFreshness::Stale);
        assert!(
            report
                .problems
                .contains(&MacOperatorActivityProblemKind::StaleObservation)
        );
    }

    #[test]
    fn very_long_idle_is_capped_for_public_output() {
        let nanos = u128::from(MAX_REPORTED_OPERATOR_IDLE_MILLIS + 1) * 1_000_000;
        let report = observe_macos_operator_activity(
            &FakeExecutor::success(output_with_idle_nanos(nanos)),
            1_000,
            1_001,
            30_000,
            30_000,
        )
        .expect("observation")
        .into_report();

        assert_eq!(report.idle_millis, Some(MAX_REPORTED_OPERATOR_IDLE_MILLIS));
        assert!(
            report
                .problems
                .contains(&MacOperatorActivityProblemKind::IdleValueCapped)
        );
    }

    #[test]
    fn debug_redacts_raw_evidence() {
        let observation = observe_macos_operator_activity(
            &FakeExecutor::success(
                "  |   \"HIDIdleTime\" = 1000000\nPRIVATE_RAW_IOREG_SENTINEL\n",
            ),
            1_000,
            1_001,
            30_000,
            30_000,
        )
        .expect("observation");
        let debug = format!("{observation:?}");

        assert!(!debug.contains("PRIVATE_RAW_IOREG_SENTINEL"));
        assert!(debug.contains("[REDACTED]"));
    }
}
