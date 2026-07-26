use std::fmt;
use std::str;

use serde::Serialize;

use crate::execution_receipt::{
    ExecutionReceipt, decode_execution_receipt, encode_execution_receipt,
};
use crate::state::{InstallationId, JournalId, StateLayout};
use crate::state_store::{
    StateRead, StateRecord, StateStore, StateStoreError, StateStoreErrorKind, StateWriteDisposition,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionReceiptPublicationDisposition {
    Created,
    Duplicate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecutionReceiptPublication {
    disposition: ExecutionReceiptPublicationDisposition,
    execution_id: JournalId,
    bytes_written: usize,
}

impl ExecutionReceiptPublication {
    #[must_use]
    pub const fn disposition(&self) -> ExecutionReceiptPublicationDisposition {
        self.disposition
    }

    #[must_use]
    pub fn execution_id(&self) -> &JournalId {
        &self.execution_id
    }

    #[must_use]
    pub const fn bytes_written(&self) -> usize {
        self.bytes_written
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionReceiptStoreErrorKind {
    InvalidReceipt,
    Conflict,
    Busy,
    Io,
    UnsafeFilesystem,
    CorruptState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecutionReceiptStoreError {
    kind: ExecutionReceiptStoreErrorKind,
    public_message: &'static str,
}

impl ExecutionReceiptStoreError {
    #[must_use]
    pub const fn kind(&self) -> ExecutionReceiptStoreErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn message(&self) -> &'static str {
        self.public_message
    }

    const fn new(kind: ExecutionReceiptStoreErrorKind, public_message: &'static str) -> Self {
        Self {
            kind,
            public_message,
        }
    }

    const fn invalid_receipt() -> Self {
        Self::new(
            ExecutionReceiptStoreErrorKind::InvalidReceipt,
            "execution receipt could not be encoded for durable publication",
        )
    }

    const fn conflict() -> Self {
        Self::new(
            ExecutionReceiptStoreErrorKind::Conflict,
            "execution receipt identity is already bound to different durable semantics",
        )
    }

    const fn corrupt_state() -> Self {
        Self::new(
            ExecutionReceiptStoreErrorKind::CorruptState,
            "durable execution receipt state is corrupt or noncanonical",
        )
    }
}

impl fmt::Display for ExecutionReceiptStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.public_message)
    }
}

impl std::error::Error for ExecutionReceiptStoreError {}

/// Atomically publish one canonical external execution receipt without replacement.
///
/// Exact replay returns `Duplicate` after reading, validating, and comparing the existing canonical
/// receipt. Reusing an execution ID with different valid semantics returns `Conflict`. Existing
/// malformed, noncanonical, or path-mismatched content returns `CorruptState` rather than being
/// replaced.
///
/// # Errors
///
/// Returns a bounded error when receipt encoding fails, the state destination is unsafe or busy,
/// durable I/O fails, existing state is corrupt, or the execution identity conflicts.
pub fn publish_execution_receipt(
    store: &mut impl StateStore,
    installation_id: &InstallationId,
    receipt: &ExecutionReceipt,
) -> Result<ExecutionReceiptPublication, ExecutionReceiptStoreError> {
    let record = StateRecord::execution_receipt(installation_id, receipt)
        .map_err(|_| ExecutionReceiptStoreError::invalid_receipt())?;
    match store.create_atomic(&record) {
        Ok(write) => {
            if write.disposition() != StateWriteDisposition::Created {
                return Err(ExecutionReceiptStoreError::corrupt_state());
            }
            Ok(ExecutionReceiptPublication {
                disposition: ExecutionReceiptPublicationDisposition::Created,
                execution_id: receipt.execution_id().clone(),
                bytes_written: write.bytes_written(),
            })
        }
        Err(error) if error.kind() == StateStoreErrorKind::Conflict => {
            match read_execution_receipt(store, installation_id, receipt.execution_id())? {
                Some(existing) if existing == *receipt => Ok(ExecutionReceiptPublication {
                    disposition: ExecutionReceiptPublicationDisposition::Duplicate,
                    execution_id: receipt.execution_id().clone(),
                    bytes_written: record.bytes().len(),
                }),
                Some(_) | None => Err(ExecutionReceiptStoreError::conflict()),
            }
        }
        Err(error) => Err(map_store_error(error)),
    }
}

/// Read one receipt by exact installation and execution identity.
///
/// Present bytes must decode through receipt v1, bind the requested execution ID, and exactly match
/// deterministic canonical encoding. This rejects hand-edited, alternate-format, truncated, or
/// identity-mismatched files even when a permissive JSON parser could otherwise read them.
///
/// # Errors
///
/// Returns a bounded error for unsafe state, durable I/O failure, invalid UTF-8, invalid receipt
/// semantics, noncanonical bytes, or an execution identity mismatch.
pub fn read_execution_receipt(
    store: &impl StateStore,
    installation_id: &InstallationId,
    execution_id: &JournalId,
) -> Result<Option<ExecutionReceipt>, ExecutionReceiptStoreError> {
    let path = StateLayout::execution_receipt_document(installation_id, execution_id);
    let bytes = match store.read(&path).map_err(map_store_error)? {
        StateRead::Missing => return Ok(None),
        StateRead::Present(bytes) => bytes,
    };
    let input = str::from_utf8(&bytes).map_err(|_| ExecutionReceiptStoreError::corrupt_state())?;
    let receipt =
        decode_execution_receipt(input).map_err(|_| ExecutionReceiptStoreError::corrupt_state())?;
    if receipt.execution_id() != execution_id {
        return Err(ExecutionReceiptStoreError::corrupt_state());
    }
    let canonical = encode_execution_receipt(&receipt)
        .map_err(|_| ExecutionReceiptStoreError::corrupt_state())?;
    if canonical.as_bytes() != bytes {
        return Err(ExecutionReceiptStoreError::corrupt_state());
    }
    Ok(Some(receipt))
}

fn map_store_error(error: StateStoreError) -> ExecutionReceiptStoreError {
    match error.kind() {
        StateStoreErrorKind::Busy => ExecutionReceiptStoreError::new(
            ExecutionReceiptStoreErrorKind::Busy,
            "execution receipt state is busy",
        ),
        StateStoreErrorKind::Conflict => ExecutionReceiptStoreError::conflict(),
        StateStoreErrorKind::Io => ExecutionReceiptStoreError::new(
            ExecutionReceiptStoreErrorKind::Io,
            "execution receipt state could not be read or published",
        ),
        StateStoreErrorKind::UnsafeFilesystem => ExecutionReceiptStoreError::new(
            ExecutionReceiptStoreErrorKind::UnsafeFilesystem,
            "execution receipt state contains an unsafe filesystem object",
        ),
        StateStoreErrorKind::CorruptState => ExecutionReceiptStoreError::corrupt_state(),
    }
}
