//! Crate-private #700 create-durability acknowledgement for newly created Lima project disks.
//!
//! Lima 2.2.0 native raw creation performs no explicit host sync before `limactl disk create`
//! returns, so a durable P3 logical success could otherwise outlive the physical backing and
//! namespace entries still pending in host caches. This module closes exactly that gap for one
//! live create transaction and nothing else:
//!
//! 1. the uninterrupted held-absence -> created P2 observation mints the opaque
//!    [`ProjectDiskCreateDurabilityTarget`](crate::project_disk_host_observation::ProjectDiskCreateDurabilityTarget)
//!    carrying duplicated descriptors for the exact accepted chain;
//! 2. [`acknowledge_created_project_disk_durability`] revalidates the held source/created
//!    observation, walks the fixed leaf->root submission sequence (`fsync` exact backing, exact
//!    disk directory, exact collection, exact LIMA_HOME, then exactly one Apple
//!    `fcntl(F_FULLFSYNC)` hardware-cache barrier on the exact backing), revalidates again, and
//!    only then mints [`ProjectDiskCreateDurabilityProof`];
//! 3. the non-cloneable proof is consumed once to authorize staging the first exactly matching
//!    `CreatedRawBound` successor, emitting only the bounded serializable
//!    [`ProjectDiskCreateDurabilityAcknowledgement`] fact (policy generation + acknowledged).
//!
//! Any pre-barrier drift, submission failure, or post-barrier drift yields the bounded
//! `create_durability_unconfirmed` recovery debt with zero retry/delete/recreate/adoption/repair,
//! no success candidate, and no proof. The caller chooses nothing: no descriptor, path, flag,
//! order, strength, or retry policy is exposed.
//!
//! The genuine v1 policy ([`ProjectDiskCreateDurabilityPolicyGeneration::DarwinP2HeldChainFsyncThenBackingFullFsyncV1`])
//! exists only on macOS; every other platform refuses with a bounded policy-unavailable error.
//! Synthetic submitters exist only in tests and never claim physical durability.
//!
//! One-way ordering preserved across the whole P3 publication path (the later steps belong to
//! #697 and are deliberately not implemented here):
//!
//! ```text
//! physical create -> exact P2 created observation -> durability barrier -> post-barrier
//! revalidation -> staged logical success fsync -> final revalidation -> canonical rename +
//! store-parent fsync -> success return
//! ```
//!
//! Remaining dependency: #697 must consume [`ProjectDiskCreateDurabilityProof::
//! authorize_first_successor`] when building/staging its first `CreatedRawBound` candidate and
//! persist only the returned acknowledgement inside that successor's provenance. Until that
//! surface lands, this module retains the typed seam without fabricating any durable-success
//! path.
//!
//! P4 reuse contract: the reviewed primitive is reusable against an already-proven exact backing
//! observation after P4's own ownership/rebind admission. P4 will need its own fenced minting of a
//! durability target from its fresh same-object P2 evidence; that admission is intentionally not
//! implemented here, so P3 creation provenance never depends on P4 state and no generic sync
//! authority is introduced.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::project_catalog::ProjectIdentity;
use crate::project_disk_host_observation::{
    HeldLimaStandaloneDiskObservation, PROJECT_DISK_CREATE_DURABILITY_POLICY_UNAVAILABLE_CODE,
    PROJECT_DISK_CREATE_DURABILITY_SUBMISSION_SEQUENCE, ProjectDiskBackingIdentity,
    ProjectDiskCreateDurabilitySubmission, ProjectDiskCreateDurabilitySubmitter,
    ProjectDiskCreateDurabilityTarget, ProjectDiskLimaSourceIdentity, ProjectDiskPhysicalIdentity,
    genuine_project_disk_create_durability_submitter,
};
use crate::project_disk_lease::{ProjectDiskGeneration, ProjectDiskId, ProjectDiskRevision};

/// Schema version of the bounded durable acknowledgement fact.
pub const PROJECT_DISK_CREATE_DURABILITY_ACKNOWLEDGEMENT_SCHEMA_VERSION: u8 = 1;

const MAX_COMMAND_IDENTITY_BYTES: usize = 96;

/// Versioned create-durability policy generation.
///
/// The generation names the reviewed physical barrier semantics. A later policy may replace the
/// primitive only by advancing this generation; it can never silently weaken under the same
/// generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDiskCreateDurabilityPolicyGeneration {
    /// Safe-rustix macOS v1: `fsync` on the exact held backing/disk-directory/collection/LIMA_HOME
    /// chain, then one Apple `fcntl(F_FULLFSYNC)` hardware-cache barrier on the exact backing.
    DarwinP2HeldChainFsyncThenBackingFullFsyncV1,
}

/// Exact one-based create-attempt number within one project-disk generation entry revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ProjectDiskCreateAttempt(u32);

impl ProjectDiskCreateAttempt {
    /// Validate one attempt number.
    ///
    /// # Errors
    ///
    /// Refuses zero attempts.
    pub fn new(value: u32) -> Result<Self, ProjectDiskCreateDurabilityError> {
        if value == 0 {
            return Err(invalid_input());
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Opaque validated identity of the exact external create command execution.
///
/// The future #696/#697 command adapters construct this canonically for each live create
/// invocation; it is bound into proofs for cross-attempt fencing but never serialized into
/// durable state and always redacted from debug output.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProjectDiskCreateCommandIdentity(String);

impl ProjectDiskCreateCommandIdentity {
    /// Validate one opaque command identity token.
    ///
    /// # Errors
    ///
    /// Refuses empty, oversized, or non-printable-ASCII values.
    pub fn parse(value: &str) -> Result<Self, ProjectDiskCreateDurabilityError> {
        if value.is_empty()
            || value.len() > MAX_COMMAND_IDENTITY_BYTES
            || !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
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

impl fmt::Debug for ProjectDiskCreateCommandIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<create-command-identity>")
    }
}

/// The P3 transaction context one durability proof is bound to.
///
/// Every field identifies one exact live create transaction: the P1 lease binding plus the entry
/// revision, one-based attempt number, and exact command identity. Nothing here is derivable from
/// names, paths, or persisted receipts alone — the values come from the caller's verified live
/// transaction state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDiskCreateTransaction {
    project: ProjectIdentity,
    disk_id: ProjectDiskId,
    disk_generation: ProjectDiskGeneration,
    entry_revision: ProjectDiskRevision,
    attempt: ProjectDiskCreateAttempt,
    command: ProjectDiskCreateCommandIdentity,
}

impl ProjectDiskCreateTransaction {
    /// Assemble one exact create transaction context from already-validated typed fields.
    #[must_use]
    pub fn new(
        project: ProjectIdentity,
        disk_id: ProjectDiskId,
        disk_generation: ProjectDiskGeneration,
        entry_revision: ProjectDiskRevision,
        attempt: ProjectDiskCreateAttempt,
        command: ProjectDiskCreateCommandIdentity,
    ) -> Self {
        Self {
            project,
            disk_id,
            disk_generation,
            entry_revision,
            attempt,
            command,
        }
    }

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
    pub const fn entry_revision(&self) -> ProjectDiskRevision {
        self.entry_revision
    }

    #[must_use]
    pub const fn attempt(&self) -> ProjectDiskCreateAttempt {
        self.attempt
    }

    #[must_use]
    pub const fn command(&self) -> &ProjectDiskCreateCommandIdentity {
        &self.command
    }
}

/// The complete identity bundle bound by one durability proof and required of the first successor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDiskCreateDurabilityBinding {
    transaction: ProjectDiskCreateTransaction,
    source_identity: ProjectDiskLimaSourceIdentity,
    physical_identity: ProjectDiskPhysicalIdentity,
    backing_identity: ProjectDiskBackingIdentity,
    backing_logical_bytes: u64,
}

impl ProjectDiskCreateDurabilityBinding {
    /// Bind one exact transaction to one exact observed created object.
    #[must_use]
    pub fn new(
        transaction: ProjectDiskCreateTransaction,
        source_identity: ProjectDiskLimaSourceIdentity,
        physical_identity: ProjectDiskPhysicalIdentity,
        backing_identity: ProjectDiskBackingIdentity,
        backing_logical_bytes: u64,
    ) -> Self {
        Self {
            transaction,
            source_identity,
            physical_identity,
            backing_identity,
            backing_logical_bytes,
        }
    }

    #[must_use]
    pub const fn transaction(&self) -> &ProjectDiskCreateTransaction {
        &self.transaction
    }

    #[must_use]
    pub const fn source_identity(&self) -> &ProjectDiskLimaSourceIdentity {
        &self.source_identity
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
}

/// Bounded classification of one create-durability refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDiskCreateDurabilityErrorKind {
    InvalidInput,
    /// Held source/created evidence drifted before any submission ran.
    UnconfirmedBeforeBarrier,
    /// A fixed submission failed; the walk stopped at that step.
    UnconfirmedDuringBarrier,
    /// Every submission completed but post-barrier revalidation refused.
    UnconfirmedAfterBarrier,
    PolicyUnavailableOnHost,
    ProofMismatch,
}

/// Bounded create-durability refusal.
///
/// Recovery may classify only these kinds plus the failed submission code; raw errno values,
/// descriptor numbers, and private paths never appear in any field, message, or serialization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDiskCreateDurabilityError {
    kind: ProjectDiskCreateDurabilityErrorKind,
    failed_submission: Option<ProjectDiskCreateDurabilitySubmission>,
    code: &'static str,
    message: &'static str,
}

impl ProjectDiskCreateDurabilityError {
    #[must_use]
    pub const fn kind(&self) -> ProjectDiskCreateDurabilityErrorKind {
        self.kind
    }

    /// The exact submission that failed, when the walk stopped mid-sequence.
    #[must_use]
    pub const fn failed_submission(&self) -> Option<ProjectDiskCreateDurabilitySubmission> {
        self.failed_submission
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for ProjectDiskCreateDurabilityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ProjectDiskCreateDurabilityError {}

/// Private short-lived proof that the complete exact durability barrier succeeded for one live
/// create transaction and its exact observed created object.
///
/// Minted only by [`acknowledge_created_project_disk_durability`] after the full five-step
/// sequence and the post-barrier revalidation both succeeded. Not cloneable, copyable,
/// serializable, or deserializable: controller death destroys it, no restart or serialized receipt
/// reconstructs it, and it authorizes exactly one first successor whose binding matches field for
/// field. Disk A / attempt N can therefore never authorize disk B / attempt N+1.
pub struct ProjectDiskCreateDurabilityProof {
    binding: ProjectDiskCreateDurabilityBinding,
    policy: ProjectDiskCreateDurabilityPolicyGeneration,
    acknowledged: bool,
}

impl ProjectDiskCreateDurabilityProof {
    /// Consume this proof to authorize staging the first exactly matching successor.
    ///
    /// The proof is consumed by value whatever the outcome: a mismatched expectation refuses
    /// without leaving any replayable capability behind. On success it emits only the bounded
    /// durable acknowledgement fact that #697 persists inside the successor provenance.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectDiskCreateDurabilityErrorKind::ProofMismatch`] when the expected policy
    /// generation or any identity/attempt/command/bytes field differs from this proof's binding.
    pub fn authorize_first_successor(
        self,
        expected_policy: ProjectDiskCreateDurabilityPolicyGeneration,
        successor_binding: &ProjectDiskCreateDurabilityBinding,
    ) -> Result<ProjectDiskCreateDurabilityAcknowledgement, ProjectDiskCreateDurabilityError> {
        if self.policy != expected_policy || self.binding != *successor_binding {
            return Err(proof_mismatch());
        }
        Ok(ProjectDiskCreateDurabilityAcknowledgement {
            schema_version: PROJECT_DISK_CREATE_DURABILITY_ACKNOWLEDGEMENT_SCHEMA_VERSION,
            policy_generation: self.policy,
            acknowledged: true,
        })
    }

    #[must_use]
    pub const fn binding(&self) -> &ProjectDiskCreateDurabilityBinding {
        &self.binding
    }

    #[must_use]
    pub const fn policy(&self) -> ProjectDiskCreateDurabilityPolicyGeneration {
        self.policy
    }

    #[must_use]
    pub const fn acknowledged(&self) -> bool {
        self.acknowledged
    }
}

impl fmt::Debug for ProjectDiskCreateDurabilityProof {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectDiskCreateDurabilityProof")
            .field("policy", &self.policy)
            .field("project", self.binding.transaction.project())
            .field("disk_id", self.binding.transaction.disk_id())
            .field(
                "disk_generation",
                &self.binding.transaction.disk_generation().get(),
            )
            .field(
                "entry_revision",
                &self.binding.transaction.entry_revision().get(),
            )
            .field("create_attempt", &self.binding.transaction.attempt().get())
            .field("create_command", &"<redacted>")
            .field("source_identity", self.binding.source_identity())
            .field("physical_identity", self.binding.physical_identity())
            .field("backing_identity", self.binding.backing_identity())
            .field("backing_logical_bytes", &self.binding.backing_logical_bytes)
            .field("acknowledged", &self.acknowledged)
            .finish()
    }
}

/// Bounded durable fact recording why P3 considered the physical create durable.
///
/// This is the only durability data allowed to survive into durable state: a schema version, the
/// policy generation, and the acknowledged flag. It contains no descriptor, private path, fd, or
/// syscall detail and carries no mutation authority by itself; it interprets an existing
/// `CreatedRawBound` provenance record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectDiskCreateDurabilityAcknowledgement {
    schema_version: u8,
    policy_generation: ProjectDiskCreateDurabilityPolicyGeneration,
    acknowledged: bool,
}

impl ProjectDiskCreateDurabilityAcknowledgement {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn policy_generation(&self) -> ProjectDiskCreateDurabilityPolicyGeneration {
        self.policy_generation
    }

    #[must_use]
    pub const fn acknowledged(&self) -> bool {
        self.acknowledged
    }
}

/// Run the reviewed #700 durability barrier with the genuine host submitter and mint the proof.
///
/// See [`acknowledge_created_project_disk_durability_with_submitter`] for the exact semantics. On
/// non-macOS hosts this refuses with
/// [`ProjectDiskCreateDurabilityErrorKind::PolicyUnavailableOnHost`] before running anything.
///
/// # Errors
///
/// Returns the bounded create-durability refusals described on this module.
pub fn acknowledge_created_project_disk_durability(
    observation: &mut HeldLimaStandaloneDiskObservation,
    fresh_inventory_json_lines: &[u8],
    target: ProjectDiskCreateDurabilityTarget,
    transaction: ProjectDiskCreateTransaction,
) -> Result<ProjectDiskCreateDurabilityProof, ProjectDiskCreateDurabilityError> {
    let mut submitter =
        genuine_project_disk_create_durability_submitter(&target).map_err(|err| {
            if err.code() == PROJECT_DISK_CREATE_DURABILITY_POLICY_UNAVAILABLE_CODE {
                policy_unavailable()
            } else {
                unconfirmed_before_barrier(None)
            }
        })?;
    acknowledge_created_project_disk_durability_with_submitter(
        observation,
        fresh_inventory_json_lines,
        target,
        &transaction,
        submitter.as_mut(),
    )
}

/// Run the reviewed #700 durability barrier with an injected submitter.
///
/// This is the crate-private submitter-injection seam: production callers must use
/// [`acknowledge_created_project_disk_durability`], which constructs the single genuine submitter.
/// Synthetic submitters exist only in tests and never claim physical durability. The sequence is
/// fixed:
///
/// 1. exact target/observation consistency and pre-barrier held-evidence revalidation;
/// 2. the five fixed submissions in [`PROJECT_DISK_CREATE_DURABILITY_SUBMISSION_SEQUENCE`] order,
///    stopping permanently at the first failure;
/// 3. post-barrier held-evidence revalidation;
/// 4. proof minting bound to the moved target's identities plus the supplied transaction.
///
/// Any failure consumes the target without a proof, reports the bounded
/// `create_durability_unconfirmed` recovery debt class, and performs zero
/// retry/delete/recreate/adoption/repair. The canonical durable phase stays at `CreateStarted`
/// because no success candidate is written here or anywhere else in this module.
///
/// # Errors
///
/// Returns the bounded create-durability refusals described on this module.
pub fn acknowledge_created_project_disk_durability_with_submitter(
    observation: &mut HeldLimaStandaloneDiskObservation,
    fresh_inventory_json_lines: &[u8],
    target: ProjectDiskCreateDurabilityTarget,
    transaction: &ProjectDiskCreateTransaction,
    submitter: &mut dyn ProjectDiskCreateDurabilitySubmitter,
) -> Result<ProjectDiskCreateDurabilityProof, ProjectDiskCreateDurabilityError> {
    if target.physical_identity() != observation.summary().physical_identity()
        || target.backing_identity().digest() != observation.summary().backing_identity().digest()
        || target.backing_logical_bytes() != observation.summary().backing_logical_bytes()
    {
        return Err(target_mismatch());
    }
    observation
        .confirm(fresh_inventory_json_lines)
        .map_err(|_| unconfirmed_before_barrier(None))?;

    for submission in PROJECT_DISK_CREATE_DURABILITY_SUBMISSION_SEQUENCE {
        submitter
            .submit(submission)
            .map_err(|_| unconfirmed_during_barrier(Some(submission)))?;
    }

    observation
        .confirm(fresh_inventory_json_lines)
        .map_err(|_| unconfirmed_after_barrier(None))?;

    let binding = ProjectDiskCreateDurabilityBinding {
        transaction: transaction.clone(),
        source_identity: target.source_identity().clone(),
        physical_identity: target.physical_identity().clone(),
        backing_identity: ProjectDiskBackingIdentity::parse(
            target.backing_identity().digest().as_str(),
        )
        .expect("minted target backing identity digest is canonical SHA-256"),
        backing_logical_bytes: target.backing_logical_bytes(),
    };
    drop(target);
    Ok(ProjectDiskCreateDurabilityProof {
        binding,
        policy:
            ProjectDiskCreateDurabilityPolicyGeneration::DarwinP2HeldChainFsyncThenBackingFullFsyncV1,
        acknowledged: true,
    })
}

const fn invalid_input() -> ProjectDiskCreateDurabilityError {
    ProjectDiskCreateDurabilityError {
        kind: ProjectDiskCreateDurabilityErrorKind::InvalidInput,
        failed_submission: None,
        code: "project_disk_create_durability_invalid_input",
        message: "project disk create durability input is invalid",
    }
}

const fn target_mismatch() -> ProjectDiskCreateDurabilityError {
    ProjectDiskCreateDurabilityError {
        kind: ProjectDiskCreateDurabilityErrorKind::InvalidInput,
        failed_submission: None,
        code: "project_disk_create_durability_target_mismatch",
        message: "create durability target does not belong to the supplied held observation",
    }
}

const fn unconfirmed_before_barrier(
    failed: Option<ProjectDiskCreateDurabilitySubmission>,
) -> ProjectDiskCreateDurabilityError {
    ProjectDiskCreateDurabilityError {
        kind: ProjectDiskCreateDurabilityErrorKind::UnconfirmedBeforeBarrier,
        failed_submission: failed,
        code: "create_durability_unconfirmed",
        message: "project disk create durability is unconfirmed before the barrier",
    }
}

const fn unconfirmed_during_barrier(
    failed: Option<ProjectDiskCreateDurabilitySubmission>,
) -> ProjectDiskCreateDurabilityError {
    ProjectDiskCreateDurabilityError {
        kind: ProjectDiskCreateDurabilityErrorKind::UnconfirmedDuringBarrier,
        failed_submission: failed,
        code: "create_durability_unconfirmed",
        message: "project disk create durability is unconfirmed during the barrier",
    }
}

const fn unconfirmed_after_barrier(
    failed: Option<ProjectDiskCreateDurabilitySubmission>,
) -> ProjectDiskCreateDurabilityError {
    ProjectDiskCreateDurabilityError {
        kind: ProjectDiskCreateDurabilityErrorKind::UnconfirmedAfterBarrier,
        failed_submission: failed,
        code: "create_durability_unconfirmed",
        message: "project disk create durability is unconfirmed after the barrier",
    }
}

const fn policy_unavailable() -> ProjectDiskCreateDurabilityError {
    ProjectDiskCreateDurabilityError {
        kind: ProjectDiskCreateDurabilityErrorKind::PolicyUnavailableOnHost,
        failed_submission: None,
        code: "project_disk_create_durability_policy_unavailable_on_host",
        message: "the reviewed project disk create durability policy is unavailable on this host",
    }
}

const fn proof_mismatch() -> ProjectDiskCreateDurabilityError {
    ProjectDiskCreateDurabilityError {
        kind: ProjectDiskCreateDurabilityErrorKind::ProofMismatch,
        failed_submission: None,
        code: "create_durability_proof_mismatch",
        message: "create durability proof does not match the requested successor or policy",
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::project_disk_host_observation::{
        ConfiguredProjectDiskLimaSource, LimaStandaloneDiskName,
        LimaStandaloneDiskObservationRequest, ProjectDiskBackingIdentity,
        ProjectDiskHostObservationError, ProjectDiskLimaSourceIdentity,
        ProjectDiskPhysicalIdentity, observe_lima_standalone_disk,
        observe_lima_standalone_disk_absence,
    };

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);
    const DISK_BYTES: u64 = 1024 * 1024;

    struct Fixture {
        root: PathBuf,
        lima_home: PathBuf,
        disk_directory: PathBuf,
        backing: PathBuf,
        disk_name: LimaStandaloneDiskName,
        source: ConfiguredProjectDiskLimaSource,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "smolrunner-project-disk-durability-{tag}-{}-{}",
                std::process::id(),
                NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
            let lima_home = root.join("lima");
            fs::create_dir(&lima_home).unwrap();
            fs::set_permissions(&lima_home, fs::Permissions::from_mode(0o700)).unwrap();
            let disk_name = LimaStandaloneDiskName::parse("durability-disk").unwrap();
            let disk_directory = lima_home.join("_disks").join(disk_name.as_str());
            let backing = disk_directory.join("opaque-regular-entry");
            let source = ConfiguredProjectDiskLimaSource::new(&lima_home).unwrap();
            Self {
                root,
                lima_home,
                disk_directory,
                backing,
                disk_name,
                source,
            }
        }

        fn planned_request(&self) -> LimaStandaloneDiskObservationRequest {
            LimaStandaloneDiskObservationRequest::for_planned_disk(
                &self.source,
                self.disk_name.clone(),
            )
            .unwrap()
        }

        /// Stand in for the external `limactl disk create`: `_disks/<locator>` plus one exact
        /// size/mode backing entry, exactly like the accepted P2 fixtures.
        fn external_create(&self) {
            fs::create_dir_all(&self.disk_directory).unwrap();
            let collection = self.lima_home.join("_disks");
            fs::set_permissions(&collection, fs::Permissions::from_mode(0o700)).unwrap();
            fs::set_permissions(&self.disk_directory, fs::Permissions::from_mode(0o700)).unwrap();
            let file = File::create(&self.backing).unwrap();
            file.set_len(DISK_BYTES).unwrap();
            drop(file);
            fs::set_permissions(&self.backing, fs::Permissions::from_mode(0o600)).unwrap();
        }

        /// Replace the backing entry so every held snapshot comparison refuses.
        fn replace_backing(&self) {
            fs::remove_file(&self.backing).unwrap();
            let file = File::create(&self.backing).unwrap();
            file.set_len(DISK_BYTES).unwrap();
            drop(file);
            fs::set_permissions(&self.backing, fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    struct LiveCreate {
        fixture: Fixture,
        inventory: Vec<u8>,
        observation: HeldLimaStandaloneDiskObservation,
    }

    /// One uninterrupted planned absence -> created lineage with a live held observation.
    fn live_created_lineage(tag: &str) -> LiveCreate {
        let fixture = Fixture::new(tag);
        let absent = observe_lima_standalone_disk_absence(fixture.planned_request(), &[]).unwrap();
        fixture.external_create();
        let inventory = inventory_for(&fixture);
        let observation = absent.observe_created(&inventory).unwrap();
        LiveCreate {
            fixture,
            inventory,
            observation,
        }
    }

    fn inventory_for(fixture: &Fixture) -> Vec<u8> {
        format!(
            "{{\"name\":\"{}\",\"size\":{DISK_BYTES},\"format\":\"raw\",\"dir\":\"{}\",\"instance\":\"\",\"instanceDir\":\"\",\"mountPoint\":\"/mnt/durability\"}}\n",
            fixture.disk_name.as_str(),
            fixture.disk_directory.display()
        )
        .into_bytes()
    }

    fn mint_target(live: &mut LiveCreate) -> ProjectDiskCreateDurabilityTarget {
        live.observation
            .project_disk_create_durability_target(&live.inventory)
            .unwrap()
    }

    fn project() -> ProjectIdentity {
        ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap()
    }

    fn transaction(attempt: u32) -> ProjectDiskCreateTransaction {
        ProjectDiskCreateTransaction::new(
            project(),
            ProjectDiskId::parse("disk-a").unwrap(),
            ProjectDiskGeneration::new(3).unwrap(),
            ProjectDiskRevision::new(7).unwrap(),
            ProjectDiskCreateAttempt::new(attempt).unwrap(),
            ProjectDiskCreateCommandIdentity::parse("limactl-create-cmd-1").unwrap(),
        )
    }

    fn binding_for(
        live: &LiveCreate,
        tx: &ProjectDiskCreateTransaction,
    ) -> ProjectDiskCreateDurabilityBinding {
        ProjectDiskCreateDurabilityBinding::new(
            tx.clone(),
            live.fixture.source.identity().clone(),
            live.observation.summary().physical_identity().clone(),
            ProjectDiskBackingIdentity::parse(
                live.observation
                    .summary()
                    .backing_identity()
                    .digest()
                    .as_str(),
            )
            .unwrap(),
            live.observation.summary().backing_logical_bytes(),
        )
    }

    fn rebinding_with_transaction(
        base: &ProjectDiskCreateDurabilityBinding,
        tx: ProjectDiskCreateTransaction,
    ) -> ProjectDiskCreateDurabilityBinding {
        ProjectDiskCreateDurabilityBinding::new(
            tx,
            base.source_identity().clone(),
            base.physical_identity().clone(),
            base.backing_identity().clone(),
            base.backing_logical_bytes(),
        )
    }

    fn any_host_error() -> ProjectDiskHostObservationError {
        LimaStandaloneDiskName::parse("").unwrap_err()
    }

    /// Synthetic submitter proving exact ordering and failure semantics without claiming any
    /// physical durability.
    struct SpySubmitter {
        fail_at: Option<ProjectDiskCreateDurabilitySubmission>,
        hook: Option<Box<dyn FnMut(ProjectDiskCreateDurabilitySubmission)>>,
        attempted: Vec<ProjectDiskCreateDurabilitySubmission>,
    }

    impl SpySubmitter {
        fn succeeding() -> Self {
            Self {
                fail_at: None,
                hook: None,
                attempted: Vec::new(),
            }
        }

        fn failing_at(step: ProjectDiskCreateDurabilitySubmission) -> Self {
            Self {
                fail_at: Some(step),
                hook: None,
                attempted: Vec::new(),
            }
        }
    }

    impl ProjectDiskCreateDurabilitySubmitter for SpySubmitter {
        fn submit(
            &mut self,
            submission: ProjectDiskCreateDurabilitySubmission,
        ) -> Result<(), ProjectDiskHostObservationError> {
            self.attempted.push(submission);
            if let Some(hook) = &mut self.hook {
                hook(submission);
            }
            if self.fail_at == Some(submission) {
                return Err(any_host_error());
            }
            Ok(())
        }
    }

    fn run_barrier(
        live: &mut LiveCreate,
        tx: &ProjectDiskCreateTransaction,
        submitter: &mut dyn ProjectDiskCreateDurabilitySubmitter,
    ) -> Result<ProjectDiskCreateDurabilityProof, ProjectDiskCreateDurabilityError> {
        let target = mint_target(live);
        acknowledge_created_project_disk_durability_with_submitter(
            &mut live.observation,
            &live.inventory,
            target,
            tx,
            submitter,
        )
    }

    fn minted_success_proof(
        tag: &str,
        attempt: u32,
    ) -> (
        ProjectDiskCreateDurabilityBinding,
        ProjectDiskCreateDurabilityProof,
    ) {
        let mut live = live_created_lineage(tag);
        let tx = transaction(attempt);
        let binding = binding_for(&live, &tx);
        let mut spy = SpySubmitter::succeeding();
        let proof = run_barrier(&mut live, &tx, &mut spy).unwrap();
        (binding, proof)
    }

    #[test]
    fn submission_sequence_is_exact_leaf_to_root_with_single_final_fullsync() {
        assert_eq!(
            PROJECT_DISK_CREATE_DURABILITY_SUBMISSION_SEQUENCE,
            [
                ProjectDiskCreateDurabilitySubmission::BackingData,
                ProjectDiskCreateDurabilitySubmission::DiskDirectoryEntry,
                ProjectDiskCreateDurabilitySubmission::CollectionEntry,
                ProjectDiskCreateDurabilitySubmission::LimaHomeEntry,
                ProjectDiskCreateDurabilitySubmission::BackingHardwareCacheBarrier,
            ]
        );
        assert_eq!(
            PROJECT_DISK_CREATE_DURABILITY_SUBMISSION_SEQUENCE
                .iter()
                .filter(|step| **step
                    == ProjectDiskCreateDurabilitySubmission::BackingHardwareCacheBarrier)
                .count(),
            1
        );
        assert_eq!(
            PROJECT_DISK_CREATE_DURABILITY_SUBMISSION_SEQUENCE
                .last()
                .copied(),
            Some(ProjectDiskCreateDurabilitySubmission::BackingHardwareCacheBarrier)
        );
    }

    #[test]
    fn successful_barrier_walks_exact_order_mints_proof_and_authorizes_once() {
        let mut live = live_created_lineage("success");
        let tx = transaction(2);
        let expected_binding = binding_for(&live, &tx);
        let mut spy = SpySubmitter::succeeding();
        let proof = run_barrier(&mut live, &tx, &mut spy).unwrap();

        assert_eq!(
            spy.attempted,
            PROJECT_DISK_CREATE_DURABILITY_SUBMISSION_SEQUENCE
        );
        assert_eq!(
            proof.policy(),
            ProjectDiskCreateDurabilityPolicyGeneration::DarwinP2HeldChainFsyncThenBackingFullFsyncV1
        );
        assert!(proof.acknowledged());
        assert_eq!(proof.binding(), &expected_binding);

        // Consume-once: `authorize_first_successor` takes the proof by value, so this single
        // matching authorization is the only one the minted proof can ever produce.
        let acknowledgement = proof
            .authorize_first_successor(
                ProjectDiskCreateDurabilityPolicyGeneration::DarwinP2HeldChainFsyncThenBackingFullFsyncV1,
                &expected_binding,
            )
            .unwrap();
        assert_eq!(
            acknowledgement.schema_version(),
            PROJECT_DISK_CREATE_DURABILITY_ACKNOWLEDGEMENT_SCHEMA_VERSION
        );
        assert!(acknowledgement.acknowledged());

        let json = serde_json::to_string(&acknowledgement).unwrap();
        assert_eq!(
            json,
            "{\"schema_version\":1,\"policy_generation\":\"darwin_p2_held_chain_fsync_then_backing_full_fsync_v1\",\"acknowledged\":true}"
        );
        assert!(!json.contains(live.fixture.root.to_str().unwrap()));
        assert!(!json.contains("opaque-regular-entry"));
        assert!(!json.contains("descriptor"));
    }

    #[test]
    fn each_failed_submission_stops_without_proof_or_later_steps() {
        for failed in PROJECT_DISK_CREATE_DURABILITY_SUBMISSION_SEQUENCE {
            let mut live = live_created_lineage("fail-step");
            let tx = transaction(1);
            let mut spy = SpySubmitter::failing_at(failed);
            let error = run_barrier(&mut live, &tx, &mut spy).unwrap_err();

            let position = PROJECT_DISK_CREATE_DURABILITY_SUBMISSION_SEQUENCE
                .iter()
                .position(|step| *step == failed)
                .unwrap();
            assert_eq!(
                error.kind(),
                ProjectDiskCreateDurabilityErrorKind::UnconfirmedDuringBarrier
            );
            assert_eq!(error.code(), "create_durability_unconfirmed");
            assert_eq!(error.failed_submission(), Some(failed));
            assert_eq!(
                failed.code(),
                error.failed_submission().map(|s| s.code()).unwrap()
            );
            assert_eq!(
                spy.attempted,
                PROJECT_DISK_CREATE_DURABILITY_SUBMISSION_SEQUENCE[..=position]
            );
        }
    }

    #[test]
    fn pre_barrier_drift_refuses_before_any_submission() {
        let mut live = live_created_lineage("pre-drift");
        let tx = transaction(1);
        let target = mint_target(&mut live);
        live.fixture.replace_backing();
        let mut spy = SpySubmitter::succeeding();
        let error = acknowledge_created_project_disk_durability_with_submitter(
            &mut live.observation,
            &live.inventory,
            target,
            &tx,
            &mut spy,
        )
        .unwrap_err();

        assert_eq!(
            error.kind(),
            ProjectDiskCreateDurabilityErrorKind::UnconfirmedBeforeBarrier
        );
        assert_eq!(error.code(), "create_durability_unconfirmed");
        assert!(error.failed_submission().is_none());
        assert!(spy.attempted.is_empty());
    }

    #[test]
    fn post_barrier_drift_refuses_after_complete_sequence_without_proof() {
        let mut live = live_created_lineage("post-drift");
        let tx = transaction(1);
        let sabotaged_backing = live.fixture.backing.clone();
        let sabotage_hook = move |submission: ProjectDiskCreateDurabilitySubmission| {
            if submission == ProjectDiskCreateDurabilitySubmission::BackingHardwareCacheBarrier {
                fs::remove_file(&sabotaged_backing).unwrap();
                let file = File::create(&sabotaged_backing).unwrap();
                file.set_len(DISK_BYTES).unwrap();
                drop(file);
                fs::set_permissions(&sabotaged_backing, fs::Permissions::from_mode(0o600)).unwrap();
            }
        };
        let mut spy = SpySubmitter {
            fail_at: None,
            hook: Some(Box::new(sabotage_hook)),
            attempted: Vec::new(),
        };
        let error = run_barrier(&mut live, &tx, &mut spy).unwrap_err();

        assert_eq!(
            error.kind(),
            ProjectDiskCreateDurabilityErrorKind::UnconfirmedAfterBarrier
        );
        assert_eq!(error.code(), "create_durability_unconfirmed");
        assert!(error.failed_submission().is_none());
        assert_eq!(
            spy.attempted,
            PROJECT_DISK_CREATE_DURABILITY_SUBMISSION_SEQUENCE
        );
    }

    #[test]
    fn proof_authorization_refuses_any_binding_mismatch() {
        let policy =
            ProjectDiskCreateDurabilityPolicyGeneration::DarwinP2HeldChainFsyncThenBackingFullFsyncV1;

        fn assert_refused(
            policy: ProjectDiskCreateDurabilityPolicyGeneration,
            wrong: &ProjectDiskCreateDurabilityBinding,
            label: &str,
        ) {
            let (_binding, proof) = minted_success_proof("authorize-mismatch", 4);
            let error = proof.authorize_first_successor(policy, wrong).unwrap_err();
            assert_eq!(
                error.kind(),
                ProjectDiskCreateDurabilityErrorKind::ProofMismatch,
                "{label}"
            );
            assert_eq!(error.code(), "create_durability_proof_mismatch", "{label}");
        }

        let (template, _) = minted_success_proof("authorize-template", 4);

        assert_refused(
            policy,
            &rebinding_with_transaction(&template, transaction(5)),
            "attempt",
        );

        let mut disk_tx = transaction(4);
        disk_tx.disk_id = ProjectDiskId::parse("disk-b").unwrap();
        assert_refused(
            policy,
            &rebinding_with_transaction(&template, disk_tx),
            "disk",
        );

        let mut generation_tx = transaction(4);
        generation_tx.disk_generation = ProjectDiskGeneration::new(9).unwrap();
        assert_refused(
            policy,
            &rebinding_with_transaction(&template, generation_tx),
            "generation",
        );

        let mut revision_tx = transaction(4);
        revision_tx.entry_revision = ProjectDiskRevision::new(8).unwrap();
        assert_refused(
            policy,
            &rebinding_with_transaction(&template, revision_tx),
            "revision",
        );

        let mut command_tx = transaction(4);
        command_tx.command =
            ProjectDiskCreateCommandIdentity::parse("limactl-create-cmd-2").unwrap();
        assert_refused(
            policy,
            &rebinding_with_transaction(&template, command_tx),
            "command",
        );

        assert_refused(
            policy,
            &ProjectDiskCreateDurabilityBinding::new(
                template.transaction().clone(),
                ProjectDiskLimaSourceIdentity::parse(&format!("sha256:{}", "b".repeat(64)))
                    .unwrap(),
                template.physical_identity().clone(),
                template.backing_identity().clone(),
                template.backing_logical_bytes(),
            ),
            "source",
        );
        assert_refused(
            policy,
            &ProjectDiskCreateDurabilityBinding::new(
                template.transaction().clone(),
                template.source_identity().clone(),
                ProjectDiskPhysicalIdentity::parse(&format!("sha256:{}", "c".repeat(64))).unwrap(),
                template.backing_identity().clone(),
                template.backing_logical_bytes(),
            ),
            "physical",
        );
        assert_refused(
            policy,
            &ProjectDiskCreateDurabilityBinding::new(
                template.transaction().clone(),
                template.source_identity().clone(),
                template.physical_identity().clone(),
                ProjectDiskBackingIdentity::parse(&format!("sha256:{}", "d".repeat(64))).unwrap(),
                template.backing_logical_bytes(),
            ),
            "backing",
        );
        assert_refused(
            policy,
            &ProjectDiskCreateDurabilityBinding::new(
                template.transaction().clone(),
                template.source_identity().clone(),
                template.physical_identity().clone(),
                template.backing_identity().clone(),
                template.backing_logical_bytes() + 1,
            ),
            "bytes",
        );

        // A foreign policy generation value cannot even be constructed from persisted input:
        // unknown generations refuse deserialization, and successor authorization compares the
        // expected generation against the proof before anything else.
        serde_json::from_str::<ProjectDiskCreateDurabilityPolicyGeneration>(
            "\"future_unknown_policy_generation\"",
        )
        .unwrap_err();

        let (binding, proof) = minted_success_proof("authorize-ok", 4);
        let acknowledgement = proof.authorize_first_successor(policy, &binding).unwrap();
        assert!(acknowledgement.acknowledged());
        assert_eq!(acknowledgement.policy_generation(), policy);
    }

    #[test]
    fn foreign_target_refuses_against_observation_before_any_submission() {
        let mut live_a = live_created_lineage("foreign-a");
        let mut live_b = live_created_lineage("foreign-b");
        let target_a = mint_target(&mut live_a);
        let tx_b = transaction(1);
        let mut spy = SpySubmitter::succeeding();
        let error = acknowledge_created_project_disk_durability_with_submitter(
            &mut live_b.observation,
            &live_b.inventory,
            target_a,
            &tx_b,
            &mut spy,
        )
        .unwrap_err();

        assert_eq!(
            error.kind(),
            ProjectDiskCreateDurabilityErrorKind::InvalidInput
        );
        assert_eq!(
            error.code(),
            "project_disk_create_durability_target_mismatch"
        );
        assert!(spy.attempted.is_empty());
    }

    #[test]
    fn debug_output_redacts_private_details() {
        let mut live = live_created_lineage("redact");
        let tx = transaction(1);
        let target = mint_target(&mut live);
        let target_debug = format!("{target:?}");
        assert!(!target_debug.contains(live.fixture.root.to_str().unwrap()));
        assert!(!target_debug.contains("opaque-regular-entry"));
        assert!(!target_debug.contains("OwnedFd"));

        let mut spy = SpySubmitter::succeeding();
        let proof = acknowledge_created_project_disk_durability_with_submitter(
            &mut live.observation,
            &live.inventory,
            target,
            &tx,
            &mut spy,
        )
        .unwrap();
        let proof_debug = format!("{proof:?}");
        assert!(!proof_debug.contains(live.fixture.root.to_str().unwrap()));
        assert!(!proof_debug.contains("opaque-regular-entry"));
        assert!(proof_debug.contains("acknowledged: true"));
        assert!(proof_debug.contains("<redacted>"));

        let mut failing = SpySubmitter::failing_at(
            ProjectDiskCreateDurabilitySubmission::BackingHardwareCacheBarrier,
        );
        let replacement = mint_target(&mut live);
        let error = acknowledge_created_project_disk_durability_with_submitter(
            &mut live.observation,
            &live.inventory,
            replacement,
            &tx,
            &mut failing,
        )
        .unwrap_err();
        let error_text = format!("{error:?} {error}");
        assert!(!error_text.contains(live.fixture.root.to_str().unwrap()));
        assert!(!error_text.contains("opaque-regular-entry"));
    }

    #[test]
    fn transaction_validation_refuses_invalid_attempt_or_command_identity() {
        assert_eq!(
            ProjectDiskCreateAttempt::new(0).unwrap_err().code(),
            "project_disk_create_durability_invalid_input"
        );
        for bad_command in [
            "",
            "has space",
            "has\ttab",
            &"x".repeat(MAX_COMMAND_IDENTITY_BYTES + 1),
        ] {
            assert!(
                ProjectDiskCreateCommandIdentity::parse(bad_command).is_err(),
                "{bad_command}"
            );
        }
        let tx = transaction(1);
        assert_eq!(tx.attempt().get(), 1);
        assert_eq!(tx.command().as_str(), "limactl-create-cmd-1");
    }

    #[test]
    fn controller_restart_leaves_only_unbound_evidence_and_the_bounded_ack_fact() {
        let mut live = live_created_lineage("restart");
        let tx = transaction(1);
        let binding = binding_for(&live, &tx);
        let mut spy = SpySubmitter::succeeding();
        let proof = run_barrier(&mut live, &tx, &mut spy).unwrap();
        let acknowledgement = proof
            .authorize_first_successor(
                ProjectDiskCreateDurabilityPolicyGeneration::DarwinP2HeldChainFsyncThenBackingFullFsyncV1,
                &binding,
            )
            .unwrap();
        let ack_json = serde_json::to_string(&acknowledgement).unwrap();

        // Controller "restart": every live object drops. Nothing serialized reconstructs the
        // proof or target — they are neither Clone nor Deserialize, and the restart-time P2
        // surface stays fenced to unbound evidence.
        drop(live);
        let restarted_fixture = {
            // Rebuild the same physical layout independently, as a fresh controller would see.
            let fixture = Fixture::new("restart-after");
            fixture.external_create();
            fixture
        };
        let inventory = inventory_for(&restarted_fixture);
        let mut rediscovered =
            observe_lima_standalone_disk(restarted_fixture.planned_request(), &inventory).unwrap();
        let error = rediscovered
            .project_disk_create_durability_target(&inventory)
            .unwrap_err();
        assert_eq!(
            error.code(),
            "project_disk_create_durability_target_unavailable"
        );

        // Only the bounded acknowledgement fact survives, and it round-trips as inert data.
        let restored: ProjectDiskCreateDurabilityAcknowledgement =
            serde_json::from_str(&ack_json).unwrap();
        assert_eq!(restored, acknowledgement);
        assert!(restored.acknowledged());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn genuine_barrier_completes_on_local_fixture_chain() {
        let mut live = live_created_lineage("genuine-success");
        let tx = transaction(3);
        let binding = binding_for(&live, &tx);
        let target = mint_target(&mut live);
        let proof = acknowledge_created_project_disk_durability(
            &mut live.observation,
            &live.inventory,
            target,
            tx.clone(),
        )
        .unwrap();
        assert!(proof.acknowledged());
        assert_eq!(proof.binding(), &binding);
        proof
            .authorize_first_successor(
                ProjectDiskCreateDurabilityPolicyGeneration::DarwinP2HeldChainFsyncThenBackingFullFsyncV1,
                &binding,
            )
            .unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn genuine_barrier_reports_post_barrier_drift_as_unconfirmed() {
        let mut live = live_created_lineage("genuine-drift");
        let tx = transaction(3);
        let target = mint_target(&mut live);
        // Drift between target minting and the barrier's own pre-revalidation.
        live.fixture.replace_backing();
        let error = acknowledge_created_project_disk_durability(
            &mut live.observation,
            &live.inventory,
            target,
            tx,
        )
        .unwrap_err();
        assert_eq!(
            error.kind(),
            ProjectDiskCreateDurabilityErrorKind::UnconfirmedBeforeBarrier
        );
        assert_eq!(error.code(), "create_durability_unconfirmed");
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn genuine_policy_is_unavailable_on_non_macos() {
        let mut live = live_created_lineage("non-macos");
        let tx = transaction(1);
        let target = mint_target(&mut live);
        let error = acknowledge_created_project_disk_durability(
            &mut live.observation,
            &live.inventory,
            target,
            tx,
        )
        .unwrap_err();
        assert_eq!(
            error.kind(),
            ProjectDiskCreateDurabilityErrorKind::PolicyUnavailableOnHost
        );
        assert_eq!(
            error.code(),
            PROJECT_DISK_CREATE_DURABILITY_POLICY_UNAVAILABLE_CODE
        );
    }
}
