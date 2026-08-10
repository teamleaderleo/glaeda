use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::artifact::{CommitId, GitTreeId, Sha256Digest};
use crate::project_catalog::{ProjectCatalog, ProjectCatalogIdentity, ProjectIdentity};
use crate::project_discovery::{
    ProjectDiscoveryEntry, ProjectDiscoveryEntryKind, ProjectDiscoveryMatch, ProjectRecoveryRisk,
};

pub const PROJECT_MATERIALIZATION_GENERATION_SCHEMA_VERSION: u8 = 1;
pub const MAX_PROJECT_MATERIALIZATIONS: usize = 512;
pub const MAX_PROJECT_ADOPTION_RECORDS: usize = 1024;
pub const MAX_PROJECT_ADOPTION_REQUEST_ID_BYTES: usize = 64;

const SHA256_PREFIX: &str = "sha256:";
const HEX: &[u8; 16] = b"0123456789abcdef";
const OBSERVATION_DIGEST_DOMAIN: &[u8] = b"smolrunner-project-checkout-observation-v1\0";
const ADOPTION_INPUT_DIGEST_DOMAIN: &[u8] = b"smolrunner-project-adoption-input-v1\0";
const ADOPTION_RECORD_DIGEST_DOMAIN: &[u8] = b"smolrunner-project-adoption-record-v1\0";
const GENERATION_DIGEST_DOMAIN: &[u8] = b"smolrunner-project-materialization-generation-v1\0";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ProjectAdoptionRequestId(String);

impl ProjectAdoptionRequestId {
    /// Parse one bounded caller-selected idempotency identity.
    ///
    /// # Errors
    ///
    /// Returns a bounded error unless the ID starts with a lowercase ASCII letter or digit and
    /// continues with lowercase letters, digits, `.`, `_`, or `-`.
    pub fn parse(value: &str) -> Result<Self, ProjectAdoptionError> {
        let mut bytes = value.bytes();
        let Some(first) = bytes.next() else {
            return Err(invalid_request_id());
        };
        if value.len() > MAX_PROJECT_ADOPTION_REQUEST_ID_BYTES
            || (!first.is_ascii_lowercase() && !first.is_ascii_digit())
            || !bytes.all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'_' | b'-')
            })
        {
            return Err(invalid_request_id());
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectMaterializationClass {
    AdoptedMac,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectMaterializationGenerationIdentity {
    pub number: u64,
    pub digest: Sha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AcceptedProjectMaterialization {
    pub project: ProjectIdentity,
    pub class: ProjectMaterializationClass,
    pub materialization_id: Sha256Digest,
    pub observation_digest: Sha256Digest,
    pub observed_commit: CommitId,
    pub observed_tree: GitTreeId,
    pub recovery: ProjectRecoveryRisk,
    pub first_request_id: ProjectAdoptionRequestId,
    pub accepted_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectAdoptionOperationRecord {
    pub request_id: ProjectAdoptionRequestId,
    pub input_digest: Sha256Digest,
    pub project: ProjectIdentity,
    pub materialization_id: Sha256Digest,
    pub accepted_generation: u64,
    pub record_digest: Sha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectMaterializationGeneration {
    schema_version: u8,
    identity: ProjectMaterializationGenerationIdentity,
    #[serde(skip_serializing_if = "Option::is_none")]
    predecessor: Option<ProjectMaterializationGenerationIdentity>,
    catalog_identity: ProjectCatalogIdentity,
    materializations: Vec<AcceptedProjectMaterialization>,
    adoption_records: Vec<ProjectAdoptionOperationRecord>,
}

impl ProjectMaterializationGeneration {
    /// Build deterministic empty generation zero for one exact logical project catalog.
    ///
    /// # Errors
    ///
    /// Returns a bounded error only when canonical generation identity encoding fails.
    pub fn initial(catalog_identity: ProjectCatalogIdentity) -> Result<Self, ProjectAdoptionError> {
        build_generation(0, None, catalog_identity, Vec::new(), Vec::new())
    }

    #[must_use]
    pub const fn identity(&self) -> &ProjectMaterializationGenerationIdentity {
        &self.identity
    }

    #[must_use]
    pub const fn predecessor(&self) -> Option<&ProjectMaterializationGenerationIdentity> {
        self.predecessor.as_ref()
    }

    #[must_use]
    pub const fn catalog_identity(&self) -> &ProjectCatalogIdentity {
        &self.catalog_identity
    }

    #[must_use]
    pub fn materializations(&self) -> &[AcceptedProjectMaterialization] {
        &self.materializations
    }

    #[must_use]
    pub fn adoption_records(&self) -> &[ProjectAdoptionOperationRecord] {
        &self.adoption_records
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectAdoptionCandidate {
    pub project: ProjectIdentity,
    pub materialization_id: Sha256Digest,
    pub observation_digest: Sha256Digest,
    pub observed_commit: CommitId,
    pub observed_tree: GitTreeId,
    pub recovery: ProjectRecoveryRisk,
}

impl ProjectAdoptionCandidate {
    /// Project one accepted read-only discovery entry into pure adoption evidence.
    ///
    /// # Errors
    ///
    /// Returns a bounded error unless the entry is a successful checkout resolving to exactly one
    /// catalogued project with a complete recovery/observation receipt.
    pub fn from_discovery_entry(entry: &ProjectDiscoveryEntry) -> Result<Self, ProjectAdoptionError> {
        if entry.kind != ProjectDiscoveryEntryKind::Checkout {
            return Err(discovery_not_checkout());
        }
        let project = project_from_match(entry.project_match.as_ref())?.clone();
        let checkout = entry.checkout.as_ref().ok_or_else(discovery_incomplete)?;
        let recovery = entry.recovery.clone().ok_or_else(discovery_incomplete)?;
        let bytes = serde_json::to_vec(checkout).map_err(|_| identity_encoding_failed())?;
        let observation_digest = domain_digest(OBSERVATION_DIGEST_DOMAIN, &bytes)?;
        let value: SerializedCheckoutIdentity =
            serde_json::from_slice(&bytes).map_err(|_| discovery_incomplete())?;
        let observed_commit = CommitId::parse(&value.commit).map_err(|_| discovery_incomplete())?;
        let observed_tree = GitTreeId::parse(&value.tree).map_err(|_| discovery_incomplete())?;
        Ok(Self {
            project,
            materialization_id: checkout.materialization_id().clone(),
            observation_digest,
            observed_commit,
            observed_tree,
            recovery,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectAdoptionPlan {
    pub predecessor: ProjectMaterializationGenerationIdentity,
    pub input_digest: Sha256Digest,
    pub operation_record: ProjectAdoptionOperationRecord,
    pub successor: ProjectMaterializationGeneration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum ProjectAdoptionDecision {
    Apply { plan: Box<ProjectAdoptionPlan> },
    Replay { record: ProjectAdoptionOperationRecord },
    Satisfied { materialization: AcceptedProjectMaterialization },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectAdoptionErrorKind {
    InvalidRequestId,
    DiscoveryNotCheckout,
    CatalogProjectRequired,
    AmbiguousSource,
    AmbiguousCatalog,
    DiscoveryIncomplete,
    ChangedInputConflict,
    StaleGeneration,
    CatalogIdentityMismatch,
    DuplicateProjectMaterialization,
    MaterializationIdentityConflict,
    GenerationExhausted,
    MaterializationLimitReached,
    AdoptionRecordLimitReached,
    IdentityEncodingFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectAdoptionError {
    pub kind: ProjectAdoptionErrorKind,
    pub code: &'static str,
    pub problem: &'static str,
}

impl fmt::Display for ProjectAdoptionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.problem)
    }
}

impl std::error::Error for ProjectAdoptionError {}

/// Plan one pure in-place adoption from accepted read-only discovery evidence.
///
/// Replay lookup deliberately precedes stale-generation checks, so a caller that lost the original
/// response can recover the exact accepted operation after later unrelated generations publish.
///
/// # Errors
///
/// Returns bounded conflicts for changed request reuse, stale predecessor evidence, catalog drift,
/// duplicate project materializations, materialization identity reuse, exhausted bounds, or invalid
/// discovery evidence. This function performs no I/O.
pub fn plan_project_adoption(
    current: &ProjectMaterializationGeneration,
    catalog: &ProjectCatalog,
    discovery: &ProjectDiscoveryEntry,
    request_id: ProjectAdoptionRequestId,
    expected_predecessor: &ProjectMaterializationGenerationIdentity,
) -> Result<ProjectAdoptionDecision, ProjectAdoptionError> {
    let candidate = ProjectAdoptionCandidate::from_discovery_entry(discovery)?;
    plan_project_adoption_candidate(
        current,
        catalog,
        &candidate,
        request_id,
        expected_predecessor,
    )
}

fn plan_project_adoption_candidate(
    current: &ProjectMaterializationGeneration,
    catalog: &ProjectCatalog,
    candidate: &ProjectAdoptionCandidate,
    request_id: ProjectAdoptionRequestId,
    expected_predecessor: &ProjectMaterializationGenerationIdentity,
) -> Result<ProjectAdoptionDecision, ProjectAdoptionError> {
    let input_digest = adoption_input_digest(catalog.identity(), candidate)?;
    if let Some(record) = current
        .adoption_records
        .iter()
        .find(|record| record.request_id == request_id)
    {
        return if record.input_digest == input_digest {
            Ok(ProjectAdoptionDecision::Replay {
                record: record.clone(),
            })
        } else {
            Err(changed_input_conflict())
        };
    }

    if current.identity != *expected_predecessor {
        return Err(stale_generation());
    }
    if current.catalog_identity != *catalog.identity() {
        return Err(catalog_identity_mismatch());
    }

    if let Some(existing) = current
        .materializations
        .iter()
        .find(|materialization| materialization.project == candidate.project)
    {
        return if existing.materialization_id == candidate.materialization_id {
            Ok(ProjectAdoptionDecision::Satisfied {
                materialization: existing.clone(),
            })
        } else {
            Err(duplicate_project_materialization())
        };
    }
    if current.materializations.iter().any(|materialization| {
        materialization.materialization_id == candidate.materialization_id
            && materialization.project != candidate.project
    }) {
        return Err(materialization_identity_conflict());
    }
    if current.materializations.len() >= MAX_PROJECT_MATERIALIZATIONS {
        return Err(materialization_limit_reached());
    }
    if current.adoption_records.len() >= MAX_PROJECT_ADOPTION_RECORDS {
        return Err(adoption_record_limit_reached());
    }
    let accepted_generation = current
        .identity
        .number
        .checked_add(1)
        .ok_or_else(generation_exhausted)?;

    let materialization = AcceptedProjectMaterialization {
        project: candidate.project.clone(),
        class: ProjectMaterializationClass::AdoptedMac,
        materialization_id: candidate.materialization_id.clone(),
        observation_digest: candidate.observation_digest.clone(),
        observed_commit: candidate.observed_commit.clone(),
        observed_tree: candidate.observed_tree.clone(),
        recovery: candidate.recovery.clone(),
        first_request_id: request_id.clone(),
        accepted_generation,
    };
    let operation_record = ProjectAdoptionOperationRecord::new(
        request_id,
        input_digest.clone(),
        candidate.project.clone(),
        candidate.materialization_id.clone(),
        accepted_generation,
    )?;

    let mut materializations = current.materializations.clone();
    materializations.push(materialization);
    materializations.sort_by(|left, right| {
        left.project
            .cmp(&right.project)
            .then(left.materialization_id.cmp(&right.materialization_id))
    });
    let mut adoption_records = current.adoption_records.clone();
    adoption_records.push(operation_record.clone());
    adoption_records.sort_by(|left, right| left.request_id.cmp(&right.request_id));

    let successor = build_generation(
        accepted_generation,
        Some(current.identity.clone()),
        current.catalog_identity.clone(),
        materializations,
        adoption_records,
    )?;
    Ok(ProjectAdoptionDecision::Apply {
        plan: Box::new(ProjectAdoptionPlan {
            predecessor: current.identity.clone(),
            input_digest,
            operation_record,
            successor,
        }),
    })
}

impl ProjectAdoptionOperationRecord {
    fn new(
        request_id: ProjectAdoptionRequestId,
        input_digest: Sha256Digest,
        project: ProjectIdentity,
        materialization_id: Sha256Digest,
        accepted_generation: u64,
    ) -> Result<Self, ProjectAdoptionError> {
        let canonical = ProjectAdoptionRecordDigestDocument {
            request_id: request_id.clone(),
            input_digest: input_digest.clone(),
            project: project.clone(),
            materialization_id: materialization_id.clone(),
            accepted_generation,
        };
        let bytes = serde_json::to_vec(&canonical).map_err(|_| identity_encoding_failed())?;
        let record_digest = domain_digest(ADOPTION_RECORD_DIGEST_DOMAIN, &bytes)?;
        Ok(Self {
            request_id,
            input_digest,
            project,
            materialization_id,
            accepted_generation,
            record_digest,
        })
    }
}

#[derive(Debug, Deserialize)]
struct SerializedCheckoutIdentity {
    commit: String,
    tree: String,
}

#[derive(Serialize)]
struct ProjectAdoptionInputDigestDocument<'a> {
    catalog_identity: &'a ProjectCatalogIdentity,
    class: ProjectMaterializationClass,
    project: &'a ProjectIdentity,
    materialization_id: &'a Sha256Digest,
    observation_digest: &'a Sha256Digest,
    observed_commit: &'a CommitId,
    observed_tree: &'a GitTreeId,
    recovery: &'a ProjectRecoveryRisk,
}

fn adoption_input_digest(
    catalog_identity: &ProjectCatalogIdentity,
    candidate: &ProjectAdoptionCandidate,
) -> Result<Sha256Digest, ProjectAdoptionError> {
    let document = ProjectAdoptionInputDigestDocument {
        catalog_identity,
        class: ProjectMaterializationClass::AdoptedMac,
        project: &candidate.project,
        materialization_id: &candidate.materialization_id,
        observation_digest: &candidate.observation_digest,
        observed_commit: &candidate.observed_commit,
        observed_tree: &candidate.observed_tree,
        recovery: &candidate.recovery,
    };
    let bytes = serde_json::to_vec(&document).map_err(|_| identity_encoding_failed())?;
    domain_digest(ADOPTION_INPUT_DIGEST_DOMAIN, &bytes)
}

#[derive(Serialize)]
struct ProjectAdoptionRecordDigestDocument {
    request_id: ProjectAdoptionRequestId,
    input_digest: Sha256Digest,
    project: ProjectIdentity,
    materialization_id: Sha256Digest,
    accepted_generation: u64,
}

#[derive(Serialize)]
struct ProjectMaterializationGenerationDigestDocument<'a> {
    schema_version: u8,
    number: u64,
    predecessor: &'a Option<ProjectMaterializationGenerationIdentity>,
    catalog_identity: &'a ProjectCatalogIdentity,
    materializations: &'a [AcceptedProjectMaterialization],
    adoption_records: &'a [ProjectAdoptionOperationRecord],
}

fn build_generation(
    number: u64,
    predecessor: Option<ProjectMaterializationGenerationIdentity>,
    catalog_identity: ProjectCatalogIdentity,
    mut materializations: Vec<AcceptedProjectMaterialization>,
    mut adoption_records: Vec<ProjectAdoptionOperationRecord>,
) -> Result<ProjectMaterializationGeneration, ProjectAdoptionError> {
    materializations.sort_by(|left, right| {
        left.project
            .cmp(&right.project)
            .then(left.materialization_id.cmp(&right.materialization_id))
    });
    adoption_records.sort_by(|left, right| left.request_id.cmp(&right.request_id));
    let document = ProjectMaterializationGenerationDigestDocument {
        schema_version: PROJECT_MATERIALIZATION_GENERATION_SCHEMA_VERSION,
        number,
        predecessor: &predecessor,
        catalog_identity: &catalog_identity,
        materializations: &materializations,
        adoption_records: &adoption_records,
    };
    let bytes = serde_json::to_vec(&document).map_err(|_| identity_encoding_failed())?;
    let digest = domain_digest(GENERATION_DIGEST_DOMAIN, &bytes)?;
    Ok(ProjectMaterializationGeneration {
        schema_version: PROJECT_MATERIALIZATION_GENERATION_SCHEMA_VERSION,
        identity: ProjectMaterializationGenerationIdentity { number, digest },
        predecessor,
        catalog_identity,
        materializations,
        adoption_records,
    })
}

fn domain_digest(domain: &[u8], bytes: &[u8]) -> Result<Sha256Digest, ProjectAdoptionError> {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut value = String::with_capacity(SHA256_PREFIX.len() + digest.len() * 2);
    value.push_str(SHA256_PREFIX);
    for byte in digest {
        value.push(char::from(HEX[usize::from(byte >> 4)]));
        value.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Sha256Digest::parse(&value).map_err(|_| identity_encoding_failed())
}

fn project_from_match(
    project_match: Option<&ProjectDiscoveryMatch>,
) -> Result<&ProjectIdentity, ProjectAdoptionError> {
    match project_match {
        Some(ProjectDiscoveryMatch::Catalogued { project }) => Ok(project),
        Some(ProjectDiscoveryMatch::Uncatalogued { .. }) => Err(catalog_project_required()),
        Some(ProjectDiscoveryMatch::AmbiguousCatalog { .. }) => Err(ambiguous_catalog()),
        Some(ProjectDiscoveryMatch::AmbiguousSource | ProjectDiscoveryMatch::NoCanonicalSource)
        | None => Err(ambiguous_source()),
    }
}

fn error(
    kind: ProjectAdoptionErrorKind,
    code: &'static str,
    problem: &'static str,
) -> ProjectAdoptionError {
    ProjectAdoptionError {
        kind,
        code,
        problem,
    }
}

fn invalid_request_id() -> ProjectAdoptionError {
    error(
        ProjectAdoptionErrorKind::InvalidRequestId,
        "invalid_request_id",
        "project adoption request identity is invalid",
    )
}

fn discovery_not_checkout() -> ProjectAdoptionError {
    error(
        ProjectAdoptionErrorKind::DiscoveryNotCheckout,
        "discovery_not_checkout",
        "project adoption requires one successful checkout discovery entry",
    )
}

fn catalog_project_required() -> ProjectAdoptionError {
    error(
        ProjectAdoptionErrorKind::CatalogProjectRequired,
        "catalog_project_required",
        "project adoption requires a project already declared in the logical catalog",
    )
}

fn ambiguous_source() -> ProjectAdoptionError {
    error(
        ProjectAdoptionErrorKind::AmbiguousSource,
        "ambiguous_source",
        "project adoption source identity is ambiguous",
    )
}

fn ambiguous_catalog() -> ProjectAdoptionError {
    error(
        ProjectAdoptionErrorKind::AmbiguousCatalog,
        "ambiguous_catalog",
        "project adoption matches several catalogued projects",
    )
}

fn discovery_incomplete() -> ProjectAdoptionError {
    error(
        ProjectAdoptionErrorKind::DiscoveryIncomplete,
        "discovery_incomplete",
        "project discovery evidence is incomplete for adoption",
    )
}

fn changed_input_conflict() -> ProjectAdoptionError {
    error(
        ProjectAdoptionErrorKind::ChangedInputConflict,
        "changed_input_conflict",
        "project adoption request identity was reused with changed input",
    )
}

fn stale_generation() -> ProjectAdoptionError {
    error(
        ProjectAdoptionErrorKind::StaleGeneration,
        "stale_generation",
        "project materialization generation changed and must be replanned",
    )
}

fn catalog_identity_mismatch() -> ProjectAdoptionError {
    error(
        ProjectAdoptionErrorKind::CatalogIdentityMismatch,
        "catalog_identity_mismatch",
        "accepted materialization generation belongs to another project catalog identity",
    )
}

fn duplicate_project_materialization() -> ProjectAdoptionError {
    error(
        ProjectAdoptionErrorKind::DuplicateProjectMaterialization,
        "duplicate_project_materialization",
        "project already has another accepted materialization",
    )
}

fn materialization_identity_conflict() -> ProjectAdoptionError {
    error(
        ProjectAdoptionErrorKind::MaterializationIdentityConflict,
        "materialization_identity_conflict",
        "materialization identity is already accepted for another project",
    )
}

fn generation_exhausted() -> ProjectAdoptionError {
    error(
        ProjectAdoptionErrorKind::GenerationExhausted,
        "generation_exhausted",
        "project materialization generation number is exhausted",
    )
}

fn materialization_limit_reached() -> ProjectAdoptionError {
    error(
        ProjectAdoptionErrorKind::MaterializationLimitReached,
        "materialization_limit_reached",
        "project materialization generation reached its bounded materialization count",
    )
}

fn adoption_record_limit_reached() -> ProjectAdoptionError {
    error(
        ProjectAdoptionErrorKind::AdoptionRecordLimitReached,
        "adoption_record_limit_reached",
        "project materialization generation reached its bounded adoption record count",
    )
}

fn identity_encoding_failed() -> ProjectAdoptionError {
    error(
        ProjectAdoptionErrorKind::IdentityEncodingFailed,
        "identity_encoding_failed",
        "project adoption canonical identity could not be encoded",
    )
}

#[cfg(test)]
mod tests {
    use crate::artifact::{CommitId, GitTreeId, Sha256Digest};
    use crate::project_catalog::{ProjectCatalog, ProjectIdentity};
    use crate::project_discovery::{
        ProjectDiscoveryEntryKind, ProjectDiscoveryMatch, ProjectRecoveryRisk,
    };

    use super::{
        MAX_PROJECT_ADOPTION_RECORDS, MAX_PROJECT_MATERIALIZATIONS, AcceptedProjectMaterialization,
        ProjectAdoptionCandidate, ProjectAdoptionDecision, ProjectAdoptionErrorKind,
        ProjectAdoptionOperationRecord, ProjectAdoptionRequestId, ProjectMaterializationClass,
        ProjectMaterializationGeneration, build_generation, plan_project_adoption_candidate,
        project_from_match,
    };

    const COMMIT: &str = "1111111111111111111111111111111111111111";
    const TREE: &str = "2222222222222222222222222222222222222222";

    fn digest(byte: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", byte.to_string().repeat(64))).expect("digest")
    }

    fn request(value: &str) -> ProjectAdoptionRequestId {
        ProjectAdoptionRequestId::parse(value).expect("request id")
    }

    fn catalog() -> ProjectCatalog {
        ProjectCatalog::decode_yaml(
            br#"version: 1
projects:
  - id: github.com/example/alpha
    aliases: [alpha]
    source: https://github.com/example/alpha.git
    materialization: developer
    restore: eager
  - id: github.com/example/beta
    aliases: [beta]
    source: https://github.com/example/beta.git
    materialization: developer
    restore: lazy
"#,
        )
        .expect("catalog")
    }

    fn candidate(project: &str, materialization: char, observation: char) -> ProjectAdoptionCandidate {
        ProjectAdoptionCandidate {
            project: ProjectIdentity::parse(project).expect("project"),
            materialization_id: digest(materialization),
            observation_digest: digest(observation),
            observed_commit: CommitId::parse(COMMIT).expect("commit"),
            observed_tree: GitTreeId::parse(TREE).expect("tree"),
            recovery: ProjectRecoveryRisk {
                tracked_changes_present: false,
                untracked_entry_count: 0,
                upstream_missing: false,
                local_commits_ahead: Some(0),
                source_ambiguous: false,
                multiple_worktrees: false,
                submodules_present: false,
                owner_mismatch: false,
                duplicate_materialization: false,
            },
        }
    }

    fn apply(
        current: &ProjectMaterializationGeneration,
        catalog: &ProjectCatalog,
        candidate: &ProjectAdoptionCandidate,
        request_id: &str,
    ) -> ProjectMaterializationGeneration {
        let decision = plan_project_adoption_candidate(
            current,
            catalog,
            candidate,
            request(request_id),
            current.identity(),
        )
        .expect("adoption plan");
        match decision {
            ProjectAdoptionDecision::Apply { plan } => plan.successor,
            other => panic!("expected apply, got {other:?}"),
        }
    }

    #[test]
    fn initial_and_successor_generations_are_deterministic() {
        let catalog = catalog();
        let initial = ProjectMaterializationGeneration::initial(catalog.identity().clone())
            .expect("initial generation");
        let repeated = ProjectMaterializationGeneration::initial(catalog.identity().clone())
            .expect("repeated initial");
        assert_eq!(initial, repeated);
        assert_eq!(initial.identity().number, 0);
        assert!(initial.materializations().is_empty());

        let alpha = candidate("github.com/example/alpha", 'a', 'b');
        let first = plan_project_adoption_candidate(
            &initial,
            &catalog,
            &alpha,
            request("adopt-alpha"),
            initial.identity(),
        )
        .expect("first plan");
        let second = plan_project_adoption_candidate(
            &initial,
            &catalog,
            &alpha,
            request("adopt-alpha"),
            initial.identity(),
        )
        .expect("repeated plan");
        assert_eq!(first, second);
        let ProjectAdoptionDecision::Apply { plan } = first else {
            panic!("expected apply")
        };
        assert_eq!(plan.successor.identity().number, 1);
        assert_eq!(plan.successor.materializations().len(), 1);
        assert_eq!(plan.successor.adoption_records().len(), 1);
        assert_eq!(plan.successor.predecessor(), Some(initial.identity()));
    }

    #[test]
    fn replay_precedes_stale_generation_and_changed_input_conflicts() {
        let catalog = catalog();
        let initial = ProjectMaterializationGeneration::initial(catalog.identity().clone())
            .expect("initial generation");
        let alpha = candidate("github.com/example/alpha", 'a', 'b');
        let generation_one = apply(&initial, &catalog, &alpha, "request-one");
        let beta = candidate("github.com/example/beta", 'c', 'd');
        let generation_two = apply(&generation_one, &catalog, &beta, "request-two");

        let replay = plan_project_adoption_candidate(
            &generation_two,
            &catalog,
            &alpha,
            request("request-one"),
            initial.identity(),
        )
        .expect("replay despite stale expected predecessor");
        assert!(matches!(replay, ProjectAdoptionDecision::Replay { .. }));

        let changed = candidate("github.com/example/alpha", 'a', 'e');
        let error = plan_project_adoption_candidate(
            &generation_two,
            &catalog,
            &changed,
            request("request-one"),
            generation_two.identity(),
        )
        .expect_err("changed request replay must conflict");
        assert_eq!(error.kind, ProjectAdoptionErrorKind::ChangedInputConflict);
    }

    #[test]
    fn stale_catalog_duplicate_and_identity_conflicts_are_distinct() {
        let catalog = catalog();
        let initial = ProjectMaterializationGeneration::initial(catalog.identity().clone())
            .expect("initial generation");
        let alpha = candidate("github.com/example/alpha", 'a', 'b');
        let generation_one = apply(&initial, &catalog, &alpha, "request-one");

        let error = plan_project_adoption_candidate(
            &generation_one,
            &catalog,
            &candidate("github.com/example/beta", 'c', 'd'),
            request("stale"),
            initial.identity(),
        )
        .expect_err("stale generation");
        assert_eq!(error.kind, ProjectAdoptionErrorKind::StaleGeneration);

        let another_catalog = ProjectCatalog::decode_yaml(
            br#"version: 1
projects:
  - id: github.com/example/alpha
    aliases: [alpha]
    source: https://github.com/example/alpha.git
    materialization: developer
    restore: eager
"#,
        )
        .expect("other catalog");
        let error = plan_project_adoption_candidate(
            &generation_one,
            &another_catalog,
            &candidate("github.com/example/alpha", 'a', 'b'),
            request("catalog-mismatch"),
            generation_one.identity(),
        )
        .expect_err("catalog mismatch");
        assert_eq!(error.kind, ProjectAdoptionErrorKind::CatalogIdentityMismatch);

        let error = plan_project_adoption_candidate(
            &generation_one,
            &catalog,
            &candidate("github.com/example/alpha", 'c', 'd'),
            request("duplicate-alpha"),
            generation_one.identity(),
        )
        .expect_err("duplicate project");
        assert_eq!(
            error.kind,
            ProjectAdoptionErrorKind::DuplicateProjectMaterialization
        );

        let error = plan_project_adoption_candidate(
            &generation_one,
            &catalog,
            &candidate("github.com/example/beta", 'a', 'd'),
            request("identity-conflict"),
            generation_one.identity(),
        )
        .expect_err("materialization id conflict");
        assert_eq!(
            error.kind,
            ProjectAdoptionErrorKind::MaterializationIdentityConflict
        );
    }

    #[test]
    fn same_project_and_materialization_is_already_satisfied() {
        let catalog = catalog();
        let initial = ProjectMaterializationGeneration::initial(catalog.identity().clone())
            .expect("initial generation");
        let alpha = candidate("github.com/example/alpha", 'a', 'b');
        let generation_one = apply(&initial, &catalog, &alpha, "request-one");
        let changed_observation = candidate("github.com/example/alpha", 'a', 'c');
        let decision = plan_project_adoption_candidate(
            &generation_one,
            &catalog,
            &changed_observation,
            request("another-request"),
            generation_one.identity(),
        )
        .expect("already accepted materialization");
        assert!(matches!(
            decision,
            ProjectAdoptionDecision::Satisfied { .. }
        ));
    }

    #[test]
    fn discovery_match_classes_are_fail_closed() {
        let project = ProjectIdentity::parse("github.com/example/alpha").expect("project");
        assert_eq!(
            project_from_match(Some(&ProjectDiscoveryMatch::Catalogued {
                project: project.clone()
            }))
            .expect("catalogued project"),
            &project
        );
        let cases = [
            (
                ProjectDiscoveryMatch::Uncatalogued {
                    project: project.clone(),
                },
                ProjectAdoptionErrorKind::CatalogProjectRequired,
            ),
            (
                ProjectDiscoveryMatch::AmbiguousCatalog {
                    projects: vec![project.clone()],
                },
                ProjectAdoptionErrorKind::AmbiguousCatalog,
            ),
            (
                ProjectDiscoveryMatch::AmbiguousSource,
                ProjectAdoptionErrorKind::AmbiguousSource,
            ),
            (
                ProjectDiscoveryMatch::NoCanonicalSource,
                ProjectAdoptionErrorKind::AmbiguousSource,
            ),
        ];
        for (project_match, expected) in cases {
            assert_eq!(
                project_from_match(Some(&project_match))
                    .expect_err("disallowed discovery match")
                    .kind,
                expected
            );
        }
        let _ = ProjectDiscoveryEntryKind::NonGitDirectory;
    }

    #[test]
    fn bounds_and_generation_exhaustion_block_before_successor() {
        let catalog = catalog();
        let alpha = candidate("github.com/example/alpha", 'a', 'b');
        let materialization = AcceptedProjectMaterialization {
            project: alpha.project.clone(),
            class: ProjectMaterializationClass::AdoptedMac,
            materialization_id: alpha.materialization_id.clone(),
            observation_digest: alpha.observation_digest.clone(),
            observed_commit: alpha.observed_commit.clone(),
            observed_tree: alpha.observed_tree.clone(),
            recovery: alpha.recovery.clone(),
            first_request_id: request("seed"),
            accepted_generation: 1,
        };
        let seed_record = ProjectAdoptionOperationRecord::new(
            request("seed"),
            digest('f'),
            alpha.project.clone(),
            alpha.materialization_id.clone(),
            1,
        )
        .expect("seed record");

        let full_materializations = vec![materialization; MAX_PROJECT_MATERIALIZATIONS];
        let full = build_generation(
            10,
            None,
            catalog.identity().clone(),
            full_materializations,
            Vec::new(),
        )
        .expect("full generation");
        let error = plan_project_adoption_candidate(
            &full,
            &catalog,
            &candidate("github.com/example/beta", 'c', 'd'),
            request("over-materialization-limit"),
            full.identity(),
        )
        .expect_err("materialization bound");
        assert_eq!(
            error.kind,
            ProjectAdoptionErrorKind::MaterializationLimitReached
        );

        let records = vec![seed_record; MAX_PROJECT_ADOPTION_RECORDS];
        let record_full = build_generation(
            10,
            None,
            catalog.identity().clone(),
            Vec::new(),
            records,
        )
        .expect("record-full generation");
        let error = plan_project_adoption_candidate(
            &record_full,
            &catalog,
            &candidate("github.com/example/beta", 'c', 'd'),
            request("over-record-limit"),
            record_full.identity(),
        )
        .expect_err("record bound");
        assert_eq!(
            error.kind,
            ProjectAdoptionErrorKind::AdoptionRecordLimitReached
        );

        let exhausted = build_generation(
            u64::MAX,
            None,
            catalog.identity().clone(),
            Vec::new(),
            Vec::new(),
        )
        .expect("exhausted generation");
        let error = plan_project_adoption_candidate(
            &exhausted,
            &catalog,
            &candidate("github.com/example/beta", 'c', 'd'),
            request("generation-overflow"),
            exhausted.identity(),
        )
        .expect_err("generation overflow");
        assert_eq!(error.kind, ProjectAdoptionErrorKind::GenerationExhausted);
    }

    #[test]
    fn public_generation_has_no_private_workspace_vocabulary() {
        let catalog = catalog();
        let initial = ProjectMaterializationGeneration::initial(catalog.identity().clone())
            .expect("initial generation");
        let successor = apply(
            &initial,
            &catalog,
            &candidate("github.com/example/alpha", 'a', 'b'),
            "privacy-check",
        );
        let json = serde_json::to_string(&successor).expect("public generation");
        for private_marker in [
            "/Users/",
            "/private/",
            "secret.txt",
            "remote.origin.url",
            "https://github.com/",
            "GIT_ASKPASS",
            "HOME=",
        ] {
            assert!(!json.contains(private_marker), "leaked {private_marker}");
        }
    }
}
