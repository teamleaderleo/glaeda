//! Canonical protocol-v3 transport frames for trusted guest-control transactions.
//!
//! The common layer preserves exact nested JSON bodies without importing operation-specific Rust
//! types. Typed operation codecs still own semantic decoding and request/result confirmation. This
//! module performs no process execution, filesystem I/O, guest observation, or mutation.
//!
//! Protocol v3 is the Glaeda generation used for fresh transactions. Explicit legacy-v2 decoders
//! retain exact SmolRunner interpretation for old-state inspection and retirement only.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::trusted_guest_control_protocol::{
    LegacySmolRunnerTrustedGuestControlReceiptV2, LegacySmolRunnerTrustedGuestControlRequestV2,
    TrustedGuestControlOperation, TrustedGuestControlOutcome, TrustedGuestControlReceipt,
    TrustedGuestControlRequest, decode_legacy_smolrunner_trusted_guest_control_receipt_v2,
    decode_legacy_smolrunner_trusted_guest_control_request_v2,
    decode_trusted_guest_control_receipt_body, decode_trusted_guest_control_request_body,
    encode_trusted_guest_control_receipt_body, encode_trusted_guest_control_request_body,
};

pub const TRUSTED_GUEST_CONTROL_TRANSACTION_SCHEMA_VERSION: u8 = 3;
pub const LEGACY_SMOLRUNNER_TRUSTED_GUEST_CONTROL_TRANSACTION_SCHEMA_VERSION: u8 = 2;
pub const MAX_TRUSTED_GUEST_CONTROL_TRANSACTION_BYTES: usize = 16 * 1024;
pub const MAX_TRUSTED_GUEST_CONTROL_TRANSACTION_RECEIPT_BYTES: usize = 8 * 1024;

const PAYLOAD_BODY_DIGEST_DOMAIN: &[u8] = b"glaeda-trusted-guest-control-payload-body-v3\0";
const RESULT_BODY_DIGEST_DOMAIN: &[u8] = b"glaeda-trusted-guest-control-result-body-v3\0";
const TRANSACTION_DIGEST_DOMAIN: &[u8] = b"glaeda-trusted-guest-control-transaction-v3\0";
const TRANSACTION_RECEIPT_DIGEST_DOMAIN: &[u8] =
    b"glaeda-trusted-guest-control-transaction-receipt-v3\0";
const LEGACY_SMOLRUNNER_PAYLOAD_BODY_DIGEST_DOMAIN: &[u8] =
    b"smolrunner-trusted-guest-control-payload-body-v2\0";
const LEGACY_SMOLRUNNER_RESULT_BODY_DIGEST_DOMAIN: &[u8] =
    b"smolrunner-trusted-guest-control-result-body-v2\0";
const LEGACY_SMOLRUNNER_TRANSACTION_DIGEST_DOMAIN: &[u8] =
    b"smolrunner-trusted-guest-control-transaction-v2\0";
const LEGACY_SMOLRUNNER_TRANSACTION_RECEIPT_DIGEST_DOMAIN: &[u8] =
    b"smolrunner-trusted-guest-control-transaction-receipt-v2\0";
const SHA256_PREFIX: &str = "sha256:";
const HEX: &[u8; 16] = b"0123456789abcdef";

#[derive(Clone, PartialEq, Eq)]
pub struct TrustedGuestControlTransaction {
    request: TrustedGuestControlRequest,
    payload_body: Vec<u8>,
}

impl TrustedGuestControlTransaction {
    /// Bind one exact common request to one exact operation payload body.
    ///
    /// # Errors
    ///
    /// Returns a bounded error unless the payload is one JSON object, its standardized digest
    /// equals the request payload digest, and the complete canonical frame fits the v3 ceiling.
    pub fn new(
        request: TrustedGuestControlRequest,
        payload_body: Vec<u8>,
    ) -> Result<Self, TrustedGuestControlTransactionError> {
        validate_json_object(&payload_body, MAX_TRUSTED_GUEST_CONTROL_TRANSACTION_BYTES)?;
        let digest = trusted_guest_control_payload_body_digest(request.operation(), &payload_body)?;
        if request.payload_digest() != &digest {
            return Err(digest_mismatch());
        }
        let transaction = Self {
            request,
            payload_body,
        };
        encode_trusted_guest_control_transaction(&transaction)?;
        Ok(transaction)
    }

    #[must_use]
    pub const fn request(&self) -> &TrustedGuestControlRequest {
        &self.request
    }

    #[must_use]
    #[allow(dead_code)]
    pub(crate) fn payload_body(&self) -> &[u8] {
        &self.payload_body
    }
}

impl fmt::Debug for TrustedGuestControlTransaction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedGuestControlTransaction")
            .field("request", &self.request)
            .field("payload_body", &"<bounded-canonical-operation-payload>")
            .finish()
    }
}

/// Canonically decoded SmolRunner transaction-v2 frame retained for inspection only.
#[derive(Clone, PartialEq, Eq)]
pub struct LegacySmolRunnerTrustedGuestControlTransactionV2 {
    request: LegacySmolRunnerTrustedGuestControlRequestV2,
    payload_body: Vec<u8>,
    canonical: Vec<u8>,
}

impl LegacySmolRunnerTrustedGuestControlTransactionV2 {
    #[must_use]
    pub const fn request(&self) -> &LegacySmolRunnerTrustedGuestControlRequestV2 {
        &self.request
    }

    #[must_use]
    pub fn payload_body(&self) -> &[u8] {
        &self.payload_body
    }

    #[must_use]
    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical
    }
}

impl fmt::Debug for LegacySmolRunnerTrustedGuestControlTransactionV2 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LegacySmolRunnerTrustedGuestControlTransactionV2")
            .field("request", &self.request)
            .field("payload_body", &"<bounded-canonical-operation-payload>")
            .field("canonical", &"<exact-smolrunner-v2-frame>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct TrustedGuestControlTransactionReceipt {
    receipt: TrustedGuestControlReceipt,
    receipt_body: Vec<u8>,
    result_body: Option<Vec<u8>>,
}

impl TrustedGuestControlTransactionReceipt {
    /// Bind one terminal common receipt to the exact optional operation result body.
    ///
    /// # Errors
    ///
    /// Success requires one JSON result object whose standardized digest equals the common result
    /// digest. Refusal and recovery outcomes require `None`. The complete frame is bounded.
    pub fn new(
        request: &TrustedGuestControlRequest,
        outcome: &TrustedGuestControlOutcome,
        result_body: Option<Vec<u8>>,
    ) -> Result<Self, TrustedGuestControlTransactionError> {
        validate_outcome_result(request.operation(), outcome, result_body.as_deref())?;
        let receipt_body = encode_trusted_guest_control_receipt_body(request, outcome)
            .map_err(|_| protocol_error())?;
        let receipt = decode_trusted_guest_control_receipt_body(&receipt_body, request)
            .map_err(|_| protocol_error())?;
        let transaction_receipt = Self {
            receipt,
            receipt_body,
            result_body,
        };
        encode_trusted_guest_control_transaction_receipt(&transaction_receipt)?;
        Ok(transaction_receipt)
    }

    #[must_use]
    pub const fn receipt(&self) -> &TrustedGuestControlReceipt {
        &self.receipt
    }

    #[must_use]
    #[allow(dead_code)]
    pub(crate) fn result_body(&self) -> Option<&[u8]> {
        self.result_body.as_deref()
    }
}

impl fmt::Debug for TrustedGuestControlTransactionReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedGuestControlTransactionReceipt")
            .field("receipt", &self.receipt)
            .field(
                "result_body",
                &self
                    .result_body
                    .as_ref()
                    .map(|_| "<bounded-canonical-operation-result>"),
            )
            .finish()
    }
}

/// Canonically decoded SmolRunner transaction-receipt-v2 frame retained for inspection only.
#[derive(Clone, PartialEq, Eq)]
pub struct LegacySmolRunnerTrustedGuestControlTransactionReceiptV2 {
    receipt: LegacySmolRunnerTrustedGuestControlReceiptV2,
    result_body: Option<Vec<u8>>,
    canonical: Vec<u8>,
}

impl LegacySmolRunnerTrustedGuestControlTransactionReceiptV2 {
    #[must_use]
    pub const fn receipt(&self) -> &LegacySmolRunnerTrustedGuestControlReceiptV2 {
        &self.receipt
    }

    #[must_use]
    pub fn result_body(&self) -> Option<&[u8]> {
        self.result_body.as_deref()
    }

    #[must_use]
    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical
    }
}

impl fmt::Debug for LegacySmolRunnerTrustedGuestControlTransactionReceiptV2 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LegacySmolRunnerTrustedGuestControlTransactionReceiptV2")
            .field("receipt", &self.receipt)
            .field(
                "result_body",
                &self
                    .result_body
                    .as_ref()
                    .map(|_| "<bounded-canonical-operation-result>"),
            )
            .field("canonical", &"<exact-smolrunner-v2-frame>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustedGuestControlTransactionErrorKind {
    DigestMismatch,
    OutcomeMismatch,
    TooLarge,
    Malformed,
    VersionIncompatible,
    NonCanonical,
    Protocol,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TrustedGuestControlTransactionError {
    kind: TrustedGuestControlTransactionErrorKind,
    code: &'static str,
    message: &'static str,
}

impl TrustedGuestControlTransactionError {
    #[must_use]
    pub const fn kind(self) -> TrustedGuestControlTransactionErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for TrustedGuestControlTransactionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedGuestControlTransactionError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for TrustedGuestControlTransactionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for TrustedGuestControlTransactionError {}

pub fn encode_trusted_guest_control_transaction(
    transaction: &TrustedGuestControlTransaction,
) -> Result<Vec<u8>, TrustedGuestControlTransactionError> {
    let request_body = encode_trusted_guest_control_request_body(&transaction.request)
        .map_err(|_| protocol_error())?;
    let request = raw_json_object(&request_body)?;
    let payload = raw_json_object(&transaction.payload_body)?;
    canonical_frame(
        &TransactionEncodeWire {
            schema_version: TRUSTED_GUEST_CONTROL_TRANSACTION_SCHEMA_VERSION,
            request: &request,
            payload: &payload,
        },
        MAX_TRUSTED_GUEST_CONTROL_TRANSACTION_BYTES,
    )
}

pub fn decode_trusted_guest_control_transaction(
    bytes: &[u8],
) -> Result<TrustedGuestControlTransaction, TrustedGuestControlTransactionError> {
    require_frame_size(bytes, MAX_TRUSTED_GUEST_CONTROL_TRANSACTION_BYTES)?;
    let wire: TransactionDecodeWire = serde_json::from_slice(bytes).map_err(|_| malformed())?;
    if wire.schema_version != TRUSTED_GUEST_CONTROL_TRANSACTION_SCHEMA_VERSION {
        return Err(version_incompatible());
    }
    let request = decode_trusted_guest_control_request_body(wire.request.get().as_bytes())
        .map_err(|_| protocol_error())?;
    let transaction =
        TrustedGuestControlTransaction::new(request, wire.payload.get().as_bytes().to_vec())?;
    if encode_trusted_guest_control_transaction(&transaction)? != bytes {
        return Err(noncanonical());
    }
    Ok(transaction)
}

/// Decode one exact canonical SmolRunner v2 transaction for inspection or retirement planning.
pub fn decode_legacy_smolrunner_trusted_guest_control_transaction_v2(
    bytes: &[u8],
) -> Result<LegacySmolRunnerTrustedGuestControlTransactionV2, TrustedGuestControlTransactionError> {
    require_frame_size(bytes, MAX_TRUSTED_GUEST_CONTROL_TRANSACTION_BYTES)?;
    let wire: TransactionDecodeWire = serde_json::from_slice(bytes).map_err(|_| malformed())?;
    if wire.schema_version != LEGACY_SMOLRUNNER_TRUSTED_GUEST_CONTROL_TRANSACTION_SCHEMA_VERSION {
        return Err(version_incompatible());
    }
    let request_raw = wire.request.get().as_bytes();
    let payload_raw = wire.payload.get().as_bytes();
    validate_json_object(payload_raw, MAX_TRUSTED_GUEST_CONTROL_TRANSACTION_BYTES)?;
    let mut request_bytes = request_raw.to_vec();
    request_bytes.push(b'\n');
    let request = decode_legacy_smolrunner_trusted_guest_control_request_v2(&request_bytes)
        .map_err(|_| protocol_error())?;
    let payload_body = payload_raw.to_vec();
    if request.request().payload_digest()
        != &legacy_smolrunner_trusted_guest_control_payload_body_v2_digest(
            request.request().operation(),
            &payload_body,
        )?
    {
        return Err(digest_mismatch());
    }
    let request_value = raw_json_object(request_raw)?;
    let payload_value = raw_json_object(&payload_body)?;
    let canonical = canonical_frame(
        &TransactionEncodeWire {
            schema_version: LEGACY_SMOLRUNNER_TRUSTED_GUEST_CONTROL_TRANSACTION_SCHEMA_VERSION,
            request: &request_value,
            payload: &payload_value,
        },
        MAX_TRUSTED_GUEST_CONTROL_TRANSACTION_BYTES,
    )?;
    if canonical != bytes {
        return Err(noncanonical());
    }
    Ok(LegacySmolRunnerTrustedGuestControlTransactionV2 {
        request,
        payload_body,
        canonical,
    })
}

pub fn trusted_guest_control_transaction_digest(
    transaction: &TrustedGuestControlTransaction,
) -> Result<Sha256Digest, TrustedGuestControlTransactionError> {
    domain_digest(
        TRANSACTION_DIGEST_DOMAIN,
        &encode_trusted_guest_control_transaction(transaction)?,
    )
}

pub fn legacy_smolrunner_trusted_guest_control_transaction_v2_digest(
    transaction: &LegacySmolRunnerTrustedGuestControlTransactionV2,
) -> Result<Sha256Digest, TrustedGuestControlTransactionError> {
    domain_digest(
        LEGACY_SMOLRUNNER_TRANSACTION_DIGEST_DOMAIN,
        transaction.canonical_bytes(),
    )
}

pub fn encode_trusted_guest_control_transaction_receipt(
    transaction_receipt: &TrustedGuestControlTransactionReceipt,
) -> Result<Vec<u8>, TrustedGuestControlTransactionError> {
    let receipt = raw_json_object(&transaction_receipt.receipt_body)?;
    let result = transaction_receipt
        .result_body
        .as_deref()
        .map(raw_json_object)
        .transpose()?;
    canonical_frame(
        &TransactionReceiptEncodeWire {
            schema_version: TRUSTED_GUEST_CONTROL_TRANSACTION_SCHEMA_VERSION,
            receipt: &receipt,
            result: result.as_deref(),
        },
        MAX_TRUSTED_GUEST_CONTROL_TRANSACTION_RECEIPT_BYTES,
    )
}

pub fn decode_trusted_guest_control_transaction_receipt(
    bytes: &[u8],
    expected_request: &TrustedGuestControlRequest,
) -> Result<TrustedGuestControlTransactionReceipt, TrustedGuestControlTransactionError> {
    require_frame_size(bytes, MAX_TRUSTED_GUEST_CONTROL_TRANSACTION_RECEIPT_BYTES)?;
    let wire: TransactionReceiptDecodeWire =
        serde_json::from_slice(bytes).map_err(|_| malformed())?;
    if wire.schema_version != TRUSTED_GUEST_CONTROL_TRANSACTION_SCHEMA_VERSION {
        return Err(version_incompatible());
    }
    let receipt_body = wire.receipt.get().as_bytes().to_vec();
    let receipt = decode_trusted_guest_control_receipt_body(&receipt_body, expected_request)
        .map_err(|_| protocol_error())?;
    let result_body = wire.result.map(|result| result.get().as_bytes().to_vec());
    validate_outcome_result(
        expected_request.operation(),
        receipt.outcome(),
        result_body.as_deref(),
    )?;
    let transaction_receipt = TrustedGuestControlTransactionReceipt {
        receipt,
        receipt_body,
        result_body,
    };
    if encode_trusted_guest_control_transaction_receipt(&transaction_receipt)? != bytes {
        return Err(noncanonical());
    }
    Ok(transaction_receipt)
}

/// Decode one exact canonical SmolRunner v2 transaction receipt for inspection only.
pub fn decode_legacy_smolrunner_trusted_guest_control_transaction_receipt_v2(
    bytes: &[u8],
    expected_transaction: &LegacySmolRunnerTrustedGuestControlTransactionV2,
) -> Result<LegacySmolRunnerTrustedGuestControlTransactionReceiptV2, TrustedGuestControlTransactionError>
{
    require_frame_size(bytes, MAX_TRUSTED_GUEST_CONTROL_TRANSACTION_RECEIPT_BYTES)?;
    let wire: TransactionReceiptDecodeWire =
        serde_json::from_slice(bytes).map_err(|_| malformed())?;
    if wire.schema_version != LEGACY_SMOLRUNNER_TRUSTED_GUEST_CONTROL_TRANSACTION_SCHEMA_VERSION {
        return Err(version_incompatible());
    }
    let receipt_raw = wire.receipt.get().as_bytes();
    let mut receipt_bytes = receipt_raw.to_vec();
    receipt_bytes.push(b'\n');
    let receipt = decode_legacy_smolrunner_trusted_guest_control_receipt_v2(
        &receipt_bytes,
        expected_transaction.request(),
    )
    .map_err(|_| protocol_error())?;
    let result_body = wire.result.map(|result| result.get().as_bytes().to_vec());
    validate_outcome_result_with_domain(
        LEGACY_SMOLRUNNER_RESULT_BODY_DIGEST_DOMAIN,
        expected_transaction.request().request().operation(),
        receipt.receipt().outcome(),
        result_body.as_deref(),
    )?;
    let receipt_value = raw_json_object(receipt_raw)?;
    let result_value = result_body
        .as_deref()
        .map(raw_json_object)
        .transpose()?;
    let canonical = canonical_frame(
        &TransactionReceiptEncodeWire {
            schema_version: LEGACY_SMOLRUNNER_TRUSTED_GUEST_CONTROL_TRANSACTION_SCHEMA_VERSION,
            receipt: &receipt_value,
            result: result_value.as_deref(),
        },
        MAX_TRUSTED_GUEST_CONTROL_TRANSACTION_RECEIPT_BYTES,
    )?;
    if canonical != bytes {
        return Err(noncanonical());
    }
    Ok(LegacySmolRunnerTrustedGuestControlTransactionReceiptV2 {
        receipt,
        result_body,
        canonical,
    })
}

pub fn trusted_guest_control_transaction_receipt_digest(
    transaction_receipt: &TrustedGuestControlTransactionReceipt,
) -> Result<Sha256Digest, TrustedGuestControlTransactionError> {
    domain_digest(
        TRANSACTION_RECEIPT_DIGEST_DOMAIN,
        &encode_trusted_guest_control_transaction_receipt(transaction_receipt)?,
    )
}

pub fn legacy_smolrunner_trusted_guest_control_transaction_receipt_v2_digest(
    transaction_receipt: &LegacySmolRunnerTrustedGuestControlTransactionReceiptV2,
) -> Result<Sha256Digest, TrustedGuestControlTransactionError> {
    domain_digest(
        LEGACY_SMOLRUNNER_TRANSACTION_RECEIPT_DIGEST_DOMAIN,
        transaction_receipt.canonical_bytes(),
    )
}

pub fn trusted_guest_control_payload_body_digest(
    operation: TrustedGuestControlOperation,
    body: &[u8],
) -> Result<Sha256Digest, TrustedGuestControlTransactionError> {
    operation_body_digest(PAYLOAD_BODY_DIGEST_DOMAIN, operation, body)
}

pub fn legacy_smolrunner_trusted_guest_control_payload_body_v2_digest(
    operation: TrustedGuestControlOperation,
    body: &[u8],
) -> Result<Sha256Digest, TrustedGuestControlTransactionError> {
    operation_body_digest(
        LEGACY_SMOLRUNNER_PAYLOAD_BODY_DIGEST_DOMAIN,
        operation,
        body,
    )
}

pub fn trusted_guest_control_result_body_digest(
    operation: TrustedGuestControlOperation,
    body: &[u8],
) -> Result<Sha256Digest, TrustedGuestControlTransactionError> {
    operation_body_digest(RESULT_BODY_DIGEST_DOMAIN, operation, body)
}

pub fn legacy_smolrunner_trusted_guest_control_result_body_v2_digest(
    operation: TrustedGuestControlOperation,
    body: &[u8],
) -> Result<Sha256Digest, TrustedGuestControlTransactionError> {
    operation_body_digest(
        LEGACY_SMOLRUNNER_RESULT_BODY_DIGEST_DOMAIN,
        operation,
        body,
    )
}

fn validate_outcome_result(
    operation: TrustedGuestControlOperation,
    outcome: &TrustedGuestControlOutcome,
    result_body: Option<&[u8]>,
) -> Result<(), TrustedGuestControlTransactionError> {
    validate_outcome_result_with_domain(RESULT_BODY_DIGEST_DOMAIN, operation, outcome, result_body)
}

fn validate_outcome_result_with_domain(
    result_domain: &[u8],
    operation: TrustedGuestControlOperation,
    outcome: &TrustedGuestControlOutcome,
    result_body: Option<&[u8]>,
) -> Result<(), TrustedGuestControlTransactionError> {
    match (outcome, result_body) {
        (TrustedGuestControlOutcome::Succeeded { result_digest }, Some(body)) => {
            validate_json_object(body, MAX_TRUSTED_GUEST_CONTROL_TRANSACTION_RECEIPT_BYTES)?;
            if &operation_body_digest(result_domain, operation, body)? != result_digest {
                return Err(digest_mismatch());
            }
            Ok(())
        }
        (TrustedGuestControlOutcome::Succeeded { .. }, None)
        | (TrustedGuestControlOutcome::Refused(_), Some(_))
        | (TrustedGuestControlOutcome::RecoveryRequired(_), Some(_)) => Err(outcome_mismatch()),
        (TrustedGuestControlOutcome::Refused(_), None)
        | (TrustedGuestControlOutcome::RecoveryRequired(_), None) => Ok(()),
    }
}

fn operation_body_digest(
    domain: &[u8],
    operation: TrustedGuestControlOperation,
    body: &[u8],
) -> Result<Sha256Digest, TrustedGuestControlTransactionError> {
    validate_json_object(body, MAX_TRUSTED_GUEST_CONTROL_TRANSACTION_BYTES)?;
    let operation = serde_json::to_vec(&operation).map_err(|_| malformed())?;
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(operation);
    hasher.update([0]);
    hasher.update(body);
    digest_bytes(&hasher.finalize())
}

fn raw_json_object(bytes: &[u8]) -> Result<Box<RawValue>, TrustedGuestControlTransactionError> {
    validate_json_object(bytes, MAX_TRUSTED_GUEST_CONTROL_TRANSACTION_BYTES)?;
    let text = std::str::from_utf8(bytes).map_err(|_| malformed())?;
    serde_json::from_str(text).map_err(|_| malformed())
}

fn validate_json_object(
    bytes: &[u8],
    maximum: usize,
) -> Result<(), TrustedGuestControlTransactionError> {
    if bytes.is_empty() || bytes.len() > maximum {
        return Err(too_large());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| malformed())?;
    let trimmed = text.trim_matches([' ', '\n', '\r', '\t']);
    if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
        return Err(malformed());
    }
    let _: Box<RawValue> = serde_json::from_str(text).map_err(|_| malformed())?;
    Ok(())
}

fn canonical_frame(
    value: &impl Serialize,
    maximum: usize,
) -> Result<Vec<u8>, TrustedGuestControlTransactionError> {
    let mut bytes = serde_json::to_vec(value).map_err(|_| malformed())?;
    bytes.push(b'\n');
    require_frame_size(&bytes, maximum)?;
    Ok(bytes)
}

fn require_frame_size(
    bytes: &[u8],
    maximum: usize,
) -> Result<(), TrustedGuestControlTransactionError> {
    if bytes.is_empty() || bytes.len() > maximum {
        return Err(too_large());
    }
    Ok(())
}

fn domain_digest(
    domain: &[u8],
    bytes: &[u8],
) -> Result<Sha256Digest, TrustedGuestControlTransactionError> {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(bytes);
    digest_bytes(&hasher.finalize())
}

fn digest_bytes(bytes: &[u8]) -> Result<Sha256Digest, TrustedGuestControlTransactionError> {
    let mut value = String::with_capacity(SHA256_PREFIX.len() + bytes.len() * 2);
    value.push_str(SHA256_PREFIX);
    for byte in bytes {
        value.push(char::from(HEX[usize::from(byte >> 4)]));
        value.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Sha256Digest::parse(&value).map_err(|_| malformed())
}

#[derive(Serialize)]
struct TransactionEncodeWire<'a> {
    schema_version: u8,
    request: &'a RawValue,
    payload: &'a RawValue,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TransactionDecodeWire {
    schema_version: u8,
    request: Box<RawValue>,
    payload: Box<RawValue>,
}

#[derive(Serialize)]
struct TransactionReceiptEncodeWire<'a> {
    schema_version: u8,
    receipt: &'a RawValue,
    result: Option<&'a RawValue>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TransactionReceiptDecodeWire {
    schema_version: u8,
    receipt: Box<RawValue>,
    result: Option<Box<RawValue>>,
}

const fn transaction_error(
    kind: TrustedGuestControlTransactionErrorKind,
    code: &'static str,
    message: &'static str,
) -> TrustedGuestControlTransactionError {
    TrustedGuestControlTransactionError {
        kind,
        code,
        message,
    }
}

const fn digest_mismatch() -> TrustedGuestControlTransactionError {
    transaction_error(
        TrustedGuestControlTransactionErrorKind::DigestMismatch,
        "trusted_guest_control_transaction_digest_mismatch",
        "trusted guest-control transaction body digest does not match its common envelope",
    )
}

const fn outcome_mismatch() -> TrustedGuestControlTransactionError {
    transaction_error(
        TrustedGuestControlTransactionErrorKind::OutcomeMismatch,
        "trusted_guest_control_transaction_outcome_mismatch",
        "trusted guest-control transaction result presence does not match its outcome",
    )
}

const fn too_large() -> TrustedGuestControlTransactionError {
    transaction_error(
        TrustedGuestControlTransactionErrorKind::TooLarge,
        "trusted_guest_control_transaction_too_large",
        "trusted guest-control transaction exceeds its bounded size",
    )
}

const fn malformed() -> TrustedGuestControlTransactionError {
    transaction_error(
        TrustedGuestControlTransactionErrorKind::Malformed,
        "trusted_guest_control_transaction_malformed",
        "trusted guest-control transaction is malformed",
    )
}

const fn version_incompatible() -> TrustedGuestControlTransactionError {
    transaction_error(
        TrustedGuestControlTransactionErrorKind::VersionIncompatible,
        "trusted_guest_control_transaction_version_incompatible",
        "trusted guest-control transaction version is unsupported",
    )
}

const fn noncanonical() -> TrustedGuestControlTransactionError {
    transaction_error(
        TrustedGuestControlTransactionErrorKind::NonCanonical,
        "trusted_guest_control_transaction_noncanonical",
        "trusted guest-control transaction is noncanonical",
    )
}

const fn protocol_error() -> TrustedGuestControlTransactionError {
    transaction_error(
        TrustedGuestControlTransactionErrorKind::Protocol,
        "trusted_guest_control_transaction_protocol_invalid",
        "trusted guest-control transaction contains an invalid common envelope",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_catalog::ProjectIdentity;
    use crate::project_disk_lease::{ResidentSandboxGeneration, ResidentSandboxId};
    use crate::trusted_guest_control_probe_protocol::{
        TrustedGuestControlProbePayload, TrustedGuestControlProbeResult,
        decode_trusted_guest_control_probe_payload_body,
        encode_trusted_guest_control_probe_payload_body,
        encode_trusted_guest_control_probe_result_body, trusted_guest_control_probe_payload_digest,
        trusted_guest_control_probe_result_digest,
    };
    use crate::trusted_guest_control_protocol::{
        TrustedGuestControlArchitecture, TrustedGuestControlAuthority,
        TrustedGuestControlBinaryBinding, TrustedGuestControlRefusal, TrustedGuestControlRequestId,
        TrustedGuestControlResidentAuthorityGeneration, TrustedGuestControlResidentConfigClaim,
        TrustedGuestControlResidentConfigGeneration,
    };
    use crate::trusted_project_filesystem_guest_protocol::{
        MAX_TRUSTED_PROJECT_FILESYSTEM_PAYLOAD_BYTES, MAX_TRUSTED_PROJECT_FILESYSTEM_RESULT_BYTES,
        decode_trusted_project_filesystem_payload_body,
        decode_trusted_project_filesystem_result_body,
        encode_trusted_project_filesystem_payload_body,
        encode_trusted_project_filesystem_result_body, trusted_project_filesystem_payload_digest,
        trusted_project_filesystem_result_digest,
    };

    fn digest(byte: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    fn authority() -> TrustedGuestControlAuthority {
        TrustedGuestControlAuthority::resident_sandbox(
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            ResidentSandboxId::parse("resident-a").unwrap(),
            ResidentSandboxGeneration::new(3).unwrap(),
            TrustedGuestControlResidentConfigGeneration::new(4).unwrap(),
            TrustedGuestControlResidentConfigClaim::new(digest('a')),
            TrustedGuestControlResidentAuthorityGeneration::new(5).unwrap(),
        )
    }

    fn binary() -> TrustedGuestControlBinaryBinding {
        TrustedGuestControlBinaryBinding::new(
            7,
            digest('b'),
            TrustedGuestControlArchitecture::LinuxAarch64,
        )
        .unwrap()
    }

    fn fixture(
        request_id: &str,
    ) -> (
        TrustedGuestControlRequest,
        TrustedGuestControlProbePayload,
        Vec<u8>,
    ) {
        let authority = authority();
        let binary = binary();
        let payload = TrustedGuestControlProbePayload::new(&authority, &binary, 9).unwrap();
        let body = encode_trusted_guest_control_probe_payload_body(&payload).unwrap();
        let request = TrustedGuestControlRequest::new(
            TrustedGuestControlRequestId::parse(request_id).unwrap(),
            binary,
            authority,
            TrustedGuestControlOperation::ProbeGuestControl,
            trusted_guest_control_probe_payload_digest(&payload).unwrap(),
        )
        .unwrap();
        (request, payload, body)
    }

    #[test]
    fn exact_request_and_payload_round_trip_as_nested_raw_objects() {
        let (request, payload, body) = fixture("probe-1");
        let transaction = TrustedGuestControlTransaction::new(request.clone(), body).unwrap();
        let bytes = encode_trusted_guest_control_transaction(&transaction).unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(text.contains("\"schema_version\":3"));
        assert!(text.contains("\"payload\":{\"schema_version\":1"));
        assert!(!text.contains("\"payload\":\""));
        assert!(bytes.ends_with(b"\n"));

        let decoded = decode_trusted_guest_control_transaction(&bytes).unwrap();
        assert_eq!(decoded.request(), &request);
        assert_eq!(decoded.payload_body(), transaction.payload_body());
        let typed =
            decode_trusted_guest_control_probe_payload_body(decoded.payload_body()).unwrap();
        assert_eq!(typed, payload);
        typed.confirm_common_request(decoded.request()).unwrap();
    }

    #[test]
    fn legacy_v2_transaction_is_inspection_only_and_has_distinct_domains() {
        let (_, payload, body) = fixture("probe-legacy");
        let authority = authority();
        let binary = binary();
        let request = TrustedGuestControlRequest::new(
            TrustedGuestControlRequestId::parse("probe-legacy").unwrap(),
            binary,
            authority,
            TrustedGuestControlOperation::ProbeGuestControl,
            legacy_smolrunner_trusted_guest_control_payload_body_v2_digest(
                TrustedGuestControlOperation::ProbeGuestControl,
                &body,
            )
            .unwrap(),
        )
        .unwrap();
        let current_request_body = encode_trusted_guest_control_request_body(&request).unwrap();
        let legacy_request_body = std::str::from_utf8(&current_request_body)
            .unwrap()
            .replacen("\"schema_version\":3", "\"schema_version\":2", 1)
            .into_bytes();
        let request_value = raw_json_object(&legacy_request_body).unwrap();
        let payload_value = raw_json_object(&body).unwrap();
        let legacy_bytes = canonical_frame(
            &TransactionEncodeWire {
                schema_version: 2,
                request: &request_value,
                payload: &payload_value,
            },
            MAX_TRUSTED_GUEST_CONTROL_TRANSACTION_BYTES,
        )
        .unwrap();
        assert_eq!(
            decode_trusted_guest_control_transaction(&legacy_bytes)
                .unwrap_err()
                .kind(),
            TrustedGuestControlTransactionErrorKind::VersionIncompatible
        );
        let legacy =
            decode_legacy_smolrunner_trusted_guest_control_transaction_v2(&legacy_bytes).unwrap();
        assert_eq!(legacy.payload_body(), body.as_slice());
        assert_eq!(legacy.request().request().operation(), request.operation());
        assert_ne!(
            legacy_smolrunner_trusted_guest_control_transaction_v2_digest(&legacy).unwrap(),
            domain_digest(TRANSACTION_DIGEST_DOMAIN, legacy.canonical_bytes()).unwrap()
        );
        assert_ne!(
            legacy_smolrunner_trusted_guest_control_payload_body_v2_digest(
                TrustedGuestControlOperation::ProbeGuestControl,
                &body,
            )
            .unwrap(),
            trusted_guest_control_payload_body_digest(
                TrustedGuestControlOperation::ProbeGuestControl,
                &body,
            )
            .unwrap()
        );
        assert_eq!(
            payload,
            decode_trusted_guest_control_probe_payload_body(legacy.payload_body()).unwrap()
        );
    }

    #[test]
    fn payload_substitution_without_request_digest_change_is_rejected() {
        let (request, _, body) = fixture("probe-1");
        let changed = String::from_utf8(body)
            .unwrap()
            .replace(
                "\"probe_policy_generation\":9",
                "\"probe_policy_generation\":8",
            )
            .into_bytes();
        assert_eq!(
            TrustedGuestControlTransaction::new(request.clone(), changed.clone())
                .unwrap_err()
                .kind(),
            TrustedGuestControlTransactionErrorKind::DigestMismatch
        );

        let transaction =
            TrustedGuestControlTransaction::new(request, fixture("probe-1").2).unwrap();
        let substituted =
            String::from_utf8(encode_trusted_guest_control_transaction(&transaction).unwrap())
                .unwrap()
                .replace(
                    "\"probe_policy_generation\":9",
                    "\"probe_policy_generation\":8",
                );
        assert_eq!(
            decode_trusted_guest_control_transaction(substituted.as_bytes())
                .unwrap_err()
                .kind(),
            TrustedGuestControlTransactionErrorKind::DigestMismatch
        );
    }

    #[test]
    fn outer_canonicality_unknown_fields_second_document_and_bound_fail_closed() {
        let (request, _, body) = fixture("probe-1");
        let transaction = TrustedGuestControlTransaction::new(request, body).unwrap();
        let canonical = encode_trusted_guest_control_transaction(&transaction).unwrap();

        let mut spaced = canonical.clone();
        spaced.insert(0, b' ');
        assert_eq!(
            decode_trusted_guest_control_transaction(&spaced)
                .unwrap_err()
                .kind(),
            TrustedGuestControlTransactionErrorKind::NonCanonical
        );
        let unknown = String::from_utf8(canonical.clone())
            .unwrap()
            .trim_end()
            .strip_suffix('}')
            .unwrap()
            .to_owned()
            + ",\"extra\":true}\n";
        assert_eq!(
            decode_trusted_guest_control_transaction(unknown.as_bytes())
                .unwrap_err()
                .kind(),
            TrustedGuestControlTransactionErrorKind::Malformed
        );
        let mut second = canonical.clone();
        second.extend_from_slice(&canonical);
        assert_eq!(
            decode_trusted_guest_control_transaction(&second)
                .unwrap_err()
                .kind(),
            TrustedGuestControlTransactionErrorKind::Malformed
        );
        assert_eq!(
            decode_trusted_guest_control_transaction(&vec![
                b'x';
                MAX_TRUSTED_GUEST_CONTROL_TRANSACTION_BYTES
                    + 1
            ])
            .unwrap_err()
            .kind(),
            TrustedGuestControlTransactionErrorKind::TooLarge
        );
    }

    #[test]
    fn noncanonical_nested_payload_reaches_only_the_typed_decoder() {
        let (_, _, canonical_body) = fixture("probe-1");
        let mut noncanonical_body = canonical_body;
        noncanonical_body.insert(1, b' ');
        let request = TrustedGuestControlRequest::new(
            TrustedGuestControlRequestId::parse("probe-1").unwrap(),
            binary(),
            authority(),
            TrustedGuestControlOperation::ProbeGuestControl,
            trusted_guest_control_payload_body_digest(
                TrustedGuestControlOperation::ProbeGuestControl,
                &noncanonical_body,
            )
            .unwrap(),
        )
        .unwrap();
        let transaction = TrustedGuestControlTransaction::new(request, noncanonical_body).unwrap();
        let decoded = decode_trusted_guest_control_transaction(
            &encode_trusted_guest_control_transaction(&transaction).unwrap(),
        )
        .unwrap();
        assert_eq!(
            decode_trusted_guest_control_probe_payload_body(decoded.payload_body())
                .unwrap_err()
                .kind(),
            crate::trusted_guest_control_probe_protocol::TrustedGuestControlProbeProtocolErrorKind::NonCanonical
        );
    }

    #[test]
    fn transaction_digest_changes_with_request_or_payload_bytes() {
        let (request, _, body) = fixture("probe-1");
        let first = TrustedGuestControlTransaction::new(request, body).unwrap();
        let (other_request, _, other_body) = fixture("probe-2");
        let second = TrustedGuestControlTransaction::new(other_request, other_body).unwrap();
        assert_ne!(
            trusted_guest_control_transaction_digest(&first).unwrap(),
            trusted_guest_control_transaction_digest(&second).unwrap()
        );
    }

    #[test]
    fn success_receipt_requires_exact_result_and_round_trips() {
        let (request, payload, _) = fixture("probe-1");
        let result = TrustedGuestControlProbeResult::from_verified_dispatch(&payload);
        let result_body = encode_trusted_guest_control_probe_result_body(&result).unwrap();
        let outcome = TrustedGuestControlOutcome::Succeeded {
            result_digest: trusted_guest_control_probe_result_digest(&result).unwrap(),
        };
        let transaction_receipt = TrustedGuestControlTransactionReceipt::new(
            &request,
            &outcome,
            Some(result_body.clone()),
        )
        .unwrap();
        let bytes = encode_trusted_guest_control_transaction_receipt(&transaction_receipt).unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(text.contains("\"result\":{\"schema_version\":1"));
        let decoded = decode_trusted_guest_control_transaction_receipt(&bytes, &request).unwrap();
        assert_eq!(decoded.result_body(), Some(result_body.as_slice()));
        assert_eq!(decoded.receipt().outcome(), &outcome);
        assert!(
            trusted_guest_control_transaction_receipt_digest(&decoded)
                .unwrap()
                .as_str()
                .starts_with("sha256:")
        );
    }

    #[test]
    fn result_presence_digest_and_request_pairing_fail_closed() {
        let (request, payload, _) = fixture("probe-1");
        let result = TrustedGuestControlProbeResult::from_verified_dispatch(&payload);
        let result_body = encode_trusted_guest_control_probe_result_body(&result).unwrap();
        let success = TrustedGuestControlOutcome::Succeeded {
            result_digest: trusted_guest_control_probe_result_digest(&result).unwrap(),
        };
        assert_eq!(
            TrustedGuestControlTransactionReceipt::new(&request, &success, None)
                .unwrap_err()
                .kind(),
            TrustedGuestControlTransactionErrorKind::OutcomeMismatch
        );
        assert_eq!(
            TrustedGuestControlTransactionReceipt::new(
                &request,
                &TrustedGuestControlOutcome::Succeeded {
                    result_digest: digest('f'),
                },
                Some(result_body.clone()),
            )
            .unwrap_err()
            .kind(),
            TrustedGuestControlTransactionErrorKind::DigestMismatch
        );
        assert_eq!(
            TrustedGuestControlTransactionReceipt::new(
                &request,
                &TrustedGuestControlOutcome::Refused(TrustedGuestControlRefusal::InvalidPayload),
                Some(result_body.clone()),
            )
            .unwrap_err()
                .kind(),
            TrustedGuestControlTransactionErrorKind::OutcomeMismatch
        );

        let transaction_receipt =
            TrustedGuestControlTransactionReceipt::new(&request, &success, Some(result_body))
                .unwrap();
        let bytes = encode_trusted_guest_control_transaction_receipt(&transaction_receipt).unwrap();
        let mut second = bytes.clone();
        second.extend_from_slice(&bytes);
        assert_eq!(
            decode_trusted_guest_control_transaction_receipt(&second, &request)
                .unwrap_err()
                .kind(),
            TrustedGuestControlTransactionErrorKind::Malformed
        );
        let mut spaced = bytes.clone();
        spaced.insert(0, b' ');
        assert_eq!(
            decode_trusted_guest_control_transaction_receipt(&spaced, &request)
                .unwrap_err()
                .kind(),
            TrustedGuestControlTransactionErrorKind::NonCanonical
        );
        assert_eq!(
            decode_trusted_guest_control_transaction_receipt(
                &vec![b'x'; MAX_TRUSTED_GUEST_CONTROL_TRANSACTION_RECEIPT_BYTES + 1],
                &request,
            )
            .unwrap_err()
            .kind(),
            TrustedGuestControlTransactionErrorKind::TooLarge
        );
        let (other_request, _, _) = fixture("probe-2");
        assert_eq!(
            decode_trusted_guest_control_transaction_receipt(&bytes, &other_request)
                .unwrap_err()
                .kind(),
            TrustedGuestControlTransactionErrorKind::Protocol
        );

        let refused = TrustedGuestControlTransactionReceipt::new(
            &request,
            &TrustedGuestControlOutcome::Refused(TrustedGuestControlRefusal::InvalidPayload),
            None,
        )
        .unwrap();
        let refused_bytes = encode_trusted_guest_control_transaction_receipt(&refused).unwrap();
        assert!(
            std::str::from_utf8(&refused_bytes)
                .unwrap()
                .contains("\"result\":null")
        );
        decode_trusted_guest_control_transaction_receipt(&refused_bytes, &request).unwrap();
    }

    #[test]
    fn identical_body_bytes_have_distinct_payload_and_result_operation_domains() {
        let body = br#"{"schema_version":1}"#;
        assert_ne!(
            trusted_guest_control_payload_body_digest(
                TrustedGuestControlOperation::ProbeGuestControl,
                body,
            )
            .unwrap(),
            trusted_guest_control_payload_body_digest(
                TrustedGuestControlOperation::ObserveProjectFilesystem,
                body,
            )
            .unwrap()
        );
        assert_ne!(
            trusted_guest_control_payload_body_digest(
                TrustedGuestControlOperation::ProbeGuestControl,
                body,
            )
            .unwrap(),
            trusted_guest_control_result_body_digest(
                TrustedGuestControlOperation::ProbeGuestControl,
                body,
            )
            .unwrap()
        );
    }

    #[test]
    fn maximum_landed_operation_golden_bodies_fit_explicit_process_bounds() {
        use crate::project_disk_lease::{
            ProjectDiskGeneration, ProjectDiskId, ProjectDiskLeaseRecord,
            ProjectDiskLockObservation, ProjectDiskObservation, ProjectDiskPhysicalObservation,
            ProjectDiskRecoverability, ProjectDiskUseObservation,
        };

        let project_text = format!("github.com/{}/{}", "a".repeat(100), "b".repeat(100));
        let project = ProjectIdentity::parse(&project_text).unwrap();
        let disk_text = "c".repeat(96);
        let sandbox_text = "d".repeat(96);
        let disk_id = ProjectDiskId::parse(&disk_text).unwrap();
        let sandbox_id = ResidentSandboxId::parse(&sandbox_text).unwrap();
        let disk_generation = ProjectDiskGeneration::new(u64::MAX).unwrap();
        let sandbox_generation = ResidentSandboxGeneration::new(u64::MAX).unwrap();
        let detached =
            ProjectDiskLeaseRecord::new_detached(project.clone(), disk_id.clone(), disk_generation);
        let attach = detached
            .plan_attach(
                sandbox_id,
                sandbox_generation,
                ProjectDiskObservation::new(
                    ProjectDiskPhysicalObservation::Exact,
                    ProjectDiskUseObservation::Unused,
                    ProjectDiskLockObservation::Unlocked,
                    ProjectDiskRecoverability::Rebuildable,
                ),
            )
            .unwrap();
        let attached = detached
            .record_attach_success(
                &attach,
                ProjectDiskObservation::new(
                    ProjectDiskPhysicalObservation::Exact,
                    ProjectDiskUseObservation::CurrentAttachment,
                    ProjectDiskLockObservation::CurrentAttachment,
                    ProjectDiskRecoverability::Rebuildable,
                ),
            )
            .unwrap();

        let payload_golden = format!(
            "{{\"schema_version\":1,\"project\":\"{project_text}\",\"disk_id\":\"{disk_text}\",\"disk_generation\":{},\"disk_revision\":{},\"attachment_generation\":{},\"sandbox_id\":\"{sandbox_text}\",\"sandbox_generation\":{},\"filesystem_generation\":{},\"format_profile_generation\":{},\"filesystem_kind\":\"xfs\",\"selector\":\"resident_project_root\"}}",
            u64::MAX,
            attached.revision().get(),
            1,
            u64::MAX,
            u64::MAX,
            u64::MAX,
        )
        .into_bytes();
        let payload = decode_trusted_project_filesystem_payload_body(&payload_golden).unwrap();
        let payload_body = encode_trusted_project_filesystem_payload_body(&payload).unwrap();
        assert_eq!(payload_body, payload_golden);
        assert!(payload_body.len() < MAX_TRUSTED_PROJECT_FILESYSTEM_PAYLOAD_BYTES);
        let request = TrustedGuestControlRequest::new(
            TrustedGuestControlRequestId::parse(&"r".repeat(64)).unwrap(),
            TrustedGuestControlBinaryBinding::new(
                u64::MAX,
                digest('e'),
                TrustedGuestControlArchitecture::LinuxAarch64,
            )
            .unwrap(),
            TrustedGuestControlAuthority::from_attached_project_disk(&attached).unwrap(),
            TrustedGuestControlOperation::ObserveProjectFilesystem,
            trusted_project_filesystem_payload_digest(&payload).unwrap(),
        )
        .unwrap();
        let transaction =
            TrustedGuestControlTransaction::new(request.clone(), payload_body).unwrap();
        assert!(
            encode_trusted_guest_control_transaction(&transaction)
                .unwrap()
                .len()
                < MAX_TRUSTED_GUEST_CONTROL_TRANSACTION_BYTES
        );
        assert_eq!(
            MAX_TRUSTED_GUEST_CONTROL_TRANSACTION_BYTES,
            crate::process::MAX_CAPTURED_STDIN_BYTES
        );

        let result_golden = format!(
            "{{\"schema_version\":1,\"project\":\"{project_text}\",\"disk_id\":\"{disk_text}\",\"disk_generation\":{},\"disk_revision\":{},\"attachment_generation\":{},\"sandbox_id\":\"{sandbox_text}\",\"sandbox_generation\":{},\"filesystem_generation\":{},\"format_profile_generation\":{},\"filesystem_kind\":\"xfs\",\"device_mountinfo_bound\":true,\"read_write\":true}}",
            u64::MAX,
            attached.revision().get(),
            1,
            u64::MAX,
            u64::MAX,
            u64::MAX,
        )
        .into_bytes();
        let result = decode_trusted_project_filesystem_result_body(&result_golden).unwrap();
        let result_body = encode_trusted_project_filesystem_result_body(&result).unwrap();
        assert_eq!(result_body, result_golden);
        assert!(result_body.len() < MAX_TRUSTED_PROJECT_FILESYSTEM_RESULT_BYTES);
        let receipt = TrustedGuestControlTransactionReceipt::new(
            &request,
            &TrustedGuestControlOutcome::Succeeded {
                result_digest: trusted_project_filesystem_result_digest(&result).unwrap(),
            },
            Some(result_body),
        )
        .unwrap();
        assert!(
            encode_trusted_guest_control_transaction_receipt(&receipt)
                .unwrap()
                .len()
                < MAX_TRUSTED_GUEST_CONTROL_TRANSACTION_RECEIPT_BYTES
        );
    }
}
