//! Strict canonical record for one installed personal-worker runtime closure.
//!
//! This document is declaration evidence only. It does not observe the host and cannot construct
//! [`crate::personal_worker_runtime_contract::PersonalWorkerRuntimeReadiness`]. A later Linux
//! observer must independently re-open and match the complete exact closure before readiness can
//! be sealed.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::artifact::Sha256Digest;
use crate::personal_worker_runtime_contract::{
    MAX_RUNTIME_GENERATION, PERSONAL_WORKER_RUNTIME_CONTRACT_SCHEMA_VERSION,
    PersonalWorkerRuntimeArchitecture, PersonalWorkerRuntimePlatform,
};
use crate::state::InstallationId;

pub const PERSONAL_WORKER_RUNTIME_MANIFEST_SCHEMA_VERSION: u8 = 1;
pub const MAX_PERSONAL_WORKER_RUNTIME_MANIFEST_BYTES: usize = 4_096;

const DOCUMENT_TYPE: &str = "smolrunner_personal_worker_runtime_manifest";
const REDACTED_RUNTIME_MANIFEST: &str = "<private-recorded-runtime-closure>";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerRuntimeManifestDisposition {
    RecordedNotObserved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerRuntimeManifestSummary {
    schema_version: u8,
    runtime_contract_schema_version: u8,
    disposition: PersonalWorkerRuntimeManifestDisposition,
    platform: PersonalWorkerRuntimePlatform,
    architecture: PersonalWorkerRuntimeArchitecture,
}

impl PersonalWorkerRuntimeManifestSummary {
    #[must_use]
    pub const fn schema_version(self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn runtime_contract_schema_version(self) -> u8 {
        self.runtime_contract_schema_version
    }

    #[must_use]
    pub const fn disposition(self) -> PersonalWorkerRuntimeManifestDisposition {
        self.disposition
    }

    #[must_use]
    pub const fn platform(self) -> PersonalWorkerRuntimePlatform {
        self.platform
    }

    #[must_use]
    pub const fn architecture(self) -> PersonalWorkerRuntimeArchitecture {
        self.architecture
    }
}

/// One strict persisted declaration of an installed runtime closure.
///
/// The type intentionally has no public constructor and exposes no installation, generation, or
/// digest accessor. Decoding this record does not prove that any current host object still matches
/// it and confers no execution authority.
#[derive(PartialEq, Eq)]
pub struct PersonalWorkerRuntimeManifest {
    schema_version: u8,
    runtime_contract_schema_version: u8,
    installation_id: InstallationId,
    runtime_generation: u64,
    image_store_generation: u64,
    platform: PersonalWorkerRuntimePlatform,
    architecture: PersonalWorkerRuntimeArchitecture,
    runtime_identity_digest: Sha256Digest,
}

impl PersonalWorkerRuntimeManifest {
    #[must_use]
    pub const fn summary(&self) -> PersonalWorkerRuntimeManifestSummary {
        PersonalWorkerRuntimeManifestSummary {
            schema_version: self.schema_version,
            runtime_contract_schema_version: self.runtime_contract_schema_version,
            disposition: PersonalWorkerRuntimeManifestDisposition::RecordedNotObserved,
            platform: self.platform,
            architecture: self.architecture,
        }
    }
}

impl fmt::Debug for PersonalWorkerRuntimeManifest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersonalWorkerRuntimeManifest")
            .field("summary", &self.summary())
            .field("private_runtime_record", &REDACTED_RUNTIME_MANIFEST)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerRuntimeManifestErrorKind {
    InvalidDocument,
    VersionIncompatible,
    CorruptDocument,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerRuntimeManifestError {
    pub kind: PersonalWorkerRuntimeManifestErrorKind,
    pub code: &'static str,
    pub message: &'static str,
}

impl PersonalWorkerRuntimeManifestError {
    const fn new(
        kind: PersonalWorkerRuntimeManifestErrorKind,
        code: &'static str,
        message: &'static str,
    ) -> Self {
        Self {
            kind,
            code,
            message,
        }
    }

    const fn invalid_document() -> Self {
        Self::new(
            PersonalWorkerRuntimeManifestErrorKind::InvalidDocument,
            "runtime_manifest_invalid",
            "personal worker runtime manifest is invalid",
        )
    }

    const fn version_incompatible() -> Self {
        Self::new(
            PersonalWorkerRuntimeManifestErrorKind::VersionIncompatible,
            "runtime_manifest_version_incompatible",
            "personal worker runtime manifest schema is incompatible",
        )
    }

    const fn corrupt_document() -> Self {
        Self::new(
            PersonalWorkerRuntimeManifestErrorKind::CorruptDocument,
            "runtime_manifest_corrupt",
            "personal worker runtime manifest is corrupt or noncanonical",
        )
    }
}

impl fmt::Debug for PersonalWorkerRuntimeManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersonalWorkerRuntimeManifestError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for PersonalWorkerRuntimeManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for PersonalWorkerRuntimeManifestError {}

/// Encode one validated runtime manifest as bounded canonical JSON.
///
/// # Errors
///
/// Returns a bounded error if the in-memory document violates the closed schema or cannot be
/// encoded within the fixed document limit.
pub fn encode_personal_worker_runtime_manifest(
    manifest: &PersonalWorkerRuntimeManifest,
) -> Result<Vec<u8>, PersonalWorkerRuntimeManifestError> {
    validate_manifest(manifest)
        .map_err(|_| PersonalWorkerRuntimeManifestError::invalid_document())?;
    let wire = ManifestWire::from(manifest);
    let mut encoded = serde_json::to_vec_pretty(&wire)
        .map_err(|_| PersonalWorkerRuntimeManifestError::invalid_document())?;
    encoded.push(b'\n');
    if encoded.len() > MAX_PERSONAL_WORKER_RUNTIME_MANIFEST_BYTES {
        return Err(PersonalWorkerRuntimeManifestError::invalid_document());
    }
    Ok(encoded)
}

/// Decode one bounded, strict, canonical runtime manifest without observing or mutating the host.
///
/// # Errors
///
/// Unsupported manifest or runtime-contract schemas are classified separately from malformed,
/// unknown-field, invalid-identity, oversized, and noncanonical documents.
pub fn decode_personal_worker_runtime_manifest(
    bytes: &[u8],
) -> Result<PersonalWorkerRuntimeManifest, PersonalWorkerRuntimeManifestError> {
    if bytes.len() > MAX_PERSONAL_WORKER_RUNTIME_MANIFEST_BYTES {
        return Err(PersonalWorkerRuntimeManifestError::corrupt_document());
    }

    let version: ManifestVersion = serde_json::from_slice(bytes)
        .map_err(|_| PersonalWorkerRuntimeManifestError::corrupt_document())?;
    if version.document_type != DOCUMENT_TYPE {
        return Err(PersonalWorkerRuntimeManifestError::corrupt_document());
    }
    if version.schema_version != PERSONAL_WORKER_RUNTIME_MANIFEST_SCHEMA_VERSION {
        return Err(PersonalWorkerRuntimeManifestError::version_incompatible());
    }
    let contract_version: RuntimeContractVersion = serde_json::from_slice(bytes)
        .map_err(|_| PersonalWorkerRuntimeManifestError::corrupt_document())?;
    if contract_version.runtime_contract_schema_version
        != PERSONAL_WORKER_RUNTIME_CONTRACT_SCHEMA_VERSION
    {
        return Err(PersonalWorkerRuntimeManifestError::version_incompatible());
    }

    let raw: RawManifest = serde_json::from_slice(bytes)
        .map_err(|_| PersonalWorkerRuntimeManifestError::corrupt_document())?;
    let manifest = PersonalWorkerRuntimeManifest::try_from(raw)
        .map_err(|_| PersonalWorkerRuntimeManifestError::corrupt_document())?;
    let canonical = encode_personal_worker_runtime_manifest(&manifest)
        .map_err(|_| PersonalWorkerRuntimeManifestError::corrupt_document())?;
    if canonical != bytes {
        return Err(PersonalWorkerRuntimeManifestError::corrupt_document());
    }
    Ok(manifest)
}

fn validate_manifest(
    manifest: &PersonalWorkerRuntimeManifest,
) -> Result<(), PersonalWorkerRuntimeManifestError> {
    if manifest.schema_version != PERSONAL_WORKER_RUNTIME_MANIFEST_SCHEMA_VERSION
        || manifest.runtime_contract_schema_version
            != PERSONAL_WORKER_RUNTIME_CONTRACT_SCHEMA_VERSION
        || !(1..=MAX_RUNTIME_GENERATION).contains(&manifest.runtime_generation)
        || !(1..=MAX_RUNTIME_GENERATION).contains(&manifest.image_store_generation)
    {
        return Err(PersonalWorkerRuntimeManifestError::invalid_document());
    }
    Ok(())
}

const fn platform_tag(platform: PersonalWorkerRuntimePlatform) -> &'static str {
    match platform {
        PersonalWorkerRuntimePlatform::Ubuntu2404 => "ubuntu2404",
    }
}

const fn architecture_tag(architecture: PersonalWorkerRuntimeArchitecture) -> &'static str {
    match architecture {
        PersonalWorkerRuntimeArchitecture::Aarch64 => "aarch64",
        PersonalWorkerRuntimeArchitecture::X86_64 => "x86_64",
    }
}

fn parse_platform(value: &str) -> Result<PersonalWorkerRuntimePlatform, ()> {
    match value {
        "ubuntu2404" => Ok(PersonalWorkerRuntimePlatform::Ubuntu2404),
        _ => Err(()),
    }
}

fn parse_architecture(value: &str) -> Result<PersonalWorkerRuntimeArchitecture, ()> {
    match value {
        "aarch64" => Ok(PersonalWorkerRuntimeArchitecture::Aarch64),
        "x86_64" => Ok(PersonalWorkerRuntimeArchitecture::X86_64),
        _ => Err(()),
    }
}

#[derive(Deserialize)]
struct ManifestVersion {
    document_type: String,
    schema_version: u8,
}

#[derive(Deserialize)]
struct RuntimeContractVersion {
    runtime_contract_schema_version: u8,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    document_type: String,
    schema_version: u8,
    runtime_contract_schema_version: u8,
    installation_id: String,
    runtime_generation: u64,
    image_store_generation: u64,
    platform: String,
    architecture: String,
    runtime_identity_digest: String,
}

impl TryFrom<RawManifest> for PersonalWorkerRuntimeManifest {
    type Error = ();

    fn try_from(raw: RawManifest) -> Result<Self, Self::Error> {
        if raw.document_type != DOCUMENT_TYPE
            || raw.schema_version != PERSONAL_WORKER_RUNTIME_MANIFEST_SCHEMA_VERSION
            || raw.runtime_contract_schema_version
                != PERSONAL_WORKER_RUNTIME_CONTRACT_SCHEMA_VERSION
            || !(1..=MAX_RUNTIME_GENERATION).contains(&raw.runtime_generation)
            || !(1..=MAX_RUNTIME_GENERATION).contains(&raw.image_store_generation)
        {
            return Err(());
        }
        Ok(Self {
            schema_version: raw.schema_version,
            runtime_contract_schema_version: raw.runtime_contract_schema_version,
            installation_id: InstallationId::parse(&raw.installation_id).map_err(|_| ())?,
            runtime_generation: raw.runtime_generation,
            image_store_generation: raw.image_store_generation,
            platform: parse_platform(&raw.platform)?,
            architecture: parse_architecture(&raw.architecture)?,
            runtime_identity_digest: Sha256Digest::parse(&raw.runtime_identity_digest)
                .map_err(|_| ())?,
        })
    }
}

#[derive(Serialize)]
struct ManifestWire<'a> {
    document_type: &'static str,
    schema_version: u8,
    runtime_contract_schema_version: u8,
    installation_id: &'a str,
    runtime_generation: u64,
    image_store_generation: u64,
    platform: &'static str,
    architecture: &'static str,
    runtime_identity_digest: &'a str,
}

impl<'a> From<&'a PersonalWorkerRuntimeManifest> for ManifestWire<'a> {
    fn from(manifest: &'a PersonalWorkerRuntimeManifest) -> Self {
        Self {
            document_type: DOCUMENT_TYPE,
            schema_version: manifest.schema_version,
            runtime_contract_schema_version: manifest.runtime_contract_schema_version,
            installation_id: manifest.installation_id.as_str(),
            runtime_generation: manifest.runtime_generation,
            image_store_generation: manifest.image_store_generation,
            platform: platform_tag(manifest.platform),
            architecture: architecture_tag(manifest.architecture),
            runtime_identity_digest: manifest.runtime_identity_digest.as_str(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRIVATE_INSTALLATION: &str = "private-runtime-installation";
    const PRIVATE_DIGEST: &str =
        "sha256:1111111111111111111111111111111111111111111111111111111111111111";

    fn manifest() -> PersonalWorkerRuntimeManifest {
        PersonalWorkerRuntimeManifest {
            schema_version: PERSONAL_WORKER_RUNTIME_MANIFEST_SCHEMA_VERSION,
            runtime_contract_schema_version: PERSONAL_WORKER_RUNTIME_CONTRACT_SCHEMA_VERSION,
            installation_id: InstallationId::parse(PRIVATE_INSTALLATION).expect("installation ID"),
            runtime_generation: 7,
            image_store_generation: 11,
            platform: PersonalWorkerRuntimePlatform::Ubuntu2404,
            architecture: PersonalWorkerRuntimeArchitecture::Aarch64,
            runtime_identity_digest: Sha256Digest::parse(PRIVATE_DIGEST)
                .expect("runtime closure digest"),
        }
    }

    fn encoded() -> Vec<u8> {
        encode_personal_worker_runtime_manifest(&manifest()).expect("encode runtime manifest")
    }

    #[test]
    fn canonical_round_trip_preserves_recorded_not_observed_classification() {
        let bytes = encoded();
        let expected = concat!(
            "{\n",
            "  \"document_type\": \"smolrunner_personal_worker_runtime_manifest\",\n",
            "  \"schema_version\": 1,\n",
            "  \"runtime_contract_schema_version\": 1,\n",
            "  \"installation_id\": \"private-runtime-installation\",\n",
            "  \"runtime_generation\": 7,\n",
            "  \"image_store_generation\": 11,\n",
            "  \"platform\": \"ubuntu2404\",\n",
            "  \"architecture\": \"aarch64\",\n",
            "  \"runtime_identity_digest\": ",
            "\"sha256:1111111111111111111111111111111111111111111111111111111111111111\"\n",
            "}\n"
        );
        assert_eq!(bytes, expected.as_bytes());

        let decoded =
            decode_personal_worker_runtime_manifest(&bytes).expect("decode runtime manifest");
        assert_eq!(
            encode_personal_worker_runtime_manifest(&decoded).unwrap(),
            bytes
        );
        let summary = decoded.summary();
        assert_eq!(
            summary.disposition(),
            PersonalWorkerRuntimeManifestDisposition::RecordedNotObserved
        );
        assert_eq!(
            summary.platform(),
            PersonalWorkerRuntimePlatform::Ubuntu2404
        );
        assert_eq!(
            summary.architecture(),
            PersonalWorkerRuntimeArchitecture::Aarch64
        );
    }

    #[test]
    fn public_summary_and_debug_do_not_disclose_private_record_evidence() {
        let manifest = manifest();
        let debug = format!("{manifest:?}");
        let summary = serde_json::to_string(&manifest.summary()).expect("serialize summary");
        for public in [&debug, &summary] {
            assert!(!public.contains(PRIVATE_INSTALLATION));
            assert!(!public.contains(PRIVATE_DIGEST));
            assert!(!public.contains("runtime_generation"));
            assert!(!public.contains("image_store_generation"));
        }
        assert!(debug.contains(REDACTED_RUNTIME_MANIFEST));
        assert!(summary.contains("recorded_not_observed"));
    }

    #[test]
    fn unsupported_manifest_or_contract_schema_is_version_incompatible() {
        for field in ["schema_version", "runtime_contract_schema_version"] {
            let original = format!("\"{field}\": 1");
            let changed = format!("\"{field}\": 2");
            let bytes = String::from_utf8(encoded())
                .expect("UTF-8 fixture")
                .replace(&original, &changed);
            let error = decode_personal_worker_runtime_manifest(bytes.as_bytes())
                .expect_err("unsupported version");
            assert_eq!(
                error.kind,
                PersonalWorkerRuntimeManifestErrorKind::VersionIncompatible
            );
        }

        let future_minimal = br#"{
  "document_type": "smolrunner_personal_worker_runtime_manifest",
  "schema_version": 2
}
"#;
        let error = decode_personal_worker_runtime_manifest(future_minimal)
            .expect_err("future schema does not require current-version fields");
        assert_eq!(
            error.kind,
            PersonalWorkerRuntimeManifestErrorKind::VersionIncompatible
        );
    }

    #[test]
    fn malformed_unknown_and_noncanonical_documents_fail_closed() {
        let canonical = String::from_utf8(encoded()).expect("UTF-8 fixture");
        let replacements = [
            (
                "smolrunner_personal_worker_runtime_manifest",
                "smolrunner_personal_worker_runtime_other",
            ),
            (PRIVATE_INSTALLATION, "short"),
            (PRIVATE_DIGEST, "sha256:ABC"),
            ("\"runtime_generation\": 7", "\"runtime_generation\": 0"),
            (
                "\"image_store_generation\": 11",
                "\"image_store_generation\": 0",
            ),
            ("ubuntu2404", "ubuntu2604"),
            ("aarch64", "riscv64"),
        ];
        for (original, replacement) in replacements {
            let bytes = canonical.replace(original, replacement);
            let error = decode_personal_worker_runtime_manifest(bytes.as_bytes())
                .expect_err("malformed document");
            assert_eq!(
                error.kind,
                PersonalWorkerRuntimeManifestErrorKind::CorruptDocument
            );
        }

        let unknown = canonical.replace(
            "  \"schema_version\": 1,",
            "  \"schema_version\": 1,\n  \"unknown\": true,",
        );
        assert!(decode_personal_worker_runtime_manifest(unknown.as_bytes()).is_err());

        let compact: serde_json::Value = serde_json::from_str(&canonical).expect("JSON fixture");
        let compact = serde_json::to_vec(&compact).expect("compact fixture");
        assert!(decode_personal_worker_runtime_manifest(&compact).is_err());

        let oversized = vec![b' '; MAX_PERSONAL_WORKER_RUNTIME_MANIFEST_BYTES + 1];
        assert!(decode_personal_worker_runtime_manifest(&oversized).is_err());
    }

    #[test]
    fn in_memory_generation_bounds_are_shared_with_the_sealed_contract() {
        for generation in [0, MAX_RUNTIME_GENERATION + 1] {
            let mut invalid = manifest();
            invalid.runtime_generation = generation;
            let error = encode_personal_worker_runtime_manifest(&invalid)
                .expect_err("invalid runtime generation");
            assert_eq!(
                error.kind,
                PersonalWorkerRuntimeManifestErrorKind::InvalidDocument
            );
        }

        let mut invalid = manifest();
        invalid.image_store_generation = 0;
        assert!(encode_personal_worker_runtime_manifest(&invalid).is_err());
    }

    #[test]
    fn manifest_has_no_observation_persistence_process_or_readiness_authority() {
        let source = include_str!("personal_worker_runtime_manifest.rs");
        for forbidden in [
            ["std", "::fs"].concat(),
            ["std", "::process"].concat(),
            ["Command", "Executor"].concat(),
            ["System", "Time"].concat(),
            ["PersonalWorkerRuntime", "EvidenceBundle"].concat(),
            ["seal_personal_worker_", "runtime_readiness"].concat(),
            ["mount", "("].concat(),
        ] {
            assert!(
                !source.contains(&forbidden),
                "forbidden authority: {forbidden}"
            );
        }
    }
}
