//! Direct, command-free Linux account evidence for the personal-worker runtime closure.
//!
//! This module earns only the five account-related R01 evidence classes. It cannot construct the
//! complete runtime bundle or readiness capability.

use std::collections::BTreeSet;
use std::fmt;
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom, Take};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::{Component, Path};

use rustix::fs::{self, AtFlags, FileType, Mode, OFlags};
use rustix::io::{Errno, fcntl_dupfd_cloexec};
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::manifest::RunnerScope;
use crate::ownership::ProjectIdentity;
use crate::runner_account_plan::DesiredRunnerAccount;
use crate::runner_user::{PasswdRecord, parse_passwd_record};
use crate::subordinate_id::{SubordinateIdOwner, parse_subordinate_authority};

pub const PERSONAL_WORKER_RUNTIME_ACCOUNT_EVIDENCE_SCHEMA_VERSION: u8 = 1;

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NONBLOCK);
const MAX_AUTHORITY_BYTES: usize = 1_048_576;
const MAX_NSSWITCH_BYTES: usize = 65_536;
const MAX_ROWS: usize = 16_384;
const MAX_LINE_BYTES: usize = 4_096;
const ROOT_FILE_MODE: u32 = 0o644;
const HOME_MODE: u32 = 0o750;
const RUNTIME_MODE: u32 = 0o700;
const NOLOGIN: &str = "/usr/sbin/nologin";
const DOMAIN: &[u8] = b"smolrunner-personal-worker-runtime-account-evidence-v1";
const REDACTED: &str = "<private-runtime-account-evidence>";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerRuntimeAccountEvidenceDisposition {
    ObservedPrerequisite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerRuntimeAccountEvidenceSummary {
    schema_version: u8,
    disposition: PersonalWorkerRuntimeAccountEvidenceDisposition,
    evidence_classes: u8,
}

impl PersonalWorkerRuntimeAccountEvidenceSummary {
    #[must_use]
    pub const fn schema_version(self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn disposition(self) -> PersonalWorkerRuntimeAccountEvidenceDisposition {
        self.disposition
    }

    #[must_use]
    pub const fn evidence_classes(self) -> u8 {
        self.evidence_classes
    }
}

/// Opaque current evidence for the five account-related runtime classes.
///
/// It has no public constructor, serialization, clone, or digest/identity accessor. The complete
/// R01 observer may consume the private class digests only while composing all forty classes.
#[derive(PartialEq, Eq)]
pub struct PersonalWorkerRuntimeAccountEvidence {
    summary: PersonalWorkerRuntimeAccountEvidenceSummary,
    runner_account: Sha256Digest,
    primary_group: Sha256Digest,
    subordinate_uids: Sha256Digest,
    subordinate_gids: Sha256Digest,
    runtime_directory: Sha256Digest,
}

impl PersonalWorkerRuntimeAccountEvidence {
    #[must_use]
    pub const fn summary(&self) -> PersonalWorkerRuntimeAccountEvidenceSummary {
        self.summary
    }
}

impl fmt::Debug for PersonalWorkerRuntimeAccountEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersonalWorkerRuntimeAccountEvidence")
            .field("summary", &self.summary)
            .field("private_evidence", &REDACTED)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerRuntimeAccountEvidenceErrorKind {
    IdentityMismatch,
    Missing,
    UnsafeFilesystem,
    InvalidAuthority,
    ChangedDuringRead,
    Io,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerRuntimeAccountEvidenceError {
    pub kind: PersonalWorkerRuntimeAccountEvidenceErrorKind,
    pub code: &'static str,
    pub message: &'static str,
}

impl fmt::Debug for PersonalWorkerRuntimeAccountEvidenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersonalWorkerRuntimeAccountEvidenceError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for PersonalWorkerRuntimeAccountEvidenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for PersonalWorkerRuntimeAccountEvidenceError {}

/// Observe exact local account authority without invoking NSS, getent, systemd, or another child.
///
/// # Errors
///
/// Fails closed unless the project and desired runner agree, local NSS precedence is reviewed,
/// every authority file and directory is safely bound, and all requested account evidence is exact.
pub fn observe_personal_worker_runtime_account_evidence(
    project: &ProjectIdentity,
    desired: &DesiredRunnerAccount,
) -> Result<PersonalWorkerRuntimeAccountEvidence, PersonalWorkerRuntimeAccountEvidenceError> {
    observe_at(Path::new("/"), 0, project, desired)
}

fn observe_at(
    root_path: &Path,
    expected_authority_uid: u32,
    project: &ProjectIdentity,
    desired: &DesiredRunnerAccount,
) -> Result<PersonalWorkerRuntimeAccountEvidence, PersonalWorkerRuntimeAccountEvidenceError> {
    if project.runner_user != desired.username().as_str() {
        return Err(identity_error());
    }
    let etc = DirectoryChain::open(root_path, "etc", expected_authority_uid, None)?;
    let authority_owner = etc.root_owner();
    let mut passwd = BoundFile::open(etc.leaf(), "passwd", authority_owner, MAX_AUTHORITY_BYTES)?;
    let mut group = BoundFile::open(etc.leaf(), "group", authority_owner, MAX_AUTHORITY_BYTES)?;
    let mut nsswitch = BoundFile::open(
        etc.leaf(),
        "nsswitch.conf",
        authority_owner,
        MAX_NSSWITCH_BYTES,
    )?;
    let mut subuid = BoundFile::open(etc.leaf(), "subuid", authority_owner, MAX_AUTHORITY_BYTES)?;
    let mut subgid = BoundFile::open(etc.leaf(), "subgid", authority_owner, MAX_AUTHORITY_BYTES)?;

    require_local_nss(&nsswitch.bytes)?;
    let passwd_record = require_passwd(&passwd.bytes, desired)?;
    let group_gid = require_group(&group.bytes, desired)?;
    if passwd_record.primary_gid() != group_gid {
        return Err(identity_error());
    }
    require_subordinate(&subuid.bytes, desired, passwd_record.uid(), true)?;
    require_subordinate(&subgid.bytes, desired, passwd_record.uid(), false)?;

    let home = DirectoryChain::open(
        root_path,
        desired.home().trim_start_matches('/'),
        expected_authority_uid,
        Some((passwd_record.uid(), passwd_record.primary_gid(), HOME_MODE)),
    )?;
    let runtime_relative = format!("run/user/{}", passwd_record.uid());
    let runtime = DirectoryChain::open(
        root_path,
        &runtime_relative,
        expected_authority_uid,
        Some((
            passwd_record.uid(),
            passwd_record.primary_gid(),
            RUNTIME_MODE,
        )),
    )?;
    let linger_parent = DirectoryChain::open(
        root_path,
        "var/lib/systemd/linger",
        expected_authority_uid,
        None,
    )?;
    let mut linger = BoundFile::open_exact_size(
        linger_parent.leaf(),
        desired.username().as_str(),
        linger_parent.root_owner(),
        0,
    )?;
    let project_binding = project_digest(project);

    let runner_account = class_digest(
        b"runner_account",
        &[
            project_binding.clone(),
            passwd.digest(b"etc/passwd"),
            nsswitch.digest(b"etc/nsswitch.conf"),
            home.digest(b"runner_home"),
            linger.digest(b"linger"),
        ],
        &[
            desired.username().as_str().as_bytes(),
            desired.home().as_bytes(),
            &passwd_record.uid().to_be_bytes(),
        ],
    )?;
    let primary_group = class_digest(
        b"primary_group",
        &[
            project_binding.clone(),
            group.digest(b"etc/group"),
            nsswitch.digest(b"etc/nsswitch.conf"),
        ],
        &[
            desired.primary_group().as_str().as_bytes(),
            &group_gid.to_be_bytes(),
        ],
    )?;
    let subordinate_uids = class_digest(
        b"subordinate_uids",
        &[project_binding.clone(), subuid.digest(b"etc/subuid")],
        &[
            &desired.subordinate_uids().start().to_be_bytes(),
            &desired.subordinate_uids().count().to_be_bytes(),
        ],
    )?;
    let subordinate_gids = class_digest(
        b"subordinate_gids",
        &[project_binding.clone(), subgid.digest(b"etc/subgid")],
        &[
            &desired.subordinate_gids().start().to_be_bytes(),
            &desired.subordinate_gids().count().to_be_bytes(),
        ],
    )?;
    let runtime_directory = class_digest(
        b"runtime_directory",
        &[project_binding, runtime.digest(b"runtime_directory")],
        &[],
    )?;

    for file in [
        &mut passwd,
        &mut group,
        &mut nsswitch,
        &mut subuid,
        &mut subgid,
        &mut linger,
    ] {
        file.revalidate()?;
    }
    for directory in [&etc, &home, &runtime, &linger_parent] {
        directory.revalidate()?;
    }

    Ok(PersonalWorkerRuntimeAccountEvidence {
        summary: PersonalWorkerRuntimeAccountEvidenceSummary {
            schema_version: PERSONAL_WORKER_RUNTIME_ACCOUNT_EVIDENCE_SCHEMA_VERSION,
            disposition: PersonalWorkerRuntimeAccountEvidenceDisposition::ObservedPrerequisite,
            evidence_classes: 5,
        },
        runner_account,
        primary_group,
        subordinate_uids,
        subordinate_gids,
        runtime_directory,
    })
}

struct BoundFile {
    file: File,
    parent: BorrowedParent,
    name: String,
    bytes: Vec<u8>,
    snapshot: rustix::fs::Stat,
    max_bytes: usize,
}

// A raw borrowed descriptor cannot be stored safely; retain a duplicate owned descriptor instead.
struct BorrowedParent(OwnedFd);

impl BoundFile {
    fn open(
        parent: BorrowedFd<'_>,
        name: &str,
        owner: (u32, u32),
        max_bytes: usize,
    ) -> Result<Self, PersonalWorkerRuntimeAccountEvidenceError> {
        Self::open_with_size(parent, name, owner, max_bytes, None)
    }

    fn open_exact_size(
        parent: BorrowedFd<'_>,
        name: &str,
        owner: (u32, u32),
        size: usize,
    ) -> Result<Self, PersonalWorkerRuntimeAccountEvidenceError> {
        Self::open_with_size(parent, name, owner, size, Some(size))
    }

    fn open_with_size(
        parent: BorrowedFd<'_>,
        name: &str,
        owner: (u32, u32),
        max_bytes: usize,
        exact_size: Option<usize>,
    ) -> Result<Self, PersonalWorkerRuntimeAccountEvidenceError> {
        let parent = fcntl_dupfd_cloexec(parent, 0).map_err(|_| io_error())?;
        let fd = fs::openat(&parent, name, FILE_FLAGS, Mode::empty()).map_err(map_open)?;
        let mut file = File::from(fd);
        let before = fs::fstat(&file).map_err(|_| io_error())?;
        inspect_file(&before, owner, max_bytes, exact_size)?;
        let first = read_bounded(&mut file, max_bytes)?;
        file.seek(SeekFrom::Start(0)).map_err(|_| io_error())?;
        let second = read_bounded(&mut file, max_bytes)?;
        let after = fs::fstat(&file).map_err(|_| io_error())?;
        inspect_file(&after, owner, max_bytes, exact_size)?;
        if first != second || !same_snapshot(&before, &after) {
            return Err(changed_error());
        }
        Ok(Self {
            file,
            parent: BorrowedParent(parent),
            name: name.to_owned(),
            bytes: first,
            snapshot: after,
            max_bytes,
        })
    }

    fn digest(&self, label: &[u8]) -> Vec<u8> {
        let mut hasher = Sha256::new();
        hash_field(&mut hasher, label);
        hash_stat(&mut hasher, &self.snapshot);
        hash_field(&mut hasher, &self.bytes);
        hasher.finalize().to_vec()
    }

    fn revalidate(&mut self) -> Result<(), PersonalWorkerRuntimeAccountEvidenceError> {
        let before = fs::fstat(&self.file).map_err(|_| changed_error())?;
        if !same_snapshot(&self.snapshot, &before) {
            return Err(changed_error());
        }
        self.file.seek(SeekFrom::Start(0)).map_err(|_| io_error())?;
        let first = read_bounded(&mut self.file, self.max_bytes).map_err(|_| changed_error())?;
        self.file.seek(SeekFrom::Start(0)).map_err(|_| io_error())?;
        let second = read_bounded(&mut self.file, self.max_bytes).map_err(|_| changed_error())?;
        let after = fs::fstat(&self.file).map_err(|_| changed_error())?;
        let path = fs::statat(&self.parent.0, &self.name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| changed_error())?;
        if first != self.bytes
            || second != self.bytes
            || !same_snapshot(&self.snapshot, &after)
            || !same_snapshot(&self.snapshot, &path)
        {
            return Err(changed_error());
        }
        Ok(())
    }
}

struct DirectoryNode {
    fd: OwnedFd,
    name: Option<String>,
    snapshot: rustix::fs::Stat,
}

struct DirectoryChain {
    root_path: std::path::PathBuf,
    nodes: Vec<DirectoryNode>,
    root_owner: (u32, u32),
}

impl DirectoryChain {
    fn open(
        root_path: &Path,
        relative: &str,
        expected_authority_uid: u32,
        final_policy: Option<(u32, u32, u32)>,
    ) -> Result<Self, PersonalWorkerRuntimeAccountEvidenceError> {
        let root = fs::open(root_path, DIRECTORY_FLAGS, Mode::empty()).map_err(map_open)?;
        let root_stat = fs::fstat(&root).map_err(|_| io_error())?;
        inspect_authority_directory(&root_stat, expected_authority_uid)?;
        let root_owner = (root_stat.st_uid, root_stat.st_gid);
        let mut nodes = vec![DirectoryNode {
            fd: root,
            name: None,
            snapshot: root_stat,
        }];
        let path = Path::new(relative);
        if path.is_absolute()
            || path
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(identity_error());
        }
        let components = path.components().collect::<Vec<_>>();
        if components.is_empty() {
            return Err(identity_error());
        }
        for (index, component) in components.iter().enumerate() {
            let name = component.as_os_str().to_str().ok_or_else(identity_error)?;
            let parent = nodes.last().expect("root node").fd.as_fd();
            let fd = fs::openat(parent, name, DIRECTORY_FLAGS, Mode::empty()).map_err(map_open)?;
            let stat = fs::fstat(&fd).map_err(|_| io_error())?;
            if index + 1 == components.len() {
                if let Some((uid, gid, mode)) = final_policy {
                    inspect_exact_directory(&stat, uid, gid, mode)?;
                } else {
                    inspect_authority_directory_with_owner(&stat, root_owner)?;
                }
            } else {
                inspect_authority_directory_with_owner(&stat, root_owner)?;
            }
            nodes.push(DirectoryNode {
                fd,
                name: Some(name.to_owned()),
                snapshot: stat,
            });
        }
        Ok(Self {
            root_path: root_path.to_owned(),
            nodes,
            root_owner,
        })
    }

    fn leaf(&self) -> BorrowedFd<'_> {
        self.nodes.last().expect("nonempty chain").fd.as_fd()
    }

    const fn root_owner(&self) -> (u32, u32) {
        self.root_owner
    }

    fn digest(&self, label: &[u8]) -> Vec<u8> {
        let mut hasher = Sha256::new();
        hash_field(&mut hasher, label);
        for node in &self.nodes {
            hash_field(&mut hasher, node.name.as_deref().unwrap_or("/").as_bytes());
            hash_stat(&mut hasher, &node.snapshot);
        }
        hasher.finalize().to_vec()
    }

    fn revalidate(&self) -> Result<(), PersonalWorkerRuntimeAccountEvidenceError> {
        for (index, node) in self.nodes.iter().enumerate() {
            let held = fs::fstat(&node.fd).map_err(|_| io_error())?;
            if !same_snapshot(&node.snapshot, &held) {
                return Err(changed_error());
            }
            if index > 0 {
                let parent = &self.nodes[index - 1].fd;
                let path = fs::statat(
                    parent,
                    node.name.as_deref().expect("child name"),
                    AtFlags::SYMLINK_NOFOLLOW,
                )
                .map_err(|_| changed_error())?;
                if !same_snapshot(&node.snapshot, &path) {
                    return Err(changed_error());
                }
            }
        }
        let rebound = fs::open(&self.root_path, DIRECTORY_FLAGS, Mode::empty())
            .map_err(|_| changed_error())?;
        let rebound = fs::fstat(&rebound).map_err(|_| io_error())?;
        if !same_snapshot(&self.nodes[0].snapshot, &rebound) {
            return Err(changed_error());
        }
        Ok(())
    }
}

fn require_local_nss(bytes: &[u8]) -> Result<(), PersonalWorkerRuntimeAccountEvidenceError> {
    let input = authority_text(bytes)?;
    let lines = input.lines().collect::<Vec<_>>();
    if lines.is_empty()
        || lines.len() > MAX_ROWS
        || lines.iter().any(|line| {
            line.len() > MAX_LINE_BYTES
                || line
                    .chars()
                    .any(|character| character.is_control() && character != '\t')
        })
    {
        return Err(authority_error());
    }
    let mut seen = BTreeSet::new();
    for line in lines {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Some((database, sources)) = line.split_once(':') else {
            continue;
        };
        if database != "passwd" && database != "group" {
            continue;
        }
        if !seen.insert(database) {
            return Err(authority_error());
        }
        let sources = sources.split_whitespace().collect::<Vec<_>>();
        if sources != ["files"] && sources != ["files", "systemd"] {
            return Err(authority_error());
        }
    }
    if seen.len() != 2 {
        return Err(authority_error());
    }
    Ok(())
}

fn require_passwd(
    bytes: &[u8],
    desired: &DesiredRunnerAccount,
) -> Result<PasswdRecord, PersonalWorkerRuntimeAccountEvidenceError> {
    let input = authority_text(bytes)?;
    let mut names = BTreeSet::new();
    let mut uids = BTreeSet::new();
    let mut selected = None;
    for line in bounded_lines(input)? {
        let fields = line.split(':').collect::<Vec<_>>();
        if fields.len() != 7 || fields[0].is_empty() {
            return Err(authority_error());
        }
        let uid = canonical_u32(fields[2]).ok_or_else(authority_error)?;
        canonical_u32(fields[3]).ok_or_else(authority_error)?;
        if !names.insert(fields[0]) || !uids.insert(uid) {
            return Err(authority_error());
        }
        if fields[0] == desired.username().as_str() {
            selected = Some(format!("{line}\n"));
        }
    }
    let selected = selected.ok_or_else(missing_error)?;
    let record = parse_passwd_record(&selected).map_err(|_| authority_error())?;
    if record.username() != desired.username()
        || record.uid() == 0
        || record.primary_gid() == 0
        || record.home() != desired.home()
        || record.shell() != NOLOGIN
    {
        return Err(identity_error());
    }
    Ok(record)
}

fn require_group(
    bytes: &[u8],
    desired: &DesiredRunnerAccount,
) -> Result<u32, PersonalWorkerRuntimeAccountEvidenceError> {
    let input = authority_text(bytes)?;
    let mut names = BTreeSet::new();
    let mut gids = BTreeSet::new();
    let mut selected = None;
    for line in bounded_lines(input)? {
        let fields = line.split(':').collect::<Vec<_>>();
        if fields.len() != 4 || fields[0].is_empty() {
            return Err(authority_error());
        }
        let gid = canonical_u32(fields[2]).ok_or_else(authority_error)?;
        if !names.insert(fields[0]) || !gids.insert(gid) {
            return Err(authority_error());
        }
        if fields[0] == desired.primary_group().as_str() {
            if !fields[3].is_empty() {
                return Err(identity_error());
            }
            selected = Some(gid);
        }
    }
    let gid = selected.ok_or_else(missing_error)?;
    if gid == 0 {
        return Err(identity_error());
    }
    Ok(gid)
}

fn require_subordinate(
    bytes: &[u8],
    desired: &DesiredRunnerAccount,
    runner_uid: u32,
    uids: bool,
) -> Result<(), PersonalWorkerRuntimeAccountEvidenceError> {
    let input = authority_text(bytes)?;
    let authority = parse_subordinate_authority(input).map_err(|_| authority_error())?;
    let owner = SubordinateIdOwner::from(desired.username());
    let expected = if uids {
        desired.subordinate_uids()
    } else {
        desired.subordinate_gids()
    };
    let observed = authority.range_for(&owner).ok_or_else(missing_error)?;
    if observed.start() != expected.start() || observed.count() != expected.count() {
        return Err(identity_error());
    }
    let numeric_owner = runner_uid.to_string();
    if authority
        .records()
        .iter()
        .any(|record| record.owner.as_str() == numeric_owner)
    {
        return Err(authority_error());
    }
    Ok(())
}

fn authority_text(bytes: &[u8]) -> Result<&str, PersonalWorkerRuntimeAccountEvidenceError> {
    let input = std::str::from_utf8(bytes).map_err(|_| authority_error())?;
    if input.is_empty() || !input.ends_with('\n') || input.contains('\0') {
        return Err(authority_error());
    }
    Ok(input)
}

fn bounded_lines(input: &str) -> Result<Vec<&str>, PersonalWorkerRuntimeAccountEvidenceError> {
    let lines = input.lines().collect::<Vec<_>>();
    if lines.is_empty()
        || lines.len() > MAX_ROWS
        || lines.iter().any(|line| {
            line.is_empty() || line.len() > MAX_LINE_BYTES || line.chars().any(char::is_control)
        })
    {
        return Err(authority_error());
    }
    Ok(lines)
}

fn canonical_u32(value: &str) -> Option<u32> {
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    value.parse().ok()
}

fn inspect_file(
    stat: &rustix::fs::Stat,
    owner: (u32, u32),
    max_bytes: usize,
    exact_size: Option<usize>,
) -> Result<(), PersonalWorkerRuntimeAccountEvidenceError> {
    if !FileType::from_raw_mode(stat.st_mode).is_file()
        || stat.st_nlink != 1
        || stat.st_mode & 0o7777 != ROOT_FILE_MODE
        || (stat.st_uid, stat.st_gid) != owner
    {
        return Err(unsafe_error());
    }
    if stat.st_size < 0
        || stat.st_size as u64 > max_bytes as u64
        || exact_size.is_some_and(|size| stat.st_size as u64 != size as u64)
    {
        return Err(authority_error());
    }
    Ok(())
}

fn inspect_authority_directory(
    stat: &rustix::fs::Stat,
    uid: u32,
) -> Result<(), PersonalWorkerRuntimeAccountEvidenceError> {
    if !FileType::from_raw_mode(stat.st_mode).is_dir()
        || stat.st_uid != uid
        || stat.st_mode & 0o022 != 0
    {
        return Err(unsafe_error());
    }
    Ok(())
}

fn inspect_authority_directory_with_owner(
    stat: &rustix::fs::Stat,
    owner: (u32, u32),
) -> Result<(), PersonalWorkerRuntimeAccountEvidenceError> {
    inspect_authority_directory(stat, owner.0)?;
    if stat.st_gid != owner.1 {
        return Err(unsafe_error());
    }
    Ok(())
}

fn inspect_exact_directory(
    stat: &rustix::fs::Stat,
    uid: u32,
    gid: u32,
    mode: u32,
) -> Result<(), PersonalWorkerRuntimeAccountEvidenceError> {
    if !FileType::from_raw_mode(stat.st_mode).is_dir()
        || (stat.st_uid, stat.st_gid) != (uid, gid)
        || stat.st_mode & 0o7777 != mode
    {
        return Err(unsafe_error());
    }
    Ok(())
}

fn read_bounded(
    file: &mut File,
    max_bytes: usize,
) -> Result<Vec<u8>, PersonalWorkerRuntimeAccountEvidenceError> {
    let mut reader: Take<&mut File> = file.take((max_bytes + 1) as u64);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).map_err(|_| io_error())?;
    if bytes.len() > max_bytes {
        return Err(authority_error());
    }
    Ok(bytes)
}

fn same_snapshot(left: &rustix::fs::Stat, right: &rustix::fs::Stat) -> bool {
    left.st_dev == right.st_dev
        && left.st_ino == right.st_ino
        && left.st_uid == right.st_uid
        && left.st_gid == right.st_gid
        && left.st_mode == right.st_mode
        && left.st_nlink == right.st_nlink
        && left.st_size == right.st_size
        && left.st_mtime == right.st_mtime
        && left.st_mtime_nsec == right.st_mtime_nsec
        && left.st_ctime == right.st_ctime
        && left.st_ctime_nsec == right.st_ctime_nsec
}

fn hash_stat(hasher: &mut Sha256, stat: &rustix::fs::Stat) {
    for value in [
        stat.st_dev,
        stat.st_ino,
        stat.st_nlink as u64,
        stat.st_size as u64,
    ] {
        hash_field(hasher, &value.to_be_bytes());
    }
    for value in [stat.st_uid, stat.st_gid, stat.st_mode] {
        hash_field(hasher, &value.to_be_bytes());
    }
    for value in [stat.st_mtime, stat.st_ctime] {
        hash_field(hasher, &value.to_be_bytes());
    }
    for value in [stat.st_mtime_nsec, stat.st_ctime_nsec] {
        hash_field(hasher, &value.to_be_bytes());
    }
}

fn class_digest(
    class: &[u8],
    sources: &[Vec<u8>],
    values: &[&[u8]],
) -> Result<Sha256Digest, PersonalWorkerRuntimeAccountEvidenceError> {
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, DOMAIN);
    hash_field(&mut hasher, class);
    for source in sources {
        hash_field(&mut hasher, source);
    }
    for value in values {
        hash_field(&mut hasher, value);
    }
    Sha256Digest::parse(&format!("sha256:{:x}", hasher.finalize())).map_err(|_| authority_error())
}

fn project_digest(project: &ProjectIdentity) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, DOMAIN);
    hash_field(&mut hasher, b"project_identity");
    hash_field(&mut hasher, project.repository.as_bytes());
    hash_field(
        &mut hasher,
        &[match project.runner_scope {
            RunnerScope::Repository => 1,
            RunnerScope::Organization => 2,
        }],
    );
    hash_field(&mut hasher, project.runner_user.as_bytes());
    hasher.finalize().to_vec()
}

fn hash_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn map_open(error: Errno) -> PersonalWorkerRuntimeAccountEvidenceError {
    match error {
        Errno::NOENT => missing_error(),
        Errno::LOOP | Errno::NOTDIR | Errno::ISDIR => unsafe_error(),
        _ => io_error(),
    }
}

const fn error(
    kind: PersonalWorkerRuntimeAccountEvidenceErrorKind,
    code: &'static str,
    message: &'static str,
) -> PersonalWorkerRuntimeAccountEvidenceError {
    PersonalWorkerRuntimeAccountEvidenceError {
        kind,
        code,
        message,
    }
}

const fn identity_error() -> PersonalWorkerRuntimeAccountEvidenceError {
    error(
        PersonalWorkerRuntimeAccountEvidenceErrorKind::IdentityMismatch,
        "runtime_account_identity_mismatch",
        "personal worker runtime account identity does not match",
    )
}
const fn missing_error() -> PersonalWorkerRuntimeAccountEvidenceError {
    error(
        PersonalWorkerRuntimeAccountEvidenceErrorKind::Missing,
        "runtime_account_missing",
        "personal worker runtime account evidence is missing",
    )
}
const fn unsafe_error() -> PersonalWorkerRuntimeAccountEvidenceError {
    error(
        PersonalWorkerRuntimeAccountEvidenceErrorKind::UnsafeFilesystem,
        "runtime_account_unsafe_filesystem",
        "personal worker runtime account evidence has unsafe filesystem state",
    )
}
const fn authority_error() -> PersonalWorkerRuntimeAccountEvidenceError {
    error(
        PersonalWorkerRuntimeAccountEvidenceErrorKind::InvalidAuthority,
        "runtime_account_invalid_authority",
        "personal worker runtime account authority is invalid",
    )
}
const fn changed_error() -> PersonalWorkerRuntimeAccountEvidenceError {
    error(
        PersonalWorkerRuntimeAccountEvidenceErrorKind::ChangedDuringRead,
        "runtime_account_changed",
        "personal worker runtime account evidence changed during observation",
    )
}
const fn io_error() -> PersonalWorkerRuntimeAccountEvidenceError {
    error(
        PersonalWorkerRuntimeAccountEvidenceErrorKind::Io,
        "runtime_account_unavailable",
        "personal worker runtime account evidence could not be read",
    )
}

#[cfg(test)]
mod tests {
    use std::fs as std_fs;
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use rustix::process::{Gid, Uid, getegid, geteuid};

    use crate::lane_command::LinuxAccountName;
    use crate::runner_account_plan::PlannedSubordinateRange;

    use super::*;

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(label: &str) -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "smolrunner-runtime-account-{label}-{}-{sequence}",
                std::process::id()
            ));
            std_fs::create_dir(&path).expect("create fixture root");
            set_mode(&path, 0o755);
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std_fs::remove_dir_all(&self.0);
        }
    }

    struct Fixture {
        root: TempRoot,
        project: ProjectIdentity,
        desired: DesiredRunnerAccount,
        authority_uid: u32,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let root = TempRoot::new(label);
            let authority_uid = geteuid().as_raw();
            let authority_gid = getegid().as_raw();
            let (runner_uid, runner_gid) = if authority_uid == 0 {
                (10_001, 10_001)
            } else {
                assert_ne!(authority_gid, 0, "nonroot fixture requires a nonroot group");
                (authority_uid, authority_gid)
            };

            for relative in [
                "etc",
                "home",
                "run",
                "run/user",
                "var",
                "var/lib",
                "var/lib/systemd",
                "var/lib/systemd/linger",
            ] {
                let path = root.path().join(relative);
                std_fs::create_dir(&path).expect("create authority directory");
                set_mode(&path, 0o755);
            }
            let home = root.path().join("home/runtime-runner");
            std_fs::create_dir(&home).expect("create runner home");
            set_mode(&home, HOME_MODE);
            let runtime = root.path().join(format!("run/user/{runner_uid}"));
            std_fs::create_dir(&runtime).expect("create runtime directory");
            set_mode(&runtime, RUNTIME_MODE);
            if authority_uid == 0 {
                set_owner(&home, runner_uid, runner_gid);
                set_owner(&runtime, runner_uid, runner_gid);
            }

            write_authority(
                &root.path().join("etc/passwd"),
                format!(
                    "root:x:0:0:root:/root:/bin/bash\nruntime-runner:x:{runner_uid}:{runner_gid}::/home/runtime-runner:{NOLOGIN}\n"
                )
                .as_bytes(),
            );
            write_authority(
                &root.path().join("etc/group"),
                format!("root:x:0:\nruntime-runner:x:{runner_gid}:\n").as_bytes(),
            );
            write_authority(
                &root.path().join("etc/nsswitch.conf"),
                b"passwd: files systemd\ngroup: files systemd\nhosts: files dns\n",
            );
            write_authority(
                &root.path().join("etc/subuid"),
                b"runtime-runner:100000:65536\n",
            );
            write_authority(
                &root.path().join("etc/subgid"),
                b"runtime-runner:200000:65536\n",
            );
            write_authority(
                &root.path().join("var/lib/systemd/linger/runtime-runner"),
                b"",
            );

            let desired = DesiredRunnerAccount::new(
                LinuxAccountName::parse("runtime-runner").expect("username"),
                LinuxAccountName::parse("runtime-runner").expect("group"),
                "/home/runtime-runner",
                PlannedSubordinateRange::new(100_000, 65_536).expect("subuids"),
                PlannedSubordinateRange::new(200_000, 65_536).expect("subgids"),
            )
            .expect("desired account");
            let project = ProjectIdentity {
                repository: "example/runtime".to_owned(),
                runner_scope: RunnerScope::Repository,
                runner_user: "runtime-runner".to_owned(),
            };
            Self {
                root,
                project,
                desired,
                authority_uid,
            }
        }

        fn observe(
            &self,
        ) -> Result<PersonalWorkerRuntimeAccountEvidence, PersonalWorkerRuntimeAccountEvidenceError>
        {
            observe_at(
                self.root.path(),
                self.authority_uid,
                &self.project,
                &self.desired,
            )
        }
    }

    fn set_mode(path: &Path, mode: u32) {
        std_fs::set_permissions(path, std_fs::Permissions::from_mode(mode)).expect("set mode");
    }

    fn set_owner(path: &Path, uid: u32, gid: u32) {
        fs::chown(path, Some(Uid::from_raw(uid)), Some(Gid::from_raw(gid))).expect("set owner");
    }

    fn write_authority(path: &Path, bytes: &[u8]) {
        std_fs::write(path, bytes).expect("write authority file");
        set_mode(path, ROOT_FILE_MODE);
    }

    #[test]
    fn observes_five_private_account_classes_without_public_identity() {
        let fixture = Fixture::new("success");
        let evidence = fixture.observe().expect("observe account evidence");
        assert_eq!(
            evidence.summary(),
            PersonalWorkerRuntimeAccountEvidenceSummary {
                schema_version: 1,
                disposition: PersonalWorkerRuntimeAccountEvidenceDisposition::ObservedPrerequisite,
                evidence_classes: 5,
            }
        );
        let json = serde_json::to_string(&evidence.summary()).expect("serialize summary");
        assert_eq!(
            json,
            r#"{"schema_version":1,"disposition":"observed_prerequisite","evidence_classes":5}"#
        );
        let debug = format!("{evidence:?}");
        assert!(debug.contains(REDACTED));
        for private in [
            fixture.root.path().to_string_lossy().as_ref(),
            "runtime-runner",
            "10001",
            "sha256:",
        ] {
            assert!(!debug.contains(private));
        }
    }

    #[test]
    fn project_mismatch_precedes_filesystem_access() {
        let fixture = Fixture::new("project-mismatch");
        let mut project = fixture.project.clone();
        project.runner_user = "other-runner".to_owned();
        let missing_root = fixture.root.path().join("missing");
        let error = observe_at(
            &missing_root,
            fixture.authority_uid,
            &project,
            &fixture.desired,
        )
        .expect_err("mismatch must fail");
        assert_eq!(
            error.kind,
            PersonalWorkerRuntimeAccountEvidenceErrorKind::IdentityMismatch
        );
    }

    #[test]
    fn unsafe_authority_and_remote_nss_fail_closed() {
        let fixture = Fixture::new("unsafe-authority");
        let passwd = fixture.root.path().join("etc/passwd");
        let saved = fixture.root.path().join("etc/passwd.saved");
        std_fs::rename(&passwd, &saved).expect("move passwd");
        symlink("passwd.saved", &passwd).expect("symlink passwd");
        let error = fixture.observe().expect_err("symlink must fail");
        assert_eq!(
            error.kind,
            PersonalWorkerRuntimeAccountEvidenceErrorKind::UnsafeFilesystem
        );

        std_fs::remove_file(&passwd).expect("remove symlink");
        std_fs::rename(&saved, &passwd).expect("restore passwd");
        write_authority(
            &fixture.root.path().join("etc/nsswitch.conf"),
            b"passwd: files ldap\ngroup: files systemd\n",
        );
        let error = fixture.observe().expect_err("remote NSS must fail");
        assert_eq!(
            error.kind,
            PersonalWorkerRuntimeAccountEvidenceErrorKind::InvalidAuthority
        );
    }

    #[test]
    fn duplicate_ids_and_overlapping_subordinate_authority_fail_closed() {
        let fixture = Fixture::new("invalid-authority");
        let passwd =
            std_fs::read_to_string(fixture.root.path().join("etc/passwd")).expect("read passwd");
        let runner = passwd.lines().nth(1).expect("runner record");
        let uid = runner.split(':').nth(2).expect("runner uid");
        write_authority(
            &fixture.root.path().join("etc/passwd"),
            format!("{passwd}other:x:{uid}:4000::/home/other:{NOLOGIN}\n").as_bytes(),
        );
        assert_eq!(
            fixture.observe().expect_err("duplicate UID").kind,
            PersonalWorkerRuntimeAccountEvidenceErrorKind::InvalidAuthority
        );

        write_authority(&fixture.root.path().join("etc/passwd"), passwd.as_bytes());
        write_authority(
            &fixture.root.path().join("etc/subuid"),
            b"runtime-runner:100000:65536\nother:120000:65536\n",
        );
        assert_eq!(
            fixture.observe().expect_err("overlap").kind,
            PersonalWorkerRuntimeAccountEvidenceErrorKind::InvalidAuthority
        );

        write_authority(
            &fixture.root.path().join("etc/subuid"),
            format!("runtime-runner:100000:65536\n{uid}:300000:65536\n").as_bytes(),
        );
        assert_eq!(
            fixture.observe().expect_err("numeric owner alias").kind,
            PersonalWorkerRuntimeAccountEvidenceErrorKind::InvalidAuthority
        );
    }

    #[test]
    fn retained_file_detects_in_place_and_path_replacement_drift() {
        let fixture = Fixture::new("file-drift");
        let etc = DirectoryChain::open(fixture.root.path(), "etc", fixture.authority_uid, None)
            .expect("open etc");
        let path = fixture.root.path().join("etc/group");
        let original = std_fs::read(&path).expect("read group");
        let mut bound = BoundFile::open(etc.leaf(), "group", etc.root_owner(), MAX_AUTHORITY_BYTES)
            .expect("bind group");
        let mut changed = original.clone();
        changed[0] = b'R';
        std_fs::write(&path, &changed).expect("rewrite group");
        assert_eq!(
            bound.revalidate().expect_err("in-place drift").kind,
            PersonalWorkerRuntimeAccountEvidenceErrorKind::ChangedDuringRead
        );

        write_authority(&path, &original);
        let mut rebound =
            BoundFile::open(etc.leaf(), "group", etc.root_owner(), MAX_AUTHORITY_BYTES)
                .expect("bind restored group");
        let displaced = fixture.root.path().join("etc/group.displaced");
        std_fs::rename(&path, &displaced).expect("displace group");
        write_authority(&path, &original);
        assert_eq!(
            rebound.revalidate().expect_err("path replacement").kind,
            PersonalWorkerRuntimeAccountEvidenceErrorKind::ChangedDuringRead
        );
    }

    #[test]
    fn accepted_authority_changes_private_identity() {
        let fixture = Fixture::new("digest-change");
        let first = fixture.observe().expect("first observation");
        write_authority(
            &fixture.root.path().join("etc/nsswitch.conf"),
            b"passwd: files\ngroup: files\nhosts: files dns\n",
        );
        let second = fixture.observe().expect("second observation");
        assert_ne!(first.runner_account, second.runner_account);
        assert_ne!(first.primary_group, second.primary_group);
        assert_eq!(first.subordinate_uids, second.subordinate_uids);
        assert_eq!(first.subordinate_gids, second.subordinate_gids);
    }

    #[test]
    fn exact_project_identity_binds_every_private_class() {
        let fixture = Fixture::new("project-binding");
        let first = fixture.observe().expect("first observation");
        let mut project = fixture.project.clone();
        project.repository = "example/other-runtime".to_owned();
        let second = observe_at(
            fixture.root.path(),
            fixture.authority_uid,
            &project,
            &fixture.desired,
        )
        .expect("second observation");
        assert_ne!(first.runner_account, second.runner_account);
        assert_ne!(first.primary_group, second.primary_group);
        assert_ne!(first.subordinate_uids, second.subordinate_uids);
        assert_ne!(first.subordinate_gids, second.subordinate_gids);
        assert_ne!(first.runtime_directory, second.runtime_directory);
    }

    #[test]
    fn module_has_no_process_or_readiness_authority() {
        let source = include_str!("linux_personal_worker_runtime_account_evidence.rs");
        let production = source
            .split("#[cfg(test)]")
            .next()
            .expect("production source");
        for forbidden in [
            "std::process",
            "CommandSpec",
            "ProcessExecutor",
            "PersonalWorkerRuntimeReadiness::",
            "seal_runtime",
        ] {
            assert!(
                !production.contains(forbidden),
                "forbidden authority: {forbidden}"
            );
        }
    }
}
