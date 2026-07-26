use std::fmt;
use std::io::{self, Write as _};

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::execution_receipt::{ExecutionReceipt, ReceiptTimestamp, validate_receipt_token};
use crate::host_preparation_execution::HostPreparationExecutionReport;
use crate::host_preparation_plan::HostReadinessSourceIdentity;
use crate::host_preparation_receipt::{
    HostPreparationReceiptContext, map_host_preparation_execution_receipt,
};
use crate::state::JournalId;

pub const HOST_PREPARATION_SOURCE_DIGEST_SCHEMA_VERSION: u8 = 1;
pub const MAX_HOST_PREPARATION_SOURCE_DIGEST_BYTES: usize = 65_536;

const SOURCE_DIGEST_DOCUMENT_TYPE: &str = "smolrunner_host_preparation_source";
const SHA256_PREFIX: &str = "sha256:";
const HEX: &[u8; 16] = b"0123456789abcdef";

#[derive(Clone, PartialEq, Eq)]
pub struct HostPreparationReceiptBinding {
    execution_id: JournalId,
    source_digest: Sha256Digest,
    phase_id: String,
    started_at: ReceiptTimestamp,
}

impl HostPreparationReceiptBinding {
    /// Bind receipt identity and exact reviewed source before the first durable execution checkpoint.
    ///
    /// The complete source identity is canonically encoded only in memory, hashed with a
    /// domain-separated schema, and then discarded. The binding retains the durable execution ID,
    /// source digest, phase ID, and explicit start timestamp needed to finish one receipt later.
    ///
    /// # Errors
    ///
    /// Returns a bounded error when the phase identity is invalid or the reviewed source cannot be
    /// encoded into the bounded digest document.
    pub fn begin(
        execution_id: JournalId,
        source: &HostReadinessSourceIdentity,
        phase_id: &str,
        started_at: ReceiptTimestamp,
    ) -> Result<Self, HostPreparationReceiptBindingError> {
        validate_receipt_token("phase ID", phase_id)
            .map_err(|_| HostPreparationReceiptBindingError::invalid_phase_identity())?;
        let source_digest = digest_host_preparation_source(source)?;
        Ok(Self {
            execution_id,
            source_digest,
            phase_id: phase_id.to_owned(),
            started_at,
        })
    }

    #[must_use]
    pub fn execution_id(&self) -> &JournalId {
        &self.execution_id
    }

    #[must_use]
    pub fn source_digest(&self) -> &Sha256Digest {
        &self.source_digest
    }

    #[must_use]
    pub fn phase_id(&self) -> &str {
        &self.phase_id
    }

    #[must_use]
    pub const fn started_at(&self) -> &ReceiptTimestamp {
        &self.started_at
    }

    /// Finish one receipt only from the terminal report produced by the bound execution.
    ///
    /// The report phase must match exactly and its retained reviewed source must reproduce the
    /// pre-execution digest. The terminal timestamp is supplied only at this terminal boundary.
    /// Consuming `self` prevents one pre-execution binding from authorizing multiple receipts.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for a terminal timestamp before the start, a phase or source
    /// mismatch, source re-encoding failure, or a terminal report that cannot satisfy receipt v1.
    pub fn finish(
        self,
        report: &HostPreparationExecutionReport,
        terminal_at: ReceiptTimestamp,
    ) -> Result<ExecutionReceipt, HostPreparationReceiptBindingError> {
        if terminal_at < self.started_at {
            return Err(HostPreparationReceiptBindingError::invalid_terminal_time());
        }
        if report.phase_id != self.phase_id {
            return Err(HostPreparationReceiptBindingError::phase_mismatch());
        }
        let report_digest = digest_host_preparation_source(&report.source)?;
        if report_digest != self.source_digest {
            return Err(HostPreparationReceiptBindingError::source_mismatch());
        }
        map_host_preparation_execution_receipt(
            report,
            HostPreparationReceiptContext {
                execution_id: self.execution_id,
                source_digest: self.source_digest,
                started_at: self.started_at,
                terminal_at,
            },
        )
        .map_err(|_| HostPreparationReceiptBindingError::invalid_terminal_report())
    }
}

impl fmt::Debug for HostPreparationReceiptBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostPreparationReceiptBinding")
            .field("execution_id", &self.execution_id)
            .field("source_digest", &self.source_digest)
            .field("phase_id", &self.phase_id)
            .field("started_at", &self.started_at)
            .finish()
    }
}

#[derive(Serialize)]
struct HostPreparationSourceDigestDocument<'a> {
    document_type: &'static str,
    schema_version: u8,
    source: &'a HostReadinessSourceIdentity,
}

/// Compute the domain-separated SHA-256 binding for one exact reviewed source identity.
///
/// Canonical JSON is deterministic for this fixed struct-and-enum source model. The encoded private
/// source is bounded, never returned, and never included in errors or the binding's `Debug` output.
///
/// # Errors
///
/// Returns a bounded error when source encoding fails, exceeds the source-document limit, or cannot
/// be represented by the canonical digest type.
pub fn digest_host_preparation_source(
    source: &HostReadinessSourceIdentity,
) -> Result<Sha256Digest, HostPreparationReceiptBindingError> {
    let document = HostPreparationSourceDigestDocument {
        document_type: SOURCE_DIGEST_DOCUMENT_TYPE,
        schema_version: HOST_PREPARATION_SOURCE_DIGEST_SCHEMA_VERSION,
        source,
    };
    let mut writer = BoundedDigestWriter::new();
    if serde_json::to_writer(&mut writer, &document).is_err() {
        return Err(if writer.exceeded {
            HostPreparationReceiptBindingError::source_too_large()
        } else {
            HostPreparationReceiptBindingError::source_encoding()
        });
    }
    let digest = writer.finish();
    let mut value = String::with_capacity(SHA256_PREFIX.len() + digest.len() * 2);
    value.push_str(SHA256_PREFIX);
    for byte in digest {
        value.push(HEX[(byte >> 4) as usize] as char);
        value.push(HEX[(byte & 0x0f) as usize] as char);
    }
    Sha256Digest::parse(&value).map_err(|_| HostPreparationReceiptBindingError::source_encoding())
}

struct BoundedDigestWriter {
    hasher: Sha256,
    bytes_written: usize,
    exceeded: bool,
}

impl BoundedDigestWriter {
    fn new() -> Self {
        Self {
            hasher: Sha256::new(),
            bytes_written: 0,
            exceeded: false,
        }
    }

    fn finish(self) -> sha2::digest::Output<Sha256> {
        self.hasher.finalize()
    }
}

impl io::Write for BoundedDigestWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let Some(next_size) = self.bytes_written.checked_add(buffer.len()) else {
            self.exceeded = true;
            return Err(io::Error::other("source digest input exceeds its bound"));
        };
        if next_size > MAX_HOST_PREPARATION_SOURCE_DIGEST_BYTES {
            self.exceeded = true;
            return Err(io::Error::other("source digest input exceeds its bound"));
        }
        self.hasher.update(buffer);
        self.bytes_written = next_size;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostPreparationReceiptBindingErrorKind {
    InvalidPhaseIdentity,
    SourceEncoding,
    SourceTooLarge,
    InvalidTerminalTime,
    PhaseMismatch,
    SourceMismatch,
    InvalidTerminalReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostPreparationReceiptBindingError {
    kind: HostPreparationReceiptBindingErrorKind,
    public_message: &'static str,
}

impl HostPreparationReceiptBindingError {
    #[must_use]
    pub const fn kind(&self) -> HostPreparationReceiptBindingErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn message(&self) -> &'static str {
        self.public_message
    }

    const fn new(
        kind: HostPreparationReceiptBindingErrorKind,
        public_message: &'static str,
    ) -> Self {
        Self {
            kind,
            public_message,
        }
    }

    const fn invalid_phase_identity() -> Self {
        Self::new(
            HostPreparationReceiptBindingErrorKind::InvalidPhaseIdentity,
            "host-preparation receipt phase identity is invalid",
        )
    }

    const fn source_encoding() -> Self {
        Self::new(
            HostPreparationReceiptBindingErrorKind::SourceEncoding,
            "reviewed host-preparation source could not be bound",
        )
    }

    const fn source_too_large() -> Self {
        Self::new(
            HostPreparationReceiptBindingErrorKind::SourceTooLarge,
            "reviewed host-preparation source exceeds the binding limit",
        )
    }

    const fn invalid_terminal_time() -> Self {
        Self::new(
            HostPreparationReceiptBindingErrorKind::InvalidTerminalTime,
            "host-preparation terminal time precedes its bound start time",
        )
    }

    const fn phase_mismatch() -> Self {
        Self::new(
            HostPreparationReceiptBindingErrorKind::PhaseMismatch,
            "terminal host-preparation phase does not match its execution binding",
        )
    }

    const fn source_mismatch() -> Self {
        Self::new(
            HostPreparationReceiptBindingErrorKind::SourceMismatch,
            "terminal host-preparation source does not match its execution binding",
        )
    }

    const fn invalid_terminal_report() -> Self {
        Self::new(
            HostPreparationReceiptBindingErrorKind::InvalidTerminalReport,
            "terminal host-preparation report cannot produce a bound receipt",
        )
    }
}

impl fmt::Display for HostPreparationReceiptBindingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.public_message)
    }
}

impl std::error::Error for HostPreparationReceiptBindingError {}
