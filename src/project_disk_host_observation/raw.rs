//! Descriptor-bound read-only observation of one Lima standalone disk.
//!
//! The accepted Lima 2.2.0 schema comes from the real operator-Mac fixture captured by #634:
//! one private disk directory containing exactly one private regular backing file while detached,
//! plus exactly one current-user symlink while attached. Entry names stay opaque. The symlink is
//! attachment correlation only; neither it, the disk locator, nor Lima inventory grants ownership.
//!
//! This module executes no process and exposes no attach, detach, unlock, format, resize, delete,
//! repair, mount, or proof-minting authority. A bound P1 observation additionally requires an
//! expected physical identity, the exact current lease revision, and descriptor-bound resident VZ
//! host evidence for a `current_attachment` classification.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::os::fd::OwnedFd;
#[cfg(test)]
use std::os::fd::{AsFd as _, BorrowedFd};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::{Component, Path, PathBuf};

use rustix::fs::{self as rustix_fs, AtFlags, Dir, FileType, Mode, OFlags};
use rustix::io::Errno;
use rustix::process::geteuid;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::lima_host_identity::LimaHostIdentityObservation;
use crate::lima_observation::{
    LimaArchitecture, LimaInstanceName, LimaObservationRequest, LimaObservationSourceIdentity,
    LimaVmType,
};
use crate::project_catalog::ProjectIdentity;

use super::source::ProjectDiskLimaSourceIdentity;
use crate::project_disk_lease::{
    ProjectDiskAttachmentGeneration, ProjectDiskAttachmentLease, ProjectDiskGeneration,
    ProjectDiskId, ProjectDiskLeaseRecord, ProjectDiskLeaseState, ProjectDiskLockObservation,
    ProjectDiskObservation, ProjectDiskPhysicalObservation, ProjectDiskRecoverability,
    ProjectDiskRevision, ProjectDiskUseObservation, ResidentSandboxGeneration, ResidentSandboxId,
};

pub const PROJECT_DISK_HOST_OBSERVATION_SCHEMA_VERSION: u8 = 1;
const MAX_DISK_NAME_BYTES: usize = 64;
const MAX_INVENTORY_BYTES: usize = 64 * 1024;
const MAX_INVENTORY_RECORDS: usize = 256;
const MAX_PATH_BYTES: usize = 1_024;
const ALLOCATED_BLOCK_BYTES: u64 = 512;
const PHYSICAL_IDENTITY_DOMAIN: &[u8] = b"smolrunner-project-disk-physical-identity-v1";
const BACKING_IDENTITY_DOMAIN: &[u8] = b"smolrunner-project-disk-backing-identity-v1";
const REDACTED_DESCRIPTORS: &str = "<private-project-disk-descriptors>";

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NONBLOCK);
const FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NONBLOCK);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct LimaStandaloneDiskName(String);

impl LimaStandaloneDiskName {
    /// Parse one bounded non-option Lima standalone-disk locator.
    ///
    /// # Errors
    ///
    /// Returns a path-free refusal unless the value is one simple ASCII Lima identifier.
    pub fn parse(value: &str) -> Result<Self, ProjectDiskHostObservationError> {
        let mut bytes = value.bytes();
        let Some(first) = bytes.next() else {
            return Err(invalid_input());
        };
        if value.len() > MAX_DISK_NAME_BYTES
            || !(first.is_ascii_alphanumeric())
            || !bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(invalid_input());
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ProjectDiskPhysicalIdentity(Sha256Digest);

impl ProjectDiskPhysicalIdentity {
    /// Parse one persisted opaque physical identity digest.
    ///
    /// # Errors
    ///
    /// Returns a path-free refusal unless the value is canonical SHA-256.
    pub fn parse(value: &str) -> Result<Self, ProjectDiskHostObservationError> {
        Sha256Digest::parse(value)
            .map(Self)
            .map_err(|_| invalid_input())
    }

    #[must_use]
    pub const fn digest(&self) -> &Sha256Digest {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ProjectDiskBackingIdentity(Sha256Digest);

impl ProjectDiskBackingIdentity {
    #[must_use]
    pub const fn digest(&self) -> &Sha256Digest {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LimaStandaloneDiskDisposition {
    Detached,
    Attached,
    Conflicting,
}

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
    retained_lima_home_descriptor: bool,
    retained_collection_descriptor: bool,
    retained_disk_directory_descriptor: bool,
    retained_backing_descriptor: bool,
    retained_instance_directory_descriptor: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LimaStandaloneDiskAbsenceSummary {
    schema_version: u8,
    disk_directory_absent: bool,
    inventory_record_absent: bool,
    proven_collection_absent: bool,
    retained_lima_home_descriptor: bool,
    retained_collection_descriptor: bool,
}

impl LimaStandaloneDiskAbsenceSummary {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn disk_directory_absent(&self) -> bool {
        self.disk_directory_absent
    }

    #[must_use]
    pub const fn inventory_record_absent(&self) -> bool {
        self.inventory_record_absent
    }

    /// Whether the whole standalone-disk collection itself was proven absent.
    ///
    /// True means first-disk bootstrap held only the Lima-home descriptor while proving the exact
    /// collection child absent beneath it; the collection descriptor is then not retained.
    #[must_use]
    pub const fn proven_collection_absent(&self) -> bool {
        self.proven_collection_absent
    }

    #[must_use]
    pub const fn retained_lima_home_descriptor(&self) -> bool {
        self.retained_lima_home_descriptor
    }

    #[must_use]
    pub const fn retained_collection_descriptor(&self) -> bool {
        self.retained_collection_descriptor
    }
}

impl LimaStandaloneDiskObservationSummary {
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

pub struct LimaStandaloneDiskObservationRequest {
    disk_name: LimaStandaloneDiskName,
    lima_home: AcceptedPath,
    disk_directory: AcceptedPath,
    collection_name: OsString,
    planned_source_identity: Option<ProjectDiskLimaSourceIdentity>,
    held_lima_home: Option<HeldLimaSource>,
}

impl LimaStandaloneDiskObservationRequest {
    /// Bind one exact directly observed disk directory to one private Lima home and locator.
    ///
    /// The physical path must have exactly two components beneath the Lima home. The final
    /// component must equal the disk locator, but that equality is locator validation only. On
    /// macOS, `/var/...` and `/private/var/...` are treated as the same path only after proving the
    /// platform root alias; no other intermediate symlink is accepted.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal for aliased, non-absolute, overlong, or unrelated paths.
    pub fn new(
        disk_name: LimaStandaloneDiskName,
        lima_home: impl Into<PathBuf>,
        disk_directory: impl Into<PathBuf>,
    ) -> Result<Self, ProjectDiskHostObservationError> {
        let lima_home = AcceptedPath::new(lima_home.into())?;
        let disk_directory = AcceptedPath::new(disk_directory.into())?;
        let relative = disk_directory
            .physical
            .strip_prefix(&lima_home.physical)
            .map_err(|_| invalid_input())?;
        let components = relative.components().collect::<Vec<_>>();
        let [Component::Normal(collection), Component::Normal(candidate)] = components.as_slice()
        else {
            return Err(invalid_input());
        };
        if candidate.as_bytes() != disk_name.as_str().as_bytes() {
            return Err(invalid_input());
        }
        let collection_name = (*collection).to_owned();
        Ok(Self {
            disk_name,
            lima_home,
            disk_directory,
            collection_name,
            planned_source_identity: None,
            held_lima_home: None,
        })
    }

    /// Mark this request as derived from one configured planned source identity.
    ///
    /// Only planned production requests may later mint a #700 create-durability target; explicit
    /// fixture requests stay permanently ineligible.
    pub(super) fn with_planned_source_identity(
        mut self,
        identity: ProjectDiskLimaSourceIdentity,
    ) -> Self {
        self.planned_source_identity = Some(identity);
        self
    }

    pub(super) fn with_held_lima_source(mut self, held: HeldLimaSource) -> Self {
        self.held_lima_home = Some(held);
        self
    }

    fn take_lima_home(&mut self) -> Result<BoundDirectory, ProjectDiskHostObservationError> {
        match self.held_lima_home.take() {
            Some(held) => {
                held.confirm_path_binding()?;
                Ok(held.directory)
            }
            None => BoundDirectory::open_path(&self.lima_home),
        }
    }

    #[must_use]
    pub const fn disk_name(&self) -> &LimaStandaloneDiskName {
        &self.disk_name
    }
}

/// Crate-contained live binding for the configured Lima home.
pub(super) struct HeldLimaSource {
    path: AcceptedPath,
    directory: BoundDirectory,
}

impl HeldLimaSource {
    pub(super) fn open(path: PathBuf) -> Result<Self, ProjectDiskHostObservationError> {
        let path = AcceptedPath::new(path)?;
        let directory = BoundDirectory::open_path(&path)?;
        validate_private_directory(&directory.snapshot)?;
        Ok(Self { path, directory })
    }

    pub(super) fn confirm_path_binding(&self) -> Result<(), ProjectDiskHostObservationError> {
        let rebound = BoundDirectory::open_path(&self.path).map_err(|_| changed())?;
        if !same_stable_directory_identity(&rebound.snapshot, &self.directory.snapshot) {
            return Err(changed());
        }
        self.directory.revalidate_stable_identity()
    }

    pub(super) fn resident_observation_request(
        &self,
        instance: LimaInstanceName,
        guest_cache_path: &Path,
        max_age_seconds: u64,
    ) -> Result<LimaObservationRequest, ProjectDiskHostObservationError> {
        self.confirm_path_binding()?;
        LimaObservationRequest::new(
            instance,
            self.path.physical.clone(),
            LimaVmType::Vz,
            LimaArchitecture::Aarch64,
            guest_cache_path,
            max_age_seconds,
        )
        .map_err(|_| invalid_input())
    }

    pub(super) fn confirm_resident_instance_absent(
        &self,
        instance: &LimaInstanceName,
    ) -> Result<(), ProjectDiskHostObservationError> {
        self.confirm_path_binding()?;
        require_entry_absent(&self.directory.fd, OsStr::new(instance.as_str()))?;
        self.confirm_path_binding()
    }

    pub(super) fn into_planned_request(
        self,
        disk_name: LimaStandaloneDiskName,
        source_identity: ProjectDiskLimaSourceIdentity,
    ) -> Result<LimaStandaloneDiskObservationRequest, ProjectDiskHostObservationError> {
        self.confirm_path_binding()?;
        let disk_directory = self.path.physical.join("_disks").join(disk_name.as_str());
        LimaStandaloneDiskObservationRequest::new(
            disk_name,
            self.path.physical.clone(),
            disk_directory,
        )
        .map(|request| {
            request
                .with_planned_source_identity(source_identity)
                .with_held_lima_source(self)
        })
    }
}

impl fmt::Debug for HeldLimaSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HeldLimaSource(<private-source-binding>)")
    }
}

impl fmt::Debug for LimaStandaloneDiskObservationRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LimaStandaloneDiskObservationRequest")
            .field("disk_name", &self.disk_name)
            .field("private_paths", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiskLeaseObservationBindingSummary {
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    disk_revision: ProjectDiskRevision,
    #[serde(skip_serializing_if = "Option::is_none")]
    attachment_generation: Option<ProjectDiskAttachmentGeneration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resident_sandbox_id: Option<ResidentSandboxId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resident_sandbox_generation: Option<ResidentSandboxGeneration>,
}

impl ProjectDiskLeaseObservationBindingSummary {
    #[must_use]
    pub const fn project(&self) -> &ProjectIdentity {
        &self.project
    }

    #[must_use]
    pub const fn disk_id(&self) -> &ProjectDiskId {
        &self.disk_id
    }

    #[must_use]
    pub const fn disk_generation(&self) -> ProjectDiskGeneration {
        self.disk_generation
    }

    #[must_use]
    pub const fn disk_revision(&self) -> ProjectDiskRevision {
        self.disk_revision
    }

    #[must_use]
    pub const fn attachment_generation(&self) -> Option<ProjectDiskAttachmentGeneration> {
        self.attachment_generation
    }

    #[must_use]
    pub const fn resident_sandbox_id(&self) -> Option<&ResidentSandboxId> {
        self.resident_sandbox_id.as_ref()
    }

    #[must_use]
    pub const fn resident_sandbox_generation(&self) -> Option<ResidentSandboxGeneration> {
        self.resident_sandbox_generation
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiskHostObservationReport {
    schema_version: u8,
    binding: ProjectDiskLeaseObservationBindingSummary,
    physical_identity: ProjectDiskPhysicalIdentity,
    backing_identity: ProjectDiskBackingIdentity,
    backing_logical_bytes: u64,
    backing_allocated_bytes: u64,
    observation: ProjectDiskObservation,
    resident_host_identity_bound: bool,
}

impl ProjectDiskHostObservationReport {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn binding(&self) -> &ProjectDiskLeaseObservationBindingSummary {
        &self.binding
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
    pub const fn observation(&self) -> ProjectDiskObservation {
        self.observation
    }

    #[must_use]
    pub const fn resident_host_identity_bound(&self) -> bool {
        self.resident_host_identity_bound
    }
}

/// Expected host-controlled physical identity for one exact P1 lease revision.
///
/// Constructing this value records an expectation; it does not observe the host or grant mutation
/// authority. P3 must persist this expectation only after its separate durable pre-mutation create
/// checkpoint and fresh post-create observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDiskPhysicalBinding {
    binding: ProjectDiskLeaseObservationBindingSummary,
    physical_identity: ProjectDiskPhysicalIdentity,
}

impl ProjectDiskPhysicalBinding {
    #[must_use]
    pub fn new(
        record: &ProjectDiskLeaseRecord,
        physical_identity: ProjectDiskPhysicalIdentity,
    ) -> Self {
        Self {
            binding: binding_summary(record, None),
            physical_identity,
        }
    }
}

/// Exact P1 current-attachment mapping to a descriptor-bound Lima VZ host observation.
pub struct ProjectDiskResidentSandboxBinding {
    attachment: ProjectDiskAttachmentLease,
    request: LimaObservationRequest,
    host: Option<LimaHostIdentityObservation>,
    canonical_source: LimaObservationSourceIdentity,
}

impl ProjectDiskResidentSandboxBinding {
    /// Bind the exact current P1 attachment to one validated Lima request and held VZ identity.
    ///
    /// # Errors
    ///
    /// Fails unless the lease is currently attached and the held host observation reconfirms the
    /// supplied Lima request.
    pub fn new(
        record: &ProjectDiskLeaseRecord,
        request: LimaObservationRequest,
        host: LimaHostIdentityObservation,
    ) -> Result<Self, ProjectDiskHostObservationError> {
        let ProjectDiskLeaseState::Attached { attachment } = record.state() else {
            return Err(binding_mismatch());
        };
        host.confirm(&request).map_err(|_| resident_mismatch())?;
        let canonical_home = AcceptedPath::new(request.lima_home().to_owned())?;
        let canonical_source = LimaObservationSourceIdentity::from_validated(
            request.instance().clone(),
            canonical_home.physical,
        );
        Ok(Self {
            attachment: attachment.clone(),
            request,
            host: Some(host),
            canonical_source,
        })
    }

    #[cfg(test)]
    fn for_test(
        record: &ProjectDiskLeaseRecord,
        request: LimaObservationRequest,
    ) -> Result<Self, ProjectDiskHostObservationError> {
        let ProjectDiskLeaseState::Attached { attachment } = record.state() else {
            return Err(binding_mismatch());
        };
        let canonical_home = AcceptedPath::new(request.lima_home().to_owned())?;
        let canonical_source = LimaObservationSourceIdentity::from_validated(
            request.instance().clone(),
            canonical_home.physical,
        );
        Ok(Self {
            attachment: attachment.clone(),
            request,
            host: None,
            canonical_source,
        })
    }

    fn confirm(
        &self,
    ) -> Result<
        (LimaObservationSourceIdentity, &ProjectDiskAttachmentLease),
        ProjectDiskHostObservationError,
    > {
        if let Some(host) = &self.host {
            host.confirm(&self.request)
                .map_err(|_| resident_mismatch())?;
        }
        Ok((self.canonical_source.clone(), &self.attachment))
    }
}

impl fmt::Debug for ProjectDiskResidentSandboxBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectDiskResidentSandboxBinding")
            .field("attachment", &self.attachment)
            .field("private_host_evidence", &"<redacted>")
            .finish()
    }
}

pub struct LimaStandaloneDiskObservation {
    summary: LimaStandaloneDiskObservationSummary,
    request: LimaStandaloneDiskObservationRequest,
    inventory: SelectedInventory,
    lima_home: BoundDirectory,
    collection: BoundDirectory,
    disk_directory: BoundDirectory,
    backing_name: OsString,
    backing: BoundFile,
    lock: Option<BoundSymlink>,
    instance_directory: Option<BoundDirectory>,
    attached_source: Option<LimaObservationSourceIdentity>,
    created_lineage: Option<CreatedLineage>,
    collection_retained_from_before_creation: bool,
}

/// Private zero-sized proof that one observation was produced by consuming an uninterrupted
/// held-absence -> created-child transition inside one live controller transaction.
///
/// It is deliberately unconstructible outside [`establish_created_lineage`]'s success path and is
/// neither `Clone`, `Copy`, nor serializable. It is not a project/disk generation, lease revision,
/// persisted marker, or attempt identity; controller death destroys it together with the held
/// absence lease, and a later ordinary same-name observation can never obtain it.
struct CreatedLineage(());

/// Held lineage of one absence proof: either the trusted collection descriptor itself, or exact
/// proof that the collection child was absent beneath the held Lima home (first-disk bootstrap).
enum HeldAbsenceCollection {
    Bound(BoundDirectory),
    ProvenAbsent,
}

/// Opaque held proof that one planned locator is absent from a trusted private disk collection and
/// from the supplied strict Lima inventory observation.
pub struct LimaStandaloneDiskAbsenceObservation {
    summary: LimaStandaloneDiskAbsenceSummary,
    request: LimaStandaloneDiskObservationRequest,
    lima_home: BoundDirectory,
    collection: HeldAbsenceCollection,
}

impl LimaStandaloneDiskAbsenceObservation {
    #[must_use]
    pub const fn summary(&self) -> &LimaStandaloneDiskAbsenceSummary {
        &self.summary
    }

    /// Reconfirm descriptor-relative name absence and fresh inventory absence.
    ///
    /// # Errors
    ///
    /// Returns a bounded changed refusal if an entry or inventory record appears or either held
    /// parent is rebound. When the collection itself was proven absent, any later collection
    /// appearance also refuses: a foreign collection appearing before create confirmation grants
    /// zero create authority.
    pub fn confirm(
        &mut self,
        inventory_json_lines: &[u8],
    ) -> Result<(), ProjectDiskHostObservationError> {
        revalidate_absence(self, inventory_json_lines)
    }

    /// Exact pre/post pathname binding check for the accepted private source around a future
    /// external `limactl` handoff.
    ///
    /// Reopens the accepted source pathname under the same reviewed alias discipline and requires
    /// it to still resolve to the exact physical filesystem object held by this lease — device,
    /// inode, owner, group, and mode. The uninterrupted create transaction legitimately mutates
    /// parent timestamps and link counts, so those volatile fields stay reserved for the strict
    /// quiet-window confirmation. This executes no process and exposes no path; it proves only
    /// that the pathname the child would receive still binds to the held evidence.
    ///
    /// # Errors
    ///
    /// Returns a bounded changed refusal when the pathname is deleted, rebound, aliased, or no
    /// longer names the held physical home object.
    pub fn confirm_source_path_binding(&self) -> Result<(), ProjectDiskHostObservationError> {
        let rebound = BoundDirectory::open_path(&self.request.lima_home).map_err(|_| changed())?;
        let bound = same_stable_directory_identity(&rebound.snapshot, &self.lima_home.snapshot);
        drop(rebound);
        if !bound {
            return Err(changed());
        }
        self.lima_home.revalidate_stable_identity()
    }

    /// Consume this held absence lease and observe the externally created disk as ordinary unbound
    /// P2 evidence, preserving continuous ancestry at the strongest parent held before creation.
    ///
    /// The lease is consumed by value, so one absence proof can feed exactly one created
    /// observation regardless of outcome. The held home (and collection when present) descriptors
    /// are revalidated to the exact same stable filesystem object first — device, inode, owner,
    /// group, and mode; the create transaction itself legitimately mutates parent timestamps and
    /// directory link counts, so those stay reserved for the strict quiet-window confirmation at
    /// the end. A proven-absent collection must now exist as an exact private directory opened
    /// relative to the same held home. When a trusted collection descriptor was retained before
    /// creation, that exact retained descriptor is what traverses to the newly appeared child; it
    /// is never substituted with a freshly reopened equivalent pathname handle. Fresh strict
    /// inventory must report exactly the planned locator, every ordinary existing-disk check runs
    /// against that child, and a final confirmation re-proves the held lineage with stable-field
    /// discipline for parents held across the mutating create window before returning. Controller
    /// death destroys the lease; a fresh same-name observation can never recreate this live
    /// transition or the create-durability eligibility it grants.
    ///
    /// # Errors
    ///
    /// Fails closed when the source/collection rebinds, the collection or child is still absent,
    /// the new collection/child is unsafe, inventory is absent/duplicate/malformed/unsupported, or
    /// any state changes during the window.
    pub fn observe_created(
        self,
        fresh_inventory_json_lines: &[u8],
    ) -> Result<LimaStandaloneDiskObservation, ProjectDiskHostObservationError> {
        establish_created_lineage(self, fresh_inventory_json_lines, || {})
    }

    #[cfg(test)]
    fn observe_created_with_hook<F>(
        self,
        fresh_inventory_json_lines: &[u8],
        before_final_confirmation: F,
    ) -> Result<LimaStandaloneDiskObservation, ProjectDiskHostObservationError>
    where
        F: FnOnce(),
    {
        establish_created_lineage(self, fresh_inventory_json_lines, before_final_confirmation)
    }
}

fn establish_created_lineage<F>(
    lease: LimaStandaloneDiskAbsenceObservation,
    fresh_inventory_json_lines: &[u8],
    before_final_confirmation: F,
) -> Result<LimaStandaloneDiskObservation, ProjectDiskHostObservationError>
where
    F: FnOnce(),
{
    let LimaStandaloneDiskAbsenceObservation {
        summary: _,
        request,
        lima_home,
        collection,
    } = lease;
    lima_home.revalidate_stable_identity()?;
    let fresh_home = BoundDirectory::open_path(&request.lima_home).map_err(|_| changed())?;
    if !same_stable_directory_identity(&fresh_home.snapshot, &lima_home.snapshot) {
        return Err(changed());
    }
    let collection_retained = matches!(collection, HeldAbsenceCollection::Bound(_));
    let collection = match collection {
        HeldAbsenceCollection::Bound(bound) => {
            // #691 step 4: the created child is opened relative to the exact retained collection
            // descriptor, never a freshly reopened equivalent pathname handle. The create
            // transaction legitimately mutates the held collection's volatile metadata, so only
            // stable identity is reconfirmed on the retained descriptor here and the strict
            // quiet-window confirmation applies stable-field discipline to this parent.
            bound.revalidate_stable_identity()?;
            bound
        }
        HeldAbsenceCollection::ProvenAbsent => {
            let created = BoundDirectory::open_child(&lima_home.fd, &request.collection_name)?;
            validate_private_directory(&created.snapshot)?;
            created
        }
    };
    drop(lima_home);
    observe_existing_from_bound_parents(
        request,
        fresh_home,
        collection,
        fresh_inventory_json_lines,
        before_final_confirmation,
        Some(CreatedLineage(())),
        collection_retained,
    )
}

impl fmt::Debug for LimaStandaloneDiskAbsenceObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let _ = &self.lima_home.fd;
        formatter
            .debug_struct("LimaStandaloneDiskAbsenceObservation")
            .field("summary", &self.summary)
            .field("private_descriptors", &REDACTED_DESCRIPTORS)
            .finish()
    }
}

impl LimaStandaloneDiskObservation {
    #[must_use]
    pub const fn summary(&self) -> &LimaStandaloneDiskObservationSummary {
        &self.summary
    }

    /// Reconfirm every held descriptor, opaque entry role, path binding, and a fresh inventory
    /// observation without mutation.
    ///
    /// # Errors
    ///
    /// Returns `changed_during_observation` for any same-name replacement or rebind.
    pub fn confirm(
        &mut self,
        inventory_json_lines: &[u8],
    ) -> Result<(), ProjectDiskHostObservationError> {
        revalidate_observation(self, inventory_json_lines)
    }

    /// Project the held physical result into P1's closed vocabulary.
    ///
    /// A physical identity mismatch is reported as `conflicting`, never adopted. An attached disk
    /// becomes `current_attachment` only when the optional resident binding matches both the exact
    /// current P1 attachment and the descriptor-bound Lima source observed through the lock and
    /// inventory. Other attached state remains `other`.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal when the expected binding names another P1 revision or resident
    /// attachment, or when held resident host evidence drifts.
    pub fn bind_to_project_disk(
        &mut self,
        record: &ProjectDiskLeaseRecord,
        physical_binding: &ProjectDiskPhysicalBinding,
        resident_binding: Option<&ProjectDiskResidentSandboxBinding>,
        fresh_inventory_json_lines: &[u8],
    ) -> Result<ProjectDiskHostObservationReport, ProjectDiskHostObservationError> {
        self.confirm(fresh_inventory_json_lines)?;
        let expected_binding = binding_summary(record, None);
        if physical_binding.binding != expected_binding {
            return Err(binding_mismatch());
        }

        let physical = if physical_binding.physical_identity == self.summary.physical_identity {
            ProjectDiskPhysicalObservation::Exact
        } else {
            ProjectDiskPhysicalObservation::Conflicting
        };

        let mut bound_attachment = None;
        let (use_state, lock_state, resident_host_identity_bound) =
            if physical != ProjectDiskPhysicalObservation::Exact {
                (
                    ProjectDiskUseObservation::Unknown,
                    ProjectDiskLockObservation::Unknown,
                    false,
                )
            } else {
                match self.summary.disposition {
                    LimaStandaloneDiskDisposition::Detached => (
                        ProjectDiskUseObservation::Unused,
                        ProjectDiskLockObservation::Unlocked,
                        false,
                    ),
                    LimaStandaloneDiskDisposition::Attached => {
                        if let Some(resident) = resident_binding {
                            let (source, attachment) = resident.confirm()?;
                            let ProjectDiskLeaseState::Attached {
                                attachment: current,
                            } = record.state()
                            else {
                                return Err(binding_mismatch());
                            };
                            if attachment != current {
                                return Err(binding_mismatch());
                            }
                            if self.attached_source.as_ref() == Some(&source) {
                                bound_attachment = Some(current.clone());
                                (
                                    ProjectDiskUseObservation::CurrentAttachment,
                                    ProjectDiskLockObservation::CurrentAttachment,
                                    true,
                                )
                            } else {
                                (
                                    ProjectDiskUseObservation::Other,
                                    ProjectDiskLockObservation::Other,
                                    false,
                                )
                            }
                        } else {
                            (
                                ProjectDiskUseObservation::Other,
                                ProjectDiskLockObservation::Other,
                                false,
                            )
                        }
                    }
                    LimaStandaloneDiskDisposition::Conflicting => (
                        ProjectDiskUseObservation::Unknown,
                        ProjectDiskLockObservation::Unknown,
                        false,
                    ),
                }
            };

        let report = self.report(
            record,
            bound_attachment.as_ref(),
            physical,
            use_state,
            lock_state,
            resident_host_identity_bound,
        );
        self.confirm(fresh_inventory_json_lines)?;
        Ok(report)
    }

    fn report(
        &self,
        record: &ProjectDiskLeaseRecord,
        attachment: Option<&ProjectDiskAttachmentLease>,
        physical: ProjectDiskPhysicalObservation,
        use_state: ProjectDiskUseObservation,
        lock_state: ProjectDiskLockObservation,
        resident_host_identity_bound: bool,
    ) -> ProjectDiskHostObservationReport {
        ProjectDiskHostObservationReport {
            schema_version: PROJECT_DISK_HOST_OBSERVATION_SCHEMA_VERSION,
            binding: binding_summary(record, attachment),
            physical_identity: self.summary.physical_identity.clone(),
            backing_identity: self.summary.backing_identity.clone(),
            backing_logical_bytes: self.summary.backing_logical_bytes,
            backing_allocated_bytes: self.summary.backing_allocated_bytes,
            observation: ProjectDiskObservation::new(
                physical,
                use_state,
                lock_state,
                ProjectDiskRecoverability::Unknown,
            ),
            resident_host_identity_bound,
        }
    }
}

impl LimaStandaloneDiskObservation {
    /// Mint the crate-private #700 create-durability target from one uninterrupted created
    /// observation.
    ///
    /// Minting is fenced to the uninterrupted held-absence -> created-child lineage: only an
    /// observation returned by a successful [`LimaStandaloneDiskAbsenceObservation::observe_created`]
    /// carries the private created-lineage eligibility, and the planned configured source identity
    /// must be present as well. An ordinary later observation of an existing disk — even from a
    /// planned request naming the same locator — can never mint this target, so a post-restart or
    /// same-name rediscovery stays unbound physical evidence. Minting revalidates every held
    /// descriptor, path binding, and the supplied fresh inventory first, then binds a duplicated
    /// read-only held backing descriptor to the exact configured source identity, disk locator,
    /// physical identity, backing identity, and logical bytes. Explicit fixture requests are
    /// permanently ineligible. This performs no durability mutation itself and exposes no generic
    /// path/fd/write/sync authority; #700 owns the single reviewed barrier operation.
    ///
    /// # Errors
    ///
    /// Returns a bounded refusal for observations outside the uninterrupted created lineage,
    /// fixture-origin observations, and the ordinary P2 changed refusal when any held evidence no
    /// longer matches.
    pub(crate) fn project_disk_create_durability_target(
        &mut self,
        fresh_inventory_json_lines: &[u8],
    ) -> Result<ProjectDiskCreateDurabilityTarget, ProjectDiskHostObservationError> {
        if self.created_lineage.is_none() {
            return Err(durability_target_unavailable());
        }
        let Some(source_identity) = self.request.planned_source_identity.clone() else {
            return Err(durability_target_unavailable());
        };
        self.confirm(fresh_inventory_json_lines)?;
        let backing = self.backing.fd.try_clone().map_err(|_| io_error())?;
        if snapshot_file(&backing).map_err(|_| changed())? != self.backing.snapshot {
            return Err(changed());
        }
        Ok(ProjectDiskCreateDurabilityTarget {
            source_identity,
            disk_name: self.request.disk_name.clone(),
            physical_identity: self.summary.physical_identity.clone(),
            backing_identity: self.summary.backing_identity.clone(),
            backing_logical_bytes: self.summary.backing_logical_bytes,
            backing,
        })
    }
}

/// Crate-private opaque create-durability seam target required by #700.
///
/// Non-cloneable, non-copyable, and non-serializable. It is mintable only from an uninterrupted
/// held-absence -> created-child observation (`observe_created` success) whose full held
/// revalidation passed, and it binds the exact configured source identity, disk locator, physical
/// identity, backing identity, logical bytes, and one duplicated read-only held backing
/// descriptor. An ordinary later observation of an existing disk — even from a planned request
/// with the same name — can never mint it. It carries zero ownership, adoption, or mutation
/// authority and no generic path/fd/write/sync surface outside the narrow #700 barrier. No
/// attempt identity is carried here; cross-attempt fencing belongs to #700's non-cloneable proof.
pub struct ProjectDiskCreateDurabilityTarget {
    source_identity: ProjectDiskLimaSourceIdentity,
    disk_name: LimaStandaloneDiskName,
    physical_identity: ProjectDiskPhysicalIdentity,
    backing_identity: ProjectDiskBackingIdentity,
    backing_logical_bytes: u64,
    backing: OwnedFd,
}

impl ProjectDiskCreateDurabilityTarget {
    #[must_use]
    pub const fn source_identity(&self) -> &ProjectDiskLimaSourceIdentity {
        &self.source_identity
    }

    #[must_use]
    pub const fn disk_name(&self) -> &LimaStandaloneDiskName {
        &self.disk_name
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

    /// Test-only borrowed view of the exact held backing descriptor.
    ///
    /// This is deliberately unavailable to production crate callers: #700 must not receive a
    /// reusable generic fd capability. The reviewed #700 barrier has to consume the target behind
    /// a module-owned operation or closure defined in this module instead, and it must align the
    /// descriptor kind it requires (the #700 `fsync_volume_np` design names the held
    /// source/filesystem fd; this seam currently holds the backing fd of the same flush volume).
    #[cfg(test)]
    fn held_backing_descriptor(&self) -> BorrowedFd<'_> {
        self.backing.as_fd()
    }
}

impl fmt::Debug for ProjectDiskCreateDurabilityTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectDiskCreateDurabilityTarget")
            .field("source_identity", &self.source_identity)
            .field("disk_name", &self.disk_name)
            .field("physical_identity", &self.physical_identity)
            .field("backing_identity", &self.backing_identity)
            .field("backing_logical_bytes", &self.backing_logical_bytes)
            .field("private_backing_descriptor", &"<redacted>")
            .finish()
    }
}

impl fmt::Debug for LimaStandaloneDiskObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let _ = (
            &self.lima_home.fd,
            &self.collection.fd,
            &self.disk_directory.fd,
            &self.backing.fd,
            &self.instance_directory,
        );
        formatter
            .debug_struct("LimaStandaloneDiskObservation")
            .field("summary", &self.summary)
            .field("private_descriptors", &REDACTED_DESCRIPTORS)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDiskHostObservationErrorKind {
    InvalidInput,
    Missing,
    Present,
    UnsafeFilesystem,
    MalformedInventory,
    DuplicateInventory,
    UnsupportedSchema,
    BindingMismatch,
    ResidentMismatch,
    ChangedDuringObservation,
    Io,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiskHostObservationError {
    kind: ProjectDiskHostObservationErrorKind,
    code: &'static str,
    message: &'static str,
}

impl ProjectDiskHostObservationError {
    #[must_use]
    pub const fn kind(&self) -> ProjectDiskHostObservationErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for ProjectDiskHostObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectDiskHostObservationError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for ProjectDiskHostObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ProjectDiskHostObservationError {}

/// Observe one existing standalone disk from pre-captured Lima 2.2.0 JSON-lines inventory.
///
/// The observer executes no command. It classifies entry roles only from the physically accepted
/// one-regular-file/optional-one-symlink schema and immediately performs a complete held-descriptor
/// revalidation before returning.
///
/// # Errors
///
/// Fails closed on missing/unsafe objects, unreviewed inventory schema, unexpected direct entries,
/// or any rebind/drift during the observation window.
pub fn observe_lima_standalone_disk(
    request: LimaStandaloneDiskObservationRequest,
    inventory_json_lines: &[u8],
) -> Result<LimaStandaloneDiskObservation, ProjectDiskHostObservationError> {
    observe_with_hook(request, inventory_json_lines, || {})
}

/// Prove one planned standalone-disk locator absent without creating or adopting it.
///
/// The private Lima home must exist and is always retained. When its directly observed disk
/// collection exists it is validated and retained with exact planned-child absence; when the
/// collection itself is absent, first-disk bootstrap instead proves the exact collection child
/// absent beneath the held home. Both states additionally require the strict external inventory
/// to contain no record with that locator.
///
/// # Errors
///
/// Fails closed if the name exists, inventory reports it, a parent is unsafe, or any state changes
/// during the observation window.
pub fn observe_lima_standalone_disk_absence(
    request: LimaStandaloneDiskObservationRequest,
    inventory_json_lines: &[u8],
) -> Result<LimaStandaloneDiskAbsenceObservation, ProjectDiskHostObservationError> {
    observe_absence_with_hook(request, inventory_json_lines, || {})
}

fn observe_absence_with_hook<F>(
    mut request: LimaStandaloneDiskObservationRequest,
    inventory_json_lines: &[u8],
    before_revalidation: F,
) -> Result<LimaStandaloneDiskAbsenceObservation, ProjectDiskHostObservationError>
where
    F: FnOnce(),
{
    if select_inventory_optional(&request, inventory_json_lines)?.is_some() {
        return Err(present());
    }
    let lima_home = request.take_lima_home()?;
    validate_private_directory(&lima_home.snapshot)?;
    let collection = match BoundDirectory::open_child(&lima_home.fd, &request.collection_name) {
        Ok(collection) => {
            validate_private_directory(&collection.snapshot)?;
            require_entry_absent(&collection.fd, OsStr::new(request.disk_name.as_str()))?;
            HeldAbsenceCollection::Bound(collection)
        }
        Err(err) if err.kind() == ProjectDiskHostObservationErrorKind::Missing => {
            require_entry_absent(&lima_home.fd, &request.collection_name)?;
            HeldAbsenceCollection::ProvenAbsent
        }
        Err(err) => return Err(err),
    };
    let proven_collection_absent = matches!(collection, HeldAbsenceCollection::ProvenAbsent);
    let mut observation = LimaStandaloneDiskAbsenceObservation {
        summary: LimaStandaloneDiskAbsenceSummary {
            schema_version: PROJECT_DISK_HOST_OBSERVATION_SCHEMA_VERSION,
            disk_directory_absent: true,
            inventory_record_absent: true,
            proven_collection_absent,
            retained_lima_home_descriptor: true,
            retained_collection_descriptor: !proven_collection_absent,
        },
        request,
        lima_home,
        collection,
    };
    before_revalidation();
    observation.confirm(inventory_json_lines)?;
    Ok(observation)
}

fn observe_with_hook<F>(
    mut request: LimaStandaloneDiskObservationRequest,
    inventory_json_lines: &[u8],
    before_revalidation: F,
) -> Result<LimaStandaloneDiskObservation, ProjectDiskHostObservationError>
where
    F: FnOnce(),
{
    let lima_home = request.take_lima_home()?;
    validate_private_directory(&lima_home.snapshot)?;
    let collection = BoundDirectory::open_child(&lima_home.fd, &request.collection_name)?;
    validate_private_directory(&collection.snapshot)?;
    observe_existing_from_bound_parents(
        request,
        lima_home,
        collection,
        inventory_json_lines,
        before_revalidation,
        None,
        false,
    )
}

fn observe_existing_from_bound_parents<F>(
    request: LimaStandaloneDiskObservationRequest,
    lima_home: BoundDirectory,
    collection: BoundDirectory,
    inventory_json_lines: &[u8],
    before_revalidation: F,
    created_lineage: Option<CreatedLineage>,
    collection_retained_from_before_creation: bool,
) -> Result<LimaStandaloneDiskObservation, ProjectDiskHostObservationError>
where
    F: FnOnce(),
{
    let inventory = select_inventory(&request, inventory_json_lines)?;
    let disk_directory =
        BoundDirectory::open_child(&collection.fd, OsStr::new(request.disk_name.as_str()))?;
    validate_private_directory(&disk_directory.snapshot)?;

    let roles = observe_roles(&disk_directory.fd)?;
    let backing = BoundFile::open(&disk_directory.fd, &roles.backing_name)?;
    validate_backing_file(&backing.snapshot)?;
    if backing.snapshot != roles.backing_snapshot {
        return Err(changed());
    }

    let lock = roles.lock.map(|lock| BoundSymlink {
        name: lock.name,
        snapshot: lock.snapshot,
        target: lock.target,
    });
    let (disposition, instance_directory, attached_source) = correlate_state(
        &request,
        &inventory,
        lock.as_ref(),
        backing.snapshot.logical_bytes,
        &lima_home,
    )?;

    let backing_identity = derive_backing_identity(&roles.backing_name, &backing.snapshot)?;
    let physical_identity = derive_physical_identity(
        &request.collection_name,
        &request.disk_name,
        &disk_directory.snapshot,
        &backing_identity,
    )?;
    let mut observation = LimaStandaloneDiskObservation {
        summary: LimaStandaloneDiskObservationSummary {
            schema_version: PROJECT_DISK_HOST_OBSERVATION_SCHEMA_VERSION,
            disposition,
            physical_identity,
            backing_identity,
            backing_logical_bytes: backing.snapshot.logical_bytes,
            backing_allocated_bytes: backing.snapshot.allocated_bytes,
            inventory_logical_bytes: inventory.size,
            inventory_format_raw: inventory.format == "raw",
            retained_lima_home_descriptor: true,
            retained_collection_descriptor: true,
            retained_disk_directory_descriptor: true,
            retained_backing_descriptor: true,
            retained_instance_directory_descriptor: instance_directory.is_some(),
        },
        request,
        inventory,
        lima_home,
        collection,
        disk_directory,
        backing_name: roles.backing_name,
        backing,
        lock,
        instance_directory,
        attached_source,
        created_lineage,
        collection_retained_from_before_creation,
    };
    before_revalidation();
    let inventory_confirmation = inventory_json_lines.to_vec();
    observation.confirm(&inventory_confirmation)?;
    Ok(observation)
}

#[derive(Clone, PartialEq, Eq)]
struct AcceptedPath {
    supplied: PathBuf,
    physical: PathBuf,
    darwin_var_alias: Option<SystemAliasObservation>,
}

impl AcceptedPath {
    fn new(path: PathBuf) -> Result<Self, ProjectDiskHostObservationError> {
        validate_absolute_path(&path)?;
        let (physical, darwin_var_alias) = accepted_physical_path(&path)?;
        Ok(Self {
            supplied: path,
            physical,
            darwin_var_alias,
        })
    }

    fn revalidate_alias(&self) -> Result<(), ProjectDiskHostObservationError> {
        if let Some(expected) = &self.darwin_var_alias {
            let (physical, observed) = accepted_physical_path(&self.supplied)?;
            if physical != self.physical || observed.as_ref() != Some(expected) {
                return Err(changed());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SystemAliasObservation {
    device: u64,
    inode: u64,
    uid: u32,
    gid: u32,
    mode: u32,
    link_count: u64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
    target: Vec<u8>,
}

impl fmt::Debug for AcceptedPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<private-accepted-path>")
    }
}

fn validate_absolute_path(path: &Path) -> Result<(), ProjectDiskHostObservationError> {
    if !path.is_absolute()
        || path.as_os_str().as_bytes().len() > MAX_PATH_BYTES
        || path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(invalid_input());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn accepted_physical_path(
    path: &Path,
) -> Result<(PathBuf, Option<SystemAliasObservation>), ProjectDiskHostObservationError> {
    let mut components = path.components();
    let _ = components.next();
    if components.next() != Some(Component::Normal(OsStr::new("var"))) {
        return Ok((path.to_owned(), None));
    }

    let root =
        rustix_fs::open(Path::new("/"), DIRECTORY_FLAGS, Mode::empty()).map_err(|_| io_error())?;
    let alias = rustix_fs::statat(&root, "var", AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| unsafe_filesystem())?;
    if !FileType::from_raw_mode(alias.st_mode).is_symlink() || alias.st_uid != 0 {
        return Err(unsafe_filesystem());
    }
    let target = rustix_fs::readlinkat(&root, "var", Vec::new())
        .map_err(|_| unsafe_filesystem())?
        .into_bytes();
    if target != b"private/var" && target != b"/private/var" {
        return Err(unsafe_filesystem());
    }

    let suffix = path.strip_prefix("/var").map_err(|_| unsafe_filesystem())?;
    Ok((
        Path::new("/private/var").join(suffix),
        Some(SystemAliasObservation {
            device: u64::try_from(alias.st_dev).map_err(|_| unsafe_filesystem())?,
            inode: alias.st_ino,
            uid: alias.st_uid,
            gid: alias.st_gid,
            mode: u32::from(alias.st_mode),
            link_count: u64::from(alias.st_nlink),
            mtime: alias.st_mtime,
            mtime_nsec: alias.st_mtime_nsec,
            ctime: alias.st_ctime,
            ctime_nsec: alias.st_ctime_nsec,
            target,
        }),
    ))
}

#[cfg(not(target_os = "macos"))]
fn accepted_physical_path(
    path: &Path,
) -> Result<(PathBuf, Option<SystemAliasObservation>), ProjectDiskHostObservationError> {
    Ok((path.to_owned(), None))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InventoryWire {
    name: String,
    size: u64,
    format: String,
    dir: String,
    instance: String,
    #[serde(rename = "instanceDir")]
    instance_dir: String,
    #[serde(rename = "mountPoint")]
    mount_point: String,
}

#[derive(Clone, PartialEq, Eq)]
struct InventoryAttachment {
    instance: LimaInstanceName,
    instance_directory: AcceptedPath,
}

#[derive(Clone, PartialEq, Eq)]
enum InventoryUse {
    Detached,
    Attached(InventoryAttachment),
    Conflicting,
}

#[derive(Clone, PartialEq, Eq)]
struct SelectedInventory {
    size: u64,
    format: String,
    directory_matches: bool,
    use_state: InventoryUse,
}

impl fmt::Debug for SelectedInventory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SelectedInventory")
            .field("size", &self.size)
            .field("format", &self.format)
            .field("directory_matches", &self.directory_matches)
            .field("private_attachment", &"<redacted>")
            .finish()
    }
}

fn select_inventory(
    request: &LimaStandaloneDiskObservationRequest,
    bytes: &[u8],
) -> Result<SelectedInventory, ProjectDiskHostObservationError> {
    select_inventory_optional(request, bytes)?.ok_or_else(missing)
}

fn select_inventory_optional(
    request: &LimaStandaloneDiskObservationRequest,
    bytes: &[u8],
) -> Result<Option<SelectedInventory>, ProjectDiskHostObservationError> {
    if bytes.len() > MAX_INVENTORY_BYTES {
        return Err(malformed_inventory());
    }
    let mut selected = None;
    let mut records = 0_usize;
    for line in bytes.split(|byte| *byte == b'\n') {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        records = records.checked_add(1).ok_or_else(malformed_inventory)?;
        if records > MAX_INVENTORY_RECORDS {
            return Err(malformed_inventory());
        }
        let wire: InventoryWire =
            serde_json::from_slice(line).map_err(|_| malformed_inventory())?;
        if wire.name != request.disk_name.as_str() {
            LimaStandaloneDiskName::parse(&wire.name).map_err(|_| malformed_inventory())?;
            continue;
        }
        if selected.is_some() {
            return Err(duplicate_inventory());
        }
        if wire.size == 0 || wire.format != "raw" {
            return Err(unsupported_schema());
        }
        let directory = AcceptedPath::new(PathBuf::from(wire.dir))?;
        let mount_point = PathBuf::from(wire.mount_point);
        validate_absolute_path(&mount_point).map_err(|_| malformed_inventory())?;
        let use_state = match (wire.instance.is_empty(), wire.instance_dir.is_empty()) {
            (true, true) => InventoryUse::Detached,
            (false, false) => {
                let instance =
                    LimaInstanceName::parse(&wire.instance).map_err(|_| malformed_inventory())?;
                let instance_directory = AcceptedPath::new(PathBuf::from(wire.instance_dir))?;
                InventoryUse::Attached(InventoryAttachment {
                    instance,
                    instance_directory,
                })
            }
            _ => InventoryUse::Conflicting,
        };
        selected = Some(SelectedInventory {
            size: wire.size,
            format: wire.format,
            directory_matches: directory.physical == request.disk_directory.physical,
            use_state,
        });
    }
    Ok(selected)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DirectorySnapshot {
    device: u64,
    inode: u64,
    uid: u32,
    gid: u32,
    mode: u32,
    link_count: u64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileSnapshot {
    device: u64,
    inode: u64,
    uid: u32,
    gid: u32,
    mode: u32,
    link_count: u64,
    logical_bytes: u64,
    allocated_bytes: u64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SymlinkSnapshot {
    device: u64,
    inode: u64,
    uid: u32,
    gid: u32,
    mode: u32,
    link_count: u64,
    logical_bytes: u64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}

struct BoundDirectory {
    fd: OwnedFd,
    snapshot: DirectorySnapshot,
}

impl BoundDirectory {
    fn open_path(path: &AcceptedPath) -> Result<Self, ProjectDiskHostObservationError> {
        path.revalidate_alias()?;
        let mut fd =
            rustix_fs::open(Path::new("/"), DIRECTORY_FLAGS, Mode::empty()).map_err(map_open)?;
        for component in path.physical.components() {
            let Component::Normal(name) = component else {
                continue;
            };
            fd = rustix_fs::openat(&fd, name, DIRECTORY_FLAGS, Mode::empty()).map_err(map_open)?;
        }
        let snapshot = snapshot_directory(&fd)?;
        path.revalidate_alias()?;
        if path.darwin_var_alias.is_some() {
            let followed = rustix_fs::stat(&path.supplied).map_err(|_| changed())?;
            if snapshot_directory_stat(&followed).map_err(|_| changed())? != snapshot {
                return Err(changed());
            }
        }
        Ok(Self { fd, snapshot })
    }

    fn open_child(parent: &OwnedFd, name: &OsStr) -> Result<Self, ProjectDiskHostObservationError> {
        let fd =
            rustix_fs::openat(parent, name, DIRECTORY_FLAGS, Mode::empty()).map_err(map_open)?;
        let snapshot = snapshot_directory(&fd)?;
        Ok(Self { fd, snapshot })
    }

    fn revalidate(&self) -> Result<(), ProjectDiskHostObservationError> {
        if snapshot_directory(&self.fd).map_err(|_| changed())? != self.snapshot {
            return Err(changed());
        }
        Ok(())
    }

    /// Reconfirm only the stable physical identity of the held directory.
    ///
    /// The uninterrupted create transaction legitimately mutates the held home/collection
    /// directories (new child entries change their mutable timestamps and directory link count),
    /// so the live absence-to-created lineage proves the exact same filesystem object through
    /// device, inode, owner, group, and mode alone. Volatile fields stay reserved for the strict
    /// quiet-window confirmation.
    fn revalidate_stable_identity(&self) -> Result<(), ProjectDiskHostObservationError> {
        let current = snapshot_directory(&self.fd).map_err(|_| changed())?;
        if !same_stable_directory_identity(&current, &self.snapshot) {
            return Err(changed());
        }
        Ok(())
    }
}

fn same_stable_directory_identity(current: &DirectorySnapshot, held: &DirectorySnapshot) -> bool {
    current.device == held.device
        && current.inode == held.inode
        && current.uid == held.uid
        && current.gid == held.gid
        && current.mode == held.mode
}

struct BoundFile {
    fd: OwnedFd,
    snapshot: FileSnapshot,
}

impl BoundFile {
    fn open(parent: &OwnedFd, name: &OsStr) -> Result<Self, ProjectDiskHostObservationError> {
        let fd = rustix_fs::openat(parent, name, FILE_FLAGS, Mode::empty()).map_err(map_open)?;
        let snapshot = snapshot_file(&fd)?;
        Ok(Self { fd, snapshot })
    }

    fn revalidate(&self) -> Result<(), ProjectDiskHostObservationError> {
        if snapshot_file(&self.fd).map_err(|_| changed())? != self.snapshot {
            return Err(changed());
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq)]
struct BoundSymlink {
    name: OsString,
    snapshot: SymlinkSnapshot,
    target: AcceptedPath,
}

impl fmt::Debug for BoundSymlink {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BoundSymlink")
            .field("snapshot", &self.snapshot)
            .field("private_name_and_target", &"<redacted>")
            .finish()
    }
}

struct ObservedLock {
    name: OsString,
    snapshot: SymlinkSnapshot,
    target: AcceptedPath,
}

struct ObservedRoles {
    backing_name: OsString,
    backing_snapshot: FileSnapshot,
    lock: Option<ObservedLock>,
}

fn observe_roles(directory: &OwnedFd) -> Result<ObservedRoles, ProjectDiskHostObservationError> {
    let before = snapshot_directory(directory)?;
    let mut entries = Dir::read_from(directory).map_err(|_| io_error())?;
    let mut backing = None;
    let mut lock = None;
    let mut count = 0_u8;
    for entry in &mut entries {
        let entry = entry.map_err(|_| io_error())?;
        let name = entry.file_name();
        let name_bytes = name.to_bytes();
        if name_bytes == b"." || name_bytes == b".." {
            continue;
        }
        count = count.checked_add(1).ok_or_else(unsupported_schema)?;
        if count > 2 || name_bytes.is_empty() || name_bytes.len() > 255 {
            return Err(unsupported_schema());
        }
        let stat = rustix_fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| io_error())?;
        let file_type = FileType::from_raw_mode(stat.st_mode);
        if file_type.is_file() {
            if backing.is_some() {
                return Err(unsupported_schema());
            }
            backing = Some((
                OsString::from_vec(name_bytes.to_vec()),
                snapshot_file_stat(&stat)?,
            ));
        } else if file_type.is_symlink() {
            if lock.is_some() {
                return Err(unsupported_schema());
            }
            let snapshot = snapshot_symlink_stat(&stat)?;
            validate_lock_symlink(&snapshot)?;
            let raw_target =
                rustix_fs::readlinkat(directory, name, Vec::new()).map_err(|_| io_error())?;
            let target =
                AcceptedPath::new(PathBuf::from(OsString::from_vec(raw_target.into_bytes())))?;
            let after = rustix_fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|_| changed())?;
            if snapshot_symlink_stat(&after).map_err(|_| changed())? != snapshot {
                return Err(changed());
            }
            lock = Some(ObservedLock {
                name: OsString::from_vec(name_bytes.to_vec()),
                snapshot,
                target,
            });
        } else {
            return Err(unsupported_schema());
        }
    }
    let (backing_name, backing_snapshot) = backing.ok_or_else(unsupported_schema)?;
    if snapshot_directory(directory).map_err(|_| changed())? != before {
        return Err(changed());
    }
    Ok(ObservedRoles {
        backing_name,
        backing_snapshot,
        lock,
    })
}

fn correlate_state(
    request: &LimaStandaloneDiskObservationRequest,
    inventory: &SelectedInventory,
    lock: Option<&BoundSymlink>,
    backing_logical_bytes: u64,
    lima_home: &BoundDirectory,
) -> Result<
    (
        LimaStandaloneDiskDisposition,
        Option<BoundDirectory>,
        Option<LimaObservationSourceIdentity>,
    ),
    ProjectDiskHostObservationError,
> {
    if !inventory.directory_matches
        || inventory.size != backing_logical_bytes
        || inventory.format != "raw"
    {
        return Ok((LimaStandaloneDiskDisposition::Conflicting, None, None));
    }
    match (&inventory.use_state, lock) {
        (InventoryUse::Detached, None) => Ok((LimaStandaloneDiskDisposition::Detached, None, None)),
        (InventoryUse::Attached(attachment), Some(lock)) => {
            let expected_directory = request
                .lima_home
                .physical
                .join(attachment.instance.as_str());
            if attachment.instance_directory.physical != expected_directory
                || lock.target.physical != expected_directory
            {
                return Ok((LimaStandaloneDiskDisposition::Conflicting, None, None));
            }
            let instance = BoundDirectory::open_child(
                &lima_home.fd,
                OsStr::new(attachment.instance.as_str()),
            )?;
            validate_private_directory(&instance.snapshot)?;
            let source = LimaObservationSourceIdentity::from_validated(
                attachment.instance.clone(),
                request.lima_home.physical.clone(),
            );
            Ok((
                LimaStandaloneDiskDisposition::Attached,
                Some(instance),
                Some(source),
            ))
        }
        (InventoryUse::Conflicting, _) | (_, _) => {
            Ok((LimaStandaloneDiskDisposition::Conflicting, None, None))
        }
    }
}

fn revalidate_observation(
    observation: &mut LimaStandaloneDiskObservation,
    inventory_json_lines: &[u8],
) -> Result<(), ProjectDiskHostObservationError> {
    let collection_held_from_before_creation = observation.collection_retained_from_before_creation;
    observation.lima_home.revalidate()?;
    if collection_held_from_before_creation {
        // The retained collection descriptor was held across the mutating create window, so its
        // volatile metadata legitimately differs from the pre-creation snapshot; stable identity
        // plus descriptor-relative entry binding remains the exact-object proof.
        observation.collection.revalidate_stable_identity()?;
    } else {
        observation.collection.revalidate()?;
    }
    observation.disk_directory.revalidate()?;
    observation.backing.revalidate()?;

    require_same_directory_entry_discipline(
        &observation.lima_home.fd,
        &observation.request.collection_name,
        &observation.collection,
        collection_held_from_before_creation,
    )?;
    require_same_directory_entry(
        &observation.collection.fd,
        OsStr::new(observation.request.disk_name.as_str()),
        &observation.disk_directory,
    )?;
    require_same_file_entry(
        &observation.disk_directory.fd,
        &observation.backing_name,
        &observation.backing,
    )?;

    let roles = observe_roles(&observation.disk_directory.fd).map_err(|_| changed())?;
    if roles.backing_name != observation.backing_name
        || roles.backing_snapshot != observation.backing.snapshot
    {
        return Err(changed());
    }
    match (&observation.lock, roles.lock) {
        (None, None) => {}
        (Some(expected), Some(current))
            if expected.name == current.name
                && expected.snapshot == current.snapshot
                && expected.target == current.target => {}
        _ => return Err(changed()),
    }

    if let Some(instance) = &observation.instance_directory {
        instance.revalidate()?;
        let InventoryUse::Attached(attachment) = &observation.inventory.use_state else {
            return Err(changed());
        };
        require_same_directory_entry(
            &observation.lima_home.fd,
            OsStr::new(attachment.instance.as_str()),
            instance,
        )?;
    }

    let rebound_home =
        BoundDirectory::open_path(&observation.request.lima_home).map_err(|_| changed())?;
    if rebound_home.snapshot != observation.lima_home.snapshot {
        return Err(changed());
    }
    let rebound_disk =
        BoundDirectory::open_path(&observation.request.disk_directory).map_err(|_| changed())?;
    if rebound_disk.snapshot != observation.disk_directory.snapshot {
        return Err(changed());
    }

    let inventory =
        select_inventory(&observation.request, inventory_json_lines).map_err(|_| changed())?;
    if inventory != observation.inventory {
        return Err(changed());
    }
    observation.lima_home.revalidate()?;
    if collection_held_from_before_creation {
        observation.collection.revalidate_stable_identity()?;
    } else {
        observation.collection.revalidate()?;
    }
    observation.disk_directory.revalidate()?;
    observation.backing.revalidate()?;
    Ok(())
}

fn revalidate_absence(
    observation: &mut LimaStandaloneDiskAbsenceObservation,
    inventory_json_lines: &[u8],
) -> Result<(), ProjectDiskHostObservationError> {
    observation.lima_home.revalidate()?;
    match &observation.collection {
        HeldAbsenceCollection::Bound(collection) => {
            collection.revalidate()?;
            require_same_directory_entry(
                &observation.lima_home.fd,
                &observation.request.collection_name,
                collection,
            )?;
            require_entry_absent(
                &collection.fd,
                OsStr::new(observation.request.disk_name.as_str()),
            )?;
        }
        HeldAbsenceCollection::ProvenAbsent => {
            require_entry_absent(
                &observation.lima_home.fd,
                &observation.request.collection_name,
            )
            .map_err(|err| match err.kind() {
                ProjectDiskHostObservationErrorKind::Present => changed(),
                _ => err,
            })?;
        }
    }
    if select_inventory_optional(&observation.request, inventory_json_lines)
        .map_err(|_| changed())?
        .is_some()
    {
        return Err(changed());
    }
    let rebound_home =
        BoundDirectory::open_path(&observation.request.lima_home).map_err(|_| changed())?;
    if rebound_home.snapshot != observation.lima_home.snapshot {
        return Err(changed());
    }
    drop(rebound_home);
    observation.lima_home.revalidate()?;
    if let HeldAbsenceCollection::Bound(collection) = &observation.collection {
        collection.revalidate()?;
        require_entry_absent(
            &collection.fd,
            OsStr::new(observation.request.disk_name.as_str()),
        )?;
    } else {
        require_entry_absent(
            &observation.lima_home.fd,
            &observation.request.collection_name,
        )?;
    }
    Ok(())
}

fn require_entry_absent(
    parent: &OwnedFd,
    name: &OsStr,
) -> Result<(), ProjectDiskHostObservationError> {
    match rustix_fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Err(Errno::NOENT) => Ok(()),
        Ok(_) => Err(present()),
        Err(Errno::LOOP | Errno::NOTDIR) => Err(unsafe_filesystem()),
        Err(_) => Err(io_error()),
    }
}

fn require_same_directory_entry(
    parent: &OwnedFd,
    name: &OsStr,
    expected: &BoundDirectory,
) -> Result<(), ProjectDiskHostObservationError> {
    require_same_directory_entry_discipline(parent, name, expected, false)
}

/// Re-prove that `name` directly beneath `parent` is still exactly `expected`.
///
/// With `stable_only` the comparison proves device, inode, owner, group, and mode; this is the
/// correct discipline for a collection descriptor retained across the mutating create window,
/// whose timestamps and link count legitimately changed. Otherwise the full snapshot must match.
fn require_same_directory_entry_discipline(
    parent: &OwnedFd,
    name: &OsStr,
    expected: &BoundDirectory,
    stable_only: bool,
) -> Result<(), ProjectDiskHostObservationError> {
    let current = BoundDirectory::open_child(parent, name).map_err(|_| changed())?;
    let matches = if stable_only {
        same_stable_directory_identity(&current.snapshot, &expected.snapshot)
    } else {
        current.snapshot == expected.snapshot
    };
    if !matches {
        return Err(changed());
    }
    Ok(())
}

fn require_same_file_entry(
    parent: &OwnedFd,
    name: &OsStr,
    expected: &BoundFile,
) -> Result<(), ProjectDiskHostObservationError> {
    let current = BoundFile::open(parent, name).map_err(|_| changed())?;
    if current.snapshot != expected.snapshot {
        return Err(changed());
    }
    Ok(())
}

fn snapshot_directory(
    descriptor: &OwnedFd,
) -> Result<DirectorySnapshot, ProjectDiskHostObservationError> {
    let stat = rustix_fs::fstat(descriptor).map_err(|_| io_error())?;
    snapshot_directory_stat(&stat)
}

fn snapshot_directory_stat(
    stat: &rustix_fs::Stat,
) -> Result<DirectorySnapshot, ProjectDiskHostObservationError> {
    if !FileType::from_raw_mode(stat.st_mode).is_dir() {
        return Err(unsafe_filesystem());
    }
    Ok(DirectorySnapshot {
        device: stat_device(stat.st_dev)?,
        inode: stat.st_ino,
        uid: stat.st_uid,
        gid: stat.st_gid,
        mode: stat_mode(stat.st_mode),
        link_count: stat_link_count(stat.st_nlink)?,
        mtime: stat.st_mtime,
        mtime_nsec: stat_nanoseconds(stat.st_mtime_nsec)?,
        ctime: stat.st_ctime,
        ctime_nsec: stat_nanoseconds(stat.st_ctime_nsec)?,
    })
}

fn snapshot_file(descriptor: &OwnedFd) -> Result<FileSnapshot, ProjectDiskHostObservationError> {
    let stat = rustix_fs::fstat(descriptor).map_err(|_| io_error())?;
    snapshot_file_stat(&stat)
}

fn snapshot_file_stat(
    stat: &rustix::fs::Stat,
) -> Result<FileSnapshot, ProjectDiskHostObservationError> {
    if !FileType::from_raw_mode(stat.st_mode).is_file() {
        return Err(unsafe_filesystem());
    }
    let blocks = u64::try_from(stat.st_blocks).map_err(|_| unsafe_filesystem())?;
    Ok(FileSnapshot {
        device: stat_device(stat.st_dev)?,
        inode: stat.st_ino,
        uid: stat.st_uid,
        gid: stat.st_gid,
        mode: stat_mode(stat.st_mode),
        link_count: stat_link_count(stat.st_nlink)?,
        logical_bytes: u64::try_from(stat.st_size).map_err(|_| unsafe_filesystem())?,
        allocated_bytes: blocks
            .checked_mul(ALLOCATED_BLOCK_BYTES)
            .ok_or_else(unsafe_filesystem)?,
        mtime: stat.st_mtime,
        mtime_nsec: stat_nanoseconds(stat.st_mtime_nsec)?,
        ctime: stat.st_ctime,
        ctime_nsec: stat_nanoseconds(stat.st_ctime_nsec)?,
    })
}

fn snapshot_symlink_stat(
    stat: &rustix::fs::Stat,
) -> Result<SymlinkSnapshot, ProjectDiskHostObservationError> {
    if !FileType::from_raw_mode(stat.st_mode).is_symlink() {
        return Err(unsafe_filesystem());
    }
    Ok(SymlinkSnapshot {
        device: stat_device(stat.st_dev)?,
        inode: stat.st_ino,
        uid: stat.st_uid,
        gid: stat.st_gid,
        mode: stat_mode(stat.st_mode),
        link_count: stat_link_count(stat.st_nlink)?,
        logical_bytes: u64::try_from(stat.st_size).map_err(|_| unsafe_filesystem())?,
        mtime: stat.st_mtime,
        mtime_nsec: stat_nanoseconds(stat.st_mtime_nsec)?,
        ctime: stat.st_ctime,
        ctime_nsec: stat_nanoseconds(stat.st_ctime_nsec)?,
    })
}

#[cfg(target_os = "macos")]
const fn stat_nanoseconds(value: i64) -> Result<i64, ProjectDiskHostObservationError> {
    Ok(value)
}

#[cfg(target_os = "linux")]
fn stat_nanoseconds(value: u64) -> Result<i64, ProjectDiskHostObservationError> {
    i64::try_from(value).map_err(|_| unsafe_filesystem())
}

#[cfg(target_os = "macos")]
fn stat_device(value: i32) -> Result<u64, ProjectDiskHostObservationError> {
    u64::try_from(value).map_err(|_| unsafe_filesystem())
}

#[cfg(target_os = "linux")]
const fn stat_device(value: u64) -> Result<u64, ProjectDiskHostObservationError> {
    Ok(value)
}

#[cfg(target_os = "macos")]
fn stat_mode(value: u16) -> u32 {
    u32::from(value)
}

#[cfg(target_os = "linux")]
const fn stat_mode(value: u32) -> u32 {
    value
}

fn stat_link_count<T>(value: T) -> Result<u64, ProjectDiskHostObservationError>
where
    T: TryInto<u64>,
{
    value.try_into().map_err(|_| unsafe_filesystem())
}

fn validate_private_directory(
    snapshot: &DirectorySnapshot,
) -> Result<(), ProjectDiskHostObservationError> {
    if snapshot.uid != geteuid().as_raw() || snapshot.mode & 0o7777 != 0o700 {
        return Err(unsafe_filesystem());
    }
    Ok(())
}

fn validate_backing_file(snapshot: &FileSnapshot) -> Result<(), ProjectDiskHostObservationError> {
    if snapshot.uid != geteuid().as_raw()
        || snapshot.link_count != 1
        || snapshot.mode & 0o7777 != 0o600
        || snapshot.logical_bytes == 0
    {
        return Err(unsafe_filesystem());
    }
    Ok(())
}

fn validate_lock_symlink(
    snapshot: &SymlinkSnapshot,
) -> Result<(), ProjectDiskHostObservationError> {
    if snapshot.uid != geteuid().as_raw() || snapshot.link_count != 1 {
        return Err(unsafe_filesystem());
    }
    Ok(())
}

fn derive_backing_identity(
    name: &OsStr,
    snapshot: &FileSnapshot,
) -> Result<ProjectDiskBackingIdentity, ProjectDiskHostObservationError> {
    let digest = digest_fields(
        BACKING_IDENTITY_DOMAIN,
        [
            name.as_bytes(),
            &snapshot.device.to_be_bytes(),
            &snapshot.inode.to_be_bytes(),
            &snapshot.uid.to_be_bytes(),
            &snapshot.gid.to_be_bytes(),
            &snapshot.mode.to_be_bytes(),
            &snapshot.link_count.to_be_bytes(),
            &snapshot.logical_bytes.to_be_bytes(),
        ],
    )?;
    Ok(ProjectDiskBackingIdentity(digest))
}

fn derive_physical_identity(
    collection_name: &OsStr,
    disk_name: &LimaStandaloneDiskName,
    directory: &DirectorySnapshot,
    backing: &ProjectDiskBackingIdentity,
) -> Result<ProjectDiskPhysicalIdentity, ProjectDiskHostObservationError> {
    let digest = digest_fields(
        PHYSICAL_IDENTITY_DOMAIN,
        [
            collection_name.as_bytes(),
            disk_name.as_str().as_bytes(),
            &directory.device.to_be_bytes(),
            &directory.inode.to_be_bytes(),
            &directory.uid.to_be_bytes(),
            &directory.gid.to_be_bytes(),
            &directory.mode.to_be_bytes(),
            backing.digest().as_str().as_bytes(),
        ],
    )?;
    Ok(ProjectDiskPhysicalIdentity(digest))
}

fn digest_fields<'a>(
    domain: &[u8],
    fields: impl IntoIterator<Item = &'a [u8]>,
) -> Result<Sha256Digest, ProjectDiskHostObservationError> {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update([0]);
    for field in fields {
        digest.update((field.len() as u64).to_be_bytes());
        digest.update(field);
    }
    Sha256Digest::parse(&format!("sha256:{:x}", digest.finalize())).map_err(|_| io_error())
}

fn binding_summary(
    record: &ProjectDiskLeaseRecord,
    attachment: Option<&ProjectDiskAttachmentLease>,
) -> ProjectDiskLeaseObservationBindingSummary {
    ProjectDiskLeaseObservationBindingSummary {
        project: record.project().clone(),
        disk_id: record.disk_id().clone(),
        disk_generation: record.disk_generation(),
        disk_revision: record.revision(),
        attachment_generation: attachment.map(ProjectDiskAttachmentLease::generation),
        resident_sandbox_id: attachment.map(|value| value.sandbox_id().clone()),
        resident_sandbox_generation: attachment.map(ProjectDiskAttachmentLease::sandbox_generation),
    }
}

const fn error(
    kind: ProjectDiskHostObservationErrorKind,
    code: &'static str,
    message: &'static str,
) -> ProjectDiskHostObservationError {
    ProjectDiskHostObservationError {
        kind,
        code,
        message,
    }
}

const fn invalid_input() -> ProjectDiskHostObservationError {
    error(
        ProjectDiskHostObservationErrorKind::InvalidInput,
        "project_disk_observation_invalid_input",
        "project disk observation input is invalid",
    )
}

const fn missing() -> ProjectDiskHostObservationError {
    error(
        ProjectDiskHostObservationErrorKind::Missing,
        "project_disk_observation_missing",
        "project disk observation evidence is missing",
    )
}

const fn present() -> ProjectDiskHostObservationError {
    error(
        ProjectDiskHostObservationErrorKind::Present,
        "project_disk_observation_present",
        "project disk locator is present where absence was required",
    )
}

const fn unsafe_filesystem() -> ProjectDiskHostObservationError {
    error(
        ProjectDiskHostObservationErrorKind::UnsafeFilesystem,
        "project_disk_observation_unsafe_filesystem",
        "project disk observation found an unsafe filesystem shape",
    )
}

const fn malformed_inventory() -> ProjectDiskHostObservationError {
    error(
        ProjectDiskHostObservationErrorKind::MalformedInventory,
        "project_disk_inventory_malformed",
        "Lima project disk inventory is malformed",
    )
}

const fn duplicate_inventory() -> ProjectDiskHostObservationError {
    error(
        ProjectDiskHostObservationErrorKind::DuplicateInventory,
        "project_disk_inventory_duplicate",
        "Lima project disk inventory contains duplicate locator evidence",
    )
}

const fn unsupported_schema() -> ProjectDiskHostObservationError {
    error(
        ProjectDiskHostObservationErrorKind::UnsupportedSchema,
        "project_disk_schema_unsupported",
        "project disk evidence does not match the reviewed Lima 2.2.0 schema",
    )
}

const fn binding_mismatch() -> ProjectDiskHostObservationError {
    error(
        ProjectDiskHostObservationErrorKind::BindingMismatch,
        "project_disk_observation_binding_mismatch",
        "project disk observation does not match the expected P1 lease binding",
    )
}

const fn resident_mismatch() -> ProjectDiskHostObservationError {
    error(
        ProjectDiskHostObservationErrorKind::ResidentMismatch,
        "project_disk_resident_sandbox_mismatch",
        "project disk attachment does not match the descriptor-bound resident sandbox",
    )
}

const fn changed() -> ProjectDiskHostObservationError {
    error(
        ProjectDiskHostObservationErrorKind::ChangedDuringObservation,
        "project_disk_observation_changed",
        "project disk evidence changed or was rebound during observation",
    )
}

const fn durability_target_unavailable() -> ProjectDiskHostObservationError {
    error(
        ProjectDiskHostObservationErrorKind::InvalidInput,
        "project_disk_create_durability_target_unavailable",
        "create durability target requires an uninterrupted planned create observation",
    )
}

const fn io_error() -> ProjectDiskHostObservationError {
    error(
        ProjectDiskHostObservationErrorKind::Io,
        "project_disk_observation_io",
        "project disk evidence could not be observed",
    )
}

const fn map_open(cause: Errno) -> ProjectDiskHostObservationError {
    match cause {
        Errno::NOENT => missing(),
        Errno::LOOP | Errno::NOTDIR => unsafe_filesystem(),
        _ => io_error(),
    }
}

#[cfg(test)]
mod tests;
