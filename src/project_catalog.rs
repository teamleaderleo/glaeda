use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;

pub const PROJECT_CATALOG_SCHEMA_VERSION: u8 = 1;
pub const MAX_PROJECT_CATALOG_DOCUMENT_BYTES: usize = 128 * 1024;
pub const MAX_PROJECTS: usize = 512;
pub const MAX_ALIASES_PER_PROJECT: usize = 16;
pub const MAX_PROJECT_ID_BYTES: usize = 256;
pub const MAX_ALIAS_BYTES: usize = 64;
pub const MAX_SOURCE_BYTES: usize = 512;

const SHA256_PREFIX: &str = "sha256:";
const HEX: &[u8; 16] = b"0123456789abcdef";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ProjectIdentity(String);

impl ProjectIdentity {
    /// Parse one canonical lowercase GitHub project identity.
    ///
    /// # Errors
    ///
    /// Returns a bounded error unless the value has exact `github.com/owner/repository` form.
    pub fn parse(value: &str) -> Result<Self, ProjectCatalogError> {
        if value.is_empty() || value.len() > MAX_PROJECT_ID_BYTES || !value.is_ascii() {
            return Err(ProjectCatalogError::new(
                "project.id",
                "invalid_project_identity",
                "project identity must be bounded canonical ASCII",
            ));
        }
        if value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
        {
            return Err(ProjectCatalogError::new(
                "project.id",
                "invalid_project_identity",
                "project identity must not contain whitespace or control characters",
            ));
        }
        let mut parts = value.split('/');
        let host = parts.next().unwrap_or_default();
        let owner = parts.next().unwrap_or_default();
        let repository = parts.next().unwrap_or_default();
        if parts.next().is_some()
            || host != "github.com"
            || !valid_owner(owner)
            || !valid_repository(repository)
            || repository.ends_with(".git")
        {
            return Err(ProjectCatalogError::new(
                "project.id",
                "invalid_project_identity",
                "project identity must use canonical github.com/owner/repository form",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ProjectAlias(String);

impl ProjectAlias {
    /// Parse one short ergonomic project alias.
    ///
    /// # Errors
    ///
    /// Returns a bounded error unless the alias uses lowercase ASCII letters, digits, `.`, `_`, or
    /// `-`, begins with a letter or digit, and contains no path separator.
    pub fn parse(value: &str) -> Result<Self, ProjectCatalogError> {
        let mut bytes = value.bytes();
        let Some(first) = bytes.next() else {
            return Err(invalid_alias_error());
        };
        if value.len() > MAX_ALIAS_BYTES
            || (!first.is_ascii_lowercase() && !first.is_ascii_digit())
        {
            return Err(invalid_alias_error());
        }
        if !bytes.all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'_' | b'-')
        }) {
            return Err(invalid_alias_error());
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitHubProjectSource {
    remote: String,
    project: ProjectIdentity,
}

impl GitHubProjectSource {
    /// Parse one reviewed GitHub HTTPS or SSH/scp-style remote and normalize it.
    ///
    /// # Errors
    ///
    /// Returns a bounded error for another host, embedded credentials, extra path components,
    /// whitespace/control characters, or invalid repository identity.
    pub fn parse(value: &str) -> Result<Self, ProjectCatalogError> {
        if value.is_empty()
            || value.len() > MAX_SOURCE_BYTES
            || !value.is_ascii()
            || value
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
        {
            return Err(invalid_source_error());
        }

        let path = value
            .strip_prefix("https://github.com/")
            .or_else(|| value.strip_prefix("git@github.com:"))
            .or_else(|| value.strip_prefix("ssh://git@github.com/"))
            .ok_or_else(invalid_source_error)?;
        if path
            .bytes()
            .any(|byte| matches!(byte, b'?' | b'#' | b':'))
            || path.starts_with('/')
            || path.ends_with('/')
        {
            return Err(invalid_source_error());
        }
        let path = path.strip_suffix(".git").unwrap_or(path);
        let mut parts = path.split('/');
        let owner = parts.next().unwrap_or_default().to_ascii_lowercase();
        let repository = parts.next().unwrap_or_default().to_ascii_lowercase();
        if parts.next().is_some() || !valid_owner(&owner) || !valid_repository(&repository) {
            return Err(invalid_source_error());
        }
        let project = ProjectIdentity::parse(&format!("github.com/{owner}/{repository}"))?;
        Ok(Self {
            remote: format!("https://github.com/{owner}/{repository}.git"),
            project,
        })
    }

    #[must_use]
    pub fn canonical_remote(&self) -> &str {
        &self.remote
    }

    #[must_use]
    pub const fn project(&self) -> &ProjectIdentity {
        &self.project
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestorePolicy {
    Eager,
    Lazy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesiredMaterializationClass {
    Developer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectCatalogEntry {
    id: ProjectIdentity,
    aliases: Vec<ProjectAlias>,
    source: GitHubProjectSource,
    materialization: DesiredMaterializationClass,
    restore: RestorePolicy,
}

impl ProjectCatalogEntry {
    #[must_use]
    pub const fn id(&self) -> &ProjectIdentity {
        &self.id
    }

    #[must_use]
    pub fn aliases(&self) -> &[ProjectAlias] {
        &self.aliases
    }

    #[must_use]
    pub const fn source(&self) -> &GitHubProjectSource {
        &self.source
    }

    #[must_use]
    pub const fn materialization(&self) -> DesiredMaterializationClass {
        self.materialization
    }

    #[must_use]
    pub const fn restore(&self) -> RestorePolicy {
        self.restore
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectCatalogIdentity {
    schema_version: u8,
    digest: Sha256Digest,
}

impl ProjectCatalogIdentity {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn digest(&self) -> &Sha256Digest {
        &self.digest
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectCatalog {
    schema_version: u8,
    identity: ProjectCatalogIdentity,
    projects: Vec<ProjectCatalogEntry>,
}

impl ProjectCatalog {
    /// Decode one bounded strict YAML project catalog without performing I/O.
    ///
    /// # Errors
    ///
    /// Returns a bounded typed error for malformed YAML, unsupported versions, unknown fields,
    /// invalid identities, duplicate projects or aliases, or exceeded collection bounds.
    pub fn decode_yaml(bytes: &[u8]) -> Result<Self, ProjectCatalogError> {
        if bytes.is_empty() {
            return Err(ProjectCatalogError::new(
                "document",
                "empty_document",
                "project catalog document must not be empty",
            ));
        }
        if bytes.len() > MAX_PROJECT_CATALOG_DOCUMENT_BYTES {
            return Err(ProjectCatalogError::new(
                "document",
                "document_too_large",
                "project catalog exceeds the bounded document size",
            ));
        }
        let document: ProjectCatalogDocument = serde_yaml::from_slice(bytes).map_err(|_| {
            ProjectCatalogError::new(
                "document",
                "invalid_document",
                "project catalog must be strict valid YAML",
            )
        })?;
        if document.version != PROJECT_CATALOG_SCHEMA_VERSION {
            return Err(ProjectCatalogError::new(
                "version",
                "unsupported_schema_version",
                "project catalog schema version is unsupported",
            ));
        }
        if document.projects.is_empty() {
            return Err(ProjectCatalogError::new(
                "projects",
                "empty_project_catalog",
                "project catalog must declare at least one project",
            ));
        }
        if document.projects.len() > MAX_PROJECTS {
            return Err(ProjectCatalogError::new(
                "projects",
                "too_many_projects",
                "project catalog exceeds the bounded project count",
            ));
        }

        let mut project_ids = BTreeSet::new();
        let mut alias_owners = BTreeMap::<ProjectAlias, ProjectIdentity>::new();
        let mut projects = Vec::with_capacity(document.projects.len());
        for raw in document.projects {
            let id = ProjectIdentity::parse(&raw.id)?;
            let source = GitHubProjectSource::parse(&raw.source)?;
            if source.project() != &id {
                return Err(ProjectCatalogError::new(
                    "project.source",
                    "source_project_mismatch",
                    "project source must resolve to the declared canonical project identity",
                ));
            }
            if !project_ids.insert(id.clone()) {
                return Err(ProjectCatalogError::new(
                    "project.id",
                    "duplicate_project",
                    "project catalog contains a duplicate canonical project identity",
                ));
            }
            if raw.aliases.len() > MAX_ALIASES_PER_PROJECT {
                return Err(ProjectCatalogError::new(
                    "project.aliases",
                    "too_many_aliases",
                    "project exceeds the bounded alias count",
                ));
            }
            let mut aliases = Vec::with_capacity(raw.aliases.len());
            for raw_alias in raw.aliases {
                let alias = ProjectAlias::parse(&raw_alias)?;
                if let Some(existing_project) = alias_owners.get(&alias) {
                    let code = if existing_project == &id {
                        "duplicate_alias"
                    } else {
                        "alias_conflict"
                    };
                    return Err(ProjectCatalogError::new(
                        "project.aliases",
                        code,
                        "project alias must identify exactly one catalog project",
                    ));
                }
                alias_owners.insert(alias.clone(), id.clone());
                aliases.push(alias);
            }
            aliases.sort();
            projects.push(ProjectCatalogEntry {
                id,
                aliases,
                source,
                materialization: raw.materialization,
                restore: raw.restore,
            });
        }

        let identity = digest_catalog(&projects)?;
        Ok(Self {
            schema_version: PROJECT_CATALOG_SCHEMA_VERSION,
            identity,
            projects,
        })
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn identity(&self) -> &ProjectCatalogIdentity {
        &self.identity
    }

    #[must_use]
    pub fn projects(&self) -> &[ProjectCatalogEntry] {
        &self.projects
    }

    /// Resolve one canonical project identity or ergonomic alias within the catalog.
    ///
    /// # Errors
    ///
    /// Returns a bounded unknown-project error when no exact catalog entry matches.
    pub fn resolve(&self, value: &str) -> Result<&ProjectCatalogEntry, ProjectCatalogError> {
        if value.starts_with("github.com/") {
            let id = ProjectIdentity::parse(value)?;
            return self
                .projects
                .iter()
                .find(|project| project.id() == &id)
                .ok_or_else(unknown_project_error);
        }
        let alias = ProjectAlias::parse(value)?;
        self.projects
            .iter()
            .find(|project| project.aliases.binary_search(&alias).is_ok())
            .ok_or_else(unknown_project_error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectCatalogError {
    pub field: &'static str,
    pub code: &'static str,
    pub problem: &'static str,
}

impl ProjectCatalogError {
    const fn new(field: &'static str, code: &'static str, problem: &'static str) -> Self {
        Self {
            field,
            code,
            problem,
        }
    }
}

impl fmt::Display for ProjectCatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.problem)
    }
}

impl std::error::Error for ProjectCatalogError {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectCatalogDocument {
    version: u8,
    projects: Vec<ProjectCatalogEntryDocument>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectCatalogEntryDocument {
    id: String,
    #[serde(default)]
    aliases: Vec<String>,
    source: String,
    materialization: DesiredMaterializationClass,
    restore: RestorePolicy,
}

#[derive(Debug, Serialize)]
struct ProjectCatalogDigestDocument {
    schema_version: u8,
    projects: Vec<ProjectCatalogEntry>,
}

fn digest_catalog(
    projects: &[ProjectCatalogEntry],
) -> Result<ProjectCatalogIdentity, ProjectCatalogError> {
    let mut normalized_projects = projects.to_vec();
    normalized_projects.sort_by(|left, right| left.id.cmp(&right.id));
    let bytes = serde_json::to_vec(&ProjectCatalogDigestDocument {
        schema_version: PROJECT_CATALOG_SCHEMA_VERSION,
        projects: normalized_projects,
    })
    .map_err(|_| {
        ProjectCatalogError::new(
            "document",
            "identity_encoding_failed",
            "project catalog identity could not be encoded",
        )
    })?;
    let digest = Sha256::digest(bytes);
    let mut value = String::with_capacity(SHA256_PREFIX.len() + digest.len() * 2);
    value.push_str(SHA256_PREFIX);
    for byte in digest {
        value.push(char::from(HEX[usize::from(byte >> 4)]));
        value.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    let digest = Sha256Digest::parse(&value).map_err(|_| {
        ProjectCatalogError::new(
            "document",
            "identity_encoding_failed",
            "project catalog identity could not be encoded",
        )
    })?;
    Ok(ProjectCatalogIdentity {
        schema_version: PROJECT_CATALOG_SCHEMA_VERSION,
        digest,
    })
}

fn valid_owner(value: &str) -> bool {
    if value.is_empty() || value.len() > 100 {
        return false;
    }
    let bytes = value.as_bytes();
    (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
        && bytes.last().is_some_and(|byte| *byte != b'-')
}

fn valid_repository(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value != "."
        && value != ".."
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'_' | b'-')
        })
}

fn invalid_alias_error() -> ProjectCatalogError {
    ProjectCatalogError::new(
        "project.aliases",
        "invalid_alias",
        "project alias must use bounded lowercase ASCII letters, digits, '.', '_', or '-'",
    )
}

fn invalid_source_error() -> ProjectCatalogError {
    ProjectCatalogError::new(
        "project.source",
        "invalid_source",
        "project source must be one reviewed GitHub HTTPS or SSH remote",
    )
}

fn unknown_project_error() -> ProjectCatalogError {
    ProjectCatalogError::new(
        "project",
        "unknown_project",
        "project request does not match the accepted catalog",
    )
}

#[cfg(test)]
mod tests {
    use super::{
        GitHubProjectSource, MAX_ALIAS_BYTES, MAX_ALIASES_PER_PROJECT,
        MAX_PROJECT_CATALOG_DOCUMENT_BYTES, MAX_PROJECTS, ProjectAlias, ProjectCatalog,
        ProjectIdentity,
    };

    fn catalog_yaml() -> &'static [u8] {
        br#"version: 1
projects:
  - id: github.com/teamleaderleo/smolrunner
    aliases: [smolrunner, sr]
    source: git@github.com:TeamLeaderLeo/SmolRunner.git
    materialization: developer
    restore: eager
  - id: github.com/openai/codex
    aliases: [codex]
    source: https://github.com/OpenAI/Codex
    materialization: developer
    restore: lazy
"#
    }

    #[test]
    fn source_normalizes_reviewed_github_forms() {
        for source in [
            "https://github.com/TeamLeaderLeo/SmolRunner",
            "https://github.com/TeamLeaderLeo/SmolRunner.git",
            "git@github.com:TeamLeaderLeo/SmolRunner.git",
            "ssh://git@github.com/TeamLeaderLeo/SmolRunner.git",
        ] {
            let parsed = GitHubProjectSource::parse(source).unwrap();
            assert_eq!(
                parsed.project().as_str(),
                "github.com/teamleaderleo/smolrunner"
            );
            assert_eq!(
                parsed.canonical_remote(),
                "https://github.com/teamleaderleo/smolrunner.git"
            );
        }
    }

    #[test]
    fn catalog_decodes_and_resolves_canonical_and_alias_names() {
        let catalog = ProjectCatalog::decode_yaml(catalog_yaml()).unwrap();
        assert_eq!(catalog.schema_version(), 1);
        assert_eq!(catalog.projects().len(), 2);
        assert_eq!(
            catalog.resolve("smolrunner").unwrap().id().as_str(),
            "github.com/teamleaderleo/smolrunner"
        );
        assert_eq!(
            catalog
                .resolve("github.com/openai/codex")
                .unwrap()
                .id()
                .as_str(),
            "github.com/openai/codex"
        );
    }

    #[test]
    fn catalog_identity_is_stable_across_project_and_alias_order() {
        let first = ProjectCatalog::decode_yaml(catalog_yaml()).unwrap();
        let reordered = ProjectCatalog::decode_yaml(
            br#"version: 1
projects:
  - id: github.com/openai/codex
    aliases: [codex]
    source: git@github.com:openai/codex.git
    materialization: developer
    restore: lazy
  - id: github.com/teamleaderleo/smolrunner
    aliases: [sr, smolrunner]
    source: https://github.com/teamleaderleo/smolrunner.git
    materialization: developer
    restore: eager
"#,
        )
        .unwrap();
        assert_eq!(first.identity(), reordered.identity());
    }

    #[test]
    fn strict_document_rejects_unknown_fields_and_versions() {
        let unknown = ProjectCatalog::decode_yaml(
            br#"version: 1
unexpected: true
projects:
  - id: github.com/openai/codex
    source: https://github.com/openai/codex.git
    materialization: developer
    restore: lazy
"#,
        )
        .unwrap_err();
        assert_eq!(unknown.code, "invalid_document");

        let version = ProjectCatalog::decode_yaml(
            br#"version: 2
projects:
  - id: github.com/openai/codex
    source: https://github.com/openai/codex.git
    materialization: developer
    restore: lazy
"#,
        )
        .unwrap_err();
        assert_eq!(version.code, "unsupported_schema_version");
    }

    #[test]
    fn catalog_rejects_project_source_mismatch_and_duplicates() {
        let mismatch = ProjectCatalog::decode_yaml(
            br#"version: 1
projects:
  - id: github.com/openai/codex
    source: https://github.com/openai/openai.git
    materialization: developer
    restore: lazy
"#,
        )
        .unwrap_err();
        assert_eq!(mismatch.code, "source_project_mismatch");

        let duplicate = ProjectCatalog::decode_yaml(
            br#"version: 1
projects:
  - id: github.com/openai/codex
    aliases: [codex]
    source: https://github.com/openai/codex.git
    materialization: developer
    restore: lazy
  - id: github.com/openai/codex
    aliases: [codex-two]
    source: git@github.com:openai/codex.git
    materialization: developer
    restore: lazy
"#,
        )
        .unwrap_err();
        assert_eq!(duplicate.code, "duplicate_project");
    }

    #[test]
    fn catalog_rejects_alias_conflicts_and_duplicates() {
        let conflict = ProjectCatalog::decode_yaml(
            br#"version: 1
projects:
  - id: github.com/openai/codex
    aliases: [work]
    source: https://github.com/openai/codex.git
    materialization: developer
    restore: lazy
  - id: github.com/teamleaderleo/smolrunner
    aliases: [work]
    source: https://github.com/teamleaderleo/smolrunner.git
    materialization: developer
    restore: lazy
"#,
        )
        .unwrap_err();
        assert_eq!(conflict.code, "alias_conflict");

        let duplicate = ProjectCatalog::decode_yaml(
            br#"version: 1
projects:
  - id: github.com/openai/codex
    aliases: [codex, codex]
    source: https://github.com/openai/codex.git
    materialization: developer
    restore: lazy
"#,
        )
        .unwrap_err();
        assert_eq!(duplicate.code, "duplicate_alias");
    }

    #[test]
    fn invalid_alias_identity_source_and_unknown_project_are_bounded() {
        assert_eq!(ProjectAlias::parse("Bad").unwrap_err().code, "invalid_alias");
        assert_eq!(
            ProjectAlias::parse(&"a".repeat(MAX_ALIAS_BYTES + 1))
                .unwrap_err()
                .code,
            "invalid_alias"
        );
        assert_eq!(
            ProjectIdentity::parse("github.com/OpenAI/codex")
                .unwrap_err()
                .code,
            "invalid_project_identity"
        );
        assert_eq!(
            GitHubProjectSource::parse("https://example.com/openai/codex.git")
                .unwrap_err()
                .code,
            "invalid_source"
        );
        assert_eq!(
            ProjectCatalog::decode_yaml(catalog_yaml())
                .unwrap()
                .resolve("missing")
                .unwrap_err()
                .code,
            "unknown_project"
        );
    }

    #[test]
    fn document_alias_and_project_collection_bounds_are_enforced() {
        let oversized = vec![b'x'; MAX_PROJECT_CATALOG_DOCUMENT_BYTES + 1];
        assert_eq!(
            ProjectCatalog::decode_yaml(&oversized).unwrap_err().code,
            "document_too_large"
        );

        let aliases = (0..=MAX_ALIASES_PER_PROJECT)
            .map(|index| format!("alias{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let yaml = format!(
            "version: 1\nprojects:\n  - id: github.com/openai/codex\n    aliases: [{aliases}]\n    source: https://github.com/openai/codex.git\n    materialization: developer\n    restore: lazy\n"
        );
        assert_eq!(
            ProjectCatalog::decode_yaml(yaml.as_bytes()).unwrap_err().code,
            "too_many_aliases"
        );

        let projects = (0..=MAX_PROJECTS)
            .map(|index| {
                format!(
                    "  - id: github.com/example/repo{index}\n    source: https://github.com/example/repo{index}.git\n    materialization: developer\n    restore: lazy\n"
                )
            })
            .collect::<String>();
        let yaml = format!("version: 1\nprojects:\n{projects}");
        assert_eq!(
            ProjectCatalog::decode_yaml(yaml.as_bytes()).unwrap_err().code,
            "too_many_projects"
        );
    }
}
