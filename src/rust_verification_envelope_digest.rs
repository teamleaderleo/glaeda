use std::fmt;
use std::io;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::rust_verification_envelope::RustVerificationEnvelope;

pub const RUST_VERIFICATION_ENVELOPE_DIGEST_SCHEMA_VERSION: u8 = 1;
pub const MAX_RUST_VERIFICATION_ENVELOPE_DIGEST_BYTES: usize = 262_144;

const ENVELOPE_DIGEST_DOCUMENT_TYPE: &str = "smolrunner_rust_verification_envelope";
const SHA256_PREFIX: &str = "sha256:";
const HEX: &[u8; 16] = b"0123456789abcdef";

#[derive(Serialize)]
struct RustVerificationEnvelopeDigestDocument<'a> {
    document_type: &'static str,
    schema_version: u8,
    envelope: &'a RustVerificationEnvelope,
}

/// Compute one domain-separated canonical SHA-256 identity for an exact reviewed envelope.
///
/// The complete envelope is encoded only into a bounded in-memory digest writer. The encoded
/// authority document is never returned or included in errors.
///
/// # Errors
///
/// Returns a bounded error when the envelope cannot be encoded, exceeds the fixed digest-document
/// bound, or cannot be represented by the canonical digest type.
pub fn digest_rust_verification_envelope(
    envelope: &RustVerificationEnvelope,
) -> Result<Sha256Digest, RustVerificationEnvelopeDigestError> {
    let document = RustVerificationEnvelopeDigestDocument {
        document_type: ENVELOPE_DIGEST_DOCUMENT_TYPE,
        schema_version: RUST_VERIFICATION_ENVELOPE_DIGEST_SCHEMA_VERSION,
        envelope,
    };
    let mut writer = BoundedDigestWriter::new();
    if serde_json::to_writer(&mut writer, &document).is_err() {
        return Err(if writer.exceeded {
            RustVerificationEnvelopeDigestError::too_large()
        } else {
            RustVerificationEnvelopeDigestError::encoding()
        });
    }
    let digest = writer.finish();
    let mut value = String::with_capacity(SHA256_PREFIX.len() + digest.len() * 2);
    value.push_str(SHA256_PREFIX);
    for byte in digest {
        value.push(HEX[(byte >> 4) as usize] as char);
        value.push(HEX[(byte & 0x0f) as usize] as char);
    }
    Sha256Digest::parse(&value).map_err(|_| RustVerificationEnvelopeDigestError::encoding())
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
            return Err(io::Error::other("envelope digest input exceeds its bound"));
        };
        if next_size > MAX_RUST_VERIFICATION_ENVELOPE_DIGEST_BYTES {
            self.exceeded = true;
            return Err(io::Error::other("envelope digest input exceeds its bound"));
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
pub enum RustVerificationEnvelopeDigestErrorKind {
    Encoding,
    TooLarge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RustVerificationEnvelopeDigestError {
    kind: RustVerificationEnvelopeDigestErrorKind,
    message: &'static str,
}

impl RustVerificationEnvelopeDigestError {
    const fn encoding() -> Self {
        Self {
            kind: RustVerificationEnvelopeDigestErrorKind::Encoding,
            message: "Rust verification envelope could not be canonically digested",
        }
    }

    const fn too_large() -> Self {
        Self {
            kind: RustVerificationEnvelopeDigestErrorKind::TooLarge,
            message: "Rust verification envelope exceeds the bounded digest document",
        }
    }

    #[must_use]
    pub const fn kind(&self) -> RustVerificationEnvelopeDigestErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn message(&self) -> &'static str {
        self.message
    }
}

impl fmt::Display for RustVerificationEnvelopeDigestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for RustVerificationEnvelopeDigestError {}
