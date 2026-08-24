//! Pure sealed planning for the non-task Git producer phase of immutable pool publication.
//!
//! This module grants no process or filesystem authority. A later #592 guest-local transaction must
//! freshly verify the immutable source generation, the exact root-created empty staging candidate,
//! the reviewed empty Git template/config root, and the fixed `smolrunner-admin` account before it
//! can construct the private target used here. The later executor must then drop from root to the
//! verified admin UID/GID and clear supplementary groups before spawning the fixed Git command.

use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::artifact::Sha256Digest;
use crate::immutable_git_object_pool::GitObjectPoolBinding;
use crate::immutable_git_object_pool_marker::git_object_pool_binding_digest;
use crate::process::CommandSpec;

pub const IMMUTABLE_GIT_OBJECT_POOL_ADMIN_PRODUCER_PLAN_SCHEMA_VERSION: u8 = 1;
pub const IMMUTABLE_GIT_OBJECT_POOL_ADMIN_PRODUCER_TIMEOUT: Duration = Duration::from_secs(120);
pub const MAX_IMMUTABLE_GIT_OBJECT_POOL_ADMIN_PRODUCER_STDOUT_BYTES: usize = 64 * 1024;
pub const MAX_IMMUTABLE_GIT_OBJECT_POOL_ADMIN_PRODUCER_STDERR_BYTES: usize = 64 * 1024;

const GIT: &str = "/usr/bin/git";
const SAFE_PATH: &str = "/usr/bin:/bin";
const MAX_PRIVATE_PATH_BYTES: usize = 1_024;
const REDACTED_PATH: &str = "<private-verified-path>";
const REDACTED_ACCOUNT: &str = "<verified-nonroot-admin-account>";

#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ImmutableGitObjectPoolProducerSourceIdentity(Sha256Digest);

impl ImmutableGitObjectPoolProducerSourceIdentity {
    /// Construct one opaque logical digest from already verified immutable source-generation
    /// evidence. The later source observer/publication transaction owns that evidence boundary.
    #[allow(dead_code)]
    pub(crate) const fn from_verified(digest: Sha256Digest) -> Self {
        Self(digest)
    }

    #[must_use]
    pub const fn digest(&self) -> &Sha256Digest {
        &self.0
    }
}

impl fmt::Debug for ImmutableGitObjectPoolProducerSourceIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ImmutableGitObjectPoolProducerSourceIdentity")
            .field(&self.0)
            .finish()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ImmutableGitObjectPoolAdminProducerIdentity {
    uid: u32,
    gid: u32,
}

impl ImmutableGitObjectPoolAdminProducerIdentity {
    /// Bind one freshly observed non-root producer account.
    ///
    /// There is no public constructor. A later command-free account observer will mint this only
    /// for the fixed `smolrunner-admin` account in the accepted resident guest.
    #[allow(dead_code)]
    pub(crate) fn from_verified(
        uid: u32,
        gid: u32,
    ) -> Result<Self, ImmutableGitObjectPoolAdminProducerPlanError> {
        if uid == 0 || gid == 0 {
            return Err(invalid_admin());
        }
        Ok(Self { uid, gid })
    }

    #[allow(dead_code)] // Consumed by the next #592 executor slice.
    pub(crate) const fn uid(self) -> u32 {
        self.uid
    }

    #[allow(dead_code)] // Consumed by the next #592 executor slice.
    pub(crate) const fn gid(self) -> u32 {
        self.gid
    }
}

impl fmt::Debug for ImmutableGitObjectPoolAdminProducerIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(REDACTED_ACCOUNT)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImmutableGitObjectPoolAdminCredentialPolicy {
    VerifiedAdminPrimaryIdentityClearSupplementaryGroups,
}

/// Private verified locators and identities for one future producer invocation.
///
/// The exact source/candidate/template/config paths are locators only. The later root transaction
/// must retain and revalidate descriptor authority around process execution. This pure type performs
/// no I/O and has no public constructor.
pub struct ImmutableGitObjectPoolAdminProducerTarget {
    source_path: PathBuf,
    candidate_path: PathBuf,
    empty_template_path: PathBuf,
    config_root_path: PathBuf,
    source_identity: ImmutableGitObjectPoolProducerSourceIdentity,
    candidate_binding: GitObjectPoolBinding,
    admin: ImmutableGitObjectPoolAdminProducerIdentity,
}

impl ImmutableGitObjectPoolAdminProducerTarget {
    #[allow(dead_code, clippy::too_many_arguments)]
    pub(crate) fn from_verified(
        source_path: PathBuf,
        candidate_path: PathBuf,
        empty_template_path: PathBuf,
        config_root_path: PathBuf,
        source_identity: ImmutableGitObjectPoolProducerSourceIdentity,
        candidate_binding: GitObjectPoolBinding,
        admin: ImmutableGitObjectPoolAdminProducerIdentity,
    ) -> Result<Self, ImmutableGitObjectPoolAdminProducerPlanError> {
        let source_path = validate_private_absolute_path(source_path)?;
        let candidate_path = validate_private_absolute_path(candidate_path)?;
        let empty_template_path = validate_private_absolute_path(empty_template_path)?;
        let config_root_path = validate_private_absolute_path(config_root_path)?;
        require_private_roots_separate(
            &source_path,
            &candidate_path,
            &empty_template_path,
            &config_root_path,
        )?;
        Ok(Self {
            source_path,
            candidate_path,
            empty_template_path,
            config_root_path,
            source_identity,
            candidate_binding,
            admin,
        })
    }
}

impl fmt::Debug for ImmutableGitObjectPoolAdminProducerTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImmutableGitObjectPoolAdminProducerTarget")
            .field("source_path", &REDACTED_PATH)
            .field("candidate_path", &REDACTED_PATH)
            .field("empty_template_path", &REDACTED_PATH)
            .field("config_root_path", &REDACTED_PATH)
            .field("source_identity", &self.source_identity)
            .field("candidate_binding", &self.candidate_binding)
            .field("admin", &self.admin)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImmutableGitObjectPoolAdminProducerPlanSummary {
    schema_version: u8,
    source_identity_digest: Sha256Digest,
    candidate_binding_digest: Sha256Digest,
    credential_policy: ImmutableGitObjectPoolAdminCredentialPolicy,
    bare_clone: bool,
    local_source_only: bool,
    hardlinks_disabled: bool,
    reviewed_empty_template: bool,
    ambient_environment_cleared: bool,
    timeout_seconds: u64,
    stdout_limit_bytes: usize,
    stderr_limit_bytes: usize,
    argument_count: usize,
    environment_key_count: usize,
}

impl ImmutableGitObjectPoolAdminProducerPlanSummary {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn source_identity_digest(&self) -> &Sha256Digest {
        &self.source_identity_digest
    }

    #[must_use]
    pub const fn candidate_binding_digest(&self) -> &Sha256Digest {
        &self.candidate_binding_digest
    }

    #[must_use]
    pub const fn credential_policy(&self) -> ImmutableGitObjectPoolAdminCredentialPolicy {
        self.credential_policy
    }

    #[must_use]
    pub const fn bare_clone(&self) -> bool {
        self.bare_clone
    }

    #[must_use]
    pub const fn local_source_only(&self) -> bool {
        self.local_source_only
    }

    #[must_use]
    pub const fn hardlinks_disabled(&self) -> bool {
        self.hardlinks_disabled
    }

    #[must_use]
    pub const fn reviewed_empty_template(&self) -> bool {
        self.reviewed_empty_template
    }

    #[must_use]
    pub const fn ambient_environment_cleared(&self) -> bool {
        self.ambient_environment_cleared
    }

    #[must_use]
    pub const fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_seconds)
    }

    #[must_use]
    pub const fn stdout_limit_bytes(&self) -> usize {
        self.stdout_limit_bytes
    }

    #[must_use]
    pub const fn stderr_limit_bytes(&self) -> usize {
        self.stderr_limit_bytes
    }

    #[must_use]
    pub const fn argument_count(&self) -> usize {
        self.argument_count
    }

    #[must_use]
    pub const fn environment_key_count(&self) -> usize {
        self.environment_key_count
    }
}

pub struct ImmutableGitObjectPoolAdminProducerPlan {
    summary: ImmutableGitObjectPoolAdminProducerPlanSummary,
    command: CommandSpec,
    admin: ImmutableGitObjectPoolAdminProducerIdentity,
}

impl ImmutableGitObjectPoolAdminProducerPlan {
    #[must_use]
    pub const fn summary(&self) -> &ImmutableGitObjectPoolAdminProducerPlanSummary {
        &self.summary
    }

    #[allow(dead_code)]
    pub(crate) const fn command(&self) -> &CommandSpec {
        &self.command
    }

    #[allow(dead_code)]
    pub(crate) const fn admin(&self) -> ImmutableGitObjectPoolAdminProducerIdentity {
        self.admin
    }
}

impl fmt::Debug for ImmutableGitObjectPoolAdminProducerPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImmutableGitObjectPoolAdminProducerPlan")
            .field("summary", &self.summary)
            .field("command", &"<private-fixed-admin-git-command>")
            .field("admin", &REDACTED_ACCOUNT)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImmutableGitObjectPoolAdminProducerPlanErrorKind {
    InvalidAdmin,
    InvalidPath,
    ConflictingPrivateRoots,
    InvalidBinding,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct ImmutableGitObjectPoolAdminProducerPlanError {
    kind: ImmutableGitObjectPoolAdminProducerPlanErrorKind,
    code: &'static str,
    message: &'static str,
}

impl ImmutableGitObjectPoolAdminProducerPlanError {
    #[must_use]
    pub const fn kind(&self) -> ImmutableGitObjectPoolAdminProducerPlanErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for ImmutableGitObjectPoolAdminProducerPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImmutableGitObjectPoolAdminProducerPlanError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for ImmutableGitObjectPoolAdminProducerPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ImmutableGitObjectPoolAdminProducerPlanError {}

/// Seal the fixed V1 admin Git producer command from already verified private target evidence.
///
/// # Errors
///
/// Returns a bounded error only when the candidate binding digest cannot be represented. Path and
/// account validation occurs at the private target constructors.
pub fn plan_immutable_git_object_pool_admin_producer(
    target: &ImmutableGitObjectPoolAdminProducerTarget,
) -> Result<ImmutableGitObjectPoolAdminProducerPlan, ImmutableGitObjectPoolAdminProducerPlanError> {
    let source = private_utf8(&target.source_path)?;
    let candidate = private_utf8(&target.candidate_path)?;
    let template = private_utf8(&target.empty_template_path)?;
    let config_root = private_utf8(&target.config_root_path)?;
    let template_argument = format!("--template={template}");

    let command = CommandSpec::new(GIT)
        .argument("--no-optional-locks")
        .argument("-c")
        .argument("credential.helper=")
        .argument("-c")
        .argument("core.fsmonitor=false")
        .argument("-c")
        .argument("core.hooksPath=/dev/null")
        .argument("clone")
        .argument("--bare")
        .argument("--local")
        .argument("--no-hardlinks")
        .argument(template_argument)
        .argument(source)
        .argument(candidate)
        .environment("GIT_ASKPASS", "/bin/false")
        .environment("GIT_ATTR_NOSYSTEM", "1")
        .environment("GIT_CONFIG_GLOBAL", "/dev/null")
        .environment("GIT_CONFIG_NOSYSTEM", "1")
        .environment("GIT_TERMINAL_PROMPT", "0")
        .environment("HOME", config_root)
        .environment("LANG", "C")
        .environment("LC_ALL", "C")
        .environment("PATH", SAFE_PATH)
        .environment("XDG_CONFIG_HOME", config_root);

    let candidate_binding_digest =
        git_object_pool_binding_digest(&target.candidate_binding).map_err(|_| invalid_binding())?;
    let argument_count = command.arguments.len();
    let environment_key_count = command.environment.len();
    Ok(ImmutableGitObjectPoolAdminProducerPlan {
        summary: ImmutableGitObjectPoolAdminProducerPlanSummary {
            schema_version: IMMUTABLE_GIT_OBJECT_POOL_ADMIN_PRODUCER_PLAN_SCHEMA_VERSION,
            source_identity_digest: target.source_identity.digest().clone(),
            candidate_binding_digest,
            credential_policy:
                ImmutableGitObjectPoolAdminCredentialPolicy::VerifiedAdminPrimaryIdentityClearSupplementaryGroups,
            bare_clone: true,
            local_source_only: true,
            hardlinks_disabled: true,
            reviewed_empty_template: true,
            ambient_environment_cleared: true,
            timeout_seconds: IMMUTABLE_GIT_OBJECT_POOL_ADMIN_PRODUCER_TIMEOUT.as_secs(),
            stdout_limit_bytes: MAX_IMMUTABLE_GIT_OBJECT_POOL_ADMIN_PRODUCER_STDOUT_BYTES,
            stderr_limit_bytes: MAX_IMMUTABLE_GIT_OBJECT_POOL_ADMIN_PRODUCER_STDERR_BYTES,
            argument_count,
            environment_key_count,
        },
        command,
        admin: target.admin,
    })
}

fn validate_private_absolute_path(
    path: PathBuf,
) -> Result<PathBuf, ImmutableGitObjectPoolAdminProducerPlanError> {
    if !path.is_absolute()
        || path == Path::new("/")
        || path.as_os_str().as_encoded_bytes().len() > MAX_PRIVATE_PATH_BYTES
        || path.to_str().is_none()
        || path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(invalid_path());
    }
    Ok(path)
}

fn require_private_roots_separate(
    source: &Path,
    candidate: &Path,
    template: &Path,
    config_root: &Path,
) -> Result<(), ImmutableGitObjectPoolAdminProducerPlanError> {
    let paths = [source, candidate, template, config_root];
    for (index, left) in paths.iter().enumerate() {
        for right in paths.iter().skip(index + 1) {
            if left == right || left.starts_with(right) || right.starts_with(left) {
                return Err(conflicting_roots());
            }
        }
    }
    Ok(())
}

fn private_utf8(path: &Path) -> Result<&str, ImmutableGitObjectPoolAdminProducerPlanError> {
    path.to_str().ok_or_else(invalid_path)
}

const fn error(
    kind: ImmutableGitObjectPoolAdminProducerPlanErrorKind,
    code: &'static str,
    message: &'static str,
) -> ImmutableGitObjectPoolAdminProducerPlanError {
    ImmutableGitObjectPoolAdminProducerPlanError {
        kind,
        code,
        message,
    }
}

const fn invalid_admin() -> ImmutableGitObjectPoolAdminProducerPlanError {
    error(
        ImmutableGitObjectPoolAdminProducerPlanErrorKind::InvalidAdmin,
        "immutable_git_pool_admin_producer_invalid_admin",
        "immutable Git pool producer requires a verified non-root admin identity",
    )
}

const fn invalid_path() -> ImmutableGitObjectPoolAdminProducerPlanError {
    error(
        ImmutableGitObjectPoolAdminProducerPlanErrorKind::InvalidPath,
        "immutable_git_pool_admin_producer_invalid_path",
        "immutable Git pool producer private locator is invalid",
    )
}

const fn conflicting_roots() -> ImmutableGitObjectPoolAdminProducerPlanError {
    error(
        ImmutableGitObjectPoolAdminProducerPlanErrorKind::ConflictingPrivateRoots,
        "immutable_git_pool_admin_producer_conflicting_roots",
        "immutable Git pool producer private roots must be disjoint",
    )
}

const fn invalid_binding() -> ImmutableGitObjectPoolAdminProducerPlanError {
    error(
        ImmutableGitObjectPoolAdminProducerPlanErrorKind::InvalidBinding,
        "immutable_git_pool_admin_producer_invalid_binding",
        "immutable Git pool producer candidate binding is invalid",
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use super::{
        GIT, ImmutableGitObjectPoolAdminCredentialPolicy,
        ImmutableGitObjectPoolAdminProducerIdentity,
        ImmutableGitObjectPoolAdminProducerPlanErrorKind,
        ImmutableGitObjectPoolAdminProducerTarget, ImmutableGitObjectPoolProducerSourceIdentity,
        SAFE_PATH, plan_immutable_git_object_pool_admin_producer,
    };
    use crate::artifact::Sha256Digest;
    use crate::immutable_git_object_pool::{
        GitObjectFormat, GitObjectPoolBinding, GitObjectPoolGeneration, GitObjectPoolId,
        GitObjectPoolProducerGenerationId, GitObjectPoolTrustGenerationId,
    };
    use crate::immutable_git_object_pool_marker::git_object_pool_binding_digest;
    use crate::process::CommandValue;
    use crate::project_catalog::ProjectIdentity;
    use crate::project_disk_lease::{ProjectDiskGeneration, ProjectDiskId};

    fn binding() -> GitObjectPoolBinding {
        GitObjectPoolBinding::new(
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            GitObjectPoolId::parse("pool-a").unwrap(),
            GitObjectPoolGeneration::new(2).unwrap(),
            ProjectDiskId::parse("disk-a").unwrap(),
            ProjectDiskGeneration::new(1).unwrap(),
            GitObjectFormat::Sha1,
            GitObjectPoolProducerGenerationId::parse("git-2.55.0").unwrap(),
            GitObjectPoolTrustGenerationId::parse("trust-a").unwrap(),
        )
    }

    fn source_identity() -> ImmutableGitObjectPoolProducerSourceIdentity {
        ImmutableGitObjectPoolProducerSourceIdentity::from_verified(
            Sha256Digest::parse(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .unwrap(),
        )
    }

    fn target() -> ImmutableGitObjectPoolAdminProducerTarget {
        ImmutableGitObjectPoolAdminProducerTarget::from_verified(
            PathBuf::from("/srv/smolrunner/source.git"),
            PathBuf::from("/srv/smolrunner/staging/candidate.git"),
            PathBuf::from("/opt/smolrunner/empty-git-template"),
            PathBuf::from("/run/smolrunner/pool-config"),
            source_identity(),
            binding(),
            ImmutableGitObjectPoolAdminProducerIdentity::from_verified(1000, 1000).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn plan_is_bare_local_no_hardlinks_with_scrubbed_environment() {
        let plan = plan_immutable_git_object_pool_admin_producer(&target()).unwrap();
        let argv = plan.command().displayed_argv();
        assert_eq!(argv[0], GIT);
        assert!(argv.iter().any(|value| value == "clone"));
        assert!(argv.iter().any(|value| value == "--bare"));
        assert!(argv.iter().any(|value| value == "--local"));
        assert!(argv.iter().any(|value| value == "--no-hardlinks"));
        assert!(argv.iter().any(|value| value == "--no-optional-locks"));
        assert!(
            argv.iter()
                .any(|value| value.starts_with("--template=/opt/smolrunner/"))
        );
        assert!(
            !argv
                .iter()
                .any(|value| value == "--shared" || value == "--reference")
        );
        assert!(!argv.iter().any(|value| value.contains("://")));

        let keys = plan
            .command()
            .environment
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            keys,
            BTreeSet::from([
                "GIT_ASKPASS",
                "GIT_ATTR_NOSYSTEM",
                "GIT_CONFIG_GLOBAL",
                "GIT_CONFIG_NOSYSTEM",
                "GIT_TERMINAL_PROMPT",
                "HOME",
                "LANG",
                "LC_ALL",
                "PATH",
                "XDG_CONFIG_HOME",
            ])
        );
        assert_eq!(
            plan.summary().credential_policy(),
            ImmutableGitObjectPoolAdminCredentialPolicy::VerifiedAdminPrimaryIdentityClearSupplementaryGroups
        );
        assert!(plan.summary().bare_clone());
        assert!(plan.summary().local_source_only());
        assert!(plan.summary().hardlinks_disabled());
        assert!(plan.summary().reviewed_empty_template());
        assert!(plan.summary().ambient_environment_cleared());
        match &plan.command().environment["PATH"] {
            CommandValue::Plain(value) => assert_eq!(value, SAFE_PATH),
            CommandValue::Secret(_) => panic!("PATH must be plain"),
        }
    }

    #[test]
    fn summary_binds_source_and_candidate_without_private_paths_or_uid_gid() {
        let target = target();
        let plan = plan_immutable_git_object_pool_admin_producer(&target).unwrap();
        assert_eq!(
            plan.summary().source_identity_digest(),
            target.source_identity.digest()
        );
        assert_eq!(
            plan.summary().candidate_binding_digest(),
            &git_object_pool_binding_digest(&target.candidate_binding).unwrap()
        );
        let summary = serde_json::to_string(plan.summary()).unwrap();
        let debug = format!("{target:?} {plan:?}");
        for secret in [
            "/srv/smolrunner/source.git",
            "/srv/smolrunner/staging/candidate.git",
            "/opt/smolrunner/empty-git-template",
            "/run/smolrunner/pool-config",
            "uid: 1000",
            "gid: 1000",
        ] {
            assert!(!summary.contains(secret));
            assert!(!debug.contains(secret));
        }
    }

    #[test]
    fn root_credentials_are_refused() {
        for (uid, gid) in [(0, 1000), (1000, 0), (0, 0)] {
            assert_eq!(
                ImmutableGitObjectPoolAdminProducerIdentity::from_verified(uid, gid)
                    .unwrap_err()
                    .kind(),
                ImmutableGitObjectPoolAdminProducerPlanErrorKind::InvalidAdmin
            );
        }
    }

    #[test]
    fn relative_or_nested_private_roots_are_refused() {
        let admin = ImmutableGitObjectPoolAdminProducerIdentity::from_verified(1000, 1000).unwrap();
        assert_eq!(
            ImmutableGitObjectPoolAdminProducerTarget::from_verified(
                PathBuf::from("relative.git"),
                PathBuf::from("/srv/staging/candidate.git"),
                PathBuf::from("/opt/empty-template"),
                PathBuf::from("/run/config"),
                source_identity(),
                binding(),
                admin,
            )
            .unwrap_err()
            .kind(),
            ImmutableGitObjectPoolAdminProducerPlanErrorKind::InvalidPath
        );
        assert_eq!(
            ImmutableGitObjectPoolAdminProducerTarget::from_verified(
                PathBuf::from("/srv/source.git"),
                PathBuf::from("/srv/staging/candidate.git"),
                PathBuf::from("/srv/staging/candidate.git/template"),
                PathBuf::from("/run/config"),
                source_identity(),
                binding(),
                admin,
            )
            .unwrap_err()
            .kind(),
            ImmutableGitObjectPoolAdminProducerPlanErrorKind::ConflictingPrivateRoots
        );
    }

    #[test]
    fn private_admin_identity_is_available_only_to_later_executor() {
        let plan = plan_immutable_git_object_pool_admin_producer(&target()).unwrap();
        assert_eq!(plan.admin().uid(), 1000);
        assert_eq!(plan.admin().gid(), 1000);
        let debug = format!("{plan:?}");
        assert!(!debug.contains("1000"));
    }
}
