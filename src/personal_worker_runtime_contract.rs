//! Pure sealed authority for one reviewed personal-worker runtime closure.
//!
//! This module performs no observation, persistence, process execution, namespace entry, mount, or
//! host mutation. A later Linux observer and journaled installer must earn the opaque evidence
//! bundle whose construction is private to this module before
//! [`seal_personal_worker_runtime_readiness`] can succeed.

use std::fmt;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::state::InstallationId;

pub const PERSONAL_WORKER_RUNTIME_CONTRACT_SCHEMA_VERSION: u8 = 1;

const MAX_RUNTIME_GENERATION: u64 = 1_000_000_000_000;
const RUNTIME_IDENTITY_DOMAIN: &[u8] = b"smolrunner-personal-worker-runtime-readiness-v1";
const RUNTIME_POLICY_DOMAIN: &[u8] = b"smolrunner-personal-worker-runtime-required-policy-v1";
const REDACTED_PRIVATE_RUNTIME_EVIDENCE: &str = "<private-runtime-closure-evidence>";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum PersonalWorkerRuntimePlatform {
    Ubuntu2404 = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum PersonalWorkerRuntimeArchitecture {
    Aarch64 = 1,
    X86_64 = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerRuntimeReadinessDisposition {
    ReadyForClosedVerification,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum RuntimeImagePolicy {
    ExactDigestOfflineOnly = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum RuntimeFilesystemPolicy {
    ReadOnlyRootAndExactSource = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum RuntimeWritableTargetPolicy {
    FreshByteAndInodeBounded = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum RuntimeDependencyCachePolicy {
    VerifiedReadOnlyNonAuthoritative = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum RuntimeTemporaryFilesystemPolicy {
    FreshByteAndInodeBoundedNoExec = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum RuntimeNetworkPolicy {
    Denied = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum RuntimeCredentialPolicy {
    Absent = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum RuntimeEnvironmentPolicy {
    EmptyThenFixedAllowlist = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum RuntimePrivilegePolicy {
    RootlessDropAllAndNoNewPrivileges = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum RuntimeNamespacePolicy {
    PrivatePidIpcUtsCgroup = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum RuntimeExecutionGroupPolicy {
    DedicatedCgroupV2ProveEmpty = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum RuntimeDetachedProcessPolicy {
    Forbidden = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum RuntimeOutputPolicy {
    BoundedCapture = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum RuntimeDeadlinePolicy {
    FixedPlanDeadlineNeverReset = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerRuntimePolicy {
    pub image: RuntimeImagePolicy,
    pub filesystem: RuntimeFilesystemPolicy,
    pub writable_target: RuntimeWritableTargetPolicy,
    pub dependency_cache: RuntimeDependencyCachePolicy,
    pub temporary_filesystems: RuntimeTemporaryFilesystemPolicy,
    pub network: RuntimeNetworkPolicy,
    pub credentials: RuntimeCredentialPolicy,
    pub environment: RuntimeEnvironmentPolicy,
    pub privilege: RuntimePrivilegePolicy,
    pub namespaces: RuntimeNamespacePolicy,
    pub execution_group: RuntimeExecutionGroupPolicy,
    pub detached_processes: RuntimeDetachedProcessPolicy,
    pub output: RuntimeOutputPolicy,
    pub deadline: RuntimeDeadlinePolicy,
}

impl PersonalWorkerRuntimePolicy {
    pub const REQUIRED: Self = Self {
        image: RuntimeImagePolicy::ExactDigestOfflineOnly,
        filesystem: RuntimeFilesystemPolicy::ReadOnlyRootAndExactSource,
        writable_target: RuntimeWritableTargetPolicy::FreshByteAndInodeBounded,
        dependency_cache: RuntimeDependencyCachePolicy::VerifiedReadOnlyNonAuthoritative,
        temporary_filesystems: RuntimeTemporaryFilesystemPolicy::FreshByteAndInodeBoundedNoExec,
        network: RuntimeNetworkPolicy::Denied,
        credentials: RuntimeCredentialPolicy::Absent,
        environment: RuntimeEnvironmentPolicy::EmptyThenFixedAllowlist,
        privilege: RuntimePrivilegePolicy::RootlessDropAllAndNoNewPrivileges,
        namespaces: RuntimeNamespacePolicy::PrivatePidIpcUtsCgroup,
        execution_group: RuntimeExecutionGroupPolicy::DedicatedCgroupV2ProveEmpty,
        detached_processes: RuntimeDetachedProcessPolicy::Forbidden,
        output: RuntimeOutputPolicy::BoundedCapture,
        deadline: RuntimeDeadlinePolicy::FixedPlanDeadlineNeverReset,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerRuntimeReadinessSummary {
    schema_version: u8,
    disposition: PersonalWorkerRuntimeReadinessDisposition,
    platform: PersonalWorkerRuntimePlatform,
    architecture: PersonalWorkerRuntimeArchitecture,
    policy: PersonalWorkerRuntimePolicy,
}

impl PersonalWorkerRuntimeReadinessSummary {
    #[must_use]
    pub const fn schema_version(self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn disposition(self) -> PersonalWorkerRuntimeReadinessDisposition {
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

    #[must_use]
    pub const fn policy(self) -> PersonalWorkerRuntimePolicy {
        self.policy
    }
}

#[derive(Clone, PartialEq, Eq)]
struct PersonalWorkerRuntimeIdentity {
    digest: Sha256Digest,
}

/// One exact, sealed runtime-closure capability.
///
/// This type is intentionally not serializable or deserializable and exposes no path, account,
/// object, generation, package, or digest accessor. Equality compares the complete private
/// canonical identity. The public summary is descriptive only and cannot recreate this authority.
#[derive(Clone, PartialEq, Eq)]
pub struct PersonalWorkerRuntimeReadiness {
    summary: PersonalWorkerRuntimeReadinessSummary,
    identity: PersonalWorkerRuntimeIdentity,
}

impl PersonalWorkerRuntimeReadiness {
    #[must_use]
    pub const fn summary(&self) -> PersonalWorkerRuntimeReadinessSummary {
        self.summary
    }

    #[must_use]
    pub fn has_same_identity(&self, other: &Self) -> bool {
        self.identity == other.identity
    }
}

impl fmt::Debug for PersonalWorkerRuntimeReadiness {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersonalWorkerRuntimeReadiness")
            .field("summary", &self.summary)
            .field(
                "private_runtime_evidence",
                &REDACTED_PRIVATE_RUNTIME_EVIDENCE,
            )
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
enum RuntimeClosureEvidenceClass {
    RunnerAccount = 1,
    PrimaryGroup = 2,
    SubordinateUidRange = 3,
    SubordinateGidRange = 4,
    RuntimeDirectory = 5,
    CgroupDelegation = 6,
    KernelCapabilitySet = 7,
    PodmanExecutableClosure = 8,
    GitExecutableClosure = 9,
    RunuserExecutableClosure = 10,
    EnvExecutableClosure = 11,
    SystemctlExecutableClosure = 12,
    SystemdRunExecutableClosure = 13,
    CrunExecutableClosure = 14,
    ConmonExecutableClosure = 15,
    CatatonitExecutableClosure = 16,
    NewuidmapExecutableClosure = 17,
    NewgidmapExecutableClosure = 18,
    PackagedCapabilitySet = 19,
    ContainersConfig = 20,
    StorageConfig = 21,
    RegistriesConfig = 22,
    MountsConfigAbsence = 23,
    SignaturePolicy = 24,
    SeccompPolicy = 25,
    PreExecHookAbsence = 26,
    OciHookAbsence = 27,
    CdiAbsence = 28,
    NetworkState = 29,
    UserBusAbsence = 30,
    CredentialAndRemoteState = 31,
    EmptyHome = 32,
    EmptyConfigHome = 33,
    ImageStoreBacking = 34,
    ImageStoreMount = 35,
    ImageManifest = 36,
    ImageConfig = 37,
    ImageLayers = 38,
    ImageGate = 39,
    RustToolchain = 40,
}

impl RuntimeClosureEvidenceClass {
    const fn tag(self) -> u8 {
        self as u8
    }
}

const REQUIRED_EVIDENCE_CLASSES: [RuntimeClosureEvidenceClass; 40] = [
    RuntimeClosureEvidenceClass::RunnerAccount,
    RuntimeClosureEvidenceClass::PrimaryGroup,
    RuntimeClosureEvidenceClass::SubordinateUidRange,
    RuntimeClosureEvidenceClass::SubordinateGidRange,
    RuntimeClosureEvidenceClass::RuntimeDirectory,
    RuntimeClosureEvidenceClass::CgroupDelegation,
    RuntimeClosureEvidenceClass::KernelCapabilitySet,
    RuntimeClosureEvidenceClass::PodmanExecutableClosure,
    RuntimeClosureEvidenceClass::GitExecutableClosure,
    RuntimeClosureEvidenceClass::RunuserExecutableClosure,
    RuntimeClosureEvidenceClass::EnvExecutableClosure,
    RuntimeClosureEvidenceClass::SystemctlExecutableClosure,
    RuntimeClosureEvidenceClass::SystemdRunExecutableClosure,
    RuntimeClosureEvidenceClass::CrunExecutableClosure,
    RuntimeClosureEvidenceClass::ConmonExecutableClosure,
    RuntimeClosureEvidenceClass::CatatonitExecutableClosure,
    RuntimeClosureEvidenceClass::NewuidmapExecutableClosure,
    RuntimeClosureEvidenceClass::NewgidmapExecutableClosure,
    RuntimeClosureEvidenceClass::PackagedCapabilitySet,
    RuntimeClosureEvidenceClass::ContainersConfig,
    RuntimeClosureEvidenceClass::StorageConfig,
    RuntimeClosureEvidenceClass::RegistriesConfig,
    RuntimeClosureEvidenceClass::MountsConfigAbsence,
    RuntimeClosureEvidenceClass::SignaturePolicy,
    RuntimeClosureEvidenceClass::SeccompPolicy,
    RuntimeClosureEvidenceClass::PreExecHookAbsence,
    RuntimeClosureEvidenceClass::OciHookAbsence,
    RuntimeClosureEvidenceClass::CdiAbsence,
    RuntimeClosureEvidenceClass::NetworkState,
    RuntimeClosureEvidenceClass::UserBusAbsence,
    RuntimeClosureEvidenceClass::CredentialAndRemoteState,
    RuntimeClosureEvidenceClass::EmptyHome,
    RuntimeClosureEvidenceClass::EmptyConfigHome,
    RuntimeClosureEvidenceClass::ImageStoreBacking,
    RuntimeClosureEvidenceClass::ImageStoreMount,
    RuntimeClosureEvidenceClass::ImageManifest,
    RuntimeClosureEvidenceClass::ImageConfig,
    RuntimeClosureEvidenceClass::ImageLayers,
    RuntimeClosureEvidenceClass::ImageGate,
    RuntimeClosureEvidenceClass::RustToolchain,
];

#[derive(Clone, PartialEq, Eq)]
struct RuntimeClosureEvidence {
    class: RuntimeClosureEvidenceClass,
    digest: Sha256Digest,
}

/// One complete opaque input from the future static observer and journaled installer.
///
/// This bundle has no public constructor, fields, accessors, serialization, Debug representation,
/// or cloning surface. The producer must bind the installation, both durable generations,
/// platform, architecture, and complete exact evidence set before handing it to the sealer; callers
/// cannot substitute one of those axes afterward.
pub struct PersonalWorkerRuntimeEvidenceBundle {
    installation_id: InstallationId,
    runtime_generation: u64,
    image_store_generation: u64,
    platform: PersonalWorkerRuntimePlatform,
    architecture: PersonalWorkerRuntimeArchitecture,
    evidence: Vec<RuntimeClosureEvidence>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerRuntimeContractErrorKind {
    Generation,
    Evidence,
    Identity,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerRuntimeContractError {
    pub kind: PersonalWorkerRuntimeContractErrorKind,
    pub code: &'static str,
    pub message: &'static str,
}

impl PersonalWorkerRuntimeContractError {
    const fn new(
        kind: PersonalWorkerRuntimeContractErrorKind,
        code: &'static str,
        message: &'static str,
    ) -> Self {
        Self {
            kind,
            code,
            message,
        }
    }
}

impl fmt::Debug for PersonalWorkerRuntimeContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersonalWorkerRuntimeContractError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for PersonalWorkerRuntimeContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for PersonalWorkerRuntimeContractError {}

/// Seal exact observer-owned runtime evidence into one equality-only authority object.
///
/// No production evidence producer exists yet. The later R01 observer must construct the complete
/// opaque bundle from exact held/reopened host evidence and journaled durable generations. External
/// callers cannot construct, clone, inspect, or deserialize that bundle.
pub fn seal_personal_worker_runtime_readiness(
    mut bundle: PersonalWorkerRuntimeEvidenceBundle,
) -> Result<PersonalWorkerRuntimeReadiness, PersonalWorkerRuntimeContractError> {
    validate_generation(bundle.runtime_generation)?;
    validate_generation(bundle.image_store_generation)?;
    let policy = PersonalWorkerRuntimePolicy::REQUIRED;

    if bundle.evidence.len() != REQUIRED_EVIDENCE_CLASSES.len() {
        return Err(PersonalWorkerRuntimeContractError::new(
            PersonalWorkerRuntimeContractErrorKind::Evidence,
            "runtime_evidence_incomplete",
            "runtime evidence must contain the complete exact closure once",
        ));
    }
    bundle.evidence.sort_by_key(|item| item.class);
    if bundle
        .evidence
        .iter()
        .zip(REQUIRED_EVIDENCE_CLASSES)
        .any(|(item, required)| item.class != required)
    {
        return Err(PersonalWorkerRuntimeContractError::new(
            PersonalWorkerRuntimeContractErrorKind::Evidence,
            "runtime_evidence_incomplete",
            "runtime evidence must contain the complete exact closure once",
        ));
    }

    let identity = derive_runtime_identity(
        &bundle.installation_id,
        bundle.runtime_generation,
        bundle.image_store_generation,
        bundle.platform,
        bundle.architecture,
        &bundle.evidence,
    )?;
    Ok(PersonalWorkerRuntimeReadiness {
        summary: PersonalWorkerRuntimeReadinessSummary {
            schema_version: PERSONAL_WORKER_RUNTIME_CONTRACT_SCHEMA_VERSION,
            disposition: PersonalWorkerRuntimeReadinessDisposition::ReadyForClosedVerification,
            platform: bundle.platform,
            architecture: bundle.architecture,
            policy,
        },
        identity,
    })
}

fn validate_generation(value: u64) -> Result<(), PersonalWorkerRuntimeContractError> {
    if !(1..=MAX_RUNTIME_GENERATION).contains(&value) {
        return Err(PersonalWorkerRuntimeContractError::new(
            PersonalWorkerRuntimeContractErrorKind::Generation,
            "runtime_generation_invalid",
            "runtime generations must be within the bounded positive range",
        ));
    }
    Ok(())
}

fn derive_runtime_identity(
    installation_id: &InstallationId,
    runtime_generation: u64,
    image_store_generation: u64,
    platform: PersonalWorkerRuntimePlatform,
    architecture: PersonalWorkerRuntimeArchitecture,
    evidence: &[RuntimeClosureEvidence],
) -> Result<PersonalWorkerRuntimeIdentity, PersonalWorkerRuntimeContractError> {
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, RUNTIME_IDENTITY_DOMAIN);
    hash_field(&mut hasher, installation_id.as_str().as_bytes());
    hash_field(&mut hasher, &runtime_generation.to_be_bytes());
    hash_field(&mut hasher, &image_store_generation.to_be_bytes());
    hash_field(&mut hasher, &[platform as u8]);
    hash_field(&mut hasher, &[architecture as u8]);
    hash_field(&mut hasher, RUNTIME_POLICY_DOMAIN);
    hash_field(
        &mut hasher,
        &policy_tags(PersonalWorkerRuntimePolicy::REQUIRED),
    );
    for item in evidence {
        hash_field(&mut hasher, &[item.class.tag()]);
        hash_field(&mut hasher, item.digest.as_str().as_bytes());
    }
    let digest = format!("sha256:{:x}", hasher.finalize());
    let digest = Sha256Digest::parse(&digest).map_err(|_| {
        PersonalWorkerRuntimeContractError::new(
            PersonalWorkerRuntimeContractErrorKind::Identity,
            "runtime_identity_invalid",
            "runtime evidence did not produce one canonical identity",
        )
    })?;
    Ok(PersonalWorkerRuntimeIdentity { digest })
}

fn hash_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

const fn policy_tags(policy: PersonalWorkerRuntimePolicy) -> [u8; 14] {
    [
        policy.image as u8,
        policy.filesystem as u8,
        policy.writable_target as u8,
        policy.dependency_cache as u8,
        policy.temporary_filesystems as u8,
        policy.network as u8,
        policy.credentials as u8,
        policy.environment as u8,
        policy.privilege as u8,
        policy.namespaces as u8,
        policy.execution_group as u8,
        policy.detached_processes as u8,
        policy.output as u8,
        policy.deadline as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRIVATE_SENTINEL: &str = "private-runtime-sentinel";

    fn installation_id() -> InstallationId {
        InstallationId::parse("private-runtime-sentinel").expect("installation id")
    }

    fn evidence() -> Vec<RuntimeClosureEvidence> {
        REQUIRED_EVIDENCE_CLASSES
            .into_iter()
            .enumerate()
            .map(|(index, class)| RuntimeClosureEvidence {
                class,
                digest: Sha256Digest::parse(&format!("sha256:{:064x}", index + 1))
                    .expect("evidence digest"),
            })
            .collect()
    }

    fn bundle(
        installation_id: InstallationId,
        runtime_generation: u64,
        image_store_generation: u64,
        architecture: PersonalWorkerRuntimeArchitecture,
        evidence: Vec<RuntimeClosureEvidence>,
    ) -> PersonalWorkerRuntimeEvidenceBundle {
        PersonalWorkerRuntimeEvidenceBundle {
            installation_id,
            runtime_generation,
            image_store_generation,
            platform: PersonalWorkerRuntimePlatform::Ubuntu2404,
            architecture,
            evidence,
        }
    }

    fn seal(
        architecture: PersonalWorkerRuntimeArchitecture,
        evidence: Vec<RuntimeClosureEvidence>,
    ) -> Result<PersonalWorkerRuntimeReadiness, PersonalWorkerRuntimeContractError> {
        seal_personal_worker_runtime_readiness(bundle(
            installation_id(),
            7,
            11,
            architecture,
            evidence,
        ))
    }

    #[test]
    fn complete_evidence_seals_one_path_free_runtime_identity() {
        let readiness =
            seal(PersonalWorkerRuntimeArchitecture::Aarch64, evidence()).expect("seal readiness");
        let summary = readiness.summary();
        assert_eq!(
            summary.schema_version(),
            PERSONAL_WORKER_RUNTIME_CONTRACT_SCHEMA_VERSION
        );
        assert_eq!(
            summary.disposition(),
            PersonalWorkerRuntimeReadinessDisposition::ReadyForClosedVerification
        );
        assert_eq!(
            summary.platform(),
            PersonalWorkerRuntimePlatform::Ubuntu2404
        );
        assert_eq!(
            summary.architecture(),
            PersonalWorkerRuntimeArchitecture::Aarch64
        );
        assert_eq!(summary.policy(), PersonalWorkerRuntimePolicy::REQUIRED);
        assert_eq!(
            readiness.identity.digest.as_str(),
            "sha256:704796d20b92ead9ca34d674dea1152c67b2f3e2279b403ab46864cfe96b78ea"
        );

        let debug = format!("{readiness:?}");
        let json = serde_json::to_string(&summary).expect("serialize public summary");
        for public in [&debug, &json] {
            assert!(!public.contains(PRIVATE_SENTINEL));
            assert!(!public.contains("sha256:"));
            assert!(!public.contains("uid"));
            assert!(!public.contains("/usr/"));
        }
        assert!(debug.contains(REDACTED_PRIVATE_RUNTIME_EVIDENCE));
    }

    #[test]
    fn evidence_order_is_canonical_and_every_digest_binds_identity() {
        let original = evidence();
        let mut reversed = original.clone();
        reversed.reverse();
        let first =
            seal(PersonalWorkerRuntimeArchitecture::X86_64, original).expect("seal original");
        let reordered =
            seal(PersonalWorkerRuntimeArchitecture::X86_64, reversed).expect("seal reordered");
        assert!(first.has_same_identity(&reordered));

        let mut changed = evidence();
        changed[17].digest =
            Sha256Digest::parse(&format!("sha256:{:064x}", 999)).expect("changed digest");
        let changed =
            seal(PersonalWorkerRuntimeArchitecture::X86_64, changed).expect("seal changed");
        assert!(!first.has_same_identity(&changed));
        assert_eq!(first.summary(), changed.summary());
    }

    #[test]
    fn installation_runtime_image_store_and_architecture_all_bind_identity() {
        let baseline =
            seal(PersonalWorkerRuntimeArchitecture::Aarch64, evidence()).expect("seal baseline");
        let changed_installation = seal_personal_worker_runtime_readiness(bundle(
            InstallationId::parse("different-runtime-sentinel").expect("installation id"),
            7,
            11,
            PersonalWorkerRuntimeArchitecture::Aarch64,
            evidence(),
        ))
        .expect("seal changed installation");
        let changed_runtime = seal_personal_worker_runtime_readiness(bundle(
            installation_id(),
            8,
            11,
            PersonalWorkerRuntimeArchitecture::Aarch64,
            evidence(),
        ))
        .expect("seal changed runtime");
        let changed_image_store = seal_personal_worker_runtime_readiness(bundle(
            installation_id(),
            7,
            12,
            PersonalWorkerRuntimeArchitecture::Aarch64,
            evidence(),
        ))
        .expect("seal changed image store");
        let changed_architecture = seal(PersonalWorkerRuntimeArchitecture::X86_64, evidence())
            .expect("seal changed architecture");

        for changed in [
            changed_installation,
            changed_runtime,
            changed_image_store,
            changed_architecture,
        ] {
            assert!(!baseline.has_same_identity(&changed));
        }
    }

    #[test]
    fn incomplete_duplicate_and_extra_evidence_fail_closed() {
        let mut missing = evidence();
        missing.pop();
        let error = seal(PersonalWorkerRuntimeArchitecture::Aarch64, missing)
            .expect_err("missing evidence");
        assert_eq!(error.kind, PersonalWorkerRuntimeContractErrorKind::Evidence);
        assert_eq!(error.code, "runtime_evidence_incomplete");

        let mut duplicate = evidence();
        duplicate[1].class = duplicate[0].class;
        assert!(seal(PersonalWorkerRuntimeArchitecture::Aarch64, duplicate).is_err());

        let mut extra = evidence();
        extra.push(extra[0].clone());
        assert!(seal(PersonalWorkerRuntimeArchitecture::Aarch64, extra).is_err());
    }

    #[test]
    fn invalid_generations_fail_before_identity_publication() {
        for generation in [0, MAX_RUNTIME_GENERATION + 1] {
            let error = seal_personal_worker_runtime_readiness(bundle(
                installation_id(),
                generation,
                1,
                PersonalWorkerRuntimeArchitecture::Aarch64,
                evidence(),
            ))
            .expect_err("invalid generation");
            assert_eq!(
                error.kind,
                PersonalWorkerRuntimeContractErrorKind::Generation
            );
        }

        let error = seal_personal_worker_runtime_readiness(bundle(
            installation_id(),
            1,
            0,
            PersonalWorkerRuntimeArchitecture::Aarch64,
            evidence(),
        ))
        .expect_err("invalid image-store generation");
        assert_eq!(
            error.kind,
            PersonalWorkerRuntimeContractErrorKind::Generation
        );
    }

    #[test]
    fn module_contains_no_observation_process_persistence_or_mutation_authority() {
        let source = include_str!("personal_worker_runtime_contract.rs");
        for forbidden in [
            ["std", "::fs"].concat(),
            ["std", "::process"].concat(),
            ["Command", "Spec"].concat(),
            ["Command", "Executor"].concat(),
            ["Path", "Buf"].concat(),
            ["System", "Time"].concat(),
            ["mount", "("].concat(),
            ["podman", " "].concat(),
            ["git", " "].concat(),
        ] {
            assert!(
                !source.contains(&forbidden),
                "forbidden authority: {forbidden}"
            );
        }
    }
}
