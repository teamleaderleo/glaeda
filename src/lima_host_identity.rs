use std::ffi::OsStr;
use std::fmt;
use std::fs::File;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::FileExt as _;
use std::path::{Component, Path};

use rustix::fs::{self, AtFlags, FileType, Mode, OFlags};
use rustix::io::Errno;
use rustix::process::geteuid;
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::lima_observation::{LimaArchitecture, LimaObservationRequest, LimaVmType};

pub const LIMA_HOST_IDENTITY_SCHEMA_VERSION: u8 = 1;

const VZ_IDENTIFIER_FILE: &str = "vz-identifier";
const ROOT_DISK_FILE: &str = "disk";
const MAX_VZ_IDENTIFIER_BYTES: usize = 4_096;
const EXPECTED_VZ_IDENTIFIER_BYTES: usize = 70;
const VZ_IDENTIFIER_MAGIC: &[u8; 8] = b"bplist00";
const VZ_IDENTIFIER_PREFIX: &[u8; 11] = b"\xd1\x01\x02\x54UUIDO\x10\x10";
const VZ_IDENTIFIER_OFFSET_TABLE: &[u8; 3] = b"\x08\x0b\x10";
const LOGICAL_BLOCK_BYTES: u64 = 512;
const MIN_GPT_HEADER_BYTES: u32 = 92;
const MAX_GPT_TABLE_BYTES: u64 = 1024 * 1024;
const MAX_GPT_ENTRIES: u32 = 1_024;
const MIN_GPT_ENTRY_BYTES: u32 = 128;
const MAX_GPT_ENTRY_BYTES: u32 = 4_096;
const GPT_SIGNATURE: &[u8; 8] = b"EFI PART";
const IDENTITY_DOMAIN: &[u8] = b"smolrunner-lima-host-identity-v1";
const DISK_DOMAIN: &[u8] = b"smolrunner-lima-root-disk-gpt-v1";

#[derive(Clone, PartialEq, Eq)]
pub struct LimaHostInstanceIdentity {
    digest: Sha256Digest,
    ownership_digest: Sha256Digest,
    legacy_digest: Option<Sha256Digest>,
}

impl LimaHostInstanceIdentity {
    #[must_use]
    pub const fn digest(&self) -> &Sha256Digest {
        &self.digest
    }

    /// Host-controlled VZ identity used to bind cleanup authority. Unlike the full clone
    /// identity, this digest excludes root-disk bytes that privileged guest code can modify.
    #[must_use]
    pub(crate) const fn ownership_digest(&self) -> &Sha256Digest {
        &self.ownership_digest
    }

    pub(crate) const fn legacy_digest(&self) -> Option<&Sha256Digest> {
        self.legacy_digest.as_ref()
    }
}

impl fmt::Debug for LimaHostInstanceIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LimaHostInstanceIdentity")
            .field("digest", &self.digest)
            .finish()
    }
}

pub struct LimaHostIdentityObservation {
    identity: LimaHostInstanceIdentity,
    lima_home: OwnedFd,
    lima_home_stat: rustix::fs::Stat,
    instance_directory: OwnedFd,
    instance_stat: rustix::fs::Stat,
    platform_identifier: File,
    identifier_stat: rustix::fs::Stat,
    root_disk: File,
    root_disk_stat: rustix::fs::Stat,
    root_disk_bytes: u64,
    verify_disk_identity: bool,
}

impl LimaHostIdentityObservation {
    #[must_use]
    pub const fn identity(&self) -> &LimaHostInstanceIdentity {
        &self.identity
    }

    pub(crate) const fn root_disk_bytes(&self) -> u64 {
        self.root_disk_bytes
    }

    pub(crate) fn confirm(
        &self,
        request: &LimaObservationRequest,
    ) -> Result<(), LimaHostIdentityError> {
        let held_evidence = HeldIdentityEvidence {
            lima_home: &self.lima_home,
            lima_home_stat: &self.lima_home_stat,
            instance_directory: &self.instance_directory,
            instance_stat: &self.instance_stat,
            identifier: &self.platform_identifier,
            identifier_stat: &self.identifier_stat,
            root_disk: &self.root_disk,
            root_disk_stat: &self.root_disk_stat,
        };
        held_evidence.verify_bindings(request)?;
        let identifier = read_identifier(&self.platform_identifier, &self.identifier_stat)?;
        let disk = if self.verify_disk_identity {
            Some(read_disk_evidence(&self.root_disk, self.root_disk_bytes)?)
        } else {
            None
        };
        held_evidence.verify_bindings(request)?;
        let current = match disk {
            Some(disk) => derive_identity(&identifier, self.root_disk_bytes, &disk)?,
            None => derive_cleanup_identity(&identifier, None)?,
        };
        let matches = if self.verify_disk_identity {
            current == self.identity
        } else {
            current.ownership_digest == self.identity.ownership_digest
        };
        if !matches {
            return Err(drift("host_identity"));
        }
        Ok(())
    }
}

impl fmt::Debug for LimaHostIdentityObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let _ = (
            &self.lima_home,
            &self.instance_directory,
            &self.platform_identifier,
            &self.root_disk,
        );
        formatter
            .debug_struct("LimaHostIdentityObservation")
            .field("identity", &self.identity)
            .field("private_filesystem_evidence", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LimaHostIdentityErrorKind {
    UnsupportedBackend,
    MissingEvidence,
    UnsafeFilesystem,
    MalformedIdentifier,
    MalformedDisk,
    IdentityDrift,
    Unavailable,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct LimaHostIdentityError {
    pub kind: LimaHostIdentityErrorKind,
    pub stage: &'static str,
    pub code: &'static str,
    pub message: &'static str,
}

impl LimaHostIdentityError {
    const fn new(
        kind: LimaHostIdentityErrorKind,
        stage: &'static str,
        code: &'static str,
        message: &'static str,
    ) -> Self {
        Self {
            kind,
            stage,
            code,
            message,
        }
    }
}

impl fmt::Debug for LimaHostIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LimaHostIdentityError")
            .field("kind", &self.kind)
            .field("stage", &self.stage)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for LimaHostIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for LimaHostIdentityError {}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LimaHostIdentityAdapter;

impl LimaHostIdentityAdapter {
    /// Observe the exact VZ machine identifier and raw root-disk GPT identity without mutation.
    ///
    /// The private Lima home is traversed descriptor-relatively with symlinks refused. The final
    /// Lima home and instance directory must be private to the current account; the two identity
    /// files must be current-user-owned, single-link regular files with no group/other write bits.
    /// Device and inode values are retained only by held descriptors and rebind checks. The public
    /// identity is derived exclusively from bounded immutable content and the exact disk length.
    ///
    /// # Errors
    ///
    /// Returns a bounded, path-free refusal when the backend is not the initial VZ backend, the
    /// filesystem shape is unsafe, identity evidence is absent or malformed, or any held object
    /// changes or is rebound during observation.
    pub fn observe(
        &self,
        request: &LimaObservationRequest,
    ) -> Result<LimaHostIdentityObservation, LimaHostIdentityError> {
        self.observe_with_disk_identity(request, true)
    }

    /// Observe cleanup ownership from host-controlled VZ evidence while retaining the root disk
    /// descriptor for name-to-inode and size checks. Guest-mutable GPT contents are deliberately
    /// excluded so a hostile guest cannot veto destruction by corrupting its own disk.
    pub(crate) fn observe_cleanup(
        &self,
        request: &LimaObservationRequest,
    ) -> Result<LimaHostIdentityObservation, LimaHostIdentityError> {
        self.observe_with_disk_identity(request, false)
    }

    fn observe_with_disk_identity(
        &self,
        request: &LimaObservationRequest,
        verify_disk_identity: bool,
    ) -> Result<LimaHostIdentityObservation, LimaHostIdentityError> {
        if request.expected_vm_type() != LimaVmType::Vz
            || request.expected_architecture() != LimaArchitecture::Aarch64
        {
            return Err(error(
                LimaHostIdentityErrorKind::UnsupportedBackend,
                "backend",
                "unsupported_backend",
                "stopped host identity is supported only for the reviewed Lima VZ backend",
            ));
        }

        let lima_home = open_absolute_directory(request.lima_home())?;
        let lima_home_stat = validate_private_directory(&lima_home, "lima_home")?;
        let instance_directory = fs::openat(
            &lima_home,
            OsStr::new(request.instance().as_str()),
            directory_flags(),
            Mode::empty(),
        )
        .map_err(|cause| map_open(cause, "instance_directory"))?;
        let instance_stat = validate_private_directory(&instance_directory, "instance_directory")?;

        let identifier = open_identity_file(
            &instance_directory,
            VZ_IDENTIFIER_FILE,
            "platform_identifier",
        )?;
        let identifier_stat = validate_identity_file(&identifier, "platform_identifier")?;
        let identifier_bytes = read_identifier(&identifier, &identifier_stat)?;

        let root_disk = open_identity_file(&instance_directory, ROOT_DISK_FILE, "root_disk")?;
        let root_disk_stat = validate_identity_file(&root_disk, "root_disk")?;
        let root_disk_bytes =
            u64::try_from(root_disk_stat.st_size).map_err(|_| malformed_disk())?;
        let disk_evidence = verify_disk_identity
            .then(|| read_disk_evidence(&root_disk, root_disk_bytes))
            .transpose()?;

        let held_evidence = HeldIdentityEvidence {
            lima_home: &lima_home,
            lima_home_stat: &lima_home_stat,
            instance_directory: &instance_directory,
            instance_stat: &instance_stat,
            identifier: &identifier,
            identifier_stat: &identifier_stat,
            root_disk: &root_disk,
            root_disk_stat: &root_disk_stat,
        };
        held_evidence.verify_bindings(request)?;

        let identifier_again = read_identifier(&identifier, &identifier_stat)?;
        if identifier_again != identifier_bytes {
            return Err(drift("platform_identifier"));
        }
        let disk_evidence_again = if verify_disk_identity {
            Some(read_disk_evidence(&root_disk, root_disk_bytes)?)
        } else {
            read_disk_evidence(&root_disk, root_disk_bytes).ok()
        };
        if verify_disk_identity && disk_evidence_again != disk_evidence {
            return Err(drift("root_disk"));
        }
        held_evidence.verify_bindings(request)?;

        Ok(LimaHostIdentityObservation {
            identity: match disk_evidence_again.as_deref() {
                Some(disk) if !verify_disk_identity => {
                    derive_cleanup_identity(&identifier_again, Some((root_disk_bytes, disk)))?
                }
                Some(disk) => derive_identity(&identifier_again, root_disk_bytes, disk)?,
                None => derive_cleanup_identity(&identifier_again, None)?,
            },
            lima_home,
            lima_home_stat,
            instance_directory,
            instance_stat,
            platform_identifier: identifier,
            identifier_stat,
            root_disk,
            root_disk_stat,
            root_disk_bytes,
            verify_disk_identity,
        })
    }
}

struct HeldIdentityEvidence<'a> {
    lima_home: &'a OwnedFd,
    lima_home_stat: &'a rustix::fs::Stat,
    instance_directory: &'a OwnedFd,
    instance_stat: &'a rustix::fs::Stat,
    identifier: &'a File,
    identifier_stat: &'a rustix::fs::Stat,
    root_disk: &'a File,
    root_disk_stat: &'a rustix::fs::Stat,
}

impl HeldIdentityEvidence<'_> {
    fn verify_bindings(
        &self,
        request: &LimaObservationRequest,
    ) -> Result<(), LimaHostIdentityError> {
        verify_held_entry(
            self.instance_directory,
            VZ_IDENTIFIER_FILE,
            self.identifier,
            self.identifier_stat,
            "platform_identifier",
            EntryStability::ImmutableFile,
        )?;
        verify_held_entry(
            self.instance_directory,
            ROOT_DISK_FILE,
            self.root_disk,
            self.root_disk_stat,
            "root_disk",
            EntryStability::MutableDisk,
        )?;
        verify_held_entry(
            self.lima_home,
            request.instance().as_str(),
            self.instance_directory,
            self.instance_stat,
            "instance_directory",
            EntryStability::Directory,
        )?;
        let rebound_lima_home = open_absolute_directory(request.lima_home())?;
        let rebound_lima_home_stat = validate_private_directory(&rebound_lima_home, "lima_home")?;
        if !same_entry(self.lima_home_stat, &rebound_lima_home_stat) {
            return Err(drift("lima_home"));
        }
        Ok(())
    }
}

fn open_absolute_directory(path: &Path) -> Result<OwnedFd, LimaHostIdentityError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(error(
            LimaHostIdentityErrorKind::UnsafeFilesystem,
            "lima_home",
            "unsafe_filesystem",
            "the private Lima home is not an exact absolute directory path",
        ));
    }
    let mut current = fs::open(Path::new("/"), directory_flags(), Mode::empty())
        .map_err(|cause| map_open(cause, "lima_home"))?;
    for component in path.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        current = fs::openat(&current, name, directory_flags(), Mode::empty())
            .map_err(|cause| map_open(cause, "lima_home"))?;
    }
    Ok(current)
}

fn validate_private_directory(
    directory: &OwnedFd,
    stage: &'static str,
) -> Result<rustix::fs::Stat, LimaHostIdentityError> {
    let stat = fs::fstat(directory).map_err(|_| unavailable(stage))?;
    if !FileType::from_raw_mode(stat.st_mode).is_dir()
        || stat.st_uid != geteuid().as_raw()
        || stat.st_mode & 0o7777 != 0o700
    {
        return Err(unsafe_filesystem(stage));
    }
    Ok(stat)
}

fn open_identity_file(
    directory: &OwnedFd,
    name: &'static str,
    stage: &'static str,
) -> Result<File, LimaHostIdentityError> {
    fs::openat(directory, name, file_flags(), Mode::empty())
        .map(File::from)
        .map_err(|cause| map_open(cause, stage))
}

fn validate_identity_file(
    file: &File,
    stage: &'static str,
) -> Result<rustix::fs::Stat, LimaHostIdentityError> {
    let stat = fs::fstat(file).map_err(|_| unavailable(stage))?;
    let permissions = stat.st_mode & 0o7777;
    if !FileType::from_raw_mode(stat.st_mode).is_file()
        || stat.st_uid != geteuid().as_raw()
        || stat.st_nlink != 1
        || !matches!(permissions, 0o600 | 0o640 | 0o644)
    {
        return Err(unsafe_filesystem(stage));
    }
    Ok(stat)
}

fn read_identifier(
    file: &File,
    expected: &rustix::fs::Stat,
) -> Result<Vec<u8>, LimaHostIdentityError> {
    let size = usize::try_from(expected.st_size).map_err(|_| malformed_identifier())?;
    if size != EXPECTED_VZ_IDENTIFIER_BYTES || size > MAX_VZ_IDENTIFIER_BYTES {
        return Err(malformed_identifier());
    }
    let mut bytes = vec![0_u8; size];
    file.read_exact_at(&mut bytes, 0)
        .map_err(|_| unavailable("platform_identifier"))?;
    if !valid_vz_identifier(&bytes) {
        return Err(malformed_identifier());
    }
    let after = fs::fstat(file).map_err(|_| unavailable("platform_identifier"))?;
    if !stable_file(expected, &after) {
        return Err(drift("platform_identifier"));
    }
    Ok(bytes)
}

fn valid_vz_identifier(bytes: &[u8]) -> bool {
    bytes.get(..8) == Some(VZ_IDENTIFIER_MAGIC)
        && bytes.get(8..19) == Some(VZ_IDENTIFIER_PREFIX)
        && bytes
            .get(19..35)
            .is_some_and(|identifier| identifier != [0; 16])
        && bytes.get(35..38) == Some(VZ_IDENTIFIER_OFFSET_TABLE)
        && bytes.get(38..44) == Some(&[0; 6])
        && bytes.get(44..46) == Some(&[1, 1])
        && bytes.get(46..54) == Some(&3_u64.to_be_bytes())
        && bytes.get(54..62) == Some(&0_u64.to_be_bytes())
        && bytes.get(62..70) == Some(&35_u64.to_be_bytes())
}

#[derive(Clone, PartialEq, Eq)]
struct GptHeader {
    first_usable_lba: u64,
    last_usable_lba: u64,
    disk_guid: [u8; 16],
    entries_lba: u64,
    entry_count: u32,
    entry_bytes: u32,
    table_bytes: u64,
    table_blocks: u64,
    table_crc32: u32,
}

fn read_disk_evidence(file: &File, disk_bytes: u64) -> Result<Vec<u8>, LimaHostIdentityError> {
    if disk_bytes < 68 * LOGICAL_BLOCK_BYTES || !disk_bytes.is_multiple_of(LOGICAL_BLOCK_BYTES) {
        return Err(malformed_disk());
    }
    let last_lba = disk_bytes / LOGICAL_BLOCK_BYTES - 1;
    let primary_bytes = read_exact_region(file, LOGICAL_BLOCK_BYTES, LOGICAL_BLOCK_BYTES as usize)?;
    let primary = parse_gpt_header(&primary_bytes)?;
    let primary_table_end = primary
        .entries_lba
        .checked_add(primary.table_blocks)
        .ok_or_else(malformed_disk)?;
    if primary.entries_lba != 2
        || primary.first_usable_lba < primary_table_end
        || primary.last_usable_lba >= last_lba
    {
        return Err(malformed_disk());
    }

    let backup_offset = last_lba
        .checked_mul(LOGICAL_BLOCK_BYTES)
        .ok_or_else(malformed_disk)?;
    let backup_bytes = read_exact_region(file, backup_offset, LOGICAL_BLOCK_BYTES as usize)?;
    let backup = parse_gpt_header(&backup_bytes)?;
    let backup_table_end = backup
        .entries_lba
        .checked_add(backup.table_blocks)
        .ok_or_else(malformed_disk)?;
    if backup_table_end != last_lba
        || primary.last_usable_lba >= backup.entries_lba
        || backup.first_usable_lba != primary.first_usable_lba
        || backup.last_usable_lba != primary.last_usable_lba
        || backup.disk_guid != primary.disk_guid
        || backup.entry_count != primary.entry_count
        || backup.entry_bytes != primary.entry_bytes
        || backup.table_bytes != primary.table_bytes
        || backup.table_crc32 != primary.table_crc32
    {
        return Err(malformed_disk());
    }

    validate_header_positions(&primary_bytes, 1, last_lba)?;
    validate_header_positions(&backup_bytes, last_lba, 1)?;
    let primary_table = read_exact_region(
        file,
        lba_offset(primary.entries_lba, disk_bytes)?,
        usize::try_from(primary.table_bytes).map_err(|_| malformed_disk())?,
    )?;
    let backup_table = read_exact_region(
        file,
        lba_offset(backup.entries_lba, disk_bytes)?,
        usize::try_from(backup.table_bytes).map_err(|_| malformed_disk())?,
    )?;
    if primary_table != backup_table || crc32(&primary_table) != primary.table_crc32 {
        return Err(malformed_disk());
    }

    let mut evidence = Vec::with_capacity(
        primary_bytes.len() + backup_bytes.len() + primary_table.len() + backup_table.len(),
    );
    evidence.extend_from_slice(&primary_bytes);
    evidence.extend_from_slice(&primary_table);
    evidence.extend_from_slice(&backup_table);
    evidence.extend_from_slice(&backup_bytes);
    Ok(evidence)
}

fn parse_gpt_header(bytes: &[u8]) -> Result<GptHeader, LimaHostIdentityError> {
    if bytes.get(..GPT_SIGNATURE.len()) != Some(GPT_SIGNATURE) {
        return Err(malformed_disk());
    }
    if le_u32(bytes, 8)? != 0x0001_0000 {
        return Err(malformed_disk());
    }
    let header_bytes = le_u32(bytes, 12)?;
    if header_bytes != MIN_GPT_HEADER_BYTES
        || le_u32(bytes, 20)? != 0
        || !bytes
            .get(MIN_GPT_HEADER_BYTES as usize..)
            .is_some_and(|padding| padding.iter().all(|byte| *byte == 0))
    {
        return Err(malformed_disk());
    }
    let mut canonical_header = bytes
        .get(..usize::try_from(header_bytes).map_err(|_| malformed_disk())?)
        .ok_or_else(malformed_disk)?
        .to_vec();
    canonical_header[16..20].fill(0);
    if crc32(&canonical_header) != le_u32(bytes, 16)? {
        return Err(malformed_disk());
    }
    let entry_count = le_u32(bytes, 80)?;
    let entry_bytes = le_u32(bytes, 84)?;
    if entry_count == 0
        || entry_count > MAX_GPT_ENTRIES
        || !(MIN_GPT_ENTRY_BYTES..=MAX_GPT_ENTRY_BYTES).contains(&entry_bytes)
        || !entry_bytes.is_multiple_of(128)
    {
        return Err(malformed_disk());
    }
    let table_bytes = u64::from(entry_count)
        .checked_mul(u64::from(entry_bytes))
        .filter(|bytes| *bytes <= MAX_GPT_TABLE_BYTES)
        .ok_or_else(malformed_disk)?;
    let table_blocks = table_bytes
        .checked_add(LOGICAL_BLOCK_BYTES - 1)
        .ok_or_else(malformed_disk)?
        / LOGICAL_BLOCK_BYTES;
    let disk_guid: [u8; 16] = bytes
        .get(56..72)
        .ok_or_else(malformed_disk)?
        .try_into()
        .map_err(|_| malformed_disk())?;
    if disk_guid == [0; 16] {
        return Err(malformed_disk());
    }
    let first_usable_lba = le_u64(bytes, 40)?;
    let last_usable_lba = le_u64(bytes, 48)?;
    if first_usable_lba > last_usable_lba {
        return Err(malformed_disk());
    }
    Ok(GptHeader {
        first_usable_lba,
        last_usable_lba,
        disk_guid,
        entries_lba: le_u64(bytes, 72)?,
        entry_count,
        entry_bytes,
        table_bytes,
        table_blocks,
        table_crc32: le_u32(bytes, 88)?,
    })
}

fn validate_header_positions(
    bytes: &[u8],
    expected_current: u64,
    expected_backup: u64,
) -> Result<(), LimaHostIdentityError> {
    if le_u64(bytes, 24)? != expected_current || le_u64(bytes, 32)? != expected_backup {
        return Err(malformed_disk());
    }
    Ok(())
}

fn lba_offset(lba: u64, disk_bytes: u64) -> Result<u64, LimaHostIdentityError> {
    lba.checked_mul(LOGICAL_BLOCK_BYTES)
        .filter(|offset| *offset < disk_bytes)
        .ok_or_else(malformed_disk)
}

fn read_exact_region(
    file: &File,
    offset: u64,
    size: usize,
) -> Result<Vec<u8>, LimaHostIdentityError> {
    let mut bytes = vec![0_u8; size];
    file.read_exact_at(&mut bytes, offset)
        .map_err(|_| unavailable("root_disk"))?;
    Ok(bytes)
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn le_u32(bytes: &[u8], offset: usize) -> Result<u32, LimaHostIdentityError> {
    bytes
        .get(offset..offset + 4)
        .and_then(|value| value.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or_else(malformed_disk)
}

fn le_u64(bytes: &[u8], offset: usize) -> Result<u64, LimaHostIdentityError> {
    bytes
        .get(offset..offset + 8)
        .and_then(|value| value.try_into().ok())
        .map(u64::from_le_bytes)
        .ok_or_else(malformed_disk)
}

fn verify_held_entry(
    parent: &OwnedFd,
    name: &str,
    held: impl AsFd,
    expected: &rustix::fs::Stat,
    stage: &'static str,
    stability: EntryStability,
) -> Result<(), LimaHostIdentityError> {
    let held_stat = fs::fstat(held.as_fd()).map_err(|_| unavailable(stage))?;
    let rebound =
        fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| unavailable(stage))?;
    let stable = match stability {
        EntryStability::Directory => same_entry(expected, &held_stat),
        EntryStability::ImmutableFile => stable_file(expected, &held_stat),
        EntryStability::MutableDisk => {
            same_entry(expected, &held_stat) && expected.st_size == held_stat.st_size
        }
    };
    if !stable || !same_entry(&held_stat, &rebound) || held_stat.st_size != rebound.st_size {
        return Err(drift(stage));
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum EntryStability {
    Directory,
    ImmutableFile,
    MutableDisk,
}

fn stable_file(before: &rustix::fs::Stat, after: &rustix::fs::Stat) -> bool {
    same_entry(before, after)
        && before.st_size == after.st_size
        && before.st_mtime == after.st_mtime
        && before.st_mtime_nsec == after.st_mtime_nsec
        && before.st_ctime == after.st_ctime
        && before.st_ctime_nsec == after.st_ctime_nsec
}

fn same_entry(left: &rustix::fs::Stat, right: &rustix::fs::Stat) -> bool {
    left.st_dev == right.st_dev
        && left.st_ino == right.st_ino
        && left.st_uid == right.st_uid
        && left.st_gid == right.st_gid
        && left.st_mode == right.st_mode
        && left.st_nlink == right.st_nlink
}

fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

fn file_flags() -> OFlags {
    OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

fn digest_fields<'a>(domain: &[u8], fields: impl IntoIterator<Item = &'a [u8]>) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update([0]);
    for field in fields {
        digest.update((field.len() as u64).to_be_bytes());
        digest.update(field);
    }
    digest.finalize().into()
}

fn derive_identity(
    identifier: &[u8],
    root_disk_bytes: u64,
    disk_evidence: &[u8],
) -> Result<LimaHostInstanceIdentity, LimaHostIdentityError> {
    let identifier_digest = digest_fields(b"smolrunner-lima-vz-identifier-v1", [identifier]);
    let disk_length = root_disk_bytes.to_be_bytes();
    let disk_digest = digest_fields(DISK_DOMAIN, [disk_length.as_slice(), disk_evidence]);
    let digest = digest_fields(
        IDENTITY_DOMAIN,
        [identifier_digest.as_slice(), disk_digest.as_slice()],
    );
    Ok(LimaHostInstanceIdentity {
        digest: parse_digest(digest)?,
        ownership_digest: parse_digest(identifier_digest)?,
        legacy_digest: Some(parse_digest(digest)?),
    })
}

fn derive_cleanup_identity(
    identifier: &[u8],
    legacy_disk: Option<(u64, &[u8])>,
) -> Result<LimaHostInstanceIdentity, LimaHostIdentityError> {
    let ownership = digest_fields(b"smolrunner-lima-vz-identifier-v1", [identifier]);
    let legacy_digest = legacy_disk
        .map(|(bytes, disk)| derive_identity(identifier, bytes, disk))
        .transpose()?
        .map(|identity| identity.digest);
    Ok(LimaHostInstanceIdentity {
        digest: parse_digest(ownership)?,
        ownership_digest: parse_digest(ownership)?,
        legacy_digest,
    })
}

fn parse_digest(bytes: [u8; 32]) -> Result<Sha256Digest, LimaHostIdentityError> {
    let value = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Sha256Digest::parse(&format!("sha256:{value}")).map_err(|_| unavailable("identity_derivation"))
}

const fn error(
    kind: LimaHostIdentityErrorKind,
    stage: &'static str,
    code: &'static str,
    message: &'static str,
) -> LimaHostIdentityError {
    LimaHostIdentityError::new(kind, stage, code, message)
}

const fn unsafe_filesystem(stage: &'static str) -> LimaHostIdentityError {
    error(
        LimaHostIdentityErrorKind::UnsafeFilesystem,
        stage,
        "unsafe_filesystem",
        "Lima host identity evidence has an unsafe filesystem shape",
    )
}

const fn malformed_identifier() -> LimaHostIdentityError {
    error(
        LimaHostIdentityErrorKind::MalformedIdentifier,
        "platform_identifier",
        "malformed_platform_identifier",
        "the Lima VZ platform identifier is malformed",
    )
}

const fn malformed_disk() -> LimaHostIdentityError {
    error(
        LimaHostIdentityErrorKind::MalformedDisk,
        "root_disk",
        "malformed_root_disk",
        "the Lima raw root disk has malformed or inconsistent GPT identity evidence",
    )
}

const fn drift(stage: &'static str) -> LimaHostIdentityError {
    error(
        LimaHostIdentityErrorKind::IdentityDrift,
        stage,
        "identity_drift",
        "Lima host identity evidence changed or was rebound during observation",
    )
}

const fn unavailable(stage: &'static str) -> LimaHostIdentityError {
    error(
        LimaHostIdentityErrorKind::Unavailable,
        stage,
        "identity_unavailable",
        "Lima host identity evidence could not be read or inspected",
    )
}

const fn map_open(cause: Errno, stage: &'static str) -> LimaHostIdentityError {
    match cause {
        Errno::NOENT => error(
            LimaHostIdentityErrorKind::MissingEvidence,
            stage,
            "missing_identity_evidence",
            "required Lima host identity evidence is missing",
        ),
        Errno::LOOP | Errno::NOTDIR => unsafe_filesystem(stage),
        _ => unavailable(stage),
    }
}

#[cfg(test)]
mod tests;
