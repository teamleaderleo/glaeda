//! Pure repository-verification adapter into the generic compute workload contract.
//!
//! Exact immutable source, verification-profile, and repository-command identities are hashed into
//! the family-owned opaque input identity. The generic compute layer receives none of the command,
//! path, workspace, runner, or environment surfaces behind those identities.

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::compute_workload::{
    ComputeCapabilitySet, ComputeInputIdentity, ComputeOutputContractIdentity,
    ComputeSemanticGeneration, ComputeTrustClass, ComputeWorkloadFamilyId, ComputeWorkloadIdentity,
};
use crate::verification_profile::{
    ImmutableSourceInputs, RepositoryCommandId, VerificationProfileId,
};

pub const REPOSITORY_VERIFICATION_WORKLOAD_FAMILY_ID: &str = "repository_verification.v1";
pub const REPOSITORY_VERIFICATION_COMPUTE_SEMANTIC_GENERATION: u64 = 1;

const INPUT_IDENTITY_DOCUMENT_TYPE: &str = "repository_verification_compute_input_v1";
const SHA256_PREFIX: &str = "sha256:";
const HEX: &[u8; 16] = b"0123456789abcdef";

#[derive(Serialize)]
struct RepositoryVerificationComputeInputDocument<'a> {
    document_type: &'static str,
    schema_version: u8,
    source: &'a ImmutableSourceInputs,
    verification_profile_id: &'a VerificationProfileId,
    repository_command_id: &'a RepositoryCommandId,
}

/// Derive one exact family-owned input identity without exposing executable or private surfaces.
#[must_use]
pub fn repository_verification_compute_input_identity(
    source: &ImmutableSourceInputs,
    verification_profile_id: &VerificationProfileId,
    repository_command_id: &RepositoryCommandId,
) -> ComputeInputIdentity {
    let document = RepositoryVerificationComputeInputDocument {
        document_type: INPUT_IDENTITY_DOCUMENT_TYPE,
        schema_version: 1,
        source,
        verification_profile_id,
        repository_command_id,
    };
    let bytes = serde_json::to_vec(&document)
        .expect("typed repository verification compute input must serialize");
    ComputeInputIdentity::new(sha256_digest(&bytes))
}

/// Build the generic semantic workload while leaving resource and execution intent outside it.
#[must_use]
pub fn repository_verification_compute_workload_identity(
    source: &ImmutableSourceInputs,
    verification_profile_id: &VerificationProfileId,
    repository_command_id: &RepositoryCommandId,
    trust_class: ComputeTrustClass,
    required_capabilities: ComputeCapabilitySet,
    output_contract: ComputeOutputContractIdentity,
) -> ComputeWorkloadIdentity {
    ComputeWorkloadIdentity::new(
        ComputeWorkloadFamilyId::parse(REPOSITORY_VERIFICATION_WORKLOAD_FAMILY_ID)
            .expect("fixed repository verification family must validate"),
        ComputeSemanticGeneration::new(REPOSITORY_VERIFICATION_COMPUTE_SEMANTIC_GENERATION)
            .expect("fixed repository verification semantic generation must validate"),
        repository_verification_compute_input_identity(
            source,
            verification_profile_id,
            repository_command_id,
        ),
        trust_class,
        required_capabilities,
        output_contract,
    )
}

fn sha256_digest(bytes: &[u8]) -> Sha256Digest {
    let digest = Sha256::digest(bytes);
    let mut value = String::with_capacity(SHA256_PREFIX.len() + digest.len() * 2);
    value.push_str(SHA256_PREFIX);
    for byte in digest {
        value.push(HEX[(byte >> 4) as usize] as char);
        value.push(HEX[(byte & 0x0f) as usize] as char);
    }
    Sha256Digest::parse(&value).expect("SHA-256 encoder must produce a canonical digest")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{CommitId, GitTreeId, RepositoryRef};
    use crate::compute_execution_request::ComputeExecutionRequest;
    use crate::compute_workload::{ComputeCapabilityId, ComputeOutputContractIdentity};
    use crate::execution_admission::ExecutionRequestId;
    use crate::execution_capacity::{CapacityAmounts, CapacityDimension};
    use crate::verification_profile::{
        ImmutableRefInput, RepositoryRefName, SourceComposition, TestedSourceIdentity,
    };

    fn digest(hex: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", hex.to_string().repeat(64))).unwrap()
    }

    fn source(commit_hex: char, tree_hex: char) -> ImmutableSourceInputs {
        let commit = CommitId::parse(&commit_hex.to_string().repeat(40)).unwrap();
        ImmutableSourceInputs::new(
            RepositoryRef::parse("example/project").unwrap(),
            vec![ImmutableRefInput::new(
                RepositoryRefName::parse("refs/heads/main").unwrap(),
                commit.clone(),
            )],
            SourceComposition::SingleRef,
            TestedSourceIdentity::Commit {
                commit,
                tree: GitTreeId::parse(&tree_hex.to_string().repeat(40)).unwrap(),
            },
        )
        .unwrap()
    }

    fn capabilities() -> ComputeCapabilitySet {
        ComputeCapabilitySet::new(vec![
            ComputeCapabilityId::parse("linux.arm64").unwrap(),
            ComputeCapabilityId::parse("toolchain.rust").unwrap(),
        ])
        .unwrap()
    }

    #[test]
    fn source_profile_and_command_each_bind_the_opaque_input_identity() {
        let source_a = source('a', 'b');
        let source_b = source('c', 'd');
        let profile_a = VerificationProfileId::parse("glaeda-rust-v1").unwrap();
        let profile_b = VerificationProfileId::parse("glaeda-rust-v2").unwrap();
        let command_a = RepositoryCommandId::parse("glaeda-rust-test-v1").unwrap();
        let command_b = RepositoryCommandId::parse("glaeda-rust-test-v2").unwrap();

        let base = repository_verification_compute_input_identity(
            &source_a,
            &profile_a,
            &command_a,
        );
        assert_ne!(
            base,
            repository_verification_compute_input_identity(&source_b, &profile_a, &command_a)
        );
        assert_ne!(
            base,
            repository_verification_compute_input_identity(&source_a, &profile_b, &command_a)
        );
        assert_ne!(
            base,
            repository_verification_compute_input_identity(&source_a, &profile_a, &command_b)
        );
    }

    #[test]
    fn verification_adapter_preserves_exact_workload_in_generic_request() {
        let source = source('a', 'b');
        let workload = repository_verification_compute_workload_identity(
            &source,
            &VerificationProfileId::parse("glaeda-rust-v1").unwrap(),
            &RepositoryCommandId::parse("glaeda-rust-test-v1").unwrap(),
            ComputeTrustClass::UltraTrusted,
            capabilities(),
            ComputeOutputContractIdentity::new(digest('e')),
        );
        let request = ComputeExecutionRequest::new(
            ExecutionRequestId::parse("verification-request1").unwrap(),
            workload.clone(),
            CapacityAmounts::new(&[
                (CapacityDimension::CpuMillis, 4_000),
                (CapacityDimension::MemoryBytes, 8 * 1024 * 1024 * 1024),
                (CapacityDimension::DiskBytes, 16 * 1024 * 1024 * 1024),
                (CapacityDimension::Pids, 256),
            ])
            .unwrap(),
        );

        assert_eq!(request.workload(), &workload);
        assert_eq!(
            request.workload().family().as_str(),
            REPOSITORY_VERIFICATION_WORKLOAD_FAMILY_ID
        );
        let json = serde_json::to_string(&request).unwrap();
        for private_or_executable in [
            "example/project",
            "refs/heads/main",
            "glaeda-rust-test-v1",
            "runner_profile",
            "/private/",
            "cargo test",
        ] {
            assert!(!json.contains(private_or_executable));
        }
    }
}
