use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;

use serde::Serialize;

use crate::artifact::{CommitId, GitTreeId, RepositoryRef, Sha256Digest};
use crate::verification_profile::{
    CacheId, CacheIdentity, CacheObservation, CapabilityObservation, HostResourceObservation,
    ImmutableRefInput, ImmutableSourceInputs, LocalCommitAuthority, MAX_CAPABILITIES,
    MAX_PROFILE_REFS, PrivateVerificationEvidence, PublicationAuthority, RepositoryCommandIdentity,
    RequestedAuthority, ResolvedRef, RunnerInstallationId, RunnerOwnedWorkspaceIdentity,
    RunnerWorkspaceId, SourceComposition, TestedSourceIdentity, VerificationPreflightObservation,
    VerificationPreflightReport, VerificationProfileContract, VerificationProfileDefinition,
    VerificationProfileError, WorkspaceCleanliness, WorkspaceMutationAuthority,
    WorkspaceObservation, evaluate_verification_preflight,
};
use crate::verification_profile_registry::{
    RegisteredVerificationProfile, VerificationProfileRegistryError,
};

/// Private runner evidence retained only inside the trusted receipt and merged v1 evidence types.
#[derive(Clone, PartialEq, Eq)]
pub struct TrustedRunnerPrivateEvidence {
    workspace_root: PathBuf,
    cache_path: PathBuf,
}

impl TrustedRunnerPrivateEvidence {
    #[must_use]
    pub fn new(workspace_root: impl Into<PathBuf>, cache_path: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            cache_path: cache_path.into(),
        }
    }
}

impl fmt::Debug for TrustedRunnerPrivateEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedRunnerPrivateEvidence")
            .field("workspace_root", &"<private-path>")
            .field("cache_path", &"<private-path>")
            .finish()
    }
}

/// Complete typed input used to construct one already trusted runner workspace receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedRunnerWorkspaceReceiptDefinition {
    pub repository: RepositoryRef,
    pub installation_id: RunnerInstallationId,
    pub workspace_id: RunnerWorkspaceId,
    pub cleanliness: WorkspaceCleanliness,
    pub resolved_refs: Vec<ResolvedRef>,
    pub tested_commit: CommitId,
    pub tested_tree: GitTreeId,
    pub cache_id: CacheId,
    pub cache_owner_workspace_id: RunnerWorkspaceId,
    pub cache_namespace_digest: Sha256Digest,
    pub cache_present: bool,
    pub resources: HostResourceObservation,
    pub capabilities: Vec<CapabilityObservation>,
    pub selected_command: RepositoryCommandIdentity,
    pub requested_authorities: BTreeSet<RequestedAuthority>,
    pub private_evidence: TrustedRunnerPrivateEvidence,
}

/// Already trusted, typed runner-owned workspace and capability evidence.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct TrustedRunnerWorkspaceReceipt {
    repository: RepositoryRef,
    installation_id: RunnerInstallationId,
    workspace_id: RunnerWorkspaceId,
    cleanliness: WorkspaceCleanliness,
    resolved_refs: Vec<ResolvedRef>,
    tested_commit: CommitId,
    tested_tree: GitTreeId,
    cache_id: CacheId,
    cache_owner_workspace_id: RunnerWorkspaceId,
    cache_namespace_digest: Sha256Digest,
    cache_present: bool,
    resources: HostResourceObservation,
    capabilities: Vec<CapabilityObservation>,
    selected_command: RepositoryCommandIdentity,
    requested_authorities: BTreeSet<RequestedAuthority>,
    #[serde(skip)]
    private_evidence: TrustedRunnerPrivateEvidence,
}

impl TrustedRunnerWorkspaceReceipt {
    /// Validate one complete trusted runner receipt without observing the host or filesystem.
    ///
    /// # Errors
    ///
    /// Returns an error for missing or ambiguous refs, duplicate capabilities, source drift,
    /// repository drift, invalid private paths, or cache ownership drift.
    pub fn new(
        definition: TrustedRunnerWorkspaceReceiptDefinition,
    ) -> Result<Self, VerificationProfilePreflightAdapterError> {
        validate_receipt_shape(&definition)?;

        let workspace = RunnerOwnedWorkspaceIdentity::new(
            definition.installation_id.clone(),
            definition.workspace_id.clone(),
            definition.repository.clone(),
            definition.private_evidence.workspace_root.clone(),
        )?;
        CacheIdentity::new(
            &workspace,
            definition.cache_id.clone(),
            definition.cache_owner_workspace_id.clone(),
            definition.cache_namespace_digest.clone(),
            definition.private_evidence.cache_path.clone(),
        )?;

        Ok(Self {
            repository: definition.repository,
            installation_id: definition.installation_id,
            workspace_id: definition.workspace_id,
            cleanliness: definition.cleanliness,
            resolved_refs: definition.resolved_refs,
            tested_commit: definition.tested_commit,
            tested_tree: definition.tested_tree,
            cache_id: definition.cache_id,
            cache_owner_workspace_id: definition.cache_owner_workspace_id,
            cache_namespace_digest: definition.cache_namespace_digest,
            cache_present: definition.cache_present,
            resources: definition.resources,
            capabilities: definition.capabilities,
            selected_command: definition.selected_command,
            requested_authorities: definition.requested_authorities,
            private_evidence: definition.private_evidence,
        })
    }
}

impl fmt::Debug for TrustedRunnerWorkspaceReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedRunnerWorkspaceReceipt")
            .field("repository", &self.repository)
            .field("installation_id", &self.installation_id)
            .field("workspace_id", &self.workspace_id)
            .field("cleanliness", &self.cleanliness)
            .field("resolved_refs", &self.resolved_refs)
            .field("tested_commit", &self.tested_commit)
            .field("tested_tree", &self.tested_tree)
            .field("cache_id", &self.cache_id)
            .field("cache_owner_workspace_id", &self.cache_owner_workspace_id)
            .field("cache_namespace_digest", &self.cache_namespace_digest)
            .field("cache_present", &self.cache_present)
            .field("resources", &self.resources)
            .field("capabilities", &self.capabilities)
            .field("selected_command", &self.selected_command)
            .field("requested_authorities", &self.requested_authorities)
            .field("private_evidence", &"<retained private evidence>")
            .finish()
    }
}

/// Bind one checked-in registry entry to exact trusted runner evidence and evaluate preflight.
///
/// This function performs no filesystem reads, subprocess execution, cache creation, Git
/// operations, publication, or mutation.
///
/// # Errors
///
/// Returns an error for identity drift, missing or undeclared capability observations, an absent
/// cache, command aliases or fallbacks, widened authority requests, or any merged-v1 contract
/// validation failure.
pub fn evaluate_registered_verification_profile(
    profile: &RegisteredVerificationProfile,
    receipt: TrustedRunnerWorkspaceReceipt,
) -> Result<VerificationPreflightReport, VerificationProfilePreflightAdapterError> {
    validate_receipt_for_profile(profile, &receipt)?;

    let workspace = RunnerOwnedWorkspaceIdentity::new(
        receipt.installation_id.clone(),
        receipt.workspace_id.clone(),
        receipt.repository.clone(),
        receipt.private_evidence.workspace_root.clone(),
    )?;
    let cache = CacheIdentity::new(
        &workspace,
        receipt.cache_id.clone(),
        receipt.cache_owner_workspace_id.clone(),
        receipt.cache_namespace_digest.clone(),
        receipt.private_evidence.cache_path.clone(),
    )?;
    let resolved_ref = receipt
        .resolved_refs
        .first()
        .ok_or_else(|| {
            VerificationProfilePreflightAdapterError::new(
                "receipt.resolved_refs",
                "missing_source_ref",
                "trusted receipt must contain one exact resolved ref",
            )
        })?
        .clone();
    let source = ImmutableSourceInputs::new(
        receipt.repository.clone(),
        vec![ImmutableRefInput::new(
            resolved_ref.ref_name.clone(),
            resolved_ref.commit.clone(),
        )],
        SourceComposition::SingleRef,
        TestedSourceIdentity::Commit {
            commit: receipt.tested_commit.clone(),
            tree: receipt.tested_tree.clone(),
        },
    )?;
    let contract = VerificationProfileContract::new(VerificationProfileDefinition {
        profile_id: profile.profile_id().clone(),
        workspace,
        source,
        required_capabilities: profile.required_capabilities().to_vec(),
        optional_capabilities: profile.optional_capabilities().to_vec(),
        canonical_command: profile.canonical_command().clone(),
        approved_equivalents: profile.approved_equivalents().to_vec(),
        resources: profile.resources(),
        cache,
        timeout: profile.timeout().clone(),
        authority: profile.authority().clone(),
        additional_declared_deviations: Vec::new(),
    })?;
    let observation = VerificationPreflightObservation {
        workspace: WorkspaceObservation::new(
            receipt.installation_id,
            receipt.workspace_id.clone(),
            receipt.repository,
            receipt.private_evidence.workspace_root.clone(),
            receipt.cleanliness,
        )?,
        resolved_refs: receipt.resolved_refs,
        capabilities: receipt.capabilities,
        resources: receipt.resources,
        cache: CacheObservation {
            cache_id: receipt.cache_id,
            owner_workspace_id: receipt.cache_owner_workspace_id,
            namespace_digest: receipt.cache_namespace_digest,
            present: receipt.cache_present,
        },
        selected_command: receipt.selected_command,
        requested_authorities: receipt.requested_authorities,
        private_evidence: PrivateVerificationEvidence::new(
            vec![
                receipt.private_evidence.workspace_root,
                receipt.private_evidence.cache_path,
            ],
            BTreeMap::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ),
    };

    Ok(evaluate_verification_preflight(&contract, observation)?)
}

fn validate_receipt_shape(
    definition: &TrustedRunnerWorkspaceReceiptDefinition,
) -> Result<(), VerificationProfilePreflightAdapterError> {
    if definition.resolved_refs.is_empty() {
        return Err(VerificationProfilePreflightAdapterError::new(
            "receipt.resolved_refs",
            "missing_source_ref",
            "trusted receipt must contain one exact resolved ref",
        ));
    }
    if definition.resolved_refs.len() > MAX_PROFILE_REFS {
        return Err(VerificationProfilePreflightAdapterError::new(
            "receipt.resolved_refs",
            "source_ref_count_exceeded",
            format!("trusted receipt may contain at most {MAX_PROFILE_REFS} resolved refs"),
        ));
    }
    let mut ref_names = BTreeSet::new();
    if definition
        .resolved_refs
        .iter()
        .any(|entry| !ref_names.insert(entry.ref_name.clone()))
    {
        return Err(VerificationProfilePreflightAdapterError::new(
            "receipt.resolved_refs",
            "duplicate_source_ref",
            "trusted receipt must not contain duplicate ref names",
        ));
    }
    if definition.resolved_refs.len() != 1 {
        return Err(VerificationProfilePreflightAdapterError::new(
            "receipt.resolved_refs",
            "ambiguous_source_refs",
            "trusted commit-backed receipt must contain exactly one resolved ref",
        ));
    }
    if definition.resolved_refs[0].commit != definition.tested_commit {
        return Err(VerificationProfilePreflightAdapterError::new(
            "receipt.tested_commit",
            "tested_commit_mismatch",
            "tested commit must equal the exact commit resolved from the sole source ref",
        ));
    }
    if definition.capabilities.len() > MAX_CAPABILITIES {
        return Err(VerificationProfilePreflightAdapterError::new(
            "receipt.capabilities",
            "capability_count_exceeded",
            format!(
                "trusted receipt may contain at most {MAX_CAPABILITIES} capability observations"
            ),
        ));
    }
    let mut capability_ids = BTreeSet::new();
    if definition
        .capabilities
        .iter()
        .any(|entry| !capability_ids.insert(entry.capability.clone()))
    {
        return Err(VerificationProfilePreflightAdapterError::new(
            "receipt.capabilities",
            "duplicate_capability_observation",
            "trusted receipt must contain at most one observation per canonical capability ID",
        ));
    }
    if definition.selected_command.repository() != &definition.repository {
        return Err(VerificationProfilePreflightAdapterError::new(
            "receipt.selected_command.repository",
            "repository_identity_mismatch",
            "selected command must belong to the exact receipt repository",
        ));
    }

    Ok(())
}

fn validate_receipt_for_profile(
    profile: &RegisteredVerificationProfile,
    receipt: &TrustedRunnerWorkspaceReceipt,
) -> Result<(), VerificationProfilePreflightAdapterError> {
    let repository = profile.canonical_command().identity().repository();
    if &receipt.repository != repository {
        return Err(VerificationProfilePreflightAdapterError::new(
            "receipt.repository",
            "repository_identity_mismatch",
            "trusted receipt repository must exactly match the checked-in profile repository",
        ));
    }
    profile.select_command(&receipt.selected_command)?;

    let expected_capabilities = profile
        .required_capabilities()
        .iter()
        .map(|entry| entry.capability.clone())
        .chain(
            profile
                .optional_capabilities()
                .iter()
                .map(|entry| entry.capability.clone()),
        )
        .collect::<BTreeSet<_>>();
    let observed_capabilities = receipt
        .capabilities
        .iter()
        .map(|entry| entry.capability.clone())
        .collect::<BTreeSet<_>>();
    if let Some(capability) = expected_capabilities
        .difference(&observed_capabilities)
        .next()
    {
        return Err(VerificationProfilePreflightAdapterError::new(
            "receipt.capabilities",
            "missing_capability_observation",
            format!(
                "trusted receipt is missing canonical capability observation {}",
                capability.as_str()
            ),
        ));
    }
    if let Some(capability) = observed_capabilities
        .difference(&expected_capabilities)
        .next()
    {
        return Err(VerificationProfilePreflightAdapterError::new(
            "receipt.capabilities",
            "undeclared_capability_observation",
            format!(
                "trusted receipt contains undeclared capability observation {}",
                capability.as_str()
            ),
        ));
    }
    if receipt.cache_id != *profile.cache_class().cache_id() {
        return Err(VerificationProfilePreflightAdapterError::new(
            "receipt.cache_id",
            "cache_identity_mismatch",
            "trusted receipt cache ID must exactly match the checked-in cache identity class",
        ));
    }
    if receipt.cache_owner_workspace_id != receipt.workspace_id {
        return Err(VerificationProfilePreflightAdapterError::new(
            "receipt.cache_owner_workspace_id",
            "cache_owner_mismatch",
            "trusted receipt cache owner must exactly match the runner workspace ID",
        ));
    }
    if !receipt.cache_present {
        return Err(VerificationProfilePreflightAdapterError::new(
            "receipt.cache_present",
            "missing_cache",
            "trusted receipt must prove the exact runner-owned cache is present",
        ));
    }

    let allowed_authorities = allowed_requested_authorities(profile);
    if let Some(authority) = receipt
        .requested_authorities
        .difference(&allowed_authorities)
        .next()
    {
        return Err(VerificationProfilePreflightAdapterError::new(
            "receipt.requested_authorities",
            "authority_widening",
            format!("requested authority {authority:?} is not declared by the checked-in profile"),
        ));
    }

    Ok(())
}

fn allowed_requested_authorities(
    profile: &RegisteredVerificationProfile,
) -> BTreeSet<RequestedAuthority> {
    let mut allowed = BTreeSet::new();
    if profile.authority().workspace.authority
        == WorkspaceMutationAuthority::ResetRunnerOwnedWorkspace
    {
        allowed.insert(RequestedAuthority::WorkspaceReset);
    }
    if profile.authority().local_commit == LocalCommitAuthority::CreateInRunnerOwnedWorkspace {
        allowed.insert(RequestedAuthority::LocalCommit);
    }
    if matches!(
        &profile.authority().publication,
        PublicationAuthority::CredentialedPublisherOnly { .. }
    ) {
        allowed.insert(RequestedAuthority::Publication);
    }
    allowed
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerificationProfilePreflightAdapterError {
    pub field: String,
    pub code: String,
    pub problem: String,
}

impl VerificationProfilePreflightAdapterError {
    fn new(field: impl Into<String>, code: impl Into<String>, problem: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            code: code.into(),
            problem: problem.into(),
        }
    }
}

impl From<VerificationProfileError> for VerificationProfilePreflightAdapterError {
    fn from(error: VerificationProfileError) -> Self {
        Self {
            field: error.field,
            code: error.code,
            problem: error.problem,
        }
    }
}

impl From<VerificationProfileRegistryError> for VerificationProfilePreflightAdapterError {
    fn from(error: VerificationProfileRegistryError) -> Self {
        Self {
            field: error.field,
            code: error.code,
            problem: error.problem,
        }
    }
}

impl fmt::Display for VerificationProfilePreflightAdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}: {}", self.field, self.code, self.problem)
    }
}

impl std::error::Error for VerificationProfilePreflightAdapterError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verification_profile::{
        CapabilityId, PreflightBlocker, PreflightReadiness, RepositoryCommandId, RepositoryRefName,
        VerificationProfileId,
    };
    use crate::verification_profile_registry::{
        SMOLRUNNER_REQUIRED_PROFILE_ID, smolrunner_profile_registry,
    };

    const WORKSPACE_ROOT: &str = "/srv/smolrunner/workspaces/verification-a";
    const CACHE_PATH: &str = "/srv/smolrunner/workspaces/verification-a/.cache/cargo-target";
    const CONCRETE_CACHE_NAMESPACE_DIGEST: &str =
        "sha256:abababababababababababababababababababababababababababababababab";

    fn commit(value: &str) -> CommitId {
        CommitId::parse(&value.repeat(40)).expect("commit")
    }

    fn tree(value: &str) -> GitTreeId {
        GitTreeId::parse(&value.repeat(40)).expect("tree")
    }

    fn required_profile() -> RegisteredVerificationProfile {
        smolrunner_profile_registry()
            .expect("registry")
            .lookup(
                &VerificationProfileId::parse(SMOLRUNNER_REQUIRED_PROFILE_ID).expect("profile ID"),
            )
            .expect("required profile")
            .clone()
    }

    fn capability_observations(
        profile: &RegisteredVerificationProfile,
        available: bool,
    ) -> Vec<CapabilityObservation> {
        profile
            .required_capabilities()
            .iter()
            .map(|entry| entry.capability.clone())
            .chain(
                profile
                    .optional_capabilities()
                    .iter()
                    .map(|entry| entry.capability.clone()),
            )
            .map(|capability| CapabilityObservation {
                capability,
                available,
            })
            .collect()
    }

    fn receipt_definition(
        profile: &RegisteredVerificationProfile,
    ) -> TrustedRunnerWorkspaceReceiptDefinition {
        let workspace_id = RunnerWorkspaceId::parse("workspace-a").expect("workspace ID");
        let resolved_commit = commit("1");
        TrustedRunnerWorkspaceReceiptDefinition {
            repository: RepositoryRef::parse("teamleaderleo/smolrunner").expect("repository"),
            installation_id: RunnerInstallationId::parse("installation-a")
                .expect("installation ID"),
            workspace_id: workspace_id.clone(),
            cleanliness: WorkspaceCleanliness::Clean,
            resolved_refs: vec![ResolvedRef {
                ref_name: RepositoryRefName::parse("refs/heads/main").expect("ref"),
                commit: resolved_commit.clone(),
            }],
            tested_commit: resolved_commit,
            tested_tree: tree("2"),
            cache_id: profile.cache_class().cache_id().clone(),
            cache_owner_workspace_id: workspace_id,
            cache_namespace_digest: Sha256Digest::parse(CONCRETE_CACHE_NAMESPACE_DIGEST)
                .expect("concrete cache namespace"),
            cache_present: true,
            resources: HostResourceObservation {
                available_memory_bytes: profile.resources().memory.minimum_available_bytes,
                available_swap_bytes: profile.resources().memory.minimum_swap_bytes,
            },
            capabilities: capability_observations(profile, true),
            selected_command: profile.canonical_command().identity().clone(),
            requested_authorities: BTreeSet::new(),
            private_evidence: TrustedRunnerPrivateEvidence::new(WORKSPACE_ROOT, CACHE_PATH),
        }
    }

    fn receipt(profile: &RegisteredVerificationProfile) -> TrustedRunnerWorkspaceReceipt {
        TrustedRunnerWorkspaceReceipt::new(receipt_definition(profile)).expect("receipt")
    }

    #[test]
    fn exact_trusted_receipt_evaluates_ready() {
        let profile = required_profile();
        let report =
            evaluate_registered_verification_profile(&profile, receipt(&profile)).expect("report");
        assert_eq!(report.readiness(), PreflightReadiness::Ready);
        assert!(report.blockers().is_empty());
        assert_eq!(
            report.selected_command(),
            profile.canonical_command().identity()
        );
    }

    #[test]
    fn concrete_cache_namespace_is_bound_by_the_trusted_receipt_not_the_global_class() {
        let profile = required_profile();
        let mut definition = receipt_definition(&profile);
        definition.cache_namespace_digest = Sha256Digest::parse(
            "sha256:cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
        )
        .expect("second concrete namespace");

        let report = evaluate_registered_verification_profile(
            &profile,
            TrustedRunnerWorkspaceReceipt::new(definition).expect("trusted receipt"),
        )
        .expect("concrete namespace is installation-specific");

        assert_eq!(report.readiness(), PreflightReadiness::Ready);
    }

    #[test]
    fn unavailable_declared_optional_capabilities_remain_bounded_deviations() {
        let profile = required_profile();
        let mut definition = receipt_definition(&profile);
        for observation in &mut definition.capabilities {
            if matches!(observation.capability.as_str(), "podman" | "systemd") {
                observation.available = false;
            }
        }
        let report = evaluate_registered_verification_profile(
            &profile,
            TrustedRunnerWorkspaceReceipt::new(definition).expect("receipt"),
        )
        .expect("report");
        assert_eq!(
            report.readiness(),
            PreflightReadiness::ReadyWithDeclaredDeviations
        );
        assert_eq!(report.deviations().len(), 2);
    }

    #[test]
    fn dirty_workspace_is_preserved_for_the_merged_evaluator() {
        let profile = required_profile();
        let mut definition = receipt_definition(&profile);
        definition.cleanliness = WorkspaceCleanliness::Dirty {
            changed_path_count: 1,
        };
        let report = evaluate_registered_verification_profile(
            &profile,
            TrustedRunnerWorkspaceReceipt::new(definition).expect("receipt"),
        )
        .expect("report");
        assert_eq!(report.readiness(), PreflightReadiness::Blocked);
        assert_eq!(
            report.blockers(),
            &[PreflightBlocker::DirtyWorkspaceForbidden]
        );
    }

    #[test]
    fn missing_duplicate_and_undeclared_capabilities_are_rejected() {
        let profile = required_profile();

        let mut missing = receipt_definition(&profile);
        missing.capabilities.pop();
        assert_eq!(
            evaluate_registered_verification_profile(
                &profile,
                TrustedRunnerWorkspaceReceipt::new(missing).expect("receipt"),
            )
            .expect_err("missing")
            .code,
            "missing_capability_observation"
        );

        let mut duplicate = receipt_definition(&profile);
        duplicate
            .capabilities
            .push(duplicate.capabilities.first().expect("capability").clone());
        assert_eq!(
            TrustedRunnerWorkspaceReceipt::new(duplicate)
                .expect_err("duplicate")
                .code,
            "duplicate_capability_observation"
        );

        let mut undeclared = receipt_definition(&profile);
        undeclared.capabilities.push(CapabilityObservation {
            capability: CapabilityId::parse("cargo-alias").expect("capability"),
            available: true,
        });
        assert_eq!(
            evaluate_registered_verification_profile(
                &profile,
                TrustedRunnerWorkspaceReceipt::new(undeclared).expect("receipt"),
            )
            .expect_err("undeclared")
            .code,
            "undeclared_capability_observation"
        );
    }

    #[test]
    fn source_ambiguity_and_commit_drift_are_rejected() {
        let profile = required_profile();

        let mut ambiguous = receipt_definition(&profile);
        ambiguous.resolved_refs.push(ResolvedRef {
            ref_name: RepositoryRefName::parse("refs/heads/other").expect("ref"),
            commit: ambiguous.tested_commit.clone(),
        });
        assert_eq!(
            TrustedRunnerWorkspaceReceipt::new(ambiguous)
                .expect_err("ambiguous")
                .code,
            "ambiguous_source_refs"
        );

        let mut drifted = receipt_definition(&profile);
        drifted.tested_commit = commit("3");
        assert_eq!(
            TrustedRunnerWorkspaceReceipt::new(drifted)
                .expect_err("drift")
                .code,
            "tested_commit_mismatch"
        );
    }

    #[test]
    fn repository_command_cache_and_authority_drift_are_rejected() {
        let profile = required_profile();

        let mut repository = receipt_definition(&profile);
        repository.repository = RepositoryRef::parse("example/other").expect("repository");
        repository.selected_command = RepositoryCommandIdentity::new(
            repository.repository.clone(),
            RepositoryCommandId::parse("smolrunner.required.v1").expect("command"),
            Sha256Digest::parse(
                "sha256:fab0c53ffcb5bf63764155bc1e9dc85371cf2240190ab9cd36ad412cace62dc5",
            )
            .expect("digest"),
        );
        let repository_receipt =
            TrustedRunnerWorkspaceReceipt::new(repository).expect("typed receipt");
        assert_eq!(
            evaluate_registered_verification_profile(&profile, repository_receipt)
                .expect_err("repository")
                .code,
            "repository_identity_mismatch"
        );

        let mut cache = receipt_definition(&profile);
        cache.cache_id = CacheId::parse("cache-alias").expect("cache");
        assert_eq!(
            evaluate_registered_verification_profile(
                &profile,
                TrustedRunnerWorkspaceReceipt::new(cache).expect("receipt"),
            )
            .expect_err("cache")
            .code,
            "cache_identity_mismatch"
        );

        let mut absent = receipt_definition(&profile);
        absent.cache_present = false;
        assert_eq!(
            evaluate_registered_verification_profile(
                &profile,
                TrustedRunnerWorkspaceReceipt::new(absent).expect("receipt"),
            )
            .expect_err("absent")
            .code,
            "missing_cache"
        );

        let mut authority = receipt_definition(&profile);
        authority
            .requested_authorities
            .insert(RequestedAuthority::Publication);
        assert_eq!(
            evaluate_registered_verification_profile(
                &profile,
                TrustedRunnerWorkspaceReceipt::new(authority).expect("receipt"),
            )
            .expect_err("authority")
            .code,
            "authority_widening"
        );
    }

    #[test]
    fn command_aliases_and_private_path_escape_are_rejected() {
        let profile = required_profile();

        let mut alias = receipt_definition(&profile);
        alias.selected_command = RepositoryCommandIdentity::new(
            alias.repository.clone(),
            RepositoryCommandId::parse("smolrunner.required.alias").expect("command"),
            Sha256Digest::parse(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .expect("digest"),
        );
        assert_eq!(
            evaluate_registered_verification_profile(
                &profile,
                TrustedRunnerWorkspaceReceipt::new(alias).expect("receipt"),
            )
            .expect_err("alias")
            .code,
            "undeclared_fallback"
        );

        let mut escaped = receipt_definition(&profile);
        escaped.private_evidence =
            TrustedRunnerPrivateEvidence::new(WORKSPACE_ROOT, "/srv/shared/cache");
        assert_eq!(
            TrustedRunnerWorkspaceReceipt::new(escaped)
                .expect_err("escape")
                .code,
            "workspace_path_escape"
        );
    }

    #[test]
    fn private_roots_never_enter_debug_or_json_output() {
        let profile = required_profile();
        let receipt = receipt(&profile);
        let debug = format!("{receipt:?}");
        let json = serde_json::to_string(&receipt).expect("JSON");
        assert!(!debug.contains(WORKSPACE_ROOT));
        assert!(!debug.contains(CACHE_PATH));
        assert!(!json.contains(WORKSPACE_ROOT));
        assert!(!json.contains(CACHE_PATH));

        let report = evaluate_registered_verification_profile(&profile, receipt).expect("report");
        let report_debug = format!("{report:?}");
        let report_json = serde_json::to_string(&report).expect("JSON");
        assert!(!report_debug.contains(WORKSPACE_ROOT));
        assert!(!report_debug.contains(CACHE_PATH));
        assert!(!report_json.contains(WORKSPACE_ROOT));
        assert!(!report_json.contains(CACHE_PATH));
    }
}
