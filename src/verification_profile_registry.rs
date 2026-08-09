use std::collections::BTreeSet;
use std::fmt;

use serde::Serialize;

use crate::artifact::{ArtifactIdentityError, RepositoryRef, Sha256Digest};
use crate::execution_admission::ExecutionResourceLimits;
use crate::lima_lifecycle::LimaResourceProfile;
use crate::rust_verification_envelope::{
    CargoTargetDirectoryIdentity, RustBuildScriptInclusion, RustCacheContract,
    RustCacheIdentityClass, RustCargoProfileKind, RustCompilationContract, RustConcurrencyPlan,
    RustFeatureSelection, RustRetryPolicy, RustRetryPolicyId, RustRuntimeConcurrency,
    RustTargetDirectoryId, RustTargetPolicy, RustTargetTriple, RustToolchainId,
    RustToolchainIdentity, RustVerificationEnvelope, RustVerificationEnvelopeDefinition,
    RustVerificationEnvelopeError, RustVerificationScope, RustVerificationSourceIdentity,
};
use crate::verification_profile::{
    ApprovedEquivalentCommand, CacheId, CapabilityId, ConcurrencyPolicy, DeclaredDeviation,
    DeviationCode, DirtyWorkspacePolicy, ExactBuildScope, ExactVerificationScope,
    LocalCommitAuthority, MAX_CAPABILITIES, MemoryPolicy, OptionalCapability, PackageId,
    PublicationAuthority, RepositoryCommandContract, RepositoryCommandId,
    RepositoryCommandIdentity, RequiredCapability, ResourceDefaults, TimeoutPolicy,
    VerificationAuthorityPolicy, VerificationProfileError, VerificationProfileId,
    WorkspaceMutationAuthority, WorkspaceMutationPolicy,
};

pub const SMOLRUNNER_REQUIRED_PROFILE_ID: &str = "smolrunner.required";
pub const SMOLRUNNER_DOCTOR_PROFILE_ID: &str = "smolrunner.doctor";
pub const SMOLRUNNER_PLAN_PROFILE_ID: &str = "smolrunner.plan";

const REPOSITORY: &str = "teamleaderleo/smolrunner";
const PACKAGE: &str = "smolrunner";
const REQUIRED_COMMAND_ID: &str = "smolrunner.required.v1";
const DOCTOR_COMMAND_ID: &str = "smolrunner.doctor.v1";
const PLAN_COMMAND_ID: &str = "smolrunner.plan.v1";
const REQUIRED_COMMAND_DIGEST: &str =
    "sha256:fab0c53ffcb5bf63764155bc1e9dc85371cf2240190ab9cd36ad412cace62dc5";
const DOCTOR_COMMAND_DIGEST: &str =
    "sha256:46d9f7be1e888b842fe77e81e3826d6338e637901022d7acc9d18fb61b8ffe6e";
const PLAN_COMMAND_DIGEST: &str =
    "sha256:cf9866af6335cd4d3a579dc2f61202cdd3652eb25031330062848251a6e8d0d1";
const CACHE_ID: &str = "cargo-target";
const RUST_TOOLCHAIN_ID: &str = "rust-1.97.1-minimal-clippy-rustfmt";
const RUST_TOOLCHAIN_CONTRACT_DIGEST: &str =
    "sha256:279d77167cec5426fa80f457cd066dc74a360fbe4e2816f4f3fa01487a918fdc";
const RUST_HOST_TRIPLE: &str = "aarch64-unknown-linux-gnu";
const RUST_TARGET_TRIPLE: &str = "aarch64-unknown-linux-gnu";
const RUST_WORKSPACE_MEMBERS_DIGEST: &str =
    "sha256:7c4f356a716b2b4cc10680a9a121a56860141340ad966b311b5b419fb01fa272";
const PROFILE_IDS: [&str; 3] = [
    SMOLRUNNER_REQUIRED_PROFILE_ID,
    SMOLRUNNER_DOCTOR_PROFILE_ID,
    SMOLRUNNER_PLAN_PROFILE_ID,
];
const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationCacheClassKind {
    RunnerOwnedCargoTarget,
}

/// A path-free cache policy class.
///
/// A trusted workspace producer binds the exact installation-specific namespace separately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerificationCacheIdentityClass {
    kind: VerificationCacheClassKind,
    cache_id: CacheId,
}

impl VerificationCacheIdentityClass {
    #[must_use]
    pub const fn kind(&self) -> VerificationCacheClassKind {
        self.kind
    }

    #[must_use]
    pub const fn cache_id(&self) -> &CacheId {
        &self.cache_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegisteredVerificationProfile {
    profile_id: VerificationProfileId,
    canonical_command: RepositoryCommandContract,
    approved_equivalents: Vec<ApprovedEquivalentCommand>,
    required_capabilities: Vec<RequiredCapability>,
    optional_capabilities: Vec<OptionalCapability>,
    resources: ResourceDefaults,
    cache_class: VerificationCacheIdentityClass,
    timeout: TimeoutPolicy,
    authority: VerificationAuthorityPolicy,
}

struct ProfileDefinition {
    profile_id: VerificationProfileId,
    canonical_command: RepositoryCommandContract,
    approved_equivalents: Vec<ApprovedEquivalentCommand>,
    required_capabilities: Vec<RequiredCapability>,
    optional_capabilities: Vec<OptionalCapability>,
    resources: ResourceDefaults,
    cache_class: VerificationCacheIdentityClass,
    timeout: TimeoutPolicy,
    authority: VerificationAuthorityPolicy,
}

impl RegisteredVerificationProfile {
    fn new(definition: ProfileDefinition) -> Result<Self, VerificationProfileRegistryError> {
        validate_capabilities(
            &definition.required_capabilities,
            &definition.optional_capabilities,
            definition.canonical_command.required_capabilities(),
        )?;
        if !definition.approved_equivalents.is_empty() {
            return Err(VerificationProfileRegistryError::new(
                "registry.approved_equivalents",
                "undeclared_fallback",
                "the checked-in SmolRunner profiles declare no command fallbacks",
            ));
        }
        validate_read_only(&definition.authority)?;
        if definition.canonical_command.identity().repository()
            != &RepositoryRef::parse(REPOSITORY)?
        {
            return Err(VerificationProfileRegistryError::new(
                "registry.command.repository",
                "repository_identity_mismatch",
                "registered commands must belong to teamleaderleo/smolrunner",
            ));
        }
        Ok(Self {
            profile_id: definition.profile_id,
            canonical_command: definition.canonical_command,
            approved_equivalents: definition.approved_equivalents,
            required_capabilities: definition.required_capabilities,
            optional_capabilities: definition.optional_capabilities,
            resources: definition.resources,
            cache_class: definition.cache_class,
            timeout: definition.timeout,
            authority: definition.authority,
        })
    }

    #[must_use]
    pub const fn profile_id(&self) -> &VerificationProfileId {
        &self.profile_id
    }

    #[must_use]
    pub const fn canonical_command(&self) -> &RepositoryCommandContract {
        &self.canonical_command
    }

    #[must_use]
    pub fn approved_equivalents(&self) -> &[ApprovedEquivalentCommand] {
        &self.approved_equivalents
    }

    #[must_use]
    pub fn required_capabilities(&self) -> &[RequiredCapability] {
        &self.required_capabilities
    }

    #[must_use]
    pub fn optional_capabilities(&self) -> &[OptionalCapability] {
        &self.optional_capabilities
    }

    #[must_use]
    pub const fn resources(&self) -> ResourceDefaults {
        self.resources
    }

    #[must_use]
    pub const fn cache_class(&self) -> &VerificationCacheIdentityClass {
        &self.cache_class
    }

    #[must_use]
    pub const fn timeout(&self) -> &TimeoutPolicy {
        &self.timeout
    }

    #[must_use]
    pub const fn authority(&self) -> &VerificationAuthorityPolicy {
        &self.authority
    }

    /// Select the sole checked-in repository command for this profile.
    ///
    /// # Errors
    ///
    /// Returns `undeclared_fallback` for any other identity.
    pub fn select_command(
        &self,
        identity: &RepositoryCommandIdentity,
    ) -> Result<&RepositoryCommandContract, VerificationProfileRegistryError> {
        if identity == self.canonical_command.identity() {
            Ok(&self.canonical_command)
        } else {
            Err(VerificationProfileRegistryError::new(
                "registry.command",
                "undeclared_fallback",
                "selected command is not the profile's canonical command",
            ))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerificationProfileRegistry {
    profiles: Vec<RegisteredVerificationProfile>,
}

impl VerificationProfileRegistry {
    fn new(
        profiles: Vec<RegisteredVerificationProfile>,
    ) -> Result<Self, VerificationProfileRegistryError> {
        let mut seen = BTreeSet::new();
        if profiles
            .iter()
            .any(|profile| !seen.insert(profile.profile_id().clone()))
        {
            return Err(VerificationProfileRegistryError::new(
                "registry.profiles",
                "duplicate_profile_id",
                "registry profile IDs must be unique",
            ));
        }
        if profiles.len() != PROFILE_IDS.len()
            || profiles
                .iter()
                .zip(PROFILE_IDS)
                .any(|(profile, expected)| profile.profile_id().as_str() != expected)
        {
            return Err(VerificationProfileRegistryError::new(
                "registry.profiles",
                "profile_alias_or_order_mismatch",
                "registry must contain the three canonical IDs in stable order",
            ));
        }
        Ok(Self { profiles })
    }

    #[must_use]
    pub fn profiles(&self) -> &[RegisteredVerificationProfile] {
        &self.profiles
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.profiles.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty()
    }

    /// Look up one already-validated profile ID.
    ///
    /// # Errors
    ///
    /// Returns `unknown_profile` when the ID is not registered.
    pub fn lookup(
        &self,
        profile_id: &VerificationProfileId,
    ) -> Result<&RegisteredVerificationProfile, VerificationProfileRegistryError> {
        self.profiles
            .iter()
            .find(|profile| profile.profile_id() == profile_id)
            .ok_or_else(|| {
                VerificationProfileRegistryError::new(
                    "registry.profile_id",
                    "unknown_profile",
                    "profile ID is not present in the checked-in registry",
                )
            })
    }

    /// Resolve one checked-in profile into its exact source-bound Rust execution envelope.
    ///
    /// The caller supplies only immutable source identity and the already-derived source/command
    /// target namespace. Toolchain, scope, features, target categories, resources, concurrency,
    /// capabilities, duration, target-directory class, and retry authority remain registry-owned.
    ///
    /// # Errors
    ///
    /// Returns a bounded registry error when the profile is unknown or a checked-in Rust envelope
    /// declaration no longer satisfies the accepted contract.
    pub fn resolve_rust_envelope(
        &self,
        profile_id: &VerificationProfileId,
        source: RustVerificationSourceIdentity,
        source_command_namespace: Sha256Digest,
    ) -> Result<RustVerificationEnvelope, VerificationProfileRegistryError> {
        rust_envelope(self.lookup(profile_id)?, source, source_command_namespace)
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let ids = self
            .profiles
            .iter()
            .map(|profile| profile.profile_id().as_str())
            .collect::<Vec<_>>()
            .join(",");
        format!("profiles={} ids={ids}", self.profiles.len())
    }
}

/// Construct the exact checked-in SmolRunner verification profile registry.
///
/// # Errors
///
/// Returns an error if any checked-in identifier, digest, scope, capability, resource, cache,
/// timeout, fallback, or authority no longer satisfies the merged v1 contract.
pub fn smolrunner_profile_registry()
-> Result<VerificationProfileRegistry, VerificationProfileRegistryError> {
    VerificationProfileRegistry::new(vec![
        required_profile()?,
        doctor_profile()?,
        plan_profile()?,
    ])
}

fn required_profile() -> Result<RegisteredVerificationProfile, VerificationProfileRegistryError> {
    let required = capabilities(&["cargo", "rustc", "rustfmt", "clippy"])?;
    profile(ProfileDefinition {
        profile_id: VerificationProfileId::parse(SMOLRUNNER_REQUIRED_PROFILE_ID)?,
        canonical_command: command(
            REQUIRED_COMMAND_ID,
            REQUIRED_COMMAND_DIGEST,
            ExactVerificationScope::WholeWorkspaceTests,
            ExactBuildScope::WholeWorkspace,
            required.clone(),
        )?,
        approved_equivalents: Vec::new(),
        required_capabilities: required.into_iter().map(RequiredCapability::new).collect(),
        optional_capabilities: vec![
            optional("podman", "podman-unavailable", "Podman is unavailable")?,
            optional("systemd", "systemd-unavailable", "systemd is unavailable")?,
        ],
        resources: resources(4 * GIB, 4 * GIB, 2, 1, 2)?,
        cache_class: cache_class()?,
        timeout: TimeoutPolicy::new(3_600, Vec::new())?,
        authority: read_only_authority()?,
    })
}

fn doctor_profile() -> Result<RegisteredVerificationProfile, VerificationProfileRegistryError> {
    package_profile(
        SMOLRUNNER_DOCTOR_PROFILE_ID,
        DOCTOR_COMMAND_ID,
        DOCTOR_COMMAND_DIGEST,
        vec![
            optional("podman", "podman-unavailable", "Podman is unavailable")?,
            optional("systemd", "systemd-unavailable", "systemd is unavailable")?,
        ],
        resources(512 * MIB, 512 * MIB, 1, 1, 1)?,
        300,
    )
}

fn plan_profile() -> Result<RegisteredVerificationProfile, VerificationProfileRegistryError> {
    package_profile(
        SMOLRUNNER_PLAN_PROFILE_ID,
        PLAN_COMMAND_ID,
        PLAN_COMMAND_DIGEST,
        Vec::new(),
        resources(GIB, GIB, 1, 1, 1)?,
        600,
    )
}

fn package_profile(
    profile_id: &str,
    command_id: &str,
    digest: &str,
    optional_capabilities: Vec<OptionalCapability>,
    resources: ResourceDefaults,
    timeout_seconds: u64,
) -> Result<RegisteredVerificationProfile, VerificationProfileRegistryError> {
    let package = PackageId::parse(PACKAGE)?;
    let required = capabilities(&["cargo", "rustc"])?;
    profile(ProfileDefinition {
        profile_id: VerificationProfileId::parse(profile_id)?,
        canonical_command: command(
            command_id,
            digest,
            ExactVerificationScope::WholePackageTests {
                package: package.clone(),
            },
            ExactBuildScope::WholePackage { package },
            required.clone(),
        )?,
        approved_equivalents: Vec::new(),
        required_capabilities: required.into_iter().map(RequiredCapability::new).collect(),
        optional_capabilities,
        resources,
        cache_class: cache_class()?,
        timeout: TimeoutPolicy::new(timeout_seconds, Vec::new())?,
        authority: read_only_authority()?,
    })
}

fn profile(
    definition: ProfileDefinition,
) -> Result<RegisteredVerificationProfile, VerificationProfileRegistryError> {
    RegisteredVerificationProfile::new(definition)
}

fn command(
    command_id: &str,
    digest: &str,
    test_scope: ExactVerificationScope,
    build_scope: ExactBuildScope,
    required_capabilities: Vec<CapabilityId>,
) -> Result<RepositoryCommandContract, VerificationProfileRegistryError> {
    Ok(RepositoryCommandContract::new(
        RepositoryCommandIdentity::new(
            RepositoryRef::parse(REPOSITORY)?,
            RepositoryCommandId::parse(command_id)?,
            Sha256Digest::parse(digest)?,
        ),
        test_scope,
        build_scope,
        required_capabilities,
    )?)
}

fn capabilities(values: &[&str]) -> Result<Vec<CapabilityId>, VerificationProfileRegistryError> {
    values
        .iter()
        .map(|value| CapabilityId::parse(value).map_err(VerificationProfileRegistryError::from))
        .collect()
}

fn optional(
    capability: &str,
    code: &str,
    summary: &str,
) -> Result<OptionalCapability, VerificationProfileRegistryError> {
    Ok(OptionalCapability::new(
        CapabilityId::parse(capability)?,
        DeclaredDeviation::new(DeviationCode::parse(code)?, summary)?,
    ))
}

fn resources(
    minimum_available_bytes: u64,
    estimated_peak_bytes: u64,
    build_jobs: u16,
    linker_jobs: u16,
    test_threads: u16,
) -> Result<ResourceDefaults, VerificationProfileRegistryError> {
    Ok(ResourceDefaults::new(
        MemoryPolicy::new(minimum_available_bytes, 0, estimated_peak_bytes)?,
        ConcurrencyPolicy::new(build_jobs, linker_jobs, test_threads)?,
    ))
}

fn cache_class() -> Result<VerificationCacheIdentityClass, VerificationProfileRegistryError> {
    Ok(VerificationCacheIdentityClass {
        kind: VerificationCacheClassKind::RunnerOwnedCargoTarget,
        cache_id: CacheId::parse(CACHE_ID)?,
    })
}

fn read_only_authority() -> Result<VerificationAuthorityPolicy, VerificationProfileRegistryError> {
    Ok(VerificationAuthorityPolicy {
        workspace: WorkspaceMutationPolicy::new(
            WorkspaceMutationAuthority::ReadOnly,
            DirtyWorkspacePolicy::RequireClean,
        )?,
        local_commit: LocalCommitAuthority::Forbidden,
        publication: PublicationAuthority::Forbidden,
    })
}

fn rust_envelope(
    profile: &RegisteredVerificationProfile,
    source: RustVerificationSourceIdentity,
    source_command_namespace: Sha256Digest,
) -> Result<RustVerificationEnvelope, VerificationProfileRegistryError> {
    let (cpu_millis, pids, target_directory_id, retry_policy_id, features) =
        match profile.profile_id().as_str() {
            SMOLRUNNER_REQUIRED_PROFILE_ID => (
                4_000,
                2_048,
                "smolrunner-required-target-v1",
                "smolrunner-required-no-retry-v1",
                RustFeatureSelection::All,
            ),
            SMOLRUNNER_DOCTOR_PROFILE_ID => (
                1_000,
                512,
                "smolrunner-doctor-target-v1",
                "smolrunner-doctor-no-retry-v1",
                RustFeatureSelection::Default,
            ),
            SMOLRUNNER_PLAN_PROFILE_ID => (
                1_000,
                512,
                "smolrunner-plan-target-v1",
                "smolrunner-plan-no-retry-v1",
                RustFeatureSelection::Default,
            ),
            _ => {
                return Err(VerificationProfileRegistryError::new(
                    "registry.rust_envelope.profile_id",
                    "unknown_rust_envelope",
                    "profile has no checked-in Rust verification envelope",
                ));
            }
        };
    let profile_resources = profile.resources();
    let reserved_resources = ExecutionResourceLimits::new(
        cpu_millis,
        profile_resources.memory.estimated_peak_bytes,
        pids,
    )
    .map_err(|_| {
        VerificationProfileRegistryError::new(
            "registry.rust_envelope.resources",
            "invalid_rust_resource_envelope",
            "checked-in Rust resource limits are invalid",
        )
    })?;
    let concurrency = RustConcurrencyPlan::new(
        profile_resources.concurrency.build_jobs,
        RustRuntimeConcurrency::Libtest {
            test_threads: profile_resources.concurrency.test_threads,
            filter: None,
        },
        Vec::new(),
    )?;
    let maximum_execution_millis = profile
        .timeout()
        .total_seconds()
        .checked_mul(1_000)
        .ok_or_else(|| {
            VerificationProfileRegistryError::new(
                "registry.rust_envelope.duration",
                "invalid_rust_execution_duration",
                "checked-in Rust execution duration is invalid",
            )
        })?;
    let resources = crate::rust_verification_envelope::RustResourceEnvelope::new(
        LimaResourceProfile::Work,
        reserved_resources,
        concurrency,
        profile_resources.memory.minimum_available_bytes,
        profile_resources.memory.minimum_swap_bytes,
        maximum_execution_millis,
    )?;
    let scope = match profile.canonical_command().test_scope() {
        ExactVerificationScope::WholeWorkspaceTests => RustVerificationScope::WorkspaceTests {
            members_digest: Sha256Digest::parse(RUST_WORKSPACE_MEMBERS_DIGEST)?,
            targets: RustTargetPolicy::all_targets(true),
        },
        ExactVerificationScope::WholePackageTests { package } => {
            RustVerificationScope::PackageTests {
                package: package.clone(),
                targets: RustTargetPolicy::repository_default(false, false, true),
            }
        }
        ExactVerificationScope::LibraryTests { .. }
        | ExactVerificationScope::IntegrationTestBinary { .. }
        | ExactVerificationScope::FilteredTest { .. } => {
            return Err(VerificationProfileRegistryError::new(
                "registry.rust_envelope.scope",
                "unmapped_rust_scope",
                "checked-in profile scope has no exact Rust envelope mapping",
            ));
        }
    };
    RustVerificationEnvelope::new(RustVerificationEnvelopeDefinition {
        profile_id: profile.profile_id().clone(),
        source,
        command: profile.canonical_command().identity().clone(),
        scope,
        compilation: RustCompilationContract::new(
            RustToolchainIdentity::new(
                RustToolchainId::parse(RUST_TOOLCHAIN_ID)?,
                Sha256Digest::parse(RUST_TOOLCHAIN_CONTRACT_DIGEST)?,
                RustTargetTriple::parse(RUST_HOST_TRIPLE)?,
                RustTargetTriple::parse(RUST_TARGET_TRIPLE)?,
            ),
            RustCargoProfileKind::Test,
            features,
            RustBuildScriptInclusion::Included,
        ),
        resources,
        cache: RustCacheContract::new(
            RustCacheIdentityClass::SourceScoped,
            CargoTargetDirectoryIdentity::new(
                RustTargetDirectoryId::parse(target_directory_id)?,
                profile.cache_class().cache_id().clone(),
                source_command_namespace,
            ),
        ),
        required_capabilities: profile
            .required_capabilities()
            .iter()
            .map(|required| required.capability.clone())
            .collect(),
        retry: RustRetryPolicy::no_retry(RustRetryPolicyId::parse(retry_policy_id)?),
    })
    .map_err(VerificationProfileRegistryError::from)
}

fn validate_capabilities(
    required: &[RequiredCapability],
    optional: &[OptionalCapability],
    command_required: &[CapabilityId],
) -> Result<(), VerificationProfileRegistryError> {
    if required.len() > MAX_CAPABILITIES || optional.len() > MAX_CAPABILITIES {
        return Err(VerificationProfileRegistryError::new(
            "registry.capabilities",
            "capability_count_exceeded",
            format!("each capability class may contain at most {MAX_CAPABILITIES} entries"),
        ));
    }
    let required_ids = required
        .iter()
        .map(|entry| entry.capability.clone())
        .collect::<BTreeSet<_>>();
    let optional_ids = optional
        .iter()
        .map(|entry| entry.capability.clone())
        .collect::<BTreeSet<_>>();
    if required_ids.len() != required.len() || optional_ids.len() != optional.len() {
        return Err(VerificationProfileRegistryError::new(
            "registry.capabilities",
            "duplicate_capability",
            "required and optional capabilities must be unique",
        ));
    }
    if required_ids.iter().any(|id| optional_ids.contains(id)) {
        return Err(VerificationProfileRegistryError::new(
            "registry.capabilities",
            "overlapping_capability_classes",
            "required and optional capability classes must remain distinct",
        ));
    }
    let command_ids = command_required.iter().cloned().collect::<BTreeSet<_>>();
    if command_ids != required_ids {
        return Err(VerificationProfileRegistryError::new(
            "registry.command.required_capabilities",
            "command_capability_mismatch",
            "canonical command capabilities must exactly match profile requirements",
        ));
    }
    Ok(())
}

fn validate_read_only(
    authority: &VerificationAuthorityPolicy,
) -> Result<(), VerificationProfileRegistryError> {
    let valid = authority.workspace.authority == WorkspaceMutationAuthority::ReadOnly
        && matches!(
            &authority.workspace.dirty_workspace,
            DirtyWorkspacePolicy::RequireClean
        )
        && authority.local_commit == LocalCommitAuthority::Forbidden
        && matches!(&authority.publication, PublicationAuthority::Forbidden);
    if valid {
        Ok(())
    } else {
        Err(VerificationProfileRegistryError::new(
            "registry.authority",
            "authority_widening",
            "checked-in profiles must remain strictly read-only",
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerificationProfileRegistryError {
    pub field: String,
    pub code: String,
    pub problem: String,
}

impl VerificationProfileRegistryError {
    fn new(field: impl Into<String>, code: impl Into<String>, problem: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            code: code.into(),
            problem: problem.into(),
        }
    }
}

impl From<VerificationProfileError> for VerificationProfileRegistryError {
    fn from(error: VerificationProfileError) -> Self {
        Self {
            field: error.field,
            code: error.code,
            problem: error.problem,
        }
    }
}

impl From<ArtifactIdentityError> for VerificationProfileRegistryError {
    fn from(error: ArtifactIdentityError) -> Self {
        Self {
            field: error.field,
            code: "invalid_artifact_identity".to_owned(),
            problem: error.problem,
        }
    }
}

impl From<RustVerificationEnvelopeError> for VerificationProfileRegistryError {
    fn from(error: RustVerificationEnvelopeError) -> Self {
        Self {
            field: error.field,
            code: error.code,
            problem: error.problem,
        }
    }
}

impl fmt::Display for VerificationProfileRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}: {}", self.field, self.code, self.problem)
    }
}

impl std::error::Error for VerificationProfileRegistryError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{CommitId, GitTreeId};
    use crate::rust_verification_envelope_digest::digest_rust_verification_envelope;
    use sha2::{Digest as _, Sha256};

    fn registry() -> VerificationProfileRegistry {
        smolrunner_profile_registry().expect("registry")
    }

    fn identity(command_id: &str, digest: &str) -> RepositoryCommandIdentity {
        RepositoryCommandIdentity::new(
            RepositoryRef::parse(REPOSITORY).expect("repository"),
            RepositoryCommandId::parse(command_id).expect("command ID"),
            Sha256Digest::parse(digest).expect("digest"),
        )
    }

    fn rust_source() -> RustVerificationSourceIdentity {
        RustVerificationSourceIdentity::new(
            RepositoryRef::parse(REPOSITORY).expect("repository"),
            CommitId::parse(&"1".repeat(40)).expect("commit"),
            GitTreeId::parse(&"2".repeat(40)).expect("tree"),
        )
    }

    #[test]
    fn rust_envelopes_are_checked_in_exact_and_digestible() {
        let toolchain_document_digest = format!(
            "sha256:{:x}",
            Sha256::digest(include_bytes!("../rust-toolchain.toml"))
        );
        assert_eq!(toolchain_document_digest, RUST_TOOLCHAIN_CONTRACT_DIGEST);
        let workspace_members_digest = format!(
            "sha256:{:x}",
            Sha256::digest(b"smolrunner-rust-workspace-members-v1\0smolrunner")
        );
        assert_eq!(workspace_members_digest, RUST_WORKSPACE_MEMBERS_DIGEST);

        let registry = registry();
        let namespace =
            Sha256Digest::parse(&format!("sha256:{}", "a".repeat(64))).expect("namespace");
        let expected = [
            (
                SMOLRUNNER_REQUIRED_PROFILE_ID,
                4_000,
                4 * GIB,
                2_048,
                2,
                2,
                "sha256:a8169b6dd94905418011fc04fbe01a1c94bc730a94498eab64805d0cbe8940c7",
            ),
            (
                SMOLRUNNER_DOCTOR_PROFILE_ID,
                1_000,
                512 * MIB,
                512,
                1,
                1,
                "sha256:06f63fd4887beb67ad749469d0d5cf071604c6aa9112d603b07a84ca605eda9f",
            ),
            (
                SMOLRUNNER_PLAN_PROFILE_ID,
                1_000,
                GIB,
                512,
                1,
                1,
                "sha256:a4a1e50e5df93cf7d66ecabe24be8587fce4d5752b2f1e825c72d09ff604df2e",
            ),
        ];
        for (profile_id, cpu, memory, pids, build_jobs, test_threads, expected_digest) in expected {
            let profile_id = VerificationProfileId::parse(profile_id).expect("profile ID");
            let envelope = registry
                .resolve_rust_envelope(&profile_id, rust_source(), namespace.clone())
                .expect("Rust envelope");
            assert_eq!(envelope.profile_id(), &profile_id);
            assert_eq!(
                envelope.command(),
                registry
                    .lookup(&profile_id)
                    .expect("profile")
                    .canonical_command()
                    .identity()
            );
            assert_eq!(
                envelope.resources().reserved_resources,
                ExecutionResourceLimits::new(cpu, memory, pids).expect("limits")
            );
            assert_eq!(
                envelope.resources().concurrency.cargo_build_jobs,
                build_jobs
            );
            assert!(matches!(
                envelope.resources().concurrency.runtime,
                RustRuntimeConcurrency::Libtest {
                    test_threads: actual,
                    filter: None,
                } if actual == test_threads
            ));
            assert_eq!(
                envelope.cache().cargo_target_directory.namespace_digest,
                namespace
            );
            assert_eq!(
                envelope.resources().required_worker_profile,
                LimaResourceProfile::Work
            );
            assert_eq!(
                envelope.resources().minimum_guest_available_memory_bytes,
                memory
            );
            assert_eq!(envelope.resources().minimum_guest_available_swap_bytes, 0);
            assert_eq!(
                envelope.resources().maximum_execution_millis,
                registry
                    .lookup(&profile_id)
                    .expect("profile")
                    .timeout()
                    .total_seconds()
                    * 1_000
            );
            assert!(matches!(envelope.retry(), RustRetryPolicy::NoRetry { .. }));
            let json = serde_json::to_string(&envelope).expect("envelope JSON");
            assert!(json.contains(RUST_TOOLCHAIN_ID));
            assert!(json.contains(RUST_TOOLCHAIN_CONTRACT_DIGEST));
            assert!(json.contains(RUST_TARGET_TRIPLE));
            let digest = digest_rust_verification_envelope(&envelope).expect("envelope digest");
            assert_eq!(digest.as_str(), expected_digest);
        }
    }

    #[test]
    fn exact_names_are_enumerated_in_stable_order() {
        let registry = registry();
        let ids = registry
            .profiles()
            .iter()
            .map(|profile| profile.profile_id().as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, PROFILE_IDS.to_vec());
        assert_eq!(
            registry.human_summary(),
            "profiles=3 ids=smolrunner.required,smolrunner.doctor,smolrunner.plan"
        );
    }

    #[test]
    fn lookup_refuses_unknown_validated_names() {
        let registry = registry();
        let required = VerificationProfileId::parse(SMOLRUNNER_REQUIRED_PROFILE_ID).expect("ID");
        assert_eq!(
            registry.lookup(&required).expect("known").profile_id(),
            &required
        );
        let unknown = VerificationProfileId::parse("smolrunner.unknown").expect("valid ID");
        assert_eq!(
            registry.lookup(&unknown).expect_err("unknown").code,
            "unknown_profile"
        );
    }

    #[test]
    fn scopes_are_exact_and_never_widened() {
        let registry = registry();
        let required = registry
            .lookup(&VerificationProfileId::parse(SMOLRUNNER_REQUIRED_PROFILE_ID).expect("ID"))
            .expect("required");
        assert_eq!(
            required.canonical_command().test_scope(),
            &ExactVerificationScope::WholeWorkspaceTests
        );
        assert_eq!(
            required.canonical_command().build_scope(),
            &ExactBuildScope::WholeWorkspace
        );
        for id in [SMOLRUNNER_DOCTOR_PROFILE_ID, SMOLRUNNER_PLAN_PROFILE_ID] {
            let profile = registry
                .lookup(&VerificationProfileId::parse(id).expect("ID"))
                .expect("profile");
            let package = PackageId::parse(PACKAGE).expect("package");
            assert_eq!(
                profile.canonical_command().test_scope(),
                &ExactVerificationScope::WholePackageTests {
                    package: package.clone(),
                }
            );
            assert_eq!(
                profile.canonical_command().build_scope(),
                &ExactBuildScope::WholePackage { package }
            );
        }
    }

    #[test]
    fn command_identities_and_digests_are_stable() {
        for (profile_id, command_id, digest) in [
            (
                SMOLRUNNER_REQUIRED_PROFILE_ID,
                REQUIRED_COMMAND_ID,
                REQUIRED_COMMAND_DIGEST,
            ),
            (
                SMOLRUNNER_DOCTOR_PROFILE_ID,
                DOCTOR_COMMAND_ID,
                DOCTOR_COMMAND_DIGEST,
            ),
            (
                SMOLRUNNER_PLAN_PROFILE_ID,
                PLAN_COMMAND_ID,
                PLAN_COMMAND_DIGEST,
            ),
        ] {
            let profile = registry()
                .lookup(&VerificationProfileId::parse(profile_id).expect("ID"))
                .expect("profile")
                .clone();
            assert_eq!(
                profile.canonical_command().identity(),
                &identity(command_id, digest)
            );
        }
    }

    #[test]
    fn aliases_and_duplicate_ids_are_rejected() {
        let mut aliases = vec![
            required_profile().expect("required"),
            doctor_profile().expect("doctor"),
            plan_profile().expect("plan"),
        ];
        aliases[0].profile_id = VerificationProfileId::parse("smolrunner.alias").expect("alias");
        assert_eq!(
            VerificationProfileRegistry::new(aliases)
                .expect_err("alias")
                .code,
            "profile_alias_or_order_mismatch"
        );
        let duplicate = required_profile().expect("required");
        assert_eq!(
            VerificationProfileRegistry::new(vec![
                duplicate.clone(),
                duplicate,
                plan_profile().expect("plan"),
            ])
            .expect_err("duplicate")
            .code,
            "duplicate_profile_id"
        );
    }

    #[test]
    fn undeclared_fallback_is_rejected() {
        let profile = required_profile().expect("required");
        let fallback = identity(
            "smolrunner.required.fallback",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        );
        assert_eq!(
            profile
                .select_command(&fallback)
                .expect_err("fallback")
                .code,
            "undeclared_fallback"
        );
    }

    #[test]
    fn shared_contract_rejects_scope_widening() {
        let error = RepositoryCommandContract::new(
            identity(REQUIRED_COMMAND_ID, REQUIRED_COMMAND_DIGEST),
            ExactVerificationScope::WholePackageTests {
                package: PackageId::parse(PACKAGE).expect("package"),
            },
            ExactBuildScope::WholeWorkspace,
            capabilities(&["cargo"]).expect("capability"),
        )
        .expect_err("widened scope");
        assert_eq!(error.code, "widened_build_scope");
    }

    #[test]
    fn authority_widening_is_rejected() {
        let mut profile = required_profile().expect("required");
        profile.authority.local_commit = LocalCommitAuthority::CreateInRunnerOwnedWorkspace;
        assert_eq!(
            validate_read_only(profile.authority())
                .expect_err("widened")
                .code,
            "authority_widening"
        );
    }

    #[test]
    fn exact_capability_resource_cache_timeout_and_authority_bindings_hold() {
        let required = required_profile().expect("required");
        assert!(required.approved_equivalents().is_empty());
        assert_eq!(
            required.resources(),
            resources(4 * GIB, 4 * GIB, 2, 1, 2).expect("resources")
        );
        assert_eq!(required.timeout().total_seconds(), 3_600);
        assert_eq!(required.cache_class(), &cache_class().expect("cache"));
        assert_eq!(required.cache_class().cache_id().as_str(), "cargo-target");
        assert_eq!(
            required
                .required_capabilities()
                .iter()
                .map(|entry| entry.capability.as_str())
                .collect::<Vec<_>>(),
            vec!["cargo", "rustc", "rustfmt", "clippy"]
        );
        assert_eq!(
            required
                .optional_capabilities()
                .iter()
                .map(|entry| entry.capability.as_str())
                .collect::<Vec<_>>(),
            vec!["podman", "systemd"]
        );
        assert_eq!(
            required.authority().workspace.authority,
            WorkspaceMutationAuthority::ReadOnly
        );
        assert_eq!(
            required.authority().local_commit,
            LocalCommitAuthority::Forbidden
        );
        assert!(matches!(
            &required.authority().publication,
            PublicationAuthority::Forbidden
        ));
    }

    #[test]
    fn public_output_contains_no_private_paths_or_secrets() {
        let registry = registry();
        let json = serde_json::to_string(&registry).expect("JSON");
        let debug = format!("{registry:?}");
        assert!(json.contains("\"cache_id\":\"cargo-target\""));
        assert!(!json.contains("namespace_digest"));
        for private in [
            "/var/lib/smolrunner",
            "/home/runner",
            "/Users/",
            "CARGO_HOME=",
            "RUSTUP_HOME=",
            "credential-value",
            "secret-token",
            "github.token",
        ] {
            assert!(!json.contains(private), "JSON leaked {private}");
            assert!(!debug.contains(private), "Debug leaked {private}");
        }
    }
}
