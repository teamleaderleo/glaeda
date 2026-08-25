//! Public fail-closed facade for descriptor-bound Lima standalone-disk observation.
//!
//! The physically established Lima 2.2.0 parser and descriptor engine live in the private `raw`
//! child module. Product callers receive only unbound physical observation/absence evidence. The
//! held absence lease supports exactly one live consume-once `observe_created` transition for the
//! uninterrupted external create transaction; controller death destroys that lease, and a fresh
//! same-name observation can never recreate it. A crate-private create-durability target seam is
//! reserved for #700's reviewed barrier. P2 deliberately exposes no API that can combine an
//! arbitrary P1 lease with a physical digest or manufacture `Exact`/`CurrentAttachment` lease
//! transition evidence. Durable P3 create provenance is the first layer allowed to bind these
//! observed identities to a SmolRunner project-disk generation.

// The private child temporarily retains the pre-repair P1 projection implementation so the proven
// descriptor/parser code can land without a simultaneous 1,800-line rewrite. It is unreachable to
// sibling/product modules through this facade and will be deleted once P3 owns the replacement
// provenance seam.
#[allow(dead_code)]
mod raw;
mod source;

use std::fmt;

use serde::Serialize;

use crate::artifact::Sha256Digest;

pub(crate) use raw::ProjectDiskCreateDurabilityTarget;
pub use raw::{
    LimaStandaloneDiskAbsenceObservation, LimaStandaloneDiskAbsenceSummary,
    LimaStandaloneDiskDisposition, LimaStandaloneDiskName,
    PROJECT_DISK_HOST_OBSERVATION_SCHEMA_VERSION, ProjectDiskHostObservationError,
    ProjectDiskHostObservationErrorKind, ProjectDiskPhysicalIdentity,
};
pub use source::{
    ConfiguredProjectDiskLimaSource, LimaStandaloneDiskFixtureObservationRequest,
    LimaStandaloneDiskObservationRequest, ProjectDiskLimaSourceError,
    ProjectDiskLimaSourceIdentity, ProjectDiskLimaSourceIdentityParseError,
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
        let backing_identity =
            ProjectDiskBackingIdentity::parse(value.backing_identity().digest().as_str())
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

    /// Mint the crate-private #700 create-durability target from this held observation.
    ///
    /// Crate-private by design: only in-crate P3 durability code can name or consume the target,
    /// and minting revalidates every held descriptor and path binding first. Fixture-origin
    /// observations are permanently ineligible.
    ///
    /// # Errors
    ///
    /// Returns the bounded P2 refusal for fixture-origin observations or drifted held evidence.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn project_disk_create_durability_target(
        &mut self,
        fresh_inventory_json_lines: &[u8],
    ) -> Result<ProjectDiskCreateDurabilityTarget, ProjectDiskHostObservationError> {
        self.inner
            .project_disk_create_durability_target(fresh_inventory_json_lines)
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

/// Observe one planned locator as absent using a production request derived by P2.
///
/// # Errors
///
/// Fails closed if the derived child exists, inventory reports it, or the retained source/collection
/// evidence cannot satisfy the existing raw descriptor contract.
pub fn observe_lima_standalone_disk_absence(
    request: LimaStandaloneDiskObservationRequest,
    inventory_json_lines: &[u8],
) -> Result<LimaStandaloneDiskAbsenceObservation, ProjectDiskHostObservationError> {
    raw::observe_lima_standalone_disk_absence(request.into_raw(), inventory_json_lines)
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
    observe_raw_lima_standalone_disk(request.into_raw(), inventory_json_lines)
}

/// Observe one explicitly supplied retained fixture as unbound diagnostic evidence.
///
/// This entrypoint is deliberately request-type-distinct from the production source+locator path.
/// It is useful for the retained #634 research fixture and grants no path into the live P3
/// absence->create->created transition, which consumes a held production absence lease instead.
///
/// # Errors
///
/// Returns the same bounded raw P2 refusal as the ordinary observation path.
pub fn observe_lima_standalone_disk_fixture(
    request: LimaStandaloneDiskFixtureObservationRequest,
    inventory_json_lines: &[u8],
) -> Result<LimaStandaloneDiskObservation, ProjectDiskHostObservationError> {
    observe_raw_lima_standalone_disk(request.into_raw(), inventory_json_lines)
}

fn observe_raw_lima_standalone_disk(
    request: raw::LimaStandaloneDiskObservationRequest,
    inventory_json_lines: &[u8],
) -> Result<LimaStandaloneDiskObservation, ProjectDiskHostObservationError> {
    let inner = raw::observe_lima_standalone_disk(request, inventory_json_lines)?;
    let summary = LimaStandaloneDiskObservationSummary::from_raw(inner.summary());
    Ok(LimaStandaloneDiskObservation { inner, summary })
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);
    const DISK_BYTES: u64 = 1024 * 1024;
    const COLLECTION: &str = "_disks";

    struct FacadeFixture {
        root: PathBuf,
        lima_home: PathBuf,
        disk_directory: PathBuf,
        disk_name: LimaStandaloneDiskName,
    }

    impl FacadeFixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "smolrunner-project-disk-facade-{}-{}",
                std::process::id(),
                NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
            let lima_home = root.join("lima");
            let disk_name = LimaStandaloneDiskName::parse("facade-disk").unwrap();
            let collection = lima_home.join(COLLECTION);
            let disk_directory = collection.join(disk_name.as_str());
            fs::create_dir(&lima_home).unwrap();
            fs::create_dir(&collection).unwrap();
            fs::create_dir(&disk_directory).unwrap();
            for directory in [&lima_home, &collection, &disk_directory] {
                fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
            }
            let backing = disk_directory.join("opaque-regular-entry");
            let file = File::create(&backing).unwrap();
            file.set_len(DISK_BYTES).unwrap();
            drop(file);
            fs::set_permissions(&backing, fs::Permissions::from_mode(0o600)).unwrap();
            Self {
                root,
                lima_home,
                disk_directory,
                disk_name,
            }
        }

        fn inventory(&self) -> Vec<u8> {
            format!(
                "{{\"name\":\"{}\",\"size\":{DISK_BYTES},\"format\":\"raw\",\"dir\":\"{}\",\"instance\":\"\",\"instanceDir\":\"\",\"mountPoint\":\"/mnt/facade\"}}\n",
                self.disk_name.as_str(),
                self.disk_directory.display()
            )
            .into_bytes()
        }
    }

    impl Drop for FacadeFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn planned_facade_observation_mints_durability_target_with_configured_identity() {
        let fixture = FacadeFixture::new();
        let source = ConfiguredProjectDiskLimaSource::new(&fixture.lima_home).unwrap();
        let request = LimaStandaloneDiskObservationRequest::for_planned_disk(
            &source,
            fixture.disk_name.clone(),
        )
        .unwrap();
        let inventory = fixture.inventory();
        let mut observed = observe_lima_standalone_disk(request, &inventory).unwrap();
        let target = observed
            .project_disk_create_durability_target(&inventory)
            .unwrap();
        assert_eq!(target.source_identity(), source.identity());
        assert_eq!(
            target.physical_identity(),
            observed.summary().physical_identity()
        );
        assert_eq!(target.backing_logical_bytes(), DISK_BYTES);
        let debug = format!("{target:?}");
        assert!(!debug.contains(fixture.root.to_str().unwrap()));
        assert!(!debug.contains("opaque-regular-entry"));
    }

    #[test]
    fn fixture_facade_observation_cannot_mint_durability_target() {
        let fixture = FacadeFixture::new();
        let request = LimaStandaloneDiskFixtureObservationRequest::new(
            fixture.disk_name.clone(),
            fixture.lima_home.clone(),
            fixture.disk_directory.clone(),
        )
        .unwrap();
        let inventory = fixture.inventory();
        let mut observed = observe_lima_standalone_disk_fixture(request, &inventory).unwrap();
        let error = observed
            .project_disk_create_durability_target(&inventory)
            .unwrap_err();
        assert_eq!(
            error.code(),
            "project_disk_create_durability_target_unavailable"
        );
    }

    #[test]
    fn planned_facade_absence_supports_first_disk_bootstrap() {
        let fixture = FacadeFixture::new();
        let collection = fixture.lima_home.join(COLLECTION);
        fs::remove_dir_all(&collection).unwrap();
        let source = ConfiguredProjectDiskLimaSource::new(&fixture.lima_home).unwrap();
        let request = LimaStandaloneDiskObservationRequest::for_planned_disk(
            &source,
            fixture.disk_name.clone(),
        )
        .unwrap();
        let absent = observe_lima_standalone_disk_absence(request, &[]).unwrap();
        assert!(absent.summary().proven_collection_absent());

        fs::create_dir(&collection).unwrap();
        fs::set_permissions(&collection, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(&fixture.disk_directory).unwrap();
        fs::set_permissions(&fixture.disk_directory, fs::Permissions::from_mode(0o700)).unwrap();
        let backing = fixture.disk_directory.join("opaque-regular-entry");
        let file = File::create(&backing).unwrap();
        file.set_len(DISK_BYTES).unwrap();
        drop(file);
        fs::set_permissions(&backing, fs::Permissions::from_mode(0o600)).unwrap();

        let created = absent.observe_created(&fixture.inventory()).unwrap();
        assert_eq!(
            created.summary().disposition(),
            LimaStandaloneDiskDisposition::Detached
        );
    }
}
