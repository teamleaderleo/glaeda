//! Root-only, closed, one-transaction guest-control dispatcher.
//!
//! The production entrypoint admits effective root before reading stdin, accepts exactly one
//! bounded canonical transaction, dispatches only compiled-in reviewed operations, constructs the
//! complete receipt in memory, and enters one bounded stdout write phase. It has no durable replay
//! state and does not read policy, paths, operation selection, or credentials from the environment.

use std::fmt;
#[cfg(any(target_os = "linux", test))]
use std::io::{self, Read, Write};

#[cfg(any(target_os = "linux", test))]
use crate::artifact::Sha256Digest;
#[cfg(any(target_os = "linux", test))]
use crate::trusted_guest_control_probe_protocol::{
    TrustedGuestControlProbePayload, TrustedGuestControlProbeResult,
    decode_trusted_guest_control_probe_payload_body,
    encode_trusted_guest_control_probe_result_body, trusted_guest_control_probe_result_digest,
};
#[cfg(any(target_os = "linux", test))]
use crate::trusted_guest_control_protocol::{
    TRUSTED_GUEST_CONTROL_PROTOCOL_SCHEMA_VERSION, TrustedGuestControlOperation,
    TrustedGuestControlOutcome, TrustedGuestControlRefusal, TrustedGuestControlRequest,
    trusted_guest_control_request_digest,
};
#[cfg(any(target_os = "linux", test))]
use crate::trusted_guest_control_transaction::{
    MAX_TRUSTED_GUEST_CONTROL_TRANSACTION_BYTES, TrustedGuestControlTransaction,
    TrustedGuestControlTransactionReceipt, decode_trusted_guest_control_transaction,
    encode_trusted_guest_control_transaction_receipt,
};

/// Execute the exact internal stdio boundary on Linux.
///
/// Root admission occurs before stdin is read. The function constructs the complete canonical
/// transaction receipt before writing stdout. A stdout error remains transport ambiguity because a
/// failed writer may have accepted a prefix; callers must never treat that prefix as a receipt.
///
/// # Errors
///
/// Returns a bounded error on unsupported platforms, non-root admission, input/framing failure,
/// handler failure, receipt construction failure, or stdout failure.
pub fn serve_trusted_guest_control_stdio() -> Result<(), TrustedGuestControlDispatcherError> {
    #[cfg(target_os = "linux")]
    {
        let effective_uid = rustix::process::geteuid().as_raw();
        let stdin = io::stdin();
        let stdout = io::stdout();
        serve_with_io(effective_uid, stdin.lock(), stdout.lock())
    }

    #[cfg(not(target_os = "linux"))]
    Err(dispatcher_error(
        TrustedGuestControlDispatcherErrorKind::UnsupportedPlatform,
        "trusted_guest_control_dispatcher_unsupported_platform",
        "trusted guest-control dispatcher requires Linux",
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustedGuestControlDispatcherErrorKind {
    UnsupportedPlatform,
    EffectiveRootRequired,
    Input,
    Transaction,
    Handler,
    Receipt,
    Output,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct TrustedGuestControlDispatcherError {
    kind: TrustedGuestControlDispatcherErrorKind,
    code: &'static str,
    message: &'static str,
}

impl TrustedGuestControlDispatcherError {
    #[must_use]
    pub const fn kind(self) -> TrustedGuestControlDispatcherErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for TrustedGuestControlDispatcherError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedGuestControlDispatcherError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for TrustedGuestControlDispatcherError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for TrustedGuestControlDispatcherError {}

#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Clone, Copy)]
struct EffectiveRootAdmission;

/// A decoded request remains claim data. This private context additionally proves that the exact
/// request crossed the effective-root stdio admission boundary. Operation handlers must still
/// validate their typed payload and establish their own fresh local evidence before mutation.
#[cfg(any(target_os = "linux", test))]
struct TrustedGuestControlVerifiedRequestContext<'a> {
    request: &'a TrustedGuestControlRequest,
    request_digest: Sha256Digest,
    protocol_schema_version: u8,
    _root_admission: EffectiveRootAdmission,
}

#[cfg(any(target_os = "linux", test))]
impl<'a> TrustedGuestControlVerifiedRequestContext<'a> {
    fn new(
        request: &'a TrustedGuestControlRequest,
        root_admission: EffectiveRootAdmission,
    ) -> Result<Self, TrustedGuestControlDispatcherError> {
        let request_digest =
            trusted_guest_control_request_digest(request).map_err(|_| transaction_error())?;
        Ok(Self {
            request,
            request_digest,
            protocol_schema_version: TRUSTED_GUEST_CONTROL_PROTOCOL_SCHEMA_VERSION,
            _root_admission: root_admission,
        })
    }

    const fn request(&self) -> &TrustedGuestControlRequest {
        self.request
    }

    const fn request_digest(&self) -> &Sha256Digest {
        &self.request_digest
    }

    const fn protocol_schema_version(&self) -> u8 {
        self.protocol_schema_version
    }
}

#[cfg(any(target_os = "linux", test))]
impl fmt::Debug for TrustedGuestControlVerifiedRequestContext<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedGuestControlVerifiedRequestContext")
            .field("operation", &self.request.operation())
            .field("request_digest", &self.request_digest)
            .field("protocol_schema_version", &self.protocol_schema_version)
            .field("effective_root_admitted", &true)
            .finish_non_exhaustive()
    }
}

#[cfg(any(target_os = "linux", test))]
fn serve_with_io(
    effective_uid: u32,
    reader: impl Read,
    writer: impl Write,
) -> Result<(), TrustedGuestControlDispatcherError> {
    serve_with_io_and_probe_handler(effective_uid, reader, writer, |context, payload| {
        if context.protocol_schema_version() != TRUSTED_GUEST_CONTROL_PROTOCOL_SCHEMA_VERSION
            || context.request_digest()
                != &trusted_guest_control_request_digest(context.request()).ok()?
        {
            return None;
        }
        Some(TrustedGuestControlProbeResult::from_verified_dispatch(
            payload,
        ))
    })
}

#[cfg(any(target_os = "linux", test))]
fn serve_with_io_and_probe_handler<F>(
    effective_uid: u32,
    reader: impl Read,
    mut writer: impl Write,
    probe_handler: F,
) -> Result<(), TrustedGuestControlDispatcherError>
where
    F: FnOnce(
        &TrustedGuestControlVerifiedRequestContext<'_>,
        &TrustedGuestControlProbePayload,
    ) -> Option<TrustedGuestControlProbeResult>,
{
    let root_admission = require_effective_root(effective_uid)?;
    let input = read_one_bounded_transaction(reader)?;
    let receipt = prepare_transaction_receipt(root_admission, &input, probe_handler)?;
    writer.write_all(&receipt).map_err(|_| output_error())?;
    writer.flush().map_err(|_| output_error())
}

#[cfg(any(target_os = "linux", test))]
fn require_effective_root(
    effective_uid: u32,
) -> Result<EffectiveRootAdmission, TrustedGuestControlDispatcherError> {
    if effective_uid != 0 {
        return Err(dispatcher_error(
            TrustedGuestControlDispatcherErrorKind::EffectiveRootRequired,
            "trusted_guest_control_dispatcher_effective_root_required",
            "trusted guest-control dispatcher requires effective root",
        ));
    }
    Ok(EffectiveRootAdmission)
}

#[cfg(any(target_os = "linux", test))]
fn read_one_bounded_transaction(
    reader: impl Read,
) -> Result<Vec<u8>, TrustedGuestControlDispatcherError> {
    let mut bytes = Vec::with_capacity(MAX_TRUSTED_GUEST_CONTROL_TRANSACTION_BYTES + 1);
    reader
        .take((MAX_TRUSTED_GUEST_CONTROL_TRANSACTION_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| input_error())?;
    if bytes.len() > MAX_TRUSTED_GUEST_CONTROL_TRANSACTION_BYTES {
        return Err(input_error());
    }
    Ok(bytes)
}

#[cfg(any(target_os = "linux", test))]
fn prepare_transaction_receipt<F>(
    root_admission: EffectiveRootAdmission,
    input: &[u8],
    probe_handler: F,
) -> Result<Vec<u8>, TrustedGuestControlDispatcherError>
where
    F: FnOnce(
        &TrustedGuestControlVerifiedRequestContext<'_>,
        &TrustedGuestControlProbePayload,
    ) -> Option<TrustedGuestControlProbeResult>,
{
    let transaction =
        decode_trusted_guest_control_transaction(input).map_err(|_| transaction_error())?;
    let context =
        TrustedGuestControlVerifiedRequestContext::new(transaction.request(), root_admission)?;
    let (outcome, result_body) = dispatch_transaction(&context, &transaction, probe_handler)?;
    let receipt =
        TrustedGuestControlTransactionReceipt::new(context.request(), &outcome, result_body)
            .map_err(|_| receipt_error())?;
    encode_trusted_guest_control_transaction_receipt(&receipt).map_err(|_| receipt_error())
}

#[cfg(any(target_os = "linux", test))]
fn dispatch_transaction<F>(
    context: &TrustedGuestControlVerifiedRequestContext<'_>,
    transaction: &TrustedGuestControlTransaction,
    probe_handler: F,
) -> Result<(TrustedGuestControlOutcome, Option<Vec<u8>>), TrustedGuestControlDispatcherError>
where
    F: FnOnce(
        &TrustedGuestControlVerifiedRequestContext<'_>,
        &TrustedGuestControlProbePayload,
    ) -> Option<TrustedGuestControlProbeResult>,
{
    match context.request().operation() {
        TrustedGuestControlOperation::ProbeGuestControl => {
            dispatch_probe(context, transaction, probe_handler)
        }
        TrustedGuestControlOperation::ObservePendingProjectDiskAttachment
        | TrustedGuestControlOperation::ObserveProjectFilesystem
        | TrustedGuestControlOperation::MountProjectFilesystem
        | TrustedGuestControlOperation::ObserveProjectBlockDeviceForFormat
        | TrustedGuestControlOperation::FormatProjectFilesystem
        | TrustedGuestControlOperation::ObserveFormattedProjectFilesystem
        | TrustedGuestControlOperation::ObserveImmutableGitPool
        | TrustedGuestControlOperation::PublishImmutableGitPoolGeneration
        | TrustedGuestControlOperation::PrepareTrustedTaskView
        | TrustedGuestControlOperation::ObserveTrustedTaskView
        | TrustedGuestControlOperation::CleanupTrustedTaskView => Ok((
            TrustedGuestControlOutcome::Refused(TrustedGuestControlRefusal::UnsupportedOperation),
            None,
        )),
    }
}

#[cfg(any(target_os = "linux", test))]
fn dispatch_probe<F>(
    context: &TrustedGuestControlVerifiedRequestContext<'_>,
    transaction: &TrustedGuestControlTransaction,
    probe_handler: F,
) -> Result<(TrustedGuestControlOutcome, Option<Vec<u8>>), TrustedGuestControlDispatcherError>
where
    F: FnOnce(
        &TrustedGuestControlVerifiedRequestContext<'_>,
        &TrustedGuestControlProbePayload,
    ) -> Option<TrustedGuestControlProbeResult>,
{
    let payload = match decode_trusted_guest_control_probe_payload_body(transaction.payload_body())
    {
        Ok(payload) => payload,
        Err(_) => {
            return Ok((
                TrustedGuestControlOutcome::Refused(TrustedGuestControlRefusal::InvalidPayload),
                None,
            ));
        }
    };
    if payload.confirm_common_request(context.request()).is_err() {
        return Ok((
            TrustedGuestControlOutcome::Refused(TrustedGuestControlRefusal::InvalidPayload),
            None,
        ));
    }
    let result = probe_handler(context, &payload).ok_or_else(handler_error)?;
    result
        .confirm_payload(&payload)
        .map_err(|_| handler_error())?;
    let result_body =
        encode_trusted_guest_control_probe_result_body(&result).map_err(|_| handler_error())?;
    let result_digest =
        trusted_guest_control_probe_result_digest(&result).map_err(|_| handler_error())?;
    Ok((
        TrustedGuestControlOutcome::Succeeded { result_digest },
        Some(result_body),
    ))
}

const fn dispatcher_error(
    kind: TrustedGuestControlDispatcherErrorKind,
    code: &'static str,
    message: &'static str,
) -> TrustedGuestControlDispatcherError {
    TrustedGuestControlDispatcherError {
        kind,
        code,
        message,
    }
}

#[cfg(any(target_os = "linux", test))]
const fn input_error() -> TrustedGuestControlDispatcherError {
    dispatcher_error(
        TrustedGuestControlDispatcherErrorKind::Input,
        "trusted_guest_control_dispatcher_input_failed",
        "trusted guest-control dispatcher input is unavailable or oversized",
    )
}

#[cfg(any(target_os = "linux", test))]
const fn transaction_error() -> TrustedGuestControlDispatcherError {
    dispatcher_error(
        TrustedGuestControlDispatcherErrorKind::Transaction,
        "trusted_guest_control_dispatcher_transaction_invalid",
        "trusted guest-control dispatcher transaction is invalid",
    )
}

#[cfg(any(target_os = "linux", test))]
const fn handler_error() -> TrustedGuestControlDispatcherError {
    dispatcher_error(
        TrustedGuestControlDispatcherErrorKind::Handler,
        "trusted_guest_control_dispatcher_handler_failed",
        "trusted guest-control dispatcher handler failed",
    )
}

#[cfg(any(target_os = "linux", test))]
const fn receipt_error() -> TrustedGuestControlDispatcherError {
    dispatcher_error(
        TrustedGuestControlDispatcherErrorKind::Receipt,
        "trusted_guest_control_dispatcher_receipt_failed",
        "trusted guest-control dispatcher receipt cannot be encoded",
    )
}

#[cfg(any(target_os = "linux", test))]
const fn output_error() -> TrustedGuestControlDispatcherError {
    dispatcher_error(
        TrustedGuestControlDispatcherErrorKind::Output,
        "trusted_guest_control_dispatcher_output_failed",
        "trusted guest-control dispatcher receipt cannot be written",
    )
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::artifact::Sha256Digest;
    use crate::project_catalog::ProjectIdentity;
    use crate::project_disk_lease::{
        ProjectDiskAttachmentGeneration, ProjectDiskGeneration, ProjectDiskId, ProjectDiskRevision,
        ResidentSandboxGeneration, ResidentSandboxId,
    };
    use crate::trusted_guest_control_probe_protocol::{
        TrustedGuestControlProbePayload, decode_trusted_guest_control_probe_result_body,
        encode_trusted_guest_control_probe_payload_body,
        trusted_guest_control_probe_payload_digest,
    };
    use crate::trusted_guest_control_protocol::{
        TrustedGuestControlArchitecture, TrustedGuestControlAttachAuthorityClaim,
        TrustedGuestControlAttachTransactionGeneration, TrustedGuestControlAuthority,
        TrustedGuestControlBinaryBinding, TrustedGuestControlRequestId,
        TrustedGuestControlResidentAuthorityGeneration, TrustedGuestControlResidentConfigClaim,
        TrustedGuestControlResidentConfigGeneration, TrustedGuestControlTargetIdentity,
    };
    use crate::trusted_guest_control_transaction::{
        TrustedGuestControlTransaction, decode_trusted_guest_control_transaction_receipt,
        encode_trusted_guest_control_transaction, trusted_guest_control_payload_body_digest,
    };

    fn digest(byte: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    fn fixture() -> (
        TrustedGuestControlRequest,
        TrustedGuestControlProbePayload,
        Vec<u8>,
    ) {
        let authority = TrustedGuestControlAuthority::resident_sandbox(
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            ResidentSandboxId::parse("resident-a").unwrap(),
            ResidentSandboxGeneration::new(3).unwrap(),
            TrustedGuestControlResidentConfigGeneration::new(4).unwrap(),
            TrustedGuestControlResidentConfigClaim::new(digest('a')),
            TrustedGuestControlResidentAuthorityGeneration::new(5).unwrap(),
        );
        let binary = TrustedGuestControlBinaryBinding::new(
            7,
            digest('b'),
            TrustedGuestControlArchitecture::LinuxAarch64,
        )
        .unwrap();
        let payload = TrustedGuestControlProbePayload::new(&authority, &binary, 9).unwrap();
        let body = encode_trusted_guest_control_probe_payload_body(&payload).unwrap();
        let request = TrustedGuestControlRequest::new(
            TrustedGuestControlRequestId::parse("probe-1").unwrap(),
            binary,
            authority,
            TrustedGuestControlOperation::ProbeGuestControl,
            trusted_guest_control_probe_payload_digest(&payload).unwrap(),
        )
        .unwrap();
        let transaction = TrustedGuestControlTransaction::new(request.clone(), body).unwrap();
        (
            request,
            payload,
            encode_trusted_guest_control_transaction(&transaction).unwrap(),
        )
    }

    #[test]
    fn exact_root_probe_calls_one_handler_and_emits_one_bound_result() {
        let (request, payload, input) = fixture();
        let calls = Cell::new(0);
        let root = require_effective_root(0).unwrap();
        let output = prepare_transaction_receipt(root, &input, |context, decoded| {
            calls.set(calls.get() + 1);
            assert_eq!(context.request(), &request);
            assert_eq!(decoded, &payload);
            Some(TrustedGuestControlProbeResult::from_verified_dispatch(
                decoded,
            ))
        })
        .unwrap();
        assert_eq!(calls.get(), 1);
        assert!(output.ends_with(b"\n"));
        let receipt = decode_trusted_guest_control_transaction_receipt(&output, &request).unwrap();
        let result = decode_trusted_guest_control_probe_result_body(
            receipt.result_body().expect("success result body"),
        )
        .unwrap();
        result.confirm_payload(&payload).unwrap();
        assert!(matches!(
            receipt.receipt().outcome(),
            TrustedGuestControlOutcome::Succeeded { .. }
        ));
    }

    struct PanicReader;

    impl Read for PanicReader {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            panic!("non-root admission must precede stdin read");
        }
    }

    #[test]
    fn non_root_refuses_before_input_and_writes_nothing() {
        let mut output = Vec::new();
        let error = serve_with_io(1, PanicReader, &mut output).unwrap_err();
        assert_eq!(
            error.kind(),
            TrustedGuestControlDispatcherErrorKind::EffectiveRootRequired
        );
        assert!(output.is_empty());
    }

    #[test]
    fn malformed_multiple_and_oversized_input_call_no_handler() {
        let (_, _, canonical) = fixture();
        let mut multiple = canonical.clone();
        multiple.extend_from_slice(&canonical);
        let oversized = vec![b'x'; MAX_TRUSTED_GUEST_CONTROL_TRANSACTION_BYTES + 1];
        for input in [b"{}\n".to_vec(), multiple, oversized] {
            let calls = Cell::new(0);
            let result = if input.len() > MAX_TRUSTED_GUEST_CONTROL_TRANSACTION_BYTES {
                let mut output = Vec::new();
                serve_with_io(0, input.as_slice(), &mut output).map(|()| output)
            } else {
                prepare_transaction_receipt(require_effective_root(0).unwrap(), &input, |_, _| {
                    calls.set(calls.get() + 1);
                    None
                })
            };
            assert!(result.is_err());
            assert_eq!(calls.get(), 0);
        }
    }

    #[test]
    fn canonical_outer_frame_with_invalid_typed_probe_payload_is_a_bound_refusal() {
        let (request, _, _) = fixture();
        let body = b"{}".to_vec();
        let request = TrustedGuestControlRequest::new(
            request.request_id().clone(),
            request.binary().clone(),
            request.authority().clone(),
            TrustedGuestControlOperation::ProbeGuestControl,
            trusted_guest_control_payload_body_digest(
                TrustedGuestControlOperation::ProbeGuestControl,
                &body,
            )
            .unwrap(),
        )
        .unwrap();
        let input = encode_trusted_guest_control_transaction(
            &TrustedGuestControlTransaction::new(request.clone(), body).unwrap(),
        )
        .unwrap();
        let calls = Cell::new(0);
        let output =
            prepare_transaction_receipt(require_effective_root(0).unwrap(), &input, |_, _| {
                calls.set(calls.get() + 1);
                None
            })
            .unwrap();
        assert_eq!(calls.get(), 0);
        let receipt = decode_trusted_guest_control_transaction_receipt(&output, &request).unwrap();
        assert_eq!(
            receipt.receipt().outcome(),
            &TrustedGuestControlOutcome::Refused(TrustedGuestControlRefusal::InvalidPayload)
        );
        assert!(receipt.result_body().is_none());
    }

    #[test]
    fn inactive_compiled_in_operation_returns_one_unsupported_receipt_without_handler_call() {
        let project = ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap();
        let sandbox_id = ResidentSandboxId::parse("resident-a").unwrap();
        let sandbox_generation = ResidentSandboxGeneration::new(3).unwrap();
        let authority = TrustedGuestControlAuthority::resident_pending_project_disk_attachment(
            TrustedGuestControlTargetIdentity::resident(project, sandbox_id, sandbox_generation),
            ProjectDiskId::parse("disk-a").unwrap(),
            ProjectDiskGeneration::new(4).unwrap(),
            ProjectDiskRevision::new(5).unwrap(),
            ProjectDiskAttachmentGeneration::new(6).unwrap(),
            TrustedGuestControlAttachTransactionGeneration::new(7).unwrap(),
            TrustedGuestControlAttachAuthorityClaim::new(digest('c')),
        )
        .unwrap();
        let operation = TrustedGuestControlOperation::ObservePendingProjectDiskAttachment;
        let body = b"{}".to_vec();
        let request = TrustedGuestControlRequest::new(
            TrustedGuestControlRequestId::parse("unsupported-1").unwrap(),
            TrustedGuestControlBinaryBinding::new(
                7,
                digest('b'),
                TrustedGuestControlArchitecture::LinuxAarch64,
            )
            .unwrap(),
            authority,
            operation,
            trusted_guest_control_payload_body_digest(operation, &body).unwrap(),
        )
        .unwrap();
        let input = encode_trusted_guest_control_transaction(
            &TrustedGuestControlTransaction::new(request.clone(), body).unwrap(),
        )
        .unwrap();
        let calls = Cell::new(0);
        let output =
            prepare_transaction_receipt(require_effective_root(0).unwrap(), &input, |_, _| {
                calls.set(calls.get() + 1);
                None
            })
            .unwrap();
        assert_eq!(calls.get(), 0);
        let receipt = decode_trusted_guest_control_transaction_receipt(&output, &request).unwrap();
        assert_eq!(
            receipt.receipt().outcome(),
            &TrustedGuestControlOutcome::Refused(TrustedGuestControlRefusal::UnsupportedOperation)
        );
        assert!(receipt.result_body().is_none());
    }

    #[test]
    fn handler_failure_produces_no_partial_protocol_output() {
        let (_, _, input) = fixture();
        let mut output = Vec::new();
        let error = serve_with_io_and_probe_handler(0, input.as_slice(), &mut output, |_, _| None)
            .unwrap_err();
        assert_eq!(
            error.kind(),
            TrustedGuestControlDispatcherErrorKind::Handler
        );
        assert!(output.is_empty());
    }

    #[test]
    fn debug_and_errors_do_not_echo_request_bytes() {
        let (request, _, input) = fixture();
        let context = TrustedGuestControlVerifiedRequestContext::new(
            &request,
            require_effective_root(0).unwrap(),
        )
        .unwrap();
        let debug = format!("{context:?}");
        let input_text = std::str::from_utf8(&input).unwrap();
        assert!(!debug.contains(input_text));
        assert!(!format!("{:?}", transaction_error()).contains(input_text));
    }
}
