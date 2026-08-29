//! Unix descriptor-bound producer for protected cache replacement-equivalence receipts.
//!
//! This boundary creates one absent caller-named candidate, executes exact reviewed materializer
//! and validator programs with an empty environment and wall-clock limits, derives a bounded
//! path-free identity from the retained output tree, and only then atomically publishes the strict
//! receipt. It does not adopt existing cache paths, update the protected catalog, infer leases,
//! quarantine, delete, reclaim, or clean a failed candidate.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::OsStringExt;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use rustix::fs::{self, AtFlags, Dir, FileType, Mode, OFlags, RenameFlags};
use rustix::io::Errno;
use rustix::process;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::artifact::Sha256Digest;
use crate::cache_inventory::CacheStateId;
use crate::descriptor_bound_launcher::{
    DescriptorBoundLaunchError, ReviewedFilesystemIdentity, ReviewedLaunchCredentials,
    ReviewedLaunchValue, ReviewedLinuxLaunchPlan, execute_reviewed_linux_launch_with_timeout,
};
use crate::protected_cache_generation_catalog::{
    ProtectedCacheGenerationFamily, ProtectedCacheGenerationIdentity,
    ProtectedCacheNamespaceIdentity,
};
use crate::protected_cache_replacement_equivalence::{
    ProtectedCacheReconstructionBinding, ProtectedCacheReplacementEquivalenceBinding,
    ProtectedCacheReplacementEquivalenceReceipt, ProtectedCacheReplacementTarget,
    decode_protected_cache_replacement_equivalence_receipt,
    encode_protected_cache_replacement_equivalence_receipt,
};

pub const PROTECTED_CACHE_REPLACEMENT_PRODUCTION_SCHEMA_VERSION: u8 = 1;
pub const MAX_PROTECTED_CACHE_REPLACEMENT_TREE_ENTRIES: u64 = 2_000_000;
pub const MAX_PROTECTED_CACHE_REPLACEMENT_TREE_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const MAX_TREE_DEPTH: usize = 64;
const MAX_PROGRAM_BYTES: u64 = 512 * 1024 * 1024;
const MAX_CANDIDATE_NAME_BYTES: usize = 128;
const MAX_PROGRAM_ARGUMENTS: usize = 64;
const MAX_PROGRAM_ARGUMENT_BYTES: usize = 65_536;
const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC);
const NEW_FILE_FLAGS: OFlags = OFlags::WRONLY
    .union(OFlags::CREATE)
    .union(OFlags::EXCL)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const PRIVATE_DIRECTORY_MODE: Mode = Mode::RUSR.union(Mode::WUSR).union(Mode::XUSR);
const PRIVATE_FILE_MODE: Mode = Mode::RUSR.union(Mode::WUSR);

/// Unforgeable token required to construct a raw physical-production plan.
///
/// This type is public only so the sealed constructors can remain ordinary public Rust APIs. Its
/// field has no production constructor, so external callers cannot select an arbitrary
/// materializer or validator and turn generic process success into cache equivalence. A later
/// reviewed adapter must add its construction path inside this module.
#[derive(Debug, Clone, Copy)]
pub struct ProtectedCacheReplacementPlanAuthority {
    _private: (),
}

#[derive(Clone)]
pub struct ProtectedCacheReplacementProgram {
    executable_path: PathBuf,
    executable_identity: ReviewedFilesystemIdentity,
    arguments: Vec<ReviewedLaunchValue>,
    executable_content_digest: Sha256Digest,
    generation_digest: Sha256Digest,
}

impl ProtectedCacheReplacementProgram {
    /// Review and bind one exact direct-ELF program, argument vector, and expected generation digest.
    ///
    /// The constructor opens and hashes the exact executable. The digest is reverified from the
    /// descriptor-bound executable bytes and exact argument vector before execution, and the held
    /// launch descriptor is content-hashed again immediately before spawn. Environment is not
    /// accepted: the producer always supplies an empty map.
    ///
    /// # Errors
    ///
    /// Returns a bounded plan error for a noncanonical path or oversized argument surface.
    pub fn new(
        _authority: &ProtectedCacheReplacementPlanAuthority,
        executable_path: impl Into<PathBuf>,
        executable_identity: ReviewedFilesystemIdentity,
        arguments: Vec<ReviewedLaunchValue>,
        generation_digest: Sha256Digest,
    ) -> Result<Self, ProtectedCacheReplacementProductionError> {
        let executable_path = validate_absolute_path(executable_path.into())?;
        validate_arguments(&arguments)?;
        let (executable_content_digest, observed_generation_digest) =
            derive_program_digests(&executable_path, &executable_identity, &arguments)?;
        if observed_generation_digest != generation_digest {
            return Err(production_error(
                ProtectedCacheReplacementProductionErrorKind::ProgramIdentity,
                "reviewed program generation does not match the expected digest",
            ));
        }
        Ok(Self {
            executable_path,
            executable_identity,
            arguments,
            executable_content_digest,
            generation_digest,
        })
    }
}

impl fmt::Debug for ProtectedCacheReplacementProgram {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProtectedCacheReplacementProgram")
            .field("executable", &"<private descriptor-bound executable>")
            .field("argument_count", &self.arguments.len())
            .field("generation_digest", &self.generation_digest)
            .finish()
    }
}

#[derive(Clone)]
pub struct ProtectedCacheReplacementProductionPlan {
    family: ProtectedCacheGenerationFamily,
    namespace_identity: ProtectedCacheNamespaceIdentity,
    state_id: CacheStateId,
    reconstruction: ProtectedCacheReconstructionBinding,
    family_semantic_digest: Sha256Digest,
    materialization_root: PathBuf,
    materialization_root_identity: ReviewedFilesystemIdentity,
    receipt_root: PathBuf,
    receipt_root_identity: ReviewedFilesystemIdentity,
    candidate_name: String,
    materializer: ProtectedCacheReplacementProgram,
    validator: ProtectedCacheReplacementProgram,
    materialization_timeout: Duration,
    validation_timeout: Duration,
    identity_timeout: Duration,
}

impl ProtectedCacheReplacementProductionPlan {
    /// Build one exact fresh-candidate production plan.
    ///
    /// Both roots must already exist as exact owner-private directories. The candidate component
    /// must be absent; this producer never adopts or replaces an existing path.
    ///
    /// # Errors
    ///
    /// Returns an error for noncanonical roots/name, a non-Cargo family, or invalid time bounds.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        _authority: &ProtectedCacheReplacementPlanAuthority,
        family: ProtectedCacheGenerationFamily,
        namespace_identity: ProtectedCacheNamespaceIdentity,
        state_id: CacheStateId,
        reconstruction: ProtectedCacheReconstructionBinding,
        family_semantic_digest: Sha256Digest,
        materialization_root: impl Into<PathBuf>,
        materialization_root_identity: ReviewedFilesystemIdentity,
        receipt_root: impl Into<PathBuf>,
        receipt_root_identity: ReviewedFilesystemIdentity,
        candidate_name: impl Into<String>,
        materializer: ProtectedCacheReplacementProgram,
        validator: ProtectedCacheReplacementProgram,
        materialization_timeout: Duration,
        validation_timeout: Duration,
        identity_timeout: Duration,
    ) -> Result<Self, ProtectedCacheReplacementProductionError> {
        if family != ProtectedCacheGenerationFamily::CargoTargetV1 {
            return Err(plan_error("unsupported protected cache generation family"));
        }
        let materialization_root = validate_absolute_path(materialization_root.into())?;
        let receipt_root = validate_absolute_path(receipt_root.into())?;
        if materialization_root == receipt_root
            || materialization_root.starts_with(&receipt_root)
            || receipt_root.starts_with(&materialization_root)
            || materialization_root_identity.exact_match(&receipt_root_identity)
        {
            return Err(plan_error(
                "materialization and receipt roots must be distinct non-nested directories",
            ));
        }
        let candidate_name = candidate_name.into();
        validate_component(&candidate_name)?;
        validate_timeout(materialization_timeout)?;
        validate_timeout(validation_timeout)?;
        validate_timeout(identity_timeout)?;
        if materializer.generation_digest != *reconstruction.plan_generation_digest()
            || validator.generation_digest != *reconstruction.validator_generation_digest()
        {
            return Err(plan_error(
                "program generation digests do not match the reconstruction binding",
            ));
        }
        Ok(Self {
            family,
            namespace_identity,
            state_id,
            reconstruction,
            family_semantic_digest,
            materialization_root,
            materialization_root_identity,
            receipt_root,
            receipt_root_identity,
            candidate_name,
            materializer,
            validator,
            materialization_timeout,
            validation_timeout,
            identity_timeout,
        })
    }
}

impl fmt::Debug for ProtectedCacheReplacementProductionPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProtectedCacheReplacementProductionPlan")
            .field("family", &self.family)
            .field("namespace_identity", &self.namespace_identity)
            .field("state_id", &self.state_id)
            .field("materialization_root", &"<private reviewed root>")
            .field("receipt_root", &"<private reviewed root>")
            .field("candidate_name", &"<private caller-owned component>")
            .field("materializer", &self.materializer)
            .field("validator", &self.validator)
            .field("materialization_timeout", &self.materialization_timeout)
            .field("validation_timeout", &self.validation_timeout)
            .field("identity_timeout", &self.identity_timeout)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectedCacheReplacementProductionAuthority {
    FreshMaterializationValidatedAndPersisted,
}

pub(crate) struct ProtectedCacheReplacementProductionSeal {
    binding: ProtectedCacheReplacementEquivalenceBinding,
}

impl ProtectedCacheReplacementProductionSeal {
    pub(crate) const fn binding(&self) -> &ProtectedCacheReplacementEquivalenceBinding {
        &self.binding
    }
}

pub struct ProtectedCacheReplacementProduction {
    schema_version: u8,
    receipt: ProtectedCacheReplacementEquivalenceReceipt,
    _seal: ProtectedCacheReplacementProductionSeal,
}

impl ProtectedCacheReplacementProduction {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn authority(&self) -> ProtectedCacheReplacementProductionAuthority {
        ProtectedCacheReplacementProductionAuthority::FreshMaterializationValidatedAndPersisted
    }

    #[must_use]
    pub const fn receipt(&self) -> &ProtectedCacheReplacementEquivalenceReceipt {
        &self.receipt
    }
}

impl fmt::Debug for ProtectedCacheReplacementProduction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProtectedCacheReplacementProduction")
            .field("schema_version", &self.schema_version)
            .field("authority", &self.authority())
            .field("receipt", &self.receipt)
            .field("physical_evidence", &"<sealed descriptor-bound production>")
            .finish()
    }
}

/// Derive the exact program-generation digest used by production-plan correlation.
///
/// The executable is opened by no-follow traversal, matched to its reviewed identity, required to
/// be one single-link regular ELF file, and hashed with the exact argument vector.
///
/// # Errors
///
/// Returns a bounded error if the path, identity, file type, size, or arguments are invalid.
pub fn derive_protected_cache_replacement_program_generation_digest(
    executable_path: impl Into<PathBuf>,
    executable_identity: &ReviewedFilesystemIdentity,
    arguments: &[ReviewedLaunchValue],
) -> Result<Sha256Digest, ProtectedCacheReplacementProductionError> {
    let path = validate_absolute_path(executable_path.into())?;
    validate_arguments(arguments)?;
    derive_program_digests(&path, executable_identity, arguments).map(|(_, generation)| generation)
}

fn derive_program_digests(
    path: &Path,
    executable_identity: &ReviewedFilesystemIdentity,
    arguments: &[ReviewedLaunchValue],
) -> Result<(Sha256Digest, Sha256Digest), ProtectedCacheReplacementProductionError> {
    let executable = open_absolute_file(path)?;
    let observed = inspect_regular_file(&executable, None, true)?;
    require_reviewed_identity(executable_identity, &observed)?;
    if observed.size > MAX_PROGRAM_BYTES {
        return Err(limit_error("reviewed executable exceeds the byte limit"));
    }
    if observed.mode & 0o111 == 0 {
        return Err(production_error(
            ProtectedCacheReplacementProductionErrorKind::ProgramIdentity,
            "reviewed executable has no execute permission",
        ));
    }
    if executable_is_writable_by_launcher(&observed)? {
        return Err(production_error(
            ProtectedCacheReplacementProductionErrorKind::ProgramIdentity,
            "reviewed executable is writable by the launcher identity",
        ));
    }
    let probe = rustix::io::dup(&executable)
        .map_err(|_| io_error("reviewed executable could not be retained"))?;
    let mut magic = [0_u8; 4];
    File::from(probe)
        .read_exact(&mut magic)
        .map_err(|_| unsafe_filesystem("reviewed executable is not a direct ELF image"))?;
    if magic != [0x7f, b'E', b'L', b'F'] {
        return Err(unsafe_filesystem(
            "reviewed executable is not a direct ELF image",
        ));
    }
    let mut content_hasher = Sha256::new();
    hash_file_bytes(&executable, observed.size, None, &mut content_hasher)?;
    let after = inspect_regular_file(&executable, None, true)?;
    let rebound = open_absolute_file(path)?;
    let rebound = inspect_regular_file(&rebound, None, true)?;
    if !same_snapshot(&observed, &after) || !same_snapshot(&after, &rebound) {
        return Err(production_error(
            ProtectedCacheReplacementProductionErrorKind::ProgramIdentity,
            "reviewed executable changed while its generation was derived",
        ));
    }
    let content_digest = digest_from(content_hasher)?;
    let mut generation_hasher = Sha256::new();
    generation_hasher.update(b"glaeda.protected-cache.replacement-program.v1\0");
    hash_bytes(&mut generation_hasher, content_digest.as_str().as_bytes());
    hash_u64(&mut generation_hasher, arguments.len() as u64);
    for argument in arguments {
        hash_bytes(&mut generation_hasher, argument.exposed().as_bytes());
    }
    Ok((content_digest, digest_from(generation_hasher)?))
}

/// Freshly materialize, validate, identify, and durably publish one replacement receipt.
///
/// Failure never removes or renames the caller-named candidate. The caller owns any inspection or
/// discard decision. No success receipt is published until both exact processes succeed and the
/// retained candidate tree has been fully identified.
///
/// # Errors
///
/// Returns a redacted typed error for unsafe roots, pre-existing/replaced candidates, program drift,
/// execution failure/timeout, unsafe or oversized output, receipt conflict, or persistence failure.
pub fn produce_protected_cache_replacement_equivalence(
    plan: &ProtectedCacheReplacementProductionPlan,
) -> Result<ProtectedCacheReplacementProduction, ProtectedCacheReplacementProductionError> {
    let owner = (process::geteuid().as_raw(), process::getegid().as_raw());
    let materialization_root = open_exact_private_root(
        &plan.materialization_root,
        &plan.materialization_root_identity,
        owner,
    )?;
    let receipt_root =
        open_exact_private_root(&plan.receipt_root, &plan.receipt_root_identity, owner)?;
    recover_receipt_stage(&receipt_root, &plan.state_id, owner)?;
    if let Some(existing) =
        read_optional_receipt(&receipt_root, &receipt_name(&plan.state_id), owner)?
    {
        decode_protected_cache_replacement_equivalence_receipt(&existing)
            .map_err(|_| persistence_error("stored replacement receipt is corrupt"))?;
    }

    verify_program(&plan.materializer)?;
    verify_program(&plan.validator)?;

    fs::mkdirat(
        &materialization_root,
        &plan.candidate_name,
        PRIVATE_DIRECTORY_MODE,
    )
    .map_err(|error| match error {
        Errno::EXIST => production_error(
            ProtectedCacheReplacementProductionErrorKind::CandidateExists,
            "replacement candidate already exists and cannot be adopted",
        ),
        _ => io_error("replacement candidate could not be created"),
    })?;
    fs::fsync(&materialization_root)
        .map_err(|_| io_error("materialization root could not be synchronized"))?;
    let candidate = open_candidate(&materialization_root, &plan.candidate_name, owner)?;
    let candidate_identity = reviewed_identity(&inspect_private_directory(&candidate, owner)?)?;
    let candidate_path = plan.materialization_root.join(&plan.candidate_name);
    let credentials = ReviewedLaunchCredentials::Inherit {
        uid: owner.0,
        gid: owner.1,
    };

    let materializer = launch_plan(
        "protected-cache.materialize",
        &plan.materializer,
        &candidate_path,
        &candidate_identity,
        credentials,
    )?;
    let materialization =
        execute_reviewed_linux_launch_with_timeout(&materializer, plan.materialization_timeout)
            .map_err(launch_error)?;
    if !materialization.success() {
        return Err(execution_error("replacement materializer did not succeed"));
    }
    verify_candidate_binding(
        &materialization_root,
        &plan.candidate_name,
        &candidate,
        owner,
    )?;

    let validator = launch_plan(
        "protected-cache.validate",
        &plan.validator,
        &candidate_path,
        &candidate_identity,
        credentials,
    )?;
    let validation =
        execute_reviewed_linux_launch_with_timeout(&validator, plan.validation_timeout)
            .map_err(launch_error)?;
    if !validation.success() {
        return Err(execution_error("replacement validator did not succeed"));
    }
    verify_candidate_binding(
        &materialization_root,
        &plan.candidate_name,
        &candidate,
        owner,
    )?;

    let generation_identity = derive_tree_identity(&candidate, owner, plan.identity_timeout)?;
    verify_candidate_binding(
        &materialization_root,
        &plan.candidate_name,
        &candidate,
        owner,
    )?;
    let binding = ProtectedCacheReplacementEquivalenceBinding::new(
        ProtectedCacheReplacementTarget::new(
            plan.family,
            plan.namespace_identity.clone(),
            plan.state_id.clone(),
            generation_identity,
        ),
        plan.reconstruction.clone(),
        plan.family_semantic_digest.clone(),
    );
    let seal = ProtectedCacheReplacementProductionSeal { binding };
    let receipt = ProtectedCacheReplacementEquivalenceReceipt::from_physical_production(&seal);
    persist_receipt(&receipt_root, &plan.state_id, owner, &receipt)?;

    Ok(ProtectedCacheReplacementProduction {
        schema_version: PROTECTED_CACHE_REPLACEMENT_PRODUCTION_SCHEMA_VERSION,
        receipt,
        _seal: seal,
    })
}

fn verify_program(
    program: &ProtectedCacheReplacementProgram,
) -> Result<(), ProtectedCacheReplacementProductionError> {
    let (content, generation) = derive_program_digests(
        &program.executable_path,
        &program.executable_identity,
        &program.arguments,
    )?;
    if content != program.executable_content_digest || generation != program.generation_digest {
        return Err(production_error(
            ProtectedCacheReplacementProductionErrorKind::ProgramIdentity,
            "reviewed program generation does not match the bound digest",
        ));
    }
    Ok(())
}

fn launch_plan(
    command_id: &'static str,
    program: &ProtectedCacheReplacementProgram,
    candidate_path: &Path,
    candidate_identity: &ReviewedFilesystemIdentity,
    credentials: ReviewedLaunchCredentials,
) -> Result<ReviewedLinuxLaunchPlan, ProtectedCacheReplacementProductionError> {
    ReviewedLinuxLaunchPlan::new(
        command_id,
        &program.executable_path,
        program.executable_identity.clone(),
        candidate_path,
        candidate_identity.clone(),
        program.arguments.clone(),
        BTreeMap::new(),
        credentials,
    )
    .map(|plan| plan.with_executable_content_digest(program.executable_content_digest.clone()))
    .map_err(launch_error)
}

fn executable_is_writable_by_launcher(
    executable: &FileSnapshot,
) -> Result<bool, ProtectedCacheReplacementProductionError> {
    let uid = process::geteuid().as_raw();
    if uid == 0 {
        return Ok(false);
    }
    if uid == executable.uid {
        return Ok(executable.mode & 0o200 != 0);
    }
    let gid = process::getegid().as_raw();
    let in_group = gid == executable.gid
        || process::getgroups()
            .map_err(|_| io_error("launcher groups could not be inspected"))?
            .iter()
            .any(|group| group.as_raw() == executable.gid);
    if in_group {
        return Ok(executable.mode & 0o020 != 0);
    }
    Ok(executable.mode & 0o002 != 0)
}

fn derive_tree_identity(
    root: &OwnedFd,
    owner: (u32, u32),
    timeout: Duration,
) -> Result<ProtectedCacheGenerationIdentity, ProtectedCacheReplacementProductionError> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| plan_error("materialized identity deadline overflowed"))?;
    let mut hasher = Sha256::new();
    hasher.update(b"glaeda.protected-cache.materialized-tree.v1\0");
    let mut budget = TreeBudget::default();
    hash_directory(root, owner, 0, deadline, &mut budget, &mut hasher)?;
    if budget
        .hardlinks
        .values()
        .any(|links| links.observed != links.reported)
    {
        return Err(unsafe_output(
            "replacement output has a hard link outside the candidate tree",
        ));
    }
    ProtectedCacheGenerationIdentity::parse(&format!("sha256:{:x}", hasher.finalize()))
        .map_err(|_| io_error("materialized generation identity could not be encoded"))
}

#[derive(Default)]
struct TreeBudget {
    entries: u64,
    bytes: u64,
    hardlinks: BTreeMap<(u64, u64), HardlinkCount>,
}

#[derive(Default)]
struct HardlinkCount {
    observed: u64,
    reported: u64,
}

fn hash_directory(
    directory: &OwnedFd,
    owner: (u32, u32),
    depth: usize,
    deadline: Instant,
    budget: &mut TreeBudget,
    hasher: &mut Sha256,
) -> Result<(), ProtectedCacheReplacementProductionError> {
    if depth > MAX_TREE_DEPTH {
        return Err(limit_error("replacement output exceeds the depth limit"));
    }
    require_before_deadline(deadline)?;
    let before = inspect_output_directory(directory, owner)?;
    let mut directory_entries = Dir::read_from(directory)
        .map_err(|_| io_error("replacement output directory could not be enumerated"))?;
    let mut entries = Vec::new();
    for entry in &mut directory_entries {
        require_before_deadline(deadline)?;
        let entry =
            entry.map_err(|_| io_error("replacement output directory entry could not be read"))?;
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        budget.entries = budget
            .entries
            .checked_add(1)
            .ok_or_else(|| limit_error("replacement output entry count overflowed"))?;
        if budget.entries > MAX_PROTECTED_CACHE_REPLACEMENT_TREE_ENTRIES {
            return Err(limit_error("replacement output exceeds the entry limit"));
        }
        entries.push(name.to_vec());
    }
    entries.sort();
    for name in entries {
        require_before_deadline(deadline)?;
        hash_bytes(hasher, &name);
        let name = OsString::from_vec(name);
        let stat = fs::statat(directory, &name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| unsafe_output("replacement output changed during inspection"))?;
        let path_snapshot = snapshot(&stat)?;
        let file_type = FileType::from_raw_mode(stat.st_mode);
        if file_type.is_dir() {
            hasher.update(b"d");
            hash_u64(hasher, u64::from(stat.st_mode & 0o7777));
            let child = fs::openat(directory, &name, DIRECTORY_FLAGS, Mode::empty())
                .map_err(|_| unsafe_output("replacement output directory is unsafe"))?;
            let child_before = inspect_output_directory(&child, owner)?;
            if !same_identity(&path_snapshot, &child_before) {
                return Err(unsafe_output("replacement output directory was replaced"));
            }
            hash_directory(&child, owner, depth + 1, deadline, budget, hasher)?;
            let child_after = inspect_output_directory(&child, owner)?;
            let rebound = fs::statat(directory, &name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|_| unsafe_output("replacement output directory changed"))?;
            let rebound = snapshot(&rebound)?;
            if !same_snapshot(&child_before, &child_after) || !same_identity(&child_after, &rebound)
            {
                return Err(unsafe_output("replacement output directory changed"));
            }
        } else if file_type.is_file() {
            hasher.update(b"f");
            hash_u64(hasher, u64::from(stat.st_mode & 0o7777));
            let file = fs::openat(directory, &name, FILE_FLAGS, Mode::empty())
                .map_err(|_| unsafe_output("replacement output file is unsafe"))?;
            let file_before = inspect_regular_file(&file, Some(owner), false)?;
            if !same_identity(&path_snapshot, &file_before) {
                return Err(unsafe_output("replacement output file was replaced"));
            }
            let hardlinks = budget
                .hardlinks
                .entry((file_before.device, file_before.inode))
                .or_default();
            if hardlinks.reported != 0 && hardlinks.reported != file_before.links {
                return Err(unsafe_output(
                    "replacement output hard-link count changed during inspection",
                ));
            }
            hardlinks.reported = file_before.links;
            hardlinks.observed = hardlinks
                .observed
                .checked_add(1)
                .ok_or_else(|| limit_error("replacement output hard-link count overflowed"))?;
            if hardlinks.observed > hardlinks.reported {
                return Err(unsafe_output(
                    "replacement output hard-link topology is inconsistent",
                ));
            }
            hash_u64(hasher, file_before.links);
            budget.bytes = budget
                .bytes
                .checked_add(file_before.size)
                .ok_or_else(|| limit_error("replacement output byte count overflowed"))?;
            if budget.bytes > MAX_PROTECTED_CACHE_REPLACEMENT_TREE_BYTES {
                return Err(limit_error("replacement output exceeds the byte limit"));
            }
            hash_u64(hasher, file_before.size);
            hash_file_bytes(&file, file_before.size, Some(deadline), hasher)?;
            fs::fsync(&file)
                .map_err(|_| io_error("replacement output file could not be synchronized"))?;
            let file_after = inspect_regular_file(&file, Some(owner), false)?;
            let rebound = fs::statat(directory, &name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|_| unsafe_output("replacement output file changed"))?;
            let rebound = snapshot(&rebound)?;
            if !same_snapshot(&file_before, &file_after) || !same_identity(&file_after, &rebound) {
                return Err(unsafe_output("replacement output file changed"));
            }
        } else {
            return Err(unsafe_output(
                "replacement output contains an unsupported object type",
            ));
        }
    }
    fs::fsync(directory)
        .map_err(|_| io_error("replacement output directory could not be synchronized"))?;
    let after = inspect_output_directory(directory, owner)?;
    if !same_snapshot(&before, &after) {
        return Err(unsafe_output(
            "replacement output directory changed during inspection",
        ));
    }
    Ok(())
}

fn persist_receipt(
    root: &OwnedFd,
    state_id: &CacheStateId,
    owner: (u32, u32),
    receipt: &ProtectedCacheReplacementEquivalenceReceipt,
) -> Result<(), ProtectedCacheReplacementProductionError> {
    let final_name = receipt_name(state_id);
    let stage_name = stage_name(state_id);
    let bytes = encode_protected_cache_replacement_equivalence_receipt(receipt)
        .map_err(|_| persistence_error("replacement receipt could not be encoded"))?;
    if let Some(existing) = read_optional_receipt(root, &final_name, owner)? {
        return require_equal_receipt(&existing, &bytes);
    }
    let stage =
        fs::openat(root, &stage_name, NEW_FILE_FLAGS, PRIVATE_FILE_MODE).map_err(|error| {
            match error {
                Errno::EXIST => persistence_error(
                    "replacement receipt staging debt requires recovery before publication",
                ),
                _ => persistence_error("replacement receipt stage could not be created"),
            }
        })?;
    fs::fchmod(&stage, PRIVATE_FILE_MODE)
        .map_err(|_| persistence_error("replacement receipt stage mode could not be set"))?;
    let duplicated = rustix::io::dup(&stage)
        .map_err(|_| persistence_error("replacement receipt stage could not be retained"))?;
    File::from(duplicated)
        .write_all(&bytes)
        .map_err(|_| persistence_error("replacement receipt stage could not be written"))?;
    fs::fsync(&stage)
        .map_err(|_| persistence_error("replacement receipt stage could not be synchronized"))?;
    fs::fsync(root)
        .map_err(|_| persistence_error("replacement receipt directory could not synchronize"))?;
    match fs::renameat_with(root, &stage_name, root, &final_name, RenameFlags::NOREPLACE) {
        Ok(()) => fs::fsync(root)
            .map_err(|_| persistence_error("replacement receipt directory could not synchronize")),
        Err(Errno::EXIST) => {
            let existing = read_optional_receipt(root, &final_name, owner)?.ok_or_else(|| {
                persistence_error("replacement receipt changed during publication")
            })?;
            require_equal_receipt(&existing, &bytes)?;
            fs::unlinkat(root, &stage_name, AtFlags::empty()).map_err(|_| {
                persistence_error("duplicate replacement receipt stage could not be removed")
            })?;
            fs::fsync(root).map_err(|_| {
                persistence_error("replacement receipt directory could not synchronize")
            })
        }
        Err(_) => Err(persistence_error(
            "replacement receipt could not be atomically published",
        )),
    }
}

fn recover_receipt_stage(
    root: &OwnedFd,
    state_id: &CacheStateId,
    owner: (u32, u32),
) -> Result<(), ProtectedCacheReplacementProductionError> {
    let stage_name = stage_name(state_id);
    let Some(stage) = read_optional_receipt(root, &stage_name, owner)? else {
        return Ok(());
    };
    let final_name = receipt_name(state_id);
    if let Some(final_bytes) = read_optional_receipt(root, &final_name, owner)? {
        require_equal_receipt(&stage, &final_bytes)?;
        fs::unlinkat(root, &stage_name, AtFlags::empty())
            .map_err(|_| persistence_error("duplicate receipt stage could not be removed"))?;
        return fs::fsync(root)
            .map_err(|_| persistence_error("receipt directory could not synchronize"));
    }
    decode_protected_cache_replacement_equivalence_receipt(&stage)
        .map_err(|_| persistence_error("replacement receipt stage is incomplete or corrupt"))?;
    fs::fsync(root).map_err(|_| persistence_error("receipt directory could not synchronize"))?;
    fs::renameat_with(root, &stage_name, root, &final_name, RenameFlags::NOREPLACE)
        .map_err(|_| persistence_error("abandoned replacement receipt could not be recovered"))?;
    fs::fsync(root).map_err(|_| persistence_error("receipt directory could not synchronize"))
}

fn read_optional_receipt(
    root: &OwnedFd,
    name: &str,
    owner: (u32, u32),
) -> Result<Option<Vec<u8>>, ProtectedCacheReplacementProductionError> {
    let file = match fs::openat(root, name, FILE_FLAGS, Mode::empty()) {
        Ok(file) => file,
        Err(Errno::NOENT) => return Ok(None),
        Err(_) => {
            return Err(persistence_error(
                "replacement receipt is unsafe or unreadable",
            ));
        }
    };
    let before = inspect_regular_file(&file, Some(owner), true)?;
    if before.mode & 0o7777 != PRIVATE_FILE_MODE.as_raw_mode() {
        return Err(persistence_error(
            "replacement receipt does not have exact private permissions",
        ));
    }
    if before.size > crate::protected_cache_replacement_equivalence::MAX_PROTECTED_CACHE_REPLACEMENT_EQUIVALENCE_BYTES as u64 {
        return Err(persistence_error("replacement receipt exceeds the byte limit"));
    }
    let duplicated = rustix::io::dup(&file)
        .map_err(|_| persistence_error("replacement receipt could not be retained"))?;
    let mut bytes = Vec::new();
    File::from(duplicated)
        .take(before.size + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| persistence_error("replacement receipt could not be read"))?;
    let after = inspect_regular_file(&file, Some(owner), true)?;
    if !same_snapshot(&before, &after) || bytes.len() as u64 != before.size {
        return Err(persistence_error(
            "replacement receipt changed while being read",
        ));
    }
    Ok(Some(bytes))
}

fn require_equal_receipt(
    left: &[u8],
    right: &[u8],
) -> Result<(), ProtectedCacheReplacementProductionError> {
    decode_protected_cache_replacement_equivalence_receipt(left)
        .map_err(|_| persistence_error("stored replacement receipt is corrupt"))?;
    if left != right {
        return Err(production_error(
            ProtectedCacheReplacementProductionErrorKind::ReceiptConflict,
            "a different replacement receipt already exists for this state",
        ));
    }
    Ok(())
}

fn open_exact_private_root(
    path: &Path,
    expected: &ReviewedFilesystemIdentity,
    owner: (u32, u32),
) -> Result<OwnedFd, ProtectedCacheReplacementProductionError> {
    let root = open_absolute_directory(path)?;
    let stat = inspect_private_directory(&root, owner)?;
    require_reviewed_identity(expected, &stat)?;
    Ok(root)
}

fn open_candidate(
    root: &OwnedFd,
    name: &str,
    owner: (u32, u32),
) -> Result<OwnedFd, ProtectedCacheReplacementProductionError> {
    let candidate = fs::openat(root, name, DIRECTORY_FLAGS, Mode::empty())
        .map_err(|_| unsafe_output("replacement candidate could not be opened safely"))?;
    inspect_private_directory(&candidate, owner)?;
    Ok(candidate)
}

fn verify_candidate_binding(
    root: &OwnedFd,
    name: &str,
    retained: &OwnedFd,
    owner: (u32, u32),
) -> Result<(), ProtectedCacheReplacementProductionError> {
    let retained_stat = inspect_private_directory(retained, owner)?;
    let rebound = fs::openat(root, name, DIRECTORY_FLAGS, Mode::empty())
        .map_err(|_| unsafe_output("replacement candidate path changed"))?;
    let rebound_stat = inspect_private_directory(&rebound, owner)?;
    if !same_identity(&retained_stat, &rebound_stat) {
        return Err(unsafe_output("replacement candidate identity changed"));
    }
    Ok(())
}

fn open_absolute_directory(
    path: &Path,
) -> Result<OwnedFd, ProtectedCacheReplacementProductionError> {
    let mut current = fs::open("/", DIRECTORY_FLAGS, Mode::empty())
        .map_err(|_| unsafe_filesystem("filesystem root could not be opened"))?;
    for component in path.components() {
        let Component::Normal(component) = component else {
            continue;
        };
        current = fs::openat(&current, component, DIRECTORY_FLAGS, Mode::empty())
            .map_err(|_| unsafe_filesystem("reviewed root contains an unsafe component"))?;
    }
    Ok(current)
}

fn open_absolute_file(path: &Path) -> Result<OwnedFd, ProtectedCacheReplacementProductionError> {
    let (name, parent) = path
        .file_name()
        .zip(path.parent())
        .ok_or_else(|| plan_error("reviewed executable path is invalid"))?;
    let directory = open_absolute_directory(parent)?;
    fs::openat(&directory, name, FILE_FLAGS, Mode::empty())
        .map_err(|_| unsafe_filesystem("reviewed executable could not be opened safely"))
}

#[derive(Clone)]
struct FileSnapshot {
    device: u64,
    inode: u64,
    uid: u32,
    gid: u32,
    mode: u32,
    links: u64,
    size: u64,
    mtime_sec: i64,
    mtime_nsec: i64,
    ctime_sec: i64,
    ctime_nsec: i64,
}

fn inspect_private_directory(
    directory: impl AsFd,
    owner: (u32, u32),
) -> Result<FileSnapshot, ProtectedCacheReplacementProductionError> {
    let stat = fs::fstat(directory).map_err(|_| unsafe_filesystem("directory is unreadable"))?;
    if !FileType::from_raw_mode(stat.st_mode).is_dir()
        || (stat.st_uid, stat.st_gid) != owner
        || stat.st_mode & 0o7777 != PRIVATE_DIRECTORY_MODE.as_raw_mode()
    {
        return Err(unsafe_filesystem(
            "reviewed directory is not exact owner-private state",
        ));
    }
    snapshot(&stat)
}

fn inspect_output_directory(
    directory: impl AsFd,
    owner: (u32, u32),
) -> Result<FileSnapshot, ProtectedCacheReplacementProductionError> {
    let stat = fs::fstat(directory).map_err(|_| unsafe_filesystem("directory is unreadable"))?;
    if !FileType::from_raw_mode(stat.st_mode).is_dir()
        || (stat.st_uid, stat.st_gid) != owner
        || stat.st_mode & 0o7022 != 0
    {
        return Err(unsafe_output(
            "replacement output directory is not safe owner-controlled state",
        ));
    }
    snapshot(&stat)
}

fn inspect_regular_file(
    file: impl AsFd,
    owner: Option<(u32, u32)>,
    require_single_link: bool,
) -> Result<FileSnapshot, ProtectedCacheReplacementProductionError> {
    let stat = fs::fstat(file).map_err(|_| unsafe_filesystem("file is unreadable"))?;
    if !FileType::from_raw_mode(stat.st_mode).is_file()
        || (require_single_link && stat.st_nlink != 1)
        || stat.st_size < 0
        || owner.is_some_and(|owner| owner != (stat.st_uid, stat.st_gid))
        || stat.st_mode & 0o7022 != 0
    {
        return Err(unsafe_filesystem(
            "reviewed file is not safe single-link regular state",
        ));
    }
    snapshot(&stat)
}

fn snapshot(
    stat: &rustix::fs::Stat,
) -> Result<FileSnapshot, ProtectedCacheReplacementProductionError> {
    Ok(FileSnapshot {
        device: stat.st_dev,
        inode: stat.st_ino,
        uid: stat.st_uid,
        gid: stat.st_gid,
        mode: stat.st_mode,
        links: stat.st_nlink,
        size: u64::try_from(stat.st_size).map_err(|_| unsafe_filesystem("file size is invalid"))?,
        mtime_sec: stat.st_mtime,
        mtime_nsec: stat.st_mtime_nsec as i64,
        ctime_sec: stat.st_ctime,
        ctime_nsec: stat.st_ctime_nsec as i64,
    })
}

fn reviewed_identity(
    stat: &FileSnapshot,
) -> Result<ReviewedFilesystemIdentity, ProtectedCacheReplacementProductionError> {
    ReviewedFilesystemIdentity::new(
        stat.device,
        stat.inode,
        stat.uid,
        stat.gid,
        stat.mode & 0o7777,
    )
    .map_err(launch_error)
}

fn require_reviewed_identity(
    expected: &ReviewedFilesystemIdentity,
    observed: &FileSnapshot,
) -> Result<(), ProtectedCacheReplacementProductionError> {
    let observed = reviewed_identity(observed)?;
    if !expected.exact_match(&observed) {
        return Err(production_error(
            ProtectedCacheReplacementProductionErrorKind::FilesystemIdentity,
            "reviewed filesystem identity does not match",
        ));
    }
    Ok(())
}

fn same_identity(left: &FileSnapshot, right: &FileSnapshot) -> bool {
    left.device == right.device
        && left.inode == right.inode
        && left.uid == right.uid
        && left.gid == right.gid
        && left.mode == right.mode
        && left.links == right.links
}

fn same_snapshot(left: &FileSnapshot, right: &FileSnapshot) -> bool {
    same_identity(left, right)
        && left.size == right.size
        && left.mtime_sec == right.mtime_sec
        && left.mtime_nsec == right.mtime_nsec
        && left.ctime_sec == right.ctime_sec
        && left.ctime_nsec == right.ctime_nsec
}

fn hash_file_bytes(
    file: &OwnedFd,
    expected_size: u64,
    deadline: Option<Instant>,
    hasher: &mut Sha256,
) -> Result<(), ProtectedCacheReplacementProductionError> {
    let duplicated = rustix::io::dup(file)
        .map_err(|_| io_error("reviewed file could not be retained for hashing"))?;
    let mut file = File::from(duplicated);
    file.seek(SeekFrom::Start(0))
        .map_err(|_| io_error("reviewed file could not be positioned for hashing"))?;
    let mut reader = file.take(expected_size + 1);
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        if let Some(deadline) = deadline {
            require_before_deadline(deadline)?;
        }
        let count = reader
            .read(&mut buffer)
            .map_err(|_| io_error("reviewed file could not be hashed"))?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .ok_or_else(|| limit_error("reviewed file size overflowed"))?;
        if total > expected_size {
            return Err(unsafe_output("reviewed file grew while it was hashed"));
        }
        hasher.update(&buffer[..count]);
    }
    if total != expected_size {
        return Err(unsafe_output("reviewed file changed while it was hashed"));
    }
    Ok(())
}

fn require_before_deadline(
    deadline: Instant,
) -> Result<(), ProtectedCacheReplacementProductionError> {
    if Instant::now() >= deadline {
        return Err(limit_error(
            "replacement output identity exceeded the wall-clock limit",
        ));
    }
    Ok(())
}

fn digest_from(hasher: Sha256) -> Result<Sha256Digest, ProtectedCacheReplacementProductionError> {
    Sha256Digest::parse(&format!("sha256:{:x}", hasher.finalize()))
        .map_err(|_| io_error("program generation digest could not be encoded"))
}

fn hash_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    hash_u64(hasher, bytes.len() as u64);
    hasher.update(bytes);
}

fn hash_u64(hasher: &mut Sha256, value: u64) {
    hasher.update(value.to_be_bytes());
}

fn receipt_name(state_id: &CacheStateId) -> String {
    format!("{}.replacement-equivalence.json", state_id.as_str())
}

fn stage_name(state_id: &CacheStateId) -> String {
    format!(".{}.replacement-equivalence.stage", state_id.as_str())
}

fn validate_absolute_path(
    path: PathBuf,
) -> Result<PathBuf, ProtectedCacheReplacementProductionError> {
    let Some(value) = path.to_str() else {
        return Err(plan_error("reviewed path must be valid UTF-8"));
    };
    if value.is_empty()
        || value == "/"
        || value.len() > 4_096
        || value.ends_with('/')
        || value.contains("//")
        || value.chars().any(char::is_control)
        || !path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(plan_error(
            "reviewed path must be a canonical non-root absolute path",
        ));
    }
    Ok(path)
}

fn validate_component(value: &str) -> Result<(), ProtectedCacheReplacementProductionError> {
    if value.is_empty()
        || value.len() > MAX_CANDIDATE_NAME_BYTES
        || matches!(value, "." | "..")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(plan_error(
            "candidate name must be one bounded ASCII component",
        ));
    }
    Ok(())
}

fn validate_arguments(
    arguments: &[ReviewedLaunchValue],
) -> Result<(), ProtectedCacheReplacementProductionError> {
    if arguments.len() > MAX_PROGRAM_ARGUMENTS {
        return Err(plan_error("program argument count exceeds the limit"));
    }
    let total = arguments.iter().try_fold(0_usize, |total, argument| {
        if argument.exposed().as_bytes().contains(&0) {
            return Err(plan_error("program argument contains NUL"));
        }
        total
            .checked_add(argument.exposed().len())
            .ok_or_else(|| plan_error("program argument bytes overflowed"))
    })?;
    if total > MAX_PROGRAM_ARGUMENT_BYTES {
        return Err(plan_error("program argument bytes exceed the limit"));
    }
    Ok(())
}

fn validate_timeout(timeout: Duration) -> Result<(), ProtectedCacheReplacementProductionError> {
    if timeout.is_zero() || timeout > Duration::from_secs(24 * 60 * 60) {
        return Err(plan_error(
            "program timeout must be within the reviewed bound",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectedCacheReplacementProductionErrorKind {
    Plan,
    FilesystemIdentity,
    UnsafeFilesystem,
    CandidateExists,
    ProgramIdentity,
    Execution,
    OutputUnsafe,
    ResourceLimit,
    ReceiptConflict,
    Persistence,
    Io,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct ProtectedCacheReplacementProductionError {
    kind: ProtectedCacheReplacementProductionErrorKind,
    message: &'static str,
}

impl ProtectedCacheReplacementProductionError {
    #[must_use]
    pub const fn kind(&self) -> ProtectedCacheReplacementProductionErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn message(&self) -> &'static str {
        self.message
    }
}

impl fmt::Debug for ProtectedCacheReplacementProductionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProtectedCacheReplacementProductionError")
            .field("kind", &self.kind)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for ProtectedCacheReplacementProductionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ProtectedCacheReplacementProductionError {}

const fn production_error(
    kind: ProtectedCacheReplacementProductionErrorKind,
    message: &'static str,
) -> ProtectedCacheReplacementProductionError {
    ProtectedCacheReplacementProductionError { kind, message }
}

const fn plan_error(message: &'static str) -> ProtectedCacheReplacementProductionError {
    production_error(ProtectedCacheReplacementProductionErrorKind::Plan, message)
}

const fn unsafe_filesystem(message: &'static str) -> ProtectedCacheReplacementProductionError {
    production_error(
        ProtectedCacheReplacementProductionErrorKind::UnsafeFilesystem,
        message,
    )
}

const fn unsafe_output(message: &'static str) -> ProtectedCacheReplacementProductionError {
    production_error(
        ProtectedCacheReplacementProductionErrorKind::OutputUnsafe,
        message,
    )
}

const fn limit_error(message: &'static str) -> ProtectedCacheReplacementProductionError {
    production_error(
        ProtectedCacheReplacementProductionErrorKind::ResourceLimit,
        message,
    )
}

const fn execution_error(message: &'static str) -> ProtectedCacheReplacementProductionError {
    production_error(
        ProtectedCacheReplacementProductionErrorKind::Execution,
        message,
    )
}

const fn persistence_error(message: &'static str) -> ProtectedCacheReplacementProductionError {
    production_error(
        ProtectedCacheReplacementProductionErrorKind::Persistence,
        message,
    )
}

const fn io_error(message: &'static str) -> ProtectedCacheReplacementProductionError {
    production_error(ProtectedCacheReplacementProductionErrorKind::Io, message)
}

fn launch_error(error: DescriptorBoundLaunchError) -> ProtectedCacheReplacementProductionError {
    use crate::descriptor_bound_launcher::DescriptorBoundLaunchErrorKind;
    match error.kind() {
        DescriptorBoundLaunchErrorKind::Plan => {
            plan_error("descriptor-bound replacement process plan is invalid")
        }
        DescriptorBoundLaunchErrorKind::FilesystemIdentity
        | DescriptorBoundLaunchErrorKind::DescriptorAlias => production_error(
            ProtectedCacheReplacementProductionErrorKind::FilesystemIdentity,
            "descriptor-bound replacement filesystem identity changed",
        ),
        DescriptorBoundLaunchErrorKind::UnsupportedExecutable
        | DescriptorBoundLaunchErrorKind::ExecutableContent => production_error(
            ProtectedCacheReplacementProductionErrorKind::ProgramIdentity,
            "descriptor-bound replacement executable identity changed",
        ),
        DescriptorBoundLaunchErrorKind::Credentials
        | DescriptorBoundLaunchErrorKind::Spawn
        | DescriptorBoundLaunchErrorKind::OutputCapture
        | DescriptorBoundLaunchErrorKind::OutputLimit
        | DescriptorBoundLaunchErrorKind::Timeout
        | DescriptorBoundLaunchErrorKind::Status => {
            execution_error("descriptor-bound replacement process could not be executed")
        }
    }
}

#[cfg(test)]
mod tests;
