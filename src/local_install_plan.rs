use std::collections::BTreeSet;
use std::fmt;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::{CommitId, GitTreeId, Sha256Digest};

pub const LOCAL_INSTALL_PLAN_SCHEMA_VERSION: u8 = 1;
pub const MAX_RETAINED_LOCAL_INSTALL_GENERATIONS: usize = 1;
pub const MAX_LOCAL_INSTALL_TOOLCHAIN_BYTES: usize = 128;
pub const MAX_LOCAL_INSTALL_VERSION_BYTES: usize = 128;
pub const MAX_LAUNCHER_OBSERVATIONS: usize = 4;

const SOURCE_DIGEST_DOMAIN: &[u8] = b"smolrunner-local-install-source-v1\0";
const GENERATION_DIGEST_DOMAIN: &[u8] = b"smolrunner-local-install-generation-v1\0";
const SHA256_PREFIX: &str = "sha256:";
const HEX: &[u8; 16] = b"0123456789abcdef";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct LocalInstallToolchainIdentity(String);

impl LocalInstallToolchainIdentity {
    /// Parse one bounded toolchain identity used to bind a local binary build.
    ///
    /// # Errors
    ///
    /// Returns a bounded error unless the value is non-empty ASCII without whitespace/control
    /// characters and uses the reviewed toolchain token vocabulary.
    pub fn parse(value: &str) -> Result<Self, LocalInstallPlanError> {
        if value.is_empty()
            || value.len() > MAX_LOCAL_INSTALL_TOOLCHAIN_BYTES
            || !value.is_ascii()
            || !value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-')
            })
        {
            return Err(invalid_toolchain());
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalInstallSourceIdentity {
    pub commit: CommitId,
    pub tree: GitTreeId,
    pub cargo_lock_digest: Sha256Digest,
    pub toolchain: LocalInstallToolchainIdentity,
    pub digest: Sha256Digest,
}

impl LocalInstallSourceIdentity {
    /// Bind one exact source checkout and toolchain to a canonical local-install source identity.
    ///
    /// # Errors
    ///
    /// Returns a bounded error only when canonical identity encoding fails.
    pub fn new(
        commit: CommitId,
        tree: GitTreeId,
        cargo_lock_digest: Sha256Digest,
        toolchain: LocalInstallToolchainIdentity,
    ) -> Result<Self, LocalInstallPlanError> {
        let canonical = LocalInstallSourceDigestDocument {
            commit: &commit,
            tree: &tree,
            cargo_lock_digest: &cargo_lock_digest,
            toolchain: &toolchain,
        };
        let digest = canonical_digest(SOURCE_DIGEST_DOMAIN, &canonical)?;
        Ok(Self {
            commit,
            tree,
            cargo_lock_digest,
            toolchain,
            digest,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct LocalInstallGenerationIdentity {
    pub number: u64,
    pub digest: Sha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstalledLocalBinaryGeneration {
    pub identity: LocalInstallGenerationIdentity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub predecessor: Option<LocalInstallGenerationIdentity>,
    pub source: LocalInstallSourceIdentity,
    pub binary_digest: Sha256Digest,
    pub binary_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalInstallState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted: Option<InstalledLocalBinaryGeneration>,
    pub retained: Vec<InstalledLocalBinaryGeneration>,
}

impl LocalInstallState {
    /// Construct bounded accepted/retained local-install state.
    ///
    /// # Errors
    ///
    /// Returns an error for excess retained generations or duplicate generation identities.
    pub fn new(
        accepted: Option<InstalledLocalBinaryGeneration>,
        retained: Vec<InstalledLocalBinaryGeneration>,
    ) -> Result<Self, LocalInstallPlanError> {
        if retained.len() > MAX_RETAINED_LOCAL_INSTALL_GENERATIONS {
            return Err(retained_generation_limit());
        }
        let mut identities = BTreeSet::new();
        if let Some(current) = accepted.as_ref() {
            identities.insert(current.identity.clone());
        }
        for generation in &retained {
            if !identities.insert(generation.identity.clone()) {
                return Err(duplicate_generation_identity());
            }
        }
        Ok(Self { accepted, retained })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallPlatform {
    Macos,
    Linux,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LauncherLocationClass {
    HomeLocalBin,
    HomeBin,
    HomebrewBin,
    UsrLocalBin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LauncherDirectoryDisposition {
    ReadyUserOwned,
    NeedsElevation,
    Unsafe,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LauncherEntryDisposition {
    Absent,
    Owned { generation_digest: Sha256Digest },
    Foreign,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LauncherLocationObservation {
    pub location: LauncherLocationClass,
    pub in_path: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_rank: Option<u16>,
    pub directory: LauncherDirectoryDisposition,
    pub entry: LauncherEntryDisposition,
}

impl LauncherLocationObservation {
    /// Construct one symbolic observation for an approved launcher location.
    ///
    /// # Errors
    ///
    /// Returns an error unless PATH presence and rank agree.
    pub fn new(
        location: LauncherLocationClass,
        in_path: bool,
        path_rank: Option<u16>,
        directory: LauncherDirectoryDisposition,
        entry: LauncherEntryDisposition,
    ) -> Result<Self, LocalInstallPlanError> {
        if in_path != path_rank.is_some() {
            return Err(invalid_launcher_observation());
        }
        Ok(Self {
            location,
            in_path,
            path_rank,
            directory,
            entry,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalInstallBuildPlan {
    pub target_generation: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_predecessor: Option<LocalInstallGenerationIdentity>,
    pub source: LocalInstallSourceIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BuiltLocalBinaryEvidence {
    pub source_digest: Sha256Digest,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub predecessor: Option<LocalInstallGenerationIdentity>,
    pub binary_digest: Sha256Digest,
    pub binary_version: String,
}

impl BuiltLocalBinaryEvidence {
    /// Construct bounded reviewed build evidence.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty, oversized, non-ASCII, or control-containing version.
    pub fn new(
        source_digest: Sha256Digest,
        predecessor: Option<LocalInstallGenerationIdentity>,
        binary_digest: Sha256Digest,
        binary_version: impl Into<String>,
    ) -> Result<Self, LocalInstallPlanError> {
        let binary_version = binary_version.into();
        if binary_version.is_empty()
            || binary_version.len() > MAX_LOCAL_INSTALL_VERSION_BYTES
            || !binary_version.is_ascii()
            || binary_version.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(invalid_binary_version());
        }
        Ok(Self {
            source_digest,
            predecessor,
            binary_digest,
            binary_version,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LauncherSwitchPlan {
    pub target_generation: LocalInstallGenerationIdentity,
    pub location: LauncherLocationClass,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum LocalInstallDecision {
    BuildRequired { plan: LocalInstallBuildPlan },
    SwitchLauncher { plan: LauncherSwitchPlan },
    Satisfied {
        generation: LocalInstallGenerationIdentity,
        location: LauncherLocationClass,
    },
    ElevationRequired {
        generation: LocalInstallGenerationIdentity,
        location: LauncherLocationClass,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalInstallRollbackPlan {
    pub from: LocalInstallGenerationIdentity,
    pub to: LocalInstallGenerationIdentity,
    pub launcher: RollbackLauncherRequirement,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "requirement", rename_all = "snake_case")]
pub enum RollbackLauncherRequirement {
    AlreadyPointsToTarget { location: LauncherLocationClass },
    Switch { location: LauncherLocationClass },
    Elevation { location: LauncherLocationClass },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstallPlanErrorKind {
    InvalidToolchain,
    InvalidBinaryVersion,
    IdentityEncodingFailed,
    RetainedGenerationLimit,
    DuplicateGenerationIdentity,
    GenerationExhausted,
    BuildEvidenceConflict,
    TooManyLauncherObservations,
    InvalidLauncherObservation,
    DuplicateLauncherLocation,
    DuplicatePathRank,
    UnsupportedLauncherLocation,
    ForeignLauncher,
    UnknownLauncher,
    UnsafeLauncherLocation,
    NoApprovedLauncherLocation,
    RollbackTargetUnavailable,
    MissingAcceptedGeneration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalInstallPlanError {
    pub kind: LocalInstallPlanErrorKind,
    pub code: &'static str,
    pub problem: &'static str,
}

impl fmt::Display for LocalInstallPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.problem)
    }
}

impl std::error::Error for LocalInstallPlanError {}

/// Decide whether exact source requires a build or can proceed directly to launcher convergence.
///
/// # Errors
///
/// Returns a bounded error for exhausted generation space or invalid launcher evidence.
pub fn plan_local_install(
    state: &LocalInstallState,
    source: &LocalInstallSourceIdentity,
    platform: LocalInstallPlatform,
    launchers: &[LauncherLocationObservation],
) -> Result<LocalInstallDecision, LocalInstallPlanError> {
    let Some(accepted) = state.accepted.as_ref() else {
        return Ok(LocalInstallDecision::BuildRequired {
            plan: LocalInstallBuildPlan {
                target_generation: 1,
                expected_predecessor: None,
                source: source.clone(),
            },
        });
    };
    if accepted.source != *source {
        let target_generation = accepted
            .identity
            .number
            .checked_add(1)
            .ok_or_else(generation_exhausted)?;
        return Ok(LocalInstallDecision::BuildRequired {
            plan: LocalInstallBuildPlan {
                target_generation,
                expected_predecessor: Some(accepted.identity.clone()),
                source: source.clone(),
            },
        });
    }
    plan_launcher_for_generation(&accepted.identity, platform, launchers)
}

/// Turn one exact build plan plus reviewed artifact evidence into an immutable candidate generation.
///
/// # Errors
///
/// Returns a conflict unless source and predecessor evidence exactly match the build plan.
pub fn complete_local_install_build(
    plan: &LocalInstallBuildPlan,
    evidence: BuiltLocalBinaryEvidence,
) -> Result<InstalledLocalBinaryGeneration, LocalInstallPlanError> {
    if evidence.source_digest != plan.source.digest
        || evidence.predecessor != plan.expected_predecessor
    {
        return Err(build_evidence_conflict());
    }
    let canonical = LocalInstallGenerationDigestDocument {
        schema_version: LOCAL_INSTALL_PLAN_SCHEMA_VERSION,
        number: plan.target_generation,
        predecessor: &plan.expected_predecessor,
        source: &plan.source,
        binary_digest: &evidence.binary_digest,
        binary_version: &evidence.binary_version,
    };
    let digest = canonical_digest(GENERATION_DIGEST_DOMAIN, &canonical)?;
    Ok(InstalledLocalBinaryGeneration {
        identity: LocalInstallGenerationIdentity {
            number: plan.target_generation,
            digest,
        },
        predecessor: plan.expected_predecessor.clone(),
        source: plan.source.clone(),
        binary_digest: evidence.binary_digest,
        binary_version: evidence.binary_version,
    })
}

/// Plan only the stable `smol` launcher for one already-accepted or candidate generation.
///
/// # Errors
///
/// Returns a bounded conflict for foreign/unknown shadowing, unsafe approved locations, invalid
/// observations, unsupported classes, or the absence of any approved PATH location.
pub fn plan_launcher_for_generation(
    generation: &LocalInstallGenerationIdentity,
    platform: LocalInstallPlatform,
    launchers: &[LauncherLocationObservation],
) -> Result<LocalInstallDecision, LocalInstallPlanError> {
    let sorted = validated_launcher_observations(platform, launchers)?;
    let mut elevation_candidate = None;
    for observation in sorted {
        match &observation.entry {
            LauncherEntryDisposition::Foreign => return Err(foreign_launcher()),
            LauncherEntryDisposition::Unknown => return Err(unknown_launcher()),
            LauncherEntryDisposition::Owned { generation_digest } => {
                if observation.directory == LauncherDirectoryDisposition::Unsafe {
                    return Err(unsafe_launcher_location());
                }
                if generation_digest == &generation.digest {
                    return Ok(LocalInstallDecision::Satisfied {
                        generation: generation.clone(),
                        location: observation.location,
                    });
                }
                match observation.directory {
                    LauncherDirectoryDisposition::ReadyUserOwned => {
                        return Ok(LocalInstallDecision::SwitchLauncher {
                            plan: LauncherSwitchPlan {
                                target_generation: generation.clone(),
                                location: observation.location,
                            },
                        });
                    }
                    LauncherDirectoryDisposition::NeedsElevation => {
                        elevation_candidate.get_or_insert(observation.location);
                    }
                    LauncherDirectoryDisposition::Unavailable => {}
                    LauncherDirectoryDisposition::Unsafe => unreachable!(),
                }
            }
            LauncherEntryDisposition::Absent => match observation.directory {
                LauncherDirectoryDisposition::ReadyUserOwned => {
                    return Ok(LocalInstallDecision::SwitchLauncher {
                        plan: LauncherSwitchPlan {
                            target_generation: generation.clone(),
                            location: observation.location,
                        },
                    });
                }
                LauncherDirectoryDisposition::NeedsElevation => {
                    elevation_candidate.get_or_insert(observation.location);
                }
                LauncherDirectoryDisposition::Unavailable => {}
                LauncherDirectoryDisposition::Unsafe => return Err(unsafe_launcher_location()),
            },
        }
    }
    if let Some(location) = elevation_candidate {
        return Ok(LocalInstallDecision::ElevationRequired {
            generation: generation.clone(),
            location,
        });
    }
    Err(no_approved_launcher_location())
}

/// Plan rollback to one retained verified generation through the same launcher safety rules.
///
/// # Errors
///
/// Returns an error unless current state exists, the target is retained, and launcher evidence is
/// safe enough to switch or already resolves to the target.
pub fn plan_local_install_rollback(
    state: &LocalInstallState,
    target: &LocalInstallGenerationIdentity,
    platform: LocalInstallPlatform,
    launchers: &[LauncherLocationObservation],
) -> Result<LocalInstallRollbackPlan, LocalInstallPlanError> {
    let current = state
        .accepted
        .as_ref()
        .ok_or_else(missing_accepted_generation)?;
    let retained = state
        .retained
        .iter()
        .find(|generation| generation.identity == *target)
        .ok_or_else(rollback_target_unavailable)?;
    let launcher = match plan_launcher_for_generation(&retained.identity, platform, launchers)? {
        LocalInstallDecision::Satisfied { location, .. } => {
            RollbackLauncherRequirement::AlreadyPointsToTarget { location }
        }
        LocalInstallDecision::SwitchLauncher { plan } => {
            RollbackLauncherRequirement::Switch {
                location: plan.location,
            }
        }
        LocalInstallDecision::ElevationRequired { location, .. } => {
            RollbackLauncherRequirement::Elevation { location }
        }
        LocalInstallDecision::BuildRequired { .. } => unreachable!(),
    };
    Ok(LocalInstallRollbackPlan {
        from: current.identity.clone(),
        to: retained.identity.clone(),
        launcher,
    })
}

fn validated_launcher_observations(
    platform: LocalInstallPlatform,
    launchers: &[LauncherLocationObservation],
) -> Result<Vec<&LauncherLocationObservation>, LocalInstallPlanError> {
    if launchers.len() > MAX_LAUNCHER_OBSERVATIONS {
        return Err(too_many_launcher_observations());
    }
    let mut locations = BTreeSet::new();
    let mut ranks = BTreeSet::new();
    let mut result = Vec::new();
    for observation in launchers {
        if observation.in_path != observation.path_rank.is_some() {
            return Err(invalid_launcher_observation());
        }
        if platform == LocalInstallPlatform::Linux
            && observation.location == LauncherLocationClass::HomebrewBin
        {
            return Err(unsupported_launcher_location());
        }
        if !locations.insert(observation.location) {
            return Err(duplicate_launcher_location());
        }
        if !observation.in_path {
            continue;
        }
        let rank = observation.path_rank.ok_or_else(invalid_launcher_observation)?;
        if !ranks.insert(rank) {
            return Err(duplicate_path_rank());
        }
        result.push(observation);
    }
    result.sort_by_key(|observation| observation.path_rank.unwrap_or(u16::MAX));
    Ok(result)
}

#[derive(Serialize)]
struct LocalInstallSourceDigestDocument<'a> {
    commit: &'a CommitId,
    tree: &'a GitTreeId,
    cargo_lock_digest: &'a Sha256Digest,
    toolchain: &'a LocalInstallToolchainIdentity,
}

#[derive(Serialize)]
struct LocalInstallGenerationDigestDocument<'a> {
    schema_version: u8,
    number: u64,
    predecessor: &'a Option<LocalInstallGenerationIdentity>,
    source: &'a LocalInstallSourceIdentity,
    binary_digest: &'a Sha256Digest,
    binary_version: &'a str,
}

fn canonical_digest(
    domain: &[u8],
    value: &impl Serialize,
) -> Result<Sha256Digest, LocalInstallPlanError> {
    let bytes = serde_json::to_vec(value).map_err(|_| identity_encoding_failed())?;
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(SHA256_PREFIX.len() + digest.len() * 2);
    encoded.push_str(SHA256_PREFIX);
    for byte in digest {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Sha256Digest::parse(&encoded).map_err(|_| identity_encoding_failed())
}

fn error(
    kind: LocalInstallPlanErrorKind,
    code: &'static str,
    problem: &'static str,
) -> LocalInstallPlanError {
    LocalInstallPlanError {
        kind,
        code,
        problem,
    }
}

fn invalid_toolchain() -> LocalInstallPlanError {
    error(
        LocalInstallPlanErrorKind::InvalidToolchain,
        "invalid_toolchain",
        "local install toolchain identity is invalid",
    )
}

fn invalid_binary_version() -> LocalInstallPlanError {
    error(
        LocalInstallPlanErrorKind::InvalidBinaryVersion,
        "invalid_binary_version",
        "local install binary version is invalid",
    )
}

fn identity_encoding_failed() -> LocalInstallPlanError {
    error(
        LocalInstallPlanErrorKind::IdentityEncodingFailed,
        "identity_encoding_failed",
        "local install canonical identity could not be encoded",
    )
}

fn retained_generation_limit() -> LocalInstallPlanError {
    error(
        LocalInstallPlanErrorKind::RetainedGenerationLimit,
        "retained_generation_limit",
        "local install retained generation count exceeds the reviewed bound",
    )
}

fn duplicate_generation_identity() -> LocalInstallPlanError {
    error(
        LocalInstallPlanErrorKind::DuplicateGenerationIdentity,
        "duplicate_generation_identity",
        "local install state contains a duplicate generation identity",
    )
}

fn generation_exhausted() -> LocalInstallPlanError {
    error(
        LocalInstallPlanErrorKind::GenerationExhausted,
        "generation_exhausted",
        "local install generation number is exhausted",
    )
}

fn build_evidence_conflict() -> LocalInstallPlanError {
    error(
        LocalInstallPlanErrorKind::BuildEvidenceConflict,
        "build_evidence_conflict",
        "built binary evidence does not match the exact local install build plan",
    )
}

fn too_many_launcher_observations() -> LocalInstallPlanError {
    error(
        LocalInstallPlanErrorKind::TooManyLauncherObservations,
        "too_many_launcher_observations",
        "launcher observation count exceeds the reviewed location set",
    )
}

fn invalid_launcher_observation() -> LocalInstallPlanError {
    error(
        LocalInstallPlanErrorKind::InvalidLauncherObservation,
        "invalid_launcher_observation",
        "launcher PATH presence and precedence evidence is inconsistent",
    )
}

fn duplicate_launcher_location() -> LocalInstallPlanError {
    error(
        LocalInstallPlanErrorKind::DuplicateLauncherLocation,
        "duplicate_launcher_location",
        "launcher evidence repeats one approved location class",
    )
}

fn duplicate_path_rank() -> LocalInstallPlanError {
    error(
        LocalInstallPlanErrorKind::DuplicatePathRank,
        "duplicate_path_rank",
        "launcher evidence repeats one PATH precedence rank",
    )
}

fn unsupported_launcher_location() -> LocalInstallPlanError {
    error(
        LocalInstallPlanErrorKind::UnsupportedLauncherLocation,
        "unsupported_launcher_location",
        "launcher location class is unsupported on this platform",
    )
}

fn foreign_launcher() -> LocalInstallPlanError {
    error(
        LocalInstallPlanErrorKind::ForeignLauncher,
        "foreign_launcher",
        "an earlier approved PATH location already contains a foreign smol launcher",
    )
}

fn unknown_launcher() -> LocalInstallPlanError {
    error(
        LocalInstallPlanErrorKind::UnknownLauncher,
        "unknown_launcher",
        "an earlier approved PATH location has unknown smol launcher identity",
    )
}

fn unsafe_launcher_location() -> LocalInstallPlanError {
    error(
        LocalInstallPlanErrorKind::UnsafeLauncherLocation,
        "unsafe_launcher_location",
        "an approved PATH location is unsafe for deterministic smol launcher resolution",
    )
}

fn no_approved_launcher_location() -> LocalInstallPlanError {
    error(
        LocalInstallPlanErrorKind::NoApprovedLauncherLocation,
        "no_approved_launcher_location",
        "no reviewed launcher location is usable in the current PATH",
    )
}

fn rollback_target_unavailable() -> LocalInstallPlanError {
    error(
        LocalInstallPlanErrorKind::RollbackTargetUnavailable,
        "rollback_target_unavailable",
        "requested local install rollback target is not a retained verified generation",
    )
}

fn missing_accepted_generation() -> LocalInstallPlanError {
    error(
        LocalInstallPlanErrorKind::MissingAcceptedGeneration,
        "missing_accepted_generation",
        "local install rollback requires one accepted generation",
    )
}

#[cfg(test)]
mod tests {
    use crate::artifact::{CommitId, GitTreeId, Sha256Digest};

    use super::{
        BuiltLocalBinaryEvidence, InstalledLocalBinaryGeneration, LauncherDirectoryDisposition,
        LauncherEntryDisposition, LauncherLocationClass, LauncherLocationObservation,
        LocalInstallDecision, LocalInstallGenerationIdentity, LocalInstallPlanErrorKind,
        LocalInstallPlatform, LocalInstallSourceIdentity, LocalInstallState,
        LocalInstallToolchainIdentity, RollbackLauncherRequirement, complete_local_install_build,
        plan_launcher_for_generation, plan_local_install, plan_local_install_rollback,
    };

    fn digest(byte: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", byte.to_string().repeat(64))).expect("digest")
    }

    fn source(commit: char) -> LocalInstallSourceIdentity {
        LocalInstallSourceIdentity::new(
            CommitId::parse(&commit.to_string().repeat(40)).expect("commit"),
            GitTreeId::parse(&"2".repeat(40)).expect("tree"),
            digest('a'),
            LocalInstallToolchainIdentity::parse("1.97.1-aarch64-apple-darwin")
                .expect("toolchain"),
        )
        .expect("source")
    }

    fn launch(
        location: LauncherLocationClass,
        rank: u16,
        directory: LauncherDirectoryDisposition,
        entry: LauncherEntryDisposition,
    ) -> LauncherLocationObservation {
        LauncherLocationObservation::new(location, true, Some(rank), directory, entry)
            .expect("launcher observation")
    }

    fn generation(number: u64, source: LocalInstallSourceIdentity, binary: char) -> InstalledLocalBinaryGeneration {
        let predecessor = if number > 1 {
            Some(LocalInstallGenerationIdentity {
                number: number - 1,
                digest: digest('d'),
            })
        } else {
            None
        };
        let plan = super::LocalInstallBuildPlan {
            target_generation: number,
            expected_predecessor: predecessor.clone(),
            source: source.clone(),
        };
        complete_local_install_build(
            &plan,
            BuiltLocalBinaryEvidence::new(
                source.digest.clone(),
                predecessor,
                digest(binary),
                "smolrunner 0.1.0",
            )
            .expect("artifact evidence"),
        )
        .expect("generation")
    }

    #[test]
    fn source_and_generation_identity_are_deterministic() {
        let first_source = source('1');
        let second_source = source('1');
        assert_eq!(first_source, second_source);
        let first = generation(1, first_source, 'b');
        let second = generation(1, second_source, 'b');
        assert_eq!(first, second);
    }

    #[test]
    fn first_install_and_source_upgrade_require_exact_builds() {
        let source_one = source('1');
        let empty = LocalInstallState::new(None, Vec::new()).expect("empty state");
        let LocalInstallDecision::BuildRequired { plan } = plan_local_install(
            &empty,
            &source_one,
            LocalInstallPlatform::Macos,
            &[],
        )
        .expect("first build") else {
            panic!("expected build")
        };
        assert_eq!(plan.target_generation, 1);
        assert!(plan.expected_predecessor.is_none());

        let accepted = generation(1, source_one.clone(), 'b');
        let state = LocalInstallState::new(Some(accepted.clone()), Vec::new()).expect("state");
        let LocalInstallDecision::BuildRequired { plan } = plan_local_install(
            &state,
            &source('3'),
            LocalInstallPlatform::Macos,
            &[],
        )
        .expect("upgrade build") else {
            panic!("expected upgrade build")
        };
        assert_eq!(plan.target_generation, 2);
        assert_eq!(plan.expected_predecessor, Some(accepted.identity));
    }

    #[test]
    fn build_evidence_must_match_source_and_predecessor() {
        let source = source('1');
        let plan = super::LocalInstallBuildPlan {
            target_generation: 2,
            expected_predecessor: Some(LocalInstallGenerationIdentity {
                number: 1,
                digest: digest('d'),
            }),
            source: source.clone(),
        };
        let evidence = BuiltLocalBinaryEvidence::new(
            digest('f'),
            plan.expected_predecessor.clone(),
            digest('b'),
            "smolrunner 0.1.0",
        )
        .expect("evidence");
        let error = complete_local_install_build(&plan, evidence).expect_err("source mismatch");
        assert_eq!(error.kind, LocalInstallPlanErrorKind::BuildEvidenceConflict);
    }

    #[test]
    fn safe_launcher_absent_or_stale_switches_and_current_is_satisfied() {
        let generation = generation(1, source('1'), 'b');
        let absent = [launch(
            LauncherLocationClass::HomeLocalBin,
            0,
            LauncherDirectoryDisposition::ReadyUserOwned,
            LauncherEntryDisposition::Absent,
        )];
        assert!(matches!(
            plan_launcher_for_generation(&generation.identity, LocalInstallPlatform::Macos, &absent)
                .expect("switch absent"),
            LocalInstallDecision::SwitchLauncher { .. }
        ));

        let stale = [launch(
            LauncherLocationClass::HomeLocalBin,
            0,
            LauncherDirectoryDisposition::ReadyUserOwned,
            LauncherEntryDisposition::Owned {
                generation_digest: digest('f'),
            },
        )];
        assert!(matches!(
            plan_launcher_for_generation(&generation.identity, LocalInstallPlatform::Macos, &stale)
                .expect("switch stale"),
            LocalInstallDecision::SwitchLauncher { .. }
        ));

        let current = [launch(
            LauncherLocationClass::HomeLocalBin,
            0,
            LauncherDirectoryDisposition::ReadyUserOwned,
            LauncherEntryDisposition::Owned {
                generation_digest: generation.identity.digest.clone(),
            },
        )];
        assert!(matches!(
            plan_launcher_for_generation(&generation.identity, LocalInstallPlatform::Macos, &current)
                .expect("satisfied"),
            LocalInstallDecision::Satisfied { .. }
        ));
    }

    #[test]
    fn earlier_foreign_or_unknown_launcher_blocks_later_safe_location() {
        let generation = generation(1, source('1'), 'b');
        for entry in [LauncherEntryDisposition::Foreign, LauncherEntryDisposition::Unknown] {
            let observations = [
                launch(
                    LauncherLocationClass::HomeBin,
                    0,
                    LauncherDirectoryDisposition::ReadyUserOwned,
                    entry,
                ),
                launch(
                    LauncherLocationClass::HomeLocalBin,
                    1,
                    LauncherDirectoryDisposition::ReadyUserOwned,
                    LauncherEntryDisposition::Absent,
                ),
            ];
            let error = plan_launcher_for_generation(
                &generation.identity,
                LocalInstallPlatform::Macos,
                &observations,
            )
            .expect_err("shadowing launcher must block");
            assert!(matches!(
                error.kind,
                LocalInstallPlanErrorKind::ForeignLauncher
                    | LocalInstallPlanErrorKind::UnknownLauncher
            ));
        }
    }

    #[test]
    fn later_user_owned_location_avoids_elevation_and_elevation_is_explicit_when_needed() {
        let generation = generation(1, source('1'), 'b');
        let observations = [
            launch(
                LauncherLocationClass::UsrLocalBin,
                0,
                LauncherDirectoryDisposition::NeedsElevation,
                LauncherEntryDisposition::Absent,
            ),
            launch(
                LauncherLocationClass::HomeLocalBin,
                1,
                LauncherDirectoryDisposition::ReadyUserOwned,
                LauncherEntryDisposition::Absent,
            ),
        ];
        let decision = plan_launcher_for_generation(
            &generation.identity,
            LocalInstallPlatform::Macos,
            &observations,
        )
        .expect("later user location");
        assert!(matches!(
            decision,
            LocalInstallDecision::SwitchLauncher {
                plan: super::LauncherSwitchPlan {
                    location: LauncherLocationClass::HomeLocalBin,
                    ..
                }
            }
        ));

        let only_elevation = [observations[0].clone()];
        assert!(matches!(
            plan_launcher_for_generation(
                &generation.identity,
                LocalInstallPlatform::Macos,
                &only_elevation,
            )
            .expect("elevation barrier"),
            LocalInstallDecision::ElevationRequired {
                location: LauncherLocationClass::UsrLocalBin,
                ..
            }
        ));
    }

    #[test]
    fn launcher_location_platform_and_path_evidence_fail_closed() {
        let generation = generation(1, source('1'), 'b');
        let homebrew = [launch(
            LauncherLocationClass::HomebrewBin,
            0,
            LauncherDirectoryDisposition::ReadyUserOwned,
            LauncherEntryDisposition::Absent,
        )];
        let error = plan_launcher_for_generation(
            &generation.identity,
            LocalInstallPlatform::Linux,
            &homebrew,
        )
        .expect_err("homebrew on linux");
        assert_eq!(
            error.kind,
            LocalInstallPlanErrorKind::UnsupportedLauncherLocation
        );

        let absent_from_path = [LauncherLocationObservation::new(
            LauncherLocationClass::HomeLocalBin,
            false,
            None,
            LauncherDirectoryDisposition::ReadyUserOwned,
            LauncherEntryDisposition::Absent,
        )
        .expect("out of path")];
        let error = plan_launcher_for_generation(
            &generation.identity,
            LocalInstallPlatform::Linux,
            &absent_from_path,
        )
        .expect_err("no location in path");
        assert_eq!(
            error.kind,
            LocalInstallPlanErrorKind::NoApprovedLauncherLocation
        );
    }

    #[test]
    fn rollback_targets_only_retained_verified_generation() {
        let source = source('1');
        let previous = generation(1, source.clone(), 'b');
        let current = generation(2, source, 'c');
        let state = LocalInstallState::new(Some(current.clone()), vec![previous.clone()])
            .expect("rollback state");
        let launchers = [launch(
            LauncherLocationClass::HomeLocalBin,
            0,
            LauncherDirectoryDisposition::ReadyUserOwned,
            LauncherEntryDisposition::Owned {
                generation_digest: current.identity.digest.clone(),
            },
        )];
        let plan = plan_local_install_rollback(
            &state,
            &previous.identity,
            LocalInstallPlatform::Macos,
            &launchers,
        )
        .expect("rollback plan");
        assert_eq!(plan.from, current.identity);
        assert_eq!(plan.to, previous.identity);
        assert!(matches!(
            plan.launcher,
            RollbackLauncherRequirement::Switch { .. }
        ));

        let error = plan_local_install_rollback(
            &state,
            &LocalInstallGenerationIdentity {
                number: 99,
                digest: digest('f'),
            },
            LocalInstallPlatform::Macos,
            &launchers,
        )
        .expect_err("unretained rollback");
        assert_eq!(error.kind, LocalInstallPlanErrorKind::RollbackTargetUnavailable);
    }

    #[test]
    fn generation_overflow_and_state_bounds_fail_closed() {
        let source = source('1');
        let accepted = generation(u64::MAX, source.clone(), 'b');
        let state = LocalInstallState::new(Some(accepted), Vec::new()).expect("state");
        let error = plan_local_install(
            &state,
            &source('3'),
            LocalInstallPlatform::Macos,
            &[],
        )
        .expect_err("generation overflow");
        assert_eq!(error.kind, LocalInstallPlanErrorKind::GenerationExhausted);

        let previous = generation(1, source.clone(), 'c');
        let retained = vec![previous.clone(), previous];
        let error = LocalInstallState::new(None, retained).expect_err("retained bound");
        assert_eq!(error.kind, LocalInstallPlanErrorKind::RetainedGenerationLimit);
    }

    #[test]
    fn public_plan_contains_only_symbolic_location_classes() {
        let source = source('1');
        let state = LocalInstallState::new(None, Vec::new()).expect("state");
        let decision = plan_local_install(
            &state,
            &source,
            LocalInstallPlatform::Macos,
            &[],
        )
        .expect("plan");
        let json = serde_json::to_string(&decision).expect("public plan");
        for private in [
            "/Users/",
            "/home/",
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "CARGO_HOME",
            "RUSTUP_HOME",
            "PATH=",
            "secret",
        ] {
            assert!(!json.contains(private), "leaked {private}");
        }
    }
}
