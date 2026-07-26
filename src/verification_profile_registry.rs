use std::collections::BTreeSet;
use std::fmt;

use serde::Serialize;

use crate::artifact::{ArtifactIdentityError, RepositoryRef, Sha256Digest};
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
const CACHE_ID: &str = "smolrunner-cargo-target";
const CACHE_NAMESPACE_DIGEST: &str =
    "sha256:010067f11ccdd816904b7ce368bac777daeb72e699fd4807d4685c83e0434ee6";
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

/// A path-free cache identity class. A future #117 adapter binds it to one exact workspace path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerificationCacheIdentityClass {
    kind: VerificationCacheClassKind,
    cache_id: CacheId,
    namespace_digest: Sha256Digest,
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

    #[must_use]
    pub const fn namespace_digest(&self) -> &Sha256Digest {
        &self.namespace_digest
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
        namespace_digest: Sha256Digest::parse(CACHE_NAMESPACE_DIGEST)?,
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

impl fmt::Display for VerificationProfileRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}: {}", self.field, self.code, self.problem)
    }
}

impl std::error::Error for VerificationProfileRegistryError {}

#[cfg(test)]
mod tests {
    use super::*;

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
