//! Public fail-closed facade for descriptor-bound Lima standalone-disk observation.
//!
//! The physically established Lima 2.2.0 parser and descriptor engine live in the private `raw`
//! child module. Product callers receive only unbound physical observation/absence evidence. P2
//! deliberately exposes no API that can combine an arbitrary P1 lease with a physical digest or
//! manufacture `Exact`/`CurrentAttachment` lease-transition evidence. Durable P3 create provenance
//! is the first layer allowed to bind these observed identities to a SmolRunner project-disk
//! generation.

mod raw;

use std::fmt;

use serde::Serialize;

use crate::artifact::Sha256Digest;

pub use raw::{
    LimaStandaloneDiskAbsenceObservation, LimaStandaloneDiskAbsenceSummary,
    LimaStandaloneDiskDisposition, LimaStandaloneDiskName, LimaStandaloneDiskObservationRequest,
    ProjectDiskHostObservationError, ProjectDiskHostObservationErrorKind,
    ProjectDiskPhysicalIdentity, PROJECT_DISK_HOST_OBSERVATION_SCHEMA_VERSION,
    observe_lima_standalone_disk_absence,
};

/// Persistable opaque identity for the exact backing entry observed by P2.
///
/// This is physical observation data only. Equality with a later observation carries no ownership
/// or mutation authority without the separately durable P3 create provenance that first accepted
/// the backing identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ProjectDiskBackingIdentity(Sha256Digest);

impl ProjectDiskBackingIdentity {
    /// Parse one persisted canonical backing identity digest.
    ///
    /// # Errors
    ///
    /// Returns a bounded error unless `value` is canonical SHA-256.
    pub fn parse(value: &str) -> Result<Self, ProjectDiskBackingIdentityParseError> {
        Sha256Digest::parse(value)
            .map(Self)
            .map_err(|_| ProjectDiskBackingIdentityParseError)
    }

    #[must_use]
    pub const fn digest(&self) -> &Sha256Digest {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProjectDiskBackingIdentityParseError;

impl ProjectDiskBackingIdentityParseError {
    #[must_use]
    pub const fn code(self) -> &'static str {
        "project_disk_backing_identity_invalid"
    }
}

impl fmt::Display for ProjectDiskBackingIdentityParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("project disk backing identity must be canonical SHA-256")
    }
}

impl std::error::Error for ProjectDiskBackingIdentityParseError {}

/// Sanitized unbound host observation for one exact standalone disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LimaStandaloneDiskObservationSummary {
    schema_version: u8,
    disposition: LimaStandaloneDiskDisposition,
    physical_identity: ProjectDiskPhysicalIdentity,
    backing_identity: ProjectDiskBackingIdentity,
    backing_logical_bytes: u64,
    backing_allocated_bytes: u64,
    inventory_logical_bytes: u64,
    inventory_format_raw: bool,
}

impl LimaStandaloneDiskObservationSummary {
    fn from_raw(value: &raw::LimaStandaloneDiskObservationSummary) -> Self {
        let backing_identity = ProjectDiskBackingIdentity::parse(
            value.backing_identity().digest().as_str(),
        )
        .expect("raw P2 backing identity is already canonical SHA-256");
        Self {
            schema_version: value.schema_version(),
            disposition: value.disposition(),
            physical_identity: value.physical_identity().clone(),
            backing_identity,
            backing_logical_bytes: value.backing_logical_bytes(),
            backing_allocated_bytes: value.backing_allocated_bytes(),
            inventory_logical_bytes: value.inventory_logical_bytes(),
            inventory_format_raw: value.inventory_format_raw(),
        }
    }

    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn disposition(&self) -> LimaStandaloneDiskDisposition {
        self.disposition
    }

    #[must_use]
    pub const fn physical_identity(&self) -> &ProjectDiskPhysicalIdentity {
        &self.physical_identity
    }

    #[must_use]
    pub const fn backing_identity(&self) -> &ProjectDiskBackingIdentity {
        &self.backing_identity
    }

    #[must_use]
    pub const fn backing_logical_bytes(&self) -> u64 {
        self.backing_logical_bytes
    }

    #[must_use]
    pub const fn backing_allocated_bytes(&self) -> u64 {
        self.backing_allocated_bytes
    }

    #[must_use]
    pub const fn inventory_logical_bytes(&self) -> u64 {
        self.inventory_logical_bytes
    }

    #[must_use]
    pub const fn inventory_format_raw(&self) -> bool {
        self.inventory_format_raw
    }
}

/// Held descriptor-bound P2 observation with no P1 projection surface.
pub struct LimaStandaloneDiskObservation {
    inner: raw::LimaStandaloneDiskObservation,
    summary: LimaStandaloneDiskObservationSummary,
}

impl LimaStandaloneDiskObservation {
    #[must_use]
    pub const fn summary(&self) -> &LimaStandaloneDiskObservationSummary {
        &self.summary
    }

    /// Reconfirm every held descriptor, opaque entry role, path binding, and fresh inventory.
    ///
    /// # Errors
    ///
    /// Returns the bounded P2 refusal if any descriptor/path/inventory evidence drifts.
    pub fn confirm(
        &mut self,
        inventory_json_lines: &[u8],
    ) -> Result<(), ProjectDiskHostObservationError> {
        self.inner.confirm(inventory_json_lines)
    }
}

impl fmt::Debug for LimaStandaloneDiskObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LimaStandaloneDiskObservation")
            .field("summary", &self.summary)
            .field("private_descriptors", &"<private-project-disk-descriptors>")
            .finish()
    }
}

/// Observe one existing standalone disk as unbound physical evidence.
///
/// The private raw observer performs the physically accepted descriptor/no-follow/inventory checks.
/// This facade copies only the sanitized observation identity/byte summary and retains the raw
/// descriptor lease privately for later `confirm` calls. It exposes no lease-binding or mutation
/// method.
///
/// # Errors
///
/// Fails closed on missing/unsafe objects, unreviewed inventory, unexpected direct entries, or
/// rebind/drift during the observation window.
pub fn observe_lima_standalone_disk(
    request: LimaStandaloneDiskObservationRequest,
    inventory_json_lines: &[u8],
) -> Result<LimaStandaloneDiskObservation, ProjectDiskHostObservationError> {
    let inner = raw::observe_lima_standalone_disk(request, inventory_json_lines)?;
    let summary = LimaStandaloneDiskObservationSummary::from_raw(inner.summary());
    Ok(LimaStandaloneDiskObservation { inner, summary })
}
