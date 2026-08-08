use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::artifact::{RepositoryRef, Sha256Digest};
use crate::journal::{ExecutionLane, RollbackClass};
use crate::state::JournalId;

pub const EXECUTION_RECEIPT_SCHEMA_VERSION: u8 = 1;
pub const HOST_PREPARATION_OPERATION_SCHEMA_VERSION: u8 = 1;
pub const MAX_EXECUTION_RECEIPT_BYTES: usize = 65_536;
pub const MAX_EXECUTION_RECEIPT_ACTIONS: usize = 256;
pub const MAX_EXECUTION_RECEIPT_CONTINUATIONS: usize = 64;

const MAX_TOKEN_LEN: usize = 128;
const MAX_PRODUCER_VERSION_LEN: usize = 64;

#[cfg(target_os = "linux")]
pub(crate) fn validate_receipt_token(
    field: &str,
    value: &str,
) -> Result<(), ExecutionReceiptError> {
    ReceiptToken::parse(field, value).map(|_| ())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecutionReceipt {
    document_type: ExecutionReceiptDocumentType,
    schema_version: u8,
    producer: ExecutionReceiptProducer,
    execution_id: JournalId,
    started_at: ReceiptTimestamp,
    terminal_at: ReceiptTimestamp,
    disposition: ExecutionReceiptDisposition,
    operation: ExecutionReceiptOperation,
    actions: Vec<ExecutionReceiptAction>,
    summary: ExecutionReceiptSummary,
    continuation: ExecutionReceiptContinuation,
    coverage: ExecutionReceiptCoverage,
}

impl ExecutionReceipt {
    /// Build one external host-preparation execution receipt from bounded public evidence.
    ///
    /// The receipt identifies one exact durable execution through its journal ID and binds it to one
    /// repository, reviewed source digest, and phase identity. Private host observations, commands,
    /// process output, paths, credentials, precondition evidence, and journal prose remain outside
    /// this document.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid timestamps, empty or oversized action sets, duplicate action
    /// identities, inconsistent terminal outcomes, invalid continuation state, or unsupported
    /// operation versions.
    #[allow(clippy::too_many_arguments)]
    pub fn new_host_preparation(
        execution_id: JournalId,
        repository: RepositoryRef,
        source_digest: Sha256Digest,
        phase_id: &str,
        started_at: ReceiptTimestamp,
        terminal_at: ReceiptTimestamp,
        disposition: ExecutionReceiptDisposition,
        actions: Vec<ExecutionReceiptAction>,
        continuation: ExecutionReceiptContinuation,
    ) -> Result<Self, ExecutionReceiptError> {
        Self::new_host_preparation_with_producer(
            env!("CARGO_PKG_VERSION"),
            execution_id,
            repository,
            source_digest,
            phase_id,
            started_at,
            terminal_at,
            disposition,
            actions,
            continuation,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_host_preparation_with_producer(
        producer_version: &str,
        execution_id: JournalId,
        repository: RepositoryRef,
        source_digest: Sha256Digest,
        phase_id: &str,
        started_at: ReceiptTimestamp,
        terminal_at: ReceiptTimestamp,
        disposition: ExecutionReceiptDisposition,
        actions: Vec<ExecutionReceiptAction>,
        continuation: ExecutionReceiptContinuation,
    ) -> Result<Self, ExecutionReceiptError> {
        let producer = ExecutionReceiptProducer::new(producer_version)?;
        let phase_id = ReceiptToken::parse("phase ID", phase_id)?;
        if terminal_at < started_at {
            return Err(ExecutionReceiptError::single(
                "terminal timestamp must not precede the start timestamp",
            ));
        }
        validate_actions(&actions)?;
        validate_disposition(disposition, &actions, &continuation)?;
        let summary = ExecutionReceiptSummary::from_actions(&actions);
        Ok(Self {
            document_type: ExecutionReceiptDocumentType::ExecutionReceipt,
            schema_version: EXECUTION_RECEIPT_SCHEMA_VERSION,
            producer,
            execution_id,
            started_at,
            terminal_at,
            disposition,
            operation: ExecutionReceiptOperation::HostPreparation {
                schema_version: HOST_PREPARATION_OPERATION_SCHEMA_VERSION,
                repository,
                source_digest,
                phase_id,
            },
            actions,
            summary,
            continuation,
            coverage: ExecutionReceiptCoverage::external_v1(),
        })
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub fn producer_version(&self) -> &str {
        &self.producer.version
    }

    #[must_use]
    pub fn execution_id(&self) -> &JournalId {
        &self.execution_id
    }

    #[must_use]
    pub const fn started_at(&self) -> &ReceiptTimestamp {
        &self.started_at
    }

    #[must_use]
    pub const fn terminal_at(&self) -> &ReceiptTimestamp {
        &self.terminal_at
    }

    #[must_use]
    pub const fn disposition(&self) -> ExecutionReceiptDisposition {
        self.disposition
    }

    #[must_use]
    pub const fn operation(&self) -> &ExecutionReceiptOperation {
        &self.operation
    }

    #[must_use]
    pub fn actions(&self) -> &[ExecutionReceiptAction] {
        &self.actions
    }

    #[must_use]
    pub const fn summary(&self) -> &ExecutionReceiptSummary {
        &self.summary
    }

    #[must_use]
    pub const fn continuation(&self) -> &ExecutionReceiptContinuation {
        &self.continuation
    }

    #[must_use]
    pub const fn coverage(&self) -> &ExecutionReceiptCoverage {
        &self.coverage
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ExecutionReceiptDocumentType {
    ExecutionReceipt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecutionReceiptProducer {
    name: &'static str,
    version: String,
}

impl ExecutionReceiptProducer {
    fn new(version: &str) -> Result<Self, ExecutionReceiptError> {
        if version.is_empty()
            || version.len() > MAX_PRODUCER_VERSION_LEN
            || !version.as_bytes()[0].is_ascii_alphanumeric()
            || !version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
        {
            return Err(ExecutionReceiptError::single(
                "producer version must be a bounded ASCII version token",
            ));
        }
        Ok(Self {
            name: "smolrunner",
            version: version.to_owned(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionReceiptDisposition {
    Completed,
    ActionFailed,
    FreshObservationRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "family", rename_all = "snake_case")]
pub enum ExecutionReceiptOperation {
    HostPreparation {
        schema_version: u8,
        repository: RepositoryRef,
        source_digest: Sha256Digest,
        phase_id: ReceiptToken,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecutionReceiptAction {
    id: ReceiptToken,
    lane: ExecutionLane,
    rollback: RollbackClass,
    outcome: ExecutionReceiptActionOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure_code: Option<ReceiptFailureCode>,
}

impl ExecutionReceiptAction {
    /// Construct one bounded public action result.
    ///
    /// Failed and rollback-failed actions require a stable public failure code. Other outcomes must
    /// not carry one. Free-form journal messages remain outside the receipt.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid action IDs, invalid failure codes, or inconsistent outcome/code
    /// combinations.
    pub fn new(
        id: &str,
        lane: ExecutionLane,
        rollback: RollbackClass,
        outcome: ExecutionReceiptActionOutcome,
        failure_code: Option<&str>,
    ) -> Result<Self, ExecutionReceiptError> {
        let id = ReceiptToken::parse("action ID", id)?;
        let failure_code = failure_code.map(ReceiptFailureCode::parse).transpose()?;
        let requires_code = matches!(
            outcome,
            ExecutionReceiptActionOutcome::Failed | ExecutionReceiptActionOutcome::RollbackFailed
        );
        if requires_code != failure_code.is_some() {
            return Err(ExecutionReceiptError::single(
                "failed and rollback-failed actions require exactly one stable failure code",
            ));
        }
        Ok(Self {
            id,
            lane,
            rollback,
            outcome,
            failure_code,
        })
    }

    #[must_use]
    pub fn id(&self) -> &str {
        self.id.as_str()
    }

    #[must_use]
    pub const fn lane(&self) -> ExecutionLane {
        self.lane
    }

    #[must_use]
    pub const fn rollback(&self) -> RollbackClass {
        self.rollback
    }

    #[must_use]
    pub const fn outcome(&self) -> ExecutionReceiptActionOutcome {
        self.outcome
    }

    #[must_use]
    pub fn failure_code(&self) -> Option<&str> {
        self.failure_code.as_ref().map(ReceiptFailureCode::as_str)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionReceiptActionOutcome {
    Completed,
    Failed,
    Skipped,
    NotRun,
    RolledBack,
    Compensated,
    RollbackFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecutionReceiptSummary {
    total: usize,
    completed: usize,
    failed: usize,
    skipped: usize,
    not_run: usize,
    rolled_back: usize,
    compensated: usize,
    rollback_failed: usize,
}

impl ExecutionReceiptSummary {
    fn from_actions(actions: &[ExecutionReceiptAction]) -> Self {
        let mut summary = Self {
            total: actions.len(),
            completed: 0,
            failed: 0,
            skipped: 0,
            not_run: 0,
            rolled_back: 0,
            compensated: 0,
            rollback_failed: 0,
        };
        for action in actions {
            match action.outcome {
                ExecutionReceiptActionOutcome::Completed => summary.completed += 1,
                ExecutionReceiptActionOutcome::Failed => summary.failed += 1,
                ExecutionReceiptActionOutcome::Skipped => summary.skipped += 1,
                ExecutionReceiptActionOutcome::NotRun => summary.not_run += 1,
                ExecutionReceiptActionOutcome::RolledBack => summary.rolled_back += 1,
                ExecutionReceiptActionOutcome::Compensated => summary.compensated += 1,
                ExecutionReceiptActionOutcome::RollbackFailed => summary.rollback_failed += 1,
            }
        }
        summary
    }

    #[must_use]
    pub const fn total(&self) -> usize {
        self.total
    }

    #[must_use]
    pub const fn completed(&self) -> usize {
        self.completed
    }

    #[must_use]
    pub const fn failed(&self) -> usize {
        self.failed
    }

    #[must_use]
    pub const fn skipped(&self) -> usize {
        self.skipped
    }

    #[must_use]
    pub const fn not_run(&self) -> usize {
        self.not_run
    }

    #[must_use]
    pub const fn rolled_back(&self) -> usize {
        self.rolled_back
    }

    #[must_use]
    pub const fn compensated(&self) -> usize {
        self.compensated
    }

    #[must_use]
    pub const fn rollback_failed(&self) -> usize {
        self.rollback_failed
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecutionReceiptContinuation {
    fresh_observation_required: bool,
    barriers: Vec<ReceiptToken>,
    deferred_actions: Vec<ReceiptToken>,
}

impl ExecutionReceiptContinuation {
    /// Construct bounded continuation evidence.
    ///
    /// A fresh-observation requirement must name at least one barrier. A receipt that does not
    /// require fresh observation must not carry barrier identities.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid or duplicate identities, excessive entries, or inconsistent
    /// fresh-observation state.
    pub fn new(
        fresh_observation_required: bool,
        barriers: impl IntoIterator<Item = impl AsRef<str>>,
        deferred_actions: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Result<Self, ExecutionReceiptError> {
        let barriers = parse_unique_tokens("continuation barrier", barriers)?;
        let deferred_actions = parse_unique_tokens("deferred action", deferred_actions)?;
        if barriers.len() > MAX_EXECUTION_RECEIPT_CONTINUATIONS
            || deferred_actions.len() > MAX_EXECUTION_RECEIPT_CONTINUATIONS
        {
            return Err(ExecutionReceiptError::single(format!(
                "continuation identities are limited to {MAX_EXECUTION_RECEIPT_CONTINUATIONS} per class"
            )));
        }
        if fresh_observation_required != !barriers.is_empty() {
            return Err(ExecutionReceiptError::single(
                "fresh-observation state must agree with the presence of continuation barriers",
            ));
        }
        Ok(Self {
            fresh_observation_required,
            barriers,
            deferred_actions,
        })
    }

    #[must_use]
    pub const fn fresh_observation_required(&self) -> bool {
        self.fresh_observation_required
    }

    pub fn barriers(&self) -> impl Iterator<Item = &str> {
        self.barriers.iter().map(ReceiptToken::as_str)
    }

    pub fn deferred_actions(&self) -> impl Iterator<Item = &str> {
        self.deferred_actions.iter().map(ReceiptToken::as_str)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionReceiptCoverageState {
    Partial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutionReceiptOmission {
    CommandValues,
    ProcessOutput,
    FilesystemPaths,
    HostObservations,
    PreconditionEvidence,
    JournalMessages,
    Credentials,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecutionReceiptCoverage {
    state: ExecutionReceiptCoverageState,
    redacted: bool,
    truncated: bool,
    omitted: Vec<ExecutionReceiptOmission>,
}

impl ExecutionReceiptCoverage {
    fn external_v1() -> Self {
        Self {
            state: ExecutionReceiptCoverageState::Partial,
            redacted: true,
            truncated: false,
            omitted: vec![
                ExecutionReceiptOmission::CommandValues,
                ExecutionReceiptOmission::ProcessOutput,
                ExecutionReceiptOmission::FilesystemPaths,
                ExecutionReceiptOmission::HostObservations,
                ExecutionReceiptOmission::PreconditionEvidence,
                ExecutionReceiptOmission::JournalMessages,
                ExecutionReceiptOmission::Credentials,
            ],
        }
    }

    #[must_use]
    pub const fn state(&self) -> ExecutionReceiptCoverageState {
        self.state
    }

    #[must_use]
    pub const fn redacted(&self) -> bool {
        self.redacted
    }

    #[must_use]
    pub const fn truncated(&self) -> bool {
        self.truncated
    }

    #[must_use]
    pub fn omitted(&self) -> &[ExecutionReceiptOmission] {
        &self.omitted
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct ReceiptTimestamp(String);

impl ReceiptTimestamp {
    /// Parse canonical millisecond-precision UTC time (`YYYY-MM-DDTHH:MM:SS.mmmZ`).
    ///
    /// # Errors
    ///
    /// Returns an error for another offset or precision, invalid calendar values, leap seconds, or
    /// years before 1970.
    pub fn parse(value: &str) -> Result<Self, ExecutionReceiptError> {
        if !canonical_timestamp(value) {
            return Err(ExecutionReceiptError::single(
                "receipt timestamps must use canonical YYYY-MM-DDTHH:MM:SS.mmmZ UTC form",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ReceiptToken(String);

impl ReceiptToken {
    fn parse(field: &str, value: &str) -> Result<Self, ExecutionReceiptError> {
        if value.is_empty()
            || value.len() > MAX_TOKEN_LEN
            || !value.as_bytes()[0].is_ascii_alphanumeric()
            || !value.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'_' | b':' | b'-')
            })
        {
            return Err(ExecutionReceiptError::single(format!(
                "{field} must be a bounded lowercase ASCII token"
            )));
        }
        Ok(Self(value.to_owned()))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ReceiptFailureCode(String);

impl ReceiptFailureCode {
    fn parse(value: &str) -> Result<Self, ExecutionReceiptError> {
        ReceiptToken::parse("failure code", value).map(|token| Self(token.0))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecutionReceiptError {
    pub problems: Vec<String>,
}

impl ExecutionReceiptError {
    fn single(problem: impl Into<String>) -> Self {
        Self {
            problems: vec![problem.into()],
        }
    }
}

impl fmt::Display for ExecutionReceiptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "execution receipt validation failed")?;
        for problem in &self.problems {
            writeln!(formatter, "- {problem}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ExecutionReceiptError {}

/// Serialize a validated receipt as deterministic human-readable JSON.
///
/// # Errors
///
/// Returns an error if serialization fails or the encoded receipt exceeds the fixed public size
/// limit.
pub fn encode_execution_receipt(
    receipt: &ExecutionReceipt,
) -> Result<String, ExecutionReceiptError> {
    let mut encoded = serde_json::to_string_pretty(receipt).map_err(|error| {
        ExecutionReceiptError::single(format!("execution receipt serialization failed: {error}"))
    })?;
    encoded.push('\n');
    if encoded.len() > MAX_EXECUTION_RECEIPT_BYTES {
        return Err(ExecutionReceiptError::single(format!(
            "execution receipt exceeds {MAX_EXECUTION_RECEIPT_BYTES} bytes"
        )));
    }
    Ok(encoded)
}

/// Decode untrusted JSON through the exact receipt schema and all semantic validation.
///
/// # Errors
///
/// Returns an error for malformed JSON, unknown fields, unsupported versions, invalid identities,
/// inconsistent summaries, altered coverage declarations, or invalid terminal semantics.
pub fn decode_execution_receipt(input: &str) -> Result<ExecutionReceipt, ExecutionReceiptError> {
    if input.len() > MAX_EXECUTION_RECEIPT_BYTES {
        return Err(ExecutionReceiptError::single(format!(
            "execution receipt exceeds {MAX_EXECUTION_RECEIPT_BYTES} bytes"
        )));
    }
    let wire: WireExecutionReceipt = serde_json::from_str(input).map_err(|_| {
        ExecutionReceiptError::single("execution receipt JSON or schema is invalid")
    })?;
    wire.try_into()
}

fn validate_actions(actions: &[ExecutionReceiptAction]) -> Result<(), ExecutionReceiptError> {
    if actions.is_empty() || actions.len() > MAX_EXECUTION_RECEIPT_ACTIONS {
        return Err(ExecutionReceiptError::single(format!(
            "execution receipts require 1..={MAX_EXECUTION_RECEIPT_ACTIONS} actions"
        )));
    }
    let mut ids = BTreeSet::new();
    for action in actions {
        if !ids.insert(action.id.as_str()) {
            return Err(ExecutionReceiptError::single(format!(
                "duplicate receipt action ID {:?}",
                action.id.as_str()
            )));
        }
    }
    Ok(())
}

fn validate_disposition(
    disposition: ExecutionReceiptDisposition,
    actions: &[ExecutionReceiptAction],
    continuation: &ExecutionReceiptContinuation,
) -> Result<(), ExecutionReceiptError> {
    let all_completed = actions
        .iter()
        .all(|action| action.outcome == ExecutionReceiptActionOutcome::Completed);
    let has_failure = actions.iter().any(|action| {
        matches!(
            action.outcome,
            ExecutionReceiptActionOutcome::Failed | ExecutionReceiptActionOutcome::RollbackFailed
        )
    });
    match disposition {
        ExecutionReceiptDisposition::Completed => {
            if !all_completed
                || continuation.fresh_observation_required
                || !continuation.deferred_actions.is_empty()
            {
                return Err(ExecutionReceiptError::single(
                    "completed receipts require all actions completed and no continuation or deferred work",
                ));
            }
        }
        ExecutionReceiptDisposition::FreshObservationRequired => {
            if !all_completed || !continuation.fresh_observation_required {
                return Err(ExecutionReceiptError::single(
                    "fresh-observation receipts require all actions completed and one or more barriers",
                ));
            }
        }
        ExecutionReceiptDisposition::ActionFailed => {
            if !has_failure || continuation.fresh_observation_required {
                return Err(ExecutionReceiptError::single(
                    "action-failed receipts require a failed action and cannot claim a reached observation barrier",
                ));
            }
        }
    }
    Ok(())
}

fn parse_unique_tokens(
    field: &str,
    values: impl IntoIterator<Item = impl AsRef<str>>,
) -> Result<Vec<ReceiptToken>, ExecutionReceiptError> {
    let mut result = Vec::new();
    let mut seen = BTreeSet::new();
    for value in values {
        let token = ReceiptToken::parse(field, value.as_ref())?;
        if !seen.insert(token.0.clone()) {
            return Err(ExecutionReceiptError::single(format!(
                "duplicate {field} identity {:?}",
                token.as_str()
            )));
        }
        result.push(token);
    }
    Ok(result)
}

fn canonical_timestamp(value: &str) -> bool {
    if value.len() != 24 {
        return false;
    }
    let bytes = value.as_bytes();
    if bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'.'
        || bytes[23] != b'Z'
    {
        return false;
    }
    for index in [
        0_usize, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18, 20, 21, 22,
    ] {
        if !bytes[index].is_ascii_digit() {
            return false;
        }
    }
    let Some(year) = decimal(bytes, 0, 4) else {
        return false;
    };
    let Some(month) = decimal(bytes, 5, 2) else {
        return false;
    };
    let Some(day) = decimal(bytes, 8, 2) else {
        return false;
    };
    let Some(hour) = decimal(bytes, 11, 2) else {
        return false;
    };
    let Some(minute) = decimal(bytes, 14, 2) else {
        return false;
    };
    let Some(second) = decimal(bytes, 17, 2) else {
        return false;
    };
    year >= 1970
        && (1..=12).contains(&month)
        && (1..=days_in_month(year, month)).contains(&day)
        && hour <= 23
        && minute <= 59
        && second <= 59
}

fn decimal(bytes: &[u8], start: usize, len: usize) -> Option<u32> {
    bytes
        .get(start..start + len)?
        .iter()
        .try_fold(0_u32, |value, byte| {
            byte.is_ascii_digit()
                .then_some(value * 10 + u32::from(*byte - b'0'))
        })
}

const fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        2 if leap_year(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

const fn leap_year(year: u32) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireExecutionReceipt {
    document_type: WireExecutionReceiptDocumentType,
    schema_version: u8,
    producer: WireExecutionReceiptProducer,
    execution_id: String,
    started_at: String,
    terminal_at: String,
    disposition: ExecutionReceiptDisposition,
    operation: WireExecutionReceiptOperation,
    actions: Vec<WireExecutionReceiptAction>,
    summary: WireExecutionReceiptSummary,
    continuation: WireExecutionReceiptContinuation,
    coverage: WireExecutionReceiptCoverage,
}

impl TryFrom<WireExecutionReceipt> for ExecutionReceipt {
    type Error = ExecutionReceiptError;

    fn try_from(wire: WireExecutionReceipt) -> Result<Self, Self::Error> {
        let WireExecutionReceiptDocumentType::ExecutionReceipt = wire.document_type;
        if wire.schema_version != EXECUTION_RECEIPT_SCHEMA_VERSION {
            return Err(ExecutionReceiptError::single(format!(
                "execution receipt schema version {} is not supported",
                wire.schema_version
            )));
        }
        if wire.producer.name != "smolrunner" {
            return Err(ExecutionReceiptError::single(
                "execution receipt producer must be smolrunner",
            ));
        }
        let execution_id = JournalId::parse(&wire.execution_id)
            .map_err(|error| ExecutionReceiptError::single(error.to_string()))?;
        let started_at = ReceiptTimestamp::parse(&wire.started_at)?;
        let terminal_at = ReceiptTimestamp::parse(&wire.terminal_at)?;
        let actions = wire
            .actions
            .into_iter()
            .map(ExecutionReceiptAction::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let continuation = ExecutionReceiptContinuation::new(
            wire.continuation.fresh_observation_required,
            wire.continuation.barriers,
            wire.continuation.deferred_actions,
        )?;
        let receipt = match wire.operation {
            WireExecutionReceiptOperation::HostPreparation {
                schema_version,
                repository,
                source_digest,
                phase_id,
            } => {
                if schema_version != HOST_PREPARATION_OPERATION_SCHEMA_VERSION {
                    return Err(ExecutionReceiptError::single(format!(
                        "host-preparation operation schema version {schema_version} is not supported"
                    )));
                }
                let repository = RepositoryRef::parse(&repository)
                    .map_err(|error| ExecutionReceiptError::single(error.to_string()))?;
                let source_digest = Sha256Digest::parse(&source_digest)
                    .map_err(|error| ExecutionReceiptError::single(error.to_string()))?;
                ExecutionReceipt::new_host_preparation_with_producer(
                    &wire.producer.version,
                    execution_id,
                    repository,
                    source_digest,
                    &phase_id,
                    started_at,
                    terminal_at,
                    wire.disposition,
                    actions,
                    continuation,
                )?
            }
        };
        if wire.summary != receipt.summary {
            return Err(ExecutionReceiptError::single(
                "execution receipt summary does not match its actions",
            ));
        }
        if wire.coverage != receipt.coverage {
            return Err(ExecutionReceiptError::single(
                "execution receipt coverage declaration is not the external v1 boundary",
            ));
        }
        Ok(receipt)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireExecutionReceiptDocumentType {
    ExecutionReceipt,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireExecutionReceiptProducer {
    name: String,
    version: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "family", rename_all = "snake_case", deny_unknown_fields)]
enum WireExecutionReceiptOperation {
    HostPreparation {
        schema_version: u8,
        repository: String,
        source_digest: String,
        phase_id: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireExecutionReceiptAction {
    id: String,
    lane: WireExecutionLane,
    rollback: WireRollbackClass,
    outcome: ExecutionReceiptActionOutcome,
    #[serde(default)]
    failure_code: Option<String>,
}

impl TryFrom<WireExecutionReceiptAction> for ExecutionReceiptAction {
    type Error = ExecutionReceiptError;

    fn try_from(wire: WireExecutionReceiptAction) -> Result<Self, Self::Error> {
        ExecutionReceiptAction::new(
            &wire.id,
            wire.lane.into(),
            wire.rollback.into(),
            wire.outcome,
            wire.failure_code.as_deref(),
        )
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireExecutionLane {
    Operator,
    Root,
    RunnerUser,
    Github,
}

impl From<WireExecutionLane> for ExecutionLane {
    fn from(value: WireExecutionLane) -> Self {
        match value {
            WireExecutionLane::Operator => Self::Operator,
            WireExecutionLane::Root => Self::Root,
            WireExecutionLane::RunnerUser => Self::RunnerUser,
            WireExecutionLane::Github => Self::Github,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireRollbackClass {
    Reversible,
    Compensating,
    Irreversible,
}

impl From<WireRollbackClass> for RollbackClass {
    fn from(value: WireRollbackClass) -> Self {
        match value {
            WireRollbackClass::Reversible => Self::Reversible,
            WireRollbackClass::Compensating => Self::Compensating,
            WireRollbackClass::Irreversible => Self::Irreversible,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireExecutionReceiptSummary {
    total: usize,
    completed: usize,
    failed: usize,
    skipped: usize,
    not_run: usize,
    rolled_back: usize,
    compensated: usize,
    rollback_failed: usize,
}

impl PartialEq<ExecutionReceiptSummary> for WireExecutionReceiptSummary {
    fn eq(&self, other: &ExecutionReceiptSummary) -> bool {
        self.total == other.total
            && self.completed == other.completed
            && self.failed == other.failed
            && self.skipped == other.skipped
            && self.not_run == other.not_run
            && self.rolled_back == other.rolled_back
            && self.compensated == other.compensated
            && self.rollback_failed == other.rollback_failed
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireExecutionReceiptContinuation {
    fresh_observation_required: bool,
    barriers: Vec<String>,
    deferred_actions: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireExecutionReceiptCoverage {
    state: ExecutionReceiptCoverageState,
    redacted: bool,
    truncated: bool,
    omitted: Vec<ExecutionReceiptOmission>,
}

impl PartialEq<ExecutionReceiptCoverage> for WireExecutionReceiptCoverage {
    fn eq(&self, other: &ExecutionReceiptCoverage) -> bool {
        self.state == other.state
            && self.redacted == other.redacted
            && self.truncated == other.truncated
            && self.omitted == other.omitted
    }
}
