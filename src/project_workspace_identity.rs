//! Pure versioned digest vocabulary for project/workspace observation identities.
//!
//! The existing project checkout, discovery, and trusted-workspace observers historically used
//! SmolRunner-branded domain separators. Those bytes are evidence: changing a separator in place
//! would make the same host facts claim a different identity under the same generation.
//!
//! This module centralizes the two hashing forms used by those observers and makes the generation
//! explicit. It performs no filesystem observation, persistence, process execution, cleanup, or
//! adoption. A digest returned here is equality data only and grants zero authority over the
//! filesystem facts that a caller supplied.

use std::fmt;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;

const SHA256_PREFIX: &str = "sha256:";
const HEX: &[u8; 16] = b"0123456789abcdef";

const SMOLRUNNER_PROJECT_MATERIALIZATION_V1: &[u8] = b"smolrunner-project-materialization-v1\0";
const GLAEDA_PROJECT_MATERIALIZATION_V2: &[u8] = b"glaeda-project-materialization-v2\0";
const SMOLRUNNER_PROJECT_DISCOVERY_ROOT_V1: &[u8] = b"smolrunner-project-discovery-root-v1\0";
const GLAEDA_PROJECT_DISCOVERY_ROOT_V2: &[u8] = b"glaeda-project-discovery-root-v2\0";
const SMOLRUNNER_PROJECT_DISCOVERY_ENTRY_V1: &[u8] = b"smolrunner-project-discovery-entry-v1\0";
const GLAEDA_PROJECT_DISCOVERY_ENTRY_V2: &[u8] = b"glaeda-project-discovery-entry-v2\0";

const SMOLRUNNER_TRUSTED_WORKSPACE_ID_V1: &[u8] = b"smolrunner-trusted-workspace-id-v1";
const GLAEDA_TRUSTED_WORKSPACE_ID_V2: &[u8] = b"glaeda-trusted-workspace-id-v2";
const SMOLRUNNER_TRUSTED_CACHE_NAMESPACE_V1: &[u8] = b"smolrunner-trusted-cache-namespace-v1";
const GLAEDA_TRUSTED_CACHE_NAMESPACE_V2: &[u8] = b"glaeda-trusted-cache-namespace-v2";
const SMOLRUNNER_TRUSTED_WORKSPACE_EVIDENCE_V1: &[u8] = b"smolrunner-trusted-workspace-evidence-v1";
const GLAEDA_TRUSTED_WORKSPACE_EVIDENCE_V2: &[u8] = b"glaeda-trusted-workspace-evidence-v2";

/// Closed generation selector for the project/workspace identity family.
///
/// Fresh Glaeda observation code should use [`Self::CURRENT`]. The legacy generation exists only so
/// old exact evidence can still be reproduced or compared deliberately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectWorkspaceIdentityGeneration {
    SmolrunnerV1,
    GlaedaV2,
}

impl ProjectWorkspaceIdentityGeneration {
    pub const CURRENT: Self = Self::GlaedaV2;
}

/// Filesystem-object identity purpose using the historical fixed-width host-fact encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectWorkspaceFilesystemIdentityKind {
    Materialization,
    DiscoveryRoot,
    DiscoveryEntry,
}

/// Trusted-workspace digest purpose using the historical length-prefixed field encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustedWorkspaceIdentityKind {
    WorkspaceId,
    CacheNamespace,
    Evidence,
}

/// Compute one project/workspace filesystem identity from already-observed opaque host facts.
///
/// The byte encoding intentionally matches the current v1 checkout/discovery implementations:
///
/// ```text
/// domain || device:u64-be || inode:u64-be || owner:u32-be
/// ```
///
/// The function observes nothing and the result carries no filesystem capability.
///
/// # Errors
///
/// Returns a bounded error only if the canonical SHA-256 text cannot be represented by the shared
/// digest type.
pub fn project_workspace_filesystem_identity(
    generation: ProjectWorkspaceIdentityGeneration,
    kind: ProjectWorkspaceFilesystemIdentityKind,
    device: u64,
    inode: u64,
    owner: u32,
) -> Result<Sha256Digest, ProjectWorkspaceIdentityError> {
    let mut hasher = Sha256::new();
    hasher.update(filesystem_domain(generation, kind));
    hasher.update(device.to_be_bytes());
    hasher.update(inode.to_be_bytes());
    hasher.update(owner.to_be_bytes());
    parse_digest(hasher.finalize().as_slice())
}

/// Compute one trusted-workspace identity from already-validated bounded field bytes.
///
/// The byte encoding intentionally matches `trusted_workspace_receipt` v1:
///
/// ```text
/// domain || 0x00 || repeated(field_length:u64-be || field_bytes)
/// ```
///
/// Callers retain responsibility for deciding which typed fields belong to each identity. This
/// helper only closes the generation/domain separator and common canonical encoding.
///
/// # Errors
///
/// Returns a bounded error only if the canonical SHA-256 text cannot be represented by the shared
/// digest type.
pub fn trusted_workspace_identity<'a>(
    generation: ProjectWorkspaceIdentityGeneration,
    kind: TrustedWorkspaceIdentityKind,
    fields: impl IntoIterator<Item = &'a [u8]>,
) -> Result<Sha256Digest, ProjectWorkspaceIdentityError> {
    let mut hasher = Sha256::new();
    hasher.update(trusted_workspace_domain(generation, kind));
    hasher.update([0]);
    for field in fields {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field);
    }
    parse_digest(hasher.finalize().as_slice())
}

const fn filesystem_domain(
    generation: ProjectWorkspaceIdentityGeneration,
    kind: ProjectWorkspaceFilesystemIdentityKind,
) -> &'static [u8] {
    match (generation, kind) {
        (
            ProjectWorkspaceIdentityGeneration::SmolrunnerV1,
            ProjectWorkspaceFilesystemIdentityKind::Materialization,
        ) => SMOLRUNNER_PROJECT_MATERIALIZATION_V1,
        (
            ProjectWorkspaceIdentityGeneration::GlaedaV2,
            ProjectWorkspaceFilesystemIdentityKind::Materialization,
        ) => GLAEDA_PROJECT_MATERIALIZATION_V2,
        (
            ProjectWorkspaceIdentityGeneration::SmolrunnerV1,
            ProjectWorkspaceFilesystemIdentityKind::DiscoveryRoot,
        ) => SMOLRUNNER_PROJECT_DISCOVERY_ROOT_V1,
        (
            ProjectWorkspaceIdentityGeneration::GlaedaV2,
            ProjectWorkspaceFilesystemIdentityKind::DiscoveryRoot,
        ) => GLAEDA_PROJECT_DISCOVERY_ROOT_V2,
        (
            ProjectWorkspaceIdentityGeneration::SmolrunnerV1,
            ProjectWorkspaceFilesystemIdentityKind::DiscoveryEntry,
        ) => SMOLRUNNER_PROJECT_DISCOVERY_ENTRY_V1,
        (
            ProjectWorkspaceIdentityGeneration::GlaedaV2,
            ProjectWorkspaceFilesystemIdentityKind::DiscoveryEntry,
        ) => GLAEDA_PROJECT_DISCOVERY_ENTRY_V2,
    }
}

const fn trusted_workspace_domain(
    generation: ProjectWorkspaceIdentityGeneration,
    kind: TrustedWorkspaceIdentityKind,
) -> &'static [u8] {
    match (generation, kind) {
        (
            ProjectWorkspaceIdentityGeneration::SmolrunnerV1,
            TrustedWorkspaceIdentityKind::WorkspaceId,
        ) => SMOLRUNNER_TRUSTED_WORKSPACE_ID_V1,
        (
            ProjectWorkspaceIdentityGeneration::GlaedaV2,
            TrustedWorkspaceIdentityKind::WorkspaceId,
        ) => GLAEDA_TRUSTED_WORKSPACE_ID_V2,
        (
            ProjectWorkspaceIdentityGeneration::SmolrunnerV1,
            TrustedWorkspaceIdentityKind::CacheNamespace,
        ) => SMOLRUNNER_TRUSTED_CACHE_NAMESPACE_V1,
        (
            ProjectWorkspaceIdentityGeneration::GlaedaV2,
            TrustedWorkspaceIdentityKind::CacheNamespace,
        ) => GLAEDA_TRUSTED_CACHE_NAMESPACE_V2,
        (
            ProjectWorkspaceIdentityGeneration::SmolrunnerV1,
            TrustedWorkspaceIdentityKind::Evidence,
        ) => SMOLRUNNER_TRUSTED_WORKSPACE_EVIDENCE_V1,
        (ProjectWorkspaceIdentityGeneration::GlaedaV2, TrustedWorkspaceIdentityKind::Evidence) => {
            GLAEDA_TRUSTED_WORKSPACE_EVIDENCE_V2
        }
    }
}

fn parse_digest(bytes: &[u8]) -> Result<Sha256Digest, ProjectWorkspaceIdentityError> {
    let mut value = String::with_capacity(SHA256_PREFIX.len() + bytes.len() * 2);
    value.push_str(SHA256_PREFIX);
    for byte in bytes {
        value.push(char::from(HEX[usize::from(byte >> 4)]));
        value.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Sha256Digest::parse(&value).map_err(|_| ProjectWorkspaceIdentityError)
}

/// Bounded construction error for the pure identity helper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProjectWorkspaceIdentityError;

impl fmt::Display for ProjectWorkspaceIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("project/workspace identity could not be represented")
    }
}

impl std::error::Error for ProjectWorkspaceIdentityError {}
